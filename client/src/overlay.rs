//! オーバーレイバーの寸法計算。
//!
//! 確定行が増えるたびにバーを下端から上へ伸ばし、セッション中の
//! 確定済みテキストを見えるままにする。view 側のレイアウト定数
//! （文字サイズ・行間・余白）とここの計算は対で保つこと。

/// 本文の文字サイズ（px）。
pub const TEXT_SIZE: u32 = 20;
/// 1 行の高さ（px）。iced の既定行高 1.3 × TEXT_SIZE。
pub const LINE_HEIGHT: u32 = 26;
/// 行間（px）。
pub const SPACING: u32 = 4;
/// バー上下の余白（px、片側）。
pub const PADDING_V: u32 = 8;
/// 表示する確定行数の上限。超えた分は古い行から画面外へ追い出す。
pub const MAX_COMMIT_ROWS: usize = 8;

/// 確定 `commits` 行 + 現在行（部分認識/状態表示）を収めるバーの高さ（px）。
/// 確定行は `MAX_COMMIT_ROWS` で頭打ちになる。
pub fn bar_height(commits: usize) -> u32 {
    let rows = commits.min(MAX_COMMIT_ROWS) as u32 + 1;
    PADDING_V * 2 + rows * LINE_HEIGHT + (rows - 1) * SPACING
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_commit_is_single_row() {
        // 現在行のみ: 余白 8×2 + 行 26
        assert_eq!(bar_height(0), 42);
    }

    #[test]
    fn each_commit_adds_one_row() {
        // 1 行増えるごとに 行 26 + 行間 4
        assert_eq!(bar_height(1), 72);
        assert_eq!(bar_height(2) - bar_height(1), LINE_HEIGHT + SPACING);
    }

    #[test]
    fn rows_are_capped() {
        assert_eq!(
            bar_height(MAX_COMMIT_ROWS + 5),
            bar_height(MAX_COMMIT_ROWS),
            "上限を超えたら高さは伸びない（古い行が画面外に出る）"
        );
    }
}
