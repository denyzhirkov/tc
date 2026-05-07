use std::net::SocketAddr;

use anyhow::Result;
use tokio::net::UdpSocket;

use tc_shared::config;
use tc_shared::{decode_udp_hello, encode_udp_hello, VoicePacket};

use crate::state::ServerState;

// ── Linux: batch UDP send via sendmmsg ──────────────────────────────

#[cfg(target_os = "linux")]
struct BatchSender {
    sockaddrs: Vec<libc::sockaddr_storage>,
    addrlen: Vec<libc::socklen_t>,
    /// Single shared iovec — all mmsghdr point at it. Updated per-call with current data ptr/len.
    iov: libc::iovec,
    msgs: Vec<libc::mmsghdr>,
    /// Cached peer-set; if unchanged, we skip sockaddr/msg rebuild.
    cached_addrs: Vec<SocketAddr>,
}

// SAFETY: BatchSender is used from a single tokio task. The raw pointers
// in iovec/mmsghdr are only valid during send_to_all and never escape.
#[cfg(target_os = "linux")]
unsafe impl Send for BatchSender {}

#[cfg(target_os = "linux")]
impl BatchSender {
    fn new() -> Self {
        Self {
            sockaddrs: Vec::with_capacity(16),
            addrlen: Vec::with_capacity(16),
            iov: libc::iovec {
                iov_base: std::ptr::null_mut(),
                iov_len: 0,
            },
            msgs: Vec::with_capacity(16),
            cached_addrs: Vec::with_capacity(16),
        }
    }

    /// Send `data` to all `addrs` in a single syscall.
    /// Pre-allocated buffers are reused — zero allocation after warmup.
    /// When `addrs` matches the previous call, sockaddr/mmsghdr arrays are reused as-is.
    fn send_to_all(&mut self, socket: &UdpSocket, data: &[u8], addrs: &[SocketAddr]) {
        use std::os::unix::io::AsRawFd;

        if addrs.is_empty() {
            return;
        }

        let n = addrs.len();
        let fd = socket.as_raw_fd();

        // Update the single shared iovec (pointed to by every mmsghdr).
        self.iov.iov_base = data.as_ptr() as *mut libc::c_void;
        self.iov.iov_len = data.len();

        // Rebuild sockaddrs/msgs only if peer-set changed.
        if self.cached_addrs.as_slice() != addrs {
            self.rebuild(addrs);
        }

        // SAFETY: msg_iov is &mut iov which lives in self for the whole call.
        unsafe {
            libc::sendmmsg(fd, self.msgs.as_mut_ptr(), n as u32, libc::MSG_DONTWAIT as _);
        }
    }

    fn rebuild(&mut self, addrs: &[SocketAddr]) {
        let n = addrs.len();
        self.sockaddrs.clear();
        self.sockaddrs.resize(n, unsafe { std::mem::zeroed() });
        self.addrlen.clear();
        self.addrlen.resize(n, 0);

        for (i, addr) in addrs.iter().enumerate() {
            match addr {
                SocketAddr::V4(v4) => {
                    let sa = unsafe {
                        &mut *(&mut self.sockaddrs[i] as *mut libc::sockaddr_storage
                            as *mut libc::sockaddr_in)
                    };
                    sa.sin_family = libc::AF_INET as libc::sa_family_t;
                    sa.sin_port = v4.port().to_be();
                    sa.sin_addr = libc::in_addr {
                        s_addr: u32::from(*v4.ip()).to_be(),
                    };
                    self.addrlen[i] =
                        std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
                }
                SocketAddr::V6(v6) => {
                    let sa = unsafe {
                        &mut *(&mut self.sockaddrs[i] as *mut libc::sockaddr_storage
                            as *mut libc::sockaddr_in6)
                    };
                    sa.sin6_family = libc::AF_INET6 as libc::sa_family_t;
                    sa.sin6_port = v6.port().to_be();
                    sa.sin6_addr = libc::in6_addr {
                        s6_addr: v6.ip().octets(),
                    };
                    sa.sin6_flowinfo = v6.flowinfo();
                    sa.sin6_scope_id = v6.scope_id();
                    self.addrlen[i] =
                        std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t;
                }
            }
        }

        // sockaddrs is done growing → element addresses are stable.
        let iov_ptr = &mut self.iov as *mut libc::iovec;
        self.msgs.clear();
        for i in 0..n {
            let mut msg: libc::mmsghdr = unsafe { std::mem::zeroed() };
            msg.msg_hdr.msg_name =
                &mut self.sockaddrs[i] as *mut libc::sockaddr_storage as *mut libc::c_void;
            msg.msg_hdr.msg_namelen = self.addrlen[i];
            msg.msg_hdr.msg_iov = iov_ptr;
            msg.msg_hdr.msg_iovlen = 1;
            self.msgs.push(msg);
        }

        self.cached_addrs.clear();
        self.cached_addrs.extend_from_slice(addrs);
    }
}

// ── Non-Linux: loop with try_send_to ────────────────────────────────
// Simple per-peer loop. Fine for small channels (< 20 peers).
// For higher fan-out, consider platform-specific batch APIs
// (e.g. sendmsg_x on macOS 11+) or io_uring on Linux.

#[cfg(not(target_os = "linux"))]
fn send_to_all(socket: &UdpSocket, data: &[u8], addrs: &[SocketAddr]) {
    for &addr in addrs {
        let _ = socket.try_send_to(data, addr);
    }
}

// ── UDP relay loop ──────────────────────────────────────────────────

/// Start the UDP voice relay server.
pub async fn run_udp_relay(state: ServerState, addr: String) -> Result<()> {
    let socket = UdpSocket::bind(&addr).await?;
    tracing::info!("UDP listening on {}", addr);

    let mut buf = vec![0u8; config::MAX_UDP_PACKET + 64];
    // Reusable peer list — avoids allocation on every packet
    let mut peers = Vec::<SocketAddr>::with_capacity(16);

    #[cfg(target_os = "linux")]
    let mut batch = BatchSender::new();

    loop {
        let (len, src_addr) = match socket.recv_from(&mut buf).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("UDP recv error: {}", e);
                continue;
            }
        };

        let data = &buf[..len];

        // Check for hello packet (token-based UDP registration)
        if let Some(token) = decode_udp_hello(data) {
            if state.register_udp_by_token(token, src_addr).await {
                tracing::debug!(%src_addr, "UDP hello registered via token");
                // Send ACK (echo the hello back)
                let ack = encode_udp_hello(token);
                let _ = socket.send_to(&ack, src_addr).await;
            } else {
                tracing::debug!(%src_addr, "UDP hello with invalid token");
            }
            continue;
        }

        // Parse only the channel_id without copying opus_data
        let channel_id = match VoicePacket::parse_channel_id(data) {
            Some(id) => id,
            None => continue,
        };

        // Fill reusable peer buffer (no allocation when capacity suffices)
        state.fill_channel_peers(channel_id, &src_addr, &mut peers);

        // Relay raw bytes to all other participants in the channel
        #[cfg(target_os = "linux")]
        batch.send_to_all(&socket, data, &peers);

        #[cfg(not(target_os = "linux"))]
        send_to_all(&socket, data, &peers);
    }
}
