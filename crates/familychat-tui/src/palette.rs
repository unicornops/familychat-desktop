//! The command palette (`:`), for people who don't know the letters (ADR
//! 0008): every command of the current screen, with its key, filtered as
//! you type with the window palette's fuzzy matcher
//! ([`familychat_core::fuzzy`]). Enter (or a click) runs the one under the
//! cursor.
//!
//! Adapted from Zmail's `zmail-tui` (`src/palette.rs`), Copyright
//! UnicornOps, GPL-3.0-only like this project; keep this notice. The list
//! is the screen's command bar ([`App::commands`]), every page of it, with
//! the shared table's names and descriptions, so the palette, the bar and
//! the window's palette can't disagree.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use familychat_core::fuzzy::fuzzy_match;

use crate::{
    app::{App, CommandEntry, Prompt},
    commands::Cmd,
    pane::PaneId,
};

/// A command the palette lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteItem {
    /// The command bar's entry: its key, label and command.
    pub entry: CommandEntry,
    /// Its name: the shared table's, or the bar's label.
    pub name: &'static str,
    /// What the command does, in words.
    pub description: &'static str,
}

impl PaletteItem {
    /// The text the query is matched against: the name, a space, the
    /// description.
    pub fn text(&self) -> String {
        format!("{} {}", self.name, self.description)
    }
}

/// The palette's state while it is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    /// Every command of the screen it was opened on, in the bar's order.
    pub items: Vec<PaletteItem>,
    /// What has been typed.
    pub query: String,
    /// The items that match, best first, with the byte offsets of the
    /// matched characters in [`PaletteItem::text`].
    pub shown: Vec<(usize, Vec<usize>)>,
    /// The row the cursor is on.
    pub cursor: usize,
}

impl Palette {
    /// The palette over `entries`, the command bar's (every page).
    pub fn new(entries: Vec<CommandEntry>) -> Self {
        let items = entries
            .into_iter()
            .filter(|entry| !matches!(entry.cmd, Cmd::OtherCmds | Cmd::Palette))
            .map(|entry| PaletteItem {
                // The table's name, or this screen's label for the
                // terminal's own keys (Enter is "Choose" or "Open").
                name: entry.cmd.shared().map_or(entry.label, |row| row.name),
                description: entry.cmd.description(),
                entry,
            })
            .collect();
        let mut palette = Self {
            items,
            query: String::new(),
            shown: Vec::new(),
            cursor: 0,
        };
        palette.filter();
        palette
    }

    /// Matches the query again: the bar's order when it is blank, else
    /// best first (the bar's order among equals).
    pub fn filter(&mut self) {
        let mut shown: Vec<(i32, usize, Vec<usize>)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let found = fuzzy_match(&self.query, &item.text())?;
                Some((found.score, index, found.positions))
            })
            .collect();
        shown.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
        self.shown = shown
            .into_iter()
            .map(|(_, index, positions)| (index, positions))
            .collect();
        self.cursor = 0;
    }

    /// The command under the cursor.
    pub fn current(&self) -> Option<Cmd> {
        let (index, _) = self.shown.get(self.cursor)?;
        Some(self.items[*index].entry.cmd)
    }
}

impl App {
    /// `:`: the palette over this screen's commands.
    pub(crate) fn open_palette(&mut self) {
        let palette = Palette::new(self.commands());
        self.scroll.insert(PaneId::Palette, 0);
        self.prompt = Some(Prompt::Palette(palette));
    }

    /// Runs the command under the palette's cursor, as its key would.
    fn run_from_palette(&mut self, palette: &Palette) {
        self.prompt = None;
        if let Some(cmd) = palette.current() {
            self.run(cmd);
        }
    }

    /// A key while the palette is open.
    pub(crate) fn palette_key(&mut self, mut palette: Palette, key: KeyEvent) {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let last = palette.shown.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Char('c' | 'g') if control => return,
            KeyCode::Enter => return self.run_from_palette(&palette),
            KeyCode::Up | KeyCode::BackTab => palette.cursor = palette.cursor.saturating_sub(1),
            KeyCode::Char('p') if control => palette.cursor = palette.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => palette.cursor = (palette.cursor + 1).min(last),
            KeyCode::Char('n') if control => palette.cursor = (palette.cursor + 1).min(last),
            KeyCode::PageUp | KeyCode::Home => palette.cursor = 0,
            KeyCode::PageDown | KeyCode::End => palette.cursor = last,
            KeyCode::Backspace => {
                // Backspace in an empty palette closes it, as in a shell.
                if palette.query.pop().is_none() {
                    return;
                }
                palette.filter();
            }
            KeyCode::Char('u') if control => {
                palette.query.clear();
                palette.filter();
            }
            KeyCode::Char(ch) if !control && !ch.is_control() => {
                palette.query.push(ch);
                palette.filter();
            }
            _ => {}
        }
        self.reveal.insert(PaneId::Palette);
        self.prompt = Some(Prompt::Palette(palette));
    }

    /// The mouse over the palette: a click runs a command, the wheel
    /// moves. Returns whether the palette took the event.
    pub(crate) fn palette_mouse(&mut self, mouse: MouseEvent) -> bool {
        let Some(Prompt::Palette(palette)) = &self.prompt else {
            return false;
        };
        let Some(pane) = self
            .pane(PaneId::Palette)
            .filter(|pane| pane.contains(mouse.column, mouse.row))
        else {
            return false;
        };
        let target = pane
            .lines
            .get(pane.point_at(mouse.column, mouse.row).line)
            .and_then(|line| line.target);
        let mut palette = palette.clone();
        let last = palette.shown.len().saturating_sub(1);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(target) = target {
                    palette.cursor = target;
                    self.run_from_palette(&palette);
                }
                return true;
            }
            MouseEventKind::ScrollUp => palette.cursor = palette.cursor.saturating_sub(1),
            MouseEventKind::ScrollDown => palette.cursor = (palette.cursor + 1).min(last),
            _ => return true,
        }
        self.reveal.insert(PaneId::Palette);
        self.prompt = Some(Prompt::Palette(palette));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette() -> Palette {
        Palette::new(vec![
            CommandEntry::of(Cmd::SignInWithCode),
            CommandEntry::of(Cmd::SignInWithPassword),
            CommandEntry::of(Cmd::UseAnotherServer),
            CommandEntry::of(Cmd::Quit),
            CommandEntry::of(Cmd::OtherCmds),
        ])
    }

    fn names(palette: &Palette) -> Vec<&'static str> {
        palette
            .shown
            .iter()
            .map(|(index, _)| palette.items[*index].entry.label)
            .collect()
    }

    #[test]
    fn a_blank_query_lists_the_bar_in_order_without_other_cmds() {
        let palette = palette();
        assert_eq!(names(&palette), ["Code", "Password", "OtherSrv", "Quit"]);
        assert_eq!(palette.current(), Some(Cmd::SignInWithCode));
    }

    #[test]
    fn typing_filters_by_the_tables_name_or_description() {
        let mut palette = palette();
        palette.query = "parent".to_owned();
        palette.filter();
        assert_eq!(names(&palette)[0], "Code");
        palette.query = "homeserver".to_owned();
        palette.filter();
        assert_eq!(names(&palette), ["OtherSrv"]);
        palette.query = "zzz".to_owned();
        palette.filter();
        assert!(names(&palette).is_empty());
        assert_eq!(palette.current(), None);
    }
}
