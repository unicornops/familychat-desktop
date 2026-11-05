//! The rich-text AST (PLAN.md, section 5.8; ADR 0007): what a message's
//! text becomes before a frontend draws it.
//!
//! A message's `formatted_body` HTML never reaches a frontend. The session
//! runs it through `familychat-security`'s Matrix HTML sanitiser, which
//! reads the sanitised markup into this tree; a plain body becomes the same
//! tree through [`RichText::plain`], with the web addresses written in it
//! turned into links. The window draws each [`Block`] as styled text and
//! the terminal UI (Phase 10) as spans, so both draw the same structure and
//! neither ever sees markup.
//!
//! The tree is flat on purpose: a block is one run of text with styled
//! ranges, and nesting (quotes, lists) is a depth on the block. That is
//! what a styled-text element draws in one pass, and it cannot grow a deep
//! structure whatever the sender wrote.
//!
//! Text is the message's, so `Debug` prints sizes only (PLAN.md, section
//! 5.4).

use std::{fmt, ops::Range};

use crate::links::written_urls;

/// A bullet or a number in front of a list item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListMarker {
    /// An unordered list's bullet.
    Bullet,
    /// An ordered list's number.
    Number(u32),
}

impl ListMarker {
    /// The marker as text: "•" or "3.".
    pub fn label(self) -> String {
        match self {
            Self::Bullet => "•".to_owned(),
            Self::Number(number) => format!("{number}."),
        }
    }
}

/// What a block is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockKind {
    /// A paragraph (and anything else that is just text).
    Paragraph,
    /// A heading, level 1 to 6.
    Heading(u8),
    /// A list item at `depth` (1 for a top-level list).
    ListItem {
        /// How deeply the list is nested, from 1.
        depth: u8,
        /// Its bullet or number.
        marker: ListMarker,
    },
    /// Preformatted code: drawn in a monospace font, lines kept.
    Code,
    /// A horizontal rule (its text is empty).
    Rule,
}

/// How a range of a block's text is drawn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct SpanStyle {
    /// Bold (`<b>`, `<strong>`).
    pub bold: bool,
    /// Italic (`<i>`, `<em>`, `<cite>`).
    pub italic: bool,
    /// Underlined (`<u>`, `<ins>`).
    pub underline: bool,
    /// Struck through (`<s>`, `<del>`, `<strike>`).
    pub strikethrough: bool,
    /// Inline code (`<code>`).
    pub code: bool,
    /// A spoiler (`<span data-mx-spoiler>`): hidden until the reader asks.
    pub spoiler: bool,
    /// Raised or lowered text (`<sup>`, `<sub>`), drawn smaller.
    pub script: bool,
    /// A link: an index into [`RichText::links`].
    pub link: Option<usize>,
}

impl SpanStyle {
    /// Whether the style draws like plain text.
    pub fn is_plain(&self) -> bool {
        *self == Self::default()
    }
}

/// A styled range of a block's text.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Span {
    /// A byte range of [`Block::text`], on character boundaries.
    pub range: Range<usize>,
    /// How it is drawn.
    pub style: SpanStyle,
}

/// One block of text.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Block {
    /// What it is.
    pub kind: BlockKind,
    /// How many quotes (`<blockquote>`) it is inside.
    pub quote_depth: u8,
    /// Its text; `\n` breaks a line.
    pub text: String,
    /// Its styled ranges, in order and not overlapping. Text outside every
    /// span is plain.
    pub spans: Vec<Span>,
}

impl fmt::Debug for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Block")
            .field("kind", &self.kind)
            .field("quote_depth", &self.quote_depth)
            .field("text", &format_args!("{} bytes", self.text.len()))
            .field("spans", &self.spans)
            .finish()
    }
}

impl Block {
    /// A plain block of `kind`.
    pub fn new(kind: BlockKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            quote_depth: 0,
            text: text.into(),
            spans: Vec::new(),
        }
    }

    /// The block's text cut into ranges and their styles, covering all of
    /// it in order (plain text has the default style).
    pub fn runs(&self) -> Vec<(Range<usize>, SpanStyle)> {
        let mut runs = Vec::with_capacity(self.spans.len() * 2 + 1);
        let mut cursor = 0;
        for span in &self.spans {
            let start = span.range.start.clamp(cursor, self.text.len());
            let end = span.range.end.clamp(start, self.text.len());
            if start > cursor {
                runs.push((cursor..start, SpanStyle::default()));
            }
            if end > start {
                runs.push((start..end, span.style));
            }
            cursor = end;
        }
        if cursor < self.text.len() {
            runs.push((cursor..self.text.len(), SpanStyle::default()));
        }
        runs
    }
}

/// A link in a message.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct RichLink {
    /// Where it goes, as the sanitiser kept it: an `https`, `http`,
    /// `mailto` or `matrix` address. What opening it does is
    /// [`links::classify`](crate::links::classify)'s decision.
    pub href: String,
}

impl fmt::Debug for RichLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RichLink({} bytes)", self.href.len())
    }
}

/// A message's text, as both frontends draw it.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct RichText {
    /// The blocks, in order.
    pub blocks: Vec<Block>,
    /// The links the spans point at.
    pub links: Vec<RichLink>,
}

impl fmt::Debug for RichText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "RichText({} blocks, {} links)",
            self.blocks.len(),
            self.links.len()
        )
    }
}

impl RichText {
    /// A plain body: one paragraph per run of lines separated by a blank
    /// line, with the web and `mailto:` addresses written in it as links.
    pub fn plain(text: &str) -> Self {
        let mut rich = Self::default();
        let text = text.replace("\r\n", "\n");
        for paragraph in text.split("\n\n") {
            let paragraph = paragraph.trim_matches('\n');
            if paragraph.trim().is_empty() {
                continue;
            }
            let mut block = Block::new(BlockKind::Paragraph, paragraph);
            for range in written_urls(paragraph) {
                let link = rich.links.len();
                rich.links.push(RichLink {
                    href: paragraph[range.clone()].to_owned(),
                });
                block.spans.push(Span {
                    range,
                    style: SpanStyle {
                        link: Some(link),
                        ..SpanStyle::default()
                    },
                });
            }
            rich.blocks.push(block);
        }
        rich
    }

    /// Whether there is nothing to draw.
    pub fn is_empty(&self) -> bool {
        self.blocks.iter().all(|block| block.text.trim().is_empty())
            && !self
                .blocks
                .iter()
                .any(|block| block.kind == BlockKind::Rule)
    }

    /// The text without its styles, blocks separated by blank lines: what
    /// a screen reader reads, and what the terminal UI wraps.
    pub fn plain_text(&self) -> String {
        let mut text = String::new();
        for block in &self.blocks {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            if let BlockKind::ListItem { marker, .. } = block.kind {
                text.push_str(&marker.label());
                text.push(' ');
            }
            text.push_str(&block.text);
        }
        text
    }

    /// The link a span points at.
    pub fn link(&self, index: usize) -> Option<&RichLink> {
        self.links.get(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_body_keeps_its_paragraphs_and_finds_its_links() {
        let rich = RichText::plain("Hi!\nSee https://example.org/a.\n\n\nBye");
        assert_eq!(rich.blocks.len(), 2);
        assert_eq!(rich.blocks[0].text, "Hi!\nSee https://example.org/a.");
        assert_eq!(rich.links.len(), 1);
        assert_eq!(rich.links[0].href, "https://example.org/a");
        let span = &rich.blocks[0].spans[0];
        assert_eq!(
            &rich.blocks[0].text[span.range.clone()],
            "https://example.org/a"
        );
        assert_eq!(rich.plain_text(), "Hi!\nSee https://example.org/a.\n\nBye");
    }

    #[test]
    fn runs_cover_the_whole_block() {
        let mut block = Block::new(BlockKind::Paragraph, "one two three");
        block.spans.push(Span {
            range: 4..7,
            style: SpanStyle {
                bold: true,
                ..SpanStyle::default()
            },
        });
        let runs = block.runs();
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].0, 0..4);
        assert!(runs[1].1.bold);
        assert_eq!(runs[2].0, 7..13);
        // A span past the end is clamped, never a panic.
        block.spans.push(Span {
            range: 10..99,
            style: SpanStyle::default(),
        });
        assert_eq!(block.runs().last().map(|run| run.0.end), Some(13));
    }

    #[test]
    fn debug_never_prints_the_text() {
        let rich = RichText::plain("the secret plan at https://secret.example");
        let debug = format!("{rich:?} {:?} {:?}", rich.blocks, rich.links);
        assert!(!debug.contains("secret"), "{debug}");
    }

    #[test]
    fn empty_text_is_empty() {
        assert!(RichText::plain(" \n\n ").is_empty());
        assert!(!RichText::plain("x").is_empty());
        assert_eq!(ListMarker::Number(3).label(), "3.");
    }
}
