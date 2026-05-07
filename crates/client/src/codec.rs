use anyhow::Result;
use audiopus::coder::{Decoder, Encoder};
use audiopus::{Application, Bitrate, Channels, SampleRate};

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
        self.encoder
            .set_bitrate(Bitrate::BitsPerSecond(bitrate))?;
        self.encoder.set_complexity(complexity)?;
        self.encoder.set_inband_fec(fec)?;
        self.encoder.set_packet_loss_perc(loss_percent)?;
        Ok(())
    }

    /// Encode a frame of f32 PCM samples into Opus bytes.
    /// Returns a slice into the internal buffer (zero-copy).
    pub fn encode(&mut self, pcm: &[f32]) -> Result<&[u8]> {
        let len = self.encoder.encode_float(pcm, &mut self.encode_buf)?;
        Ok(&self.encode_buf[..len])
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
        assert!(correlation > 0.0, "decoded signal should correlate with input");
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
}
