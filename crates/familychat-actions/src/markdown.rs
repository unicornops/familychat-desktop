//! What the person wrote, as the message that is sent (PLAN.md, section
//! 5.8; ADR 0012): Markdown through ruma's conversion (the one matrix-sdk
//! uses for `text_markdown`), mentions as `matrix.to` links with
//! `m.mentions`, and the HTML through Family Chat's own sanitiser.
//!
//! The formatted body sent is exactly what the sanitiser keeps
//! (`familychat_security::matrix_html::sanitize`), so a message never
//! carries markup that Family Chat itself would strip from someone
//! else's: raw HTML typed into the composer goes through the same
//! allowlist as a message received. A body over the sanitiser's caps is
//! sent as plain text only.

use familychat_core::{
    messaging::{Mention, OutgoingMessage},
    timeline::MessageBody,
};
use familychat_security::matrix_html;
use ruma::events::room::message::FormattedBody;

/// The `matrix.to` link a mention becomes.
pub fn mention_link(user_id: &str) -> String {
    format!("https://matrix.to/#/{user_id}")
}

/// `text` with every character Markdown gives a meaning to escaped, so a
/// display name is shown as written inside a link.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(
            ch,
            '\\' | '`'
                | '*'
                | '_'
                | '{'
                | '}'
                | '['
                | ']'
                | '('
                | ')'
                | '#'
                | '+'
                | '-'
                | '.'
                | '!'
                | '|'
                | '<'
                | '>'
                | '~'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// The message `text` makes, with the `mentions` the composer inserted.
/// A mention whose text is no longer in the message is dropped; each one
/// still there becomes a link to its user and joins `m.mentions`.
pub fn compose(text: &str, mentions: &[Mention]) -> OutgoingMessage {
    let text = text.trim_end();
    let mut markdown = String::with_capacity(text.len());
    let mut mentioned = Vec::new();
    let mut rest = text;
    // Each mention replaces its first occurrence after the previous one,
    // in the order the composer inserted them.
    let mut pending: Vec<&Mention> = mentions
        .iter()
        .filter(|mention| !mention.label.is_empty())
        .collect();
    while !pending.is_empty() {
        let next = pending
            .iter()
            .enumerate()
            .filter_map(|(index, mention)| rest.find(&mention.label).map(|at| (at, index)))
            .min();
        let Some((at, index)) = next else { break };
        let mention = pending.remove(index);
        markdown.push_str(&rest[..at]);
        markdown.push('[');
        markdown.push_str(&escape(&mention.label));
        markdown.push_str("](");
        markdown.push_str(&mention_link(mention.user_id.as_str()));
        markdown.push(')');
        rest = &rest[at + mention.label.len()..];
        if !mentioned.contains(&mention.user_id) {
            mentioned.push(mention.user_id.clone());
        }
    }
    markdown.push_str(rest);
    let html = FormattedBody::markdown(&markdown)
        .and_then(|formatted| matrix_html::sanitize(&formatted.body))
        .filter(|html| !html.trim().is_empty());
    OutgoingMessage {
        body: MessageBody::new(text),
        html: html.map(MessageBody::new),
        mentions: mentioned,
    }
}

#[cfg(test)]
mod tests {
    use familychat_core::OwnedUserId;

    use super::*;

    fn mention(user: &str, label: &str) -> Mention {
        Mention {
            user_id: OwnedUserId::try_from(user).expect("an id"),
            label: label.to_owned(),
        }
    }

    #[test]
    fn plain_text_is_sent_without_html() {
        for text in ["Hello", "two\nlines", "a < b and c > d", "50% off"] {
            let message = compose(text, &[]);
            assert_eq!(message.body.as_str(), text);
            assert_eq!(message.html, None, "{text}");
        }
    }

    #[test]
    fn markdown_becomes_the_html_the_sanitiser_keeps() {
        let cases = [
            (
                "**bold** and _soft_",
                "<strong>bold</strong> and <em>soft</em>",
            ),
            ("~~gone~~", "<del>gone</del>"),
            ("`code`", "<code>code</code>"),
            ("> quoted", "<blockquote>\n<p>quoted</p>\n</blockquote>\n"),
            ("- one\n- two", "<ul>\n<li>one</li>\n<li>two</li>\n</ul>\n"),
            (
                "```rust\nfn main() {}\n```",
                "<pre><code class=\"language-rust\">fn main() {}\n</code></pre>\n",
            ),
            (
                "[the plan](https://example.org/plan)",
                "<a href=\"https://example.org/plan\">the plan</a>",
            ),
        ];
        for (text, html) in cases {
            let message = compose(text, &[]);
            let sent = message.html.as_ref().expect("html").as_str().to_owned();
            assert_eq!(sent, html, "{text}");
            // What we send is a fixed point of our own sanitiser.
            assert_eq!(matrix_html::sanitize(&sent).as_deref(), Some(sent.as_str()));
        }
    }

    #[test]
    fn raw_html_goes_through_the_allowlist() {
        let message = compose(
            "<b>hi</b> <script>alert(1)</script><img src=\"https://tracker.example/p.png\"> <a href=\"javascript:alert(1)\">x</a>",
            &[],
        );
        let html = message.html.expect("html");
        let html = html.as_str();
        assert!(html.contains("<b>hi</b>"), "{html}");
        for gone in ["script", "alert", "tracker", "javascript"] {
            assert!(!html.contains(gone), "{gone} in {html}");
        }
        assert_eq!(matrix_html::sanitize(html).as_deref(), Some(html));
    }

    #[test]
    fn mentions_become_links_and_m_mentions() {
        let mum = mention("@mum:smith.safechat.family", "Mum");
        let leo = mention("@leo:smith.safechat.family", "Leo [the dog]");
        let message = compose(
            "Mum, can Leo [the dog] come? Mum?",
            &[mum.clone(), leo.clone()],
        );
        assert_eq!(message.body.as_str(), "Mum, can Leo [the dog] come? Mum?");
        assert_eq!(
            message.mentions,
            vec![mum.user_id.clone(), leo.user_id.clone()]
        );
        let html = message.html.expect("html");
        assert_eq!(
            html.as_str(),
            "<a href=\"https://matrix.to/#/@mum:smith.safechat.family\">Mum</a>, can \
             <a href=\"https://matrix.to/#/@leo:smith.safechat.family\">Leo [the dog]</a> come? Mum?"
        );
        // A mention deleted from the text is not sent.
        let message = compose("never mind", &[mum]);
        assert!(message.mentions.is_empty());
        assert_eq!(message.html, None);
    }

    #[test]
    fn trailing_space_is_trimmed() {
        assert_eq!(compose("hello \n\n", &[]).body.as_str(), "hello");
    }
}
