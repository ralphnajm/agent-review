//! Aligns two versions of a sequence: which elements to keep, and which
//! stretches changed in between (Myers' diff algorithm).

/// Edits beyond which a changed stretch is treated as replaced wholesale,
/// bounding the work for unrelated sequences to O(LIMIT²).
const LIMIT: usize = 1_000;

/// One step from the old sequence to the new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Run {
    /// The next `n` elements are the same in both.
    Keep(usize),
    /// The next `delete` old elements become the next `insert` new ones.
    Change { delete: usize, insert: usize },
}

/// The runs turning `old` into `new`, keeping as many elements as possible.
pub fn runs<T: PartialEq>(old: &[T], new: &[T]) -> Vec<Run> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..].iter().rev().zip(new[prefix..].iter().rev()).take_while(|(a, b)| a == b).count();
    let (old_middle, new_middle) = (&old[prefix..old.len() - suffix], &new[prefix..new.len() - suffix]);

    let mut runs = Vec::new();
    push_keep(&mut runs, prefix);
    match keeps(old_middle, new_middle) {
        Some(steps) => {
            let (mut x, mut y) = (0, 0);
            for (keep_x, keep_y) in steps {
                push_change(&mut runs, keep_x - x, keep_y - y);
                push_keep(&mut runs, 1);
                (x, y) = (keep_x + 1, keep_y + 1);
            }
            push_change(&mut runs, old_middle.len() - x, new_middle.len() - y);
        }
        None => push_change(&mut runs, old_middle.len(), new_middle.len()),
    }
    push_keep(&mut runs, suffix);
    runs
}

fn push_keep(runs: &mut Vec<Run>, n: usize) {
    if n == 0 {
        return;
    }
    if let Some(Run::Keep(kept)) = runs.last_mut() {
        *kept += n;
    } else {
        runs.push(Run::Keep(n));
    }
}

fn push_change(runs: &mut Vec<Run>, delete: usize, insert: usize) {
    if delete > 0 || insert > 0 {
        runs.push(Run::Change { delete, insert });
    }
}

/// The positions `(old, new)` of the elements kept, in order, or `None` if
/// more than `LIMIT` edits are needed.
/// Names follow the paper: `x`/`n` index `old`, `y`/`m` index `new`, `d` counts edits.
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss, clippy::many_single_char_names)]
fn keeps<T: PartialEq>(old: &[T], new: &[T]) -> Option<Vec<(usize, usize)>> {
    let (n, m) = (old.len() as isize, new.len() as isize);
    let max = (old.len() + new.len()).min(LIMIT) as isize;
    // `v[k + offset]`: how far along `old` the furthest path on diagonal `k = x - y` reaches.
    let offset = max + 1;
    let mut v = vec![0_isize; 2 * offset as usize + 1];
    // The diagonals each step started from, `-(d + 1)..=d + 1`, for walking back.
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut reached = None;
    'search: for d in 0..=max {
        trace.push(v[(offset - d - 1) as usize..=(offset + d + 1) as usize].to_vec());
        for k in (-d..=d).step_by(2) {
            let i = (k + offset) as usize;
            let mut x = if k == -d || (k != d && v[i - 1] < v[i + 1]) { v[i + 1] } else { v[i - 1] + 1 };
            let mut y = x - k;
            while x < n && y < m && old[x as usize] == new[y as usize] {
                x += 1;
                y += 1;
            }
            v[i] = x;
            if x >= n && y >= m {
                reached = Some(d);
                break 'search;
            }
        }
    }
    reached?;

    let mut kept = Vec::new();
    let (mut x, mut y) = (n, m);
    for (d, start) in trace.iter().enumerate().rev() {
        let d = d as isize;
        let at = |k: isize| start[(k + d + 1) as usize];
        let k = x - y;
        let previous = if k == -d || (k != d && at(k - 1) < at(k + 1)) { k + 1 } else { k - 1 };
        let (previous_x, previous_y) = (at(previous), at(previous) - previous);
        while x > previous_x && y > previous_y {
            x -= 1;
            y -= 1;
            kept.push((x as usize, y as usize));
        }
        (x, y) = (previous_x, previous_y);
    }
    kept.reverse();
    Some(kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Applies `runs` to `old`, taking changed elements from `new`.
    fn apply<'a>(old: &[&'a str], new: &[&'a str], runs: &[Run]) -> Vec<&'a str> {
        let (mut x, mut y, mut out) = (0, 0, Vec::new());
        for run in runs {
            match *run {
                Run::Keep(n) => {
                    assert_eq!(old[x..x + n], new[y..y + n]);
                    out.extend_from_slice(&old[x..x + n]);
                    (x, y) = (x + n, y + n);
                }
                Run::Change { delete, insert } => {
                    out.extend_from_slice(&new[y..y + insert]);
                    (x, y) = (x + delete, y + insert);
                }
            }
        }
        assert_eq!((x, y), (old.len(), new.len()));
        out
    }

    fn kept(runs: &[Run]) -> usize {
        runs.iter().map(|r| if let Run::Keep(n) = r { *n } else { 0 }).sum()
    }

    #[test]
    fn keeps_identical_sequences() {
        let lines = ["a", "b", "c"];
        assert_eq!(runs(&lines, &lines), [Run::Keep(3)]);
        assert_eq!(runs::<&str>(&[], &[]), []);
    }

    #[test]
    fn finds_insertions_and_deletions() {
        let old = ["a", "b", "c", "d"];
        assert_eq!(runs(&old, &["x", "a", "b", "c", "d"]), [Run::Change { delete: 0, insert: 1 }, Run::Keep(4)]);
        assert_eq!(runs(&old, &["a", "d"]), [Run::Keep(1), Run::Change { delete: 2, insert: 0 }, Run::Keep(1)]);
        assert_eq!(
            runs(&old, &["a", "x", "c", "d"]),
            [Run::Keep(1), Run::Change { delete: 1, insert: 1 }, Run::Keep(2)]
        );
        assert_eq!(runs(&old, &[]), [Run::Change { delete: 4, insert: 0 }]);
    }

    #[test]
    fn keeps_the_longest_common_subsequence() {
        let old = ["a", "b", "c", "a", "b", "b", "a"];
        let new = ["c", "b", "a", "b", "a", "c"];
        let runs = runs(&old, &new);
        assert_eq!(kept(&runs), 4);
        assert_eq!(apply(&old, &new, &runs), new);
    }

    #[test]
    fn keeps_lines_around_scattered_edits() {
        let old: Vec<String> = (0..500).map(|i| format!("line {i}")).collect();
        let mut new = old.clone();
        new.insert(10, "new".into());
        new.remove(200);
        new[300] = "changed".into();
        new.push("end".into());
        let (old, new): (Vec<&str>, Vec<&str>) =
            (old.iter().map(String::as_str).collect(), new.iter().map(String::as_str).collect());
        let runs = runs(&old, &new);
        assert_eq!(kept(&runs), 498);
        assert_eq!(apply(&old, &new, &runs), new);
    }

    #[test]
    fn replaces_wholesale_past_the_limit() {
        let old: Vec<usize> = (0..LIMIT).collect();
        let new: Vec<usize> = (LIMIT..3 * LIMIT).collect();
        assert_eq!(runs(&old, &new), [Run::Change { delete: LIMIT, insert: 2 * LIMIT }]);
    }
}
