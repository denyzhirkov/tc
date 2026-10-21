//! Automatic capture level.
//!
//! A quiet microphone stays quiet at any position of a manual slider, because
//! the slider multiplies the noise floor along with the voice. What makes a
//! MacBook's built-in microphone sound loud is not its capsule — it is an
//! automatic gain that keeps speech at a target level. This is ours.
//!
//! Two properties carry the whole design:
//!
//! * **It only learns from speech.** Frames quieter than [`AGC_FLOOR_RMS`] do
//!   not move the gain at all. An AGC that adapts in a pause is a machine for
//!   amplifying room noise between sentences, which is exactly the artefact
//!   people recognise as "bad automatic gain".
//! * **Down fast, up slow.** Coming down is an emergency — the next syllable
//!   would otherwise arrive over-amplified and clip. Going up is cosmetic, so
//!   it takes about a second and nobody hears it happen.
//!
//! The soft limiter afterwards is not optional: gain worth having exceeds the
//! headroom of a loud syllable, and without a knee those peaks would clip
//! inside the encoder.

use tc_shared::config;

/// Per-frame automatic gain. One instance per capture stream; no allocation.
pub struct Agc {
    gain: f32,
}

impl Default for Agc {
    fn default() -> Self {
        Self::new()
    }
}

impl Agc {
    pub fn new() -> Self {
        Self { gain: 1.0 }
    }

    /// Level one frame in place. Returns the gain in force after this frame.
    pub fn process(&mut self, frame: &mut [f32]) -> f32 {
        if frame.is_empty() {
            return self.gain;
        }
        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt();

        // Silence teaches nothing. Holding here is what keeps the noise floor
        // where it was between phrases.
        if rms > config::AGC_FLOOR_RMS {
            let want =
                (config::AGC_TARGET_RMS / rms).clamp(config::AGC_MIN_GAIN, config::AGC_MAX_GAIN);
            let rate = if want < self.gain {
                config::AGC_ATTACK
            } else {
                config::AGC_RELEASE
            };
            self.gain += (want - self.gain) * rate;
        }

        for s in frame.iter_mut() {
            *s = crate::limiter::soft_clip(*s * self.gain);
        }
        self.gain
    }

    /// Current gain, for tests and diagnostics.
    pub fn gain(&self) -> f32 {
        self.gain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame of a steady tone at the given amplitude.
    fn tone(amp: f32) -> Vec<f32> {
        (0..config::FRAME_SIZE)
            .map(|i| amp * (i as f32 * 0.05).sin())
            .collect()
    }

    fn rms(frame: &[f32]) -> f32 {
        (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt()
    }

    #[test]
    fn a_quiet_voice_is_brought_up_to_the_target() {
        let mut agc = Agc::new();
        let mut last = Vec::new();
        for _ in 0..300 {
            last = tone(0.02);
            agc.process(&mut last);
        }
        let reached = rms(&last);
        assert!(
            reached > config::AGC_TARGET_RMS * 0.8,
            "quiet speech should approach the target, got {reached}"
        );
        assert!(agc.gain() <= config::AGC_MAX_GAIN);
    }

    #[test]
    fn a_loud_voice_is_brought_down() {
        let mut agc = Agc::new();
        let mut last = Vec::new();
        for _ in 0..300 {
            last = tone(0.7);
            agc.process(&mut last);
        }
        assert!(agc.gain() < 1.0, "a hot input must be attenuated");
        assert!(
            rms(&last) < 0.7 / std::f32::consts::SQRT_2,
            "and the frame with it"
        );
    }

    /// The difference between an automatic level and a noise amplifier.
    #[test]
    fn silence_does_not_wind_the_gain_up() {
        let mut agc = Agc::new();
        // Settle on speech first, so there is a gain to disturb.
        for _ in 0..200 {
            agc.process(&mut tone(0.05));
        }
        let settled = agc.gain();
        // Now a long pause carrying only a faint noise floor.
        for _ in 0..500 {
            agc.process(&mut tone(0.001));
        }
        assert_eq!(
            settled,
            agc.gain(),
            "a pause must not move the gain by even a little"
        );
    }

    #[test]
    fn the_limiter_keeps_every_sample_inside_full_scale() {
        let mut agc = Agc::new();
        for _ in 0..50 {
            let mut frame = tone(0.95);
            agc.process(&mut frame);
            assert!(
                frame.iter().all(|s| s.abs() < 1.0 && s.is_finite()),
                "peaks must compress, never leave [-1, 1]"
            );
        }
    }

    /// Attack is faster than release: an arriving shout must be tamed within a
    /// few frames, while a returning quiet voice comes up over about a second.
    #[test]
    fn it_comes_down_faster_than_it_goes_up() {
        let mut down = Agc::new();
        let mut up = Agc::new();
        for _ in 0..200 {
            down.process(&mut tone(0.05));
            up.process(&mut tone(0.05));
        }
        let settled = down.gain();
        down.process(&mut tone(0.9));
        up.process(&mut tone(0.005));
        assert!(
            (settled - down.gain()).abs() > (up.gain() - settled).abs(),
            "one frame of a shout must move the gain further than one frame of a whisper"
        );
    }
}
