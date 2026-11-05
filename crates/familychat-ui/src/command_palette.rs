//! `CommandPalette`: Secondary+K's search over the commands table
//! (PLAN.md, section 5.3). Adapted from Zmail's `zmail-ui`
//! `command_palette.rs`, which searches mail commands, folders and
//! accounts; here the entries are the commands of
//! `familychat_core::commands` that can run where focus was.
//!
//! Typing filters them with the shared fuzzy matcher
//! (`familychat_core::fuzzy`) and highlights the matched letters. Up and
//! Down move the highlight, Enter or a click picks, and Escape or a click
//! outside closes the palette (the surrounding `Modal`). Picking emits
//! [`CommandPaletteEvent::Picked`]; the shell closes the palette and runs
//! the command from where focus was.

use std::ops::Range;

use familychat_core::{commands::Command, fuzzy::fuzzy_match};
use gpui::{
    Action, App, AppContext as _, Context, EventEmitter, FocusHandle, Focusable, FontWeight,
    HighlightStyle, IntoElement, Render, Role, ScrollStrategy, SharedString, StyledText,
    Subscription, UniformListScrollHandle, Window, div, prelude::*, px, rgb, uniform_list,
};

use crate::a11y::{
    AccessibleExt, Politeness, announcement_key, highlight_announcement, live_region,
};
use crate::components::actions::{SelectFirst, SelectLast, SelectNext, SelectPrevious};
use crate::components::kbd::spoken_keystrokes;
use crate::components::{ComponentTheme, HitTargetExt, Kbd, TextInput, TextInputEvent};
use crate::styles::{
    ICON_SIZE_SM, RADIUS_SM, SPACE_1, SPACE_2, SPACE_3, SPACE_8, TYPE_BODY, TYPE_CAPTION,
    TextScale, TypeStyleExt,
};
use crate::widgets::{EllipsisExt, icon};

/// Key context of the palette, for its Up and Down bindings.
pub(crate) const KEY_CONTEXT: &str = "CommandPalette";

/// Width of the palette card.
pub(crate) const PALETTE_WIDTH: f32 = 560.0;

/// Height of one result row.
const ROW_HEIGHT: f32 = SPACE_8;

/// The most result rows shown before the list scrolls.
const VISIBLE_ROWS: usize = 8;

/// One row the palette can show.
pub(crate) struct PaletteEntry {
    /// What the row says, and what typing matches first.
    pub(crate) label: SharedString,
    /// What the command does (muted, matched too).
    pub(crate) detail: SharedString,
    pub(crate) icon: &'static str,
    /// The shortcut in GPUI syntax, for [`Kbd`].
    pub(crate) shortcut: Option<SharedString>,
    /// The action to run.
    pub(crate) action: Box<dyn Action>,
}

impl PaletteEntry {
    /// The entry for `command`, if its action exists.
    pub(crate) fn for_command(command: &Command, cx: &App) -> Option<Self> {
        let action = cx
            .build_action(&format!("familychat::{}", command.id), None)
            .ok()?;
        Some(Self {
            label: command.name.into(),
            detail: command.description.into(),
            icon: command.icon,
            shortcut: command.gui_keys.first().map(|key| SharedString::from(*key)),
            action,
        })
    }
}

/// What the palette reports.
pub(crate) enum CommandPaletteEvent {
    /// The user picked this action.
    Picked(Box<dyn Action>),
}

/// An entry that matches the query.
struct PaletteMatch {
    /// Index into the entries.
    entry: usize,
    /// Byte offsets of the matched characters in the entry's label.
    positions: Vec<usize>,
}

/// The searchable list of commands.
pub(crate) struct CommandPalette {
    query: gpui::Entity<TextInput>,
    entries: Vec<PaletteEntry>,
    matches: Vec<PaletteMatch>,
    /// Index into `matches`.
    highlighted: usize,
    scroll: UniformListScrollHandle,
    _subscription: Subscription,
}

impl EventEmitter<CommandPaletteEvent> for CommandPalette {}

impl Focusable for CommandPalette {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

impl CommandPalette {
    /// A palette over `entries`.
    pub(crate) fn new(entries: Vec<PaletteEntry>, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| {
            TextInput::new(cx)
                .compact()
                .leading_icon("search")
                .label("Search commands")
                .placeholder("Search commands…")
        });
        let subscription = cx.subscribe(
            &query,
            |palette, _, event: &TextInputEvent, cx| match event {
                TextInputEvent::Changed => palette.refilter(cx),
                TextInputEvent::Submitted => palette.pick(palette.highlighted, cx),
            },
        );
        let mut palette = Self {
            query,
            entries,
            matches: Vec::new(),
            highlighted: 0,
            scroll: UniformListScrollHandle::new(),
            _subscription: subscription,
        };
        palette.refilter(cx);
        palette
    }

    /// Types `text` into the search field (tests).
    #[cfg(test)]
    pub(crate) fn set_query(&mut self, text: &str, cx: &mut Context<Self>) {
        self.query.update(cx, |query, cx| query.set_text(text, cx));
        self.refilter(cx);
    }

    /// The labels of the matching entries, best first.
    #[cfg(test)]
    pub(crate) fn matching_labels(&self) -> Vec<String> {
        self.matches
            .iter()
            .map(|found| self.entries[found.entry].label.to_string())
            .collect()
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).text().trim().to_owned();
        let mut scored: Vec<(i32, PaletteMatch)> = Vec::new();
        for (index, entry) in self.entries.iter().enumerate() {
            // The label first, so the highlight offsets index into it.
            let text = format!("{} {}", entry.label, entry.detail);
            let Some(found) = fuzzy_match(&query, &text) else {
                continue;
            };
            let positions = found
                .positions
                .into_iter()
                .filter(|&offset| offset < entry.label.len())
                .collect();
            scored.push((
                found.score,
                PaletteMatch {
                    entry: index,
                    positions,
                },
            ));
        }
        if !query.is_empty() {
            scored.sort_by_key(|(score, found)| (std::cmp::Reverse(*score), found.entry));
        }
        self.matches = scored.into_iter().map(|(_, found)| found).collect();
        self.highlighted = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn pick(&mut self, position: usize, cx: &mut Context<Self>) {
        let Some(found) = self.matches.get(position) else {
            return;
        };
        let action = self.entries[found.entry].action.boxed_clone();
        cx.emit(CommandPaletteEvent::Picked(action));
    }

    fn highlight(&mut self, position: usize, cx: &mut Context<Self>) {
        if position < self.matches.len() {
            self.highlighted = position;
            self.scroll
                .scroll_to_item(position, ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        if !self.matches.is_empty() {
            self.highlight((self.highlighted + 1) % self.matches.len(), cx);
        }
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        if !self.matches.is_empty() {
            let count = self.matches.len();
            self.highlight((self.highlighted + count - 1) % count, cx);
        }
    }

    fn select_first(&mut self, _: &SelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        self.highlight(0, cx);
    }

    fn select_last(&mut self, _: &SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        self.highlight(self.matches.len().saturating_sub(1), cx);
    }

    fn render_rows(
        &mut self,
        range: Range<usize>,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let theme = ComponentTheme::get(cx);
        let (colors, platform) = (theme.colors, theme.platform.platform);
        let count = self.matches.len();
        range
            .filter_map(|position| {
                let found = self.matches.get(position)?;
                let entry = &self.entries[found.entry];
                let highlighted = position == self.highlighted;
                let (background, foreground, muted) = if highlighted {
                    (
                        Some(colors.selection_background),
                        colors.selection_foreground,
                        colors.selection_foreground,
                    )
                } else {
                    (None, colors.foreground, colors.muted)
                };
                let bold = HighlightStyle {
                    font_weight: Some(FontWeight::BOLD),
                    ..HighlightStyle::default()
                };
                let highlights = found
                    .positions
                    .iter()
                    .filter_map(|&offset| {
                        let ch = entry.label[offset..].chars().next()?;
                        Some((offset..offset + ch.len_utf8(), bold))
                    })
                    .collect::<Vec<_>>();
                let accessible_name: SharedString =
                    format!("{}, {}", entry.label, entry.detail).into();
                Some(
                    div()
                        .id(("command-palette-entry", position))
                        .hit_target(move || format!("command-palette-{position}"))
                        .flex()
                        .w_full()
                        .items_center()
                        .gap(px(SPACE_2))
                        .h(TextScale::get(cx).fit(ROW_HEIGHT, TYPE_BODY))
                        .px(px(SPACE_2))
                        .rounded(px(RADIUS_SM))
                        .accessible(Role::ListBoxOption, accessible_name)
                        .aria_selected(highlighted)
                        .aria_position_in_set(position + 1)
                        .aria_size_of_set(count)
                        .when_some(entry.shortcut.as_ref(), |this, shortcut| {
                            this.aria_keyshortcuts(spoken_keystrokes(shortcut, platform))
                        })
                        .cursor_pointer()
                        .when_some(background, |this, background| this.bg(rgb(background)))
                        .text_color(rgb(foreground))
                        .on_hover(cx.listener(move |palette, hovered: &bool, _, cx| {
                            if *hovered && palette.highlighted != position {
                                palette.highlighted = position;
                                cx.notify();
                            }
                        }))
                        .on_click(cx.listener(move |palette, _, _, cx| palette.pick(position, cx)))
                        .child(icon(entry.icon, ICON_SIZE_SM, muted))
                        .child(div().flex_none().max_w_full().ellipsis().child(
                            StyledText::new(entry.label.clone()).with_highlights(highlights),
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .ellipsis()
                                .type_style(TYPE_CAPTION)
                                .text_color(rgb(muted))
                                .child(entry.detail.clone()),
                        )
                        .when_some(entry.shortcut.clone(), |this, shortcut| {
                            this.child(Kbd::new(shortcut))
                        })
                        .into_any_element(),
                )
            })
            .collect()
    }
}

impl Render for CommandPalette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = ComponentTheme::get(cx).colors;
        let count = self.matches.len();
        let visible = count.min(VISIBLE_ROWS);
        let highlight = highlight_announcement(
            &self
                .matches
                .get(self.highlighted)
                .map(|found| self.entries[found.entry].label.clone())
                .unwrap_or_default(),
            self.highlighted,
            count,
        );

        div()
            .id("command-palette")
            .key_context(KEY_CONTEXT)
            .relative()
            .child(live_region(
                ("command-palette-highlight", announcement_key(&highlight)),
                highlight,
                Politeness::Polite,
            ))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .flex()
            .flex_col()
            .w_full()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .p(px(SPACE_2))
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .child(self.query.clone()),
            )
            .child(
                div()
                    .id("command-palette-results")
                    .accessible(Role::ListBox, "Commands")
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .p(px(SPACE_1))
                    .when(count > 0, |this| {
                        this.child(
                            uniform_list(
                                "command-palette-list",
                                count,
                                cx.processor(|palette, range: Range<usize>, _, cx| {
                                    palette.render_rows(range, cx)
                                }),
                            )
                            .track_scroll(&self.scroll)
                            .w_full()
                            .min_h_0()
                            .h(TextScale::get(cx).fit(ROW_HEIGHT, TYPE_BODY) * visible as f32),
                        )
                    })
                    .when(count == 0, |this| {
                        this.child(
                            div()
                                .px(px(SPACE_2))
                                .py(px(SPACE_3))
                                .type_style(TYPE_BODY)
                                .text_color(rgb(colors.muted))
                                .child("Nothing matches"),
                        )
                    }),
            )
    }
}
