//! Voice actor — runs on a dedicated OS thread because `VoiceHandle` (and the
//! underlying `cpal::Stream`s it owns) is `!Send`. The thread spins up its own
//! `current_thread` Tokio runtime so we can still call the async
//! `voice::start_voice`. Other parts of the app talk to the actor only via the
//! Send-safe `VoiceManager` handle.

use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::Arc;

use anyhow::Result;
use tc_client::voice::{self, VoiceHandle};
use tc_shared::ChannelId;
use tokio::sync::{mpsc, oneshot};

pub struct StartParams {
    /// Resolved IP of the live TCP connection — the UDP voice target.
    pub server_ip: std::net::IpAddr,
    pub channel_id: ChannelId,
    pub udp_token: u64,
    pub voice_key: Vec<u8>,
    pub muted: Arc<AtomicBool>,
    pub vad_threshold: Arc<AtomicU32>,
    pub input_gain: Arc<AtomicU32>,
    pub output_vol: Arc<AtomicU32>,
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub sender_name: String,
}

#[derive(Clone, Debug)]
pub struct VoiceSnapshot {
    pub active: bool,
    pub loss_percent: u8,
    pub tier: String,
    /// `(name, level)` for each speaker active in the last 300ms.
    pub speaker_levels: Vec<(String, f32)>,
    pub tx_rate: f64,
    pub rx_rate: f64,
    /// 0.0–1.0 RMS of the most recent input frame.
    pub input_peak: f32,
    /// Server confirmed our UDP registration; `false` = sending but deaf.
    pub registered: bool,
}

impl VoiceSnapshot {
    pub fn idle() -> Self {
        Self {
            active: false,
            loss_percent: 0,
            tier: "—".into(),
            speaker_levels: Vec::new(),
            tx_rate: 0.0,
            rx_rate: 0.0,
            input_peak: 0.0,
            registered: true,
        }
    }
}

enum Cmd {
    Start(StartParams, oneshot::Sender<Result<(), String>>),
    Stop,
    Snapshot(oneshot::Sender<VoiceSnapshot>),
}

#[derive(Clone)]
pub struct VoiceManager {
    tx: mpsc::Sender<Cmd>,
}

impl VoiceManager {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel::<Cmd>(16);
        std::thread::Builder::new()
            .name("voice-actor".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("voice-actor runtime");
                rt.block_on(actor_loop(rx));
            })
            .expect("spawn voice-actor thread");
        Self { tx }
    }

    pub async fn start(&self, params: StartParams) -> Result<(), String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Cmd::Start(params, reply_tx))
            .await
            .map_err(|_| "voice actor gone".to_string())?;
        reply_rx
            .await
            .map_err(|_| "voice actor dropped reply".to_string())?
    }

    pub async fn stop(&self) {
        let _ = self.tx.send(Cmd::Stop).await;
    }

    pub async fn snapshot(&self) -> VoiceSnapshot {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self.tx.send(Cmd::Snapshot(reply_tx)).await.is_err() {
            return VoiceSnapshot::idle();
        }
        reply_rx.await.unwrap_or_else(|_| VoiceSnapshot::idle())
    }
}

async fn actor_loop(mut rx: mpsc::Receiver<Cmd>) {
    let mut handle: Option<VoiceHandle> = None;
    while let Some(cmd) = rx.recv().await {
        match cmd {
            Cmd::Start(p, reply) => {
                // Drop any prior handle first.
                handle = None;
                let res = voice::start_voice(
                    p.server_ip,
                    p.channel_id,
                    p.udp_token,
                    p.muted,
                    p.voice_key,
                    p.input_device,
                    p.output_device,
                    p.vad_threshold,
                    p.input_gain,
                    p.output_vol,
                    p.sender_name,
                )
                .await;
                match res {
                    Ok(h) => {
                        handle = Some(h);
                        let _ = reply.send(Ok(()));
                    }
                    Err(e) => {
                        let _ = reply.send(Err(format!("{:#}", e)));
                    }
                }
            }
            Cmd::Stop => {
                handle = None;
            }
            Cmd::Snapshot(reply) => {
                let snap = match handle.as_ref() {
                    Some(h) => {
                        let (loss, tier) = h.quality_info();
                        let (tx_rate, rx_rate, _bytes_total) = h.traffic_info();
                        VoiceSnapshot {
                            active: true,
                            loss_percent: loss,
                            tier: tier.to_string(),
                            speaker_levels: h.speaker_levels(),
                            tx_rate,
                            rx_rate,
                            input_peak: h.input_peak(),
                            registered: h.is_registered(),
                        }
                    }
                    None => VoiceSnapshot::idle(),
                };
                let _ = reply.send(snap);
            }
        }
    }
}
