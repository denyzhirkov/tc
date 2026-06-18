//! Per-peer local playback gain, keyed by sender display name.
//!
//! The voice receive path mixes every sender's decoded PCM into one buffer
//! (`voice::drain_streams_to_playback`). This lets the *listener* scale an
//! individual sender up or down for themselves only — never sent to the relay
//! or other peers. The relay can't do this anyway: it holds no key and forwards
//! ciphertext.
//!
//! Reads happen every drain round on the hot path; writes only when the user
//! drags a slider. So the store is an `ArcSwap<HashMap>`: lock-free reads,
//! copy-on-write swaps — the same pattern the relay uses for its peer cache.
//!
//! Identity on the channel/voice wire is the **name** (there is no pubkey in a
//! voice packet, and the roster is names-only), so the runtime key is the name.
//! A `NameChanged` is followed live via [`PeerGains::rename`] so a peer keeps
//! their adjusted volume across a rename.

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use tc_shared::config;

/// Maximum gain factor a peer can be boosted to (200% → 2.0×).
fn max_gain() -> f32 {
    config::PEER_VOL_MAX_PCT as f32 / 100.0
}

/// A gain within `f32::EPSILON` of 1.0 is "unchanged" — stored as an absent key
/// so the hot path skips the multiply entirely.
fn is_unity(gain: f32) -> bool {
    (gain - 1.0).abs() < f32::EPSILON
}

/// Shared, lock-free map of sender-name → playback gain factor.
#[derive(Clone)]
pub struct PeerGains {
    map: Arc<ArcSwap<HashMap<String, f32>>>,
}

impl Default for PeerGains {
    fn default() -> Self {
        Self::new()
    }
}

impl PeerGains {
    pub fn new() -> Self {
        Self {
            map: Arc::new(ArcSwap::from_pointee(HashMap::new())),
        }
    }

    /// Build from persisted `name → percent` (100 = unchanged). Out-of-range
    /// percents are clamped; unity entries are dropped.
    pub fn from_pct_map(pcts: &HashMap<String, u32>) -> Self {
        let g = Self::new();
        g.set_all_from_pct(pcts);
        g
    }

    /// Replace every entry from a persisted `name → percent` map, keeping the
    /// shared handle so any live call picks up the change. Used by settings import.
    pub fn set_all_from_pct(&self, pcts: &HashMap<String, u32>) {
        let mut m = HashMap::new();
        for (name, pct) in pcts {
            let gain = pct_to_gain(*pct);
            if !is_unity(gain) {
                m.insert(name.clone(), gain);
            }
        }
        self.map.store(Arc::new(m));
    }

    /// Gain factor for `name` (1.0 = unchanged). Hot-path read — no allocation.
    pub fn get(&self, name: &str) -> f32 {
        self.map.load().get(name).copied().unwrap_or(1.0)
    }

    /// Set one sender's gain from a percent (copy-on-write swap). 100 removes
    /// the entry (back to unchanged).
    pub fn set_pct(&self, name: &str, pct: u32) {
        let gain = pct_to_gain(pct);
        let mut next = HashMap::clone(&self.map.load());
        if is_unity(gain) {
            next.remove(name);
        } else {
            next.insert(name.to_string(), gain);
        }
        self.map.store(Arc::new(next));
    }

    /// Move a peer's gain from `old` to `new` on a `NameChanged`.
    pub fn rename(&self, old: &str, new: &str) {
        let cur = self.map.load();
        let Some(gain) = cur.get(old).copied() else {
            return;
        };
        let mut next = HashMap::clone(&cur);
        next.remove(old);
        next.insert(new.to_string(), gain);
        self.map.store(Arc::new(next));
    }

    /// Snapshot as `name → percent`, for persistence and the UI. Only non-unity
    /// entries are present.
    pub fn pct_map(&self) -> HashMap<String, u32> {
        self.map
            .load()
            .iter()
            .map(|(name, gain)| (name.clone(), gain_to_pct(*gain)))
            .collect()
    }
}

fn pct_to_gain(pct: u32) -> f32 {
    (pct.min(config::PEER_VOL_MAX_PCT) as f32 / 100.0).clamp(0.0, max_gain())
}

fn gain_to_pct(gain: f32) -> u32 {
    (gain * 100.0).round() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unity_is_absent_and_reads_default() {
        let g = PeerGains::new();
        assert_eq!(g.get("alice"), 1.0);
        g.set_pct("alice", 100);
        assert!(g.pct_map().is_empty(), "100% should not be stored");
        assert_eq!(g.get("alice"), 1.0);
    }

    #[test]
    fn set_get_and_clamp() {
        let g = PeerGains::new();
        g.set_pct("bob", 50);
        assert_eq!(g.get("bob"), 0.5);
        g.set_pct("bob", 200);
        assert_eq!(g.get("bob"), 2.0);
        // Above max clamps to 200%.
        g.set_pct("bob", 999);
        assert_eq!(g.get("bob"), 2.0);
    }

    #[test]
    fn rename_preserves_gain() {
        let g = PeerGains::new();
        g.set_pct("old", 150);
        g.rename("old", "new");
        assert_eq!(g.get("new"), 1.5);
        assert_eq!(g.get("old"), 1.0);
    }

    #[test]
    fn roundtrip_pct_map() {
        let mut pcts = HashMap::new();
        pcts.insert("a".to_string(), 50u32);
        pcts.insert("b".to_string(), 100u32); // unity dropped
        let g = PeerGains::from_pct_map(&pcts);
        let back = g.pct_map();
        assert_eq!(back.get("a"), Some(&50));
        assert_eq!(back.get("b"), None);
    }
}
