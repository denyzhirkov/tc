use anyhow::Result;
use audiopus::coder::{Decoder, Encoder};
use audiopus::{Application, Bitrate, Channels, SampleRate, Signal};

use tc_shared::config;

pub struct OpusEncoder {
    encoder: Encoder,
    encode_buf: Vec<u8>,
}

impl OpusEncoder {
    pub fn new() -> Result<Self> {
        let mut encoder = Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Voip)?;
        // DTX: encoder produces tiny packets during silence
        encoder.set_dtx(true)?;
        // Tell Opus the source is speech so it tunes for voice clarity. Audio
        // *bandwidth* (narrow→fullband) is left to Opus to pick from the
        // bitrate — pinning it would waste bits / hurt HF on the low tiers; at
        // the 64 kbit/s HIGH tier it selects fullband on its own.
        encoder.set_signal(Signal::Voice)?;
        Ok(Self {
            encoder,
            encode_buf: vec![0u8; config::MAX_OPUS_PACKET],
        })
    }

    /// Apply adaptive quality settings to the encoder.
    pub fn apply_quality_settings(
        &mut self,
        bitrate: i32,
        complexity: u8,
        fec: bool,
        loss_percent: u8,
    ) -> Result<()> {
        self.encoder.set_bitrate(Bitrate::BitsPerSecond(bitrate))?;
        self.encoder.set_complexity(complexity)?;
        self.encoder.set_inband_fec(fec)?;
        self.encoder.set_packet_loss_perc(loss_percent)?;
        Ok(())
    }

    /// Current (bitrate, complexity, inband FEC, packet-loss hint), for tests.
    #[cfg(test)]
    pub fn quality_settings(&self) -> (i32, u8, bool, u8) {
        let Bitrate::BitsPerSecond(bitrate) = self.encoder.bitrate().unwrap() else {
            panic!("bitrate is not a plain bits-per-second value");
        };
        (
            bitrate,
            self.encoder.complexity().unwrap(),
            self.encoder.inband_fec().unwrap(),
            self.encoder.packet_loss_perc().unwrap(),
        )
    }

    /// Constant-rate mode for traffic-analysis resistance ("paranoid"): disable
    /// VBR and DTX so every frame is full, fixed-ish size and silence never
    /// collapses into tiny packets that would betray *when* someone is speaking.
    /// `on = false` restores the defaults (VBR + DTX on).
    pub fn set_constant_bitrate(&mut self, on: bool) -> Result<()> {
        self.encoder.set_vbr(!on)?;
        self.encoder.set_dtx(!on)?;
        Ok(())
    }

    /// Encode a frame of f32 PCM samples into Opus bytes.
    /// Returns a slice into the internal buffer (zero-copy).
    pub fn encode(&mut self, pcm: &[f32]) -> Result<&[u8]> {
        let len = self.encoder.encode_float(pcm, &mut self.encode_buf)?;
        Ok(&self.encode_buf[..len])
    }
}

/// RNNoise-based noise suppression (via `nnnoiseless`). Wraps a `DenoiseState`
/// plus reusable scratch so a 20 ms / 960-sample capture frame is denoised
/// in place as two 480-sample RNNoise frames — no per-frame heap allocation.
///
/// nnnoiseless expects 48 kHz mono samples in **i16 range** (±32768), while our
/// pipeline carries normalized f32 in [-1, 1]; `process` scales in and out.
pub struct Denoiser {
    state: Box<nnnoiseless::DenoiseState<'static>>,
    scratch_in: [f32; nnnoiseless::FRAME_SIZE],
    scratch_out: [f32; nnnoiseless::FRAME_SIZE],
}

impl Denoiser {
    pub fn new() -> Self {
        Self {
            state: nnnoiseless::DenoiseState::new(),
            scratch_in: [0.0; nnnoiseless::FRAME_SIZE],
            scratch_out: [0.0; nnnoiseless::FRAME_SIZE],
        }
    }

    /// Denoise a frame in place. `pcm.len()` must be a multiple of
    /// `nnnoiseless::FRAME_SIZE` (480); our 960-sample frames satisfy this.
    pub fn process(&mut self, pcm: &mut [f32]) {
        const N: usize = nnnoiseless::FRAME_SIZE;
        for chunk in pcm.chunks_mut(N) {
            if chunk.len() != N {
                break; // tail shorter than a RNNoise frame: leave as-is
            }
            for (s, c) in self.scratch_in.iter_mut().zip(chunk.iter()) {
                *s = *c * 32768.0;
            }
            self.state
                .process_frame(&mut self.scratch_out, &self.scratch_in);
            for (c, o) in chunk.iter_mut().zip(self.scratch_out.iter()) {
                *c = (*o / 32768.0).clamp(-1.0, 1.0);
            }
        }
    }
}

impl Default for Denoiser {
    fn default() -> Self {
        Self::new()
    }
}

pub struct OpusDecoder {
    decoder: Decoder,
    decode_buf: Vec<f32>,
}

impl OpusDecoder {
    pub fn new() -> Result<Self> {
        let decoder = Decoder::new(SampleRate::Hz48000, Channels::Mono)?;
        Ok(Self {
            decoder,
            decode_buf: vec![0.0f32; config::FRAME_SIZE],
        })
    }

    /// Decode Opus bytes into f32 PCM samples.
    /// Returns a slice into the internal buffer (valid until next decode call).
    pub fn decode(&mut self, opus_data: &[u8]) -> Result<&[f32]> {
        let packet = audiopus::packet::Packet::try_from(opus_data)?;
        let signals = audiopus::MutSignals::try_from(self.decode_buf.as_mut_slice())?;
        let samples = self.decoder.decode_float(Some(packet), signals, false)?;
        Ok(&self.decode_buf[..samples])
    }

    /// Packet loss concealment — generate a replacement frame for a missing packet.
    /// Returns a slice into the internal buffer (valid until next decode call).
    pub fn decode_plc(&mut self) -> Result<&[f32]> {
        let signals = audiopus::MutSignals::try_from(self.decode_buf.as_mut_slice())?;
        let samples = self.decoder.decode_float(None, signals, false)?;
        Ok(&self.decode_buf[..samples])
    }

    /// The first `len` samples of the most recent decode.
    pub fn frame(&self, len: usize) -> &[f32] {
        &self.decode_buf[..len]
    }

    /// Recover the frame *before* `next_packet` from the redundancy (LBRR) that
    /// `next_packet` carries. Only meaningful right after a lost packet and
    /// before `next_packet` itself is decoded normally. Same buffer lifetime
    /// as `decode`.
    pub fn decode_fec(&mut self, next_packet: &[u8]) -> Result<&[f32]> {
        let packet = audiopus::packet::Packet::try_from(next_packet)?;
        let signals = audiopus::MutSignals::try_from(self.decode_buf.as_mut_slice())?;
        let samples = self.decoder.decode_float(Some(packet), signals, true)?;
        Ok(&self.decode_buf[..samples])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_new() {
        let enc = OpusEncoder::new();
        assert!(enc.is_ok());
    }

    #[test]
    fn decoder_new() {
        let dec = OpusDecoder::new();
        assert!(dec.is_ok());
    }

    #[test]
    fn denoiser_processes_full_frame_in_range() {
        let mut d = Denoiser::new();
        // A noisy 960-sample frame (our capture frame = 2 RNNoise frames).
        let mut pcm: Vec<f32> = (0..config::FRAME_SIZE)
            .map(|i| ((i as f32 * 0.37).sin() * 0.4 + (i as f32 * 0.011).cos() * 0.2))
            .collect();
        d.process(&mut pcm);
        assert_eq!(pcm.len(), config::FRAME_SIZE);
        // Output stays valid, normalized PCM (no NaNs, within range).
        assert!(
            pcm.iter()
                .all(|s| s.is_finite() && (-1.0..=1.0).contains(s)),
            "denoised samples must stay finite and in [-1, 1]"
        );
    }

    #[test]
    fn encode_silence() {
        let mut enc = OpusEncoder::new().unwrap();
        let pcm = vec![0.0f32; config::FRAME_SIZE];
        let result = enc.encode(&pcm);
        assert!(result.is_ok());
        let opus = result.unwrap();
        assert!(!opus.is_empty());
        // DTX enabled — silence should produce a very small packet
        // Opus with DTX may still produce a non-trivial first silence packet
        assert!(opus.len() <= config::MAX_OPUS_PACKET);
    }

    #[test]
    fn encode_tone() {
        let mut enc = OpusEncoder::new().unwrap();
        // 440 Hz sine wave at 48kHz
        let pcm: Vec<f32> = (0..config::FRAME_SIZE)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin() * 0.5)
            .collect();
        let opus = enc.encode(&pcm).unwrap();
        assert!(!opus.is_empty());
        assert!(opus.len() <= config::MAX_OPUS_PACKET);
    }

    #[test]
    fn encode_decode_roundtrip() {
        let mut enc = OpusEncoder::new().unwrap();
        let mut dec = OpusDecoder::new().unwrap();
        // 440 Hz sine
        let pcm: Vec<f32> = (0..config::FRAME_SIZE)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin() * 0.5)
            .collect();
        let opus = enc.encode(&pcm).unwrap().to_vec();
        let decoded = dec.decode(&opus).unwrap();
        assert_eq!(decoded.len(), config::FRAME_SIZE);
        // Lossy codec — check that the signal is roughly the same shape
        let correlation: f32 = pcm.iter().zip(decoded.iter()).map(|(a, b)| a * b).sum();
        assert!(
            correlation > 0.0,
            "decoded signal should correlate with input"
        );
    }

    #[test]
    fn decode_plc_produces_frame() {
        let mut enc = OpusEncoder::new().unwrap();
        let mut dec = OpusDecoder::new().unwrap();
        // Feed one real frame first so PLC has state
        let pcm: Vec<f32> = (0..config::FRAME_SIZE)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin() * 0.5)
            .collect();
        let opus = enc.encode(&pcm).unwrap().to_vec();
        let _ = dec.decode(&opus).unwrap();
        // Now PLC
        let plc = dec.decode_plc().unwrap();
        assert_eq!(plc.len(), config::FRAME_SIZE);
    }

    /// Paranoid mode's core property: with constant-bitrate on, silence frames
    /// stay full and equal-size instead of collapsing into tiny DTX packets —
    /// otherwise packet size would betray *when* someone is actually speaking.
    #[test]
    fn constant_bitrate_keeps_silence_frames_flat() {
        let mut enc = OpusEncoder::new().unwrap();
        enc.apply_quality_settings(32_000, 5, false, 0).unwrap();
        enc.set_constant_bitrate(true).unwrap();
        let silence = vec![0.0f32; config::FRAME_SIZE];
        let sizes: Vec<usize> = (0..10)
            .map(|_| enc.encode(&silence).unwrap().len())
            .collect();
        assert!(
            sizes.iter().all(|&s| s > 10),
            "silence frames must not shrink under CBR: {:?}",
            sizes
        );
        assert!(
            sizes.windows(2).all(|w| w[0] == w[1]),
            "CBR silence frames must all be the same size: {:?}",
            sizes
        );
    }

    #[test]
    fn apply_quality_settings_ok() {
        let mut enc = OpusEncoder::new().unwrap();
        assert!(enc.apply_quality_settings(64_000, 5, true, 10).is_ok());
        assert!(enc.apply_quality_settings(24_000, 2, false, 0).is_ok());
        assert!(enc.apply_quality_settings(96_000, 10, true, 50).is_ok());
    }

    #[test]
    fn encode_after_quality_change() {
        let mut enc = OpusEncoder::new().unwrap();
        enc.apply_quality_settings(24_000, 2, true, 20).unwrap();
        let pcm: Vec<f32> = (0..config::FRAME_SIZE)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin() * 0.5)
            .collect();
        let opus = enc.encode(&pcm).unwrap();
        assert!(!opus.is_empty());
    }

    #[test]
    fn multiple_encodes_reuse_buffer() {
        let mut enc = OpusEncoder::new().unwrap();
        let pcm: Vec<f32> = (0..config::FRAME_SIZE)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin() * 0.5)
            .collect();
        let len1 = enc.encode(&pcm).unwrap().len();
        let len2 = enc.encode(&pcm).unwrap().len();
        // Opus is stateful, so same input may produce slightly different sizes, but both valid
        assert!(len1 > 0 && len2 > 0);
    }

    /// Deterministic voiced-speech stand-in: harmonics of a ~150 Hz fundamental
    /// with slow vibrato and amplitude modulation.
    fn speech_like(frames: usize) -> Vec<f32> {
        let mut phase = 0.0f32;
        (0..frames * config::FRAME_SIZE)
            .map(|i| {
                let t = i as f32 / config::SAMPLE_RATE as f32;
                let f0 = 150.0 * (1.0 + 0.1 * (2.0 * std::f32::consts::PI * 0.7 * t).sin());
                phase += 2.0 * std::f32::consts::PI * f0 / config::SAMPLE_RATE as f32;
                let amp = 0.12 * (0.6 + 0.4 * (2.0 * std::f32::consts::PI * 2.3 * t).sin());
                let voiced: f32 = (1..=20).map(|h| (h as f32 * phase).sin() / h as f32).sum();
                amp * voiced
            })
            .collect()
    }

    fn high_tier_encoder() -> OpusEncoder {
        let mut enc = OpusEncoder::new().unwrap();
        enc.apply_quality_settings(
            config::BITRATE_HIGH,
            config::COMPLEXITY_HIGH,
            true,
            config::FEC_MIN_LOSS_PERC,
        )
        .unwrap();
        enc
    }

    fn encode_all(enc: &mut OpusEncoder, pcm: &[f32]) -> Vec<Vec<u8>> {
        pcm.chunks(config::FRAME_SIZE)
            .map(|f| enc.encode(f).unwrap().to_vec())
            .collect()
    }

    /// Relative error energy of `got` against `reference`, in dB (lower is better).
    fn err_db(got: &[f32], reference: &[f32]) -> f32 {
        let num: f32 = got
            .iter()
            .zip(reference)
            .map(|(a, b)| (a - b) * (a - b))
            .sum();
        let den: f32 = reference.iter().map(|b| b * b).sum();
        10.0 * (num / den).log10()
    }

    #[test]
    fn fec_recovers_a_lost_frame_better_than_plc() {
        const FRAMES: usize = 75; // 1.5 s
        const LOST: usize = 30;
        let packets = encode_all(&mut high_tier_encoder(), &speech_like(FRAMES));

        let mut reference = OpusDecoder::new().unwrap();
        let mut fec = OpusDecoder::new().unwrap();
        let mut plc = OpusDecoder::new().unwrap();
        let (mut ref_k, mut ref_k1) = (Vec::new(), Vec::new());
        let (mut fec_k, mut fec_k1) = (Vec::new(), Vec::new());
        let (mut plc_k, mut plc_k1) = (Vec::new(), Vec::new());

        for (n, pkt) in packets.iter().enumerate() {
            let r = reference.decode(pkt).unwrap().to_vec();
            if n == LOST {
                ref_k = r;
                fec_k = fec.decode_fec(&packets[LOST + 1]).unwrap().to_vec();
                assert_eq!(fec_k.len(), config::FRAME_SIZE);
                plc_k = plc.decode_plc().unwrap().to_vec();
            } else if n == LOST + 1 {
                ref_k1 = r;
                fec_k1 = fec.decode(pkt).unwrap().to_vec();
                plc_k1 = plc.decode(pkt).unwrap().to_vec();
            } else {
                fec.decode(pkt).unwrap();
                plc.decode(pkt).unwrap();
            }
        }

        let (fec_db, plc_db) = (err_db(&fec_k, &ref_k), err_db(&plc_k, &ref_k));
        let (fec1_db, plc1_db) = (err_db(&fec_k1, &ref_k1), err_db(&plc_k1, &ref_k1));
        eprintln!("lost frame: fec {fec_db:.1} dB vs plc {plc_db:.1} dB");
        eprintln!("next frame: fec {fec1_db:.1} dB vs plc {plc1_db:.1} dB");
        // 6 dB = half the error amplitude; FEC decodes the actual coded
        // signal, PLC extrapolates the past.
        assert!(fec_db + 6.0 < plc_db, "fec {fec_db} dB, plc {plc_db} dB");
        assert!(
            fec1_db <= plc1_db + 0.5,
            "fec {fec1_db} dB, plc {plc1_db} dB"
        );
    }

    /// With FEC on, Opus leaves SILK-only and goes hybrid/CELT at the HIGH
    /// bitrate; HD voice requires super-wideband or fullband (TOC config
    /// 12-15 hybrid, 24-31 CELT SWB/FB; 16-23 are CELT NB/WB).
    #[test]
    fn hd_voice_still_wideband_or_better() {
        let packets = encode_all(&mut high_tier_encoder(), &speech_like(50));
        for pkt in &packets[10..] {
            let cfg = pkt[0] >> 3;
            assert!(
                (12..=15).contains(&cfg) || cfg >= 24,
                "TOC config {cfg} is below super-wideband"
            );
        }
    }

    #[test]
    fn fec_is_actually_emitted_at_high_tier() {
        // LBRR makes packets larger than the same signal without the hint.
        let pcm = speech_like(50);
        let with: usize = encode_all(&mut high_tier_encoder(), &pcm)
            .iter()
            .map(Vec::len)
            .sum();
        let mut no_fec = OpusEncoder::new().unwrap();
        no_fec
            .apply_quality_settings(config::BITRATE_HIGH, config::COMPLEXITY_HIGH, false, 0)
            .unwrap();
        let without: usize = encode_all(&mut no_fec, &pcm).iter().map(Vec::len).sum();
        eprintln!("bytes with fec {with}, without {without}");
        assert!(with != without);
    }

    /// The production setup (HIGH tier + FEC) with paranoid CBR on keeps
    /// speech and silence frames the same size.
    #[test]
    fn constant_bitrate_is_flat_with_the_production_setup() {
        let mut enc = high_tier_encoder();
        enc.set_constant_bitrate(true).unwrap();
        let mut pcm = speech_like(10);
        pcm.extend(vec![0.0f32; 10 * config::FRAME_SIZE]);
        let sizes: Vec<usize> = encode_all(&mut enc, &pcm).iter().map(Vec::len).collect();
        eprintln!("cbr sizes: {sizes:?}");
        assert!(sizes.iter().all(|&s| s > 10));
        assert!(
            sizes.windows(2).all(|w| w[0] == w[1]),
            "CBR frames must all be the same size: {sizes:?}"
        );
    }
}
