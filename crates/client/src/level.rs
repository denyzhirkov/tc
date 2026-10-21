//! Speaker loudness for the UIs: the dBFS → 0..1 mapping every meter uses, and
//! [`LiveLevels`], the per-frame lane a desktop animates mouths from.
//!
//! Peer levels come from the playout engine — the RMS of each sender's frame
//! as rendered, before the listener's per-peer gain — so they are real
//! loudness, not a packet-size guess. A frame is rendered about one output
//! low-water mark (~40 ms) before it is heard; for a meter or a mouth that
//! lead is invisible.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

use tc_shared::config;

/// Perceptual loudness 0..1 of a frame with RMS `rms` (full scale = 1.0):
/// linear in dBFS from `LEVEL_FLOOR_DBFS` (0) to `LEVEL_CEIL_DBFS` (1),
/// clamped. Silence, negative and non-finite input read as 0.
pub fn loudness(rms: f32) -> f32 {
    if !rms.is_finite() || rms <= 0.0 {
        return 0.0;
    }
    let dbfs = 20.0 * rms.log10();
    ((dbfs - config::LEVEL_FLOOR_DBFS) / (config::LEVEL_CEIL_DBFS - config::LEVEL_FLOOR_DBFS))
        .clamp(0.0, 1.0)
}

/// RMS of one frame of samples.
pub(crate) fn rms(frame: &[f32]) -> f32 {
    (frame.iter().map(|s| s * s).sum::<f32>() / frame.len().max(1) as f32).sqrt()
}

#[derive(Debug)]
struct Peer {
    name: String,
    level: f32,
    seen: bool,
}

/// Live loudness of this call, updated every 20 ms frame: our own microphone
/// (written by the capture thread) and every sender the playout engine holds a
/// stream for (written by the receive task; an idle sender reads 0 until its
/// stream is pruned). Neither writer is an audio callback, so the peers'
/// short mutex never blocks a device.
///
/// `version` moves whenever any value changed, so a reader polling faster than
/// the data changes can skip the copy, and a call where nobody makes a sound
/// stops moving it at all.
#[derive(Debug)]
pub struct LiveLevels {
    own: AtomicU32,
    version: AtomicU64,
    peers: Mutex<Vec<Peer>>,
}

impl Default for LiveLevels {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveLevels {
    pub fn new() -> Self {
        Self {
            own: AtomicU32::new(0f32.to_bits()),
            version: AtomicU64::new(0),
            peers: Mutex::new(Vec::with_capacity(config::MAX_SENDER_STREAMS)),
        }
    }

    /// Current version without copying anything.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// Copy the peer levels into `out` (reusing its strings' capacity) and
    /// return our own microphone loudness and the version they belong to.
    /// The data may be newer than the version, never older: writers bump it
    /// after writing, the reader loads it first.
    pub fn snapshot(&self, out: &mut Vec<(String, f32)>) -> (f32, u64) {
        let version = self.version();
        let own = f32::from_bits(self.own.load(Ordering::Relaxed));
        let Ok(peers) = self.peers.lock() else {
            out.clear();
            return (own, version);
        };
        out.truncate(peers.len());
        for (i, p) in peers.iter().enumerate() {
            match out.get_mut(i) {
                Some(slot) => {
                    slot.0.clone_from(&p.name);
                    slot.1 = p.level;
                }
                None => out.push((p.name.clone(), p.level)),
            }
        }
        (own, version)
    }

    /// Own microphone loudness (0 while muted or without a device).
    pub(crate) fn set_own(&self, level: f32) {
        let bits = level.to_bits();
        if self.own.swap(bits, Ordering::Relaxed) != bits {
            self.version.fetch_add(1, Ordering::Release);
        }
    }

    /// Everything back to silence: the pipeline stopped, so nobody's last
    /// frame may stay frozen on screen.
    pub(crate) fn clear(&self) {
        self.set_own(0.0);
        self.publish_peers(std::iter::empty());
    }

    /// Replace the peer set with `levels` (every sender stream, idle ones at
    /// 0). A sender's name is allocated once, when it first appears; entries
    /// missing from `levels` (pruned streams) are removed.
    pub(crate) fn publish_peers<'a>(&self, levels: impl IntoIterator<Item = (&'a str, f32)>) {
        let Ok(mut peers) = self.peers.lock() else {
            return;
        };
        let mut changed = false;
        for p in peers.iter_mut() {
            p.seen = false;
        }
        for (name, level) in levels {
            match peers.iter_mut().find(|p| p.name == name) {
                Some(p) => {
                    changed |= p.level != level;
                    p.level = level;
                    p.seen = true;
                }
                None => {
                    changed = true;
                    peers.push(Peer {
                        name: name.to_string(),
                        level,
                        seen: true,
                    });
                }
            }
        }
        let before = peers.len();
        peers.retain(|p| p.seen);
        changed |= peers.len() != before;
        drop(peers);
        if changed {
            self.version.fetch_add(1, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_dbfs(db: f32) -> f32 {
        10f32.powf(db / 20.0)
    }

    #[test]
    fn loudness_maps_the_dbfs_window_onto_0_to_1() {
        assert_eq!(loudness(0.0), 0.0);
        assert!(loudness(from_dbfs(config::LEVEL_FLOOR_DBFS)).abs() < 1e-4);
        assert!((loudness(from_dbfs(config::LEVEL_CEIL_DBFS)) - 1.0).abs() < 1e-4);
        assert_eq!(loudness(from_dbfs(-80.0)), 0.0, "below the floor clamps");
        assert_eq!(loudness(1.0), 1.0, "full scale clamps");
        assert_eq!(loudness(4.0), 1.0, "an unclamped mix sample clamps");
        let agc = loudness(config::AGC_TARGET_RMS);
        assert!((0.7..=0.8).contains(&agc), "AGC target reads {agc}");
    }

    #[test]
    fn loudness_is_monotonic() {
        let mut prev = 0.0;
        for i in 1..=1000 {
            let l = loudness(i as f32 / 1000.0);
            assert!(l >= prev, "{} → {l} after {prev}", i as f32 / 1000.0);
            prev = l;
        }
    }

    #[test]
    fn loudness_is_safe_on_garbage() {
        for x in [-0.5, f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.0] {
            assert_eq!(loudness(x), 0.0, "{x}");
        }
        assert_eq!(loudness(f32::MIN_POSITIVE), 0.0);
    }

    #[test]
    fn snapshot_copies_peers_and_own_level() {
        let live = LiveLevels::new();
        let mut out = Vec::new();
        assert_eq!(live.snapshot(&mut out), (0.0, 0));
        assert!(out.is_empty());

        live.set_own(0.5);
        live.publish_peers([("alice", 0.25), ("bob", 0.0)]);
        let (own, v) = live.snapshot(&mut out);
        assert_eq!(own, 0.5);
        assert_eq!(v, 2);
        assert_eq!(out, vec![("alice".into(), 0.25), ("bob".into(), 0.0)]);

        // A pruned stream disappears; the buffer shrinks with it.
        live.publish_peers([("bob", 0.5)]);
        live.snapshot(&mut out);
        assert_eq!(out, vec![("bob".into(), 0.5)]);
    }

    #[test]
    fn version_moves_only_when_something_changed() {
        let live = LiveLevels::new();
        live.publish_peers([("alice", 0.0)]);
        let v = live.version();
        assert!(v > 0, "a new peer is a change");

        live.publish_peers([("alice", 0.0)]);
        live.set_own(0.0);
        assert_eq!(live.version(), v, "silence staying silent is not");

        live.set_own(0.3);
        let v2 = live.version();
        assert!(v2 > v);
        live.publish_peers([("alice", 0.4)]);
        let v3 = live.version();
        assert!(v3 > v2);
        live.publish_peers(std::iter::empty());
        assert!(live.version() > v3, "a removed peer is a change");
    }

    #[test]
    fn clear_zeroes_everything_and_moves_the_version() {
        let live = LiveLevels::new();
        let mut out = Vec::new();
        live.set_own(0.6);
        live.publish_peers([("alice", 0.4)]);
        let v = live.version();
        live.clear();
        assert_eq!(live.snapshot(&mut out).0, 0.0);
        assert!(out.is_empty());
        assert!(live.version() > v, "readers must see the change");
    }

    #[test]
    fn steady_publishing_does_not_allocate_names_again() {
        let live = LiveLevels::new();
        let mut out = Vec::new();
        live.publish_peers([("alice", 0.1)]);
        live.snapshot(&mut out);
        let name_ptr = out[0].0.as_ptr();
        let peers_ptr = live.peers.lock().unwrap()[0].name.as_ptr();
        for i in 0..100 {
            live.publish_peers([("alice", i as f32 / 100.0)]);
            live.snapshot(&mut out);
        }
        assert_eq!(out[0].0.as_ptr(), name_ptr, "reader string reused");
        assert_eq!(live.peers.lock().unwrap()[0].name.as_ptr(), peers_ptr);
    }
}
