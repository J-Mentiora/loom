// audio_bridge::resample — native → 16 kHz linear resampler for captured audio.
//
// The captured inbound track is already Opus-decoded upstream and the sink is a
// 16 kHz speech-to-text model, so a windowed-sinc kernel buys nothing measurable
// here (PRD D4); linear interpolation keeps the shim crate free of any new
// resampling dependency. Input is mono f32 at the track's native rate; output is
// mono f32 at `dst_rate` (the caller then maps f32 → i16 for the WAV mux).

/// Linear-interpolate `input` (mono f32 @ `src_rate`) to `dst_rate` (mono f32).
///
/// - `src_rate == dst_rate` (or a single sample) → the input is returned verbatim.
/// - Empty input, `src_rate == 0`, or `dst_rate == 0` → empty output (a zero rate
///   is meaningless and must never divide).
///
/// Output length is `round(input.len() * dst_rate / src_rate)`, computed in f64 so
/// long clips don't overflow a u32 product.
pub fn linear_resample(input: &[f32], src_rate: u32, dst_rate: u32) -> Vec<f32> {
    if input.is_empty() || src_rate == 0 || dst_rate == 0 {
        return Vec::new();
    }
    if src_rate == dst_rate || input.len() == 1 {
        return input.to_vec();
    }

    let ratio = src_rate as f64 / dst_rate as f64;
    let out_len = ((input.len() as f64) * (dst_rate as f64) / (src_rate as f64)).round() as usize;
    if out_len == 0 {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(out_len);
    let last = input.len() - 1;
    for i in 0..out_len {
        // Position in source-sample space for output sample `i`.
        let src_pos = i as f64 * ratio;
        let idx = src_pos.floor() as usize;
        if idx >= last {
            // At/past the final source sample — clamp (no right neighbour to lerp).
            out.push(input[last]);
            continue;
        }
        let frac = (src_pos - idx as f64) as f32;
        let a = input[idx];
        let b = input[idx + 1];
        out.push(a + (b - a) * frac);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_zero_rate_yield_empty() {
        assert!(linear_resample(&[], 48_000, 16_000).is_empty());
        assert!(linear_resample(&[0.1, 0.2], 0, 16_000).is_empty());
        assert!(linear_resample(&[0.1, 0.2], 48_000, 0).is_empty());
    }

    #[test]
    fn equal_rate_is_passthrough() {
        let s = vec![0.0, 0.5, -0.5, 1.0];
        assert_eq!(linear_resample(&s, 16_000, 16_000), s);
    }

    #[test]
    fn single_sample_is_passthrough() {
        assert_eq!(linear_resample(&[0.42], 48_000, 16_000), vec![0.42]);
    }

    #[test]
    fn downsample_3x_has_expected_length() {
        // 48 kHz → 16 kHz is exactly 1/3 the samples.
        let input: Vec<f32> = (0..300).map(|i| i as f32).collect();
        let out = linear_resample(&input, 48_000, 16_000);
        assert_eq!(out.len(), 100);
        // First sample preserved; interpolation is monotone on a ramp.
        assert!((out[0] - 0.0).abs() < 1e-6);
        assert!(out.windows(2).all(|w| w[1] >= w[0]));
    }

    #[test]
    fn upsample_has_expected_length_and_endpoints() {
        let input = vec![0.0, 1.0];
        let out = linear_resample(&input, 8_000, 16_000);
        assert_eq!(out.len(), 4);
        assert!((out[0] - 0.0).abs() < 1e-6);
        // Ramp stays within [0,1] and non-decreasing.
        assert!(out.windows(2).all(|w| w[1] >= w[0] - 1e-6));
        assert!(out.iter().all(|&v| (0.0..=1.0).contains(&v)));
    }

    #[test]
    fn resampled_sine_preserves_rms_within_tolerance() {
        // AC3 substrate: a 440 Hz sine resampled 48k → 16k keeps its RMS (≈0.707
        // for unit amplitude) — the resampler must not attenuate or add energy.
        let src_rate = 48_000u32;
        let dst_rate = 16_000u32;
        let freq = 440.0f64;
        let n = src_rate as usize; // 1 second
        let input: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / src_rate as f64).sin() as f32)
            .collect();
        let out = linear_resample(&input, src_rate, dst_rate);
        let rms = |s: &[f32]| {
            (s.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / s.len() as f64).sqrt()
        };
        assert!(
            (rms(&input) - rms(&out)).abs() < 1e-2,
            "RMS drift too large"
        );
    }
}
