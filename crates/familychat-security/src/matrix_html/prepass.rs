//! A tokenizer pass over a formatted body before it is parsed into a tree:
//! it counts elements and approximate nesting against the caps.
//!
//! It runs in linear time and memory whatever the input, so markup that
//! would make the tree builder slow (deep nesting makes some of its steps
//! walk the whole stack of open elements) is refused before it is built.
//!
//! Adapted from Zmail's `zmail-security` (`src/html/prepass.rs`), without
//! the style and `<body>` collection mail needs.

use std::cell::RefCell;

use html5ever::tokenizer::{Tag, TagKind, Token, TokenSink, TokenSinkResult};

use super::{
    MAX_ELEMENTS, MAX_PREPASS_DEPTH,
    elements::{self, OpenElements},
    tokenize,
};

/// What the pre-pass found.
#[derive(Debug, Default)]
pub(crate) struct PrePass {
    pub(crate) elements: usize,
    pub(crate) deepest: usize,
}

impl PrePass {
    pub(crate) fn run(html: &str) -> Self {
        let sink = PrePassSink {
            state: RefCell::new(State {
                open: OpenElements::with_limit(MAX_PREPASS_DEPTH),
                ..State::default()
            }),
        };
        tokenize(html, sink).state.into_inner().found
    }

    /// Whether the markup is over a cap.
    pub(crate) fn over_a_cap(&self) -> bool {
        self.elements > MAX_ELEMENTS || self.deepest > MAX_PREPASS_DEPTH
    }
}

struct PrePassSink {
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    found: PrePass,
    open: OpenElements,
    raw_text: Option<String>,
}

impl TokenSink for PrePassSink {
    type Handle = ();

    fn process_token(&self, token: Token, _line_number: u64) -> TokenSinkResult<()> {
        let mut state = self.state.borrow_mut();
        if state.found.over_a_cap() {
            // Over a cap: nothing more to learn.
            return TokenSinkResult::Continue;
        }
        match token {
            Token::TagToken(tag) if tag.kind == TagKind::StartTag => return state.start_tag(&tag),
            Token::TagToken(tag) => {
                if state.raw_text.as_deref() == Some(&*tag.name) {
                    state.raw_text = None;
                }
                state.open.close(&tag.name);
                state.found.deepest = state.open.deepest();
            }
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

impl State {
    fn start_tag(&mut self, tag: &Tag) -> TokenSinkResult<()> {
        let name: &str = &tag.name;
        self.found.elements += 1;
        self.open.open(name, tag.self_closing);
        self.found.deepest = self.open.deepest();
        match elements::raw_text_state(name) {
            Some(state) if !tag.self_closing => {
                self.raw_text = Some(name.to_owned());
                state
            }
            _ => TokenSinkResult::Continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_elements_and_nesting() {
        let found = PrePass::run("<p><b>x</b></p><ul><li>a<li>b</ul>");
        assert_eq!(found.elements, 5);
        assert_eq!(found.deepest, 2);
        assert!(!found.over_a_cap());
    }

    #[test]
    fn script_contents_are_not_markup() {
        let found = PrePass::run("<script>for(;;){'<b><b><b>'}</script>");
        assert_eq!(found.elements, 1);
    }

    #[test]
    fn deep_and_wide_markup_is_over_a_cap() {
        assert!(PrePass::run(&"<div>".repeat(MAX_PREPASS_DEPTH + 1)).over_a_cap());
        assert!(PrePass::run(&"<br>".repeat(MAX_ELEMENTS + 1)).over_a_cap());
    }
}
