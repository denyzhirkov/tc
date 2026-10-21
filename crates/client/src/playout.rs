//! Pull-based playout: the output device's clock decides when a frame is
//! produced, not packet arrival.
//!
//! The receive task feeds packets to [`PlayoutPump`] and pumps it on every
//! arrival and on a tick; the pump asks [`PlayoutEngine::render_frame`] for
//! exactly one mixed 20 ms frame at a time while the output is below its
//! low-water mark (two frames, or one device request plus a frame). Each
//! sender is held at an adaptive depth in front of that pull, so arrival
//! jitter up to the target is absorbed instead of reaching the output as a gap.
//!
//! Per sender the state is `Idle` (buffering, silent) or `Playing`:
//!
//! * **Idle → Playing** once the buffer holds the target depth plus the frames
//!   the output is about to pull immediately, or once the oldest packet has
//!   waited that long (a one-packet talkspurt must still play). A hole at the
//!   head is then skipped rather than concealed: it is a loss from before the
//!   pause (typically the previous talkspurt's last packet). Before anything
//!   has played, a packet up to `REANCHOR_MAX` seqs before the first arrival
//!   still becomes the start (a swapped first pair).
//! * **Playing**, per frame: a packet is decoded; a hole with later packets
//!   buffered is a loss (FEC from the next packet, else PLC); an empty buffer
//!   is an underrun, bridged by PLC ("expand") for up to `PLAYOUT_HOLD_FRAMES`
//!   before the stream goes idle. Expand never consumes a sequence number: the
//!   sender's counter only advances on packets it sent, so the next talkspurt
//!   starts exactly at the next expected seq.
//! * **Accelerate**: when the minimum depth over the last second stays above
//!   target + 1 (the VAD pre-roll burst, a fast sender clock), one quiet frame
//!   is dropped with a crossfade. Every packet is still decoded, so decoder
//!   state stays continuous. A slow sender clock needs no special case: its
//!   buffer drains to empty and expand inserts the missing frame.
//!
//! No sockets, devices or clock reads: time is a parameter and the output is a
//! ring, so the pump runs against simulated networks and a real
//! `PlaybackRenderer` on a simulated device clock in the tests below. Steady state
//! is allocation-free: slots, decode, mix and crossfade buffers are reused;
//! only a new sender allocates its stream once.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tc_shared::config;

use crate::audio::{self, AudioProducer, PlayoutSignals};
use crate::codec::OpusDecoder;
use crate::diagnostics::AudioDiagnostics;
use crate::peer_gain::PeerGains;
use crate::voice::NetworkStats;

/// Duration of one Opus frame.
pub(crate) const FRAME_DURATION: Duration =
    Duration::from_micros(config::FRAME_SIZE as u64 * 1_000_000 / config::SAMPLE_RATE as u64);
const FRAME_MS: f64 = config::FRAME_SIZE as f64 * 1000.0 / config::SAMPLE_RATE as f64;

/// First TOC config number of the CELT-only modes (0-11 SILK, 12-15 hybrid).
const CELT_ONLY_TOC_CONFIG: u8 = 16;

/// Packets at the start of a talkspurt that arrive as one burst: the sender
/// flushes its VAD pre-roll together with the frame that opened the gate.
const TALKSPURT_ONSET_PACKETS: u32 = config::VAD_PREROLL_FRAMES as u32 + 1;

/// How far before the provisional anchor (the first packet that happened to
/// arrive) a packet may still land and become the stream's start instead of
/// being dropped as late. Half the ring: the other half stays free for the
/// packets already buffered ahead of the anchor (a whole onset burst fits).
/// Anything older than that is not reordering at a stream's start.
const REANCHOR_MAX: u32 = (config::JITTER_BUF_MAX / 2) as u32;

/// Wrapping-aware sequence comparison: returns true if a is "before" b.
fn seq_before(a: u32, b: u32) -> bool {
    // Treat the difference as signed: if b - a is in [1, 2^31), a is before b.
    let diff = b.wrapping_sub(a);
    diff > 0 && diff < 0x8000_0000
}

/// Per-sender jitter buffer backed by a fixed-size ring, plus the arrival
/// jitter estimate that sizes the playout target.
/// Pre-allocated slots avoid per-packet heap allocation on the hot path.
struct JitterBuffer {
    /// Fixed-size ring of (seq, data) slots. seq=0 means slot is empty.
    /// Vecs are pre-allocated and reused — no heap alloc after warmup.
    slots: Vec<(u32, Vec<u8>)>,
    capacity: usize,
    next_seq: u32,
    started: bool,
    /// `next_seq` was set by whichever packet arrived first (stream start,
    /// restart, resync) and nothing has been consumed since, so an earlier
    /// packet arriving late may still move it back.
    provisional: bool,
    /// Smoothed jitter estimate (RFC 3550 EMA of |D|, in ms).
    jitter_ms: f64,
    /// Arrival time and seq of the last packet fed to the estimator.
    reference: Option<(Instant, u32)>,
    /// Packets seen since the talkspurt began; the onset burst is skipped.
    spurt_packets: u32,
    /// Consecutive packets that landed beyond the ring (forward gap).
    out_of_window: u32,
}

impl JitterBuffer {
    fn new() -> Self {
        let capacity = config::JITTER_BUF_MAX;
        let slots = (0..capacity)
            .map(|_| (0u32, Vec::with_capacity(256)))
            .collect();
        Self {
            slots,
            capacity,
            next_seq: 0,
            started: false,
            provisional: true,
            jitter_ms: 0.0,
            reference: None,
            spurt_packets: 0,
            out_of_window: 0,
        }
    }

    /// Push a packet into the buffer. Copies opus_data into a pre-allocated slot.
    /// Returns true when the packet was dropped as late (its slot already played).
    fn push(&mut self, seq: u32, opus_data: &[u8], now: Instant) -> bool {
        if !self.started {
            self.next_seq = seq;
            self.started = true;
        }

        // Drop packets that are too old (already played)
        if seq_before(seq, self.next_seq) {
            // Detect sender restart: a large backward jump means the remote
            // peer likely left and rejoined, resetting their sequence counter.
            // Without this, all new packets would be silently dropped until
            // the sequence catches up to the old next_seq.
            let gap = self.next_seq.wrapping_sub(seq);
            if gap > self.capacity as u32 * 2 {
                tracing::debug!(
                    "jitter buffer: stream restart detected (seq={}, next_seq={}, gap={}), resetting",
                    seq,
                    self.next_seq,
                    gap
                );
                self.reset_to(seq);
            } else if self.provisional && gap <= REANCHOR_MAX && self.fits_from(seq) {
                // The real start of the stream, overtaken by its successor:
                // the first pair of a talkspurt arriving swapped.
                self.next_seq = seq;
            } else {
                // Too late to play, but its lateness is exactly what the
                // estimate must see, or the target never grows to cover it.
                self.observe(seq, now);
                return true;
            }
        }

        // Packets too far ahead would alias in the ring. One stray packet is
        // dropped, but a sustained forward gap (loss burst / deaf window
        // longer than the ring, e.g. while UDP registration was re-healing)
        // means the stream has moved on and next_seq can never catch up —
        // without a resync every later packet is dropped forever while the
        // speaking indicator (updated before push) keeps animating.
        let ahead = seq.wrapping_sub(self.next_seq) as usize;
        if ahead >= self.capacity {
            self.out_of_window += 1;
            if self.out_of_window >= config::JITTER_RESYNC_AFTER {
                tracing::debug!(
                    "jitter buffer: forward gap resync (seq={}, next_seq={}, ahead={})",
                    seq,
                    self.next_seq,
                    ahead
                );
                self.reset_to(seq);
                // fall through and store this packet at the new position
            } else {
                return false;
            }
        } else {
            self.out_of_window = 0;
        }

        self.observe(seq, now);

        // Copy into pre-allocated slot (reuses existing Vec capacity)
        let idx = seq as usize % self.capacity;
        self.slots[idx].0 = seq;
        self.slots[idx].1.clear();
        self.slots[idx].1.extend_from_slice(opus_data);
        false
    }

    /// Feed one arrival to the jitter estimate. Uses the seq-aware transit
    /// difference D = (Rj − Ri) − (sj − si) × 20 ms, so neither a silence gap
    /// (seconds of arrival time, one seq step) nor reordering reads as jitter.
    /// The onset burst of a talkspurt is skipped: its packets were captured up
    /// to 100 ms apart but sent together. Its last packet becomes the
    /// reference, since it was sent at its own capture time.
    fn observe(&mut self, seq: u32, now: Instant) {
        self.spurt_packets = self.spurt_packets.saturating_add(1);
        if self.spurt_packets < TALKSPURT_ONSET_PACKETS {
            return;
        }
        if let Some((at, ref_seq)) = self.reference {
            let arrival_ms = now.saturating_duration_since(at).as_secs_f64() * 1000.0;
            let seq_ms = seq.wrapping_sub(ref_seq) as i32 as f64 * FRAME_MS;
            let d = arrival_ms - seq_ms;
            // Exponential moving average: alpha = 1/16 (same as RFC 3550)
            self.jitter_ms += (d.abs() - self.jitter_ms) / 16.0;
        }
        self.reference = Some((now, seq));
    }

    /// The talkspurt is over: the next arrival starts a new reference (after
    /// its own onset burst). The estimate itself is kept — it describes the
    /// network, not the talkspurt.
    fn end_talkspurt(&mut self) {
        self.reference = None;
        self.spurt_packets = 0;
    }

    /// Whether every buffered packet stays inside the ring when the window
    /// starts at `start` instead of `next_seq`.
    fn fits_from(&self, start: u32) -> bool {
        (0..self.capacity).all(|offset| {
            let seq = self.next_seq.wrapping_add(offset as u32);
            let buffered = self.slots[seq as usize % self.capacity].0 == seq && seq != 0;
            !buffered || (seq.wrapping_sub(start) as usize) < self.capacity
        })
    }

    /// Step `next_seq` forward to the oldest buffered packet and return how
    /// many sequence numbers were passed over (0 if `next_seq` is buffered).
    fn skip_to_oldest(&mut self) -> u32 {
        let skipped = (0..self.capacity as u32)
            .find(|&offset| self.peek(self.next_seq.wrapping_add(offset)).is_some())
            .unwrap_or(0);
        self.next_seq = self.next_seq.wrapping_add(skipped);
        if skipped > 0 {
            self.provisional = false;
        }
        skipped
    }

    /// The buffered packet with sequence `seq`, without consuming it.
    fn peek(&self, seq: u32) -> Option<&[u8]> {
        let slot = &self.slots[seq as usize % self.capacity];
        (slot.0 == seq && seq != 0).then_some(slot.1.as_slice())
    }

    /// Pop the next packet. Data is swapped into `out` (zero-alloc after warmup).
    /// `Empty` leaves `next_seq` where it is: an underrun is not a loss.
    fn pop(&mut self, out: &mut Vec<u8>) -> PopResult {
        if !self.started {
            return PopResult::Empty;
        }

        let idx = self.next_seq as usize % self.capacity;
        if self.slots[idx].0 == self.next_seq {
            // Swap data out: slot gets the (empty) out buffer, caller gets the data.
            // Both Vecs keep their capacity — no allocation.
            std::mem::swap(&mut self.slots[idx].1, out);
            self.slots[idx].0 = 0; // mark empty
            self.next_seq = self.next_seq.wrapping_add(1);
            self.provisional = false;
            return PopResult::Packet;
        }

        // Slot empty or stale — check if any future packets exist
        let has_future = (1..self.capacity).any(|offset| {
            let future_seq = self.next_seq.wrapping_add(offset as u32);
            let future_idx = future_seq as usize % self.capacity;
            let slot = &self.slots[future_idx];
            slot.0 == future_seq && slot.0 != 0
        });
        if has_future {
            // Clear stale data at current slot
            self.slots[idx].0 = 0;
            self.next_seq = self.next_seq.wrapping_add(1);
            self.provisional = false;
            PopResult::Missing
        } else {
            PopResult::Empty
        }
    }

    /// Count valid buffered packets in the current window.
    fn count_buffered(&self) -> usize {
        (0..self.capacity)
            .filter(|&offset| {
                let seq = self.next_seq.wrapping_add(offset as u32);
                let idx = seq as usize % self.capacity;
                self.slots[idx].0 == seq && seq != 0
            })
            .count()
    }

    /// Reset the buffer to accept a new stream starting at the given sequence.
    /// Called on a large backward jump (peer rejoined) or a sustained forward
    /// gap (loss burst longer than the ring).
    fn reset_to(&mut self, seq: u32) {
        self.next_seq = seq;
        self.provisional = true;
        for slot in &mut self.slots {
            slot.0 = 0; // mark empty (Vecs keep their capacity)
        }
        self.jitter_ms = 0.0;
        self.reference = None;
        self.spurt_packets = 0;
        self.out_of_window = 0;
    }

    /// Current jitter estimate in milliseconds.
    fn jitter_ms(&self) -> f64 {
        self.jitter_ms
    }

    fn next_seq(&self) -> u32 {
        self.next_seq
    }
}

enum PopResult {
    Packet,  // Data available in the swap buffer
    Missing, // Gap detected — use FEC / PLC
    Empty,   // Nothing available
}

/// Target buffered depth (frames) for a jitter estimate.
fn target_frames(jitter_ms: f64) -> usize {
    (((2.0 * jitter_ms + FRAME_MS) / FRAME_MS).ceil() as usize).clamp(
        config::PLAYOUT_TARGET_MIN_FRAMES,
        config::PLAYOUT_TARGET_MAX_FRAMES,
    )
}

/// Buffered depth before each pop over the last `PLAYOUT_ACCEL_WINDOW_FRAMES`
/// frames. Fixed array: no allocation.
struct DepthWindow {
    ring: [u8; config::PLAYOUT_ACCEL_WINDOW_FRAMES],
    next: usize,
    len: usize,
}

impl DepthWindow {
    fn new() -> Self {
        Self {
            ring: [0; config::PLAYOUT_ACCEL_WINDOW_FRAMES],
            next: 0,
            len: 0,
        }
    }

    fn push(&mut self, depth: usize) {
        self.ring[self.next] = depth.min(u8::MAX as usize) as u8;
        self.next = (self.next + 1) % self.ring.len();
        self.len = (self.len + 1).min(self.ring.len());
    }

    /// Minimum over the recorded history (only meaningful when `len > 0`).
    fn min(&self) -> usize {
        // Until the ring has wrapped, the recorded entries are `0..len`.
        self.ring[..self.len].iter().copied().min().unwrap_or(0) as usize
    }

    fn reset(&mut self) {
        self.next = 0;
        self.len = 0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamState {
    Idle,
    Playing,
}

/// What a playing stream put into one rendered frame. Tests replay these
/// against a reference decoder; production only needs "something".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
enum FrameOp {
    /// Packet `seq` decoded.
    Packet(u32),
    /// `dropped` decoded and discarded, `played` decoded and crossfaded in.
    Accelerated { dropped: u32, played: u32 },
    /// The lost frame recovered from the redundancy in packet `next`.
    Fec { next: u32 },
    /// A lost frame concealed.
    Plc,
    /// An underrun concealed; no sequence number consumed.
    Expand,
}

/// Per-sender receive state. Sequence counters of different senders are
/// independent (each starts at 1), so sharing one jitter buffer across
/// senders would make every interleaved packet look like a stream restart
/// and constantly reset the buffer — concurrent speakers require one
/// buffer + decoder per sender, mixed per frame.
struct SenderStream {
    jitter: JitterBuffer,
    decoder: OpusDecoder,
    last_packet: Instant,
    state: StreamState,
    /// Arrival of the first packet buffered while idle (activation timeout).
    first_idle_arrival: Option<Instant>,
    /// Consecutive expand frames in the current underrun.
    hold: u32,
    /// Expand frames not yet known to be a mid-talkspurt underrun.
    pending_expand: u64,
    depths: DepthWindow,
    #[cfg(test)]
    ops: Vec<(u64, FrameOp)>,
}

/// Scratch buffers a stream renders through, owned by the engine and shared
/// by all streams (they render one after another).
struct Scratch {
    opus: Vec<u8>,
    /// One stream's frame, before gain and mixing.
    frame: Vec<f32>,
    /// Head of a frame dropped by acceleration, for the crossfade.
    head: Vec<f32>,
}

impl SenderStream {
    fn new(decoder: OpusDecoder, now: Instant) -> Self {
        Self {
            jitter: JitterBuffer::new(),
            decoder,
            last_packet: now,
            state: StreamState::Idle,
            first_idle_arrival: None,
            hold: 0,
            pending_expand: 0,
            depths: DepthWindow::new(),
            #[cfg(test)]
            ops: Vec::new(),
        }
    }

    fn target(&self) -> usize {
        target_frames(self.jitter.jitter_ms())
    }

    fn go_idle(&mut self) {
        self.state = StreamState::Idle;
        self.hold = 0;
        // Concealment after a talkspurt's last packet is its tail, not a glitch.
        // Known blind spot: a mid-speech stall longer than the hold (network
        // outage, sender CPU stall) ends here exactly like a talkspurt does,
        // so it moves no glitch counter — the pending expand is discarded and
        // the playing count drops before the output drains, so the callback
        // sees an ordinary end of phrase rather than `output_starved`. The
        // receiver cannot tell the two apart from packets alone; it needs a
        // sender-side end-of-talkspurt marker (e.g. a flag on the last packet
        // before the VAD gate closes), which is a protocol change.
        self.pending_expand = 0;
        self.first_idle_arrival = None;
        self.depths.reset();
        self.jitter.end_talkspurt();
    }

    /// Produce this stream's next frame into `scratch.frame`, or None while
    /// idle. `pull` is how many frames the output is about to take back to
    /// back; an idle stream waits for that many on top of its target, so its
    /// buffer still holds the target once they are gone.
    fn render(
        &mut self,
        now: Instant,
        pull: usize,
        scratch: &mut Scratch,
        diag: &AudioDiagnostics,
        stats: &NetworkStats,
    ) -> Option<FrameOp> {
        let target = self.target();
        if self.state == StreamState::Idle {
            let buffered = self.jitter.count_buffered();
            if buffered == 0 {
                self.first_idle_arrival = None;
                return None;
            }
            let arrived = *self.first_idle_arrival.get_or_insert(now);
            let lead = target + pull;
            let waited = now.saturating_duration_since(arrived) >= FRAME_DURATION * lead as u32;
            if buffered < lead && !waited {
                return None;
            }
            self.state = StreamState::Playing;
            self.first_idle_arrival = None;
            self.hold = 0;
            self.depths.reset();
            // A hole at the start of a talkspurt is skipped, not concealed.
            // Typically it is the previous talkspurt's lost last packet, whose
            // FEC/PLC would replay a frame encoded seconds ago in front of the
            // new onset; it is still a loss for the stats. Trade-off: the pump
            // runs after every datagram, so an onset burst activates as soon
            // as target + pull packets are in. If the burst's first packet was
            // reordered behind that many of its successors, it is skipped here
            // too and arrives late (counted late and lost). That costs one
            // quiet pre-roll frame and leaves no gap in the output.
            for _ in 0..self.jitter.skip_to_oldest() {
                stats.record_lost();
                stats.maybe_finalize_window();
            }
        }

        self.depths.push(self.jitter.count_buffered());
        let seq = self.jitter.next_seq();
        match self.jitter.pop(&mut scratch.opus) {
            PopResult::Packet => {
                stats.record_received();
                stats.maybe_finalize_window();
                self.hold = 0;
                if self.pending_expand > 0 {
                    diag.expand_frames
                        .fetch_add(self.pending_expand, Ordering::Relaxed);
                    self.pending_expand = 0;
                }
                decode_into(&mut self.decoder, &scratch.opus, &mut scratch.frame);
                if self.should_accelerate(target, &scratch.frame) {
                    let played = self.jitter.next_seq();
                    if self.jitter.peek(played).is_some() {
                        let fade = scratch.head.len();
                        scratch.head.copy_from_slice(&scratch.frame[..fade]);
                        self.jitter.pop(&mut scratch.opus);
                        stats.record_received();
                        stats.maybe_finalize_window();
                        decode_into(&mut self.decoder, &scratch.opus, &mut scratch.frame);
                        // The dropped frame continued the previous output, so
                        // fading from its head keeps the output continuous.
                        for (i, (s, h)) in scratch.frame.iter_mut().zip(&scratch.head).enumerate() {
                            let t = (i + 1) as f32 / (fade + 1) as f32;
                            *s = *h * (1.0 - t) + *s * t;
                        }
                        diag.accelerate_drops.fetch_add(1, Ordering::Relaxed);
                        // The history describes the deeper buffer; start over.
                        self.depths.reset();
                        return Some(FrameOp::Accelerated {
                            dropped: seq,
                            played,
                        });
                    }
                }
                Some(FrameOp::Packet(seq))
            }
            PopResult::Missing => {
                stats.record_lost();
                stats.maybe_finalize_window();
                self.hold = 0;
                // `pop` already stepped past the lost seq, so the packet that
                // carries its LBRR redundancy is the one at `next_seq`. It is
                // only peeked: the next frame decodes it normally. That packet
                // may be absent (burst loss: Missing needs just *some* later
                // packet buffered), and a CELT-only packet (TOC config >= 16)
                // never carries LBRR; both go to PLC. When FEC is attempted
                // libopus still conceals internally if the packet happens to
                // hold no LBRR, so `fec_recovered` means "FEC decode
                // attempted with the next packet available".
                let next = self.jitter.next_seq();
                let fec = self
                    .jitter
                    .peek(next)
                    .filter(|p| p.first().is_some_and(|toc| toc >> 3 < CELT_ONLY_TOC_CONFIG))
                    .map(|p| self.decoder.decode_fec(p).map(|pcm| pcm.len()));
                match fec {
                    Some(Ok(len)) => {
                        diag.fec_recovered.fetch_add(1, Ordering::Relaxed);
                        copy_frame(self.decoder.frame(len), &mut scratch.frame);
                        Some(FrameOp::Fec { next })
                    }
                    _ => {
                        diag.plc_frames.fetch_add(1, Ordering::Relaxed);
                        conceal_into(&mut self.decoder, &mut scratch.frame);
                        Some(FrameOp::Plc)
                    }
                }
            }
            PopResult::Empty => {
                self.hold += 1;
                if self.hold > config::PLAYOUT_HOLD_FRAMES {
                    self.go_idle();
                    return None;
                }
                self.pending_expand += 1;
                conceal_into(&mut self.decoder, &mut scratch.frame);
                Some(FrameOp::Expand)
            }
        }
    }

    /// Whether the frame just decoded may be dropped: the buffer stayed above
    /// target + 1 over the whole window and the frame is quiet — or the excess
    /// is large enough that waiting for a quiet frame risks the ring's edge.
    fn should_accelerate(&self, target: usize, frame: &[f32]) -> bool {
        if self.depths.len < config::PLAYOUT_ACCEL_MIN_OBSERVED {
            return false;
        }
        let min = self.depths.min();
        if min <= target + 1 {
            return false;
        }
        // Capped so even the largest target forces a drop before the ring
        // fills and out-of-window packets trigger a resync.
        let force =
            (target + 1 + config::PLAYOUT_ACCEL_FORCE_EXCESS).min(config::JITTER_BUF_MAX - 2);
        min > force || rms(frame) < config::PLAYOUT_ACCEL_MAX_RMS
    }
}

fn rms(frame: &[f32]) -> f32 {
    (frame.iter().map(|s| s * s).sum::<f32>() / frame.len().max(1) as f32).sqrt()
}

/// Copy a decoded frame into the fixed-size frame buffer, zero-padding a short one.
fn copy_frame(pcm: &[f32], frame: &mut [f32]) {
    let n = pcm.len().min(frame.len());
    frame[..n].copy_from_slice(&pcm[..n]);
    frame[n..].fill(0.0);
}

fn decode_into(decoder: &mut OpusDecoder, opus: &[u8], frame: &mut [f32]) {
    match decoder.decode(opus) {
        Ok(pcm) => copy_frame(pcm, frame),
        Err(e) => {
            tracing::trace!("decode error: {}", e);
            conceal_into(decoder, frame);
        }
    }
}

fn conceal_into(decoder: &mut OpusDecoder, frame: &mut [f32]) {
    match decoder.decode_plc() {
        Ok(pcm) => copy_frame(pcm, frame),
        Err(e) => {
            tracing::trace!("PLC error: {}", e);
            frame.fill(0.0);
        }
    }
}

/// All senders' playout state and the mixer over them.
pub(crate) struct PlayoutEngine {
    streams: HashMap<String, SenderStream>,
    scratch: Scratch,
    packets_since_prune: u32,
    #[cfg(test)]
    rendered: u64,
}

impl PlayoutEngine {
    pub(crate) fn new() -> Self {
        let fade = (config::SAMPLE_RATE * config::PLAYBACK_FADE_MS / 1000) as usize;
        Self {
            streams: HashMap::new(),
            scratch: Scratch {
                opus: Vec::with_capacity(256),
                frame: vec![0.0; config::FRAME_SIZE],
                head: vec![0.0; fade.clamp(1, config::FRAME_SIZE)],
            },
            packets_since_prune: 0,
            #[cfg(test)]
            rendered: 0,
        }
    }

    /// Buffer one packet from `name`. Returns true when it arrived too late to
    /// play (its slot was already rendered).
    pub(crate) fn on_packet(&mut self, name: &str, seq: u32, opus: &[u8], now: Instant) -> bool {
        self.packets_since_prune += 1;
        if self.packets_since_prune >= config::STATS_WINDOW_PACKETS {
            self.packets_since_prune = 0;
            self.prune(now);
        }
        if !self.streams.contains_key(name) {
            if self.streams.len() >= config::MAX_SENDER_STREAMS {
                self.prune(now);
            }
            if self.streams.len() >= config::MAX_SENDER_STREAMS {
                tracing::warn!("sender stream limit reached, dropping packet");
                return false;
            }
            let decoder = match OpusDecoder::new() {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!("failed to create decoder: {}", e);
                    return false;
                }
            };
            self.streams
                .insert(name.to_string(), SenderStream::new(decoder, now));
        }
        let stream = self.streams.get_mut(name).expect("stream just ensured");
        stream.last_packet = now;
        let late = stream.jitter.push(seq, opus, now);
        if !late && stream.state == StreamState::Idle {
            stream.first_idle_arrival.get_or_insert(now);
        }
        late
    }

    fn prune(&mut self, now: Instant) {
        let ttl = Duration::from_secs(config::SENDER_STREAM_TTL_SECS);
        self.streams
            .retain(|_, s| now.saturating_duration_since(s.last_packet) < ttl);
    }

    /// Render exactly one 20 ms frame into `out`: every playing sender's next
    /// frame, scaled by its per-peer gain and summed. The sum is unclamped —
    /// the output-stage limiter in `audio::PlaybackRenderer` bounds it.
    /// `pull` is how many frames the output takes back to back right now
    /// ([`Output::pull`]). Returns the frame length, or None when no sender
    /// is playing.
    pub(crate) fn render_frame(
        &mut self,
        now: Instant,
        pull: usize,
        out: &mut [f32],
        gains: &PeerGains,
        diag: &AudioDiagnostics,
        stats: &NetworkStats,
    ) -> Option<usize> {
        let n = config::FRAME_SIZE.min(out.len());
        #[cfg(test)]
        let index = self.rendered;
        let mut contributors = 0usize;
        for (name, stream) in self.streams.iter_mut() {
            let Some(_op) = stream.render(now, pull, &mut self.scratch, diag, stats) else {
                continue;
            };
            #[cfg(test)]
            stream.ops.push((index, _op));
            // Listener-side per-peer volume (1.0 = unchanged). Read live; one
            // map lookup per sender per frame, multiply skipped when unity.
            let gain = gains.get(name);
            let unity = (gain - 1.0).abs() < f32::EPSILON;
            let frame = &self.scratch.frame[..n];
            let out = &mut out[..n];
            match (contributors == 0, unity) {
                (true, true) => out.copy_from_slice(frame),
                (true, false) => {
                    for (m, s) in out.iter_mut().zip(frame) {
                        *m = *s * gain;
                    }
                }
                (false, true) => {
                    for (m, s) in out.iter_mut().zip(frame) {
                        *m += *s;
                    }
                }
                (false, false) => {
                    for (m, s) in out.iter_mut().zip(frame) {
                        *m += *s * gain;
                    }
                }
            }
            contributors += 1;
        }
        if contributors == 0 {
            return None;
        }
        #[cfg(test)]
        {
            self.rendered += 1;
        }
        Some(n)
    }

    /// Senders currently playing (buffering ones excluded).
    pub(crate) fn playing(&self) -> usize {
        self.streams
            .values()
            .filter(|s| s.state == StreamState::Playing)
            .count()
    }

    /// Whether anything is playing or waiting to: only then does the engine
    /// need to be pulled on a timer rather than just on packet arrival.
    fn active(&self) -> bool {
        self.streams
            .values()
            .any(|s| s.state == StreamState::Playing || s.jitter.count_buffered() > 0)
    }
}

/// The output as the playout loop sees it: how much it holds (48 kHz
/// samples) and how full it has to be kept. The single definition of the
/// low-water logic, for the receive task and the simulations alike.
#[derive(Debug, Clone, Copy)]
struct Output {
    level: usize,
    low_water: usize,
}

impl Output {
    /// `device_request` is the output callback's last request (48 kHz mono
    /// samples). A device that takes more than a frame per callback must find
    /// a whole request plus a frame of slack after every top-up, or it runs
    /// dry whenever a callback lands just before the next wakeup.
    fn new(level: usize, device_request: usize) -> Self {
        Self {
            level,
            low_water: config::PLAYOUT_OUTPUT_LOW_WATER.max(device_request + config::FRAME_SIZE),
        }
    }

    fn wants_frame(&self) -> bool {
        self.level < self.low_water
    }

    /// Frames the output takes back to back before it is full again.
    fn pull(&self) -> usize {
        self.low_water
            .saturating_sub(self.level)
            .div_ceil(config::FRAME_SIZE)
    }
}

/// The receive task's half of the playout loop: the engine plus the output it
/// feeds. With a playback device the output level is the ring's occupancy plus
/// what the callback reports queued. Without one, a virtual output consumes
/// in real time, so jitter and decoder state stay current and audio resumes
/// seamlessly when a device appears; the rendered frames are discarded.
pub(crate) struct PlayoutPump {
    engine: PlayoutEngine,
    mix: Vec<f32>,
    /// Without a device: how far ahead of `now` the virtual output is covered.
    virtual_until: Instant,
    /// Every frame pushed to the output, when a test asks for it.
    #[cfg(test)]
    recorded: Option<Vec<Vec<f32>>>,
}

impl PlayoutPump {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            engine: PlayoutEngine::new(),
            mix: vec![0.0f32; config::FRAME_SIZE],
            virtual_until: now,
            #[cfg(test)]
            recorded: None,
        }
    }

    /// Buffer one packet; true when it arrived too late to play.
    pub(crate) fn on_packet(&mut self, name: &str, seq: u32, opus: &[u8], now: Instant) -> bool {
        self.engine.on_packet(name, seq, opus, now)
    }

    /// Whether the receive task must keep its playout tick armed. While
    /// nothing is buffered or playing, packet arrival is the only event that
    /// can change that, so an idle channel costs no timer wakeups.
    pub(crate) fn needs_tick(&self) -> bool {
        self.engine.active()
    }

    /// Render frames until the output holds its low-water mark or no sender
    /// is playing, then publish the playing count for the callback.
    pub(crate) fn pump(
        &mut self,
        prod: Option<&mut AudioProducer>,
        now: Instant,
        signals: &PlayoutSignals,
        gains: &PeerGains,
        diag: &AudioDiagnostics,
        stats: &NetworkStats,
    ) {
        match prod {
            Some(prod) => {
                use ringbuf::traits::Observer;
                self.virtual_until = now;
                let request = signals.request.load(Ordering::Relaxed) as usize;
                loop {
                    let queued = signals.queued.load(Ordering::Relaxed) as usize;
                    let output = Output::new(prod.occupied_len() + queued, request);
                    if !output.wants_frame() {
                        break;
                    }
                    let Some(n) = self.render(now, output.pull(), gains, diag, stats) else {
                        break;
                    };
                    let room = prod.vacant_len();
                    audio::push_counted(prod, &self.mix[..n], &diag.playback_ring_overflow);
                    if room < n {
                        // Full ring: the device has stopped pulling. Waiting
                        // is better than spinning on frames it cannot take.
                        break;
                    }
                }
            }
            None => {
                // The virtual device takes one frame per 20 ms of wall clock;
                // frames it would have taken while this task was not running
                // are rendered now. A lag beyond the jitter ring is not worth
                // replaying (the buffers have resynced by then anyway).
                let max_lag = FRAME_DURATION * config::JITTER_BUF_MAX as u32;
                if now.saturating_duration_since(self.virtual_until) > max_lag {
                    self.virtual_until = now;
                }
                loop {
                    let ahead = self.virtual_until.saturating_duration_since(now);
                    let level =
                        (ahead.as_micros() * config::SAMPLE_RATE as u128 / 1_000_000) as usize;
                    let output = Output::new(level, 0);
                    if !output.wants_frame() {
                        break;
                    }
                    if self
                        .render(now, output.pull(), gains, diag, stats)
                        .is_none()
                    {
                        // Nothing playing: the virtual output has run dry.
                        self.virtual_until = now;
                        break;
                    }
                    self.virtual_until += FRAME_DURATION;
                }
            }
        }
        signals
            .playing
            .store(self.engine.playing() as u32, Ordering::Relaxed);
    }

    fn render(
        &mut self,
        now: Instant,
        pull: usize,
        gains: &PeerGains,
        diag: &AudioDiagnostics,
        stats: &NetworkStats,
    ) -> Option<usize> {
        let n = self
            .engine
            .render_frame(now, pull, &mut self.mix, gains, diag, stats)?;
        #[cfg(test)]
        if let Some(rec) = self.recorded.as_mut() {
            rec.push(self.mix[..n].to_vec());
        }
        Some(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::OpusEncoder;
    use crate::diagnostics::DiagnosticsSnapshot;

    const PERIOD_US: f64 = 20_000.0;

    // ── sender model ─────────────────────────────────────────────────

    fn fec_encoder() -> OpusEncoder {
        let mut enc = OpusEncoder::new().unwrap();
        enc.apply_quality_settings(
            config::BITRATE_HIGH,
            config::COMPLEXITY_HIGH,
            true,
            config::FEC_MIN_LOSS_PERC,
        )
        .unwrap();
        enc
    }

    /// Deterministic speech stand-in for capture frame `frame`: harmonics of
    /// 150 Hz under a 4 Hz syllable envelope that dips to near silence between
    /// syllables, with a 0.4 s pause every 2 s.
    fn speech(frame: u64, out: &mut [f32]) {
        let two_pi = 2.0 * std::f64::consts::PI;
        for (i, s) in out.iter_mut().enumerate() {
            let t =
                (frame * config::FRAME_SIZE as u64 + i as u64) as f64 / config::SAMPLE_RATE as f64;
            let syllable = 0.5 - 0.5 * (two_pi * 4.0 * t).cos();
            let env = if t % 2.0 >= 1.6 {
                0.0005
            } else {
                0.15 * syllable * syllable
            };
            let voiced: f64 = (1..=10)
                .map(|h| (two_pi * 150.0 * h as f64 * t).sin() / h as f64)
                .sum();
            *s = (env * voiced) as f32;
        }
    }

    /// Content that is never quiet enough for a quiet-frame drop.
    fn loud_tone(frame: u64, out: &mut [f32]) {
        for (i, s) in out.iter_mut().enumerate() {
            let t =
                (frame * config::FRAME_SIZE as u64 + i as u64) as f64 / config::SAMPLE_RATE as f64;
            *s = (0.3 * (2.0 * std::f64::consts::PI * 220.0 * t).sin()) as f32;
        }
    }

    struct Sent {
        seq: u32,
        send_us: u64,
        data: Vec<u8>,
    }

    /// Encode talkspurts `[start, end)` (capture frames) into seq-numbered
    /// packets sent at `period_us` per frame — a clock 0.5% fast is 19 900 µs.
    /// With `preroll`, a talkspurt opens the way the VAD sender does: the
    /// `VAD_PREROLL_FRAMES` frames before `start` go out together with
    /// `start` itself. Seq only advances on sent packets, across talkspurts.
    fn send(
        spurts: &[(u64, u64)],
        preroll: bool,
        period_us: f64,
        signal: fn(u64, &mut [f32]),
    ) -> Vec<Sent> {
        let mut enc = fec_encoder();
        let mut pcm = vec![0.0f32; config::FRAME_SIZE];
        let mut seq = 1u32;
        let mut sent = Vec::new();
        for &(start, end) in spurts {
            let first = if preroll {
                start.saturating_sub(config::VAD_PREROLL_FRAMES as u64)
            } else {
                start
            };
            for frame in first..end {
                signal(frame, &mut pcm);
                sent.push(Sent {
                    seq,
                    send_us: (frame.max(start) as f64 * period_us) as u64,
                    data: enc.encode(&pcm).unwrap().to_vec(),
                });
                seq += 1;
            }
        }
        sent
    }

    // ── network model ────────────────────────────────────────────────

    struct Arrival {
        at_us: u64,
        name: &'static str,
        seq: u32,
        data: Vec<u8>,
    }

    /// Apply a per-packet delay (µs) schedule; `None` loses the packet.
    /// Reordering falls out of the delays.
    fn network(
        name: &'static str,
        sent: &[Sent],
        mut delay_us: impl FnMut(&Sent) -> Option<u64>,
    ) -> Vec<Arrival> {
        let mut out: Vec<Arrival> = sent
            .iter()
            .filter_map(|p| {
                delay_us(p).map(|d| Arrival {
                    at_us: p.send_us + d,
                    name,
                    seq: p.seq,
                    data: p.data.clone(),
                })
            })
            .collect();
        out.sort_by_key(|a| a.at_us);
        out
    }

    /// Deterministic uniform jitter source.
    struct Lcg(u64);

    impl Lcg {
        fn below(&mut self, max: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 33) % max
        }
    }

    // ── output clock ─────────────────────────────────────────────────

    /// An output device: how many 48 kHz mono samples each callback takes.
    /// The period follows from the size.
    #[derive(Clone, Copy)]
    struct Device {
        request: usize,
    }

    impl Device {
        /// macOS: fixed 960-sample (20 ms) callbacks.
        const FRAME: Device = Device {
            request: config::FRAME_SIZE,
        };

        fn period_us(self) -> f64 {
            self.request as f64 * 1e6 / config::SAMPLE_RATE as f64
        }
    }

    /// Windows' default timer resolution: a 5 ms interval fires every 15.6 ms.
    const WINDOWS_TIMER_US: f64 = 15_625.0;

    /// The real receive-side machinery on a simulated clock: packets go to a
    /// real `PlayoutPump` at their arrival times, the pump fills a real ring,
    /// and a real `PlaybackRenderer` drains it on the device's callback
    /// schedule — so `output_starved` is counted exactly as in production.
    /// The pump also runs on every playout tick while it asks for one.
    struct Sim {
        pump: PlayoutPump,
        prod: AudioProducer,
        cons: crate::audio::AudioConsumer,
        renderer: crate::audio::PlaybackRenderer,
        signals: PlayoutSignals,
        diag: AudioDiagnostics,
        stats: NetworkStats,
        gains: PeerGains,
        base: Instant,
        device: Device,
        tick_us: f64,
        device_buf: Vec<f32>,
        warmup_ms: u64,
        at_warmup: DiagnosticsSnapshot,
        max_depth: HashMap<&'static str, usize>,
    }

    impl Sim {
        fn new(warmup_ms: u64) -> Self {
            Self::with(
                warmup_ms,
                Device::FRAME,
                config::PLAYOUT_TICK_MS as f64 * 1000.0,
            )
        }

        fn with(warmup_ms: u64, device: Device, tick_us: f64) -> Self {
            use ringbuf::traits::Split;
            // Same ring size as `audio::start_playback`.
            let (prod, cons) = ringbuf::HeapRb::<f32>::new(config::FRAME_SIZE * 8).split();
            let base = Instant::now();
            Self {
                pump: PlayoutPump::new(base),
                prod,
                cons,
                renderer: crate::audio::PlaybackRenderer::new(config::SAMPLE_RATE, 1),
                signals: PlayoutSignals::default(),
                diag: AudioDiagnostics::default(),
                stats: NetworkStats::new(),
                gains: PeerGains::new(),
                base,
                device,
                tick_us,
                device_buf: vec![0.0; device.request],
                warmup_ms,
                at_warmup: DiagnosticsSnapshot::default(),
                max_depth: HashMap::new(),
            }
        }

        fn at(&self, us: f64) -> Instant {
            self.base + Duration::from_nanos((us * 1000.0) as u64)
        }

        fn pump(&mut self, now: Instant) {
            self.pump.pump(
                Some(&mut self.prod),
                now,
                &self.signals,
                &self.gains,
                &self.diag,
                &self.stats,
            );
        }

        fn run(&mut self, arrivals: &[Arrival], end_ms: u64) {
            self.run_probed(arrivals, end_ms, |_, _| {});
        }

        fn run_probed(
            &mut self,
            arrivals: &[Arrival],
            end_ms: u64,
            mut probe: impl FnMut(&Sim, u64),
        ) {
            // A callback phase unrelated to the 20 ms send grid.
            let mut next_callback = 7_000.0;
            let mut next_tick = 0.0;
            let mut next_arrival = 0;
            let mut next_probe_ms = 0;
            let mut warmed = false;
            let end_us = end_ms as f64 * 1000.0;
            loop {
                let arrival = arrivals
                    .get(next_arrival)
                    .map_or(f64::INFINITY, |a| a.at_us as f64);
                let t = arrival.min(next_callback).min(next_tick);
                if t > end_us {
                    break;
                }
                let now = self.at(t);
                if t == arrival {
                    let a = &arrivals[next_arrival];
                    if self.pump.on_packet(a.name, a.seq, &a.data, now) {
                        self.diag.late_packets.fetch_add(1, Ordering::Relaxed);
                    }
                    self.pump(now);
                    next_arrival += 1;
                } else if t == next_callback {
                    self.renderer.render(
                        &mut self.cons,
                        1.0,
                        &mut self.device_buf,
                        &self.diag,
                        &self.signals,
                    );
                    next_callback += self.device.period_us();
                } else {
                    if self.pump.needs_tick() {
                        self.pump(now);
                    }
                    next_tick += self.tick_us;
                }

                let t_ms = (t / 1000.0) as u64;
                if !warmed && t_ms >= self.warmup_ms {
                    warmed = true;
                    self.at_warmup = self.diag.snapshot();
                }
                if warmed {
                    for (name, s) in &self.pump.engine.streams {
                        let d = self.max_depth.entry(leak(name)).or_default();
                        *d = (*d).max(s.jitter.count_buffered());
                    }
                }
                while next_probe_ms <= t_ms {
                    probe(self, next_probe_ms);
                    next_probe_ms += 10;
                }
            }
        }

        /// Counters accumulated since the end of warm-up.
        fn since_warmup(&self) -> DiagnosticsSnapshot {
            self.diag.snapshot().delta(&self.at_warmup)
        }

        /// Output callbacks that ran dry while a sender was playing — the
        /// audible gap this whole engine exists to prevent.
        fn starved(&self) -> u64 {
            self.since_warmup().output_starved
        }

        fn engine(&self) -> &PlayoutEngine {
            &self.pump.engine
        }

        fn stream(&self, name: &str) -> &SenderStream {
            &self.pump.engine.streams[name]
        }

        fn ops(&self, name: &str) -> &[(u64, FrameOp)] {
            &self.stream(name).ops
        }

        fn count_ops(&self, name: &str, pred: impl Fn(&FrameOp) -> bool) -> usize {
            self.ops(name).iter().filter(|(_, op)| pred(op)).count()
        }

        fn recorded(&self) -> &[Vec<f32>] {
            self.pump.recorded.as_deref().unwrap_or_default()
        }
    }

    /// Test sender names are static strings; map a stream key back to one.
    fn leak(name: &str) -> &'static str {
        ["alice", "bob"]
            .into_iter()
            .find(|n| *n == name)
            .expect("test sender name")
    }

    /// `NetworkStats` restarts its window every `STATS_WINDOW_PACKETS`, so
    /// received/lost totals are counted from the op log instead.
    fn decoded_in(ops: &[(u64, FrameOp)]) -> usize {
        ops.iter()
            .map(|(_, op)| match op {
                FrameOp::Packet(_) => 1,
                FrameOp::Accelerated { .. } => 2,
                _ => 0,
            })
            .sum()
    }

    fn lost_in(ops: &[(u64, FrameOp)]) -> usize {
        ops.iter()
            .filter(|(_, op)| matches!(op, FrameOp::Fec { .. } | FrameOp::Plc))
            .count()
    }

    fn decoded_packets(sim: &Sim, name: &str) -> usize {
        decoded_in(sim.ops(name))
    }

    fn lost(sim: &Sim) -> usize {
        sim.engine().streams.values().map(|s| lost_in(&s.ops)).sum()
    }

    fn is_expand(op: &FrameOp) -> bool {
        matches!(op, FrameOp::Expand)
    }

    fn is_accel(op: &FrameOp) -> bool {
        matches!(op, FrameOp::Accelerated { .. })
    }

    // ── jitter targets & concealment under a simulated network ──────

    /// The headline property: arrival jitter within what the adaptive target
    /// covers never reaches the output as a gap or a concealment.
    #[test]
    fn jitter_up_to_30ms_plays_without_gaps() {
        let sent = send(&[(0, 1000)], false, PERIOD_US, speech);
        let mut rng = Lcg(7);
        let arrivals = network("alice", &sent, |_| Some(rng.below(30_000)));
        let mut sim = Sim::new(2_000);
        sim.run(&arrivals, 21_000);

        let d = sim.since_warmup();
        assert_eq!(sim.starved(), 0);
        assert_eq!(
            (d.expand_frames, d.plc_frames, d.late_packets),
            (0, 0, 0),
            "{d:?}"
        );
        assert_eq!(decoded_packets(&sim, "alice"), sent.len());
        let target = sim.stream("alice").target();
        assert!((2..=4).contains(&target), "target {target}");
        let jitter = sim.stream("alice").jitter.jitter_ms();
        assert!((5.0..15.0).contains(&jitter), "estimate {jitter}");
    }

    #[test]
    fn single_loss_is_recovered_by_fec_without_a_gap() {
        let sent = send(&[(0, 250)], false, PERIOD_US, speech);
        let arrivals = network("alice", &sent, |p| (p.seq != 100).then_some(0));
        let mut sim = Sim::new(0);
        sim.run(&arrivals, 6_000);

        let d = sim.diag.snapshot();
        assert_eq!((d.fec_recovered, d.plc_frames, d.expand_frames), (1, 0, 0));
        assert_eq!(sim.starved(), 0);
        assert_eq!(lost(&sim), 1);
        assert_eq!(decoded_packets(&sim, "alice"), sent.len() - 1);
    }

    /// Two in a row: the first hole is found before any later packet is in,
    /// so it is bridged as an underrun (expand) until packet 102 shows the
    /// loss; then 100 is concealed and 101 recovered from 102's redundancy.
    /// Nothing reaches the output as a gap and the stream continues.
    #[test]
    fn burst_loss_of_two_is_concealed_and_the_stream_continues() {
        let sent = send(&[(0, 250)], false, PERIOD_US, speech);
        let arrivals = network("alice", &sent, |p| {
            (!(100..=101).contains(&p.seq)).then_some(0)
        });
        let mut sim = Sim::new(0);
        sim.run(&arrivals, 6_000);

        let d = sim.diag.snapshot();
        assert_eq!((d.plc_frames, d.fec_recovered), (1, 1), "{d:?}");
        assert!(d.expand_frames <= 1, "{d:?}");
        assert_eq!(sim.starved(), 0);
        assert_eq!(lost(&sim), 2);
        assert_eq!(decoded_packets(&sim, "alice"), sent.len() - 2);
        assert_eq!(sim.count_ops("alice", |op| *op == FrameOp::Packet(250)), 1);
    }

    /// The seq-continuity trap: the hold frames at a talkspurt's end must not
    /// consume sequence numbers, or the next talkspurt (which continues at
    /// last + 1) is dropped as late. Its first two packets also arrive
    /// swapped, so reordering at a talkspurt start is covered.
    #[test]
    fn talkspurt_end_goes_idle_and_the_next_talkspurt_plays_fully() {
        let sent = send(&[(10, 60), (150, 200)], true, PERIOD_US, speech);
        let second_first = sent.iter().find(|p| p.send_us >= 150 * 20_000).unwrap().seq;
        let arrivals = network("alice", &sent, |p| {
            Some(match p.seq.cmp(&(second_first + 1)) {
                std::cmp::Ordering::Less if p.seq == second_first => 500,
                std::cmp::Ordering::Greater => 1_000,
                _ => 0,
            })
        });
        let mut sim = Sim::new(0);
        let mut idle_between = false;
        sim.run_probed(&arrivals, 5_000, |sim, t| {
            if (1_600..2_800).contains(&t) {
                idle_between |= sim.engine().playing() == 0;
            }
        });

        assert!(idle_between, "the stream must go idle between talkspurts");
        let d = sim.diag.snapshot();
        assert_eq!(d.expand_frames, 0, "a talkspurt tail is not a glitch");
        assert_eq!((d.late_packets, d.plc_frames, d.fec_recovered), (0, 0, 0));
        assert_eq!(sim.starved(), 0);
        assert_eq!(decoded_packets(&sim, "alice"), sent.len());
        assert_eq!(lost(&sim), 0);
        // Exactly the hold length of concealment after each talkspurt.
        assert_eq!(
            sim.count_ops("alice", is_expand),
            2 * config::PLAYOUT_HOLD_FRAMES as usize
        );
        let first_of_second = sim
            .ops("alice")
            .iter()
            .find(|(_, op)| !matches!(op, FrameOp::Expand) && op_seq(op) >= second_first)
            .map(|(_, op)| *op);
        assert_eq!(first_of_second, Some(FrameOp::Packet(second_first)));
        assert_eq!(sim.engine().playing(), 0);
    }

    fn op_seq(op: &FrameOp) -> u32 {
        match *op {
            FrameOp::Packet(s) => s,
            FrameOp::Accelerated { dropped, .. } => dropped,
            FrameOp::Fec { next } => next,
            FrameOp::Plc | FrameOp::Expand => 0,
        }
    }

    /// The pre-roll burst (6 packets at once) puts excess latency into every
    /// talkspurt. The onset must still be played in full; the excess goes at
    /// quiet frames later on.
    #[test]
    fn preroll_burst_onset_plays_and_depth_converges_by_accelerating() {
        let sent = send(&[(10, 260)], true, PERIOD_US, speech);
        let arrivals = network("alice", &sent, |_| Some(0));
        let mut sim = Sim::new(0);
        let mut early_max = 0;
        let mut late_max = 0;
        sim.run_probed(&arrivals, 5_600, |sim, t| {
            let Some(s) = sim.engine().streams.get("alice") else {
                return;
            };
            let depth = s.jitter.count_buffered();
            if (300..600).contains(&t) {
                early_max = early_max.max(depth);
            }
            if (3_500..5_000).contains(&t) {
                late_max = late_max.max(depth);
            }
        });

        let onset: Vec<FrameOp> = sim.ops("alice")[..TALKSPURT_ONSET_PACKETS as usize]
            .iter()
            .map(|(_, op)| *op)
            .collect();
        let expect: Vec<FrameOp> = (1..=TALKSPURT_ONSET_PACKETS).map(FrameOp::Packet).collect();
        assert_eq!(onset, expect, "the onset must play, not be skipped");
        let target = sim.stream("alice").target();
        assert!(early_max > target + 1, "burst made no excess: {early_max}");
        assert!(
            late_max <= target + 1,
            "depth {late_max} over target {target} + 1"
        );
        let accel = sim.diag.snapshot().accelerate_drops;
        assert!((1..=4).contains(&accel), "accelerate_drops {accel}");
        assert_eq!(sim.starved(), 0);
        assert_eq!(decoded_packets(&sim, "alice"), sent.len());
    }

    fn max_depth_bound(target: usize) -> usize {
        target + 1 + config::PLAYOUT_ACCEL_FORCE_EXCESS + 1
    }

    #[test]
    fn fast_sender_clock_is_corrected_by_accelerate() {
        let frames = 3_015; // 60 s of a clock 0.5% fast
        let sent = send(&[(0, frames)], false, 19_900.0, speech);
        let mut rng = Lcg(11);
        let arrivals = network("alice", &sent, |_| Some(rng.below(10_000)));
        let mut sim = Sim::new(2_000);
        sim.run(&arrivals, 61_000);

        let d = sim.diag.snapshot();
        // 60 s × 0.5% = 300 ms = 15 frames to remove.
        assert!((10..=30).contains(&d.accelerate_drops), "{d:?}");
        let depth = sim.max_depth["alice"];
        assert!(
            depth <= max_depth_bound(sim.stream("alice").target()),
            "depth {depth}"
        );
        assert_eq!((d.late_packets, lost(&sim)), (0, 0), "no resync, no loss");
        assert_eq!(sim.starved(), 0);
        assert_eq!(decoded_packets(&sim, "alice"), sent.len());
    }

    #[test]
    fn slow_sender_clock_is_absorbed_by_expand_without_runaway() {
        let frames = 2_985; // 60 s of a clock 0.5% slow
        let sent = send(&[(0, frames)], false, 20_100.0, speech);
        let arrivals = network("alice", &sent, |_| Some(0));
        let mut sim = Sim::new(2_000);
        sim.run(&arrivals, 61_000);

        let d = sim.diag.snapshot();
        // 300 ms of missing audio, one expand frame each time the buffer empties.
        assert!((5..=30).contains(&d.expand_frames), "{d:?}");
        assert_eq!(d.accelerate_drops, 0, "{d:?}");
        let target = sim.stream("alice").target();
        assert!(sim.max_depth["alice"] <= target + 1);
        assert_eq!((d.late_packets, d.plc_frames), (0, 0));
        assert_eq!(sim.starved(), 0);
        assert_eq!(decoded_packets(&sim, "alice"), sent.len());
    }

    /// Content with no quiet frame at all (music, a loud room in paranoid
    /// mode) must still be kept off the ring's edge under a fast clock.
    #[test]
    fn never_quiet_content_is_still_bounded_by_forced_accelerate() {
        let frames = 2_010; // 40 s, 0.5% fast
        let sent = send(&[(0, frames)], false, 19_900.0, loud_tone);
        let arrivals = network("alice", &sent, |_| Some(0));
        let mut sim = Sim::new(2_000);
        sim.run(&arrivals, 41_000);

        let d = sim.diag.snapshot();
        assert!(d.accelerate_drops > 0, "{d:?}");
        let depth = sim.max_depth["alice"];
        assert!(
            depth <= max_depth_bound(sim.stream("alice").target()),
            "depth {depth}"
        );
        assert!(depth < config::JITTER_BUF_MAX);
        assert_eq!((d.late_packets, lost(&sim)), (0, 0));
    }

    /// Two senders whose clocks drift in opposite directions are corrected
    /// independently, and on every frame where both played a plain packet the
    /// output is exactly the gain-weighted sum of the two decoded streams.
    #[test]
    fn two_senders_with_opposite_drift_are_corrected_and_mixed_independently() {
        let frames = 1_500; // 30 s
        let fast = send(&[(0, frames)], false, 19_900.0, speech);
        let slow = send(&[(0, frames)], false, 20_100.0, loud_tone);
        let mut arrivals = network("alice", &fast, |_| Some(0));
        arrivals.extend(network("bob", &slow, |_| Some(3_000)));
        arrivals.sort_by_key(|a| a.at_us);

        let mut sim = Sim::new(2_000);
        sim.pump.recorded = Some(Vec::new());
        sim.gains.set_pct("bob", 50);
        sim.run(&arrivals, 31_000);

        assert!(sim.count_ops("alice", is_accel) > 0);
        assert!(sim.count_ops("bob", is_expand) > 0);
        assert_eq!(sim.count_ops("bob", is_accel), 0);
        for name in ["alice", "bob"] {
            let target = sim.stream(name).target();
            assert!(sim.max_depth[name] <= max_depth_bound(target), "{name}");
        }
        assert_eq!(sim.starved(), 0);

        let ref_a = replay(&fast, sim.ops("alice"));
        let ref_b = replay(&slow, sim.ops("bob"));
        let mut compared = 0;
        for (index, mixed) in sim.recorded().iter().enumerate() {
            let (Some(a), Some(b)) = (ref_a.get(&(index as u64)), ref_b.get(&(index as u64)))
            else {
                continue;
            };
            for ((m, a), b) in mixed.iter().zip(a).zip(b) {
                assert_eq!(*m, *a + *b * 0.5, "frame {index}");
            }
            compared += 1;
        }
        assert!(compared > 1_000, "compared {compared}");
    }

    /// Re-decode a stream from its op log with a fresh decoder. Returns the
    /// reference PCM of every frame that was a plain packet decode, by frame.
    fn replay(sent: &[Sent], ops: &[(u64, FrameOp)]) -> HashMap<u64, Vec<f32>> {
        let packet = |seq: u32| &sent.iter().find(|p| p.seq == seq).unwrap().data;
        let mut dec = OpusDecoder::new().unwrap();
        let mut out = HashMap::new();
        for &(index, op) in ops {
            match op {
                FrameOp::Packet(seq) => {
                    out.insert(index, dec.decode(packet(seq)).unwrap().to_vec());
                }
                FrameOp::Accelerated { dropped, played } => {
                    dec.decode(packet(dropped)).unwrap();
                    dec.decode(packet(played)).unwrap();
                }
                FrameOp::Fec { next } => {
                    dec.decode_fec(packet(next)).unwrap();
                }
                FrameOp::Plc | FrameOp::Expand => {
                    dec.decode_plc().unwrap();
                }
            }
        }
        out
    }

    /// A talkspurt of one packet never reaches the target depth; the
    /// activation timeout must still play it.
    #[test]
    fn one_packet_talkspurt_plays_after_the_activation_timeout() {
        let sent = send(&[(0, 1)], false, PERIOD_US, speech);
        let arrivals = network("alice", &sent, |_| Some(0));
        let mut sim = Sim::new(0);
        sim.run(&arrivals, 500);

        let ops: Vec<FrameOp> = sim.ops("alice").iter().map(|(_, op)| *op).collect();
        let mut expect = vec![FrameOp::Packet(1)];
        expect.extend([FrameOp::Expand; config::PLAYOUT_HOLD_FRAMES as usize]);
        assert_eq!(ops, expect);
        assert_eq!(decoded_packets(&sim, "alice"), 1);
        assert_eq!(sim.diag.snapshot().expand_frames, 0);
        assert_eq!(sim.engine().playing(), 0);
    }

    #[test]
    fn steady_state_does_not_grow_any_buffer() {
        let sent = send(&[(0, 600)], false, 19_900.0, speech);
        let mut rng = Lcg(3);
        let arrivals = network("alice", &sent, |_| Some(rng.below(20_000)));
        let mut sim = Sim::new(0);
        let capacities = |sim: &Sim| {
            let s = sim.engine().streams.get("alice")?;
            let slots: usize = s.jitter.slots.iter().map(|(_, v)| v.capacity()).sum();
            Some((
                slots + sim.engine().scratch.opus.capacity(),
                sim.engine().scratch.frame.capacity(),
                sim.engine().scratch.head.capacity(),
                sim.engine().streams.capacity(),
            ))
        };
        let mut warm = None;
        let mut later = Vec::new();
        sim.run_probed(&arrivals, 12_000, |sim, t| {
            if t == 2_000 {
                warm = capacities(sim);
            } else if t > 2_000 && t < 11_000 {
                later.push(capacities(sim));
            }
        });
        assert!(warm.is_some());
        assert!(later.iter().all(|c| *c == warm), "{warm:?} → {later:?}");
        assert!(
            sim.diag.snapshot().accelerate_drops > 0,
            "edit path exercised"
        );
    }

    /// A pump fed `packets` frames from one sender, all arrived at `t0`.
    fn pump_with_packets(t0: Instant, packets: u32) -> PlayoutPump {
        let mut pump = PlayoutPump::new(t0);
        let mut enc = fec_encoder();
        let pcm = vec![0.1f32; config::FRAME_SIZE];
        for seq in 1..=packets {
            let opus = enc.encode(&pcm).unwrap().to_vec();
            pump.engine.on_packet("alice", seq, &opus, t0);
        }
        pump
    }

    /// The pump renders until the output holds the low-water mark: ring
    /// occupancy plus what the callback reports queued.
    #[test]
    fn pump_fills_the_output_to_the_low_water_mark() {
        use ringbuf::traits::{Observer, Split};
        let t0 = Instant::now();
        let mut pump = pump_with_packets(t0, 8);
        let (mut prod, _cons) = ringbuf::HeapRb::<f32>::new(config::FRAME_SIZE * 8).split();
        let signals = PlayoutSignals::default();
        let diag = AudioDiagnostics::default();
        let (gains, stats) = (PeerGains::new(), NetworkStats::new());

        // The callback still holds a low-water mark's worth: nothing to do.
        signals
            .queued
            .store(config::PLAYOUT_OUTPUT_LOW_WATER as u32, Ordering::Relaxed);
        pump.pump(Some(&mut prod), t0, &signals, &gains, &diag, &stats);
        assert_eq!(prod.occupied_len(), 0);

        signals.queued.store(0, Ordering::Relaxed);
        pump.pump(Some(&mut prod), t0, &signals, &gains, &diag, &stats);
        assert_eq!(prod.occupied_len(), config::PLAYOUT_OUTPUT_LOW_WATER);
        assert_eq!(signals.playing.load(Ordering::Relaxed), 1);

        // A driver whose callback size varies: the low water must cover the
        // largest request seen, not the last one. 2048 + one frame of slack
        // = 3008 → four frames, although the last callback asked for 960.
        let (_, mut device_side) = ringbuf::HeapRb::<f32>::new(config::FRAME_SIZE).split();
        let mut renderer = crate::audio::PlaybackRenderer::new(config::SAMPLE_RATE, 1);
        for request in [480, 2048, 960] {
            let mut buf = vec![0.0f32; request];
            renderer.render(&mut device_side, 1.0, &mut buf, &diag, &signals);
        }
        assert_eq!(signals.request.load(Ordering::Relaxed), 2048);
        pump.pump(Some(&mut prod), t0, &signals, &gains, &diag, &stats);
        assert_eq!(prod.occupied_len(), 4 * config::FRAME_SIZE);
    }

    /// A ring smaller than the low-water mark (a device that stopped pulling
    /// looks the same) must end the loop instead of spinning, and the refused
    /// samples are counted.
    #[test]
    fn pump_stops_on_a_full_ring_and_counts_the_overflow() {
        use ringbuf::traits::Split;
        let t0 = Instant::now();
        let mut pump = pump_with_packets(t0, 8);
        let (mut prod, _cons) = ringbuf::HeapRb::<f32>::new(config::FRAME_SIZE).split();
        let signals = PlayoutSignals::default();
        let diag = AudioDiagnostics::default();
        pump.pump(
            Some(&mut prod),
            t0,
            &signals,
            &PeerGains::new(),
            &diag,
            &NetworkStats::new(),
        );
        assert_eq!(
            diag.snapshot().playback_ring_overflow,
            config::FRAME_SIZE as u64
        );
    }

    /// With no playback device the engine still advances at wall-clock rate,
    /// including frames a late wakeup would otherwise skip.
    #[test]
    fn pump_without_a_device_advances_at_real_time() {
        let t0 = Instant::now();
        let mut pump = pump_with_packets(t0, 12);
        let signals = PlayoutSignals::default();
        let stats = NetworkStats::new();
        let (gains, diag) = (PeerGains::new(), AudioDiagnostics::default());

        pump.pump(None, t0, &signals, &gains, &diag, &stats);
        // A low-water mark ahead of now: two frames.
        assert_eq!(stats.window_counts().0, 2);
        assert_eq!(signals.playing.load(Ordering::Relaxed), 1);

        // 100 ms later the virtual device has taken five frames; the pump
        // renders those plus the low-water mark ahead again.
        pump.pump(
            None,
            t0 + Duration::from_millis(100),
            &signals,
            &gains,
            &diag,
            &stats,
        );
        assert_eq!(stats.window_counts().0, 7);
    }

    /// An idle channel must not keep the 5 ms timer running: the tick is
    /// wanted only while something is buffered or playing.
    #[test]
    fn playout_tick_is_wanted_only_while_something_is_buffered_or_playing() {
        let sent = send(&[(0, 20)], false, PERIOD_US, speech);
        let arrivals = network("alice", &sent, |_| Some(0));
        let mut sim = Sim::new(0);
        assert!(!sim.pump.needs_tick(), "a fresh pump is idle");
        let mut wanted_mid = false;
        sim.run_probed(&arrivals, 1_000, |sim, t| {
            if t == 200 {
                wanted_mid = sim.pump.needs_tick();
            }
        });
        assert!(wanted_mid, "mid-talkspurt the tick is armed");
        assert!(!sim.pump.needs_tick(), "after the talkspurt it is not");
    }

    // ── device timing ────────────────────────────────────────────────

    /// Windows-like timing: the playout tick fires at the 15.6 ms timer
    /// granularity and the device takes 480 samples every 10 ms. Packet
    /// arrival still wakes the task directly (IOCP is not timer-bound).
    #[test]
    fn windows_timer_granularity_with_a_10ms_device_never_starves() {
        let sent = send(&[(0, 1000)], false, PERIOD_US, speech);
        let mut rng = Lcg(21);
        let arrivals = network("alice", &sent, |_| Some(rng.below(30_000)));
        let device = Device {
            request: config::FRAME_SIZE / 2,
        };
        let mut sim = Sim::with(2_000, device, WINDOWS_TIMER_US);
        sim.run(&arrivals, 21_000);

        let d = sim.since_warmup();
        assert_eq!(d.output_starved, 0, "{d:?}");
        assert_eq!(
            (d.expand_frames, d.plc_frames, d.late_packets),
            (0, 0, 0),
            "{d:?}"
        );
        assert_eq!(decoded_packets(&sim, "alice"), sent.len());
    }

    /// A device whose callback takes more than the 2-frame low-water mark
    /// leaves after it would hit an empty queue whenever a callback lands
    /// just before a wakeup; the low water follows the request size.
    #[test]
    fn a_2048_sample_callback_device_never_starves() {
        let sent = send(&[(0, 1000)], false, PERIOD_US, speech);
        let mut rng = Lcg(5);
        let arrivals = network("alice", &sent, |_| Some(rng.below(30_000)));
        let mut sim = Sim::with(2_000, Device { request: 2048 }, 5_000.0);
        sim.run(&arrivals, 21_000);

        let d = sim.since_warmup();
        assert_eq!(d.output_starved, 0, "{d:?}");
        assert_eq!((d.expand_frames, d.late_packets), (0, 0), "{d:?}");
        assert_eq!(decoded_packets(&sim, "alice"), sent.len());
    }

    // ── talkspurt boundaries ─────────────────────────────────────────

    /// The previous talkspurt's last packet was lost, so the stream went idle
    /// expecting it. The next talkspurt must start at its own first packet,
    /// not with FEC/PLC of a frame encoded seconds earlier — but the loss
    /// still counts.
    #[test]
    fn a_lost_last_packet_is_skipped_at_the_next_activation() {
        let sent = send(&[(10, 60), (150, 200)], true, PERIOD_US, speech);
        let second_first = sent.iter().find(|p| p.send_us >= 150 * 20_000).unwrap().seq;
        let lost_seq = second_first - 1;
        let arrivals = network("alice", &sent, |p| (p.seq != lost_seq).then_some(0));
        let mut sim = Sim::new(0);
        sim.run(&arrivals, 5_000);

        let ops: Vec<FrameOp> = sim.ops("alice").iter().map(|(_, op)| *op).collect();
        let resumed = ops
            .iter()
            .position(|op| *op == FrameOp::Packet(second_first))
            .expect("second talkspurt played");
        assert!(
            ops[..resumed]
                .iter()
                .all(|op| !matches!(op, FrameOp::Fec { .. } | FrameOp::Plc)),
            "the lost packet was concealed: {ops:?}"
        );
        assert_eq!(lost(&sim), 0);
        assert_eq!(sim.stats.window_counts().1, 1, "still counted as lost");
        assert_eq!(decoded_packets(&sim, "alice"), sent.len() - 1);
        let d = sim.diag.snapshot();
        assert_eq!((d.plc_frames, d.fec_recovered, d.late_packets), (0, 0, 0));
    }

    /// The very first pair of a sender's first talkspurt arrives swapped:
    /// the true first packet must become the start, not be dropped as late.
    /// (A stream rebuilt after `SENDER_STREAM_TTL_SECS` of silence starts
    /// the same way.)
    #[test]
    fn first_talkspurt_survives_a_swapped_first_pair() {
        let sent = send(&[(0, 50)], false, PERIOD_US, speech);
        let arrivals = network("alice", &sent, |p| {
            Some(if p.seq == 1 { 25_000 } else { 0 })
        });
        assert_eq!(arrivals[0].seq, 2, "seq 2 must arrive first");
        let mut sim = Sim::new(0);
        sim.run(&arrivals, 2_000);

        assert_eq!(sim.ops("alice")[0].1, FrameOp::Packet(1));
        assert_eq!(sim.diag.snapshot().late_packets, 0);
        assert_eq!(decoded_packets(&sim, "alice"), sent.len());
    }

    #[test]
    fn jitter_buffer_reanchors_only_before_anything_was_consumed() {
        let mut out = Vec::new();
        let mut jb = JitterBuffer::new();
        assert!(!jb.push(5, &[5], now()));
        assert!(!jb.push(4, &[4], now()), "earlier start accepted");
        assert!(matches!(jb.pop(&mut out), PopResult::Packet));
        assert_eq!(out, vec![4]);
        // Once playback consumed something, an earlier seq is late again.
        assert!(jb.push(3, &[3], now()));
        assert!(matches!(jb.pop(&mut out), PopResult::Packet));
        assert_eq!(out, vec![5]);
    }

    #[test]
    fn jitter_buffer_reanchor_is_bounded() {
        let mut jb = JitterBuffer::new();
        jb.push(100, &[1], now());
        assert!(
            jb.push(100 - REANCHOR_MAX - 1, &[2], now()),
            "older than the bound is late"
        );
        assert!(
            !jb.push(100 - REANCHOR_MAX, &[3], now()),
            "within the bound"
        );
        let mut out = Vec::new();
        assert!(matches!(jb.pop(&mut out), PopResult::Packet));
        assert_eq!(out, vec![3]);
    }

    #[test]
    fn jitter_buffer_reanchor_never_pushes_buffered_packets_out_of_the_ring() {
        let mut jb = JitterBuffer::new();
        let top = 100 + config::JITTER_BUF_MAX as u32 - 2;
        jb.push(100, &[1], now());
        jb.push(top, &[2], now());
        // Moving the start back 2 would put `top` beyond the ring.
        assert!(jb.push(98, &[3], now()));
        assert!(!jb.push(99, &[4], now()));
    }

    #[test]
    fn jitter_buffer_skip_to_oldest_counts_the_hole() {
        let mut jb = JitterBuffer::new();
        let mut out = Vec::new();
        jb.push(1, &[1], now());
        assert!(matches!(jb.pop(&mut out), PopResult::Packet));
        jb.push(4, &[4], now());
        assert_eq!(jb.skip_to_oldest(), 2);
        assert_eq!(jb.skip_to_oldest(), 0);
        assert!(matches!(jb.pop(&mut out), PopResult::Packet));
        assert_eq!(out, vec![4]);
    }

    // ── jitter estimate ─────────────────────────────────────────────

    /// Neither the silence between talkspurts (seconds of arrival gap, one
    /// seq step) nor the onset burst (six packets captured 100 ms apart, sent
    /// at once) is network jitter.
    #[test]
    fn jitter_estimate_ignores_silence_gaps_and_preroll_bursts() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut out = Vec::new();
        let mut jb = JitterBuffer::new();
        let mut seq = 1u32;
        let mut ms = 0u64;
        for _ in 0..3 {
            for _ in 0..TALKSPURT_ONSET_PACKETS {
                jb.push(seq, &[1], at(ms));
                seq += 1;
            }
            for _ in 0..50 {
                ms += 20;
                jb.push(seq, &[1], at(ms));
                jb.pop(&mut out);
                seq += 1;
            }
            while let PopResult::Packet = jb.pop(&mut out) {}
            jb.end_talkspurt();
            ms += 2_000;
        }
        assert!(jb.jitter_ms() < 0.5, "estimate {}", jb.jitter_ms());

        // The same stream with real jitter does register.
        let mut jb = JitterBuffer::new();
        for i in 0..200u32 {
            let wobble = if i % 2 == 0 { 0 } else { 15 };
            jb.push(i + 1, &[1], at(i as u64 * 20 + wobble));
            jb.pop(&mut out);
        }
        assert!(jb.jitter_ms() > 10.0, "estimate {}", jb.jitter_ms());
    }

    #[test]
    fn engine_keeps_the_minimum_target_across_vad_talkspurts() {
        let spurts: Vec<(u64, u64)> = (0..6).map(|i| (10 + i * 150, 10 + i * 150 + 75)).collect();
        let sent = send(&spurts, true, PERIOD_US, speech);
        let arrivals = network("alice", &sent, |_| Some(0));
        let mut sim = Sim::new(0);
        sim.run(&arrivals, 19_000);

        let s = sim.stream("alice");
        assert!(
            s.jitter.jitter_ms() < 1.0,
            "estimate {}",
            s.jitter.jitter_ms()
        );
        assert_eq!(s.target(), config::PLAYOUT_TARGET_MIN_FRAMES);
        assert_eq!(decoded_packets(&sim, "alice"), sent.len());
    }

    #[test]
    fn target_follows_the_formula_within_bounds() {
        assert_eq!(target_frames(0.0), config::PLAYOUT_TARGET_MIN_FRAMES);
        assert_eq!(target_frames(10.0), 2); // (20 + 20) / 20
        assert_eq!(target_frames(15.0), 3); // (30 + 20) / 20 → 2.5 → 3
        assert_eq!(target_frames(1_000.0), config::PLAYOUT_TARGET_MAX_FRAMES);
    }

    // ── direct feed: mixing, gain, FEC/PLC branches ──────────────────

    fn encoded(enc: &mut OpusEncoder, value: f32) -> Vec<u8> {
        enc.encode(&vec![value; config::FRAME_SIZE])
            .unwrap()
            .to_vec()
    }

    struct Direct {
        engine: PlayoutEngine,
        diag: AudioDiagnostics,
        stats: NetworkStats,
        gains: PeerGains,
        t0: Instant,
    }

    impl Direct {
        fn new() -> Self {
            Self {
                engine: PlayoutEngine::new(),
                diag: AudioDiagnostics::default(),
                stats: NetworkStats::new(),
                gains: PeerGains::new(),
                t0: Instant::now(),
            }
        }

        fn feed(&mut self, name: &str, seq: u32, data: &[u8]) {
            self.engine.on_packet(name, seq, data, self.t0);
        }

        /// Render until every stream is idle again, far enough in the future
        /// that the activation timeout has passed.
        fn render_all(&mut self) -> Vec<Vec<f32>> {
            let now = self.t0 + Duration::from_secs(1);
            let mut out = vec![0.0f32; config::FRAME_SIZE];
            let mut frames = Vec::new();
            while let Some(n) = self.engine.render_frame(
                now,
                Output::new(0, 0).pull(),
                &mut out,
                &self.gains,
                &self.diag,
                &self.stats,
            ) {
                frames.push(out[..n].to_vec());
            }
            frames
        }

        fn ops(&self, name: &str) -> Vec<FrameOp> {
            self.engine.streams[name]
                .ops
                .iter()
                .map(|(_, op)| *op)
                .collect()
        }
    }

    const HOLD: usize = config::PLAYOUT_HOLD_FRAMES as usize;

    #[test]
    fn render_returns_none_with_nothing_playing() {
        let mut d = Direct::new();
        assert!(d.render_all().is_empty());
        // Buffered but not yet due: still nothing to play.
        let mut enc = fec_encoder();
        d.feed("alice", 1, &encoded(&mut enc, 0.1));
        let mut out = vec![0.0f32; config::FRAME_SIZE];
        let r = d.engine.render_frame(
            d.t0,
            Output::new(0, 0).pull(),
            &mut out,
            &d.gains,
            &d.diag,
            &d.stats,
        );
        assert_eq!(r, None);
        assert_eq!(d.engine.playing(), 0);
    }

    /// Two senders with independent sequence counters (each starts near its own
    /// base) must both decode and mix — a shared jitter buffer would treat the
    /// interleaving as endless stream restarts and produce almost nothing.
    #[test]
    fn mixes_two_concurrent_sender_streams() {
        let mut enc_a = fec_encoder();
        let mut enc_b = fec_encoder();
        let mut d = Direct::new();
        for i in 0..5u32 {
            d.feed("alice", 1 + i, &encoded(&mut enc_a, 0.0));
            d.feed("bob", 100_000 + i, &encoded(&mut enc_b, 0.0));
        }
        let frames = d.render_all();
        // One mixed frame per round, not one per packet.
        assert_eq!(frames.len(), 5 + HOLD);
        for name in ["alice", "bob"] {
            let ops = &d.engine.streams[name].ops;
            assert_eq!((decoded_in(ops), lost_in(ops)), (5, 0), "{name}");
        }
    }

    #[test]
    fn single_sender_passes_through_bit_exact() {
        let mut enc = fec_encoder();
        let packets: Vec<Vec<u8>> = (0..3).map(|i| encoded(&mut enc, 0.1 * i as f32)).collect();
        let mut d = Direct::new();
        for (i, p) in packets.iter().enumerate() {
            d.feed("alice", 1 + i as u32, p);
        }
        let frames = d.render_all();
        assert_eq!(frames.len(), 3 + HOLD);
        let mut dec = OpusDecoder::new().unwrap();
        for (frame, p) in frames.iter().zip(&packets) {
            assert_eq!(frame.as_slice(), dec.decode(p).unwrap());
        }
        assert_eq!(lost_in(&d.engine.streams["alice"].ops), 0);
    }

    /// A per-peer gain of 0% mutes that sender locally: the mixed output is
    /// silent even though the packet was received and decoded (decoder/jitter
    /// state stays current).
    #[test]
    fn applies_per_peer_gain() {
        let mut enc = fec_encoder();
        let mut d = Direct::new();
        for i in 0..3u32 {
            d.feed("loud", 1 + i, &encoded(&mut enc, 0.5));
        }
        d.gains.set_pct("loud", 0);
        let frames = d.render_all();
        assert_eq!(frames.len(), 3 + HOLD, "muted peer still produces frames");
        assert!(frames.iter().flatten().all(|s| *s == 0.0));
        assert_eq!(decoded_in(&d.engine.streams["loud"].ops), 3);
    }

    #[test]
    fn uses_fec_when_next_packet_is_buffered() {
        let mut enc = fec_encoder();
        let mut d = Direct::new();
        // seq 2 is lost; 3 carries its redundancy.
        for seq in [1u32, 3, 4] {
            d.feed("alice", seq, &encoded(&mut enc, 0.3));
        }
        let frames = d.render_all();
        let snap = d.diag.snapshot();
        assert_eq!((snap.fec_recovered, snap.plc_frames), (1, 0));
        // 1, recovered 2, then 3 and 4 decode normally.
        assert_eq!(
            d.ops("alice")[..4],
            [
                FrameOp::Packet(1),
                FrameOp::Fec { next: 3 },
                FrameOp::Packet(3),
                FrameOp::Packet(4)
            ]
        );
        assert_eq!(frames.len(), 4 + HOLD);
    }

    #[test]
    fn falls_back_to_plc_when_fec_decode_fails() {
        let mut enc = fec_encoder();
        let mut d = Direct::new();
        for seq in [1u32, 3] {
            d.feed("alice", seq, &encoded(&mut enc, 0.3));
        }
        // Corrupt the next packet so the FEC decode fails and PLC takes over.
        d.engine.streams.get_mut("alice").unwrap().jitter.slots[3 % config::JITTER_BUF_MAX].1 =
            vec![0xFF];
        d.render_all();
        let snap = d.diag.snapshot();
        assert_eq!((snap.fec_recovered, snap.plc_frames), (0, 1));
    }

    #[test]
    fn burst_loss_mixes_plc_and_fec() {
        let mut enc = fec_encoder();
        let mut d = Direct::new();
        // 2 and 3 are lost. 2 has no successor buffered (PLC); 3 is followed by 4 (FEC).
        for seq in [1u32, 4] {
            d.feed("alice", seq, &encoded(&mut enc, 0.3));
        }
        let frames = d.render_all();
        let snap = d.diag.snapshot();
        assert_eq!((snap.fec_recovered, snap.plc_frames), (1, 1));
        assert_eq!(frames.len(), 4 + HOLD);
        assert_eq!(lost_in(&d.engine.streams["alice"].ops), 2);
    }

    #[test]
    fn celt_only_next_packet_takes_plc() {
        let mut enc = fec_encoder();
        let mut d = Direct::new();
        d.feed("alice", 1, &encoded(&mut enc, 0.3));
        // TOC 0xF8 = config 31 (CELT fullband, 20 ms): never carries LBRR.
        d.feed("alice", 3, &[0xF8, 0, 0, 0]);
        d.render_all();
        let snap = d.diag.snapshot();
        assert_eq!((snap.fec_recovered, snap.plc_frames), (0, 1));
    }

    #[test]
    fn two_loud_senders_are_summed_unclamped_then_bounded_by_render() {
        use crate::audio::{PlaybackRenderer, PlayoutSignals};
        use ringbuf::traits::{Producer, Split};

        let mut d = Direct::new();
        for name in ["alice", "bob"] {
            d.gains.set_pct(name, 200);
            let mut enc = fec_encoder();
            for seq in 1..=8u32 {
                d.feed(name, seq, &encoded(&mut enc, 0.9));
            }
        }
        let mixed: Vec<f32> = d.render_all().concat();
        let peak = mixed.iter().fold(0.0f32, |p, s| p.max(s.abs()));
        assert!(peak > 1.0, "mix must pass through unclamped, peak {peak}");

        // Same samples through the output stage.
        let (mut prod, mut cons) = ringbuf::HeapRb::<f32>::new(mixed.len()).split();
        prod.push_slice(&mixed);
        let mut renderer = PlaybackRenderer::new(config::SAMPLE_RATE, 1);
        let diag = AudioDiagnostics::default();
        let signals = PlayoutSignals::default();
        let mut out = vec![0.0f32; config::FRAME_SIZE];
        for _ in 0..mixed.len() / config::FRAME_SIZE {
            renderer.render(&mut cons, 1.0, &mut out, &diag, &signals);
            assert!(out.iter().all(|s| s.abs() < 1.0));
        }
        assert!(diag.limiter_playback.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn stale_senders_are_pruned() {
        let mut enc = fec_encoder();
        let mut d = Direct::new();
        d.feed("alice", 1, &encoded(&mut enc, 0.1));
        let later = d.t0 + Duration::from_secs(config::SENDER_STREAM_TTL_SECS + 1);
        d.engine.prune(later);
        assert!(d.engine.streams.is_empty());
    }

    // ── seq_before ───────────────────────────────────────────────────

    #[test]
    fn seq_before_basic() {
        assert!(seq_before(1, 2));
        assert!(seq_before(0, 1));
        assert!(!seq_before(2, 1));
        assert!(!seq_before(5, 5)); // equal
    }

    #[test]
    fn seq_before_wrapping() {
        // Near u32::MAX boundary
        assert!(seq_before(u32::MAX - 1, u32::MAX));
        assert!(seq_before(u32::MAX, 0)); // wrap around
        assert!(seq_before(u32::MAX, 1)); // wrap around
        assert!(!seq_before(1, u32::MAX)); // 1 is "after" MAX in wrapping
    }

    // ── JitterBuffer ─────────────────────────────────────────────────

    fn now() -> Instant {
        Instant::now()
    }

    #[test]
    fn jitter_peek_does_not_consume() {
        let mut jb = JitterBuffer::new();
        jb.push(5, &[1, 2], now());
        assert_eq!(jb.peek(5), Some(&[1u8, 2][..]));
        assert_eq!(jb.peek(6), None);
        assert_eq!(jb.peek(5), Some(&[1u8, 2][..]));
    }

    #[test]
    fn jitter_buffer_push_pop_in_order() {
        let mut jb = JitterBuffer::new();
        let mut out = Vec::new();

        jb.push(1, &[10, 20], now());
        jb.push(2, &[30, 40], now());

        assert!(matches!(jb.pop(&mut out), PopResult::Packet));
        assert_eq!(out, vec![10, 20]);
        assert!(matches!(jb.pop(&mut out), PopResult::Packet));
        assert_eq!(out, vec![30, 40]);
        assert!(matches!(jb.pop(&mut out), PopResult::Empty));
    }

    #[test]
    fn jitter_buffer_missing_packet_detected() {
        let mut jb = JitterBuffer::new();
        let mut out = Vec::new();

        jb.push(1, &[10], now());
        // Skip seq 2
        jb.push(3, &[30], now());

        assert!(matches!(jb.pop(&mut out), PopResult::Packet)); // seq 1
        assert_eq!(out, vec![10]);
        assert!(matches!(jb.pop(&mut out), PopResult::Missing)); // seq 2 gap
        assert!(matches!(jb.pop(&mut out), PopResult::Packet)); // seq 3
        assert_eq!(out, vec![30]);
    }

    /// An underrun is not a loss: `Empty` must leave the expected seq alone,
    /// so the packet (or the next talkspurt) that continues it still plays.
    #[test]
    fn jitter_buffer_empty_does_not_consume_a_seq() {
        let mut jb = JitterBuffer::new();
        let mut out = Vec::new();
        jb.push(1, &[1], now());
        assert!(matches!(jb.pop(&mut out), PopResult::Packet));
        for _ in 0..5 {
            assert!(matches!(jb.pop(&mut out), PopResult::Empty));
        }
        assert!(!jb.push(2, &[2], now()), "the continuation is not late");
        assert!(matches!(jb.pop(&mut out), PopResult::Packet));
        assert_eq!(out, vec![2]);
    }

    #[test]
    fn jitter_buffer_late_packet_dropped() {
        let mut jb = JitterBuffer::new();
        let mut out = Vec::new();

        jb.push(5, &[50], now());
        jb.pop(&mut out); // consume seq 5, next_seq=6

        // Push seq 5 again (late) — should be silently dropped
        jb.push(5, &[99], now());
        assert!(matches!(jb.pop(&mut out), PopResult::Empty));
    }

    #[test]
    fn jitter_buffer_count_buffered() {
        let mut jb = JitterBuffer::new();
        jb.push(1, &[10], now());
        jb.push(2, &[20], now());
        jb.push(3, &[30], now());
        assert_eq!(jb.count_buffered(), 3);
    }

    #[test]
    fn jitter_buffer_reset_to() {
        let mut jb = JitterBuffer::new();
        jb.push(100, &[1], now());
        jb.push(101, &[2], now());

        jb.reset_to(200);
        assert_eq!(jb.count_buffered(), 0);
        assert_eq!(jb.jitter_ms(), 0.0);

        // Can push and pop from new sequence
        let mut out = Vec::new();
        jb.push(200, &[42], now());
        assert!(matches!(jb.pop(&mut out), PopResult::Packet));
        assert_eq!(out, vec![42]);
    }

    #[test]
    fn jitter_buffer_stream_restart_detection() {
        let mut jb = JitterBuffer::new();
        let mut out = Vec::new();

        // Simulate an advanced stream
        jb.push(1000, &[1], now());
        jb.pop(&mut out); // next_seq = 1001

        // Sender restarts at seq 1 — large backward jump
        jb.push(1, &[99], now());

        // Buffer should have auto-reset
        assert!(matches!(jb.pop(&mut out), PopResult::Packet));
        assert_eq!(out, vec![99]);
    }

    /// A sustained forward gap (loss burst / deaf window longer than the ring)
    /// must resync the buffer onto the new stream position — previously the
    /// buffer dropped every packet forever and playback went permanently
    /// silent while the speaking indicator stayed alive.
    #[test]
    fn jitter_buffer_resyncs_after_sustained_forward_gap() {
        let mut jb = JitterBuffer::new();
        let mut out = Vec::new();

        jb.push(1, &[1], now());
        assert!(matches!(jb.pop(&mut out), PopResult::Packet));

        // Stream resumes far ahead (e.g. after 20s of loss): the first
        // JITTER_RESYNC_AFTER - 1 packets are dropped, the next one resyncs.
        let base = 5000u32;
        for i in 0..config::JITTER_RESYNC_AFTER {
            jb.push(base + i, &[i as u8], now());
        }
        // Resync happened on the last push; it must be buffered and playable.
        assert!(matches!(jb.pop(&mut out), PopResult::Packet));
        assert_eq!(out, vec![(config::JITTER_RESYNC_AFTER - 1) as u8]);

        // And the stream continues normally from there.
        jb.push(base + config::JITTER_RESYNC_AFTER, &[42], now());
        assert!(matches!(jb.pop(&mut out), PopResult::Packet));
        assert_eq!(out, vec![42]);
    }

    /// One stray far-ahead packet (reordered/bogus) must NOT reset a healthy
    /// stream — only a sustained run of out-of-window packets does.
    #[test]
    fn jitter_buffer_single_stray_far_ahead_does_not_resync() {
        let mut jb = JitterBuffer::new();
        let mut out = Vec::new();

        jb.push(1, &[1], now());
        jb.push(900_000, &[99], now()); // stray
        jb.push(2, &[2], now()); // healthy stream continues

        assert!(matches!(jb.pop(&mut out), PopResult::Packet));
        assert_eq!(out, vec![1]);
        assert!(matches!(jb.pop(&mut out), PopResult::Packet));
        assert_eq!(out, vec![2]);
    }

    #[test]
    fn jitter_buffer_far_ahead_packet_dropped() {
        let mut jb = JitterBuffer::new();
        jb.push(1, &[10], now());
        // Push way ahead (beyond capacity)
        jb.push(1 + config::JITTER_BUF_MAX as u32, &[99], now());

        let mut out = Vec::new();
        assert!(matches!(jb.pop(&mut out), PopResult::Packet));
        assert_eq!(out, vec![10]);
        // The far-ahead packet should have been dropped
        assert!(matches!(jb.pop(&mut out), PopResult::Empty));
    }

    #[test]
    fn jitter_push_reports_late_packet() {
        let mut jb = JitterBuffer::new();
        let mut out = Vec::new();
        assert!(!jb.push(1, &[1], now()));
        assert!(!jb.push(2, &[2], now()));
        assert!(matches!(jb.pop(&mut out), PopResult::Packet));
        assert!(matches!(jb.pop(&mut out), PopResult::Packet));
        // seq 1 was already played.
        assert!(jb.push(1, &[9], now()));
    }
}
