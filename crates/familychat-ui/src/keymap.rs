//! Loads key bindings from a keymap file. Copied from Zmail's `zmail-ui`
//! (without its keymap choice; Family Chat has one keymap).
//!
//! The format is the subset of Zed's keymap format Family Chat needs: a
//! JSON array of sections, each with an optional `context` predicate and a
//! `bindings` object mapping keystrokes to action names:
//!
//! ```json
//! [
//!   { "context": "Signin", "bindings": { "secondary-1": "familychat::SignInWithCode" } }
//! ]
//! ```
//!
//! Keystrokes use GPUI syntax. `secondary-` means Cmd on macOS and Ctrl on
//! Linux and Windows, so one keymap serves every platform. An action can
//! also be written as `["familychat::Name", { ...arguments }]` for actions
//! that take arguments.
//!
//! The keys are the commands table's (`familychat_core::commands`): every
//! command's action is bound to its default keys here, which the tests
//! check, so the palette, the menus and the keymap agree. The default
//! keymap ships inside the binary, plus a small macOS keymap for the system
//! conventions that only exist there (Hide, Minimize). A user keymap file
//! is later work.

use std::rc::Rc;

use anyhow::{Context as _, Result, anyhow, bail};
use gpui::{App, KeyBinding, KeyBindingContextPredicate};
use serde_json::Value;

use crate::components;

/// The keymap bundled with the app.
pub(crate) const DEFAULT_KEYMAP: &str = include_str!("../keymaps/default.json");

/// macOS-only bindings, loaded after the default keymap on macOS.
pub(crate) const MACOS_KEYMAP: &str = include_str!("../keymaps/macos.json");

/// The bundled keymaps this platform loads, in order.
pub(crate) fn platform_keymaps() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        &[DEFAULT_KEYMAP, MACOS_KEYMAP]
    } else {
        &[DEFAULT_KEYMAP]
    }
}

/// Parses a keymap file into bindings. Every action must be registered and
/// every keystroke and context must parse; the first problem is an error,
/// so a broken keymap fails loudly in tests rather than dropping keys.
pub(crate) fn parse_keymap(source: &str, cx: &App) -> Result<Vec<KeyBinding>> {
    let sections: Value = serde_json::from_str(source).context("the keymap is not valid JSON")?;
    let sections = sections
        .as_array()
        .context("the keymap must be a JSON array of sections")?;

    let mut bindings = Vec::new();
    for (index, section) in sections.iter().enumerate() {
        let section = section
            .as_object()
            .with_context(|| format!("keymap section {index} is not an object"))?;
        let context = match section.get("context") {
            None | Some(Value::Null) => None,
            Some(Value::String(context)) => Some(Rc::new(
                KeyBindingContextPredicate::parse(context)
                    .with_context(|| format!("keymap section {index}: bad context {context:?}"))?,
            )),
            Some(other) => bail!("keymap section {index}: context must be a string, not {other}"),
        };
        let entries = section
            .get("bindings")
            .and_then(Value::as_object)
            .with_context(|| format!("keymap section {index} has no bindings object"))?;

        for (keystrokes, target) in entries {
            let (name, arguments) = match target {
                Value::String(name) => (name.as_str(), None),
                Value::Array(parts) => match parts.as_slice() {
                    [Value::String(name), arguments] => (name.as_str(), Some(arguments.clone())),
                    _ => bail!(
                        "keymap section {index}, {keystrokes:?}: expected [\"action\", {{arguments}}]"
                    ),
                },
                other => bail!("keymap section {index}, {keystrokes:?}: bad action {other}"),
            };
            let action = cx.build_action(name, arguments).map_err(|error| {
                anyhow!("keymap section {index}, {keystrokes:?}: action {name}: {error}")
            })?;
            let binding = KeyBinding::load(keystrokes, action, context.clone(), false, None, cx)
                .map_err(|error| anyhow!("keymap section {index}, {keystrokes:?}: {error}"))?;
            bindings.push(binding);
        }
    }
    Ok(bindings)
}

/// Replaces every key binding with the app's: the components', the text
/// input's and the bundled keymap files. Call it at startup before the
/// menus are set.
///
/// A parse failure is a bug in a bundled file (the tests load them), so it
/// is logged rather than stopping the app.
pub(crate) fn bind_keymap(cx: &mut App) {
    cx.clear_key_bindings();
    cx.bind_keys(components::actions::key_bindings());
    cx.bind_keys(components::text_input::key_bindings());
    for source in platform_keymaps() {
        match parse_keymap(source, cx) {
            Ok(bindings) => cx.bind_keys(bindings),
            Err(error) => eprintln!("familychat: a bundled keymap did not load: {error:#}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use familychat_core::commands::COMMANDS;
    use gpui::{Keystroke, TestAppContext};

    use super::{DEFAULT_KEYMAP, MACOS_KEYMAP, bind_keymap, parse_keymap};
    use crate::actions::{PLATFORM_ACTIONS, ToggleCommandPalette};
    use crate::components::text_input;

    fn init(cx: &mut TestAppContext) {
        cx.update(crate::actions::register);
    }

    #[gpui::test]
    fn binding_the_keymap_replaces_every_binding(cx: &mut TestAppContext) {
        init(cx);
        let count =
            |cx: &mut TestAppContext| cx.update(|cx| cx.key_bindings().borrow().bindings().len());
        cx.update(bind_keymap);
        let bound = count(cx);
        assert!(bound > 0);
        cx.update(|cx| {
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            assert!(
                keymap
                    .bindings_for_action(&ToggleCommandPalette)
                    .next()
                    .is_some(),
                "the keymap files are bound"
            );
            assert!(
                keymap
                    .bindings_for_action(&text_input::Paste)
                    .next()
                    .is_some(),
                "the text input's own keys are bound"
            );
        });
        // Binding again replaces rather than adds.
        cx.update(bind_keymap);
        assert_eq!(count(cx), bound);
    }

    #[gpui::test]
    fn the_bundled_keymaps_load(cx: &mut TestAppContext) {
        init(cx);
        for source in [DEFAULT_KEYMAP, MACOS_KEYMAP] {
            let bindings = cx
                .update(|cx| parse_keymap(source, cx))
                .expect("the bundled keymap parses");
            assert!(!bindings.is_empty());
        }
    }

    /// The commands table and the keymap agree (PLAN.md, section 5.3):
    /// every command's action exists and is bound to exactly the table's
    /// keys, and every `familychat::` action is a command (or one of the
    /// macOS window conventions, which have no terminal equivalent).
    #[gpui::test]
    fn every_command_is_bound_to_its_table_keys(cx: &mut TestAppContext) {
        init(cx);
        let (bound, registered, expected) = cx.update(|cx| {
            let mut bindings = parse_keymap(DEFAULT_KEYMAP, cx).expect("the keymap parses");
            bindings.extend(parse_keymap(MACOS_KEYMAP, cx).expect("the macOS keymap parses"));
            let bound = bindings
                .iter()
                .map(|binding| {
                    let keys = binding
                        .keystrokes()
                        .iter()
                        .map(|keystroke| format!("{:?}", keystroke.inner()))
                        .collect::<Vec<_>>()
                        .join(" ");
                    (binding.action().name().to_owned(), keys)
                })
                .collect::<BTreeSet<_>>();
            let registered = cx
                .all_action_names()
                .iter()
                .copied()
                .filter(|name| name.starts_with("familychat::"))
                .map(str::to_owned)
                .collect::<BTreeSet<_>>();
            let os = cx.platform_info().os;
            let expected = COMMANDS
                .iter()
                .flat_map(|command| command.gui_keys.iter())
                .map(|key| {
                    let keystroke = Keystroke::parse_with_os(key, os).expect("the key parses");
                    ((*key).to_owned(), format!("{keystroke:?}"))
                })
                .collect::<std::collections::BTreeMap<_, _>>();
            (bound, registered, expected)
        });
        let expected_key = |key: &str| expected.get(key).cloned().unwrap_or_default();
        for command in COMMANDS {
            let action = format!("familychat::{}", command.id);
            assert!(registered.contains(&action), "{action} is not an action");
            for key in command.gui_keys {
                assert!(
                    bound.contains(&(action.clone(), expected_key(key))),
                    "{action} is not bound to {key}: {bound:?}"
                );
            }
            let keys = bound.iter().filter(|(name, _)| *name == action).count();
            assert_eq!(
                keys,
                command.gui_keys.len(),
                "{action} has keys the commands table does not list"
            );
        }
        for action in &registered {
            let id = action.trim_start_matches("familychat::");
            assert!(
                COMMANDS.iter().any(|command| command.id == id)
                    || PLATFORM_ACTIONS.iter().any(|(name, _)| *name == id),
                "{action} is not in the commands table"
            );
        }
        for (id, _) in PLATFORM_ACTIONS {
            let action = format!("familychat::{id}");
            assert!(
                bound.iter().any(|(name, _)| *name == action),
                "{action} has no key"
            );
        }
    }

    #[gpui::test]
    fn bad_keymaps_are_reported(cx: &mut TestAppContext) {
        init(cx);
        cx.update(|cx| {
            for (source, expected) in [
                ("{}", "JSON array"),
                (
                    r#"[{"bindings": {"x": "familychat::Nope"}}]"#,
                    "familychat::Nope",
                ),
                (r#"[{"context": "A &&", "bindings": {}}]"#, "bad context"),
                (r#"[{"context": "Shell"}]"#, "no bindings"),
                (r#"[{"bindings": {"x": 3}}]"#, "bad action"),
            ] {
                let error = parse_keymap(source, cx).expect_err(source);
                assert!(
                    format!("{error:#}").contains(expected),
                    "{source}: {error:#}"
                );
            }
        });
    }
}
