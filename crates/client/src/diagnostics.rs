//! Audio glitch counters. Every audible defect on the capture/playback paths
//! bumps one of these, so a fix can be judged by numbers instead of by ear.
//! Relaxed atomics only: the audio callbacks must never lock or allocate.

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct AudioDiagnostics {
    /// Data → silence transitions in the playback callback: a short read
    /// after data, including an empty one right after a satisfied read.
    /// Callbacks spent in continued silence are not counted.
    pub playback_dry: AtomicU64,
    /// Samples (device rate) discarded by the latency-cap trim.
    pub playback_trimmed_samples: AtomicU64,
    /// Samples the decode → playback ring refused because it was full.
    pub playback_ring_overflow: AtomicU64,
    /// Samples the capture → encoder ring refused because it was full.
    pub capture_ring_overflow: AtomicU64,
    /// Frames synthesised by packet-loss concealment.
    pub plc_frames: AtomicU64,
    /// Losses where the next packet was available and an FEC decode was
    /// attempted (libopus conceals internally when that packet carries no LBRR).
    pub fec_recovered: AtomicU64,
    /// Playback callbacks where the output limiter reduced gain.
    pub limiter_playback: AtomicU64,
    /// Capture frames where the input-gain limiter reduced gain.
    pub limiter_capture: AtomicU64,
    /// Packets dropped because they arrived after their slot was played.
    pub late_packets: AtomicU64,
    /// Frames dropped by playout acceleration (latency / clock-drift
    /// correction, crossfaded at a quiet frame).
    pub accelerate_drops: AtomicU64,
    /// Concealment frames played because a sender's buffer ran empty mid
    /// talkspurt (counted once the stream resumed with a real packet; the
    /// concealment after a talkspurt's last packet is not counted).
    pub expand_frames: AtomicU64,
    /// Data → silence transitions in the playback callback while at least one
    /// sender was playing: the playout loop failed to keep the output fed.
    pub output_starved: AtomicU64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticsSnapshot {
    pub playback_dry: u64,
    pub playback_trimmed_samples: u64,
    pub playback_ring_overflow: u64,
    pub capture_ring_overflow: u64,
    pub plc_frames: u64,
    pub fec_recovered: u64,
    pub limiter_playback: u64,
    pub limiter_capture: u64,
    pub late_packets: u64,
    pub accelerate_drops: u64,
    pub expand_frames: u64,
    pub output_starved: u64,
}

impl AudioDiagnostics {
    pub fn snapshot(&self) -> DiagnosticsSnapshot {
        let get = |c: &AtomicU64| c.load(Ordering::Relaxed);
        DiagnosticsSnapshot {
            playback_dry: get(&self.playback_dry),
            playback_trimmed_samples: get(&self.playback_trimmed_samples),
            playback_ring_overflow: get(&self.playback_ring_overflow),
            capture_ring_overflow: get(&self.capture_ring_overflow),
            plc_frames: get(&self.plc_frames),
            fec_recovered: get(&self.fec_recovered),
            limiter_playback: get(&self.limiter_playback),
            limiter_capture: get(&self.limiter_capture),
            late_packets: get(&self.late_packets),
            accelerate_drops: get(&self.accelerate_drops),
            expand_frames: get(&self.expand_frames),
            output_starved: get(&self.output_starved),
        }
    }
}

impl DiagnosticsSnapshot {
    /// Counts accumulated since `earlier` (saturating, so a stale baseline
    /// can never underflow).
    pub fn delta(&self, earlier: &DiagnosticsSnapshot) -> DiagnosticsSnapshot {
        DiagnosticsSnapshot {
            playback_dry: self.playback_dry.saturating_sub(earlier.playback_dry),
            playback_trimmed_samples: self
                .playback_trimmed_samples
                .saturating_sub(earlier.playback_trimmed_samples),
            playback_ring_overflow: self
                .playback_ring_overflow
                .saturating_sub(earlier.playback_ring_overflow),
            capture_ring_overflow: self
                .capture_ring_overflow
                .saturating_sub(earlier.capture_ring_overflow),
            plc_frames: self.plc_frames.saturating_sub(earlier.plc_frames),
            fec_recovered: self.fec_recovered.saturating_sub(earlier.fec_recovered),
            limiter_playback: self
                .limiter_playback
                .saturating_sub(earlier.limiter_playback),
            limiter_capture: self.limiter_capture.saturating_sub(earlier.limiter_capture),
            late_packets: self.late_packets.saturating_sub(earlier.late_packets),
            accelerate_drops: self
                .accelerate_drops
                .saturating_sub(earlier.accelerate_drops),
            expand_frames: self.expand_frames.saturating_sub(earlier.expand_frames),
            output_starved: self.output_starved.saturating_sub(earlier.output_starved),
        }
    }

    /// Whether any counter that signals a real defect moved. Excluded, because
    /// they move in a healthy call: `playback_dry` (the output goes silent at
    /// the end of every phrase) and `accelerate_drops` (the VAD pre-roll burst
    /// puts one or two frames of excess latency into every talkspurt, removed
    /// at the first quiet frame). A safety-cap trim is a defect since playout
    /// became pull-based, so `playback_trimmed_samples` counts.
    pub fn has_glitches(&self) -> bool {
        self.plc_frames
            + self.fec_recovered
            + self.playback_trimmed_samples
            + self.playback_ring_overflow
            + self.capture_ring_overflow
            + self.late_packets
            + self.limiter_playback
            + self.limiter_capture
            + self.expand_frames
            + self.output_starved
            > 0
    }

    pub fn is_zero(&self) -> bool {
        *self == DiagnosticsSnapshot::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_delta_and_is_zero() {
        let d = AudioDiagnostics::default();
        let base = d.snapshot();
        assert!(base.is_zero());

        d.plc_frames.fetch_add(3, Ordering::Relaxed);
        d.late_packets.fetch_add(1, Ordering::Relaxed);
        let now = d.snapshot();
        assert!(!now.is_zero());

        let delta = now.delta(&base);
        assert_eq!(delta.plc_frames, 3);
        assert_eq!(delta.late_packets, 1);
        assert_eq!(delta.playback_dry, 0);
        assert!(now.delta(&now).is_zero());
        // Reversed order saturates instead of wrapping.
        assert!(base.delta(&now).is_zero());
    }

    #[test]
    fn dry_and_accelerate_alone_are_not_glitches() {
        let s = DiagnosticsSnapshot {
            playback_dry: 5,
            accelerate_drops: 2,
            ..Default::default()
        };
        assert!(!s.is_zero());
        assert!(!s.has_glitches());
        for glitch in [
            DiagnosticsSnapshot {
                late_packets: 1,
                ..s
            },
            DiagnosticsSnapshot {
                playback_trimmed_samples: 960,
                ..s
            },
            DiagnosticsSnapshot {
                expand_frames: 1,
                ..s
            },
            DiagnosticsSnapshot {
                output_starved: 1,
                ..s
            },
        ] {
            assert!(glitch.has_glitches(), "{glitch:?}");
        }
    }
}
