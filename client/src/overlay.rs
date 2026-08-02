//! オーバーレイの箱の寸法計算。
//!
//! 確定・部分認識でテキストが増えるたびに箱を伸ばし、セッション中の
//! テキストを見えるままにする。折り返しを含む表示行数を文字幅から
//! 見積もって高さを決める。view 側のレイアウト定数（文字サイズ・
//! 行間・余白）とここの計算は対で保つこと。

/// 本文の文字サイズ（px）。
pub const TEXT_SIZE: u32 = 20;
/// 半角（ASCII）文字の推定幅（px）。プロポーショナルフォントの平均値
/// として 0.5em を使う。全角（CJK）は 1em = TEXT_SIZE
pub const HALF_WIDTH: u32 = TEXT_SIZE / 2;
/// 1 行の高さ（px）。iced の既定行高 1.3 × TEXT_SIZE。
pub const LINE_HEIGHT: u32 = 26;
/// 行間（px）。
pub const SPACING: u32 = 4;
/// 箱の上下の余白（px、片側）。
pub const PADDING_V: u32 = 8;
/// 箱の左右の余白（px、片側）。
pub const PADDING_H: u32 = 16;
/// 表示行数（折り返し込み）の上限。超えた分は古い行から隠す。
pub const MAX_ROWS: u32 = 9;

/// 文字列の推定表示幅（px）。半角 0.5em / 全角 1em の見積もり。
pub fn est_text_width(s: &str) -> u32 {
    let _ = s;
    todo!("GREEN コミットで実装")
}

/// 幅 `usable_px` の領域に折り返して表示したときの推定行数（最低 1）。
pub fn est_rows(s: &str, usable_px: u32) -> u32 {
    let _ = (s, usable_px);
    todo!("GREEN コミットで実装")
}

/// 表示行数 `rows`（折り返し込み）を収める箱の高さ（px）。
/// `MAX_ROWS` で頭打ちになる。
pub fn bar_height(rows: u32) -> u32 {
    let _ = rows;
    todo!("GREEN コミットで実装")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_half_width() {
        assert_eq!(est_text_width("abcd"), 4 * HALF_WIDTH);
    }

    #[test]
    fn cjk_is_full_width() {
        assert_eq!(est_text_width("あいう"), 3 * TEXT_SIZE);
        assert_eq!(est_text_width("あa"), TEXT_SIZE + HALF_WIDTH);
    }

    #[test]
    fn short_text_is_single_row() {
        assert_eq!(est_rows("", 900), 1, "空文字列も 1 行");
        assert_eq!(est_rows("あいう", 900), 1);
    }

    #[test]
    fn wrapped_text_counts_rows() {
        // 全角 10 文字 = 200px。幅 200 なら 1 行、199 なら 2 行
        let s = "あいうえおかきくけこ";
        assert_eq!(est_rows(s, 200), 1);
        assert_eq!(est_rows(s, 199), 2);
        // 幅 66（全角 3 文字ちょっと）なら 200/66 の切り上げ = 4 行
        assert_eq!(est_rows(s, 66), 4);
    }

    #[test]
    fn single_row_height() {
        // 1 行: 余白 8×2 + 行 26
        assert_eq!(bar_height(1), 42);
        assert_eq!(bar_height(0), 42, "0 行は 1 行に切り上げ");
    }

    #[test]
    fn each_row_adds_height() {
        // 1 行増えるごとに 行 26 + 行間 4
        assert_eq!(bar_height(2), 72);
        assert_eq!(bar_height(3) - bar_height(2), LINE_HEIGHT + SPACING);
    }

    #[test]
    fn rows_are_capped() {
        assert_eq!(
            bar_height(MAX_ROWS + 5),
            bar_height(MAX_ROWS),
            "上限を超えたら高さは伸びない（古い行が隠れる）"
        );
    }
}
