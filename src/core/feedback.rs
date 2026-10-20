//! Turns review notes into a Markdown prompt for a coding agent.

use std::fmt::Write as _;

use crate::core::diff::FileDiff;
use crate::core::review::{ApprovalState, LineAnchor, Note, ReviewState, ReviewedCommit};

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Free-form text placed before the per-file notes.
    pub summary: String,
    /// Also include notes that were already copied or sent.
    pub include_sent: bool,
    /// List the files that were approved.
    pub list_approved: bool,
    /// The commit reviewed; `None` for the uncommitted changes.
    pub commit: Option<ReviewedCommit>,
}

/// A file currently under review.
#[derive(Debug, Clone, Copy)]
pub struct FileInfo<'a> {
    pub path: &'a str,
    pub diff: Option<&'a FileDiff>,
    pub approval: ApprovalState,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Feedback {
    pub text: String,
    /// Notes included in `text`, to be marked as sent.
    pub note_ids: Vec<u64>,
    pub files_with_notes: usize,
    pub approved: usize,
}

impl Feedback {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

const MAX_QUOTE_CHARS: usize = 120;

pub fn build(review: &ReviewState, files: &[FileInfo<'_>], options: &Options) -> Feedback {
    let mut feedback = Feedback::default();
    let mut sections = String::new();

    let wanted = |note: &&Note| options.include_sent || note.sent_at.is_none();

    // Files under review first, in list order; then notes on files that are
    // no longer changed (e.g. reverted or committed) so nothing gets lost.
    let mut ordered: Vec<(&str, Option<&FileDiff>, bool)> = files.iter().map(|f| (f.path, f.diff, true)).collect();
    for path in review.files.keys() {
        if !files.iter().any(|f| f.path == path) {
            ordered.push((path, None, false));
        }
    }

    let product: Vec<&Note> = review.product.iter().filter(wanted).collect();
    if !product.is_empty() {
        sections.push_str("## The running product\n\n");
        for note in product {
            feedback.note_ids.push(note.id);
            write_page_comment(&mut sections, note.page.as_deref(), note.screenshot.as_deref(), &note.body, "");
        }
        sections.push('\n');
    }

    for (path, diff, current) in ordered {
        let notes: Vec<&Note> = review.notes(path).iter().filter(wanted).collect();
        if notes.is_empty() {
            continue;
        }
        feedback.files_with_notes += 1;
        let suffix = if current { "" } else { " (no longer changed)" };
        let _ = writeln!(sections, "## `{path}`{suffix}\n");
        for note in notes {
            feedback.note_ids.push(note.id);
            write_note(&mut sections, note, diff);
        }
        sections.push('\n');
    }

    // Reviewed files with comments are covered above; the rest need no changes.
    let approved: Vec<&str> = files
        .iter()
        .filter(|f| f.approval == ApprovalState::Approved && !review.notes(f.path).iter().any(|n| wanted(&n)))
        .map(|f| f.path)
        .collect();
    feedback.approved = approved.len();

    let summary = options.summary.trim();
    if feedback.note_ids.is_empty() && approved.is_empty() && summary.is_empty() {
        return feedback;
    }

    let reviewed = options.commit.as_ref().map_or_else(
        || "the current uncommitted changes".to_owned(),
        |c| {
            let what = if c.base.is_some() { "version" } else { "commit" };
            format!("{what} {} ({})", c.short_id(), c.subject)
        },
    );
    let mut text = format!("I reviewed {reviewed}. ");
    if feedback.note_ids.is_empty() {
        text.push_str("No changes requested.\n\n");
    } else {
        text.push_str("Please address the following notes.\n\n");
    }
    if !summary.is_empty() {
        let _ = writeln!(text, "{summary}\n");
    }
    text.push_str(&sections);

    if !approved.is_empty() && (options.list_approved || feedback.note_ids.is_empty()) {
        text.push_str("## Reviewed\n\nThese files look good, no changes needed:\n");
        for path in &approved {
            let _ = writeln!(text, "- `{path}`");
        }
    }

    feedback.text = text.trim_end().to_owned() + "\n";
    feedback
}

fn write_note(out: &mut String, note: &Note, diff: Option<&FileDiff>) {
    let changed = note.anchor.as_ref().is_some_and(|a| !a.is_current(diff));
    write_comment(out, note.anchor.as_ref(), &note.body, if changed { " (changed since)" } else { "" });
}

/// Writes one comment as a Markdown list item; `tag` follows its label.
pub fn write_comment(out: &mut String, anchor: Option<&LineAnchor>, body: &str, tag: &str) {
    match anchor {
        Some(anchor) => {
            let mut lines = anchor.text.lines();
            if let (Some(only), None) = (lines.next(), lines.next()) {
                let code = code_span(only.get(1..).unwrap_or_default());
                let _ = writeln!(out, "- **{}**{tag}: {code}", anchor.label());
            } else {
                let _ = writeln!(out, "- **{}**{tag}:", anchor.label());
                out.push_str("  ```diff\n");
                for line in anchor.text.lines() {
                    let _ = writeln!(out, "  {line}");
                }
                out.push_str("  ```\n");
            }
        }
        None => {
            let _ = writeln!(out, "- **File**{tag}");
        }
    }
    write_body(out, body);
}

/// Writes one comment on the running product, on the page at `page`, with
/// the screenshot the user drew on, as a Markdown list item; `tag` follows its
/// label.
pub fn write_page_comment(out: &mut String, page: Option<&str>, screenshot: Option<&str>, body: &str, tag: &str) {
    match page {
        Some(page) => {
            let _ = writeln!(out, "- **Page**{tag}: {}", code_span(page));
        }
        None => {
            let _ = writeln!(out, "- **Product**{tag}");
        }
    }
    if let Some(screenshot) = screenshot {
        let _ = writeln!(out, "  Screenshot, drawn on: `{screenshot}`");
    }
    write_body(out, body);
}

fn write_body(out: &mut String, body: &str) {
    for line in body.lines() {
        if line.trim().is_empty() {
            out.push('\n');
        } else {
            let _ = writeln!(out, "  {line}");
        }
    }
}

fn code_span(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "(blank line)".to_owned();
    }
    let mut shown: String = trimmed.chars().take(MAX_QUOTE_CHARS).collect();
    if shown.len() < trimmed.len() {
        shown.push('…');
    }
    if shown.contains('`') { format!("`` {shown} ``") } else { format!("`{shown}`") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::diff;
    use crate::core::review::Side;

    fn sample_diff() -> FileDiff {
        diff::untracked("src/main.rs", b"fn main() {\n    let x = 1;\n}\n")
    }

    #[test]
    fn renders_notes_and_approvals() {
        let diff = sample_diff();
        let mut review = ReviewState::default();
        let anchor = LineAnchor { side: Side::New, line: 2, end_line: None, text: "+    let x = 1;".into() };
        review.add_note("src/main.rs", Some(anchor), "Use a descriptive name.\nAlso add a test.", None, 1);
        let range = LineAnchor::covering(&diff, 0, 2).unwrap();
        review.add_note("src/main.rs", Some(range), "Simplify.", None, 2);
        review.add_note("src/main.rs", None, "Split this file.", None, 2);
        review.add_note("gone.rs", None, "Restore this.", None, 3);
        review.add_note("src/extra.rs", None, "Rename.", None, 4);
        let files = [
            FileInfo { path: "src/main.rs", diff: Some(&diff), approval: ApprovalState::Unreviewed },
            FileInfo { path: "src/lib.rs", diff: None, approval: ApprovalState::Approved },
            // Reviewed but commented on: listed with its comments, not as "no changes needed".
            FileInfo { path: "src/extra.rs", diff: None, approval: ApprovalState::Approved },
        ];

        let options = Options { summary: "Mostly good.".into(), list_approved: true, ..Options::default() };
        let feedback = build(&review, &files, &options);
        assert_eq!(feedback.note_ids.len(), 5);
        assert_eq!(feedback.files_with_notes, 3);
        let expected = "\
I reviewed the current uncommitted changes. Please address the following notes.

Mostly good.

## `src/main.rs`

- **Line 2**: `let x = 1;`
  Use a descriptive name.
  Also add a test.
- **Lines 1–3**:
  ```diff
  +fn main() {
  +    let x = 1;
  +}
  ```
  Simplify.
- **File**
  Split this file.

## `src/extra.rs`

- **File**
  Rename.

## `gone.rs` (no longer changed)

- **File**
  Restore this.

## Reviewed

These files look good, no changes needed:
- `src/lib.rs`
";
        assert_eq!(feedback.text, expected);
    }

    #[test]
    fn skips_sent_notes_unless_requested() {
        let mut review = ReviewState::default();
        let id = review.add_note("a.rs", None, "Fix", None, 1);
        review.set_sent(&[id], Some(2));
        let files = [FileInfo { path: "a.rs", diff: None, approval: ApprovalState::Unreviewed }];
        assert!(build(&review, &files, &Options::default()).is_empty());
        let all = build(&review, &files, &Options { include_sent: true, ..Options::default() });
        assert_eq!(all.note_ids, [id]);
    }

    #[test]
    fn marks_outdated_anchors() {
        let diff = sample_diff();
        let mut review = ReviewState::default();
        let anchor = LineAnchor { side: Side::New, line: 2, end_line: None, text: "+let y = 2;".into() };
        review.add_note("src/main.rs", Some(anchor), "Hmm", None, 1);
        let files = [FileInfo { path: "src/main.rs", diff: Some(&diff), approval: ApprovalState::Unreviewed }];
        let text = build(&review, &files, &Options::default()).text;
        assert!(text.contains("**Line 2** (changed since): `let y = 2;`"));
    }

    #[test]
    fn names_the_commit_reviewed() {
        let mut review = ReviewState::default();
        review.add_note("a.rs", None, "Fix", None, 1);
        let files = [FileInfo { path: "a.rs", diff: None, approval: ApprovalState::Unreviewed }];
        let commit =
            ReviewedCommit { id: "0123456789abcdef".into(), subject: "Add a.rs".into(), base: None, request: None };
        let text = build(&review, &files, &Options { commit: Some(commit.clone()), ..Options::default() }).text;
        assert!(text.starts_with("I reviewed commit 0123456 (Add a.rs). Please address the following notes."));
        let version = ReviewedCommit { base: Some("fedcba9876543210".into()), ..commit };
        let text = build(&review, &files, &Options { commit: Some(version), ..Options::default() }).text;
        assert!(text.starts_with("I reviewed version 0123456 (Add a.rs)."));
    }

    #[test]
    fn product_comments_come_first() {
        let mut review = ReviewState::default();
        review.add_note("a.rs", None, "Fix", None, 1);
        review.add_product_note(
            "http://localhost:5173/pricing",
            Some("/s/1.png"),
            "Make the plans\n\nline up.",
            None,
            2,
        );
        let files = [FileInfo { path: "a.rs", diff: None, approval: ApprovalState::Unreviewed }];
        let feedback = build(&review, &files, &Options::default());
        assert_eq!(feedback.note_ids, [2, 1]);
        assert!(feedback.text.contains(
            "## The running product\n\n- **Page**: `http://localhost:5173/pricing`\n  Screenshot, drawn on: `/s/1.png`\n  \
             Make the plans\n\n  line up.\n\n## `a.rs`"
        ), "{}", feedback.text);
    }

    #[test]
    fn approvals_alone_produce_a_go_ahead() {
        let review = ReviewState::default();
        let files = [FileInfo { path: "a.rs", diff: None, approval: ApprovalState::Approved }];
        let text = build(&review, &files, &Options::default()).text;
        assert!(text.starts_with("I reviewed the current uncommitted changes. No changes requested."));
        assert!(text.contains("- `a.rs`"));
    }
}
