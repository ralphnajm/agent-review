//! Side-by-side layout of a diff: which old and new lines sit beside each
//! other, and wrapping each side to the width of its column.

use crate::core::diff::{DiffLine, LineKind};

/// The lines of one side-by-side row, as indices into a hunk's lines: the
/// old side's and the new side's, `None` where that side is blank.
pub type Pair = (Option<usize>, Option<usize>);

/// Pairs a hunk's lines into side-by-side rows: unchanged lines beside
/// themselves, and each run of removed lines beside the added lines that
/// follow it, the shorter side padded with blanks.
pub fn pairs(lines: &[DiffLine]) -> Vec<Pair> {
    fn flush(rows: &mut Vec<Pair>, removed: &mut Vec<usize>, added: &mut Vec<usize>) {
        for i in 0..removed.len().max(added.len()) {
            rows.push((removed.get(i).copied(), added.get(i).copied()));
        }
        removed.clear();
        added.clear();
    }
    let mut rows = Vec::with_capacity(lines.len());
    let (mut removed, mut added) = (Vec::new(), Vec::new());
    for (i, line) in lines.iter().enumerate() {
        match line.kind {
            LineKind::Removed => {
                if !added.is_empty() {
                    flush(&mut rows, &mut removed, &mut added);
                }
                removed.push(i);
            }
            LineKind::Added => added.push(i),
            LineKind::Context => {
                flush(&mut rows, &mut removed, &mut added);
                rows.push((Some(i), Some(i)));
            }
        }
    }
    flush(&mut rows, &mut removed, &mut added);
    rows
}

/// Splits `text` into pieces at most `columns` cells wide, breaking after a
/// space where one is near the end, with tabs expanded to `tab_width`
/// columns. There is always at least one piece.
pub fn wrap(text: &str, columns: usize, tab_width: usize) -> Vec<String> {
    let (columns, tab_width) = (columns.max(1), tab_width.max(1));
    let mut pieces = Vec::new();
    let mut piece = String::new();
    let mut width = 0;
    // Where the piece could break: its length and width after its last space.
    let mut space: Option<(usize, usize)> = None;
    let mut column = 0;
    for c in text.chars() {
        let (c, cells, count) = match c {
            '\t' => (' ', 1, tab_width - column % tab_width),
            c => (c, cell_width(c), 1),
        };
        column += cells * count;
        for _ in 0..count {
            if width + cells > columns && !piece.is_empty() {
                if let Some((len, at)) = space.filter(|&(_, at)| at > columns / 2) {
                    let rest = piece.split_off(len);
                    pieces.push(std::mem::replace(&mut piece, rest));
                    width -= at;
                } else {
                    pieces.push(std::mem::take(&mut piece));
                    width = 0;
                }
                space = None;
            }
            piece.push(c);
            width += cells;
            if c == ' ' {
                space = Some((piece.len(), width));
            }
        }
    }
    pieces.push(piece);
    pieces
}

/// Where each character of `text` starts once [`wrap`] has expanded its
/// tabs to `tab_width` columns, counted in characters, followed by where the
/// text ends.
pub fn expanded_offsets(text: &str, tab_width: usize) -> Vec<usize> {
    let tab_width = tab_width.max(1);
    let mut offsets = Vec::with_capacity(text.len() + 1);
    let (mut offset, mut column) = (0, 0);
    for c in text.chars() {
        offsets.push(offset);
        let (cells, count) = match c {
            '\t' => (1, tab_width - column % tab_width),
            c => (cell_width(c), 1),
        };
        column += cells * count;
        offset += count;
    }
    offsets.push(offset);
    offsets
}

/// How many columns `c` takes in a monospace font: two for wide East Asian
/// characters and emoji, none for combining marks.
fn cell_width(c: char) -> usize {
    match u32::from(c) {
        0x0300..=0x036F | 0x200B..=0x200F | 0xFE00..=0xFE0F => 0,
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F64F
        | 0x1F900..=0x1F9FF
        | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(kind: LineKind) -> DiffLine {
        DiffLine { kind, old_no: None, new_no: None, text: String::new() }
    }

    #[test]
    fn pairs_removed_lines_with_the_added_ones_after_them() {
        use LineKind::{Added as A, Context as C, Removed as R};
        let lines: Vec<DiffLine> = [C, R, R, A, C, A, R, C].into_iter().map(line).collect();
        assert_eq!(
            pairs(&lines),
            vec![
                (Some(0), Some(0)),
                (Some(1), Some(3)),
                (Some(2), None),
                (Some(4), Some(4)),
                (None, Some(5)),
                (Some(6), None),
                (Some(7), Some(7)),
            ]
        );
    }

    #[test]
    fn wraps_long_lines_after_spaces() {
        assert_eq!(wrap("", 10, 4), vec![""]);
        assert_eq!(wrap("short", 10, 4), vec!["short"]);
        assert_eq!(wrap("let value = compute();", 12, 4), vec!["let value = ", "compute();"]);
        // No space far enough in: break mid-word.
        assert_eq!(wrap("abcdefghij", 4, 4), vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn expands_tabs_and_counts_wide_characters() {
        assert_eq!(wrap("\tx", 10, 4), vec!["    x"]);
        assert_eq!(wrap("ab\tx", 10, 4), vec!["ab  x"]);
        assert_eq!(wrap("日本語", 4, 4), vec!["日本", "語"]);
    }

    #[test]
    fn maps_characters_past_expanded_tabs() {
        assert_eq!(expanded_offsets("", 4), [0]);
        assert_eq!(expanded_offsets("a\tb", 4), [0, 1, 4, 5]);
        assert_eq!(expanded_offsets("日\tb", 4), [0, 1, 3, 4]);
        let text = "\tif x:\tgo";
        let expanded: usize = wrap(text, 5, 4).iter().map(|p| p.chars().count()).sum();
        assert_eq!(expanded_offsets(text, 4).last(), Some(&expanded));
    }
}
