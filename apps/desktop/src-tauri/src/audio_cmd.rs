//! Audio-device commands and the test-signal player.

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::State;
use tc_client::audio;
use tokio::sync::Mutex;

use crate::state::AppCore;

type CoreState<'a> = State<'a, Arc<Mutex<AppCore>>>;

#[derive(Serialize)]
pub struct DeviceInfo {
    pub index: usize,
    pub name: String,
}

impl From<audio::DeviceInfo> for DeviceInfo {
    fn from(d: audio::DeviceInfo) -> Self {
        Self {
            index: d.index,
            name: d.name,
        }
    }
}

#[tauri::command]
pub async fn list_input_devices() -> Result<Vec<DeviceInfo>, String> {
    audio::list_input_devices()
        .map(|v| v.into_iter().map(Into::into).collect())
        .map_err(|e| format!("{:#}", e))
}

#[tauri::command]
pub async fn list_output_devices() -> Result<Vec<DeviceInfo>, String> {
    audio::list_output_devices()
        .map(|v| v.into_iter().map(Into::into).collect())
        .map_err(|e| format!("{:#}", e))
}

#[tauri::command]
pub async fn set_input_device(state: CoreState<'_>, name: Option<String>) -> Result<(), String> {
    let mut c = state.lock().await;
    c.input_device = name.clone();
    c.save();
    // Apply mid-call immediately (no-op when not in a call).
    c.voice.set_input_device(name).await;
    Ok(())
}

#[tauri::command]
pub async fn set_output_device(state: CoreState<'_>, name: Option<String>) -> Result<(), String> {
    let mut c = state.lock().await;
    c.output_device = name.clone();
    c.save();
    c.voice.set_output_device(name).await;
    Ok(())
}

#[tauri::command]
pub async fn set_input_gain(state: CoreState<'_>, pct: u32) -> Result<(), String> {
    let mut c = state.lock().await;
    c.apply_input_gain(pct);
    c.save();
    Ok(())
}

#[tauri::command]
pub async fn set_output_volume(state: CoreState<'_>, pct: u32) -> Result<(), String> {
    let mut c = state.lock().await;
    c.apply_output_vol(pct);
    c.save();
    Ok(())
}

#[tauri::command]
pub async fn set_vad_level(state: CoreState<'_>, pct: u32) -> Result<(), String> {
    let mut c = state.lock().await;
    c.apply_vad_level(pct);
    c.save();
    Ok(())
}

/// Play a 440 Hz sine for `duration_ms` through the currently selected output device.
/// Runs on a dedicated OS thread so the !Send `cpal::Stream` stays put; the thread
/// exits when the duration expires.
#[tauri::command]
pub async fn play_test_signal(
    state: CoreState<'_>,
    duration_ms: Option<u64>,
) -> Result<(), String> {
    let device_name = state.lock().await.output_device.clone();
    let duration = Duration::from_millis(duration_ms.unwrap_or(500));

    std::thread::Builder::new()
        .name("audio-test".into())
        .spawn(move || {
            if let Err(e) = play_sine(device_name.as_deref(), duration) {
                tracing::warn!("test signal failed: {:#}", e);
            }
        })
        .map_err(|e| format!("spawn failed: {}", e))?;

    Ok(())
}

fn play_sine(device_name: Option<&str>, duration: Duration) -> anyhow::Result<()> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    // Same fallback semantics as the voice pipeline: a stale saved name
    // degrades to the system default instead of failing silently.
    let device = device_name
        .and_then(|name| {
            host.output_devices()
                .ok()?
                .find(|d| d.name().map(|n| n == name).unwrap_or(false))
        })
        .or_else(|| host.default_output_device())
        .ok_or_else(|| anyhow::anyhow!("no output device available"))?;

    let config = device.default_output_config()?;
    let sample_rate = config.sample_rate().0 as f32;
    let channels = config.channels() as usize;
    let mut phase = 0.0f32;
    let two_pi = std::f32::consts::TAU;
    let freq = 440.0_f32;
    let amp = 0.15_f32;

    // Hard stop inside the callback: after `duration` worth of frames the tone
    // generator yields pure silence, so the beep ends on time even if the
    // stream drop below ever stalls (CoreAudio dispose can wedge).
    let max_frames = (sample_rate * duration.as_secs_f32()) as u64;
    let mut frames_done: u64 = 0;
    let mut next_sample = move || -> f32 {
        if frames_done >= max_frames {
            return 0.0;
        }
        frames_done += 1;
        let s = (phase * two_pi).sin() * amp;
        phase = (phase + freq / sample_rate) % 1.0;
        s
    };

    // Branch on sample format (cpal exposes I16, U16, F32, etc.).
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_output_stream(
            &config.into(),
            move |data: &mut [f32], _| {
                for frame in data.chunks_mut(channels) {
                    let s = next_sample();
                    for ch in frame {
                        *ch = s;
                    }
                }
            },
            |e| tracing::warn!("test stream error: {}", e),
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_output_stream(
            &config.into(),
            move |data: &mut [i16], _| {
                for frame in data.chunks_mut(channels) {
                    let s = (next_sample() * i16::MAX as f32) as i16;
                    for ch in frame {
                        *ch = s;
                    }
                }
            },
            |e| tracing::warn!("test stream error: {}", e),
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_output_stream(
            &config.into(),
            move |data: &mut [u16], _| {
                for frame in data.chunks_mut(channels) {
                    let s = (next_sample() * i16::MAX as f32) as i16;
                    let u = (s as i32 + i16::MAX as i32 + 1) as u16;
                    for ch in frame {
                        *ch = u;
                    }
                }
            },
            |e| tracing::warn!("test stream error: {}", e),
            None,
        )?,
        fmt => anyhow::bail!("unsupported sample format: {:?}", fmt),
    };

    stream.play()?;
    // Small pad so the device drains the final frames; the generator above is
    // already silent past `duration`.
    std::thread::sleep(duration + Duration::from_millis(150));
    drop(stream);
    Ok(())
}
