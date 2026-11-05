//! The rich-text tree from sanitised markup: a tokenizer pass over
//! ammonia's output (well formed, allowlisted) that builds
//! `familychat-core`'s [`RichText`].
//!
//! It never builds a DOM: it keeps a stack of the open elements (exact for
//! well-formed markup) and the styles they give, and cuts the text into
//! flat blocks:
//!
//! - `p`, `div`, `h1` to `h6`, `pre`, `caption`, `details`, `summary` and
//!   each table row start a block; `li` starts a list item with its bullet
//!   or number and its depth; `hr` is a rule; `blockquote` raises the
//!   quote depth of the blocks inside it.
//! - A table's cells on one row are joined with " · ", and header cells
//!   are bold.
//! - `<br>` breaks the line; whitespace collapses outside `pre`.
//! - `<img>` (an `mxc://` custom emoji or picture, which this phase does
//!   not fetch) becomes its alt text.
//! - Control characters go (outside `pre`, tabs and newlines are spaces),
//!   and so do the bidirectional embeddings, overrides and isolates
//!   (U+202A to U+202E, U+2066 to U+2069), which could make a link's text
//!   read in another order than it is; the plain marks (U+200E, U+200F)
//!   and the zero-width joiner emoji need stay.
//!
//! The output is capped: [`MAX_BLOCKS`] blocks, [`MAX_TEXT_BYTES`] of text
//! and [`MAX_LINKS`] links; past a cap the text ends with "…".

use std::cell::RefCell;

use familychat_core::rich_text::{
    Block, BlockKind, ListMarker, RichLink, RichText, Span, SpanStyle,
};
use html5ever::tokenizer::{Tag, TagKind, Token, TokenSink, TokenSinkResult};

use super::{MAX_DEPTH, tokenize};

/// The most blocks a message's text may have.
pub const MAX_BLOCKS: usize = 512;
/// The most bytes of text a message's tree may hold.
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
/// The most links a message's tree may hold; later links are plain text.
pub const MAX_LINKS: usize = 256;
/// The deepest quote or list a block records; deeper ones draw at this.
pub const MAX_NESTING_SHOWN: u8 = 8;

/// The separator between a table row's cells.
pub const CELL_SEPARATOR: &str = " · ";

/// Characters removed from message text: the bidirectional embeddings,
/// overrides and isolates.
pub fn is_reordering_char(ch: char) -> bool {
    matches!(ch, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Builds the tree of sanitised `html`; `None` when it nests deeper than
/// [`MAX_DEPTH`].
pub(crate) fn build(html: &str) -> Option<RichText> {
    let sink = TreeSink {
        state: RefCell::new(Builder::default()),
    };
    let builder = tokenize(html, sink).state.into_inner();
    builder.finish()
}

struct TreeSink {
    state: RefCell<Builder>,
}

impl TokenSink for TreeSink {
    type Handle = ();

    fn process_token(&self, token: Token, _line_number: u64) -> TokenSinkResult<()> {
        let mut state = self.state.borrow_mut();
        if state.full {
            return TokenSinkResult::Continue;
        }
        match token {
            Token::TagToken(tag) if tag.kind == TagKind::StartTag => state.start_tag(&tag),
            Token::TagToken(tag) => state.end_tag(&tag.name),
            Token::CharacterTokens(text) => state.push_text(&text),
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

/// An open element and what it gives the text inside it.
#[derive(Debug)]
struct Open {
    name: String,
    style: Option<StyleFlag>,
    /// For `ol` and `ul`: the next number (`None` for bullets).
    list: Option<Option<u32>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StyleFlag {
    Bold,
    Italic,
    Underline,
    Strikethrough,
    Code,
    Spoiler,
    Script,
    Link(usize),
}

#[derive(Default)]
struct Builder {
    open: Vec<Open>,
    deepest: usize,
    rich: RichText,
    /// The block being written.
    block: Option<Block>,
    /// The kind the next block takes (a list item's marker, a heading).
    pending_kind: Option<BlockKind>,
    pending_space: bool,
    pending_cell: bool,
    text_bytes: usize,
    /// A cap was reached: the rest is ignored.
    full: bool,
}

impl Builder {
    fn start_tag(&mut self, tag: &Tag) {
        let name: &str = &tag.name;
        match name {
            "p" | "div" | "caption" | "details" | "summary" | "table" | "blockquote" | "ul"
            | "ol" => self.end_block(),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.end_block();
                let level = name[1..].parse().unwrap_or(1);
                self.pending_kind = Some(BlockKind::Heading(level));
            }
            "pre" => {
                self.end_block();
                self.pending_kind = Some(BlockKind::Code);
            }
            "tr" => self.end_block(),
            "td" | "th" => {
                if self
                    .block
                    .as_ref()
                    .is_some_and(|block| !block.text.is_empty())
                {
                    self.pending_cell = true;
                }
            }
            "li" => {
                self.end_block();
                let depth = self
                    .open
                    .iter()
                    .filter(|open| open.list.is_some())
                    .count()
                    .clamp(1, usize::from(MAX_NESTING_SHOWN));
                let marker = match self
                    .open
                    .iter_mut()
                    .rev()
                    .find_map(|open| open.list.as_mut())
                {
                    Some(Some(number)) => {
                        let marker = ListMarker::Number(*number);
                        *number = number.saturating_add(1);
                        marker
                    }
                    _ => ListMarker::Bullet,
                };
                self.pending_kind = Some(BlockKind::ListItem {
                    depth: u8::try_from(depth).unwrap_or(MAX_NESTING_SHOWN),
                    marker,
                });
            }
            "hr" => {
                self.end_block();
                self.push_block(Block {
                    quote_depth: self.quote_depth(),
                    ..Block::new(BlockKind::Rule, "")
                });
                return;
            }
            "br" => {
                if let Some(block) = &mut self.block
                    && !block.text.is_empty()
                    && !block.text.ends_with("\n\n")
                {
                    block.text.push('\n');
                    self.text_bytes += 1;
                }
                self.pending_space = false;
                return;
            }
            "img" => {
                if let Some(alt) = attribute(tag, "alt").or_else(|| attribute(tag, "title")) {
                    let alt = alt.to_owned();
                    self.push_text(&alt);
                }
                return;
            }
            _ => {}
        }
        if tag.self_closing {
            return;
        }
        let style = match name {
            "b" | "strong" | "th" => Some(StyleFlag::Bold),
            "i" | "em" => Some(StyleFlag::Italic),
            "u" => Some(StyleFlag::Underline),
            "s" | "del" => Some(StyleFlag::Strikethrough),
            "code" if !self.in_pre() => Some(StyleFlag::Code),
            "sup" | "sub" => Some(StyleFlag::Script),
            "span" if attribute(tag, "data-mx-spoiler").is_some() => Some(StyleFlag::Spoiler),
            "a" => attribute(tag, "href")
                .filter(|_| self.rich.links.len() < MAX_LINKS)
                .map(|href| {
                    self.rich.links.push(RichLink {
                        href: href.to_owned(),
                    });
                    StyleFlag::Link(self.rich.links.len() - 1)
                }),
            _ => None,
        };
        let list = match name {
            "ol" => Some(Some(
                attribute(tag, "start")
                    .and_then(|start| start.trim().parse::<u32>().ok())
                    .unwrap_or(1),
            )),
            "ul" => Some(None),
            _ => None,
        };
        self.open.push(Open {
            name: name.to_owned(),
            style,
            list,
        });
        self.deepest = self.deepest.max(self.open.len());
    }

    fn end_tag(&mut self, name: &str) {
        let Some(index) = self.open.iter().rposition(|open| open.name == name) else {
            return;
        };
        self.open.truncate(index);
        match name {
            "p" | "div" | "caption" | "details" | "summary" | "table" | "blockquote" | "ul"
            | "ol" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "pre" | "tr" | "li" => {
                self.end_block();
            }
            _ => {}
        }
    }

    fn in_pre(&self) -> bool {
        self.open.iter().any(|open| open.name == "pre")
    }

    fn quote_depth(&self) -> u8 {
        let depth = self
            .open
            .iter()
            .filter(|open| open.name == "blockquote")
            .count();
        u8::try_from(depth)
            .unwrap_or(MAX_NESTING_SHOWN)
            .min(MAX_NESTING_SHOWN)
    }

    fn style(&self) -> SpanStyle {
        let mut style = SpanStyle::default();
        for flag in self.open.iter().filter_map(|open| open.style) {
            match flag {
                StyleFlag::Bold => style.bold = true,
                StyleFlag::Italic => style.italic = true,
                StyleFlag::Underline => style.underline = true,
                StyleFlag::Strikethrough => style.strikethrough = true,
                StyleFlag::Code => style.code = true,
                StyleFlag::Spoiler => style.spoiler = true,
                StyleFlag::Script => style.script = true,
                StyleFlag::Link(index) => style.link = Some(index),
            }
        }
        style
    }

    fn push_text(&mut self, chunk: &str) {
        let pre = self.in_pre();
        let style = self.style();
        let mut piece = String::new();
        for ch in chunk.chars() {
            if ch == '\r' || is_reordering_char(ch) {
                continue;
            }
            if pre && (ch == '\n' || ch == '\t') {
                piece.push(ch);
            } else if ch.is_whitespace() || ch.is_control() {
                if !pre {
                    self.flush_piece(&mut piece, style);
                    self.pending_space = true;
                } else if ch == ' ' || !ch.is_control() {
                    piece.push(' ');
                }
            } else {
                piece.push(ch);
            }
        }
        self.flush_piece(&mut piece, style);
    }

    /// Writes `piece` in `style` to the block, starting one if needed.
    fn flush_piece(&mut self, piece: &mut String, style: SpanStyle) {
        if piece.is_empty() || self.full {
            piece.clear();
            return;
        }
        if self.block.is_none() {
            let kind = self.pending_kind.take().unwrap_or_else(|| {
                if self.in_pre() {
                    BlockKind::Code
                } else {
                    BlockKind::Paragraph
                }
            });
            if kind == BlockKind::Code && piece.starts_with('\n') {
                piece.remove(0);
            }
            self.block = Some(Block {
                quote_depth: self.quote_depth(),
                ..Block::new(kind, "")
            });
            self.pending_space = false;
            self.pending_cell = false;
        }
        let mut separator = String::new();
        if let Some(block) = &self.block
            && !block.text.is_empty()
        {
            if self.pending_cell {
                separator.push_str(CELL_SEPARATOR);
            } else if self.pending_space && !block.text.ends_with('\n') {
                separator.push(' ');
            }
        }
        self.pending_space = false;
        self.pending_cell = false;
        let budget = MAX_TEXT_BYTES.saturating_sub(self.text_bytes);
        let mut text = std::mem::take(piece);
        if separator.len() + text.len() > budget {
            let mut end = budget.saturating_sub(separator.len());
            while end > 0 && !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            text.push('…');
            self.full = true;
        }
        let Some(block) = &mut self.block else {
            return;
        };
        block.text.push_str(&separator);
        let start = block.text.len();
        block.text.push_str(&text);
        let end = block.text.len();
        self.text_bytes += separator.len() + text.len();
        if style.is_plain() || start == end {
            return;
        }
        match block.spans.last_mut() {
            Some(last) if last.style == style && last.range.end == start => last.range.end = end,
            Some(last) if last.style == style && last.range.end + separator.len() == start => {
                // A collapsed space inside one styled run stays in the run.
                last.range.end = end;
            }
            _ => block.spans.push(Span {
                range: start..end,
                style,
            }),
        }
    }

    fn end_block(&mut self) {
        self.pending_space = false;
        self.pending_cell = false;
        if let Some(block) = self.block.take() {
            self.push_block(block);
        }
    }

    fn push_block(&mut self, mut block: Block) {
        if self.full && block.kind == BlockKind::Rule {
            return;
        }
        let keep = if block.kind == BlockKind::Code {
            block.text.trim_end_matches(['\n', ' ', '\t']).len()
        } else {
            block.text.trim_end().len()
        };
        block.text.truncate(keep);
        block.spans.retain_mut(|span| {
            span.range.end = span.range.end.min(keep);
            span.range.start < span.range.end
        });
        if block.text.is_empty() && block.kind != BlockKind::Rule {
            return;
        }
        if self.rich.blocks.len() >= MAX_BLOCKS {
            self.full = true;
            if let Some(last) = self.rich.blocks.last_mut()
                && !last.text.ends_with('…')
            {
                last.text.push('…');
            }
            return;
        }
        self.rich.blocks.push(block);
    }

    fn finish(mut self) -> Option<RichText> {
        self.end_block();
        if self.deepest > MAX_DEPTH {
            return None;
        }
        Some(self.rich)
    }
}

fn attribute<'t>(tag: &'t Tag, name: &str) -> Option<&'t str> {
    tag.attrs
        .iter()
        .find(|attribute| &*attribute.name.local == name)
        .map(|attribute| &*attribute.value)
}
