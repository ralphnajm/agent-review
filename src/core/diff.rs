//! Parsing of unified diffs as produced by `git diff`.

use crate::core::hash::Fnv64;

/// Untracked files larger than this are not read at all.
pub const MAX_UNTRACKED_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// The complete `@@ … @@ section` line.
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileDiff {
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub is_new: bool,
    pub is_deleted: bool,
    pub is_binary: bool,
    /// The file was too large to be read (untracked files only).
    pub is_oversized: bool,
    pub old_mode: Option<String>,
    pub new_mode: Option<String>,
    pub hunks: Vec<Hunk>,
    pub additions: u32,
    pub deletions: u32,
}

impl FileDiff {
    /// The path the change is listed under (`git status` uses the new path).
    pub fn path(&self) -> &str {
        self.new_path.as_deref().or(self.old_path.as_deref()).unwrap_or_default()
    }

    /// The previous path when the file was renamed or copied.
    pub fn renamed_from(&self) -> Option<&str> {
        match (&self.old_path, &self.new_path) {
            (Some(old), Some(new)) if old != new => Some(old),
            _ => None,
        }
    }

    pub fn line_count(&self) -> usize {
        self.hunks.iter().map(|h| h.lines.len() + 1).sum()
    }

    /// Stable hash of the change, used to detect edits after approval.
    pub fn fingerprint(&self) -> String {
        let mut hasher = Fnv64::new();
        for part in
            [self.old_path.as_deref(), self.new_path.as_deref(), self.old_mode.as_deref(), self.new_mode.as_deref()]
        {
            hasher.write(part.unwrap_or("-").as_bytes());
            hasher.write(b"\0");
        }
        hasher.write(&[u8::from(self.is_binary), u8::from(self.is_oversized)]);
        for hunk in &self.hunks {
            for line in &hunk.lines {
                hasher.write(match line.kind {
                    LineKind::Context => b" ",
                    LineKind::Added => b"+",
                    LineKind::Removed => b"-",
                });
                hasher.write(line.text.as_bytes());
                hasher.write(b"\n");
            }
        }
        hasher.hex()
    }

    /// All lines of all hunks, in order.
    pub fn lines(&self) -> impl Iterator<Item = &DiffLine> {
        self.hunks.iter().flat_map(|h| &h.lines)
    }

    /// Index (into [`Self::lines`]) of a line by its number on one side.
    /// The old side only matches removed lines, the new side the others.
    pub fn locate(&self, old_side: bool, number: u32) -> Option<usize> {
        self.lines().position(|line| {
            if old_side {
                line.kind == LineKind::Removed && line.old_no == Some(number)
            } else {
                line.kind != LineKind::Removed && line.new_no == Some(number)
            }
        })
    }

    /// The diff lines from `first` to `last` (numbered on one side), each
    /// prefixed with `+`, `-` or a space and joined with newlines.
    pub fn snippet(&self, old_side: bool, first: u32, last: u32) -> Option<String> {
        let start = self.locate(old_side, first)?;
        let end = self.locate(old_side, last)?;
        let (start, end) = (start.min(end), start.max(end));
        let lines: Vec<String> = self.lines().skip(start).take(end - start + 1).map(DiffLine::prefixed).collect();
        Some(lines.join("\n"))
    }
}

impl DiffLine {
    /// The line as it appears in a unified diff.
    pub fn prefixed(&self) -> String {
        let prefix = match self.kind {
            LineKind::Context => ' ',
            LineKind::Added => '+',
            LineKind::Removed => '-',
        };
        format!("{prefix}{}", self.text)
    }
}

/// Parses the output of `git diff` (with `a/` and `b/` prefixes).
pub fn parse(text: &str) -> Vec<FileDiff> {
    let mut files = Vec::new();
    let mut current: Option<FileDiff> = None;
    let (mut old_no, mut new_no) = (0_u32, 0_u32);

    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);

        if let Some(rest) = line.strip_prefix("diff --git ") {
            files.extend(current.take().map(finish));
            current = Some(header_paths(rest));
            continue;
        }
        let Some(file) = current.as_mut() else {
            continue;
        };

        if let Some(rest) = line.strip_prefix("@@ ") {
            let (old_start, new_start) = parse_hunk_ranges(rest);
            old_no = old_start;
            new_no = new_start;
            file.hunks.push(Hunk { header: line.to_owned(), lines: Vec::new() });
            continue;
        }

        if let Some(hunk) = file.hunks.last_mut() {
            let (kind, body) = match line.as_bytes().first() {
                Some(b'+') => (LineKind::Added, &line[1..]),
                Some(b'-') => (LineKind::Removed, &line[1..]),
                Some(b' ') => (LineKind::Context, &line[1..]),
                // `\ No newline at end of file`, or the trailing empty line.
                _ => continue,
            };
            let (old, new) = match kind {
                LineKind::Added => (None, Some(next(&mut new_no))),
                LineKind::Removed => (Some(next(&mut old_no)), None),
                LineKind::Context => (Some(next(&mut old_no)), Some(next(&mut new_no))),
            };
            hunk.lines.push(DiffLine { kind, old_no: old, new_no: new, text: body.to_owned() });
            continue;
        }

        parse_extended_header(line, file);
    }

    files.extend(current.map(finish));
    files
}

/// Builds a diff showing the whole of a new, untracked file as added.
pub fn untracked(path: &str, content: &[u8]) -> FileDiff {
    let mut diff = FileDiff { new_path: Some(path.to_owned()), is_new: true, ..FileDiff::default() };
    let sample = &content[..content.len().min(8000)];
    if sample.contains(&0) {
        diff.is_binary = true;
        return diff;
    }
    let text = String::from_utf8_lossy(content);
    let body = text.strip_suffix('\n').unwrap_or(&text);
    if content.is_empty() {
        return diff;
    }
    let lines: Vec<DiffLine> = body
        .split('\n')
        .zip(1..)
        .map(|(line, no)| DiffLine {
            kind: LineKind::Added,
            old_no: None,
            new_no: Some(no),
            text: line.strip_suffix('\r').unwrap_or(line).to_owned(),
        })
        .collect();
    diff.hunks.push(Hunk { header: format!("@@ -0,0 +1,{} @@", lines.len()), lines });
    finish(diff)
}

/// Placeholder for an untracked file that is too large to read.
pub fn oversized(path: &str) -> FileDiff {
    FileDiff { new_path: Some(path.to_owned()), is_new: true, is_oversized: true, ..FileDiff::default() }
}

fn next(counter: &mut u32) -> u32 {
    let value = *counter;
    *counter += 1;
    value
}

fn finish(mut file: FileDiff) -> FileDiff {
    let lines = file.hunks.iter().flat_map(|h| &h.lines);
    let (mut additions, mut deletions) = (0, 0);
    for line in lines {
        match line.kind {
            LineKind::Added => additions += 1,
            LineKind::Removed => deletions += 1,
            LineKind::Context => {}
        }
    }
    file.additions = additions;
    file.deletions = deletions;
    if file.is_new {
        file.old_path = None;
    }
    if file.is_deleted {
        file.new_path = None;
    }
    file
}

fn parse_extended_header(line: &str, file: &mut FileDiff) {
    if let Some(mode) = line.strip_prefix("new file mode ") {
        file.is_new = true;
        file.new_mode = Some(mode.to_owned());
    } else if let Some(mode) = line.strip_prefix("deleted file mode ") {
        file.is_deleted = true;
        file.old_mode = Some(mode.to_owned());
    } else if let Some(mode) = line.strip_prefix("old mode ") {
        file.old_mode = Some(mode.to_owned());
    } else if let Some(mode) = line.strip_prefix("new mode ") {
        file.new_mode = Some(mode.to_owned());
    } else if let Some(path) = line.strip_prefix("rename from ").or_else(|| line.strip_prefix("copy from ")) {
        file.old_path = Some(unquote(path));
    } else if let Some(path) = line.strip_prefix("rename to ").or_else(|| line.strip_prefix("copy to ")) {
        file.new_path = Some(unquote(path));
    } else if let Some(path) = line.strip_prefix("--- ") {
        if let Some(path) = marker_path(path, "a/") {
            file.old_path = Some(path);
        }
    } else if let Some(path) = line.strip_prefix("+++ ") {
        if let Some(path) = marker_path(path, "b/") {
            file.new_path = Some(path);
        }
    } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
        file.is_binary = true;
    }
}

/// Paths from the `diff --git a/… b/…` line. Only reliable when the path is
/// quoted or unchanged, which is when the later header lines are missing.
fn header_paths(rest: &str) -> FileDiff {
    let (old, new) = if rest.starts_with('"') {
        let (old, remainder) = take_quoted(rest);
        let remainder = remainder.trim_start();
        let new = if remainder.starts_with('"') { take_quoted(remainder).0 } else { remainder.to_owned() };
        (old, new)
    } else {
        // Unquoted and identical: "a/<p> b/<p>".
        let half = rest.len().saturating_sub(1) / 2;
        if rest.is_char_boundary(half) && rest.len() > half {
            (rest[..half].to_owned(), rest[half + 1..].to_owned())
        } else {
            (rest.to_owned(), rest.to_owned())
        }
    };
    FileDiff {
        old_path: Some(old.strip_prefix("a/").unwrap_or(&old).to_owned()),
        new_path: Some(new.strip_prefix("b/").unwrap_or(&new).to_owned()),
        ..FileDiff::default()
    }
}

fn marker_path(raw: &str, prefix: &str) -> Option<String> {
    let raw = raw.trim_end_matches('\t');
    if raw == "/dev/null" {
        return None;
    }
    let path = unquote(raw);
    Some(path.strip_prefix(prefix).unwrap_or(&path).to_owned())
}

fn parse_hunk_ranges(rest: &str) -> (u32, u32) {
    let mut parts = rest.split(' ');
    let start = |part: Option<&str>, sign: char| {
        part.and_then(|p| p.strip_prefix(sign))
            .and_then(|p| p.split(',').next())
            .and_then(|p| p.parse().ok())
            .unwrap_or(1)
    };
    let old = start(parts.next(), '-');
    let new = start(parts.next(), '+');
    (old, new)
}

/// Removes C-style quoting that git applies to unusual paths.
fn unquote(raw: &str) -> String {
    if raw.starts_with('"') { take_quoted(raw).0 } else { raw.to_owned() }
}

/// Parses a leading quoted string, returning it and the remaining input.
fn take_quoted(raw: &str) -> (String, &str) {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return (String::from_utf8_lossy(&out).into_owned(), &raw[i + 1..]),
            b'\\' if i + 1 < bytes.len() => {
                i += 1;
                match bytes[i] {
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    b'a' => out.push(0x07),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0c),
                    b'v' => out.push(0x0b),
                    digit @ b'0'..=b'7' => {
                        let mut value = u32::from(digit - b'0');
                        for _ in 0..2 {
                            match bytes.get(i + 1) {
                                Some(d @ b'0'..=b'7') => {
                                    value = value * 8 + u32::from(d - b'0');
                                    i += 1;
                                }
                                _ => break,
                            }
                        }
                        out.push(u8::try_from(value).unwrap_or(b'?'));
                    }
                    other => out.push(other),
                }
            }
            other => out.push(other),
        }
        i += 1;
    }
    (String::from_utf8_lossy(&out).into_owned(), "")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "diff --git a/src/main.rs b/src/main.rs
index 1111111..2222222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,4 +1,5 @@ fn main() {
 use std::io;
-fn old() {}
+fn new() {}
+fn newer() {}
\x20
 fn main() {}
@@ -10,2 +11,2 @@
-a
+b
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 3333333..0000000
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
\\ No newline at end of file
diff --git a/old name.rs b/new name.rs
similarity index 90%
rename from old name.rs
rename to new name.rs
diff --git a/logo.png b/logo.png
new file mode 100644
index 0000000..4444444
Binary files /dev/null and b/logo.png differ
diff --git \"a/caf\\303\\251.txt\" \"b/caf\\303\\251.txt\"
old mode 100644
new mode 100755
";

    #[test]
    fn parses_hunks_and_line_numbers() {
        let files = parse(SAMPLE);
        assert_eq!(files.len(), 5);
        let main = &files[0];
        assert_eq!(main.path(), "src/main.rs");
        assert_eq!((main.additions, main.deletions), (3, 2));
        assert_eq!(main.hunks.len(), 2);
        let lines = &main.hunks[0].lines;
        assert_eq!(
            lines[1],
            DiffLine { kind: LineKind::Removed, old_no: Some(2), new_no: None, text: "fn old() {}".into() }
        );
        assert_eq!(lines[3].new_no, Some(3));
        assert_eq!(
            lines[4],
            DiffLine { kind: LineKind::Context, old_no: Some(3), new_no: Some(4), text: String::new() }
        );
        assert_eq!(main.hunks[1].lines[1].new_no, Some(11));
        assert_eq!(main.locate(false, 3), Some(3));
        assert_eq!(main.locate(true, 2), Some(1));
        assert_eq!(main.snippet(false, 1, 3).unwrap(), " use std::io;\n-fn old() {}\n+fn new() {}\n+fn newer() {}");
    }

    #[test]
    fn handles_deletions_renames_binaries_and_modes() {
        let files = parse(SAMPLE);
        assert!(files[1].is_deleted);
        assert_eq!(files[1].path(), "gone.txt");
        assert_eq!(files[1].hunks[0].lines.len(), 1);

        assert_eq!(files[2].path(), "new name.rs");
        assert_eq!(files[2].renamed_from(), Some("old name.rs"));

        assert!(files[3].is_binary && files[3].is_new);
        assert_eq!(files[3].path(), "logo.png");

        assert_eq!(files[4].path(), "café.txt");
        assert_eq!(files[4].new_mode.as_deref(), Some("100755"));
    }

    #[test]
    fn fingerprint_tracks_content() {
        let a = parse(SAMPLE);
        let b = parse(&SAMPLE.replace("fn newer() {}", "fn newest() {}"));
        assert_eq!(a[0].fingerprint(), parse(SAMPLE)[0].fingerprint());
        assert_ne!(a[0].fingerprint(), b[0].fingerprint());
        assert_eq!(a[1].fingerprint(), b[1].fingerprint());
    }

    #[test]
    fn synthesizes_untracked_files() {
        let diff = untracked("notes.md", b"one\ntwo\n");
        assert_eq!(diff.additions, 2);
        assert_eq!(diff.hunks[0].header, "@@ -0,0 +1,2 @@");
        assert!(untracked("blob", b"\x00\x01").is_binary);
        assert_eq!(untracked("empty", b"").hunks, Vec::new());
    }
}
