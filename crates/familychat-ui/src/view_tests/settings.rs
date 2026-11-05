//! The Settings window (ADR 0015): opening it and moving through its
//! sections from the keyboard, every section's names and 24 px targets,
//! a Family Chat account's guidance and the panel behind the link
//! question, signing out, the sessions, the privacy switches the actions
//! honour, appearance applied live, notifications turned off, a save the
//! profile refuses, resetting the recovery key and the identity, and the
//! diagnostics.

use std::sync::Arc;

use familychat_core::{
    OwnedUserId,
    encryption::{EncryptionStatus, RecoveryState, wording},
    identity::PANEL_URL,
    messaging::RoomRequest,
    notifications::{IncomingNotification, NotificationContent},
    session::{AccountKey, SessionApi as _},
    settings::{
        AppearanceSettings, ContrastChoice, MotionChoice, NOT_SAVED, SettingsApi as _, ThemeChoice,
    },
    sync::SyncState,
};
use familychat_test_support::fixtures::{
    FixtureProfile, FixtureSession, FixtureSignin, FixtureStartup,
    encryption::{EncryptionAsk, FIXTURE_NEW_RECOVERY_KEY, FIXTURE_PASSWORD, settled},
    fixture_user,
    rooms::FAMILY_ROOM,
};
use gpui::{AppContext as _, Entity, Role, TestAppContext, VisualTestContext};

use super::a11y::redraw;
use super::hit_targets::check_targets;
use super::{click, keys, open, rooms::rooms_view, wait_until};
use crate::a11y::audit;
use crate::components::ComponentTheme;
use crate::settings::{self, SettingsWindow};
use crate::shell::Screen;
use crate::styles::TextScale;
use crate::styles::high_contrast::{Contrast, ContrastSetting};
use crate::{
    EncryptionLaunch, InitialView, RoomsLaunch, SettingsLaunch, SettingsOpen, SettingsSection,
    ShellView,
};

/// The open Settings window's root, and a test context for that window.
pub(super) fn settings_window(
    cx: &mut VisualTestContext,
) -> (Entity<SettingsWindow>, VisualTestContext) {
    cx.run_until_parked();
    let handle = cx
        .update(|_, cx| settings::window(cx))
        .expect("the Settings window is open");
    let root = handle
        .root(cx)
        .expect("the Settings window has its root view");
    let window_cx = VisualTestContext::from_window(handle.into(), cx);
    window_cx.run_until_parked();
    (root, window_cx)
}

struct Setup {
    session: Arc<FixtureSession>,
    startup: Arc<FixtureStartup>,
    account: AccountKey,
}

fn setup_as(user: OwnedUserId, status: EncryptionStatus) -> Setup {
    let session = FixtureSession::new();
    let account = session.add_account(&user, SyncState::Running);
    session.with_family();
    session.set_encryption(&account, status);
    let signin = FixtureSignin::new(session.clone());
    let startup = FixtureStartup::with(session.clone(), signin, &[FixtureProfile::Ready]);
    Setup {
        session,
        startup,
        account,
    }
}

fn setup() -> Setup {
    setup_as(fixture_user(), settled())
}

/// The main window on the family's rooms with Settings open as `launch`
/// says.
fn open_settings<'a>(
    cx: &'a mut TestAppContext,
    setup: &Setup,
    launch: SettingsLaunch,
) -> (
    Entity<ShellView>,
    &'a mut VisualTestContext,
    Entity<SettingsWindow>,
    VisualTestContext,
) {
    let (view, cx) = open(cx, setup.startup.clone(), InitialView::Settings(launch));
    let (root, mut window_cx) = settings_window(cx);
    wait_until(&mut window_cx, "the account's details", |cx| {
        root.read_with(cx, |root, _| root.details().is_some())
    });
    (view, cx, root, window_cx)
}

fn on(section: SettingsSection) -> SettingsLaunch {
    SettingsLaunch {
        section,
        open: None,
    }
}

fn with(section: SettingsSection, open: SettingsOpen) -> SettingsLaunch {
    SettingsLaunch {
        section,
        open: Some(open),
    }
}

fn assert_all_named(what: &str, entries: &[(Role, String)]) {
    assert!(!entries.is_empty(), "{what}: nothing was recorded");
    let unnamed: Vec<Role> = entries
        .iter()
        .filter(|(_, label)| label.trim().is_empty())
        .map(|(role, _)| *role)
        .collect();
    assert!(unnamed.is_empty(), "{what}: unnamed controls: {unnamed:?}");
}

fn has(entries: &[(Role, String)], role: Role, label: &str) -> bool {
    audit::labels(entries, role).contains(&label)
}

#[gpui::test]
fn secondary_comma_opens_one_settings_window_and_the_keys_move_through_it(cx: &mut TestAppContext) {
    let setup = setup();
    let (_view, cx) = open(
        cx,
        setup.startup.clone(),
        InitialView::Rooms(RoomsLaunch::default()),
    );
    assert!(cx.update(|_, cx| settings::window(cx)).is_none());
    let windows = cx.update(|_, cx| cx.windows().len());

    keys("secondary-,", cx);
    let (root, mut window_cx) = settings_window(cx);
    assert_eq!(cx.update(|_, cx| cx.windows().len()), windows + 1);
    assert_eq!(
        root.read_with(&window_cx, |root, _| root.section()),
        SettingsSection::Account
    );

    // The section list has focus: Down and Up move, End and Home jump.
    keys("down", &mut window_cx);
    assert_eq!(
        root.read_with(&window_cx, |root, _| root.section()),
        SettingsSection::Sessions
    );
    keys("end", &mut window_cx);
    assert_eq!(
        root.read_with(&window_cx, |root, _| root.section()),
        SettingsSection::Diagnostics
    );
    keys("up", &mut window_cx);
    assert_eq!(
        root.read_with(&window_cx, |root, _| root.section()),
        SettingsSection::Notifications
    );
    click("settings-nav-appearance", &mut window_cx);
    assert_eq!(
        root.read_with(&window_cx, |root, _| root.section()),
        SettingsSection::Appearance
    );

    // Asking again brings the same window forward, on the section asked.
    keys("secondary-alt-n", cx);
    assert_eq!(cx.update(|_, cx| cx.windows().len()), windows + 1);
    assert_eq!(
        root.read_with(&window_cx, |root, _| root.section()),
        SettingsSection::Notifications
    );

    // Escape closes it.
    keys("escape", &mut window_cx);
    assert!(cx.update(|_, cx| settings::window(cx)).is_none());
    assert_eq!(cx.update(|_, cx| cx.windows().len()), windows);
}

#[gpui::test]
fn every_section_names_its_controls_and_its_targets_are_24px(cx: &mut TestAppContext) {
    let setup = setup();
    let (_view, _cx, root, mut window_cx) = open_settings(cx, &setup, on(SettingsSection::Account));
    for section in SettingsSection::ALL {
        window_cx.update(|window, cx| {
            root.update(cx, |root, cx| root.show_section(section, window, cx));
        });
        if section == SettingsSection::Sessions {
            wait_until(&mut window_cx, "the devices", |cx| {
                root.read_with(cx, |root, cx| {
                    root.encryption()
                        .is_some_and(|encryption| encryption.read(cx).devices().is_some())
                })
            });
        }
        let entries = redraw(&mut window_cx);
        assert_all_named(section.label(), &entries);
        assert!(
            has(&entries, Role::Heading, section.label()),
            "{}: {entries:?}",
            section.label()
        );
        assert!(has(&entries, Role::TabList, "Settings sections"));
        for tab in SettingsSection::ALL {
            assert!(has(&entries, Role::Tab, tab.label()), "{}", tab.label());
        }
        assert!(
            check_targets(section.label(), &mut window_cx) > SettingsSection::ALL.len(),
            "{} draws its targets",
            section.label()
        );
    }

    // The largest text keeps every target at least as big.
    drop(
        setup
            .startup
            .settings()
            .set_appearance_settings(AppearanceSettings {
                text_size: 200,
                ..AppearanceSettings::default()
            }),
    );
    window_cx.run_until_parked();
    for section in SettingsSection::ALL {
        window_cx.update(|window, cx| {
            root.update(cx, |root, cx| root.show_section(section, window, cx));
        });
        check_targets(section.label(), &mut window_cx);
    }
}

#[gpui::test]
fn every_settings_dialog_is_named_and_its_targets_are_24px(cx: &mut TestAppContext) {
    for (launch, title) in [
        (
            with(SettingsSection::Account, SettingsOpen::SignOut),
            "Sign out of @ana:smith.safechat.family?",
        ),
        (
            with(SettingsSection::Account, SettingsOpen::PanelLink),
            "Open this link?",
        ),
        (
            with(
                SettingsSection::Privacy,
                SettingsOpen::Encryption(EncryptionLaunch::ResetRecoveryKey),
            ),
            wording::RESET_KEY_TITLE,
        ),
        (
            with(
                SettingsSection::Privacy,
                SettingsOpen::Encryption(EncryptionLaunch::ResetIdentityPassword),
            ),
            wording::RESET_IDENTITY_TITLE,
        ),
        (
            with(
                SettingsSection::Privacy,
                SettingsOpen::Encryption(EncryptionLaunch::Export),
            ),
            wording::EXPORT_TITLE,
        ),
    ] {
        let setup = setup();
        let (_view, _cx, _root, mut window_cx) = open_settings(cx, &setup, launch);
        let entries = redraw(&mut window_cx);
        assert_all_named(title, &entries);
        let dialogs = audit::labels(&entries, Role::Dialog);
        assert!(
            dialogs.iter().any(|dialog| dialog.starts_with(title)),
            "{title}: {dialogs:?}"
        );
        assert!(check_targets(title, &mut window_cx) >= 2, "{title}");
    }
}

#[gpui::test]
fn a_family_chat_account_points_to_the_panel_after_the_link_question(cx: &mut TestAppContext) {
    let setup = setup();
    let (_view, _cx, root, mut window_cx) = open_settings(cx, &setup, on(SettingsSection::Account));
    let details = root
        .read_with(&window_cx, |root, _| root.details().cloned())
        .expect("answered")
        .expect("the details");
    assert!(details.family_chat);
    let entries = redraw(&mut window_cx);
    assert!(
        audit::labels(&entries, Role::Button)
            .iter()
            .any(|label| label.starts_with("Open the Family Chat panel")),
        "{entries:?}"
    );
    assert!(has(&entries, Role::Heading, "Managed by your family"));

    click("button-settings-open-panel", &mut window_cx);
    assert_eq!(
        root.read_with(&window_cx, |root, _| root.dialog_name()),
        Some("link")
    );
    assert_eq!(
        root.read_with(&window_cx, |root, _| root.link_target().map(str::to_owned)),
        Some(PANEL_URL.to_owned()),
        "the whole address is asked about"
    );
    let entries = redraw(&mut window_cx);
    assert!(
        has(&entries, Role::Dialog, "Open this link?"),
        "{entries:?}"
    );
    // Cancel opens nothing; Open hands the address to the OS.
    click("button-link-cancel", &mut window_cx);
    assert_eq!(
        root.read_with(&window_cx, |root, _| root.dialog_name()),
        None
    );
    assert_eq!(window_cx.opened_url(), None);
    click("button-settings-open-panel", &mut window_cx);
    click("button-link-open", &mut window_cx);
    assert_eq!(
        window_cx.opened_url().as_deref(),
        Some(PANEL_URL),
        "opened only after the question"
    );
}

#[gpui::test]
fn another_servers_account_gets_no_panel(cx: &mut TestAppContext) {
    let user = OwnedUserId::try_from("@sam:matrix.example.org").expect("an id");
    let setup = setup_as(user, settled());
    let (_view, _cx, root, mut window_cx) = open_settings(cx, &setup, on(SettingsSection::Account));
    let details = root
        .read_with(&window_cx, |root, _| root.details().cloned())
        .expect("answered")
        .expect("the details");
    assert!(!details.family_chat);
    assert!(
        super::targets_named(&mut window_cx, "button-settings-open-panel").is_empty(),
        "no panel for another server"
    );
    let entries = redraw(&mut window_cx);
    assert!(has(&entries, Role::Heading, "Your server"), "{entries:?}");
    assert!(!has(&entries, Role::Heading, "Managed by your family"));
}

#[gpui::test]
fn signing_out_removes_the_account_and_returns_to_the_sign_in(cx: &mut TestAppContext) {
    let setup = setup_as(
        fixture_user(),
        EncryptionStatus {
            recovery: RecoveryState::Disabled,
            other_devices: false,
            ..settled()
        },
    );
    let (view, cx, root, mut window_cx) = open_settings(cx, &setup, on(SettingsSection::Account));
    assert!(!super::targets_named(&mut window_cx, "button-settings-sign-out").is_empty());
    let ask = |window_cx: &mut VisualTestContext| {
        window_cx.update(|window, cx| {
            root.update(cx, |root, cx| root.ask_to_sign_out(window, cx));
        });
        window_cx.run_until_parked();
    };
    ask(&mut window_cx);
    assert_eq!(
        root.read_with(&window_cx, |root, _| root.dialog_name()),
        Some("sign-out")
    );
    // Nothing else could bring the history back: the dialog says so.
    let entries = redraw(&mut window_cx);
    assert!(
        entries
            .iter()
            .any(|(_, label)| label.contains("haven't set up recovery")),
        "{entries:?}"
    );
    // Cancel signs nobody out.
    click("button-settings-sign-out-cancel", &mut window_cx);
    assert_eq!(
        root.read_with(&window_cx, |root, _| root.dialog_name()),
        None
    );
    assert!(setup.session.record().signed_out.is_empty());

    ask(&mut window_cx);
    click("button-settings-sign-out-confirm", &mut window_cx);
    wait_until(cx, "the sign-in", |cx| {
        cx.read_entity(&view, |view, _| matches!(view.screen(), Screen::Signin(_)))
    });
    assert_eq!(
        setup.session.record().signed_out,
        std::slice::from_ref(&setup.account)
    );
    assert!(setup.session.accounts().borrow().is_empty());
    assert!(
        cx.update(|_, cx| settings::window(cx)).is_none(),
        "Settings closed"
    );
    let toasts = cx.read_entity(&view, |view, cx| {
        view.toasts()
            .read(cx)
            .visible()
            .into_iter()
            .map(|(_, message)| message.to_string())
            .collect::<Vec<_>>()
    });
    assert!(
        toasts.iter().any(|toast| toast.starts_with("Signed out")),
        "{toasts:?}"
    );
    assert_eq!(cx.window_title().as_deref(), Some("Family Chat"));
}

#[gpui::test]
fn sessions_list_the_devices_and_sign_one_out_in_the_page(cx: &mut TestAppContext) {
    let setup = setup();
    let (_view, _cx, root, mut window_cx) =
        open_settings(cx, &setup, on(SettingsSection::Sessions));
    let encryption = root
        .read_with(&window_cx, |root, _| root.encryption().cloned())
        .expect("the account's encryption");
    wait_until(&mut window_cx, "the devices", |cx| {
        encryption.read_with(cx, |encryption, _| encryption.devices().is_some())
    });
    let entries = redraw(&mut window_cx);
    assert!(has(&entries, Role::List, wording::DEVICES_TITLE));
    let rows = audit::labels(&entries, Role::ListItem);
    assert!(
        rows.iter().any(|row| row.starts_with("Old tablet")),
        "{rows:?}"
    );

    // The old tablet is the third device (this one first).
    click("button-encryption-device-sign-out-2", &mut window_cx);
    click("button-encryption-sign-out-confirm", &mut window_cx);
    let password = encryption.read_with(&window_cx, |encryption, _| encryption.inputs()[0].clone());
    window_cx.update(|_, cx| password.update(cx, |input, cx| input.set_text(FIXTURE_PASSWORD, cx)));
    click("button-encryption-sign-out-confirm", &mut window_cx);
    wait_until(&mut window_cx, "the device gone", |cx| {
        encryption.read_with(cx, |encryption, _| {
            encryption.devices().is_some_and(|devices| {
                devices.as_ref().is_ok_and(|devices| {
                    devices.iter().all(|device| device.device_id != "OLDTABLET")
                })
            })
        })
    });
    // In Settings the dialog closes back to the page.
    assert!(!encryption.read_with(&window_cx, |encryption, _| encryption.dialog_open()));
    assert!(
        setup
            .session
            .encryption_asks()
            .contains(&EncryptionAsk::SignOutDevice {
                device_id: "OLDTABLET".to_owned(),
                with_password: true,
            })
    );
}

#[gpui::test]
fn read_receipts_and_typing_turned_off_are_honoured(cx: &mut TestAppContext) {
    let setup = setup();
    let (view, cx) = open(
        cx,
        setup.startup.clone(),
        InitialView::Rooms(RoomsLaunch {
            room: Some(FAMILY_ROOM.to_owned()),
            ..RoomsLaunch::default()
        }),
    );
    let timeline = super::rooms::timeline(&view, cx);
    keys("secondary-,", cx);
    let (root, mut window_cx) = settings_window(cx);
    click("settings-nav-privacy", &mut window_cx);
    click("button-settings-read-receipts", &mut window_cx);
    click("button-settings-typing-notices", &mut window_cx);
    let privacy = *setup.startup.settings().privacy_settings().borrow();
    assert!(
        !privacy.read_receipts && !privacy.typing_notices,
        "{privacy:?}"
    );
    assert_eq!(
        root.read_with(&window_cx, |root, _| root.shown_settings().1),
        privacy
    );
    keys("escape", &mut window_cx);

    // The main window's room: reading it sends a private receipt, typing
    // sends nothing.
    cx.update(|window, _| window.activate_window());
    let items = timeline.read_with(cx, |timeline, _| timeline.items().to_vec());
    setup.session.set_timeline(FAMILY_ROOM, items);
    wait_until(cx, "the private receipt", |_| {
        setup
            .session
            .record()
            .requests
            .iter()
            .any(|(_, request)| *request == RoomRequest::MarkReadPrivately)
    });
    keys("secondary-l", cx);
    cx.simulate_input("Typing quietly");
    cx.run_until_parked();
    let after: Vec<RoomRequest> = setup
        .session
        .record()
        .requests
        .into_iter()
        .map(|(_, request)| request)
        .skip_while(|request| *request != RoomRequest::MarkReadPrivately)
        .collect();
    assert!(
        !after
            .iter()
            .any(|request| matches!(request, RoomRequest::Typing(_) | RoomRequest::MarkRead)),
        "{after:?}"
    );
    drop(rooms_view(&view, cx));
}

#[gpui::test]
fn appearance_applies_to_every_window_at_once(cx: &mut TestAppContext) {
    let setup = setup();
    let (view, cx, _root, mut window_cx) =
        open_settings(cx, &setup, on(SettingsSection::Appearance));
    // The fixture pins dark; Light wins over it.
    click("button-settings-theme-1", &mut window_cx);
    assert_eq!(
        setup
            .startup
            .settings()
            .appearance_settings()
            .borrow()
            .theme,
        ThemeChoice::Light
    );
    cx.run_until_parked();
    assert_eq!(
        cx.read_entity(&view, |view, _| view.theme().appearance),
        familychat_core::ThemeAppearance::Light
    );
    assert!(cx.update(|_, cx| ComponentTheme::get(cx).is_light));

    click("button-settings-motion-1", &mut window_cx);
    click("button-settings-contrast-1", &mut window_cx);
    click("button-settings-text-size-4", &mut window_cx);
    cx.run_until_parked();
    let applied = cx.read_entity(&view, |view, _| view.appearance_settings());
    assert_eq!(
        applied,
        AppearanceSettings {
            theme: ThemeChoice::Light,
            text_size: 200,
            motion: MotionChoice::Reduce,
            contrast: ContrastChoice::Increased,
        }
    );
    cx.update(|_, cx| {
        assert_eq!(TextScale::get(cx), TextScale::from_percent(200));
        assert!(cx.reduce_motion(), "motion is reduced");
        assert_eq!(ContrastSetting::get(cx), Contrast::Increased);
    });

    // Back to the system's choices (the default text size first, so every
    // row is in the window again).
    click("button-settings-text-size-0", &mut window_cx);
    click("button-settings-theme-0", &mut window_cx);
    click("button-settings-motion-2", &mut window_cx);
    click("button-settings-contrast-0", &mut window_cx);
    cx.run_until_parked();
    cx.update(|_, cx| {
        assert!(!cx.reduce_motion(), "Full motion");
        assert_eq!(ContrastSetting::get(cx), Contrast::Standard);
    });
    assert_eq!(
        cx.read_entity(&view, |view, _| view.theme().appearance),
        familychat_core::ThemeAppearance::Dark,
        "Match system is the fixture's dark"
    );
}

#[gpui::test]
fn a_setting_the_profile_refuses_is_taken_back_and_said(cx: &mut TestAppContext) {
    let setup = setup();
    let (_view, _cx, root, mut window_cx) =
        open_settings(cx, &setup, on(SettingsSection::Notifications));
    setup.startup.settings().refuse_saves(true);
    click("button-settings-notifications-enabled", &mut window_cx);
    window_cx.run_until_parked();
    assert!(root.read_with(&window_cx, |root, _| root.shown_settings().0.enabled));
    let toasts = root.read_with(&window_cx, |root, cx| root.toast_messages(cx));
    assert!(toasts.iter().any(|toast| toast == NOT_SAVED), "{toasts:?}");
}

#[gpui::test]
fn notifications_turned_off_post_nothing(cx: &mut TestAppContext) {
    let setup = setup();
    let (_view, cx, _root, mut window_cx) =
        open_settings(cx, &setup, on(SettingsSection::Notifications));
    click("button-settings-notifications-enabled", &mut window_cx);
    assert!(
        !setup
            .startup
            .settings()
            .notification_settings()
            .borrow()
            .enabled
    );
    // The message text switch can't change while they're off.
    let entries = redraw(&mut window_cx);
    assert!(has(
        &entries,
        Role::CheckBox,
        "Show message text in notifications"
    ));
    keys("escape", &mut window_cx);
    cx.deactivate_window();
    assert!(setup.session.notify(IncomingNotification {
        account: setup.account.clone(),
        room_id: FAMILY_ROOM.try_into().expect("an id"),
        room_name: "Smith family".to_owned(),
        sender_name: "Mum".to_owned(),
        is_direct: false,
        content: NotificationContent::Text("Dinner".to_owned()),
    }));
    cx.run_until_parked();
    cx.executor()
        .advance_clock(crate::notifications::BURST_WAIT * 2);
    cx.run_until_parked();
    assert!(
        cx.shown_system_notifications().is_empty(),
        "nothing was posted"
    );
}

#[gpui::test]
fn the_recovery_key_is_changed_and_shown_once(cx: &mut TestAppContext) {
    let setup = setup();
    let (_view, _cx, root, mut window_cx) = open_settings(cx, &setup, on(SettingsSection::Privacy));
    let encryption = root
        .read_with(&window_cx, |root, _| root.encryption().cloned())
        .expect("the account's encryption");
    click("button-settings-recovery-change", &mut window_cx);
    click("button-encryption-reset-key-confirm", &mut window_cx);
    wait_until(&mut window_cx, "the new key", |cx| {
        encryption.read_with(cx, |encryption, _| encryption.recovery_key_state().0)
    });
    let entries = redraw(&mut window_cx);
    assert!(
        entries
            .iter()
            .any(|(_, label)| label.contains(FIXTURE_NEW_RECOVERY_KEY)),
        "the new key shows"
    );
    assert!(
        setup
            .session
            .encryption_asks()
            .contains(&EncryptionAsk::ResetRecoveryKey)
    );
    click("button-encryption-recovery-copy", &mut window_cx);
    click("button-encryption-recovery-done", &mut window_cx);
    assert_eq!(
        encryption.read_with(&window_cx, |encryption, _| encryption.recovery_key_state()),
        (false, false),
        "the key is gone once the dialog closes"
    );
}

#[gpui::test]
fn resetting_the_identity_asks_for_the_password_the_server_wants(cx: &mut TestAppContext) {
    let setup = setup();
    let (_view, _cx, root, mut window_cx) = open_settings(cx, &setup, on(SettingsSection::Privacy));
    let encryption = root
        .read_with(&window_cx, |root, _| root.encryption().cloned())
        .expect("the account's encryption");
    click("button-settings-reset-identity", &mut window_cx);
    click("button-encryption-reset-identity-confirm", &mut window_cx);
    wait_until(&mut window_cx, "the password field", |_| {
        setup
            .session
            .encryption_asks()
            .contains(&EncryptionAsk::ResetIdentity {
                with_password: false,
            })
    });
    let password = encryption.read_with(&window_cx, |encryption, _| encryption.inputs()[0].clone());
    window_cx.update(|_, cx| password.update(cx, |input, cx| input.set_text(FIXTURE_PASSWORD, cx)));
    click("button-encryption-reset-identity-confirm", &mut window_cx);
    wait_until(&mut window_cx, "the reset", |cx| {
        !encryption.read_with(cx, |encryption, _| encryption.dialog_open())
    });
    assert!(
        setup
            .session
            .encryption_asks()
            .contains(&EncryptionAsk::ResetIdentity {
                with_password: true,
            })
    );
    assert_eq!(
        password.read_with(&window_cx, |input, _| input.text().to_owned()),
        "",
        "the password is wiped"
    );
    assert!(
        !format!("{:?}", setup.session.encryption_asks()).contains(FIXTURE_PASSWORD),
        "no password is recorded"
    );
}

#[gpui::test]
fn diagnostics_show_the_version_the_pin_and_the_logs(cx: &mut TestAppContext) {
    let setup = setup();
    let (_view, _cx, root, mut window_cx) =
        open_settings(cx, &setup, on(SettingsSection::Diagnostics));
    let entries = redraw(&mut window_cx);
    assert!(has(&entries, Role::Button, "Open logs folder"));
    assert!(has(&entries, Role::Button, "Copy version details"));
    click("button-settings-copy-diagnostics", &mut window_cx);
    let copied = window_cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .unwrap_or_default();
    assert!(
        copied.starts_with("Family Chat 0.0.1\nGPUI: Zed v1.27.0"),
        "{copied}"
    );
    assert!(!copied.contains("ana"), "the details name nobody: {copied}");
    // The fixture's folder doesn't exist: it says so instead of opening.
    click("button-settings-open-logs", &mut window_cx);
    let toasts = root.read_with(&window_cx, |root, cx| root.toast_messages(cx));
    assert!(
        toasts
            .iter()
            .any(|toast| toast == "There are no log files yet."),
        "{toasts:?}"
    );
}

#[gpui::test]
fn the_rooms_grow_with_the_text_size(cx: &mut TestAppContext) {
    let setup = setup();
    let (_view, cx) = open(
        cx,
        setup.startup.clone(),
        InitialView::Rooms(RoomsLaunch {
            room: Some(FAMILY_ROOM.to_owned()),
            ..RoomsLaunch::default()
        }),
    );
    wait_until(cx, "the room list", |cx| {
        cx.debug_bounds("room-row-0").is_some()
    });
    let height = |cx: &mut VisualTestContext| {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        f32::from(cx.debug_bounds("room-row-0").expect("a row").size.height)
    };
    let before = height(cx);
    drop(
        setup
            .startup
            .settings()
            .set_appearance_settings(AppearanceSettings {
                text_size: 200,
                ..AppearanceSettings::default()
            }),
    );
    cx.run_until_parked();
    let after = height(cx);
    // Two lines of text, each twice as tall: the row grows by as much.
    assert!(
        after >= before + 36.0,
        "a room row is {before} px at 100% and {after} px at 200%"
    );
    check_targets("the rooms at 200%", cx);
}
