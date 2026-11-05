//! The Matrix HTML sanitiser (PLAN.md, section 5.8; ADR 0007).
//!
//! A message's `formatted_body` (`org.matrix.custom.html`) never reaches a
//! frontend as markup. [`to_rich_text`] turns it into `familychat-core`'s
//! [`RichText`] in three passes, none of which loads anything:
//!
//! 1. A tokenizer pre-pass (`prepass`) checks the caps in linear time:
//!    [`MAX_HTML_BYTES`], [`MAX_ELEMENTS`] and nesting. A body over a cap
//!    gets no rich text: the caller draws the plain `body` instead.
//! 2. ammonia parses the markup with html5ever's tree builder and keeps
//!    the Matrix specification's allowlist of elements and attributes
//!    (the client-server API's "HTML" section for `m.room.message`):
//!    formatting, headings, quotes, lists, code, tables, links, spoilers,
//!    `details`, and images as their alt text. The reply fallback
//!    (`mx-reply`), scripts, styles, frames, objects, SVG, MathML, forms
//!    and templates are removed with their contents. A link keeps only an
//!    `https`, `http`, `mailto` or `matrix` address; an image keeps only an
//!    `mxc://` one; relative addresses and comments go.
//! 3. A tokenizer pass over ammonia's output (`tree`) builds the flat
//!    rich-text tree, with its own output caps, and refuses markup nested
//!    deeper than [`MAX_DEPTH`].
//!
//! The pre-pass and the element stack are adapted from Zmail's mail
//! sanitiser (`zmail-security`'s `src/html/`); the allowlist is Matrix's,
//! not mail's.

mod elements;
mod prepass;
mod tree;

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    sync::LazyLock,
};

use familychat_core::rich_text::RichText;
use html5ever::{
    tendril::StrTendril,
    tokenizer::{BufferQueue, TokenSink, Tokenizer, TokenizerOpts},
};

pub use tree::{
    CELL_SEPARATOR, MAX_BLOCKS, MAX_LINKS, MAX_NESTING_SHOWN, MAX_TEXT_BYTES, is_reordering_char,
};

/// The largest `formatted_body` the sanitiser reads. A whole Matrix event
/// is at most 64 KiB, so a body this size is already as large as the
/// protocol allows.
pub const MAX_HTML_BYTES: usize = 64 * 1024;

/// The most elements a formatted body may have. A long, richly formatted
/// message has a few hundred.
pub const MAX_ELEMENTS: usize = 4_096;

/// The deepest nesting a formatted body may have, checked exactly on the
/// sanitised markup. Real messages rarely pass 10 (a quote in a list in a
/// quote).
pub const MAX_DEPTH: usize = 48;

/// The nesting the pre-pass refuses before parsing. Its count is
/// approximate (it applies only the common implied end tags), so it is
/// looser than [`MAX_DEPTH`], which is checked exactly afterwards.
const MAX_PREPASS_DEPTH: usize = 4 * MAX_DEPTH;

/// The elements the Matrix specification allows in `formatted_body`
/// (client-server API, "m.room.message msgtypes", HTML).
pub const ALLOWED_ELEMENTS: &[&str] = &[
    "font",
    "del",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "blockquote",
    "p",
    "a",
    "ul",
    "ol",
    "sup",
    "sub",
    "li",
    "b",
    "i",
    "u",
    "strong",
    "em",
    "s",
    "code",
    "hr",
    "br",
    "div",
    "table",
    "thead",
    "tbody",
    "tr",
    "th",
    "td",
    "caption",
    "pre",
    "span",
    "img",
    "details",
    "summary",
];

/// Elements removed together with their contents: the reply fallback, code,
/// foreign documents and form controls, whose text is not message text.
pub const REMOVED_WITH_CONTENTS: &[&str] = &[
    "mx-reply", "applet", "audio", "embed", "frame", "frameset", "head", "iframe", "math",
    "noembed", "noframes", "noscript", "object", "script", "select", "style", "svg", "template",
    "textarea", "title", "video", "xmp",
];

/// The schemes a link keeps.
pub const LINK_SCHEMES: &[&str] = &["https", "http", "mailto", "matrix"];

/// The attributes kept, per element. Colours (`data-mx-color`,
/// `data-mx-bg-color`, `color`) are dropped: the window draws in its theme's
/// colours, so a sender can't make text unreadable.
const ELEMENT_ATTRIBUTES: &[(&str, &[&str])] = &[
    ("a", &["href"]),
    ("img", &["alt", "title", "src"]),
    ("ol", &["start"]),
    ("code", &["class"]),
    ("span", &["data-mx-spoiler"]),
];

/// The ammonia builder, made once.
static SANITISER: LazyLock<ammonia::Builder<'static>> = LazyLock::new(|| {
    let tag_attributes: HashMap<&str, HashSet<&str>> = ELEMENT_ATTRIBUTES
        .iter()
        .map(|(element, attributes)| (*element, attributes.iter().copied().collect()))
        .collect();
    let mut url_schemes: HashSet<&str> = LINK_SCHEMES.iter().copied().collect();
    url_schemes.insert("mxc");
    let mut builder = ammonia::Builder::empty();
    builder
        .tags(ALLOWED_ELEMENTS.iter().copied().collect())
        .clean_content_tags(REMOVED_WITH_CONTENTS.iter().copied().collect())
        .tag_attributes(tag_attributes)
        .url_schemes(url_schemes)
        .url_relative(ammonia::UrlRelative::Deny)
        .link_rel(None)
        .strip_comments(true)
        .attribute_filter(filter_attribute);
    builder
});

/// The attribute policy ammonia's allowlist leaves to code: a link's
/// scheme, an image's source, a code block's language.
fn filter_attribute<'a>(element: &str, attribute: &str, value: &'a str) -> Option<Cow<'a, str>> {
    match (element, attribute) {
        ("a", "href") => link_target(value).map(Cow::Borrowed),
        ("img", "src") => value
            .trim()
            .get(..6)
            .filter(|scheme| scheme.eq_ignore_ascii_case("mxc://"))
            .map(|_| Cow::Borrowed(value.trim())),
        ("code", "class") => {
            let language = value.trim().strip_prefix("language-")?;
            let ok = !language.is_empty()
                && language.len() <= 32
                && language
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '#' | '_' | '-'));
            ok.then(|| Cow::Borrowed(value.trim()))
        }
        ("span", "data-mx-spoiler") => Some(Cow::Borrowed(value)),
        ("ol", "start") => value
            .trim()
            .parse::<u32>()
            .ok()
            .map(|_| Cow::Borrowed(value.trim())),
        ("img", "alt" | "title") => Some(Cow::Borrowed(value)),
        _ => None,
    }
}

/// A link target the sanitiser keeps: an address with one of
/// [`LINK_SCHEMES`].
fn link_target(value: &str) -> Option<&str> {
    let value = value.trim();
    let (scheme, rest) = value.split_once(':')?;
    let allowed = LINK_SCHEMES
        .iter()
        .any(|allowed| scheme.eq_ignore_ascii_case(allowed));
    (allowed && !rest.is_empty()).then_some(value)
}

/// Runs html5ever's tokenizer over `input` into `sink`.
pub(crate) fn tokenize<S: TokenSink<Handle = ()>>(input: &str, sink: S) -> S {
    let queue = BufferQueue::default();
    queue.push_back(StrTendril::from_slice(input));
    let tokenizer = Tokenizer::new(sink, TokenizerOpts::default());
    let _ = tokenizer.feed(&queue);
    tokenizer.end();
    tokenizer.sink
}

/// Sanitises `html` into markup that has only the allowlist in it. `None`
/// when it is over a cap. Exposed for the hostile-corpus tests.
pub fn sanitize(html: &str) -> Option<String> {
    if html.len() > MAX_HTML_BYTES {
        return None;
    }
    if prepass::PrePass::run(html).over_a_cap() {
        return None;
    }
    Some(SANITISER.clean(html).to_string())
}

/// The rich text of a message's `formatted_body` (`org.matrix.custom.html`),
/// sanitised; `None` when it is over a cap (or has no text), so the caller
/// draws the plain body instead.
pub fn to_rich_text(html: &str) -> Option<RichText> {
    let sanitised = sanitize(html)?;
    tree::build(&sanitised).filter(|rich| !rich.is_empty())
}

/// A message's text to draw: its sanitised formatted body when it has one
/// within the caps, else its plain body with its links.
pub fn message_text(body: &str, formatted_html: Option<&str>) -> RichText {
    formatted_html
        .and_then(to_rich_text)
        .unwrap_or_else(|| RichText::plain(body))
}

#[cfg(test)]
mod tests {
    use familychat_core::rich_text::{BlockKind, ListMarker};

    use super::*;

    fn rich(html: &str) -> RichText {
        to_rich_text(html).expect("rich text")
    }

    #[test]
    fn formatting_becomes_styled_spans() {
        let rich = rich("Hello <b>bold</b> and <em>soft</em> <code>x()</code>!");
        assert_eq!(rich.blocks.len(), 1);
        let block = &rich.blocks[0];
        assert_eq!(block.text, "Hello bold and soft x()!");
        let styled: Vec<_> = block
            .spans
            .iter()
            .map(|span| (&block.text[span.range.clone()], span.style))
            .collect();
        assert_eq!(styled.len(), 3);
        assert_eq!(styled[0].0, "bold");
        assert!(styled[0].1.bold);
        assert!(styled[1].1.italic);
        assert!(styled[2].1.code);
    }

    #[test]
    fn blocks_lists_quotes_and_code() {
        let rich = rich(
            "<h2>Plan</h2><p>First</p><ol start=\"3\"><li>three</li><li>four<ul><li>nested</li></ul></li></ol>\
             <blockquote><p>quoted</p></blockquote><pre><code class=\"language-rust\">fn main() {\n    x\n}\n</code></pre><hr>",
        );
        let kinds: Vec<_> = rich.blocks.iter().map(|block| block.kind).collect();
        assert_eq!(
            kinds,
            [
                BlockKind::Heading(2),
                BlockKind::Paragraph,
                BlockKind::ListItem {
                    depth: 1,
                    marker: ListMarker::Number(3)
                },
                BlockKind::ListItem {
                    depth: 1,
                    marker: ListMarker::Number(4)
                },
                BlockKind::ListItem {
                    depth: 2,
                    marker: ListMarker::Bullet
                },
                BlockKind::Paragraph,
                BlockKind::Code,
                BlockKind::Rule,
            ]
        );
        assert_eq!(rich.blocks[5].quote_depth, 1);
        assert_eq!(rich.blocks[6].text, "fn main() {\n    x\n}");
        assert!(
            rich.blocks[6].spans.is_empty(),
            "code blocks are not inline code"
        );
    }

    #[test]
    fn links_keep_safe_targets_only() {
        let rich = rich(
            "<a href=\"https://example.org/x\">web</a> <a href=\"javascript:alert(1)\">js</a> \
             <a href=\"https://matrix.to/#/@ana:x.org\">Ana</a> <a href=\"/relative\">rel</a>",
        );
        let hrefs: Vec<_> = rich.links.iter().map(|link| link.href.as_str()).collect();
        assert_eq!(
            hrefs,
            ["https://example.org/x", "https://matrix.to/#/@ana:x.org"]
        );
        assert_eq!(rich.blocks[0].text, "web js Ana rel");
    }

    #[test]
    fn replies_scripts_and_styles_go_with_their_contents() {
        let rich = rich(
            "<mx-reply><blockquote>old message</blockquote></mx-reply>new\
             <script>alert('x')</script><style>p{}</style><svg><text>svg</text></svg>",
        );
        assert_eq!(rich.plain_text(), "new");
    }

    #[test]
    fn spoilers_tables_images_and_breaks() {
        let rich = rich(
            "<p>a<br>b <span data-mx-spoiler=\"\">secret</span> <img src=\"mxc://x/y\" alt=\":wave:\"></p>\
             <table><tr><th>Name</th><th>Age</th></tr><tr><td>Ana</td><td>9</td></tr></table>",
        );
        assert_eq!(rich.blocks[0].text, "a\nb secret :wave:");
        assert!(rich.blocks[0].spans.iter().any(|span| span.style.spoiler));
        assert_eq!(rich.blocks[1].text, format!("Name{CELL_SEPARATOR}Age"));
        assert_eq!(rich.blocks[2].text, format!("Ana{CELL_SEPARATOR}9"));
        assert!(rich.blocks[1].spans.iter().all(|span| span.style.bold));
    }

    #[test]
    fn reordering_characters_go_and_joiners_stay() {
        let rich = rich(
            "<a href=\"https://evil.example\">\u{202E}moc.knab\u{202C}</a> 👨\u{200D}👩\u{200D}👧",
        );
        assert_eq!(rich.blocks[0].text, "moc.knab 👨\u{200D}👩\u{200D}👧");
    }

    #[test]
    fn over_a_cap_falls_back_to_the_plain_body() {
        assert!(to_rich_text(&"<div>".repeat(MAX_PREPASS_DEPTH + 1)).is_none());
        assert!(to_rich_text(&"x".repeat(MAX_HTML_BYTES + 1)).is_none());
        assert!(to_rich_text(&"<b>".repeat(MAX_DEPTH + 1)).is_none());
        assert!(to_rich_text("<script>only</script>").is_none());
        let text = message_text("plain https://example.org", Some(&"<i>".repeat(5_000)));
        assert_eq!(text.plain_text(), "plain https://example.org");
        assert_eq!(text.links.len(), 1);
        assert_eq!(message_text("plain", None).plain_text(), "plain");
    }
}
