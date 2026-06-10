use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::Result;
use tokio::net::UdpSocket;

use tc_shared::config;
use tc_shared::{decode_udp_hello, encode_udp_hello, VoicePacket};

use crate::metrics::Metrics;
use crate::state::ServerState;

// ── SO_REUSEPORT bind (Linux only) ──────────────────────────────────
//
// On Linux 3.9+ multiple sockets may bind to the same UDP port if all of them
// set SO_REUSEPORT. The kernel hashes incoming datagrams across the bound
// sockets by 4-tuple, giving free horizontal scaling — N workers each owning
// one socket can saturate N cores without coordination.
//
// macOS/BSD have a flag with the same name but different semantics
// (only one socket actually receives), so we stay single-socket there.

#[cfg(target_os = "linux")]
fn bind_reuseport(addr: SocketAddr) -> Result<UdpSocket> {
    use std::os::unix::io::FromRawFd;

    let domain = match addr {
        SocketAddr::V4(_) => libc::AF_INET,
        SocketAddr::V6(_) => libc::AF_INET6,
    };
    let fd = unsafe {
        libc::socket(
            domain,
            libc::SOCK_DGRAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }

    // Helper: setsockopt int. On error, close fd and return.
    let setopt_int = |opt: libc::c_int| -> Result<()> {
        let v: libc::c_int = 1;
        let rc = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                opt,
                &v as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        if rc != 0 {
            let e = std::io::Error::last_os_error();
            return Err(e.into());
        }
        Ok(())
    };

    if let Err(e) = setopt_int(libc::SO_REUSEADDR).and_then(|_| setopt_int(libc::SO_REUSEPORT)) {
        unsafe { libc::close(fd) };
        return Err(e);
    }

    let bind_rc = match addr {
        SocketAddr::V4(v4) => {
            let mut sa: libc::sockaddr_in = unsafe { std::mem::zeroed() };
            sa.sin_family = libc::AF_INET as libc::sa_family_t;
            sa.sin_port = v4.port().to_be();
            sa.sin_addr.s_addr = u32::from(*v4.ip()).to_be();
            unsafe {
                libc::bind(
                    fd,
                    &sa as *const _ as *const libc::sockaddr,
                    std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                )
            }
        }
        SocketAddr::V6(v6) => {
            let mut sa: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
            sa.sin6_family = libc::AF_INET6 as libc::sa_family_t;
            sa.sin6_port = v6.port().to_be();
            sa.sin6_addr.s6_addr = v6.ip().octets();
            sa.sin6_flowinfo = v6.flowinfo();
            sa.sin6_scope_id = v6.scope_id();
            unsafe {
                libc::bind(
                    fd,
                    &sa as *const _ as *const libc::sockaddr,
                    std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
                )
            }
        }
    };
    if bind_rc != 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(e.into());
    }

    let std_sock = unsafe { std::net::UdpSocket::from_raw_fd(fd) };
    Ok(UdpSocket::from_std(std_sock)?)
}

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
            libc::sendmmsg(
                fd,
                self.msgs.as_mut_ptr(),
                n as u32,
                libc::MSG_DONTWAIT as _,
            );
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
                    self.addrlen[i] = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
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
                    self.addrlen[i] = std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t;
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

// ── macOS: batch UDP send via sendmsg_x ─────────────────────────────
// `sendmsg_x` is a Darwin SPI exported by libsystem_kernel; it accepts an
// array of msghdr_x and emits each as a separate datagram in one syscall.
// Used internally by Apple's networking stack. Stable since macOS 11.
//
// On any error or partial result we transparently fall back to the
// per-peer try_send_to loop, so this is purely an optimisation.

#[cfg(target_os = "macos")]
#[repr(C)]
struct MsghdrX {
    msg_name: *mut libc::c_void,
    msg_namelen: libc::socklen_t,
    msg_iov: *mut libc::iovec,
    msg_iovlen: libc::c_int,
    msg_control: *mut libc::c_void,
    msg_controllen: libc::socklen_t,
    msg_flags: libc::c_int,
    msg_datalen: libc::size_t,
}

#[cfg(target_os = "macos")]
extern "C" {
    fn sendmsg_x(
        s: libc::c_int,
        msgp: *const MsghdrX,
        cnt: libc::c_uint,
        flags: libc::c_int,
    ) -> isize;
}

#[cfg(target_os = "macos")]
struct BatchSender {
    sockaddrs: Vec<libc::sockaddr_storage>,
    addrlen: Vec<libc::socklen_t>,
    iov: libc::iovec,
    msgs: Vec<MsghdrX>,
    cached_addrs: Vec<SocketAddr>,
}

// SAFETY: BatchSender is used from a single tokio task. Raw pointers in
// iovec/MsghdrX are only valid during send_to_all and never escape.
#[cfg(target_os = "macos")]
unsafe impl Send for BatchSender {}

#[cfg(target_os = "macos")]
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

    fn send_to_all(&mut self, socket: &UdpSocket, data: &[u8], addrs: &[SocketAddr]) {
        use std::os::unix::io::AsRawFd;

        if addrs.is_empty() {
            return;
        }

        let fd = socket.as_raw_fd();
        let n = addrs.len();

        self.iov.iov_base = data.as_ptr() as *mut libc::c_void;
        self.iov.iov_len = data.len();

        if self.cached_addrs.as_slice() != addrs {
            self.rebuild(addrs, data.len());
        } else {
            // datalen depends on current packet size; refresh.
            for m in &mut self.msgs {
                m.msg_datalen = data.len();
            }
        }

        // SAFETY: msgs/sockaddrs/iov live in self for the whole call;
        // pointers stored in MsghdrX entries are stable until rebuild().
        let rc = unsafe { sendmsg_x(fd, self.msgs.as_ptr(), n as libc::c_uint, 0) };

        if rc < 0 {
            // ENOBUFS / EWOULDBLOCK / unsupported — fall back per-peer.
            for &addr in addrs {
                let _ = socket.try_send_to(data, addr);
            }
        }
        // Partial success (rc < n) is treated like sendmmsg: drop the rest,
        // matching real-time semantics — a stale frame isn't worth retrying.
    }

    fn rebuild(&mut self, addrs: &[SocketAddr], datalen: usize) {
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
                    self.addrlen[i] = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
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
                    self.addrlen[i] = std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t;
                }
            }
        }

        let iov_ptr = &mut self.iov as *mut libc::iovec;
        self.msgs.clear();
        for i in 0..n {
            self.msgs.push(MsghdrX {
                msg_name: &mut self.sockaddrs[i] as *mut libc::sockaddr_storage
                    as *mut libc::c_void,
                msg_namelen: self.addrlen[i],
                msg_iov: iov_ptr,
                msg_iovlen: 1,
                msg_control: std::ptr::null_mut(),
                msg_controllen: 0,
                msg_flags: 0,
                msg_datalen: datalen,
            });
        }

        self.cached_addrs.clear();
        self.cached_addrs.extend_from_slice(addrs);
    }
}

// ── Other platforms: loop with try_send_to ──────────────────────────
// Fine for small channels (< 20 peers). io_uring on Linux is the obvious
// next step if linux_throughput stops being enough.

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn send_to_all(socket: &UdpSocket, data: &[u8], addrs: &[SocketAddr]) {
    for &addr in addrs {
        let _ = socket.try_send_to(data, addr);
    }
}

// ── UDP relay loop ──────────────────────────────────────────────────

/// Resolve the number of UDP workers to spawn.
///
/// On Linux, multiple workers share the port via SO_REUSEPORT and the kernel
/// load-balances incoming datagrams across them, scaling near-linearly with cores.
/// On other platforms a single worker is used regardless of `requested`.
fn resolve_workers(requested: usize) -> usize {
    let resolved = if requested == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    } else {
        requested
    };

    #[cfg(target_os = "linux")]
    {
        resolved.max(1)
    }
    #[cfg(not(target_os = "linux"))]
    {
        if resolved > 1 {
            tracing::warn!(
                "udp-workers={} requested but multi-socket REUSEPORT is Linux-only; using 1",
                resolved
            );
        }
        1
    }
}

/// Start the UDP voice relay server.
///
/// Spawns `workers` parallel relay loops, each owning its own UDP socket.
/// On Linux the sockets share the port via SO_REUSEPORT; on other platforms
/// a single socket is bound and `workers` is forced to 1.
pub async fn run_udp_relay(
    state: ServerState,
    metrics: Arc<Metrics>,
    addr: String,
    workers: usize,
) -> Result<()> {
    let workers = resolve_workers(workers);
    let bind_addr: SocketAddr = addr.parse()?;

    let mut handles = Vec::with_capacity(workers);
    for worker_id in 0..workers {
        let socket = bind_worker_socket(bind_addr).await?;
        let state = state.clone();
        let metrics = metrics.clone();
        handles.push(tokio::spawn(async move {
            relay_loop(socket, state, metrics, worker_id).await
        }));
    }
    tracing::info!("UDP listening on {} ({} worker(s))", addr, workers);

    // If any worker exits (unexpected), propagate so the supervisor restarts.
    for h in handles {
        match h.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(join_err) => return Err(anyhow::anyhow!("UDP worker panicked: {}", join_err)),
        }
    }
    Ok(())
}

/// Bind one socket for a worker. SO_REUSEPORT on Linux, regular bind elsewhere.
async fn bind_worker_socket(addr: SocketAddr) -> Result<UdpSocket> {
    #[cfg(target_os = "linux")]
    {
        bind_reuseport(addr)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(UdpSocket::bind(addr).await?)
    }
}

/// Per-worker hot loop. Each worker has its own buffer, peer scratch and
/// (on Linux) BatchSender state — no shared mutable state between workers.
async fn relay_loop(
    socket: UdpSocket,
    state: ServerState,
    metrics: Arc<Metrics>,
    worker_id: usize,
) -> Result<()> {
    let mut buf = vec![0u8; config::MAX_UDP_PACKET + 64];
    let mut peers = Vec::<SocketAddr>::with_capacity(16);

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let mut batch = BatchSender::new();

    loop {
        let (len, src_addr) = match socket.recv_from(&mut buf).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(worker_id, "UDP recv error: {}", e);
                continue;
            }
        };

        metrics.udp_packets_in.fetch_add(1, Ordering::Relaxed);
        metrics
            .udp_bytes_in
            .fetch_add(len as u64, Ordering::Relaxed);

        let data = &buf[..len];

        // Check for hello packet (token-based UDP registration)
        if let Some(token) = decode_udp_hello(data) {
            // Token 0 is a keepalive from an idle client — refreshes NAT/firewall
            // mappings on the way here. Count it, never register or forward.
            if token == 0 {
                metrics.udp_keepalives.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            if state.register_udp_by_token(token, src_addr).await {
                metrics.udp_hellos_ok.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(%src_addr, worker_id, "UDP hello registered via token");
                // Send ACK (echo the hello back)
                let ack = encode_udp_hello(token);
                let _ = socket.send_to(&ack, src_addr).await;
            } else {
                metrics.udp_hellos_invalid.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(%src_addr, worker_id, "UDP hello with invalid token");
            }
            continue;
        }

        // Parse only the channel_id without copying opus_data
        let channel_id = match VoicePacket::parse_channel_id(data) {
            Some(id) => id,
            None => {
                metrics.udp_voice_malformed.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };

        // Fill reusable peer buffer (no allocation when capacity suffices)
        state.fill_channel_peers(channel_id, &src_addr, &mut peers);

        if !peers.is_empty() {
            metrics.udp_voice_relayed.fetch_add(1, Ordering::Relaxed);
            metrics
                .udp_fanout_total
                .fetch_add(peers.len() as u64, Ordering::Relaxed);
            metrics.udp_bytes_relayed.fetch_add(
                (data.len() as u64) * (peers.len() as u64),
                Ordering::Relaxed,
            );
        }

        // Relay raw bytes to all other participants in the channel
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        batch.send_to_all(&socket, data, &peers);

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        send_to_all(&socket, data, &peers);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Fan a datagram out to every receiver using the exact send path the relay
    /// uses on this OS — `sendmmsg` (Linux), `sendmsg_x` (macOS) or the
    /// sequential `try_send_to` fallback (Windows/other) — and assert each peer
    /// gets the bytes intact. This is the only test that actually executes the
    /// platform-specific batch syscall, so it must run on every OS in the CI
    /// matrix; on Linux/macOS the cross-build only type-checks the syscall, it
    /// never runs it.
    #[tokio::test]
    async fn batch_send_fans_out_to_all_peers() {
        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();

        let mut receivers = Vec::new();
        let mut addrs = Vec::new();
        for _ in 0..4 {
            let r = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            addrs.push(r.local_addr().unwrap());
            receivers.push(r);
        }

        let payload = b"voice-fanout-probe";

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let mut batch = BatchSender::new();
            batch.send_to_all(&sender, payload, &addrs);
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            send_to_all(&sender, payload, &addrs);
        }

        for (i, r) in receivers.iter().enumerate() {
            let mut buf = [0u8; 64];
            let n = tokio::time::timeout(Duration::from_secs(2), r.recv(&mut buf))
                .await
                .unwrap_or_else(|_| panic!("receiver {i} timed out"))
                .unwrap();
            assert_eq!(&buf[..n], payload, "receiver {i} got wrong bytes");
        }
    }

    /// A second fan-out to the same peer set reuses the BatchSender's cached
    /// sockaddr/iovec layout. Sending payloads of different lengths also guards
    /// that the per-call datalen is refreshed and not stuck at the first size.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn batch_send_reuses_layout_across_calls() {
        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();

        let mut receivers = Vec::new();
        let mut addrs = Vec::new();
        for _ in 0..3 {
            let r = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            addrs.push(r.local_addr().unwrap());
            receivers.push(r);
        }

        let mut batch = BatchSender::new();

        batch.send_to_all(&sender, b"first", &addrs); // 5 bytes
        for r in &receivers {
            let mut buf = [0u8; 32];
            tokio::time::timeout(Duration::from_secs(2), r.recv(&mut buf))
                .await
                .expect("first round timed out")
                .unwrap();
        }

        batch.send_to_all(&sender, b"second-longer", &addrs); // 13 bytes, same peers
        for (i, r) in receivers.iter().enumerate() {
            let mut buf = [0u8; 32];
            let n = tokio::time::timeout(Duration::from_secs(2), r.recv(&mut buf))
                .await
                .unwrap_or_else(|_| panic!("receiver {i} timed out on reuse"))
                .unwrap();
            assert_eq!(
                &buf[..n],
                b"second-longer",
                "receiver {i} got stale/truncated bytes"
            );
        }
    }
}
