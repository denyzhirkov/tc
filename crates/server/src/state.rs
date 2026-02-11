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
    pub async fn create_channel(&self) -> Result<ChannelId, String> {
        let mut inner = self.inner.write().await;
        if inner.limits.max_channels > 0 && inner.channels.len() >= inner.limits.max_channels {
            return Err("channel limit reached".into());
        }
        loop {
            let id = tc_shared::generate_channel_id();
            if !inner.channels.contains_key(&id) {
                inner.channels.insert(id.clone(), HashSet::new());
                let key: [u8; 32] = rand::random();
                inner.channel_keys.insert(id.clone(), key.to_vec());
                inner.channel_created_at.insert(id.clone(), Instant::now());
                return Ok(id);
            }
        }
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

    /// Get all UDP addresses in a channel except the sender.
    /// Reads from the lock-free peer cache — no tokio RwLock on the hot path.
    pub fn get_channel_peers_cached(&self, channel_id: &str, exclude: &SocketAddr) -> Vec<SocketAddr> {
        let cache = self.peer_cache.read().unwrap();
        cache
            .get(channel_id)
            .map(|peers| peers.iter().filter(|a| *a != exclude).copied().collect())
            .unwrap_or_default()
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
        *cache.write().unwrap() = new_cache;
    }

    /// Get all TCP addresses in a channel (for broadcasting control messages).
    pub async fn get_channel_tcp_addrs(&self, channel_id: &str) -> Vec<SocketAddr> {
        let inner = self.inner.read().await;
        inner
            .clients
            .iter()
            .filter(|(_, info)| info.channel.as_deref() == Some(channel_id))
            .map(|(addr, _)| *addr)
            .collect()
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
