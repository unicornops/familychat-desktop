//! The macOS menu bar, from the commands table: the app menu (the command
//! palette, Hide, Hide Others, Quit), a Sign In menu with the sign-in
//! commands and a Rooms menu with the room and timeline commands. AppKit shows each command's first key from the keymap as its
//! key equivalent. Windows and Linux have no menu bar; their commands are
//! in the command palette (Secondary+K).

use familychat_core::{
    commands::{COMMANDS, CommandScope},
    identity::APP_NAME,
};
use gpui::{App, Menu, MenuItem, SystemMenuType};

use crate::actions::{Hide, HideOthers, Quit};

/// Sets the menu bar on macOS; elsewhere does nothing. Call after the
/// keymap is bound.
pub(crate) fn init(cx: &mut App) {
    if cfg!(target_os = "macos") {
        cx.set_menus(menus(cx));
    }
}

/// The menus, built from the commands table.
pub(crate) fn menus(cx: &App) -> Vec<Menu> {
    let command_items = |scope: CommandScope| {
        COMMANDS
            .iter()
            .filter(move |command| command.scope == scope && command.id != "Quit")
            .filter_map(|command| {
                let action = cx
                    .build_action(&format!("familychat::{}", command.id), None)
                    .ok()?;
                Some(MenuItem::Action {
                    name: command.name.into(),
                    action,
                    os_action: None,
                    checked: false,
                    disabled: false,
                })
            })
            .collect::<Vec<_>>()
    };
    let mut app_menu = command_items(CommandScope::Global);
    app_menu.extend([
        MenuItem::separator(),
        MenuItem::os_submenu("Services", SystemMenuType::Services),
        MenuItem::separator(),
        MenuItem::action(format!("Hide {APP_NAME}"), Hide),
        MenuItem::action("Hide Others", HideOthers),
        MenuItem::separator(),
        MenuItem::action(format!("Quit {APP_NAME}"), Quit),
    ]);
    vec![
        Menu::new(APP_NAME).items(app_menu),
        Menu::new("Sign In").items(command_items(CommandScope::SignIn)),
        Menu::new("Rooms").items(command_items(CommandScope::Rooms)),
    ]
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    #[gpui::test]
    fn the_menus_hold_every_command(cx: &mut TestAppContext) {
        cx.update(crate::actions::register);
        let names = cx.update(|cx| {
            menus(cx)
                .into_iter()
                .flat_map(|menu| menu.items)
                .filter_map(|item| match item {
                    MenuItem::Action { name, .. } => Some(name.to_string()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        });
        for command in COMMANDS {
            let expected = if command.id == "Quit" {
                format!("Quit {APP_NAME}")
            } else {
                command.name.to_owned()
            };
            assert!(names.contains(&expected), "{expected} is not in a menu");
        }
    }
}
