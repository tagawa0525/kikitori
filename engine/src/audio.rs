/// これ以下の RMS を無音とみなす（後パディングと取りこぼし判定用）。
/// Python 版 `SILENCE_RMS` と同値。
pub const SILENCE_RMS: f32 = 0.005;

/// 二乗平均平方根。空スライスは 0。
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples.iter().map(|&x| f64::from(x) * f64::from(x)).sum();
    (sum_sq / samples.len() as f64).sqrt() as f32
}

/// 声が乗っているか（RMS > SILENCE_RMS）。空スライスは false。
pub fn is_speech(samples: &[f32]) -> bool {
    !samples.is_empty() && rms(samples) > SILENCE_RMS
}

/// `window` を `hop` 幅で走査し、最も静かな区間の開始オフセットを返す。
/// 同値なら先頭側を選ぶ（Python の `min` がタプル第 2 要素で
/// タイブレークするのと同じ挙動）。
/// 走査範囲は Python の `range(0, len(window) - hop, hop)` に合わせ、
/// 開始位置が `len - hop` 未満のものだけを対象にする。
pub fn quietest_offset(window: &[f32], hop: usize) -> usize {
    let mut best = (f32::INFINITY, 0);
    let mut i = 0;
    while i + hop < window.len() {
        let energy = rms(&window[i..i + hop]);
        if energy < best.0 {
            best = (energy, i);
        }
        i += hop;
    }
    best.1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rms_of_silence_is_zero() {
        assert_eq!(rms(&[0.0; 100]), 0.0);
        assert_eq!(rms(&[]), 0.0);
    }

    #[test]
    fn rms_of_constant_signal() {
        let signal = [0.5_f32; 64];
        assert!((rms(&signal) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn silence_is_not_speech() {
        assert!(!is_speech(&[0.0; 100]));
        assert!(!is_speech(&[]));
    }

    #[test]
    fn loud_signal_is_speech() {
        assert!(is_speech(&[0.1; 100]));
    }

    #[test]
    fn quietest_offset_finds_quiet_block() {
        // [大, 大, 小, 大] の 4 ブロック → 3 番目（オフセット 20）
        let mut window = vec![0.5_f32; 40];
        for x in &mut window[20..30] {
            *x = 0.01;
        }
        assert_eq!(quietest_offset(&window, 10), 20);
    }

    #[test]
    fn quietest_offset_ties_pick_first() {
        // 全ブロック同音量なら先頭
        assert_eq!(quietest_offset(&[0.3; 40], 10), 0);
    }

    #[test]
    fn quietest_offset_excludes_final_partial_hop() {
        // 走査は len - hop 未満まで。最後の 10 サンプル（最小音量）は
        // 開始位置 30 = len(40) - hop(10) なので対象外
        let mut window = vec![0.5_f32; 40];
        for x in &mut window[30..40] {
            *x = 0.0;
        }
        assert_eq!(quietest_offset(&window, 10), 0);
    }
}
