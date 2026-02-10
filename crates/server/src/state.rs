use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use termicall_shared::ChannelId;

/// Global server state shared across tasks.
#[derive(Debug, Clone)]
pub struct ServerState {
    inner: Arc<RwLock<Inner>>,
}

#[derive(Debug, Default)]
struct Inner {
    /// channel_id → set of participant addresses (UDP).
    channels: HashMap<ChannelId, HashSet<SocketAddr>>,
    /// Maps a TCP peer address → (assigned name, current channel).
    clients: HashMap<SocketAddr, ClientInfo>,
    /// Counter for generating simple peer names.
    name_counter: u64,
}

#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub name: String,
    pub channel: Option<ChannelId>,
    pub udp_addr: Option<SocketAddr>,
}

impl ServerState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner::default())),
        }
    }

    /// Register a new TCP client, returns assigned name.
    pub async fn register_client(&self, tcp_addr: SocketAddr) -> String {
        let mut inner = self.inner.write().await;
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
        name
    }

    /// Remove client, returns (name, channel_id) if was in a channel.
    pub async fn remove_client(&self, tcp_addr: &SocketAddr) -> Option<(String, Option<ChannelId>)> {
        let mut inner = self.inner.write().await;
        let info = inner.clients.remove(tcp_addr)?;
        if let Some(ref ch) = info.channel {
            if let Some(participants) = inner.channels.get_mut(ch) {
                if let Some(udp) = info.udp_addr {
                    participants.remove(&udp);
                }
                if participants.is_empty() {
                    inner.channels.remove(ch);
                }
            }
        }
        Some((info.name, info.channel))
    }

    /// Create a new channel, returns channel_id.
    pub async fn create_channel(&self) -> ChannelId {
        let mut inner = self.inner.write().await;
        let id = termicall_shared::generate_channel_id();
        inner.channels.insert(id.clone(), HashSet::new());
        id
    }

    /// Join a channel. Returns list of existing participant names.
    pub async fn join_channel(
        &self,
        tcp_addr: &SocketAddr,
        channel_id: &ChannelId,
    ) -> Result<Vec<String>, String> {
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

        // Gather existing participant names
        let names: Vec<String> = inner
            .clients
            .values()
            .filter(|c| c.channel.as_deref() == Some(channel_id.as_str()))
            .map(|c| c.name.clone())
            .collect();

        Ok(names)
    }

    /// Leave current channel.
    pub async fn leave_channel(&self, tcp_addr: &SocketAddr) -> Option<(String, ChannelId)> {
        let mut inner = self.inner.write().await;
        let client = inner.clients.get(tcp_addr)?;
        let channel_id = client.channel.clone()?;
        let name = client.name.clone();
        let udp_addr = client.udp_addr;

        // Now mutate separately
        inner.clients.get_mut(tcp_addr).unwrap().channel = None;
        if let Some(participants) = inner.channels.get_mut(&channel_id) {
            if let Some(udp) = udp_addr {
                participants.remove(&udp);
            }
            if participants.is_empty() {
                inner.channels.remove(&channel_id);
            }
        }
        Some((name, channel_id))
    }

    /// Set UDP address for a client.
    pub async fn set_udp_addr(&self, tcp_addr: &SocketAddr, udp_addr: SocketAddr) {
        let mut inner = self.inner.write().await;
        let channel = inner
            .clients
            .get(tcp_addr)
            .and_then(|c| c.channel.clone());

        if let Some(client) = inner.clients.get_mut(tcp_addr) {
            client.udp_addr = Some(udp_addr);
        }
        if let Some(ch) = channel {
            if let Some(participants) = inner.channels.get_mut(&ch) {
                participants.insert(udp_addr);
            }
        }
    }

    /// Get all UDP addresses in a channel except the sender.
    pub async fn get_channel_peers(&self, channel_id: &str, exclude: &SocketAddr) -> Vec<SocketAddr> {
        let inner = self.inner.read().await;
        inner
            .channels
            .get(channel_id)
            .map(|peers| peers.iter().filter(|a| *a != exclude).copied().collect())
            .unwrap_or_default()
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

    /// Get client info by TCP address.
    pub async fn get_client(&self, tcp_addr: &SocketAddr) -> Option<ClientInfo> {
        let inner = self.inner.read().await;
        inner.clients.get(tcp_addr).cloned()
    }
}
