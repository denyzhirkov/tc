//! Soft limiter shared by the AGC, the capture gain stage and the playback
//! output. Samples at or below the knee pass bit-exact; above it they are
//! compressed asymptotically towards full scale instead of being hard-clipped
//! (the crackle of an over-driven signal).

use tc_shared::config;

/// Compress above the knee instead of clipping at it. Asymptotic to 1.0, so a
/// sample can approach full scale but never leave it however hard it is driven.
pub(crate) fn soft_clip(x: f32) -> f32 {
    let knee = config::LIMITER_KNEE;
    let a = x.abs();
    if a <= knee {
        return x;
    }
    let over = (a - knee) / (1.0 - knee);
    // tanh saturates to exactly 1.0 in f32; keep the output strictly below
    // full scale.
    let shaped = (knee + (1.0 - knee) * over.tanh()).min(1.0 - f32::EPSILON / 2.0);
    if x < 0.0 {
        -shaped
    } else {
        shaped
    }
}

/// Soft-clip every sample in place. Everything above the knee is shaped, but
/// the return value only says whether a sample reached full scale (|x| >= 1.0),
/// i.e. whether a hard clip was actually avoided. Loud-but-legal peaks between
/// the knee and 1.0 are not reported, so they don't read as a glitch.
pub(crate) fn limit_in_place(buf: &mut [f32]) -> bool {
    let mut engaged = false;
    for s in buf {
        let a = s.abs();
        if a > config::LIMITER_KNEE {
            engaged |= a >= 1.0;
            *s = soft_clip(*s);
        }
    }
    engaged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_up_to_the_knee() {
        let k = config::LIMITER_KNEE;
        for i in 0..=1000 {
            let x = k * (i as f32 / 1000.0);
            assert_eq!(soft_clip(x).to_bits(), x.to_bits());
            assert_eq!(soft_clip(-x).to_bits(), (-x).to_bits());
        }
    }

    #[test]
    fn bounded_below_full_scale() {
        for x in [0.81f32, 1.0, 2.0, 4.0, 100.0, 1e6] {
            let y = soft_clip(x);
            assert!(y > config::LIMITER_KNEE && y < 1.0, "{x} -> {y}");
            assert_eq!(soft_clip(-x), -y);
        }
    }

    #[test]
    fn monotonic_over_a_sweep() {
        let mut prev = soft_clip(-10.0);
        let mut x = -10.0f32;
        while x < 10.0 {
            x += 0.001;
            let y = soft_clip(x);
            assert!(y >= prev, "not monotonic at {x}");
            prev = y;
        }
    }

    #[test]
    fn limit_in_place_reports_engagement() {
        let mut quiet = [0.1f32, -0.5, config::LIMITER_KNEE];
        let copy = quiet;
        assert!(!limit_in_place(&mut quiet));
        assert_eq!(quiet, copy);

        // Shaped, but nothing reached full scale: not reported.
        let mut legal = [0.1f32, -0.9, 0.85];
        assert!(!limit_in_place(&mut legal));
        assert_eq!(legal[0], 0.1);
        assert!(legal[1] > -0.9 && legal[1] < -config::LIMITER_KNEE);

        let mut loud = [0.1f32, -3.0, 1.2];
        assert!(limit_in_place(&mut loud));
        assert_eq!(loud[0], 0.1);
        assert!(loud[1] < -config::LIMITER_KNEE && loud[1] > -1.0);
        assert!(loud[2] > config::LIMITER_KNEE && loud[2] < 1.0);
    }
}
