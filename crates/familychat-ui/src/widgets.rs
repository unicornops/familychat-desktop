//! Reusable widget-level building blocks for the window, copied from
//! Zmail's `zmail-ui` (its mail-only widgets left out).
//!
//! All stateless helper functions that produce GPUI elements live here so that
//! higher-level render modules can share them without duplicating code or
//! importing directly from `lib.rs`.

// Some helpers lead the views that use them (the room list, Phase 6).
#![allow(dead_code)]

use gpui::{
    AnyElement, Entity, FontWeight, InteractiveElement, IntoElement, Role, Stateful,
    StatefulInteractiveElement, Styled, Svg, div, prelude::*, px, rgb, svg,
};

use crate::a11y::{A11yFlags, AccessibleExt, Politeness, readable};
use crate::components::TextInput;
use crate::styles::{
    EMPTY_STATE_ICON_FRAME_SIZE, ICON_SIZE_LG, ICON_SIZE_SM, STATUS_BANNER_STRIP_WIDTH, UiColors,
};

// ---------------------------------------------------------------------------
// EllipsisExt
// ---------------------------------------------------------------------------

/// Single-line text that ends in an ellipsis when it does not fit.
///
/// GPUI 0.2.2's `truncate()` (`whitespace_nowrap` + `text_ellipsis`) caches
/// the first measured size of no-wrap text, which taffy takes before the
/// final width is known, so the text is clipped mid-glyph instead of
/// ending in "…". Clamping wrapping text to one line keys the cache on the
/// wrap width, so the ellipsis is computed for the real width.
pub(crate) trait EllipsisExt: Styled + Sized {
    /// Clamp to one line and end overflowing text with an ellipsis.
    fn ellipsis(self) -> Self {
        self.line_clamp(1).text_ellipsis()
    }
}

impl<T: Styled> EllipsisExt for T {}

// ---------------------------------------------------------------------------
// Hover and selection backgrounds
// ---------------------------------------------------------------------------

/// The resting background of a selectable surface: the selection colour
/// when selected, otherwise `base`.
pub(crate) fn interactive_background(selected: bool, base: u32, colors: &UiColors) -> u32 {
    if selected {
        colors.selected_background
    } else {
        base
    }
}

/// Hover feedback as a GPUI style refinement, so hovering repaints only
/// the hovered element instead of notifying the whole workspace.
pub(crate) trait HoverExt: InteractiveElement + Sized {
    /// Show the hover background unless the element is selected.
    fn hover_bg(self, selected: bool, colors: &UiColors) -> Self {
        if selected {
            self
        } else {
            let hover = colors.hover_background;
            self.hover(move |style| style.bg(rgb(hover)))
        }
    }
}

impl<T: InteractiveElement> HoverExt for T {}

// ---------------------------------------------------------------------------
// empty_state
// ---------------------------------------------------------------------------

/// A centred placeholder card shown when a list or pane has no content.
pub(crate) fn empty_state(
    title: &str,
    detail: &str,
    icon_name: &'static str,
    action: Option<AnyElement>,
    colors: &UiColors,
) -> AnyElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .px_6()
        .text_center()
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                // Full width so the title and detail wrap inside the pane.
                .w_full()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(px(EMPTY_STATE_ICON_FRAME_SIZE))
                        .h(px(EMPTY_STATE_ICON_FRAME_SIZE))
                        .rounded_full()
                        .bg(rgb(colors.button_background))
                        .child(icon(icon_name, ICON_SIZE_LG * 2.0, colors.muted)),
                )
                .child(
                    div()
                        .id("empty-state-title")
                        .accessible(Role::Heading, title.to_owned())
                        .w_full()
                        .text_xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.to_owned()),
                )
                .child(
                    div()
                        .w_full()
                        .text_sm()
                        .text_color(rgb(colors.muted))
                        .child(readable("empty-state-detail", detail.to_owned())),
                )
                .when_some(action, |this, action| this.child(action)),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// input_card
// ---------------------------------------------------------------------------

/// A labelled [`TextInput`]. The input draws its own field, focus ring and
/// placeholder; callers can add children (such as a validation error)
/// below it.
pub(crate) fn input_card(label: &str, input: &Entity<TextInput>, colors: &UiColors) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .w_full()
        .min_w_0()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(colors.muted))
                .child(label.to_owned()),
        )
        .child(input.clone())
}

// ---------------------------------------------------------------------------
// section_card
// ---------------------------------------------------------------------------

/// Render a rounded content card with a section title.
pub(crate) fn section_card(title: &str, colors: &UiColors) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_3()
        .rounded_lg()
        .bg(rgb(colors.surface))
        .border_1()
        .border_color(rgb(colors.border))
        .px_4()
        .py_4()
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.to_owned()),
        )
}

// ---------------------------------------------------------------------------
// icon
// ---------------------------------------------------------------------------

/// Render a Lucide icon SVG at the given pixel size and ARGB colour.
///
/// `name` must match a filename in `assets/icons/` without the `.svg`
/// extension — e.g. `"mail"`, `"inbox"`, `"refresh-cw"`.
pub(crate) fn icon(name: &str, size: f32, color: u32) -> Svg {
    svg()
        .path(format!("icons/{name}.svg"))
        .flex_none()
        .size(px(size))
        .text_color(rgb(color))
}

// ---------------------------------------------------------------------------
// status_banner
// ---------------------------------------------------------------------------

/// Render a compact status or error banner with a leading colour strip.
/// `role` is [`Role::Status`] for news and [`Role::Alert`] for errors.
pub(crate) fn status_banner(
    id: &'static str,
    role: Role,
    message: &str,
    icon_name: &'static str,
    accent_color: u32,
    background_color: u32,
    colors: &UiColors,
) -> Stateful<gpui::Div> {
    // A live region: shown, or changed, it is read out (errors at once).
    let politeness = if role == Role::Alert {
        Politeness::Assertive
    } else {
        Politeness::Polite
    };
    div()
        .id(id)
        .accessible(role, message.to_owned())
        .aria_value(message.to_owned())
        .a11y_flags(A11yFlags {
            live: Some(politeness),
            ..A11yFlags::default()
        })
        .w_full()
        .flex()
        .bg(rgb(background_color))
        .border_t_1()
        .border_color(rgb(colors.border))
        .child(div().w(px(STATUS_BANNER_STRIP_WIDTH)).bg(rgb(accent_color)))
        .child(
            div()
                .flex_1()
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .py_3()
                .child(icon(icon_name, ICON_SIZE_SM, accent_color))
                .child(div().flex_1().text_sm().child(message.to_owned())),
        )
}
