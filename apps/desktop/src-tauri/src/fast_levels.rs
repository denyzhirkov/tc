//! Fast level lane: pushes `voice_levels` (own mic + every peer, per-frame
//! loudness) at `FAST_LEVEL_TICK_MS` for lip sync.
//!
//! Reads only the call's `LiveLevels` handle, which `level_pump` re-points on
//! every call start, pipeline restart and call end — never `AppCore` or the
//! voice actor. While nobody is audible it goes quiet after one final
//! all-zero frame, so an idle call costs no IPC.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tauri::AppHandle;
use tc_client::level::LiveLevels;
use tc_shared::config;
use tokio::sync::watch;

use crate::events::{emit, PeerLevels, VoiceLevelsPayload};

/// When to emit, as a pure function of what the lane sees each tick.
///
/// Starts quiet. Any audible level opens it; while open, every tick with new
/// data is emitted (a pause between words included). Once everything has been
/// at zero for `silence`, exactly one more frame goes out (all zeros, so every
/// mouth closes) and the lane is quiet again until the next audible level.
struct QuietGate {
    silence: Duration,
    last_audible: Option<Instant>,
}

impl QuietGate {
    fn new(silence: Duration) -> Self {
        Self {
            silence,
            last_audible: None,
        }
    }

    /// `audible`: any level above 0 right now; `changed`: the data moved since
    /// the previous tick. Returns whether to emit this tick.
    fn tick(&mut self, audible: bool, changed: bool, now: Instant) -> bool {
        if audible {
            self.last_audible = Some(now);
            return changed;
        }
        match self.last_audible {
            None => false,
            Some(at) if now.saturating_duration_since(at) >= self.silence => {
                self.last_audible = None;
                true
            }
            Some(_) => changed,
        }
    }

    /// Whether the lane is open (the frontend may be showing non-zero levels).
    fn open(&self) -> bool {
        self.last_audible.is_some()
    }
}

pub fn spawn(app: AppHandle, mut live_rx: watch::Receiver<Option<Arc<LiveLevels>>>) {
    tauri::async_runtime::spawn(async move {
        let tick_every = Duration::from_millis(config::FAST_LEVEL_TICK_MS);
        let silence = Duration::from_millis(config::FAST_LEVEL_SILENCE_MS);
        let mut peers: Vec<(String, f32)> = Vec::with_capacity(config::MAX_SENDER_STREAMS);
        loop {
            let live = live_rx.borrow_and_update().clone();
            let Some(live) = live else {
                // Not in a call: sleep until one starts.
                if live_rx.changed().await.is_err() {
                    return;
                }
                continue;
            };
            let mut gate = QuietGate::new(silence);
            let mut seen = live.version();
            let mut me = 0.0f32;
            peers.clear();
            let mut tick = tokio::time::interval(tick_every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = tick.tick() => {}
                    changed = live_rx.changed() => {
                        if gate.open() {
                            // The call ended or was rebuilt mid-phrase.
                            emit_zero(&app);
                        }
                        if changed.is_err() {
                            return;
                        }
                        break;
                    }
                }
                let version = live.version();
                let changed = version != seen;
                if changed {
                    (me, seen) = live.snapshot(&mut peers);
                } else if !gate.open() {
                    // Quiet and nothing moved: no lock, no copy.
                    continue;
                }
                let audible = me > 0.0 || peers.iter().any(|(_, l)| *l > 0.0);
                if gate.tick(audible, changed, Instant::now()) {
                    emit(
                        &app,
                        "voice_levels",
                        VoiceLevelsPayload {
                            me,
                            peers: PeerLevels(&peers),
                        },
                    );
                }
            }
        }
    });
}

fn emit_zero(app: &AppHandle) {
    emit(
        app,
        "voice_levels",
        VoiceLevelsPayload {
            me: 0.0,
            peers: PeerLevels(&[]),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const SILENCE: Duration = Duration::from_millis(300);
    const TICK: Duration = Duration::from_millis(33);

    /// Drive the gate with one `(audible, changed)` per tick; return the ticks
    /// that emitted.
    fn run(ticks: &[(bool, bool)]) -> Vec<usize> {
        let t0 = Instant::now();
        let mut gate = QuietGate::new(SILENCE);
        ticks
            .iter()
            .enumerate()
            .filter(|(i, (audible, changed))| gate.tick(*audible, *changed, t0 + TICK * *i as u32))
            .map(|(i, _)| i)
            .collect()
    }

    #[test]
    fn a_silent_call_never_emits() {
        assert!(run(&[(false, false); 50]).is_empty());
        // Data moving at zero (a new peer joins the set) is still silence.
        assert!(run(&[(false, true); 50]).is_empty());
    }

    #[test]
    fn emits_while_audible_then_one_trailing_frame_then_quiet() {
        let mut ticks = vec![(true, true); 10];
        ticks.extend([(false, true); 30]);
        let emitted = run(&ticks);
        // Last audible at tick 9 (297 ms); 300 ms later is tick 19 (627 ms).
        let expect: Vec<usize> = (0..=19).collect();
        assert_eq!(emitted, expect);
    }

    #[test]
    fn the_trailing_frame_goes_out_even_when_nothing_moved() {
        let mut ticks = vec![(true, true); 3];
        // The levels went to zero and stayed there: no new versions at all.
        ticks.extend([(false, false); 20]);
        // Last audible at 66 ms; the trailing frame at tick 12 (396 ms).
        assert_eq!(run(&ticks), vec![0, 1, 2, 12]);
    }

    #[test]
    fn a_pause_shorter_than_the_silence_keeps_the_lane_open() {
        let mut ticks = vec![(true, true); 5];
        ticks.extend([(false, true); 5]); // 165 ms pause
        ticks.extend([(true, true); 5]);
        ticks.extend([(false, true); 15]);
        let emitted = run(&ticks);
        // Last audible at tick 14 (462 ms); trailing frame at tick 24 (792 ms).
        let expect: Vec<usize> = (0..=24).collect();
        assert_eq!(emitted, expect);
    }

    #[test]
    fn an_audible_level_reopens_a_quiet_lane() {
        let mut ticks = vec![(true, true)];
        ticks.extend([(false, false); 15]);
        ticks.push((true, true));
        let emitted = run(&ticks);
        assert_eq!(emitted, vec![0, 10, 16]);
    }

    /// The wire shape the frontend's `VoiceLevelsPayload` type declares.
    #[test]
    fn payload_serializes_as_me_and_named_peers() {
        let peers = vec![("alice".to_string(), 0.5f32), ("bob".to_string(), 0.0)];
        let json = serde_json::to_value(VoiceLevelsPayload {
            me: 0.25,
            peers: PeerLevels(&peers),
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "me": 0.25,
                "peers": [{ "name": "alice", "level": 0.5 }, { "name": "bob", "level": 0.0 }]
            })
        );
    }

    #[test]
    fn unchanged_audible_data_is_not_re_emitted() {
        assert_eq!(
            run(&[(true, true), (true, false), (true, true)]),
            vec![0, 2]
        );
    }
}
