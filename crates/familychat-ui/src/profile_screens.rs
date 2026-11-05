//! The screens the window shows before the profile is open (ADR 0003), in
//! the shape of Zmail's `store_unlock` screens:
//!
//! - **Opening**: while the profile, the keychain and the accounts open
//!   (off the UI thread). Usually too quick to see.
//! - **The keychain is locked or unavailable**: the store's sentence, with
//!   Try again. Family Chat never keeps its data without encryption, so
//!   there is nothing else to offer.
//! - **The key is lost**: the store's sentence (what Start over does and
//!   what is lost), with Start over, which moves the old data aside.
//! - **Anything else**: the error and Try again.

use gpui::{AnyElement, App, IntoElement, Role, SharedString, div, prelude::*, px, rgb};

use crate::a11y::{AccessibleExt, readable};
use crate::components::{Banner, BannerTone, Button, ButtonSize};
use crate::signin::{CARD_WIDTH, brand_mark, spinner};
use crate::styles::{
    ICON_SIZE_LG, RADIUS_XL, SPACE_2, SPACE_3, SPACE_4, SPACE_6, SPACE_8, TYPE_BODY, TYPE_CAPTION,
    TYPE_HEADLINE, TypeStyleExt, UiColors,
};
use crate::widgets::icon;

/// What a profile screen shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProfileScreen {
    /// Opening.
    Opening,
    /// The keychain is locked or can't be reached.
    Keychain(SharedString),
    /// The data's key is gone.
    LostKey(SharedString),
    /// Something else.
    Failed(SharedString),
}

/// The buttons a profile screen offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProfileAction {
    /// Try opening again.
    Retry,
    /// Move the data aside and start again.
    StartOver,
    /// Quit.
    Quit,
}

/// Draws `screen`; `on_action` runs the buttons.
pub(crate) fn render(
    screen: &ProfileScreen,
    colors: &UiColors,
    on_action: impl Fn(ProfileAction, &mut gpui::Window, &mut App) + Clone + 'static,
) -> AnyElement {
    let card = |content: AnyElement| {
        div()
            .id("profile-screen")
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .overflow_y_scroll()
            .px(px(SPACE_6))
            .py(px(SPACE_8))
            .child(
                div()
                    .id("profile-card")
                    .accessible(Role::Main, "Opening Family Chat")
                    .w_full()
                    .max_w(px(CARD_WIDTH))
                    .my_auto()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_4))
                    .p(px(SPACE_8))
                    .rounded(px(RADIUS_XL))
                    .bg(rgb(colors.background))
                    .border_1()
                    .border_color(rgb(colors.border))
                    .shadow_lg()
                    .child(content),
            )
            .into_any_element()
    };
    let button = |id: &'static str, label: &'static str, action: ProfileAction| {
        let on_action = on_action.clone();
        let button = Button::new(id, label)
            .size(ButtonSize::Large)
            .on_click(move |_, window, cx| on_action(action, window, cx));
        match action {
            ProfileAction::Retry => button.primary(),
            ProfileAction::StartOver => button.danger(),
            ProfileAction::Quit => button,
        }
    };
    let footer = |buttons: Vec<Button>| {
        div()
            .flex()
            .justify_end()
            .gap(px(SPACE_2))
            .pt(px(SPACE_2))
            .children(buttons)
    };
    match screen {
        ProfileScreen::Opening => card(
            div()
                .id("profile-opening")
                .accessible(Role::Status, "Opening your data…")
                .flex()
                .flex_col()
                .items_center()
                .gap(px(SPACE_4))
                .py(px(SPACE_6))
                .child(brand_mark(colors, 56.0))
                .child(spinner(
                    "profile-opening-spinner",
                    ICON_SIZE_LG,
                    colors.accent,
                ))
                .child(
                    div()
                        .type_style(TYPE_BODY)
                        .text_color(rgb(colors.muted))
                        .child("Opening your data…"),
                )
                .into_any_element(),
        ),
        ProfileScreen::Keychain(message) => card(
            div()
                .flex()
                .flex_col()
                .gap(px(SPACE_4))
                .child(heading(
                    "lock-keyhole",
                    "Family Chat can't reach the keychain",
                    colors.warning,
                    colors.warning_background,
                ))
                .child(
                    div()
                        .type_style(TYPE_BODY)
                        .text_color(rgb(colors.muted))
                        .child(readable("profile-keychain-message", message.clone())),
                )
                .child(footer(vec![
                    button("profile-quit", "Quit", ProfileAction::Quit),
                    button("profile-retry", "Try again", ProfileAction::Retry),
                ]))
                .into_any_element(),
        ),
        ProfileScreen::LostKey(message) => card(
            div()
                .flex()
                .flex_col()
                .gap(px(SPACE_4))
                .child(heading(
                    "key-round",
                    "Family Chat can't open your data",
                    colors.danger,
                    colors.danger_background,
                ))
                .child(
                    div()
                        .type_style(TYPE_BODY)
                        .text_color(rgb(colors.muted))
                        .child(readable("profile-lost-key-message", message.clone())),
                )
                .child(
                    div()
                        .type_style(TYPE_CAPTION)
                        .text_color(rgb(colors.muted))
                        .child(
                            "Start over keeps the old data in a folder beside the new one, in \
                             case the key turns up again.",
                        ),
                )
                .child(footer(vec![
                    button("profile-quit", "Quit", ProfileAction::Quit),
                    button("profile-start-over", "Start over", ProfileAction::StartOver),
                ]))
                .into_any_element(),
        ),
        ProfileScreen::Failed(message) => card(
            div()
                .flex()
                .flex_col()
                .gap(px(SPACE_4))
                .child(heading(
                    "triangle-alert",
                    "Family Chat couldn't open its data",
                    colors.warning,
                    colors.warning_background,
                ))
                .child(
                    div()
                        .type_style(TYPE_BODY)
                        .text_color(rgb(colors.muted))
                        .child(
                            "Nothing was changed. Fix the problem below (free some disk space, \
                             for example) and try again.",
                        ),
                )
                .child(Banner::new("profile-error", message.clone()).tone(BannerTone::Danger))
                .child(footer(vec![
                    button("profile-quit", "Quit", ProfileAction::Quit),
                    button("profile-retry", "Try again", ProfileAction::Retry),
                ]))
                .into_any_element(),
        ),
    }
}

/// An icon in a tinted circle and the screen's heading.
fn heading(
    icon_name: &'static str,
    title: &'static str,
    color: u32,
    fill: u32,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(SPACE_3))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(44.0))
                .rounded_full()
                .bg(rgb(fill))
                .child(icon(icon_name, 22.0, color)),
        )
        .child(
            div()
                .id("profile-heading")
                .flex_1()
                .min_w_0()
                .accessible(Role::Heading, title)
                .aria_level(1)
                .type_style(TYPE_HEADLINE)
                .child(title),
        )
}
