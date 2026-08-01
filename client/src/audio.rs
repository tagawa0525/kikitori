//! マイク取得まわりの純粋変換。
//! プロトコルの音声形式は 16kHz mono s16le（docs/PROTOCOL.md）。

/// f32 サンプル列（-1.0..1.0）を s16le バイト列にする。範囲外はクリップ。
pub fn f32_to_s16le(samples: &[f32]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|&x| (((x.clamp(-1.0, 1.0)) * 32767.0) as i16).to_le_bytes())
        .collect()
}

/// インターリーブされた多チャンネル音声を平均で mono に落とす。
pub fn downmix(samples: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return samples.to_vec();
    }
    samples
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// 整数比 `factor` のダウンサンプル。`factor` 個ずつの平均を取る
/// （単純な移動平均によるローパス。48kHz→16kHz は factor=3）。
/// 端数は捨てる。
pub fn downsample(samples: &[f32], factor: usize) -> Vec<f32> {
    if factor <= 1 {
        return samples.to_vec();
    }
    samples
        .chunks_exact(factor)
        .map(|group| group.iter().sum::<f32>() / factor as f32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_to_s16le_scales_and_clips() {
        let bytes = f32_to_s16le(&[0.0, 0.5, -0.5, 2.0, -2.0]);
        let vals: Vec<i16> = bytes
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        assert_eq!(vals[0], 0);
        assert_eq!(vals[1], 16383); // 0.5 * 32767
        assert_eq!(vals[2], -16383);
        assert_eq!(vals[3], 32767); // クリップ
        assert_eq!(vals[4], -32767);
    }

    #[test]
    fn downmix_stereo_averages() {
        assert_eq!(downmix(&[1.0, 0.0, 0.5, 0.5], 2), vec![0.5, 0.5]);
    }

    #[test]
    fn downmix_mono_is_identity() {
        assert_eq!(downmix(&[0.1, 0.2], 1), vec![0.1, 0.2]);
    }

    #[test]
    fn downsample_by_3_averages_triples() {
        let out = downsample(&[3.0, 0.0, 0.0, 0.0, 3.0, 0.0, 9.9], 3);
        assert_eq!(out, vec![1.0, 1.0]); // 端数 9.9 は捨てる
    }

    #[test]
    fn downsample_factor_1_is_identity() {
        assert_eq!(downsample(&[0.1, 0.2], 1), vec![0.1, 0.2]);
    }
}
