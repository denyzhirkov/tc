use arc_swap::ArcSwap;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tc_shared::ChannelId;
use tokio::sync::RwLock;

use crate::metrics::Metrics;

/// Lock-free-read cache of channel → UDP peer addresses.
///
/// Two-level [`ArcSwap`]:
/// * outer — channel set (rebuilt only when channels are added/removed),
/// * inner — peer list per channel (atomically swapped on every join/leave).
///
/// On the UDP relay hot path: one atomic load of the outer map, one atomic
/// load of the inner peer list. No locks, no allocations, no per-channel copies
/// of unrelated state.
type ChannelPeers = Arc<ArcSwap<Vec<SocketAddr>>>;
type PeerCache = Arc<ArcSwap<HashMap<String, ChannelPeers>>>;

/// Snapshot of server counters for logging.
pub struct Stats {
    pub clients: usize,
    pub channels: usize,
    pub udp_peers: usize,
}

/// Resource limits configured via CLI args.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Max channels (0 = unlimited).
    pub max_channels: usize,
    /// Max clients (0 = unlimited).
    pub max_clients: usize,
    /// Multiplier applied to all rate-limit buckets (per-conn and per-IP).
    /// 1.0 = production defaults.
    pub rate_limit_multiplier: f64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_channels: 0,
            max_clients: 0,
            rate_limit_multiplier: 1.0,
        }
    }
}

/// Global server state shared across tasks.
#[derive(Clone)]
pub struct ServerState {
    inner: Arc<RwLock<Inner>>,
    peer_cache: PeerCache,
    metrics: Arc<Metrics>,
}

#[derive(Debug)]
struct Inner {
    /// channel_id → set of participant addresses (UDP).
    channels: HashMap<ChannelId, HashSet<SocketAddr>>,
    /// Maps a TCP peer address → (assigned name, current channel).
    clients: HashMap<SocketAddr, ClientInfo>,
    /// Reverse index pubkey → tcp_addr for DM routing.
    pubkey_index: HashMap<Vec<u8>, SocketAddr>,
    /// Per-channel voice encryption keys.
    channel_keys: HashMap<ChannelId, Vec<u8>>,
    /// Channel creation timestamps (grace period for cleanup).
    channel_created_at: HashMap<ChannelId, Instant>,
    /// Channels that survive being empty (pre-created at startup); never reaped.
    persistent: HashSet<ChannelId>,
    /// Pending UDP tokens: token → (tcp_addr, channel_id, created_at).
    udp_tokens: HashMap<u64, (SocketAddr, ChannelId, Instant)>,
    /// Counter for generating simple peer names.
    name_counter: u64,
    /// Resource limits.
    limits: Limits,
}

#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub name: String,
    pub channel: Option<ChannelId>,
    pub udp_addr: Option<SocketAddr>,
    pub pubkey: Option<Vec<u8>>,
}

impl ServerState {
    pub fn new(limits: Limits) -> Self {
        Self::with_metrics(limits, Metrics::new())
    }

    pub fn with_metrics(limits: Limits, metrics: Arc<Metrics>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner {
                channels: HashMap::new(),
                channel_keys: HashMap::new(),
                channel_created_at: HashMap::new(),
                persistent: HashSet::new(),
                clients: HashMap::new(),
                pubkey_index: HashMap::new(),
                udp_tokens: HashMap::new(),
                name_counter: 0,
                limits,
            })),
            peer_cache: Arc::new(ArcSwap::from_pointee(HashMap::new())),
            metrics,
        }
    }

    /// Access shared metrics handle.
    pub fn metrics(&self) -> &Arc<Metrics> {
        &self.metrics
    }

    /// Read the configured rate-limit multiplier (≥ 0).
    pub async fn rate_limit_multiplier(&self) -> f64 {
        self.inner
            .read()
            .await
            .limits
            .rate_limit_multiplier
            .max(0.0001)
    }

    /// Snapshot of server counters for logging.
    pub async fn stats(&self) -> Stats {
        let inner = self.inner.read().await;
        let udp_peers: usize = inner.channels.values().map(|p| p.len()).sum();
        Stats {
            clients: inner.clients.len(),
            channels: inner.channels.len(),
            udp_peers,
        }
    }

    /// Register a new TCP client, returns assigned name or error if at capacity.
    pub async fn register_client(&self, tcp_addr: SocketAddr) -> Result<String, String> {
        let mut inner = self.inner.write().await;
        if inner.limits.max_clients > 0 && inner.clients.len() >= inner.limits.max_clients {
            return Err("server full".into());
        }
        inner.name_counter += 1;
        let name = format!("user-{}", inner.name_counter);
        inner.clients.insert(
            tcp_addr,
            ClientInfo {
                name: name.clone(),
                channel: None,
                udp_addr: None,
                pubkey: None,
            },
        );
        Ok(name)
    }

    /// Bind a pubkey to a TCP address. If the pubkey was already bound to a different
    /// address, the previous binding is overwritten (last writer wins).
    pub async fn set_pubkey(&self, tcp_addr: &SocketAddr, pubkey: Vec<u8>) {
        let mut inner = self.inner.write().await;
        if let Some(client) = inner.clients.get_mut(tcp_addr) {
            if let Some(old) = client.pubkey.replace(pubkey.clone()) {
                if old != pubkey {
                    inner.pubkey_index.remove(&old);
                }
            }
            inner.pubkey_index.insert(pubkey, *tcp_addr);
        }
    }

    /// Resolve a pubkey to its current TCP address.
    pub async fn lookup_pubkey(&self, pubkey: &[u8]) -> Option<SocketAddr> {
        self.inner.read().await.pubkey_index.get(pubkey).copied()
    }

    /// Remove client, returns (name, channel_id) if was in a channel.
    pub async fn remove_client(
        &self,
        tcp_addr: &SocketAddr,
    ) -> Option<(String, Option<ChannelId>)> {
        let mut inner = self.inner.write().await;
        let info = inner.clients.remove(tcp_addr)?;
        if let Some(ref pk) = info.pubkey {
            inner.pubkey_index.remove(pk);
        }
        // Remove any pending UDP tokens for this client
        inner.udp_tokens.retain(|_, (addr, _, _)| addr != tcp_addr);
        if let Some(ref ch) = info.channel {
            let mut channel_destroyed = false;
            let is_persistent = inner.persistent.contains(ch);
            if let Some(participants) = inner.channels.get_mut(ch) {
                if let Some(udp) = info.udp_addr {
                    participants.remove(&udp);
                }
                if participants.is_empty() && !is_persistent {
                    inner.channels.remove(ch);
                    inner.channel_keys.remove(ch);
                    inner.channel_created_at.remove(ch);
                    channel_destroyed = true;
                }
            }
            if channel_destroyed {
                Self::remove_channel_from_cache(&self.peer_cache, ch);
            } else {
                Self::update_channel_peers(&inner, &self.peer_cache, ch);
            }
        }
        Some((info.name, info.channel))
    }

    /// Create a new channel, returns channel_id or error if at capacity.
    /// If `name` is Some, creates a public channel with id "pub-<name>".
    pub async fn create_channel(&self, name: Option<&str>) -> Result<ChannelId, String> {
        let mut inner = self.inner.write().await;
        if inner.limits.max_channels > 0 && inner.channels.len() >= inner.limits.max_channels {
            return Err("channel limit reached".into());
        }

        let id = if let Some(name) = name {
            let id = format!("{}{}", tc_shared::config::PUBLIC_CHANNEL_PREFIX, name);
            if inner.channels.contains_key(&id) {
                return Err(format!("channel '{}' already exists", id));
            }
            id
        } else {
            loop {
                let id = tc_shared::generate_channel_id();
                if !inner.channels.contains_key(&id) {
                    break id;
                }
            }
        };

        inner.channels.insert(id.clone(), HashSet::new());
        let key: [u8; 32] = rand::random();
        inner.channel_keys.insert(id.clone(), key.to_vec());
        inner.channel_created_at.insert(id.clone(), Instant::now());
        Ok(id)
    }

    /// Pre-create a public channel that is never reaped when empty. Used at
    /// startup to stand up well-known lobbies. The voice key is rotated on the
    /// 0→1 occupancy transition (see [`Self::join_channel`]), so persistence
    /// covers the channel's existence, not a long-lived static key.
    pub async fn create_persistent_channel(&self, name: &str) -> Result<ChannelId, String> {
        let id = self.create_channel(Some(name)).await?;
        self.inner.write().await.persistent.insert(id.clone());
        Ok(id)
    }

    /// List public channels with participant counts.
    pub async fn list_public_channels(&self) -> Vec<(ChannelId, u32)> {
        let inner = self.inner.read().await;
        let prefix = tc_shared::config::PUBLIC_CHANNEL_PREFIX;

        // Single pass: count TCP clients per channel.
        let mut counts: HashMap<&str, u32> = HashMap::new();
        for c in inner.clients.values() {
            if let Some(ch) = c.channel.as_deref() {
                *counts.entry(ch).or_insert(0) += 1;
            }
        }
        inner
            .channels
            .keys()
            .filter(|id| id.starts_with(prefix))
            .map(|id| (id.clone(), counts.get(id.as_str()).copied().unwrap_or(0)))
            .collect()
    }

    /// Join a channel. Returns (participant names, udp_token, voice_key).
    pub async fn join_channel(
        &self,
        tcp_addr: &SocketAddr,
        channel_id: &ChannelId,
    ) -> Result<(Vec<String>, u64, Vec<u8>), String> {
        let mut inner = self.inner.write().await;
        if !inner.channels.contains_key(channel_id) {
            return Err(format!("channel '{}' not found", channel_id));
        }
        match inner.clients.get(tcp_addr) {
            None => return Err("client not registered".into()),
            Some(c) if c.channel.is_some() => {
                return Err("already in a channel, leave first".into());
            }
            Some(_) => {}
        }

        // Forward secrecy for persistent channels: the first client to enter an
        // otherwise-empty persistent channel mints a fresh voice key, so a prior
        // session's key can't decrypt this one. Ephemeral channels get a fresh
        // key for free (they are destroyed and recreated), but persistent ones
        // outlive their occupants, so rotate explicitly here.
        let occupied = inner
            .clients
            .values()
            .any(|c| c.channel.as_deref() == Some(channel_id.as_str()));
        if !occupied && inner.persistent.contains(channel_id) {
            let key: [u8; 32] = rand::random();
            inner.channel_keys.insert(channel_id.clone(), key.to_vec());
        }

        inner
            .clients
            .get_mut(tcp_addr)
            .expect("client presence checked above")
            .channel = Some(channel_id.clone());

        // Generate a unique UDP token
        let token = loop {
            let t = rand::random::<u64>();
            if t != 0 && !inner.udp_tokens.contains_key(&t) {
                break t;
            }
        };
        inner
            .udp_tokens
            .insert(token, (*tcp_addr, channel_id.clone(), Instant::now()));

        // Gather existing participant names
        let names: Vec<String> = inner
            .clients
            .values()
            .filter(|c| c.channel.as_deref() == Some(channel_id.as_str()))
            .map(|c| c.name.clone())
            .collect();

        let voice_key = inner
            .channel_keys
            .get(channel_id)
            .cloned()
            .unwrap_or_default();
        Ok((names, token, voice_key))
    }

    /// Create and join an ephemeral echo-test channel for this client.
    /// Returns `(channel_id, udp_token, voice_key)` — the same join params a
    /// real channel hands out. The id carries [`config::ECHO_CHANNEL_PREFIX`]
    /// so the UDP relay reflects packets back to the sender instead of fanning
    /// them out. The client must not already be in a channel.
    pub async fn start_echo_test(
        &self,
        tcp_addr: &SocketAddr,
    ) -> Result<(ChannelId, u64, Vec<u8>), String> {
        let mut inner = self.inner.write().await;
        if inner.limits.max_channels > 0 && inner.channels.len() >= inner.limits.max_channels {
            return Err("channel limit reached".into());
        }
        let client = inner.clients.get(tcp_addr).ok_or("client not registered")?;
        if client.channel.is_some() {
            return Err("already in a channel, leave first".into());
        }

        let channel_id = loop {
            let id = format!(
                "{}{}",
                tc_shared::config::ECHO_CHANNEL_PREFIX,
                tc_shared::generate_channel_id()
            );
            if !inner.channels.contains_key(&id) {
                break id;
            }
        };
        inner.channels.insert(channel_id.clone(), HashSet::new());
        let key: [u8; 32] = rand::random();
        inner.channel_keys.insert(channel_id.clone(), key.to_vec());
        inner
            .channel_created_at
            .insert(channel_id.clone(), Instant::now());

        let token = loop {
            let t = rand::random::<u64>();
            if t != 0 && !inner.udp_tokens.contains_key(&t) {
                break t;
            }
        };
        inner
            .udp_tokens
            .insert(token, (*tcp_addr, channel_id.clone(), Instant::now()));
        inner
            .clients
            .get_mut(tcp_addr)
            .ok_or("client not registered")?
            .channel = Some(channel_id.clone());

        Ok((channel_id, token, key.to_vec()))
    }

    /// Leave current channel.
    pub async fn leave_channel(&self, tcp_addr: &SocketAddr) -> Option<(String, ChannelId)> {
        let mut inner = self.inner.write().await;
        let client = inner.clients.get(tcp_addr)?;
        let channel_id = client.channel.clone()?;
        let name = client.name.clone();
        let udp_addr = client.udp_addr;

        if let Some(client) = inner.clients.get_mut(tcp_addr) {
            client.channel = None;
            client.udp_addr = None;
        }
        // Remove pending tokens for this client
        inner.udp_tokens.retain(|_, (addr, _, _)| addr != tcp_addr);
        let mut channel_destroyed = false;
        if let Some(participants) = inner.channels.get_mut(&channel_id) {
            if let Some(udp) = udp_addr {
                participants.remove(&udp);
            }
            if participants.is_empty() {
                inner.channels.remove(&channel_id);
                inner.channel_keys.remove(&channel_id);
                inner.channel_created_at.remove(&channel_id);
                channel_destroyed = true;
            }
        }
        if channel_destroyed {
            Self::remove_channel_from_cache(&self.peer_cache, &channel_id);
        } else {
            Self::update_channel_peers(&inner, &self.peer_cache, &channel_id);
        }
        Some((name, channel_id))
    }

    /// Register a UDP address using a hello token.
    ///
    /// The token issued at join is bound to the client's TCP source IP. We require
    /// the UDP hello to arrive from the same IP — otherwise a third party that
    /// learned the token (e.g. by sniffing an early frame) could hijack the voice
    /// slot and have packets routed to its address. IPs are compared in canonical
    /// form so a v4-mapped IPv6 TCP address (`::ffff:a.b.c.d` from a dual-stack
    /// listener) matches a plain IPv4 UDP source. Source ports may differ between
    /// TCP and UDP, so only the IP is compared.
    ///
    /// Registration is idempotent: the token stays valid (TTL refreshed) so a
    /// retried hello — e.g. after a lost ACK — is re-acknowledged, and a hello
    /// from a new source port (NAT rebind) replaces the stale address in the
    /// channel. A mismatch does not consume the token: a 64-bit token cannot be
    /// guessed, and burning it would let one spoofed packet permanently lock the
    /// real client out of voice receive.
    pub async fn register_udp_by_token(&self, token: u64, udp_addr: SocketAddr) -> bool {
        let mut inner = self.inner.write().await;
        let (tcp_addr, channel_id) = match inner.udp_tokens.get(&token) {
            Some((tcp, ch, _)) => (*tcp, ch.clone()),
            None => return false,
        };
        if tcp_addr.ip().to_canonical() != udp_addr.ip().to_canonical() {
            tracing::warn!(
                expected = %tcp_addr.ip(),
                actual = %udp_addr.ip(),
                "UDP hello rejected: source IP does not match TCP session"
            );
            return false;
        }
        if let Some((_, _, created)) = inner.udp_tokens.get_mut(&token) {
            *created = Instant::now();
        }
        let prev_udp = match inner.clients.get_mut(&tcp_addr) {
            Some(client) => client.udp_addr.replace(udp_addr),
            None => return false,
        };
        if let Some(participants) = inner.channels.get_mut(&channel_id) {
            if let Some(old) = prev_udp {
                if old != udp_addr {
                    participants.remove(&old);
                }
            }
            participants.insert(udp_addr);
        }
        Self::update_channel_peers(&inner, &self.peer_cache, &channel_id);
        true
    }

    /// Fill a reusable buffer with channel peers (excluding sender).
    /// Zero-allocation on the hot path when the Vec has enough capacity.
    /// Wait-free: two atomic loads, no locks.
    pub fn fill_channel_peers(
        &self,
        channel_id: &str,
        exclude: &SocketAddr,
        out: &mut Vec<SocketAddr>,
    ) {
        out.clear();
        let outer = self.peer_cache.load();
        if let Some(entry) = outer.get(channel_id) {
            let peers = entry.load();
            out.extend(peers.iter().filter(|a| *a != exclude).copied());
        }
    }

    /// Update the peer list for a single channel.
    ///
    /// Hot path for participant join/leave: looks up the channel's inner
    /// `ArcSwap` and swaps just that entry. The outer map is only rebuilt
    /// when a brand-new channel needs to be added (a one-time event per channel).
    fn update_channel_peers(inner: &Inner, cache: &PeerCache, channel_id: &str) {
        let new_peers: Vec<SocketAddr> = inner
            .channels
            .get(channel_id)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default();

        let outer = cache.load();
        if let Some(entry) = outer.get(channel_id) {
            // Channel already in cache → wait-free atomic swap of just this entry.
            entry.store(Arc::new(new_peers));
            return;
        }

        // First time we cache this channel: clone the outer map (Arc clones
        // for all other channels — cheap refcount bumps, no Vec copies)
        // and add the new entry.
        let mut next: HashMap<String, ChannelPeers> = (**outer).clone();
        next.insert(
            channel_id.to_string(),
            Arc::new(ArcSwap::from_pointee(new_peers)),
        );
        cache.store(Arc::new(next));
    }

    /// Remove a channel from the peer cache (called when the last participant leaves).
    /// Rare event — pays one outer-map clone with Arc-bump entries.
    fn remove_channel_from_cache(cache: &PeerCache, channel_id: &str) {
        let outer = cache.load();
        if !outer.contains_key(channel_id) {
            return;
        }
        let mut next: HashMap<String, ChannelPeers> = (**outer).clone();
        next.remove(channel_id);
        cache.store(Arc::new(next));
    }

    /// Iterate channel members under a single read lock, calling `send_fn` for each address.
    /// Caller is responsible for the message payload (e.g. a pre-serialized framed buffer
    /// shared by reference) so the broadcast does not pay a per-recipient clone cost.
    pub async fn broadcast_channel_addrs<F>(
        &self,
        channel_id: &str,
        exclude: Option<&SocketAddr>,
        mut send_fn: F,
    ) where
        F: FnMut(&SocketAddr),
    {
        let inner = self.inner.read().await;
        for (addr, info) in &inner.clients {
            if info.channel.as_deref() != Some(channel_id) {
                continue;
            }
            if exclude.is_some_and(|ex| ex == addr) {
                continue;
            }
            send_fn(addr);
        }
    }

    /// Rename a client. Returns (old_name, channel_id) if successful.
    pub async fn rename_client(
        &self,
        tcp_addr: &SocketAddr,
        new_name: String,
    ) -> Option<(String, Option<ChannelId>)> {
        let mut inner = self.inner.write().await;
        let client = inner.clients.get_mut(tcp_addr)?;
        let old_name = std::mem::replace(&mut client.name, new_name);
        let channel = client.channel.clone();
        Some((old_name, channel))
    }

    /// Get client info by TCP address.
    pub async fn get_client(&self, tcp_addr: &SocketAddr) -> Option<ClientInfo> {
        let inner = self.inner.read().await;
        inner.clients.get(tcp_addr).cloned()
    }

    /// Remove empty channels (no participants). Returns the removed channel ids
    /// (so the caller can refresh sidebars when a *public* channel disappears).
    /// Newly created channels get a grace period (2× maintenance interval) before
    /// being eligible for cleanup, so creators have time to join.
    pub async fn cleanup_empty_channels(&self) -> Vec<ChannelId> {
        self.cleanup_with(
            std::time::Duration::from_secs(tc_shared::config::MAINTENANCE_INTERVAL_SECS * 2),
            std::time::Duration::from_secs(tc_shared::config::UDP_TOKEN_TTL_SECS),
        )
        .await
    }

    /// Cleanup with explicit grace/TTL. Split out so tests can use zero
    /// durations instead of back-dating `Instant`s — `Instant::now() - 600s`
    /// underflows and panics on Windows runners with a short uptime.
    async fn cleanup_with(
        &self,
        grace: std::time::Duration,
        token_ttl: std::time::Duration,
    ) -> Vec<ChannelId> {
        let mut inner = self.inner.write().await;
        // Build set of occupied channel ids in one pass over clients.
        let occupied: HashSet<&str> = inner
            .clients
            .values()
            .filter_map(|c| c.channel.as_deref())
            .collect();
        let empty: Vec<ChannelId> = inner
            .channels
            .iter()
            .filter(|(id, participants)| {
                participants.is_empty()
                    && !occupied.contains(id.as_str())
                    && !inner.persistent.contains(id.as_str())
                    && inner
                        .channel_created_at
                        .get(id.as_str())
                        .map(|t| t.elapsed() > grace)
                        .unwrap_or(true)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &empty {
            inner.channels.remove(id);
            inner.channel_keys.remove(id);
            inner.channel_created_at.remove(id);
            // Also remove any stale tokens for this channel
            inner.udp_tokens.retain(|_, (_, ch, _)| ch != id);
        }
        // Expire aged UDP tokens — but never for a live session that is still
        // in the token's channel. The client's idle keepalive is a re-hello
        // with this token (refreshing its registered address after NAT
        // rebinds); expiring it would permanently lock the client out of
        // voice receive after the first 30 silent-but-talking seconds (no
        // keepalives flow while voice is streaming).
        let Inner {
            udp_tokens,
            clients,
            ..
        } = &mut *inner;
        udp_tokens.retain(|_, (tcp, ch, created)| {
            let live = clients
                .get(tcp)
                .is_some_and(|c| c.channel.as_deref() == Some(ch.as_str()));
            live || created.elapsed() < token_ttl
        });
        empty
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unlimited() -> Limits {
        Limits::default()
    }

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    // ── register / remove ────────────────────────────────────────────

    #[tokio::test]
    async fn register_client_assigns_unique_names() {
        let state = ServerState::new(unlimited());
        let n1 = state.register_client(addr(1)).await.unwrap();
        let n2 = state.register_client(addr(2)).await.unwrap();
        assert_ne!(n1, n2);
        assert!(n1.starts_with("user-"));
    }

    #[tokio::test]
    async fn register_client_respects_max_clients() {
        let limits = Limits {
            max_clients: 1,
            ..Limits::default()
        };
        let state = ServerState::new(limits);
        assert!(state.register_client(addr(1)).await.is_ok());
        assert!(state.register_client(addr(2)).await.is_err());
    }

    #[tokio::test]
    async fn remove_client_returns_name_and_channel() {
        let state = ServerState::new(unlimited());
        state.register_client(addr(1)).await.unwrap();
        let ch = state.create_channel(None).await.unwrap();
        state.join_channel(&addr(1), &ch).await.unwrap();

        let (name, channel) = state.remove_client(&addr(1)).await.unwrap();
        assert!(name.starts_with("user-"));
        assert_eq!(channel.as_deref(), Some(ch.as_str()));
    }

    #[tokio::test]
    async fn remove_nonexistent_client_returns_none() {
        let state = ServerState::new(unlimited());
        assert!(state.remove_client(&addr(99)).await.is_none());
    }

    // ── create channel ───────────────────────────────────────────────

    #[tokio::test]
    async fn create_private_channel() {
        let state = ServerState::new(unlimited());
        let id = state.create_channel(None).await.unwrap();
        assert!(!id.starts_with("pub-"));
        assert_eq!(id.len(), tc_shared::config::CHANNEL_ID_LEN);
    }

    #[tokio::test]
    async fn create_public_channel() {
        let state = ServerState::new(unlimited());
        let id = state.create_channel(Some("lobby")).await.unwrap();
        assert_eq!(id, "pub-lobby");
    }

    #[tokio::test]
    async fn create_duplicate_public_channel_fails() {
        let state = ServerState::new(unlimited());
        state.create_channel(Some("dup")).await.unwrap();
        assert!(state.create_channel(Some("dup")).await.is_err());
    }

    #[tokio::test]
    async fn create_channel_respects_max_channels() {
        let limits = Limits {
            max_channels: 1,
            ..Limits::default()
        };
        let state = ServerState::new(limits);
        assert!(state.create_channel(None).await.is_ok());
        assert!(state.create_channel(None).await.is_err());
    }

    // ── join / leave ─────────────────────────────────────────────────

    #[tokio::test]
    async fn join_channel_returns_participants_token_key() {
        let state = ServerState::new(unlimited());
        state.register_client(addr(1)).await.unwrap();
        let ch = state.create_channel(None).await.unwrap();
        let (participants, token, key) = state.join_channel(&addr(1), &ch).await.unwrap();
        assert_eq!(participants.len(), 1); // self
        assert_ne!(token, 0);
        assert_eq!(key.len(), 32);
    }

    #[tokio::test]
    async fn join_nonexistent_channel_fails() {
        let state = ServerState::new(unlimited());
        state.register_client(addr(1)).await.unwrap();
        assert!(state.join_channel(&addr(1), &"nope".into()).await.is_err());
    }

    #[tokio::test]
    async fn join_while_already_in_channel_fails() {
        let state = ServerState::new(unlimited());
        state.register_client(addr(1)).await.unwrap();
        let ch = state.create_channel(None).await.unwrap();
        state.join_channel(&addr(1), &ch).await.unwrap();
        assert!(state.join_channel(&addr(1), &ch).await.is_err());
    }

    #[tokio::test]
    async fn leave_channel_succeeds() {
        let state = ServerState::new(unlimited());
        state.register_client(addr(1)).await.unwrap();
        let ch = state.create_channel(None).await.unwrap();
        state.join_channel(&addr(1), &ch).await.unwrap();

        let (name, channel_id) = state.leave_channel(&addr(1)).await.unwrap();
        assert!(name.starts_with("user-"));
        assert_eq!(channel_id, ch);
    }

    #[tokio::test]
    async fn leave_when_not_in_channel_returns_none() {
        let state = ServerState::new(unlimited());
        state.register_client(addr(1)).await.unwrap();
        assert!(state.leave_channel(&addr(1)).await.is_none());
    }

    // ── UDP token registration ───────────────────────────────────────

    #[tokio::test]
    async fn register_udp_by_valid_token() {
        let state = ServerState::new(unlimited());
        state.register_client(addr(1)).await.unwrap();
        let ch = state.create_channel(None).await.unwrap();
        let (_, token, _) = state.join_channel(&addr(1), &ch).await.unwrap();

        let udp_addr = addr(5000);
        assert!(state.register_udp_by_token(token, udp_addr).await);

        // Peer should be visible in peer cache
        let mut peers = Vec::new();
        state.fill_channel_peers(&ch, &addr(99), &mut peers);
        assert!(peers.contains(&udp_addr));
    }

    #[tokio::test]
    async fn register_udp_by_invalid_token() {
        let state = ServerState::new(unlimited());
        assert!(!state.register_udp_by_token(12345, addr(5000)).await);
    }

    #[tokio::test]
    async fn register_udp_rejects_ip_mismatch() {
        use std::net::SocketAddr;
        let state = ServerState::new(unlimited());
        let tcp = addr(1); // 127.0.0.1:1
        state.register_client(tcp).await.unwrap();
        let ch = state.create_channel(None).await.unwrap();
        let (_, token, _) = state.join_channel(&tcp, &ch).await.unwrap();

        // UDP hello arrives from a different IP — must be rejected.
        let foreign: SocketAddr = "10.0.0.1:5000".parse().unwrap();
        assert!(!state.register_udp_by_token(token, foreign).await);

        // A mismatch must not burn the token: the real client (correct IP)
        // can still register afterwards.
        assert!(state.register_udp_by_token(token, addr(5000)).await);
    }

    #[tokio::test]
    async fn register_udp_accepts_v4_mapped_tcp_ip() {
        // Dual-stack TCP listener reports the peer as ::ffff:127.0.0.1 while
        // the UDP hello arrives over plain IPv4 — canonical comparison must match.
        let state = ServerState::new(unlimited());
        let tcp: SocketAddr = "[::ffff:127.0.0.1]:1".parse().unwrap();
        state.register_client(tcp).await.unwrap();
        let ch = state.create_channel(None).await.unwrap();
        let (_, token, _) = state.join_channel(&tcp, &ch).await.unwrap();

        assert!(state.register_udp_by_token(token, addr(5000)).await);
    }

    #[tokio::test]
    async fn register_udp_rehello_reacks_and_replaces_stale_addr() {
        let state = ServerState::new(unlimited());
        state.register_client(addr(1)).await.unwrap();
        let ch = state.create_channel(None).await.unwrap();
        let (_, token, _) = state.join_channel(&addr(1), &ch).await.unwrap();

        // First hello registers; a duplicate (lost ACK) is re-acknowledged.
        assert!(state.register_udp_by_token(token, addr(5000)).await);
        assert!(state.register_udp_by_token(token, addr(5000)).await);

        // Hello from a new source port (NAT rebind) replaces the stale address.
        assert!(state.register_udp_by_token(token, addr(5001)).await);
        let mut peers = Vec::new();
        state.fill_channel_peers(&ch, &addr(99), &mut peers);
        assert_eq!(peers, vec![addr(5001)]);
    }

    // ── peer cache ───────────────────────────────────────────────────

    #[tokio::test]
    async fn fill_channel_peers_excludes_sender() {
        let state = ServerState::new(unlimited());
        state.register_client(addr(1)).await.unwrap();
        state.register_client(addr(2)).await.unwrap();
        let ch = state.create_channel(None).await.unwrap();
        let (_, t1, _) = state.join_channel(&addr(1), &ch).await.unwrap();
        let (_, t2, _) = state.join_channel(&addr(2), &ch).await.unwrap();

        let udp1 = addr(5001);
        let udp2 = addr(5002);
        state.register_udp_by_token(t1, udp1).await;
        state.register_udp_by_token(t2, udp2).await;

        let mut peers = Vec::new();
        state.fill_channel_peers(&ch, &udp1, &mut peers);
        assert_eq!(peers, vec![udp2]);
    }

    // ── echo test ────────────────────────────────────────────────────

    #[tokio::test]
    async fn start_echo_test_returns_prefixed_channel_token_key() {
        let state = ServerState::new(unlimited());
        state.register_client(addr(1)).await.unwrap();
        let (channel_id, token, key) = state.start_echo_test(&addr(1)).await.unwrap();
        assert!(channel_id.starts_with(tc_shared::config::ECHO_CHANNEL_PREFIX));
        assert_ne!(token, 0);
        assert_eq!(key.len(), 32);
    }

    #[tokio::test]
    async fn start_echo_test_rejected_when_in_channel() {
        let state = ServerState::new(unlimited());
        state.register_client(addr(1)).await.unwrap();
        let ch = state.create_channel(None).await.unwrap();
        state.join_channel(&addr(1), &ch).await.unwrap();
        assert!(state.start_echo_test(&addr(1)).await.is_err());
    }

    /// The tester's registered address must be reflected back to itself — an
    /// echo channel includes the sender (no exclude), unlike a normal relay.
    #[tokio::test]
    async fn echo_channel_peer_includes_sender() {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        let state = ServerState::new(unlimited());
        state.register_client(addr(1)).await.unwrap();
        let (channel_id, token, _) = state.start_echo_test(&addr(1)).await.unwrap();
        let udp = addr(5001);
        assert!(state.register_udp_by_token(token, udp).await);

        // No-exclude sentinel (mirrors the relay): the sender stays in the list.
        let no_exclude = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
        let mut peers = Vec::new();
        state.fill_channel_peers(&channel_id, &no_exclude, &mut peers);
        assert_eq!(peers, vec![udp]);
    }

    // ── rename ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn rename_client_returns_old_name() {
        let state = ServerState::new(unlimited());
        let name = state.register_client(addr(1)).await.unwrap();
        let (old, ch) = state.rename_client(&addr(1), "alice".into()).await.unwrap();
        assert_eq!(old, name);
        assert!(ch.is_none());
    }

    #[tokio::test]
    async fn rename_nonexistent_client_returns_none() {
        let state = ServerState::new(unlimited());
        assert!(state.rename_client(&addr(99), "bob".into()).await.is_none());
    }

    // ── get_client ───────────────────────────────────────────────────

    #[tokio::test]
    async fn get_client_returns_info() {
        let state = ServerState::new(unlimited());
        let name = state.register_client(addr(1)).await.unwrap();
        let info = state.get_client(&addr(1)).await.unwrap();
        assert_eq!(info.name, name);
        assert!(info.channel.is_none());
    }

    #[tokio::test]
    async fn get_nonexistent_client_returns_none() {
        let state = ServerState::new(unlimited());
        assert!(state.get_client(&addr(99)).await.is_none());
    }

    // ── list public channels ─────────────────────────────────────────

    #[tokio::test]
    async fn list_public_channels_filters_private() {
        let state = ServerState::new(unlimited());
        state.create_channel(None).await.unwrap();
        state.create_channel(Some("pub1")).await.unwrap();
        state.create_channel(Some("pub2")).await.unwrap();

        let public = state.list_public_channels().await;
        assert_eq!(public.len(), 2);
        assert!(public.iter().all(|(id, _)| id.starts_with("pub-")));
    }

    // ── cleanup ──────────────────────────────────────────────────────

    // Cleanup tests drive `cleanup_with` using zero grace/TTL instead of
    // back-dating Instants: `Instant::now() - 300s` panics on Windows CI
    // runners whose uptime is shorter than the subtracted duration.

    #[tokio::test]
    async fn cleanup_removes_empty_channels_after_grace() {
        let state = ServerState::new(unlimited());
        let _ch = state.create_channel(None).await.unwrap();

        // Channel just created — grace period should protect it
        let removed = state.cleanup_empty_channels().await;
        assert_eq!(removed.len(), 0);

        // With the grace elapsed (zero grace), the empty channel goes away.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let removed = state
            .cleanup_with(
                std::time::Duration::ZERO,
                std::time::Duration::from_secs(30),
            )
            .await;
        assert_eq!(removed.len(), 1);
    }

    #[tokio::test]
    async fn cleanup_keeps_occupied_channels() {
        let state = ServerState::new(unlimited());
        state.register_client(addr(1)).await.unwrap();
        let ch = state.create_channel(None).await.unwrap();
        state.join_channel(&addr(1), &ch).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let removed = state
            .cleanup_with(
                std::time::Duration::ZERO,
                std::time::Duration::from_secs(30),
            )
            .await;
        assert_eq!(removed.len(), 0);
    }

    /// A token of a connected client that is still in the token's channel must
    /// survive cleanup indefinitely — the client's idle keepalive re-hellos
    /// with it to refresh its registered address after NAT rebinds.
    #[tokio::test]
    async fn cleanup_keeps_tokens_of_live_in_channel_sessions() {
        let state = ServerState::new(unlimited());
        state.register_client(addr(1)).await.unwrap();
        let ch = state.create_channel(None).await.unwrap();
        let (_, token, _) = state.join_channel(&addr(1), &ch).await.unwrap();
        state.register_udp_by_token(token, addr(5001)).await;

        // Zero TTL: any token age is "expired" — liveness alone must keep it.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        state
            .cleanup_with(
                std::time::Duration::from_secs(300),
                std::time::Duration::ZERO,
            )
            .await;

        // Re-hello from a new source port (NAT rebind) must still register.
        assert!(state.register_udp_by_token(token, addr(5002)).await);
    }

    #[tokio::test]
    async fn cleanup_expires_aged_tokens_of_sessions_out_of_channel() {
        let state = ServerState::new(unlimited());
        state.register_client(addr(1)).await.unwrap();
        let ch = state.create_channel(None).await.unwrap();
        let (_, token, _) = state.join_channel(&addr(1), &ch).await.unwrap();

        // Simulate a stale token whose session is no longer in that channel.
        {
            let mut inner = state.inner.write().await;
            inner.clients.get_mut(&addr(1)).unwrap().channel = None;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        state
            .cleanup_with(
                std::time::Duration::from_secs(300),
                std::time::Duration::ZERO,
            )
            .await;

        assert!(!state.register_udp_by_token(token, addr(5001)).await);
    }

    // ── stats ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn stats_returns_correct_counts() {
        let state = ServerState::new(unlimited());
        state.register_client(addr(1)).await.unwrap();
        state.register_client(addr(2)).await.unwrap();
        let ch = state.create_channel(None).await.unwrap();
        let (_, token, _) = state.join_channel(&addr(1), &ch).await.unwrap();
        state.register_udp_by_token(token, addr(5001)).await;

        let s = state.stats().await;
        assert_eq!(s.clients, 2);
        assert_eq!(s.channels, 1);
        assert_eq!(s.udp_peers, 1);
    }

    // ── broadcast_channel ────────────────────────────────────────────

    #[tokio::test]
    async fn broadcast_channel_excludes_sender() {
        let state = ServerState::new(unlimited());
        state.register_client(addr(1)).await.unwrap();
        state.register_client(addr(2)).await.unwrap();
        let ch = state.create_channel(None).await.unwrap();
        state.join_channel(&addr(1), &ch).await.unwrap();
        state.join_channel(&addr(2), &ch).await.unwrap();

        let mut received = Vec::new();
        state
            .broadcast_channel_addrs(&ch, Some(&addr(1)), |a| {
                received.push(*a);
            })
            .await;

        assert_eq!(received.len(), 1);
        assert_eq!(received[0], addr(2));
    }

    // ── remove client cleans up channel ──────────────────────────────

    #[tokio::test]
    async fn remove_last_client_deletes_channel() {
        let state = ServerState::new(unlimited());
        state.register_client(addr(1)).await.unwrap();
        let ch = state.create_channel(None).await.unwrap();
        state.join_channel(&addr(1), &ch).await.unwrap();

        state.remove_client(&addr(1)).await;

        // Channel should be gone
        let s = state.stats().await;
        assert_eq!(s.channels, 0);
    }

    // ── persistent (pre-created) public channels ─────────────────────

    #[tokio::test]
    async fn persistent_channel_survives_last_client_leaving() {
        let state = ServerState::new(unlimited());
        let ch = state.create_persistent_channel("lobby").await.unwrap();
        state.register_client(addr(1)).await.unwrap();
        state.join_channel(&addr(1), &ch).await.unwrap();

        state.remove_client(&addr(1)).await;

        // Empty, but a persistent channel is not reaped — still listed.
        let s = state.stats().await;
        assert_eq!(s.channels, 1);
        let public = state.list_public_channels().await;
        assert_eq!(public, vec![(ch, 0)]);
    }

    #[tokio::test]
    async fn persistent_channel_survives_cleanup() {
        let state = ServerState::new(unlimited());
        let _ch = state.create_persistent_channel("general").await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let removed = state
            .cleanup_with(
                std::time::Duration::ZERO,
                std::time::Duration::from_secs(30),
            )
            .await;
        assert_eq!(removed.len(), 0);
        assert_eq!(state.stats().await.channels, 1);
    }

    #[tokio::test]
    async fn persistent_channel_rotates_key_on_rejoin() {
        let state = ServerState::new(unlimited());
        let ch = state.create_persistent_channel("music").await.unwrap();

        state.register_client(addr(1)).await.unwrap();
        let (_, _, key1) = state.join_channel(&addr(1), &ch).await.unwrap();
        state.remove_client(&addr(1)).await;

        // Re-entering the now-empty persistent channel mints a fresh voice key.
        state.register_client(addr(2)).await.unwrap();
        let (_, _, key2) = state.join_channel(&addr(2), &ch).await.unwrap();

        assert_eq!(key1.len(), 32);
        assert_eq!(key2.len(), 32);
        assert_ne!(key1, key2, "voice key must rotate across sessions");
    }
}
