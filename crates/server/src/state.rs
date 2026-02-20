use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tc_shared::ChannelId;

/// Lock-free-read cache of channel → UDP peer addresses.
/// Rebuilt on join/leave/register_udp. UDP relay reads this without
/// touching the main tokio::RwLock, eliminating contention on the hot path.
type PeerCache = Arc<std::sync::RwLock<HashMap<String, Vec<SocketAddr>>>>;

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
}

/// Global server state shared across tasks.
#[derive(Clone)]
pub struct ServerState {
    inner: Arc<RwLock<Inner>>,
    peer_cache: PeerCache,
}

#[derive(Debug)]
struct Inner {
    /// channel_id → set of participant addresses (UDP).
    channels: HashMap<ChannelId, HashSet<SocketAddr>>,
    /// Maps a TCP peer address → (assigned name, current channel).
    clients: HashMap<SocketAddr, ClientInfo>,
    /// Per-channel voice encryption keys.
    channel_keys: HashMap<ChannelId, Vec<u8>>,
    /// Channel creation timestamps (grace period for cleanup).
    channel_created_at: HashMap<ChannelId, Instant>,
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
}

impl ServerState {
    pub fn new(limits: Limits) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner {
                channels: HashMap::new(),
                channel_keys: HashMap::new(),
                channel_created_at: HashMap::new(),
                clients: HashMap::new(),
                udp_tokens: HashMap::new(),
                name_counter: 0,
                limits,
            })),
            peer_cache: Arc::new(std::sync::RwLock::new(HashMap::new())),
        }
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
            },
        );
        Ok(name)
    }

    /// Remove client, returns (name, channel_id) if was in a channel.
    pub async fn remove_client(&self, tcp_addr: &SocketAddr) -> Option<(String, Option<ChannelId>)> {
        let mut inner = self.inner.write().await;
        let info = inner.clients.remove(tcp_addr)?;
        // Remove any pending UDP tokens for this client
        inner.udp_tokens.retain(|_, (addr, _, _)| addr != tcp_addr);
        if let Some(ref ch) = info.channel {
            if let Some(participants) = inner.channels.get_mut(ch) {
                if let Some(udp) = info.udp_addr {
                    participants.remove(&udp);
                }
                if participants.is_empty() {
                    inner.channels.remove(ch);
                    inner.channel_keys.remove(ch);
                    inner.channel_created_at.remove(ch);
                }
            }
            Self::rebuild_peer_cache(&inner, &self.peer_cache);
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

    /// List public channels with participant counts.
    pub async fn list_public_channels(&self) -> Vec<(ChannelId, u32)> {
        let inner = self.inner.read().await;
        let prefix = tc_shared::config::PUBLIC_CHANNEL_PREFIX;
        inner
            .channels
            .iter()
            .filter(|(id, _)| id.starts_with(prefix))
            .map(|(id, _udp_participants)| {
                // Count TCP clients in this channel (more accurate than UDP set)
                let tcp_count = inner
                    .clients
                    .values()
                    .filter(|c| c.channel.as_deref() == Some(id.as_str()))
                    .count();
                (id.clone(), tcp_count as u32)
            })
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
        let client = inner
            .clients
            .get_mut(tcp_addr)
            .ok_or("client not registered")?;
        if client.channel.is_some() {
            return Err("already in a channel, leave first".into());
        }
        client.channel = Some(channel_id.clone());

        // Generate a unique UDP token
        let token = loop {
            let t = rand::random::<u64>();
            if t != 0 && !inner.udp_tokens.contains_key(&t) {
                break t;
            }
        };
        inner.udp_tokens.insert(token, (*tcp_addr, channel_id.clone(), Instant::now()));

        // Gather existing participant names
        let names: Vec<String> = inner
            .clients
            .values()
            .filter(|c| c.channel.as_deref() == Some(channel_id.as_str()))
            .map(|c| c.name.clone())
            .collect();

        let voice_key = inner.channel_keys.get(channel_id).cloned().unwrap_or_default();
        Ok((names, token, voice_key))
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
        if let Some(participants) = inner.channels.get_mut(&channel_id) {
            if let Some(udp) = udp_addr {
                participants.remove(&udp);
            }
            if participants.is_empty() {
                inner.channels.remove(&channel_id);
                inner.channel_keys.remove(&channel_id);
                inner.channel_created_at.remove(&channel_id);
            }
        }
        Self::rebuild_peer_cache(&inner, &self.peer_cache);
        Some((name, channel_id))
    }

    /// Register a UDP address using a hello token.
    /// Returns true if the token was valid and registration succeeded.
    pub async fn register_udp_by_token(&self, token: u64, udp_addr: SocketAddr) -> bool {
        let mut inner = self.inner.write().await;
        let (tcp_addr, channel_id, _) = match inner.udp_tokens.remove(&token) {
            Some(v) => v,
            None => return false,
        };
        if let Some(client) = inner.clients.get_mut(&tcp_addr) {
            client.udp_addr = Some(udp_addr);
        }
        if let Some(participants) = inner.channels.get_mut(&channel_id) {
            participants.insert(udp_addr);
        }
        Self::rebuild_peer_cache(&inner, &self.peer_cache);
        true
    }

    /// Fill a reusable buffer with channel peers (excluding sender).
    /// Zero-allocation on the hot path when the Vec has enough capacity.
    pub fn fill_channel_peers(&self, channel_id: &str, exclude: &SocketAddr, out: &mut Vec<SocketAddr>) {
        out.clear();
        let cache = self.peer_cache.read().unwrap_or_else(|e| e.into_inner());
        if let Some(peers) = cache.get(channel_id) {
            out.extend(peers.iter().filter(|a| *a != exclude).copied());
        }
    }

    /// Rebuild the peer cache from current channel state.
    /// Called after join/leave/register_udp — these are rare events.
    fn rebuild_peer_cache(inner: &Inner, cache: &PeerCache) {
        let mut new_cache = HashMap::with_capacity(inner.channels.len());
        for (id, participants) in &inner.channels {
            if !participants.is_empty() {
                new_cache.insert(id.clone(), participants.iter().copied().collect());
            }
        }
        *cache.write().unwrap_or_else(|e| e.into_inner()) = new_cache;
    }

    /// Iterate channel members under a single read lock, calling `send_fn` for each.
    /// Avoids intermediate Vec allocation and extra lock round-trip.
    pub async fn broadcast_channel<F>(
        &self,
        channel_id: &str,
        exclude: Option<&SocketAddr>,
        msg: &tc_shared::ServerMessage,
        mut send_fn: F,
    ) where
        F: FnMut(&SocketAddr, tc_shared::ServerMessage),
    {
        let inner = self.inner.read().await;
        for (addr, info) in &inner.clients {
            if info.channel.as_deref() != Some(channel_id) {
                continue;
            }
            if exclude.is_some_and(|ex| ex == addr) {
                continue;
            }
            send_fn(addr, msg.clone());
        }
    }

    /// Rename a client. Returns (old_name, channel_id) if successful.
    pub async fn rename_client(&self, tcp_addr: &SocketAddr, new_name: String) -> Option<(String, Option<ChannelId>)> {
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

    /// Remove empty channels (no participants). Returns count of removed channels.
    /// Newly created channels get a grace period (2× maintenance interval) before
    /// being eligible for cleanup, so creators have time to join.
    pub async fn cleanup_empty_channels(&self) -> usize {
        let grace = std::time::Duration::from_secs(
            tc_shared::config::MAINTENANCE_INTERVAL_SECS * 2,
        );
        let mut inner = self.inner.write().await;
        let empty: Vec<ChannelId> = inner
            .channels
            .iter()
            .filter(|(id, participants)| {
                participants.is_empty()
                    && !inner
                        .clients
                        .values()
                        .any(|c| c.channel.as_deref() == Some(id.as_str()))
                    && inner
                        .channel_created_at
                        .get(id.as_str())
                        .map(|t| t.elapsed() > grace)
                        .unwrap_or(true)
            })
            .map(|(id, _)| id.clone())
            .collect();
        let count = empty.len();
        for id in empty {
            inner.channels.remove(&id);
            inner.channel_keys.remove(&id);
            inner.channel_created_at.remove(&id);
            // Also remove any stale tokens for this channel
            inner.udp_tokens.retain(|_, (_, ch, _)| ch != &id);
        }
        // Expire UDP tokens older than 30 seconds
        let token_ttl = std::time::Duration::from_secs(30);
        inner.udp_tokens.retain(|_, (_, _, created)| created.elapsed() < token_ttl);
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unlimited() -> Limits {
        Limits { max_channels: 0, max_clients: 0 }
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
        let limits = Limits { max_channels: 0, max_clients: 1 };
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
        let limits = Limits { max_channels: 1, max_clients: 0 };
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

    #[tokio::test]
    async fn cleanup_removes_empty_channels_after_grace() {
        let state = ServerState::new(unlimited());
        let ch = state.create_channel(None).await.unwrap();

        // Channel just created — grace period should protect it
        let removed = state.cleanup_empty_channels().await;
        assert_eq!(removed, 0);

        // Backdate channel creation to bypass grace
        {
            let mut inner = state.inner.write().await;
            inner.channel_created_at.insert(
                ch.clone(),
                Instant::now() - std::time::Duration::from_secs(300),
            );
        }

        let removed = state.cleanup_empty_channels().await;
        assert_eq!(removed, 1);
    }

    #[tokio::test]
    async fn cleanup_keeps_occupied_channels() {
        let state = ServerState::new(unlimited());
        state.register_client(addr(1)).await.unwrap();
        let ch = state.create_channel(None).await.unwrap();
        state.join_channel(&addr(1), &ch).await.unwrap();

        // Backdate
        {
            let mut inner = state.inner.write().await;
            inner.channel_created_at.insert(
                ch.clone(),
                Instant::now() - std::time::Duration::from_secs(300),
            );
        }

        let removed = state.cleanup_empty_channels().await;
        assert_eq!(removed, 0);
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
        state.broadcast_channel(&ch, Some(&addr(1)), &tc_shared::ServerMessage::Pong, |a, _| {
            received.push(*a);
        }).await;

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
}
