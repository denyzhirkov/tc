//! Backing for the `/show_dev_logs` command: a `tracing` Layer that forwards
//! DEBUG-and-above events from the tc crates to the frontend as `dev_log`
//! Tauri events while the toggle is on. Off by default, not persisted.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

static ENABLED: AtomicBool = AtomicBool::new(false);
static APP: OnceLock<AppHandle> = OnceLock::new();

/// Max events forwarded per window; the excess is counted and reported as one line.
const THROTTLE_WINDOW_MS: u128 = 1000;
const MAX_EVENTS_PER_WINDOW: u32 = 25;

#[derive(Debug, Clone, Serialize)]
pub struct DevLogPayload {
    pub level: String,
    pub target: String,
    pub message: String,
    pub ts_ms: u64,
}

/// Called once from Tauri setup so the layer can emit events.
pub fn set_app(handle: AppHandle) {
    let _ = APP.set(handle);
}

pub fn set_enabled(on: bool) -> bool {
    ENABLED.store(on, Ordering::Relaxed);
    on
}

#[tauri::command]
pub fn set_dev_logs(enabled: bool) -> bool {
    set_enabled(enabled)
}

struct Throttle {
    window_start: Instant,
    sent: u32,
    dropped: u64,
}

fn throttle() -> &'static Mutex<Throttle> {
    static T: OnceLock<Mutex<Throttle>> = OnceLock::new();
    T.get_or_init(|| {
        Mutex::new(Throttle {
            window_start: Instant::now(),
            sent: 0,
            dropped: 0,
        })
    })
}

/// Returns `Some(dropped_in_last_window)` if this event may be sent.
fn throttle_admit() -> Option<u64> {
    let mut t = throttle().lock().ok()?;
    if t.window_start.elapsed().as_millis() >= THROTTLE_WINDOW_MS {
        let dropped = t.dropped;
        t.window_start = Instant::now();
        t.sent = 0;
        t.dropped = 0;
        if dropped > 0 {
            // Report the previous window's drops alongside this event.
            t.sent = 1;
            return Some(dropped);
        }
    }
    if t.sent >= MAX_EVENTS_PER_WINDOW {
        t.dropped += 1;
        return None;
    }
    t.sent += 1;
    Some(0)
}

struct MsgVisitor {
    msg: String,
}

impl Visit for MsgVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            if !self.msg.is_empty() {
                self.msg.insert(0, ' ');
                let mut head = String::new();
                let _ = write!(head, "{:?}", value);
                self.msg.insert_str(0, &head);
            } else {
                let _ = write!(self.msg, "{:?}", value);
            }
        } else {
            let _ = write!(self.msg, " {}={:?}", field.name(), value);
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub struct DevLogLayer;

impl<S: Subscriber> Layer<S> for DevLogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if !ENABLED.load(Ordering::Relaxed) {
            return;
        }
        let meta = event.metadata();
        // DEBUG and above; TRACE is per-packet noise. Only our own crates —
        // GUI/runtime deps are not what the user is debugging.
        if *meta.level() > tracing::Level::DEBUG || !meta.target().starts_with("tc") {
            return;
        }
        let Some(app) = APP.get() else { return };
        let Some(dropped) = throttle_admit() else {
            return;
        };

        if dropped > 0 {
            let _ = app.emit(
                "dev_log",
                DevLogPayload {
                    level: "WARN".into(),
                    target: "dev_log".into(),
                    message: format!("…{} dev log lines dropped (rate limit)", dropped),
                    ts_ms: now_ms(),
                },
            );
        }

        let mut v = MsgVisitor { msg: String::new() };
        event.record(&mut v);
        let _ = app.emit(
            "dev_log",
            DevLogPayload {
                level: meta.level().to_string(),
                target: meta.target().to_string(),
                message: v.msg,
                ts_ms: now_ms(),
            },
        );
    }
}
