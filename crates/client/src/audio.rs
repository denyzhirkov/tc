use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::Stream;

use tc_shared::config;

/// Build a StreamConfig compatible with the device.
/// On Windows (WASAPI), devices often reject mono or fixed buffer sizes,
/// so we query the device defaults and adapt.
/// Returns `(config, device_channels)`.
fn build_stream_config(device: &cpal::Device, is_input: bool) -> Result<(cpal::StreamConfig, u16)> {
    #[cfg(target_os = "windows")]
    {
        let default_cfg = if is_input {
            device.default_input_config()
        } else {
            device.default_output_config()
        }
        .context("failed to get default stream config")?;

        let device_channels = default_cfg.channels();
        let stream_config = cpal::StreamConfig {
            channels: device_channels,
            sample_rate: cpal::SampleRate(config::SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Default,
        };
        Ok((stream_config, device_channels))
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = (device, is_input);
        let stream_config = cpal::StreamConfig {
            channels: config::AUDIO_CHANNELS,
            sample_rate: cpal::SampleRate(config::SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Fixed(config::FRAME_SIZE as u32),
        };
        Ok((stream_config, config::AUDIO_CHANNELS))
    }
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

    let (stream_config, device_channels) = build_stream_config(&device, true)?;
    tracing::info!("input config: {:?}, device channels: {}", stream_config, device_channels);

    let (tx, rx) = std_mpsc::channel::<Vec<f32>>();

    // Accumulate samples until we have a full frame
    let mut accumulator = Vec::with_capacity(config::FRAME_SIZE * 2);

    let stream = device.build_input_stream(
        &stream_config,
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            if device_channels > 1 {
                // Downmix interleaved multi-channel to mono
                let ch = device_channels as usize;
                for chunk in data.chunks_exact(ch) {
                    let mono: f32 = chunk.iter().sum::<f32>() / ch as f32;
                    accumulator.push(mono);
                }
            } else {
                accumulator.extend_from_slice(data);
            }
            while accumulator.len() >= config::FRAME_SIZE {
                let frame: Vec<f32> = accumulator.drain(..config::FRAME_SIZE).collect();
                let _ = tx.send(frame);
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

    let (stream_config, device_channels) = build_stream_config(&device, false)?;
    tracing::info!("output config: {:?}, device channels: {}", stream_config, device_channels);

    let (tx, rx) = std_mpsc::channel::<Vec<f32>>();

    // Buffer for feeding the output stream
    let mut playback_buf: Vec<f32> = Vec::with_capacity(config::MAX_PLAYBACK_BUF);

    let stream = device.build_output_stream(
        &stream_config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            // Drain received frames into playback buffer
            while let Ok(frame) = rx.try_recv() {
                playback_buf.extend_from_slice(&frame);
            }

            // If we've accumulated too much, drop oldest to cap latency
            let cap = playback_cap.load(Ordering::Relaxed) as usize;
            if playback_buf.len() > cap {
                let excess = playback_buf.len() - cap;
                playback_buf.drain(..excess);
            }

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
