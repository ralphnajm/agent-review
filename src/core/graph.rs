//! Lays out the commit history as a graph, one row per commit, the way
//! `git log --graph` does: each line of descent keeps to its own lane.

/// One row of the graph: the commit's dot, and the lines through the row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
    /// The lane of the commit's dot.
    pub lane: usize,
    /// Lines from the top of the row, in the first lane, to its middle in the
    /// second: from the commit's children to its dot, and lanes passing by.
    pub up: Vec<(usize, usize)>,
    /// Lines from the middle of the row, in the first lane, to its bottom in
    /// the second: from the dot to its parents, and lanes passing by.
    pub down: Vec<(usize, usize)>,
}

impl Row {
    /// How many lanes the row uses.
    pub fn width(&self) -> usize {
        let lines = self.up.iter().chain(&self.down).map(|&(from, to)| from.max(to));
        lines.fold(self.lane, usize::max) + 1
    }
}

/// Lays out `commits`, given as their id and parents, children first.
/// Parents missing from `commits` keep their lane to the bottom.
pub fn layout<'a, P: AsRef<str> + 'a>(commits: impl IntoIterator<Item = (&'a str, &'a [P])>) -> Vec<Row> {
    // The commit each lane leads down to, if any.
    let mut lanes: Vec<Option<&str>> = Vec::new();
    let mut rows = Vec::new();
    for (id, parents) in commits {
        let lane = lanes.iter().position(|l| *l == Some(id)).unwrap_or_else(|| free_lane(&mut lanes));
        let mut row = Row { lane, ..Row::default() };
        for (i, waiting) in lanes.iter_mut().enumerate() {
            match *waiting {
                Some(child_of) if child_of == id => {
                    row.up.push((i, lane));
                    *waiting = None;
                }
                Some(_) => {
                    row.up.push((i, i));
                    row.down.push((i, i));
                }
                None => {}
            }
        }
        for (n, parent) in parents.iter().map(AsRef::as_ref).enumerate() {
            let to = match lanes.iter().position(|l| *l == Some(parent)) {
                // The first parent, which a lane further right leads to: bring
                // that lane over, so the line of first parents stays straight.
                Some(existing) if n == 0 && existing > lane => {
                    if let Some(line) = row.down.iter_mut().find(|l| **l == (existing, existing)) {
                        line.1 = lane;
                    }
                    lanes[existing] = None;
                    lanes[lane] = Some(parent);
                    lane
                }
                // A parent another lane already leads to: join that lane.
                Some(existing) => existing,
                None => {
                    let to = if n == 0 { lane } else { free_lane(&mut lanes) };
                    lanes[to] = Some(parent);
                    to
                }
            };
            row.down.push((lane, to));
        }
        while lanes.last() == Some(&None) {
            lanes.pop();
        }
        rows.push(row);
    }
    rows
}

fn free_lane(lanes: &mut Vec<Option<&str>>) -> usize {
    lanes.iter().position(Option::is_none).unwrap_or_else(|| {
        lanes.push(None);
        lanes.len() - 1
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(commits: &[(&'static str, &[&'static str])]) -> Vec<Row> {
        layout(commits.iter().map(|&(id, parents)| (id, parents)))
    }

    #[test]
    fn a_straight_history_keeps_to_one_lane() {
        let rows = rows(&[("c", &["b"]), ("b", &["a"]), ("a", &[])]);
        assert_eq!(rows[0], Row { lane: 0, up: vec![], down: vec![(0, 0)] });
        assert_eq!(rows[1], Row { lane: 0, up: vec![(0, 0)], down: vec![(0, 0)] });
        assert_eq!(rows[2], Row { lane: 0, up: vec![(0, 0)], down: vec![] });
        assert!(rows.iter().all(|r| r.width() == 1));
    }

    #[test]
    fn a_branch_and_its_merge_take_a_second_lane() {
        // m merges f (on a branch from a) into b.
        let rows = rows(&[("m", &["b", "f"]), ("f", &["a"]), ("b", &["a"]), ("a", &[])]);
        assert_eq!(rows[0], Row { lane: 0, up: vec![], down: vec![(0, 0), (0, 1)] });
        assert_eq!(rows[1], Row { lane: 1, up: vec![(0, 0), (1, 1)], down: vec![(0, 0), (1, 1)] });
        // The branch's lane comes back to the first one at the fork.
        assert_eq!(rows[2], Row { lane: 0, up: vec![(0, 0), (1, 1)], down: vec![(1, 0), (0, 0)] });
        assert_eq!(rows[3], Row { lane: 0, up: vec![(0, 0)], down: vec![] });
        assert_eq!(rows[1].width(), 2);
    }

    #[test]
    fn branch_tips_open_new_lanes_and_freed_lanes_are_reused() {
        let rows = rows(&[("x", &["a"]), ("y", &["a"]), ("a", &["z"]), ("w", &[]), ("z", &[])]);
        assert_eq!(rows[1], Row { lane: 1, up: vec![(0, 0)], down: vec![(0, 0), (1, 0)] });
        assert_eq!(rows[2].lane, 0);
        // w has no children: it takes the first free lane.
        assert_eq!(rows[3], Row { lane: 1, up: vec![(0, 0)], down: vec![(0, 0)] });
        // Missing parents keep their lane open.
        let open = layout([("c", &["gone"][..])]);
        assert_eq!(open[0].down, [(0, 0)]);
    }
}
