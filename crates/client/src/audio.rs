use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::Stream;

use tc_shared::config;

/// Device stream parameters resolved at runtime.
struct DeviceStreamParams {
    stream_config: cpal::StreamConfig,
    channels: u16,
    sample_rate: u32,
}

/// Resolve stream parameters for the given device.
/// On Windows (WASAPI shared mode) the device rejects anything that doesn't
/// match its mixer format, so we use the full default config.
/// On other platforms we use the fixed 48 kHz / mono / 960-sample config.
fn resolve_stream_params(device: &cpal::Device, is_input: bool) -> Result<DeviceStreamParams> {
    #[cfg(target_os = "windows")]
    {
        let default_cfg = if is_input {
            device.default_input_config()
        } else {
            device.default_output_config()
        }
        .context("failed to get default stream config")?;

        let channels = default_cfg.channels();
        let sample_rate = default_cfg.sample_rate().0;
        let stream_config = default_cfg.config(); // guaranteed-supported config

        Ok(DeviceStreamParams {
            stream_config,
            channels,
            sample_rate,
        })
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (device, is_input);
        Ok(DeviceStreamParams {
            stream_config: cpal::StreamConfig {
                channels: config::AUDIO_CHANNELS,
                sample_rate: cpal::SampleRate(config::SAMPLE_RATE),
                buffer_size: cpal::BufferSize::Fixed(config::FRAME_SIZE as u32),
            },
            channels: config::AUDIO_CHANNELS,
            sample_rate: config::SAMPLE_RATE,
        })
    }
}

/// Linear-interpolation resampler (good enough for voice).
fn resample(input: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from_rate as f64 / to_rate as f64;
    let out_len = (input.len() as f64 / ratio).ceil() as usize;
    let mut output = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src = i as f64 * ratio;
        let idx = src as usize;
        let frac = src - idx as f64;
        let s = if idx + 1 < input.len() {
            input[idx] as f64 * (1.0 - frac) + input[idx + 1] as f64 * frac
        } else if idx < input.len() {
            input[idx] as f64
        } else {
            0.0
        };
        output.push(s as f32);
    }
    output
}

pub struct DeviceInfo {
    pub index: usize,
    pub name: String,
    pub is_default: bool,
}

pub fn list_input_devices() -> Result<Vec<DeviceInfo>> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_default();
    let devices: Vec<DeviceInfo> = host
        .input_devices()
        .context("failed to enumerate input devices")?
        .enumerate()
        .filter_map(|(i, d)| {
            d.name().ok().map(|name| DeviceInfo {
                index: i,
                is_default: name == default_name,
                name,
            })
        })
        .collect();
    Ok(devices)
}

pub fn list_output_devices() -> Result<Vec<DeviceInfo>> {
    let host = cpal::default_host();
    let default_name = host
        .default_output_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_default();
    let devices: Vec<DeviceInfo> = host
        .output_devices()
        .context("failed to enumerate output devices")?
        .enumerate()
        .filter_map(|(i, d)| {
            d.name().ok().map(|name| DeviceInfo {
                index: i,
                is_default: name == default_name,
                name,
            })
        })
        .collect();
    Ok(devices)
}

/// Start capturing audio from an input device.
/// If `device_name` is provided, uses that device; otherwise uses the system default.
/// Returns a stream handle (must be kept alive) and a receiver of PCM frames.
pub fn start_capture(device_name: Option<&str>) -> Result<(Stream, std_mpsc::Receiver<Vec<f32>>)> {
    let host = cpal::default_host();
    let device = if let Some(name) = device_name {
        host.input_devices()
            .context("failed to enumerate input devices")?
            .find(|d| d.name().ok().as_deref() == Some(name))
            .with_context(|| format!("input device '{}' not found", name))?
    } else {
        host.default_input_device()
            .context("no input device available")?
    };

    tracing::info!("input device: {}", device.name().unwrap_or_default());

    let params = resolve_stream_params(&device, true)?;
    tracing::info!(
        "input: {}ch {}Hz (need {}ch {}Hz)",
        params.channels, params.sample_rate,
        config::AUDIO_CHANNELS, config::SAMPLE_RATE
    );

    let device_channels = params.channels;
    let device_rate = params.sample_rate;
    let need_resample = device_rate != config::SAMPLE_RATE;

    // How many mono samples at device_rate correspond to one 20ms Opus frame
    let device_frame_size = (device_rate as f64 * config::FRAME_SIZE as f64
        / config::SAMPLE_RATE as f64)
        .round() as usize;

    let (tx, rx) = std_mpsc::channel::<Vec<f32>>();

    // Accumulate mono samples at device sample rate
    let mut accumulator = Vec::with_capacity(device_frame_size * 2);

    let stream = device.build_input_stream(
        &params.stream_config,
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            // 1. Downmix to mono (if needed)
            if device_channels > 1 {
                let ch = device_channels as usize;
                for chunk in data.chunks_exact(ch) {
                    let mono: f32 = chunk.iter().sum::<f32>() / ch as f32;
                    accumulator.push(mono);
                }
            } else {
                accumulator.extend_from_slice(data);
            }

            // 2. Drain in device-frame-sized chunks, resample to 48 kHz, send
            while accumulator.len() >= device_frame_size {
                let chunk: Vec<f32> = accumulator.drain(..device_frame_size).collect();
                let frame = if need_resample {
                    resample(&chunk, device_rate, config::SAMPLE_RATE)
                } else {
                    chunk
                };
                // Opus needs exactly FRAME_SIZE samples
                let mut opus_frame = vec![0.0f32; config::FRAME_SIZE];
                let n = frame.len().min(config::FRAME_SIZE);
                opus_frame[..n].copy_from_slice(&frame[..n]);
                let _ = tx.send(opus_frame);
            }
        },
        |err| {
            tracing::error!("input stream error: {}", err);
        },
        None,
    )?;

    stream.play()?;
    Ok((stream, rx))
}

/// Start playing audio on an output device.
/// If `device_name` is provided, uses that device; otherwise uses the system default.
/// Returns a stream handle (must be kept alive) and a sender to feed PCM frames.
pub fn start_playback(device_name: Option<&str>, playback_cap: Arc<AtomicU32>) -> Result<(Stream, std_mpsc::Sender<Vec<f32>>)> {
    let host = cpal::default_host();
    let device = if let Some(name) = device_name {
        host.output_devices()
            .context("failed to enumerate output devices")?
            .find(|d| d.name().ok().as_deref() == Some(name))
            .with_context(|| format!("output device '{}' not found", name))?
    } else {
        host.default_output_device()
            .context("no output device available")?
    };

    tracing::info!("output device: {}", device.name().unwrap_or_default());

    let params = resolve_stream_params(&device, false)?;
    tracing::info!(
        "output: {}ch {}Hz (need {}ch {}Hz)",
        params.channels, params.sample_rate,
        config::AUDIO_CHANNELS, config::SAMPLE_RATE
    );

    let device_channels = params.channels;
    let device_rate = params.sample_rate;
    let need_resample = device_rate != config::SAMPLE_RATE;

    let (tx, rx) = std_mpsc::channel::<Vec<f32>>();

    // Buffer holds mono samples at device sample rate
    let mut playback_buf: Vec<f32> = Vec::with_capacity(config::MAX_PLAYBACK_BUF);

    let stream = device.build_output_stream(
        &params.stream_config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            // Drain received 48 kHz mono frames, resample to device rate
            while let Ok(frame) = rx.try_recv() {
                if need_resample {
                    let resampled = resample(&frame, config::SAMPLE_RATE, device_rate);
                    playback_buf.extend_from_slice(&resampled);
                } else {
                    playback_buf.extend_from_slice(&frame);
                }
            }

            // Cap latency (scale cap from 48 kHz to device rate)
            let cap48 = playback_cap.load(Ordering::Relaxed) as usize;
            let cap = if need_resample {
                (cap48 as f64 * device_rate as f64 / config::SAMPLE_RATE as f64) as usize
            } else {
                cap48
            };
            if playback_buf.len() > cap {
                let excess = playback_buf.len() - cap;
                playback_buf.drain(..excess);
            }

            // Fill output buffer
            if device_channels > 1 {
                // Upmix mono to interleaved multi-channel
                let ch = device_channels as usize;
                let mono_needed = data.len() / ch;
                let available = playback_buf.len().min(mono_needed);
                for i in 0..available {
                    let sample = playback_buf[i];
                    for c in 0..ch {
                        data[i * ch + c] = sample;
                    }
                }
                playback_buf.drain(..available);
                for sample in &mut data[available * ch..] {
                    *sample = 0.0;
                }
            } else {
                let available = playback_buf.len().min(data.len());
                data[..available].copy_from_slice(&playback_buf[..available]);
                playback_buf.drain(..available);
                for sample in &mut data[available..] {
                    *sample = 0.0;
                }
            }
        },
        |err| {
            tracing::error!("output stream error: {}", err);
        },
        None,
    )?;

    stream.play()?;
    Ok((stream, tx))
}
