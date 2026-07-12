// audio_bridge::wav — hand-written 16 kHz mono i16 WAV mux for captured audio.
//
// The capture sink is a canonical PCM WAV (44-byte RIFF header + LE i16 body). We
// write the header by hand rather than pull in a WAV crate or shell out to ffmpeg
// (PRD D4: zero new crates; the capture path must NEVER surface an
// `encoder_unavailable`-style failure the way the video screencast path can).

/// Map a normalized f32 sample to i16, saturating at the rails. Values outside
/// `[-1.0, 1.0]` clamp to `i16::MIN`/`i16::MAX` (never wrap), and NaN → 0.
pub fn f32_to_i16(sample: f32) -> i16 {
    if sample.is_nan() {
        return 0;
    }
    let clamped = sample.clamp(-1.0, 1.0);
    // Scale by i16::MAX (32767) so +1.0 → 32767 and -1.0 → -32767; the extra
    // negative code (-32768) is simply never produced, which is standard.
    (clamped * i16::MAX as f32).round() as i16
}

/// Write a canonical mono 16-bit PCM WAV: 44-byte RIFF/`WAVE` header followed by
/// the little-endian i16 sample body. `sample_rate` is the WAV frame rate (16 000
/// for the capture path). An empty `samples` slice yields a valid header with a
/// zero-length `data` chunk.
pub fn write_wav_mono_i16(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    const CHANNELS: u16 = 1;
    const BITS_PER_SAMPLE: u16 = 16;
    const HEADER_LEN: usize = 44;

    let data_len = samples.len() * 2; // 2 bytes per i16 sample
    let byte_rate = sample_rate * u32::from(CHANNELS) * u32::from(BITS_PER_SAMPLE) / 8;
    let block_align = CHANNELS * BITS_PER_SAMPLE / 8;
    // RIFF chunk size = everything after the first 8 bytes = 36 + data_len.
    let riff_chunk_size = (36 + data_len) as u32;

    let mut out = Vec::with_capacity(HEADER_LEN + data_len);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_chunk_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");

    // fmt  sub-chunk (16 bytes, PCM).
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // sub-chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // audio format = PCM
    out.extend_from_slice(&CHANNELS.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&BITS_PER_SAMPLE.to_le_bytes());

    // data sub-chunk.
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    for &s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_to_i16_scales_and_saturates() {
        assert_eq!(f32_to_i16(0.0), 0);
        assert_eq!(f32_to_i16(1.0), i16::MAX);
        assert_eq!(f32_to_i16(-1.0), -i16::MAX);
        // Over-rail inputs saturate rather than wrap.
        assert_eq!(f32_to_i16(2.5), i16::MAX);
        assert_eq!(f32_to_i16(-2.5), -i16::MAX);
        assert_eq!(f32_to_i16(f32::NAN), 0);
    }

    #[test]
    fn header_is_canonical_16k_mono_pcm() {
        let samples = [0i16, 1, -1, 12345, -12345];
        let wav = write_wav_mono_i16(&samples, 16_000);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(&wav[36..40], b"data");
        // fmt sub-chunk fields.
        assert_eq!(u16::from_le_bytes([wav[20], wav[21]]), 1, "PCM format");
        assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1, "mono");
        assert_eq!(
            u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
            16_000,
            "sample rate"
        );
        assert_eq!(
            u16::from_le_bytes([wav[34], wav[35]]),
            16,
            "bits per sample"
        );
        // Byte rate = 16000 * 1 * 16/8 = 32000.
        assert_eq!(
            u32::from_le_bytes([wav[28], wav[29], wav[30], wav[31]]),
            32_000
        );
        // data length + total length.
        let data_len = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]);
        assert_eq!(data_len as usize, samples.len() * 2);
        assert_eq!(wav.len(), 44 + samples.len() * 2);
        // RIFF chunk size = 36 + data.
        assert_eq!(
            u32::from_le_bytes([wav[4], wav[5], wav[6], wav[7]]) as usize,
            36 + samples.len() * 2
        );
        // Body round-trips (little-endian).
        assert_eq!(i16::from_le_bytes([wav[44], wav[45]]), 0);
        assert_eq!(i16::from_le_bytes([wav[46], wav[47]]), 1);
        assert_eq!(i16::from_le_bytes([wav[48], wav[49]]), -1);
    }

    #[test]
    fn empty_samples_still_valid_header() {
        let wav = write_wav_mono_i16(&[], 16_000);
        assert_eq!(wav.len(), 44);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]), 0);
    }
}
