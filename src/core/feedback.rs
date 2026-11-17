//! Writes review comments as Markdown for a coding agent.

use std::fmt::Write as _;

use crate::core::review::{LineAnchor, Note, ReviewState};

const MAX_QUOTE_CHARS: usize = 120;

/// The unsent comments in the order they are posted: those on the product,
/// then those on each file under review, as `files` lists them, then those
/// on files no longer changed (reverted or committed), so none get lost.
pub fn unsent_order(review: &ReviewState, files: &[&str]) -> Vec<u64> {
    let unsent = |notes: &[Note]| notes.iter().filter(|n| n.sent_at.is_none()).map(|n| n.id).collect::<Vec<_>>();
    let others = review.files.keys().map(String::as_str).filter(|path| !files.contains(path));
    let mut ids = unsent(&review.product);
    for path in files.iter().copied().chain(others) {
        ids.extend(unsent(review.notes(path)));
    }
    ids
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
    use crate::core::review::Side;

    #[test]
    fn orders_unsent_comments_product_first() {
        let mut review = ReviewState::default();
        let main = review.add_note("src/main.rs", None, "Split this file.", None, 1);
        let gone = review.add_note("gone.rs", None, "Restore this.", None, 2);
        let lib = review.add_note("src/lib.rs", None, "Rename.", None, 3);
        let sent = review.add_note("src/lib.rs", None, "Old.", None, 4);
        review.set_sent(&[sent], Some(5));
        let page = review.add_product_note("http://localhost:5173/", None, "Bigger.", None, 6);
        assert_eq!(unsent_order(&review, &["src/lib.rs", "src/main.rs"]), [page, lib, main, gone]);
    }

    #[test]
    fn writes_comments() {
        let mut text = String::new();
        let anchor = LineAnchor { side: Side::New, line: 2, end_line: None, text: "+    let x = 1;".into() };
        write_comment(&mut text, Some(&anchor), "Use a descriptive name.\nAlso add a test.", " (id 1)");
        write_comment(&mut text, None, "Split this file.", "");
        write_page_comment(
            &mut text,
            Some("http://localhost:5173/pricing"),
            Some("/s/1.png"),
            "Make the plans\n\nline up.",
            "",
        );
        let expected = "\
- **Line 2** (id 1): `let x = 1;`
  Use a descriptive name.
  Also add a test.
- **File**
  Split this file.
- **Page**: `http://localhost:5173/pricing`
  Screenshot, drawn on: `/s/1.png`
  Make the plans

  line up.
";
        assert_eq!(text, expected);
    }

    #[test]
    fn quotes_multiple_lines_as_a_diff() {
        let mut text = String::new();
        let anchor = LineAnchor { side: Side::New, line: 1, end_line: Some(2), text: "+a\n+b".into() };
        write_comment(&mut text, Some(&anchor), "Simplify.", "");
        assert_eq!(text, "- **Lines 1–2**:\n  ```diff\n  +a\n  +b\n  ```\n  Simplify.\n");
    }
}
