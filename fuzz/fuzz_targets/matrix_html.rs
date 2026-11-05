//! Fuzzes the Matrix HTML sanitiser (`familychat-security`'s
//! `matrix_html`; ADR 0007) with libFuzzer. Whatever the formatted body,
//! nothing panics, and the two independent checks of the hostile corpus
//! (`crates/familychat-security/tests/hostile_html.rs`, whose checker is
//! copied here) hold for every input:
//!
//! - the sanitised markup, re-read with html5ever's tokenizer, has only the
//!   Matrix allowlist's elements and attributes, every `href` an allowed
//!   scheme and every image an `mxc://` one;
//! - the rich-text tree keeps its invariants: spans in bounds, on
//!   character boundaries, ordered and not overlapping; link indices
//!   valid; no control or reordering characters; depths and output within
//!   their caps; and input over `MAX_HTML_BYTES` is refused.
//!
//! `message_text`, the caller's fallback, is always drawable too.

#![no_main]

use std::cell::RefCell;

use familychat_core::rich_text::{BlockKind, RichText};
use familychat_security::matrix_html::{
    ALLOWED_ELEMENTS, LINK_SCHEMES, MAX_BLOCKS, MAX_HTML_BYTES, MAX_LINKS, MAX_NESTING_SHOWN,
    MAX_TEXT_BYTES, is_reordering_char, message_text, sanitize, to_rich_text,
};
use html5ever::{
    tendril::StrTendril,
    tokenizer::{
        BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
    },
};
use libfuzzer_sys::fuzz_target;

/// The attributes the checker accepts, per element.
fn attribute_allowed(element: &str, attribute: &str) -> bool {
    matches!(
        (element, attribute),
        ("a", "href")
            | ("img", "alt" | "title" | "src")
            | ("ol", "start")
            | ("code", "class")
            | ("span", "data-mx-spoiler")
    )
}

fn scheme_allowed(href: &str) -> bool {
    href.split_once(':').is_some_and(|(scheme, rest)| {
        !rest.is_empty()
            && LINK_SCHEMES
                .iter()
                .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
    })
}

#[derive(Default)]
struct Checker {
    errors: Vec<String>,
}

struct CheckerSink {
    checker: RefCell<Checker>,
}

impl TokenSink for CheckerSink {
    type Handle = ();

    fn process_token(&self, token: Token, _line_number: u64) -> TokenSinkResult<()> {
        let mut checker = self.checker.borrow_mut();
        match token {
            Token::TagToken(tag) => {
                let name: &str = &tag.name;
                if !ALLOWED_ELEMENTS.contains(&name) {
                    checker.errors.push(format!("element <{name}>"));
                }
                if tag.kind == TagKind::EndTag {
                    return TokenSinkResult::Continue;
                }
                for attribute in &tag.attrs {
                    let attribute_name: &str = &attribute.name.local;
                    let value: &str = &attribute.value;
                    if !attribute_allowed(name, attribute_name) {
                        checker
                            .errors
                            .push(format!("attribute {attribute_name} on <{name}>"));
                        continue;
                    }
                    match (name, attribute_name) {
                        ("a", "href") if !scheme_allowed(value) => {
                            checker.errors.push(format!("href {value:?}"));
                        }
                        ("img", "src") if !value.to_ascii_lowercase().starts_with("mxc://") => {
                            checker.errors.push(format!("img src {value:?}"));
                        }
                        ("code", "class")
                            if !value.starts_with("language-")
                                || value.contains(|ch: char| {
                                    ch.is_whitespace() || matches!(ch, '"' | '\'' | '<' | '>')
                                }) =>
                        {
                            checker.errors.push(format!("code class {value:?}"));
                        }
                        _ => {}
                    }
                }
            }
            Token::CommentToken(_) => checker.errors.push("a comment".to_owned()),
            Token::DoctypeToken(_) => checker.errors.push("a doctype".to_owned()),
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

fn check_markup(html: &str) -> Vec<String> {
    let sink = CheckerSink {
        checker: RefCell::new(Checker::default()),
    };
    let queue = BufferQueue::default();
    queue.push_back(StrTendril::from_slice(html));
    let tokenizer = Tokenizer::new(sink, TokenizerOpts::default());
    let _ = tokenizer.feed(&queue);
    tokenizer.end();
    tokenizer.sink.checker.into_inner().errors
}

/// The tree's invariants.
fn check_tree(rich: &RichText) -> Vec<String> {
    let mut errors = Vec::new();
    if rich.blocks.len() > MAX_BLOCKS {
        errors.push(format!("{} blocks", rich.blocks.len()));
    }
    if rich.links.len() > MAX_LINKS {
        errors.push(format!("{} links", rich.links.len()));
    }
    let text_bytes: usize = rich.blocks.iter().map(|block| block.text.len()).sum();
    // Each truncation may add one ellipsis.
    if text_bytes > MAX_TEXT_BYTES + 2 * '…'.len_utf8() {
        errors.push(format!("{text_bytes} bytes of text"));
    }
    for link in &rich.links {
        if !scheme_allowed(&link.href) {
            errors.push(format!("link {:?}", link.href));
        }
    }
    for (index, block) in rich.blocks.iter().enumerate() {
        if block.quote_depth > MAX_NESTING_SHOWN {
            errors.push(format!("block {index}: quote depth {}", block.quote_depth));
        }
        match block.kind {
            BlockKind::ListItem { depth, .. } if depth == 0 || depth > MAX_NESTING_SHOWN => {
                errors.push(format!("block {index}: list depth {depth}"));
            }
            BlockKind::Heading(level) if !(1..=6).contains(&level) => {
                errors.push(format!("block {index}: heading {level}"));
            }
            BlockKind::Rule if !block.text.is_empty() => {
                errors.push(format!("block {index}: a rule with text"));
            }
            _ => {}
        }
        if block.kind != BlockKind::Rule && block.text.trim().is_empty() {
            errors.push(format!("block {index}: empty"));
        }
        for ch in block.text.chars() {
            let allowed_control = ch == '\n' || (ch == '\t' && block.kind == BlockKind::Code);
            if (ch.is_control() && !allowed_control) || is_reordering_char(ch) {
                errors.push(format!("block {index}: character U+{:04X}", u32::from(ch)));
            }
        }
        let mut end = 0;
        for span in &block.spans {
            let range = &span.range;
            if range.start < end || range.start >= range.end || range.end > block.text.len() {
                errors.push(format!("block {index}: span {range:?} after {end}"));
                continue;
            }
            if !block.text.is_char_boundary(range.start) || !block.text.is_char_boundary(range.end)
            {
                errors.push(format!("block {index}: span {range:?} off a boundary"));
            }
            if span.style.link.is_some_and(|link| link >= rich.links.len()) {
                errors.push(format!("block {index}: link {:?}", span.style.link));
            }
            end = range.end;
        }
    }
    errors
}

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    let markup = sanitize(input);
    if input.len() > MAX_HTML_BYTES {
        assert!(markup.is_none(), "input over the byte cap was sanitised");
    }
    if let Some(markup) = &markup {
        let errors = check_markup(markup);
        assert!(errors.is_empty(), "{errors:#?}\n--- output ---\n{markup}");
    }
    if let Some(rich) = to_rich_text(input) {
        let errors = check_tree(&rich);
        assert!(errors.is_empty(), "{errors:#?}");
    }
    let text = message_text("plain body", Some(input));
    let errors = check_tree(&text);
    assert!(errors.is_empty(), "the fallback: {errors:#?}");
});
