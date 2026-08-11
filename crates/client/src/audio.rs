use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::Stream;
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::HeapRb;

use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use tc_shared::config;

/// SPSC ring buffer halves for lock-free audio transport.
pub type AudioProducer = ringbuf::HeapProd<f32>;
pub type AudioConsumer = ringbuf::HeapCons<f32>;

/// Number of Opus frames to buffer in ring buffers.
const RING_BUF_FRAMES: usize = 8;

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

/// Mono downmix that only counts the channels actually carrying something.
///
/// We do not always get to choose the capture format: WASAPI shared mode hands
/// us the device's mixer format or nothing, and that is routinely two channels
/// for a microphone that is physically mono — the second one silent. Averaging
/// across the raw channel count halves every sample, a clean 6 dB the user then
/// tries to buy back with the gain slider, paying in noise and headroom.
///
/// So the divisor is the number of channels that carry signal, decided from a
/// slow per-channel magnitude envelope. When every channel does — a real stereo
/// input, or any device on macOS/Linux where we ask for mono outright — this is
/// arithmetically identical to the plain average it replaces.
struct Downmix {
    /// Smoothed magnitude per channel.
    energy: Vec<f32>,
    /// Scratch: summed magnitude per channel over one callback.
    sums: Vec<f32>,
    /// Which channels are counted. All of them until the signal says otherwise.
    carries: Vec<bool>,
    active: usize,
    /// Whether the first real measurement has been reported yet.
    measured: bool,
}

impl Downmix {
    fn new(channels: usize) -> Self {
        Self {
            energy: vec![0.0; channels],
            sums: vec![0.0; channels],
            carries: vec![true; channels],
            active: channels.max(1),
            measured: false,
        }
    }

    /// Append one callback's worth of interleaved input as mono samples.
    fn push(&mut self, data: &[f32], out: &mut VecDeque<f32>) -> Option<usize> {
        let ch = self.energy.len();
        if ch <= 1 {
            out.extend(data.iter().copied());
            return None;
        }

        let inv = 1.0 / self.active as f32;
        for s in self.sums.iter_mut() {
            *s = 0.0;
        }
        let mut frames = 0usize;
        for chunk in data.chunks_exact(ch) {
            let mut sum = 0.0;
            for (i, s) in chunk.iter().enumerate() {
                self.sums[i] += s.abs();
                // A silent channel would contribute nothing anyway; skipping it
                // also keeps its noise floor out of the mix.
                if self.carries[i] {
                    sum += *s;
                }
            }
            out.push_back(sum * inv);
            frames += 1;
        }
        if frames == 0 {
            return None;
        }

        let n = frames as f32;
        for (e, s) in self.energy.iter_mut().zip(&self.sums) {
            *e += (*s / n - *e) * config::DOWNMIX_SMOOTHING;
        }
        let loudest = self.energy.iter().copied().fold(0.0f32, f32::max);
        // In silence there is nothing to learn, and re-deciding on a pause
        // between words is how a divisor starts flapping mid-sentence.
        if loudest < config::DOWNMIX_SILENCE_FLOOR {
            return None;
        }
        let floor = loudest * config::DOWNMIX_ACTIVE_RATIO;
        let mut active = 0;
        for (c, e) in self.carries.iter_mut().zip(&self.energy) {
            *c = *e >= floor;
            active += *c as usize;
        }
        let active = active.max(1);
        let changed = active != self.active;
        self.active = active;

        // Report the first real measurement even when nothing changed: "2ch"
        // alone does not say whether the spare channel is silent or a copy of
        // the first, and that is the whole difference between losing 6 dB and
        // losing nothing. One line per stream, off the back of a decision that
        // is only reached once there is signal to measure.
        if !self.measured || changed {
            self.measured = true;
            let mut levels = String::with_capacity(ch * 6);
            for (i, e) in self.energy.iter().enumerate() {
                if i > 0 {
                    levels.push_str(", ");
                }
                let rel = if loudest > 0.0 { *e / loudest } else { 0.0 };
                levels.push_str(&format!("{:.2}", rel));
            }
            tracing::info!(
                "capture channels: {} of {} carry signal (relative levels: {})",
                active,
                ch,
                levels
            );
        }
        changed.then_some(active)
    }
}

/// Linear-interpolation resampler writing into a provided buffer (zero allocation).
fn resample_into(input: &[f32], from_rate: u32, to_rate: u32, output: &mut Vec<f32>) {
    output.clear();
    if from_rate == to_rate || input.is_empty() {
        output.extend_from_slice(input);
        return;
    }
    let ratio = from_rate as f64 / to_rate as f64;
    let out_len = (input.len() as f64 / ratio).ceil() as usize;
    output.reserve(out_len.saturating_sub(output.capacity()));
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
}

/// High-quality sinc resampler (wraps rubato for zero-alloc hot path).
struct SincResampler {
    inner: SincFixedIn<f32>,
    input_buf: Vec<Vec<f32>>,
    output_buf: Vec<Vec<f32>>,
}

impl SincResampler {
    fn new(from_rate: u32, to_rate: u32, chunk_size: usize) -> Result<Self> {
        let ratio = to_rate as f64 / from_rate as f64;
        let params = SincInterpolationParameters {
            sinc_len: 64,
            f_cutoff: 0.95,
            oversampling_factor: 128,
            interpolation: SincInterpolationType::Cubic,
            window: WindowFunction::Hann2,
        };
        let inner = SincFixedIn::new(ratio, 2.0, params, chunk_size, 1)
            .map_err(|e| anyhow::anyhow!("resampler init: {}", e))?;
        let input_buf = inner.input_buffer_allocate(true);
        let output_buf = inner.output_buffer_allocate(true);
        Ok(Self {
            inner,
            input_buf,
            output_buf,
        })
    }

    /// Resample a chunk. Returns a slice into the internal output buffer.
    fn process(&mut self, input: &[f32]) -> &[f32] {
        let n = input.len().min(self.input_buf[0].len());
        self.input_buf[0][..n].copy_from_slice(&input[..n]);
        match self
            .inner
            .process_into_buffer(&self.input_buf, &mut self.output_buf, None)
        {
            Ok((_, out_len)) => &self.output_buf[0][..out_len],
            Err(_) => {
                // Fallback: return empty on error (caller pads with zeros)
                &[]
            }
        }
    }
}

/// Device-rate playback queue with click-free edges.
///
/// Everything the output callback does to the sample timeline other than
/// copying it lives here, because both edits it makes are audible if done as a
/// hard splice:
///
/// * **Trimming to the latency cap.** The sender flushes its VAD pre-roll as a
///   burst (`VAD_PREROLL_FRAMES` + 1 packets), which lands in one go and
///   overshoots `MAX_PLAYBACK_BUF` by a frame on every talkspurt. Dropping
///   those samples outright splices two unrelated points of the waveform
///   together — the click heard at the start of each phrase.
/// * **Running dry.** Between phrases the queue empties and the callback pads
///   with zeros; without a ramp, both leaving and re-entering silence are
///   steps in the signal.
///
/// Both are ramped linearly over `PLAYBACK_FADE_MS`. Allocation-free: the
/// deque keeps its capacity and the ramps are computed in place.
struct PlaybackQueue {
    buf: VecDeque<f32>,
    /// Ramp length in device-rate samples.
    fade: usize,
    /// The previous fill ended in padding zeros, so the next one ramps back in.
    silent: bool,
}

impl PlaybackQueue {
    fn new(device_rate: u32) -> Self {
        let fade = (device_rate * config::PLAYBACK_FADE_MS / 1000) as usize;
        Self {
            buf: VecDeque::with_capacity(config::MAX_PLAYBACK_BUF * 2),
            fade: fade.max(1),
            silent: true,
        }
    }

    fn extend(&mut self, samples: &[f32]) {
        self.buf.extend(samples.iter().copied());
    }

    /// Drop the oldest samples down to `cap`, crossfading across the splice so
    /// the discarded stretch doesn't leave a step. The outgoing head is faded
    /// into the incoming one over `fade` samples, so the first sample after the
    /// trim still continues whatever was played last.
    fn trim_to(&mut self, cap: usize) {
        if self.buf.len() <= cap {
            return;
        }
        let excess = self.buf.len() - cap;
        let f = self.fade.min(excess).min(cap);
        for i in 0..f {
            let t = (i + 1) as f32 / (f + 1) as f32;
            let outgoing = self.buf[i];
            let incoming = self.buf[excess + i];
            self.buf[excess + i] = outgoing * (1.0 - t) + incoming * t;
        }
        self.buf.drain(..excess);
    }

    /// Fill one device buffer, ramping the silence boundaries. `channels > 1`
    /// upmixes mono to interleaved. Short reads are padded with zeros, and the
    /// samples leading into that padding are faded out.
    fn fill(&mut self, data: &mut [f32], channels: usize) {
        let needed = data.len() / channels.max(1);
        let available = self.buf.len().min(needed);
        let fade_in = self.silent && available > 0;
        let fade_out = available < needed && available > 0;

        for (i, sample) in self.buf.drain(..available).enumerate() {
            let mut s = sample;
            if fade_in && i < self.fade {
                s *= (i + 1) as f32 / (self.fade + 1) as f32;
            }
            if fade_out {
                let remaining = available - 1 - i;
                if remaining < self.fade {
                    s *= (remaining + 1) as f32 / (self.fade + 1) as f32;
                }
            }
            if channels > 1 {
                for c in 0..channels {
                    data[i * channels + c] = s;
                }
            } else {
                data[i] = s;
            }
        }
        for sample in &mut data[available * channels..] {
            *sample = 0.0;
        }
        self.silent = available < needed;
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
/// Returns a stream handle (must be kept alive) and a lock-free ring buffer consumer
/// delivering 48 kHz mono PCM samples. Zero allocation on the audio callback thread.
/// Resolve a device by saved name, falling back to the system default when the
/// name no longer matches anything (device unplugged/renamed). A stale saved
/// name must degrade the call to the default device, not fail the whole join.
fn resolve_device(
    host: &cpal::Host,
    device_name: Option<&str>,
    is_input: bool,
) -> Result<cpal::Device> {
    if let Some(name) = device_name {
        let found = if is_input {
            host.input_devices()
                .context("failed to enumerate input devices")?
                .find(|d| d.name().ok().as_deref() == Some(name))
        } else {
            host.output_devices()
                .context("failed to enumerate output devices")?
                .find(|d| d.name().ok().as_deref() == Some(name))
        };
        match found {
            Some(d) => return Ok(d),
            None => tracing::warn!(
                "{} device '{}' not found, falling back to system default",
                if is_input { "input" } else { "output" },
                name
            ),
        }
    }
    if is_input {
        host.default_input_device()
            .context("no input device available")
    } else {
        host.default_output_device()
            .context("no output device available")
    }
}

/// Name of the current system-default input device, if any.
pub fn default_input_name() -> Option<String> {
    cpal::default_host().default_input_device()?.name().ok()
}

/// Name of the current system-default output device, if any.
pub fn default_output_name() -> Option<String> {
    cpal::default_host().default_output_device()?.name().ok()
}

/// Start capturing. Returns the stream, the consumer side of the ring buffer
/// and the name of the device actually opened (for follow-default checks).
pub fn start_capture(
    device_name: Option<&str>,
    failed: Arc<AtomicBool>,
) -> Result<(Stream, AudioConsumer, String)> {
    let host = cpal::default_host();
    let device = resolve_device(&host, device_name, true)?;
    let opened_name = device.name().unwrap_or_default();

    tracing::info!("input device: {}", opened_name);

    let params = resolve_stream_params(&device, true)?;
    tracing::info!(
        "input: {}ch {}Hz (need {}ch {}Hz)",
        params.channels,
        params.sample_rate,
        config::AUDIO_CHANNELS,
        config::SAMPLE_RATE
    );

    let device_channels = params.channels;
    let device_rate = params.sample_rate;
    let need_resample = device_rate != config::SAMPLE_RATE;

    // How many mono samples at device_rate correspond to one 20ms Opus frame
    let device_frame_size = (device_rate as f64 * config::FRAME_SIZE as f64
        / config::SAMPLE_RATE as f64)
        .round() as usize;

    // Lock-free SPSC ring buffer for capture → encoder (no allocation on audio thread)
    let rb = HeapRb::<f32>::new(config::FRAME_SIZE * RING_BUF_FRAMES);
    let (mut prod, cons) = rb.split();

    // Accumulate mono samples at device sample rate (VecDeque for O(1) front drain)
    let mut accumulator: VecDeque<f32> = VecDeque::with_capacity(device_frame_size * 2);
    let mut downmix = Downmix::new(device_channels as usize);
    // Reusable buffer for a device-rate chunk (avoids per-frame allocation)
    let mut chunk_buf = vec![0.0f32; device_frame_size];
    // Sinc resampler for high-quality capture resampling (if needed)
    let mut sinc_resampler = if need_resample {
        match SincResampler::new(device_rate, config::SAMPLE_RATE, device_frame_size) {
            Ok(r) => {
                tracing::info!(
                    "using sinc resampler for capture ({}→{}Hz)",
                    device_rate,
                    config::SAMPLE_RATE
                );
                Some(r)
            }
            Err(e) => {
                tracing::warn!("sinc resampler init failed ({}), falling back to linear", e);
                None
            }
        }
    } else {
        None
    };
    // Fallback linear resample buffer (used when sinc unavailable)
    let mut resample_buf = if need_resample {
        Vec::with_capacity(config::FRAME_SIZE + 16)
    } else {
        Vec::new()
    };

    let stream = device.build_input_stream(
        &params.stream_config,
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            // 1. Downmix to mono (if needed). It reports its own findings — the
            // first measurement and any change after it.
            downmix.push(data, &mut accumulator);

            // 2. Drain in device-frame-sized chunks, resample to 48 kHz, push to ring buffer
            while accumulator.len() >= device_frame_size {
                // Copy and drain in one pass (VecDeque drain is O(1) head advance)
                for (i, sample) in accumulator.drain(..device_frame_size).enumerate() {
                    chunk_buf[i] = sample;
                }

                if let Some(ref mut sinc) = sinc_resampler {
                    // High-quality sinc resampling — push resampled samples to ring buffer
                    let resampled = sinc.process(&chunk_buf[..device_frame_size]);
                    prod.push_slice(resampled);
                } else if need_resample {
                    // Fallback: linear interpolation
                    resample_into(
                        &chunk_buf[..device_frame_size],
                        device_rate,
                        config::SAMPLE_RATE,
                        &mut resample_buf,
                    );
                    prod.push_slice(&resample_buf);
                } else {
                    prod.push_slice(&chunk_buf[..device_frame_size]);
                }
            }
        },
        move |err| {
            // Device unplugged / format change: the stream is dead. Flag it so
            // the pipeline supervisor can rebuild on a working device.
            tracing::error!("input stream error: {}", err);
            failed.store(true, Ordering::Relaxed);
        },
        None,
    )?;

    stream.play()?;
    Ok((stream, cons, opened_name))
}

/// Start playing audio on an output device.
/// Returns a stream handle (must be kept alive) and a lock-free ring buffer producer
/// accepting 48 kHz mono PCM samples. Volume is applied in the playback callback.
pub fn start_playback(
    device_name: Option<&str>,
    playback_cap: Arc<AtomicU32>,
    output_vol: Arc<AtomicU32>,
    failed: Arc<AtomicBool>,
) -> Result<(Stream, AudioProducer, String)> {
    let host = cpal::default_host();
    let device = resolve_device(&host, device_name, false)?;
    let opened_name = device.name().unwrap_or_default();

    tracing::info!("output device: {}", opened_name);

    let params = resolve_stream_params(&device, false)?;
    tracing::info!(
        "output: {}ch {}Hz (need {}ch {}Hz)",
        params.channels,
        params.sample_rate,
        config::AUDIO_CHANNELS,
        config::SAMPLE_RATE
    );

    let device_channels = params.channels;
    let device_rate = params.sample_rate;
    let need_resample = device_rate != config::SAMPLE_RATE;

    // Lock-free SPSC ring buffer for decode → playback
    let rb = HeapRb::<f32>::new(config::FRAME_SIZE * RING_BUF_FRAMES);
    let (prod, mut cons) = rb.split();

    // Holds mono samples at device sample rate and owns the ramps that keep
    // the latency trim and the silence boundaries from clicking.
    let mut playback_buf = PlaybackQueue::new(device_rate);
    // Reusable buffer for playback resampling (avoids per-frame allocation)
    let mut resample_out = if need_resample {
        Vec::with_capacity(config::FRAME_SIZE * 2)
    } else {
        Vec::new()
    };
    // Reusable buffer for reading FRAME_SIZE chunks from ring buffer
    let mut temp_frame = vec![0.0f32; config::FRAME_SIZE];

    let stream = device.build_output_stream(
        &params.stream_config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let vol = f32::from_bits(output_vol.load(Ordering::Relaxed));

            // Read FRAME_SIZE chunks from ring buffer, apply volume, resample to device rate
            while cons.occupied_len() >= config::FRAME_SIZE {
                let n = cons.pop_slice(&mut temp_frame[..config::FRAME_SIZE]);
                // Apply output volume
                if vol != 1.0 {
                    for s in &mut temp_frame[..n] {
                        *s *= vol;
                    }
                }
                if need_resample {
                    resample_into(
                        &temp_frame[..n],
                        config::SAMPLE_RATE,
                        device_rate,
                        &mut resample_out,
                    );
                    playback_buf.extend(&resample_out);
                } else {
                    playback_buf.extend(&temp_frame[..n]);
                }
            }

            // Cap latency (scale cap from 48 kHz to device rate)
            let cap48 = playback_cap.load(Ordering::Relaxed) as usize;
            let cap = if need_resample {
                (cap48 as f64 * device_rate as f64 / config::SAMPLE_RATE as f64) as usize
            } else {
                cap48
            };
            playback_buf.trim_to(cap);
            playback_buf.fill(data, device_channels as usize);
        },
        move |err| {
            tracing::error!("output stream error: {}", err);
            failed.store(true, Ordering::Relaxed);
        },
        None,
    )?;

    stream.play()?;
    Ok((stream, prod, opened_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape that costs 6 dB on Windows: a mono microphone presented as a
    /// stereo stream with nothing in the second channel.
    #[test]
    fn a_silent_second_channel_does_not_halve_the_signal() {
        let mut d = Downmix::new(2);
        let mut out = VecDeque::new();
        // Loud enough, and long enough, for the envelope to make up its mind.
        let block: Vec<f32> = (0..4800)
            .flat_map(|i| [((i % 100) as f32) * 0.01, 0.0])
            .collect();
        for _ in 0..80 {
            out.clear();
            d.push(&block, &mut out);
        }
        assert_eq!(d.active, 1, "the silent channel must stop counting");
        out.clear();
        d.push(&[0.5, 0.0, 0.25, 0.0], &mut out);
        assert_eq!(out, VecDeque::from(vec![0.5, 0.25]));
    }

    /// A real stereo input must keep behaving exactly as the plain average did.
    #[test]
    fn a_true_stereo_input_is_still_averaged() {
        let mut d = Downmix::new(2);
        let mut out = VecDeque::new();
        let block: Vec<f32> = (0..4800)
            .flat_map(|i| {
                let v = ((i % 100) as f32) * 0.01;
                [v, v * 0.8]
            })
            .collect();
        for _ in 0..80 {
            out.clear();
            d.push(&block, &mut out);
        }
        assert_eq!(d.active, 2, "both channels carry signal");
        out.clear();
        d.push(&[1.0, 0.5], &mut out);
        assert_eq!(out, VecDeque::from(vec![0.75]));
    }

    #[test]
    fn a_mono_device_passes_straight_through() {
        let mut d = Downmix::new(1);
        let mut out = VecDeque::new();
        d.push(&[0.1, -0.2, 0.3], &mut out);
        assert_eq!(out, VecDeque::from(vec![0.1, -0.2, 0.3]));
    }

    /// Silence must not re-open the question — otherwise the divisor flips
    /// between words and the level pumps.
    #[test]
    fn silence_keeps_the_previous_decision() {
        let mut d = Downmix::new(2);
        let mut out = VecDeque::new();
        let block: Vec<f32> = (0..4800)
            .flat_map(|i| [((i % 100) as f32) * 0.01, 0.0])
            .collect();
        for _ in 0..80 {
            out.clear();
            d.push(&block, &mut out);
        }
        assert_eq!(d.active, 1);
        let quiet = vec![0.0f32; 9600];
        for _ in 0..200 {
            out.clear();
            d.push(&quiet, &mut out);
        }
        assert_eq!(d.active, 1, "a pause is not evidence about the wiring");
    }

    #[test]
    fn resample_identity() {
        let input: Vec<f32> = (0..100).map(|i| i as f32 * 0.01).collect();
        let mut output = Vec::new();
        resample_into(&input, 48000, 48000, &mut output);
        assert_eq!(output, input);
    }

    /// Largest jump between consecutive samples — the measurable proxy for a
    /// click. A clean waveform steps by roughly its own slope; a splice steps
    /// by the distance between two unrelated points of the signal.
    fn max_step(samples: &[f32]) -> f32 {
        samples
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max)
    }

    /// A rising ramp: any dropped stretch shows up directly as a step of the
    /// same size, so the splice is impossible to hide behind waveform shape.
    fn ramp(n: usize) -> Vec<f32> {
        (0..n).map(|i| i as f32 / n as f32).collect()
    }

    #[test]
    fn trim_crossfade_removes_the_splice_step() {
        let n = config::FRAME_SIZE * 6;
        let cap = config::MAX_PLAYBACK_BUF;
        let mut q = PlaybackQueue::new(config::SAMPLE_RATE);
        q.extend(&ramp(n));
        q.trim_to(cap);

        assert_eq!(q.buf.len(), cap, "trim must land exactly on the cap");
        let out: Vec<f32> = q.buf.iter().copied().collect();
        // A hard drain would leave a step of (n - cap) / n ≈ 0.167 here.
        assert!(
            max_step(&out) < 0.001,
            "crossfaded trim left an audible step: {}",
            max_step(&out)
        );
    }

    /// The regression this whole path exists for: the VAD pre-roll flush
    /// (VAD_PREROLL_FRAMES + 1 packets) arrives as one burst and overshoots the
    /// latency cap on every talkspurt, so the trim runs at the start of each
    /// phrase — exactly where a click is most audible.
    #[test]
    fn vad_preroll_burst_overshoots_the_cap_without_clicking() {
        let burst = config::FRAME_SIZE * (config::VAD_PREROLL_FRAMES + 1);
        assert!(
            burst > config::MAX_PLAYBACK_BUF,
            "pre-roll burst is expected to overshoot the cap; if it no longer \
             does, the onset trim is gone and this guard is stale"
        );

        let mut q = PlaybackQueue::new(config::SAMPLE_RATE);
        q.extend(&ramp(burst));
        q.trim_to(config::MAX_PLAYBACK_BUF);

        let mut out = vec![0.0f32; config::FRAME_SIZE];
        q.fill(&mut out, 1);
        // A hard drain splices in a step of (burst - cap) / burst ≈ 0.167 in a
        // single sample. What is left here is the deliberate ramp-in spread
        // over PLAYBACK_FADE_MS, two orders of magnitude gentler.
        assert!(
            max_step(&out) < 0.01,
            "onset of a talkspurt still steps: {}",
            max_step(&out)
        );
    }

    #[test]
    fn first_fill_after_silence_ramps_in() {
        let mut q = PlaybackQueue::new(config::SAMPLE_RATE);
        q.extend(&vec![1.0f32; config::FRAME_SIZE]);

        let mut out = vec![0.0f32; config::FRAME_SIZE];
        q.fill(&mut out, 1);

        assert!(
            out[0] < 0.05,
            "playback must not start at full amplitude: {}",
            out[0]
        );
        assert!(
            out[q.fade] > 0.99,
            "ramp must reach full amplitude within the fade window: {}",
            out[q.fade]
        );
        // Stepping straight from silence to 1.0 is the click being prevented.
        assert!(max_step(&out) < 0.05, "ramp-in stepped: {}", max_step(&out));
    }

    #[test]
    fn running_dry_ramps_out_instead_of_cutting_to_zero() {
        let mut q = PlaybackQueue::new(config::SAMPLE_RATE);
        let half = config::FRAME_SIZE / 2;
        q.extend(&vec![1.0f32; half]);
        q.silent = false; // mid-phrase: no ramp-in, only the underrun edge

        let mut out = vec![0.0f32; config::FRAME_SIZE];
        q.fill(&mut out, 1);

        assert!(
            out[half - 1] < 0.05,
            "last sample before the padding must be faded: {}",
            out[half - 1]
        );
        assert_eq!(out[half], 0.0, "short read must pad with silence");
        assert!(
            max_step(&out) < 0.05,
            "ramp-out stepped: {}",
            max_step(&out)
        );
        assert!(q.silent, "a short read must arm the ramp-in for next time");
    }

    #[test]
    fn steady_state_passes_samples_through_untouched() {
        let mut q = PlaybackQueue::new(config::SAMPLE_RATE);
        let signal = ramp(config::FRAME_SIZE * 3);
        q.extend(&signal);
        q.silent = false; // mid-phrase, buffer under the cap

        let mut out = vec![0.0f32; config::FRAME_SIZE];
        q.fill(&mut out, 1);

        assert_eq!(
            out,
            signal[..config::FRAME_SIZE],
            "no ramp may touch a fully-served mid-phrase buffer"
        );
        assert!(!q.silent);
    }

    #[test]
    fn fill_upmixes_mono_to_every_channel() {
        let mut q = PlaybackQueue::new(config::SAMPLE_RATE);
        q.extend(&[0.5f32; 4]);
        q.silent = false;

        let mut out = vec![0.0f32; 8];
        q.fill(&mut out, 2);

        assert_eq!(out, vec![0.5f32; 8]);
    }

    #[test]
    fn resample_ratio() {
        let input: Vec<f32> = vec![0.0; 960];
        let mut output = Vec::new();

        // 48000->24000 should halve length
        resample_into(&input, 48000, 24000, &mut output);
        assert_eq!(output.len(), 480);

        // 24000->48000 should double length
        let input24: Vec<f32> = vec![0.0; 480];
        resample_into(&input24, 24000, 48000, &mut output);
        assert_eq!(output.len(), 960);
    }
}
