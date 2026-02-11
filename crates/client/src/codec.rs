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
    pub fn encode(&mut self, pcm: &[f32]) -> Result<Vec<u8>> {
        let len = self.encoder.encode_float(pcm, &mut self.encode_buf)?;
        Ok(self.encode_buf[..len].to_vec())
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
    pub fn decode(&mut self, opus_data: &[u8]) -> Result<Vec<f32>> {
        let packet = audiopus::packet::Packet::try_from(opus_data)?;
        let signals = audiopus::MutSignals::try_from(self.decode_buf.as_mut_slice())?;
        let samples = self.decoder.decode_float(Some(packet), signals, false)?;
        Ok(self.decode_buf[..samples].to_vec())
    }

    /// Packet loss concealment — generate a replacement frame for a missing packet.
    pub fn decode_plc(&mut self) -> Result<Vec<f32>> {
        let signals = audiopus::MutSignals::try_from(self.decode_buf.as_mut_slice())?;
        let samples = self.decoder.decode_float(None, signals, false)?;
        Ok(self.decode_buf[..samples].to_vec())
    }
}
