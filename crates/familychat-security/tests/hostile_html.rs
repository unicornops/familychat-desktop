//! The Matrix HTML sanitiser against hostile formatted bodies (PLAN.md,
//! sections 5.8 and 9; ADR 0007). Modelled on Zmail's
//! `zmail-security/tests/hostile_corpus.rs`; `xss-vectors.html` and
//! `mxss.html` are copied from its fixtures.
//!
//! Every fixture under `tests/fixtures/hostile_html`, generated
//! pathological markup and thousands of deterministic mutations of both go
//! through the sanitiser, and two independent checks read each result:
//!
//! - the sanitised markup, re-read with html5ever's tokenizer, has only the
//!   Matrix allowlist's elements and attributes, every `href` an `https`,
//!   `http`, `mailto` or `matrix` address, every image an `mxc://` one;
//! - the rich-text tree keeps its invariants: spans in bounds, on character
//!   boundaries, ordered and not overlapping; link indices valid and every
//!   link's scheme allowed; no control or reordering characters; depths and
//!   output within their caps.
//!
//! Text inside removed elements (`LEAKED` in the fixtures) never reaches
//! the tree, and every input is handled within a time bound.

use std::{
    cell::RefCell,
    fs,
    path::Path,
    time::{Duration, Instant},
};

use familychat_core::rich_text::{BlockKind, RichText};
use familychat_security::matrix_html::{
    ALLOWED_ELEMENTS, LINK_SCHEMES, MAX_BLOCKS, MAX_DEPTH, MAX_ELEMENTS, MAX_HTML_BYTES, MAX_LINKS,
    MAX_NESTING_SHOWN, MAX_TEXT_BYTES, is_reordering_char, message_text, sanitize, to_rich_text,
};
use html5ever::{
    tendril::StrTendril,
    tokenizer::{
        BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
    },
};

/// How long one input may take, in a debug build.
const INPUT_BUDGET: Duration = Duration::from_millis(1_500);

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

/// Sanitises `input` and checks both results; returns the tree.
fn assert_safe(label: &str, input: &str) -> Option<RichText> {
    let started = Instant::now();
    let markup = sanitize(input);
    let rich = to_rich_text(input);
    let elapsed = started.elapsed();
    assert!(
        elapsed < INPUT_BUDGET,
        "{label}: took {elapsed:?} ({} bytes)",
        input.len()
    );
    if let Some(markup) = &markup {
        let errors = check_markup(markup);
        assert!(
            errors.is_empty(),
            "{label}: {errors:#?}\n--- output ---\n{markup}"
        );
    }
    if let Some(rich) = &rich {
        let errors = check_tree(rich);
        assert!(errors.is_empty(), "{label}: {errors:#?}");
    }
    // The caller's fallback is always drawable.
    let text = message_text("plain body", Some(input));
    assert!(check_tree(&text).is_empty(), "{label}: the fallback");
    rich
}

fn fixtures() -> Vec<(String, String)> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hostile_html");
    let mut fixtures: Vec<(String, String)> = fs::read_dir(&directory)
        .expect("the hostile fixtures exist")
        .map(|entry| {
            let path = entry.expect("a fixture").path();
            (
                path.file_name()
                    .expect("a file name")
                    .to_string_lossy()
                    .into_owned(),
                fs::read_to_string(&path).expect("a UTF-8 fixture"),
            )
        })
        .collect();
    fixtures.sort();
    assert!(fixtures.len() >= 9, "the corpus went missing");
    fixtures
}

fn text_of(rich: &Option<RichText>) -> String {
    rich.as_ref().map(RichText::plain_text).unwrap_or_default()
}

#[test]
fn hostile_fixtures_come_out_safe() {
    for (name, html) in fixtures() {
        let rich = assert_safe(&name, &html);
        let text = text_of(&rich);
        assert!(
            !text.contains("LEAKED"),
            "{name}: removed content leaked:\n{text}"
        );
    }
}

#[test]
fn hostile_fixtures_keep_what_should_be_read() {
    let fixtures = fixtures();
    let get = |name: &str| {
        let html = &fixtures
            .iter()
            .find(|(fixture, _)| fixture == name)
            .expect("the fixture exists")
            .1;
        to_rich_text(html).expect("the fixture has rich text")
    };

    let removed = get("removed-content.html").plain_text();
    assert!(removed.contains("Visible start") && removed.contains("Visible end"));

    let reply = get("reply-fallback.html").plain_text();
    assert!(reply.starts_with("The actual reply"), "{reply}");

    let links = get("links.html");
    let hrefs: Vec<&str> = links.links.iter().map(|link| link.href.as_str()).collect();
    assert!(hrefs.contains(&"https://evil.example/steal"), "{hrefs:?}");
    assert!(hrefs.contains(&"matrix:u/ana:example.org"));
    assert!(hrefs.contains(&"https://matrix.to/#/@ana:example.org"));
    assert!(hrefs.contains(&"mailto:help@example.org"));
    for refused in [
        "javascript",
        "vbscript",
        "data:",
        "file:",
        "ftp:",
        "mxc:",
        "cid:",
        "//",
        "/",
        "#",
        "?",
        "tel:",
        "sms:",
        "magnet:",
    ] {
        assert!(
            !hrefs
                .iter()
                .any(|href| href.to_ascii_lowercase().starts_with(refused)),
            "{refused}: {hrefs:?}"
        );
    }
    // The link whose text shows another site keeps its real target, for
    // the open-link dialog's warning.
    let block = links
        .blocks
        .iter()
        .find(|block| block.text.ends_with("does not."))
        .expect("the mismatched link's paragraph");
    let span = &block.spans[0];
    assert_eq!(
        &block.text[span.range.clone()],
        "https://bank.example/login"
    );
    assert_eq!(
        links.links[span.style.link.expect("a link")].href,
        "https://evil.example/steal"
    );

    let styles = get("images-and-styles.html");
    let text = styles.plain_text();
    assert!(text.contains(":party:"), "{text}");
    assert!(
        styles
            .blocks
            .iter()
            .any(|block| block.kind == BlockKind::Rule)
    );
    assert!(
        styles
            .blocks
            .iter()
            .any(|block| block.spans.iter().any(|span| span.style.spoiler))
    );

    let bidi = get("bidi-and-controls.html").plain_text();
    assert!(bidi.contains("👨\u{200D}👩\u{200D}👧"), "joiners stay");
    assert!(
        bidi.contains("moc.elpmaxe.knab//:sptth"),
        "reversed text stays reversed"
    );
}

#[test]
fn pathological_markup_is_refused_or_bounded() {
    let cases: Vec<(&str, String)> = vec![
        ("10k nested divs", "<div>".repeat(10_000) + "deep"),
        ("10k nested bold", "<b>".repeat(10_000) + "deep"),
        (
            "nesting at the exact cap",
            "<blockquote>".repeat(MAX_DEPTH) + "x",
        ),
        (
            "nesting past the exact cap",
            "<blockquote>".repeat(MAX_DEPTH + 1) + "x",
        ),
        ("misnested formatting", "<a><b><i>x</a>".repeat(400)),
        ("many elements", "<br>".repeat(MAX_ELEMENTS + 1)),
        ("elements at the cap", "<b>x</b>".repeat(MAX_ELEMENTS / 2)),
        (
            "a large table",
            format!(
                "<table>{}</table>",
                format!("<tr>{}</tr>", "<td>c</td>".repeat(60)).repeat(60)
            ),
        ),
        (
            "a huge table",
            format!(
                "<table>{}</table>",
                format!("<tr>{}</tr>", "<td>c</td>".repeat(70)).repeat(70)
            ),
        ),
        ("a 1 MB body", "<p>".to_owned() + &"x".repeat(1024 * 1024)),
        ("text at the byte cap", "y".repeat(MAX_HTML_BYTES)),
        ("many paragraphs", "<p>p</p>".repeat(1_000)),
        (
            "many links",
            "<a href=\"https://e.example/\">l</a> ".repeat(500),
        ),
        (
            "many list items",
            format!("<ol>{}</ol>", "<li>i</li>".repeat(1_000)),
        ),
        ("deep lists", "<ul><li>".repeat(40) + "x"),
        ("deep quotes", "<blockquote>".repeat(40) + "x"),
        (
            "long words",
            "<b>".to_owned() + &"w".repeat(60_000) + "</b>",
        ),
        ("many breaks", "a<br>".repeat(2_000)),
        ("only whitespace", " \n\t ".repeat(1_000)),
        ("empty", String::new()),
    ];
    for (label, html) in &cases {
        let rich = assert_safe(label, html);
        match *label {
            "10k nested divs"
            | "10k nested bold"
            | "nesting past the exact cap"
            | "many elements"
            | "a 1 MB body"
            | "a huge table"
            | "empty"
            | "only whitespace" => {
                assert!(rich.is_none(), "{label} should fall back to the plain body");
            }
            "nesting at the exact cap"
            | "text at the byte cap"
            | "many paragraphs"
            | "many links"
            | "many list items"
            | "a large table" => {
                assert!(rich.is_some(), "{label} should be drawn");
            }
            _ => {}
        }
    }
    let links = to_rich_text(&cases[12].1).expect("many links are drawn");
    assert_eq!(links.links.len(), MAX_LINKS);
    let items = to_rich_text(&cases[13].1).expect("many items are drawn");
    assert!(items.blocks.len() <= MAX_BLOCKS);
    assert!(
        items
            .blocks
            .last()
            .is_some_and(|block| block.text.ends_with('…'))
    );
    let quotes = to_rich_text(&cases[15].1).expect("deep quotes are drawn");
    assert_eq!(quotes.blocks[0].quote_depth, MAX_NESTING_SHOWN);
}

/// Matrix reply fallbacks and mentions as clients really send them.
#[test]
fn ordinary_messages_read_as_written() {
    let rich = assert_safe(
        "a reply",
        "<mx-reply><blockquote><a href=\"https://matrix.to/#/!r:x.org/$e\">In reply to</a> \
         <a href=\"https://matrix.to/#/@ben:x.org\">@ben:x.org</a><br>Original</blockquote></mx-reply>\
         Sure, <a href=\"https://matrix.to/#/@ben:x.org\">Ben</a>! See <code>notes.md</code>.",
    )
    .expect("a reply has text");
    assert_eq!(rich.plain_text(), "Sure, Ben! See notes.md.");
    assert_eq!(rich.links.len(), 1);
}

#[test]
fn mutated_hostile_markup_comes_out_safe() {
    let mut corpus: Vec<String> = fixtures().into_iter().map(|(_, html)| html).collect();
    corpus.extend(PIECES.iter().map(|piece| (*piece).to_owned()));
    // CI runs a fixed seed; a longer local run can pick its own:
    // FAMILYCHAT_FUZZ_SEED=7 FAMILYCHAT_FUZZ_ITERATIONS=100000 cargo test --release …
    let seed = std::env::var("FAMILYCHAT_FUZZ_SEED")
        .ok()
        .and_then(|seed| seed.parse().ok())
        .unwrap_or(0x0007_6d78_6874_6d6c);
    let iterations = std::env::var("FAMILYCHAT_FUZZ_ITERATIONS")
        .ok()
        .and_then(|iterations| iterations.parse().ok())
        .unwrap_or(if cfg!(debug_assertions) {
            2_000
        } else {
            20_000
        });
    let mut rng = XorShift(seed | 1);
    for iteration in 0..iterations {
        let mut mutated = corpus[rng.below(corpus.len())].clone();
        for _ in 0..1 + rng.below(6) {
            mutate(&mut mutated, &corpus, &mut rng);
        }
        if mutated.len() > MAX_HTML_BYTES {
            let cut = mutated.floor_char_boundary(MAX_HTML_BYTES);
            mutated.truncate(cut);
        }
        assert_safe(&format!("mutation {iteration} of seed {seed}"), &mutated);
    }
}

/// Pieces the mutations insert.
const PIECES: &[&str] = &[
    "<script>alert(1)</script>",
    "<img src=x onerror=alert(1)>",
    "<a href=\"javascript:alert(1)\">",
    "<a href=\"https://ok.example/\">",
    "<a href=\"matrix:r/x:example.org\">",
    "</a>",
    "<mx-reply>",
    "</mx-reply>",
    "<blockquote>",
    "</blockquote>",
    "<pre><code class=\"language-rust\">",
    "</code></pre>",
    "<ol start=\"7\"><li>",
    "<ul><li>",
    "</li></ul>",
    "<table><tr><td>",
    "</td></tr></table>",
    "<span data-mx-spoiler>",
    "</span>",
    "<svg><script>",
    "<style>",
    "</style>",
    "<!--",
    "-->",
    "<![CDATA[",
    "]]>",
    "&#x202E;",
    "\u{202E}",
    "\u{0}",
    "\u{1b}[2J",
    "&lt;",
    "<br>",
    "<hr>",
    "<h3>",
    "<details><summary>",
    "<img src=\"mxc://e.example/x\" alt=\":x:\">",
    "\"",
    "'",
    "<",
    ">",
    "=",
];

/// A tiny deterministic generator (xorshift64), so the corpus run needs no
/// random-number crate.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// A number in `0..bound` (`bound` > 0).
    fn below(&mut self, bound: usize) -> usize {
        (self.next() % u64::try_from(bound.max(1)).unwrap_or(u64::MAX)) as usize
    }
}

fn mutate(input: &mut String, corpus: &[String], rng: &mut XorShift) {
    let boundary =
        |input: &str, rng: &mut XorShift| input.floor_char_boundary(rng.below(input.len() + 1));
    match rng.below(6) {
        0 | 1 => {
            let at = boundary(input, rng);
            input.insert_str(at, PIECES[rng.below(PIECES.len())]);
        }
        2 => {
            let start = boundary(input, rng);
            let end = input.floor_char_boundary(start + input[start..].len().min(rng.below(64)));
            input.replace_range(start..end, "");
        }
        3 => {
            let start = boundary(input, rng);
            let end = input.floor_char_boundary(start + input[start..].len().min(rng.below(128)));
            let copy = input[start..end].to_owned();
            let at = boundary(input, rng);
            input.insert_str(at, &copy);
        }
        4 => {
            let other = &corpus[rng.below(corpus.len())];
            let from = boundary(other, rng);
            let to = other.floor_char_boundary(from + other[from..].len().min(rng.below(256)));
            let at = boundary(input, rng);
            input.insert_str(at, &other[from..to]);
        }
        _ => {
            let start = boundary(input, rng);
            let end = input.floor_char_boundary(start + input[start..].len().min(rng.below(32)));
            let flipped: String = input[start..end]
                .chars()
                .map(|ch| {
                    if ch.is_ascii_lowercase() {
                        ch.to_ascii_uppercase()
                    } else {
                        ch.to_ascii_lowercase()
                    }
                })
                .collect();
            input.replace_range(start..end, &flipped);
        }
    }
}
