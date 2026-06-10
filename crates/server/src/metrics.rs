//! Lock-free runtime metrics.
//!
//! Counters live in atomics so hot paths pay only a single relaxed `fetch_add`
//! per event — no locking, no allocation. The maintenance task reads
//! [`Metrics::snapshot`] periodically and logs deltas.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Shared metrics handle. Cheap to clone (Arc).
#[derive(Default)]
pub struct Metrics {
    // ── UDP voice relay ────────────────────────────────────────────────
    /// Total UDP datagrams received (any kind).
    pub udp_packets_in: AtomicU64,
    /// Total bytes received on UDP.
    pub udp_bytes_in: AtomicU64,
    /// Successful UDP hello registrations.
    pub udp_hellos_ok: AtomicU64,
    /// UDP hellos with invalid/expired tokens.
    pub udp_hellos_invalid: AtomicU64,
    /// Keepalive packets from idle clients (hello with token 0).
    pub udp_keepalives: AtomicU64,
    /// Voice packets parsed and forwarded (counted once per inbound packet, not per recipient).
    pub udp_voice_relayed: AtomicU64,
    /// Voice packets dropped due to malformed framing.
    pub udp_voice_malformed: AtomicU64,
    /// Total bytes forwarded across all recipients (relay-side egress proxy).
    pub udp_bytes_relayed: AtomicU64,
    /// Cumulative sum of recipient counts for voice packets (≈ fan-out work done).
    pub udp_fanout_total: AtomicU64,

    // ── TCP control plane ──────────────────────────────────────────────
    /// Frames written to per-client queues (single recipient sends).
    pub tcp_sends: AtomicU64,
    /// Frames written via channel broadcast (counted per recipient).
    pub tcp_broadcast_sends: AtomicU64,
    /// Frames dropped because a client's queue was full (slow or stuck client).
    pub tcp_drops_queue_full: AtomicU64,
    /// Client requests rejected by rate limiter.
    pub tcp_rate_limited: AtomicU64,
}

impl Metrics {
    /// Construct a new shared metrics handle.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Read all counters atomically (relaxed). Used for periodic logging.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            udp_packets_in: self.udp_packets_in.load(Ordering::Relaxed),
            udp_bytes_in: self.udp_bytes_in.load(Ordering::Relaxed),
            udp_hellos_ok: self.udp_hellos_ok.load(Ordering::Relaxed),
            udp_hellos_invalid: self.udp_hellos_invalid.load(Ordering::Relaxed),
            udp_keepalives: self.udp_keepalives.load(Ordering::Relaxed),
            udp_voice_relayed: self.udp_voice_relayed.load(Ordering::Relaxed),
            udp_voice_malformed: self.udp_voice_malformed.load(Ordering::Relaxed),
            udp_bytes_relayed: self.udp_bytes_relayed.load(Ordering::Relaxed),
            udp_fanout_total: self.udp_fanout_total.load(Ordering::Relaxed),
            tcp_sends: self.tcp_sends.load(Ordering::Relaxed),
            tcp_broadcast_sends: self.tcp_broadcast_sends.load(Ordering::Relaxed),
            tcp_drops_queue_full: self.tcp_drops_queue_full.load(Ordering::Relaxed),
            tcp_rate_limited: self.tcp_rate_limited.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Snapshot {
    pub udp_packets_in: u64,
    pub udp_bytes_in: u64,
    pub udp_hellos_ok: u64,
    pub udp_hellos_invalid: u64,
    pub udp_keepalives: u64,
    pub udp_voice_relayed: u64,
    pub udp_voice_malformed: u64,
    pub udp_bytes_relayed: u64,
    pub udp_fanout_total: u64,
    pub tcp_sends: u64,
    pub tcp_broadcast_sends: u64,
    pub tcp_drops_queue_full: u64,
    pub tcp_rate_limited: u64,
}

impl Snapshot {
    /// Compute deltas vs a previous snapshot (saturating; counters are monotonic).
    pub fn delta(&self, prev: &Snapshot) -> Snapshot {
        Snapshot {
            udp_packets_in: self.udp_packets_in.saturating_sub(prev.udp_packets_in),
            udp_bytes_in: self.udp_bytes_in.saturating_sub(prev.udp_bytes_in),
            udp_hellos_ok: self.udp_hellos_ok.saturating_sub(prev.udp_hellos_ok),
            udp_hellos_invalid: self
                .udp_hellos_invalid
                .saturating_sub(prev.udp_hellos_invalid),
            udp_keepalives: self.udp_keepalives.saturating_sub(prev.udp_keepalives),
            udp_voice_relayed: self
                .udp_voice_relayed
                .saturating_sub(prev.udp_voice_relayed),
            udp_voice_malformed: self
                .udp_voice_malformed
                .saturating_sub(prev.udp_voice_malformed),
            udp_bytes_relayed: self
                .udp_bytes_relayed
                .saturating_sub(prev.udp_bytes_relayed),
            udp_fanout_total: self.udp_fanout_total.saturating_sub(prev.udp_fanout_total),
            tcp_sends: self.tcp_sends.saturating_sub(prev.tcp_sends),
            tcp_broadcast_sends: self
                .tcp_broadcast_sends
                .saturating_sub(prev.tcp_broadcast_sends),
            tcp_drops_queue_full: self
                .tcp_drops_queue_full
                .saturating_sub(prev.tcp_drops_queue_full),
            tcp_rate_limited: self.tcp_rate_limited.saturating_sub(prev.tcp_rate_limited),
        }
    }
}
