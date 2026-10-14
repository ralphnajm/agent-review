//! Syntax highlighting of one side of a diff, read as the file it is: the
//! diff's buffer mixes removed and added lines, and side by side both halves
//! share a line, so highlighting it as it is carries strings and comments
//! across from one version into the other.
//!
//! Each side is highlighted in a hidden buffer of its own, which shares its
//! tag table with the diff's so the tags can be copied over. Like a source
//! view, it highlights only the lines asked for, analysing the rest in the
//! background. Its tags follow the style scheme set on it.

use std::cell::RefCell;

use gtk::prelude::*;
use sourceview5::prelude::*;

/// A highlighting tag on the characters `start..end` of a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub tag: gtk::TextTag,
}

pub struct Highlighter {
    buffer: sourceview5::Buffer,
    /// The text highlighted, one line per numbered line.
    text: RefCell<String>,
    /// The number of each line in the buffer, ascending.
    numbers: RefCell<Vec<u32>>,
    /// The tags spans have used, to take them off again.
    tags: RefCell<Vec<gtk::TextTag>>,
}

impl Highlighter {
    /// Highlights with tags in `table`.
    pub fn new(table: &gtk::TextTagTable) -> Self {
        let buffer = sourceview5::Buffer::new(Some(table));
        buffer.set_highlight_matching_brackets(false);
        buffer.set_enable_undo(false);
        Self { buffer, text: RefCell::default(), numbers: RefCell::default(), tags: RefCell::default() }
    }

    pub fn set_style_scheme(&self, scheme: Option<&sourceview5::StyleScheme>) {
        self.buffer.set_style_scheme(scheme);
    }

    /// Highlights `lines`, numbered and in order, in `language`.
    pub fn set(&self, language: Option<&sourceview5::Language>, lines: &[(u32, &str)]) {
        let mut text = String::with_capacity(lines.iter().map(|(_, line)| line.len() + 1).sum());
        for (_, line) in lines {
            text.push_str(line);
            text.push('\n');
        }
        if self.buffer.language().as_ref() != language {
            self.buffer.set_language(language);
        }
        if *self.text.borrow() != text {
            self.buffer.set_text(&text);
            self.text.replace(text);
        }
        // Lines that hold a line break of their own, such as a lone `\r`,
        // would put the others out of step: highlight nothing then.
        let in_step = usize::try_from(self.buffer.line_count()).ok() == Some(lines.len() + 1);
        self.numbers.replace(if in_step { lines.iter().map(|(n, _)| *n).collect() } else { Vec::new() });
    }

    /// The start and end of the line numbered `number`, if it was given.
    fn line_bounds(&self, number: u32) -> Option<(gtk::TextIter, gtk::TextIter)> {
        let line = self.numbers.borrow().binary_search(&number).ok()?;
        let start = self.buffer.iter_at_line(i32::try_from(line).ok()?)?;
        let mut end = start;
        if !end.ends_line() {
            end.forward_to_line_end();
        }
        Some((start, end))
    }

    /// The text of the line numbered `number`, if it was given.
    pub fn line(&self, number: u32) -> Option<String> {
        let (start, end) = self.line_bounds(number)?;
        Some(self.buffer.text(&start, &end, true).into())
    }

    /// The highlighting of the line numbered `number`, if it was given.
    pub fn spans(&self, number: u32) -> Vec<Span> {
        let Some((mut iter, end)) = self.line_bounds(number) else { return Vec::new() };
        self.buffer.ensure_highlight(&iter, &end);
        let mut spans = Vec::new();
        while iter < end {
            let start = iter;
            if !iter.forward_to_tag_toggle(None::<&gtk::TextTag>) || iter > end {
                iter = end;
            }
            let (from, to) = (offset(&start), offset(&iter));
            spans.extend(start.tags().into_iter().map(|tag| Span { start: from, end: to, tag }));
        }
        let mut seen = self.tags.borrow_mut();
        for span in &spans {
            if !seen.contains(&span.tag) {
                seen.push(span.tag.clone());
            }
        }
        spans
    }

    /// The tags the spans so far have used.
    pub fn tags(&self) -> Vec<gtk::TextTag> {
        self.tags.borrow().clone()
    }
}

fn offset(iter: &gtk::TextIter) -> usize {
    usize::try_from(iter.line_offset()).unwrap_or_default()
}
