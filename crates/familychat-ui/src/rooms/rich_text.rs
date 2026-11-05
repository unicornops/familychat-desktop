//! Drawing `familychat-core`'s rich-text tree (ADR 0007).
//!
//! The window never sees markup: a message's formatted body was sanitised
//! and read into the tree by the session, and this module draws each block
//! as styled text. Links and spoilers are click targets inside the text
//! (GPUI's `InteractiveText`, as Zmail's reader draws its links); a click
//! on a link goes to the view's handler, which opens Matrix links in the
//! app and asks before any other (`links::classify`), and a click on a
//! hidden spoiler shows it. Colours from the sender are never used, so no
//! message can hide its text against the background.
//!
//! The link drawing (styled ranges, `InteractiveText` clicks and the
//! address as a tooltip) is adapted from Zmail's `zmail-ui`
//! (`src/reader/body.rs`).

use std::{ops::Range, rc::Rc};

use familychat_core::rich_text::{Block, BlockKind, RichText, SpanStyle};
use gpui::{
    AnyElement, App, ElementId, FontStyle, FontWeight, HighlightStyle, Hsla, InteractiveText,
    IntoElement, SharedString, StrikethroughStyle, StyledText, UnderlineStyle, Window, div,
    prelude::*, px, rgb,
};

use crate::components::tooltip::Tooltip;
use crate::styles::{
    RADIUS_MD, SPACE_1, SPACE_2, SPACE_3, TYPE_BODY, TYPE_BODY_STRONG, TYPE_TITLE, TypeStyleExt,
    UiColors, contrast::mix,
};

/// The monospace family code is drawn in, per platform.
pub(crate) const MONO_FONT: &str = if cfg!(target_os = "macos") {
    "Menlo"
} else if cfg!(target_os = "windows") {
    "Consolas"
} else {
    "DejaVu Sans Mono"
};

/// How a message's text is toned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextTone {
    /// An ordinary message.
    Normal,
    /// A notice (from a bot): muted.
    Notice,
    /// An emote: italic.
    Emote,
}

/// What a click inside the text hits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextClick {
    /// A link, by its index in [`RichText::links`].
    Link(usize),
    /// A hidden spoiler.
    Spoiler,
}

/// The handler for clicks inside the text.
pub(crate) type ClickHandler = Rc<dyn Fn(TextClick, &mut Window, &mut App)>;

/// What a run of text looks like.
fn highlight(style: SpanStyle, colors: &UiColors, revealed: bool) -> Option<HighlightStyle> {
    if style.is_plain() {
        return None;
    }
    let mut highlight = HighlightStyle::default();
    if style.bold {
        highlight.font_weight = Some(FontWeight::BOLD);
    }
    if style.italic {
        highlight.font_style = Some(FontStyle::Italic);
    }
    if style.underline {
        highlight.underline = Some(UnderlineStyle {
            thickness: px(1.0),
            color: None,
            wavy: false,
        });
    }
    if style.strikethrough {
        highlight.strikethrough = Some(StrikethroughStyle {
            thickness: px(1.0),
            color: None,
        });
    }
    if style.code {
        highlight.background_color = Some(rgb(mix(colors.surface, colors.border, 0.6)).into());
    }
    if style.script {
        highlight.color = Some(rgb(colors.muted).into());
    }
    if style.link.is_some() {
        let accent: Hsla = rgb(colors.accent).into();
        highlight.color = Some(accent);
        highlight.underline = Some(UnderlineStyle {
            thickness: px(1.0),
            color: Some(accent),
            wavy: false,
        });
    }
    if style.spoiler && revealed {
        // Shown, on a tint that says it was a spoiler.
        highlight.background_color = Some(rgb(mix(colors.surface, colors.border, 0.5)).into());
    }
    if style.spoiler && !revealed {
        // Hidden: the text is drawn in its own background colour.
        let fill: Hsla = rgb(colors.muted).into();
        highlight.background_color = Some(fill);
        highlight.color = Some(fill);
        highlight.underline = None;
    }
    Some(highlight)
}

/// One block's styled text, with its links and spoilers clickable.
fn block_text(
    id: ElementId,
    block: &Block,
    rich: &RichText,
    colors: &UiColors,
    revealed: bool,
    on_click: &ClickHandler,
) -> AnyElement {
    let runs = block.runs();
    let highlights: Vec<(Range<usize>, HighlightStyle)> = runs
        .iter()
        .filter_map(|(range, style)| Some((range.clone(), highlight(*style, colors, revealed)?)))
        .collect();
    let mut targets: Vec<(Range<usize>, TextClick)> = Vec::new();
    for (range, style) in &runs {
        if style.spoiler && !revealed {
            targets.push((range.clone(), TextClick::Spoiler));
        } else if let Some(link) = style.link {
            targets.push((range.clone(), TextClick::Link(link)));
        }
    }
    let styled =
        StyledText::new(SharedString::from(block.text.clone())).with_highlights(highlights);
    if targets.is_empty() {
        return styled.into_any_element();
    }
    let ranges: Vec<Range<usize>> = targets.iter().map(|(range, _)| range.clone()).collect();
    let clicks: Vec<TextClick> = targets.iter().map(|(_, click)| *click).collect();
    let tips: Vec<(Range<usize>, SharedString)> = targets
        .iter()
        .filter_map(|(range, click)| match click {
            TextClick::Link(link) => Some((range.clone(), rich.link(*link)?.href.clone().into())),
            TextClick::Spoiler => Some((range.clone(), "Show the spoiler".into())),
        })
        .collect();
    let on_click = on_click.clone();
    InteractiveText::new(id, styled)
        .on_click(ranges, move |index, window, cx| {
            if let Some(click) = clicks.get(index) {
                on_click(*click, window, cx);
            }
        })
        .tooltip(move |index, window, cx| {
            tips.iter()
                .find(|(range, _)| range.contains(&index))
                .map(|(_, tip)| Tooltip::text(tip.clone())(window, cx))
        })
        .into_any_element()
}

/// The blocks of `rich`, in a column. `id` is unique among its siblings.
pub(crate) fn render_rich_text(
    id: impl Into<ElementId>,
    rich: &RichText,
    tone: TextTone,
    revealed: bool,
    colors: &UiColors,
    on_click: ClickHandler,
) -> AnyElement {
    let id: ElementId = id.into();
    let text_color = match tone {
        TextTone::Notice => colors.muted,
        TextTone::Normal | TextTone::Emote => colors.foreground,
    };
    div()
        .id(id.clone())
        .flex()
        .flex_col()
        .gap(px(SPACE_1))
        .min_w_0()
        .type_style(TYPE_BODY)
        .text_color(rgb(text_color))
        .when(tone == TextTone::Emote, |this| this.italic())
        .children(rich.blocks.iter().enumerate().map(|(index, block)| {
            let text_id = ElementId::NamedInteger("rich-block".into(), index as u64);
            render_block(text_id, block, rich, colors, revealed, &on_click)
        }))
        .into_any_element()
}

fn render_block(
    id: ElementId,
    block: &Block,
    rich: &RichText,
    colors: &UiColors,
    revealed: bool,
    on_click: &ClickHandler,
) -> AnyElement {
    let body = match block.kind {
        BlockKind::Rule => div()
            .my(px(SPACE_1))
            .h(px(1.0))
            .w_full()
            .bg(rgb(colors.border))
            .into_any_element(),
        BlockKind::Paragraph => div()
            .min_w_0()
            .child(block_text(id, block, rich, colors, revealed, on_click))
            .into_any_element(),
        BlockKind::Heading(level) => div()
            .min_w_0()
            .pt(px(SPACE_1))
            .when(level <= 2, |this| this.type_style(TYPE_TITLE))
            .when(level > 2, |this| this.type_style(TYPE_BODY_STRONG))
            .font_weight(FontWeight::SEMIBOLD)
            .child(block_text(id, block, rich, colors, revealed, on_click))
            .into_any_element(),
        BlockKind::ListItem { depth, marker } => div()
            .flex()
            .min_w_0()
            .pl(px(f32::from(depth.saturating_sub(1)) * 18.0))
            .child(
                div()
                    .flex_none()
                    .w(px(22.0))
                    .text_color(rgb(colors.muted))
                    .child(marker.label()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(block_text(id, block, rich, colors, revealed, on_click)),
            )
            .into_any_element(),
        BlockKind::Code => div()
            .min_w_0()
            .px(px(SPACE_3))
            .py(px(SPACE_2))
            .rounded(px(RADIUS_MD))
            .bg(rgb(mix(colors.surface, colors.border, 0.35)))
            .border_1()
            .border_color(rgb(colors.border))
            .font_family(MONO_FONT)
            .text_sm()
            .child(block_text(id, block, rich, colors, revealed, on_click))
            .into_any_element(),
    };
    if block.quote_depth == 0 {
        return body;
    }
    div()
        .flex()
        .min_w_0()
        .pl(px(f32::from(block.quote_depth.saturating_sub(1)) * 10.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .pl(px(SPACE_3))
                .border_l_2()
                .border_color(rgb(colors.control_border))
                .text_color(rgb(colors.muted))
                .child(body),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_spoilers_hide_their_text_and_links_are_underlined() {
        let colors = UiColors::fallback();
        let spoiler = SpanStyle {
            spoiler: true,
            ..SpanStyle::default()
        };
        let hidden = highlight(spoiler, &colors, false).expect("a style");
        assert_eq!(hidden.color, hidden.background_color);
        let shown = highlight(spoiler, &colors, true);
        assert!(shown.is_none_or(|style| style.color != style.background_color));
        let link = highlight(
            SpanStyle {
                link: Some(0),
                ..SpanStyle::default()
            },
            &colors,
            false,
        )
        .expect("a style");
        assert!(link.underline.is_some());
        assert!(highlight(SpanStyle::default(), &colors, false).is_none());
    }
}
