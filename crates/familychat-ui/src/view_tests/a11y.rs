//! Screen-reader names (PLAN.md, section 5.10; Zmail's accessibility
//! baseline): every control each screen draws has a non-empty accessible
//! label, and the screens, forms and dialogs say what they are.
//!
//! GPUI's test platform never turns AccessKit on, so these tests read the
//! roles and labels `AccessibleExt::accessible` set while the views render
//! (see `crate::a11y::audit`), as Zmail's do.

use std::time::Duration;

use familychat_core::{signin::SigninStep, sync::SyncState};
use familychat_test_support::fixtures::{
    FIXTURE_LINK, FixtureProfile, FixtureSession, FixtureSignin, FixtureStartup, fixture_user,
    steps,
};
use gpui::{Role, TestAppContext, VisualTestContext};

use super::{keys, open, open_signin, wait_for_step};
use crate::InitialView;
use crate::a11y::audit;

/// Fails on any empty label, naming its role.
fn assert_all_named(entries: &[(Role, String)]) {
    assert!(!entries.is_empty(), "nothing was recorded");
    let unnamed: Vec<Role> = entries
        .iter()
        .filter(|(_, label)| label.trim().is_empty())
        .map(|(role, _)| *role)
        .collect();
    assert!(unnamed.is_empty(), "unnamed controls: {unnamed:?}");
}

fn has(entries: &[(Role, String)], role: Role, label: &str) -> bool {
    audit::labels(entries, role).contains(&label)
}

/// Redraws the window and returns what it recorded.
fn redraw(cx: &mut VisualTestContext) -> Vec<(Role, String)> {
    audit::start();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    audit::finish()
}

#[gpui::test]
fn the_welcome_and_code_pages_name_every_control(cx: &mut TestAppContext) {
    let (_view, cx, _) = open_signin(cx, InitialView::Default);
    let welcome = redraw(cx);
    assert_all_named(&welcome);
    assert!(has(&welcome, Role::Heading, "Welcome to Family Chat"));
    assert!(has(&welcome, Role::Group, "Ways to sign in"));
    let buttons = audit::labels(&welcome, Role::Button);
    for option in [
        "Sign in with a code from your parent",
        "Sign in with your password",
        "Use another Matrix server",
    ] {
        assert!(
            buttons.iter().any(|label| label.starts_with(option)),
            "{option}: {buttons:?}"
        );
    }
    assert!(has(&welcome, Role::Button, "Command palette"));

    keys("secondary-1", cx);
    let code = redraw(cx);
    assert_all_named(&code);
    assert!(has(
        &code,
        Role::Heading,
        "Sign in with a code from your parent"
    ));
    assert!(has(&code, Role::Group, "QR code image"));
    assert!(has(&code, Role::Form, "Sign-in link"));
    for button in ["Back", "Paste from clipboard", "Continue"] {
        assert!(has(&code, Role::Button, button), "{button}");
    }
    assert!(
        audit::labels(&code, Role::PasswordInput).contains(&"Sign-in link")
            || audit::labels(&code, Role::TextInput).contains(&"Sign-in link"),
        "the link field is named"
    );
}

#[gpui::test]
fn the_confirmation_is_a_named_dialog(cx: &mut TestAppContext) {
    let (view, cx, _) = open_signin(cx, InitialView::PastedLink(FIXTURE_LINK.to_owned()));
    wait_for_step(&view, cx, "the confirmation", |step| {
        matches!(step, SigninStep::Confirm(_))
    });
    let entries = redraw(cx);
    assert_all_named(&entries);
    assert!(has(
        &entries,
        Role::Dialog,
        "Sign in as @ana:smith.safechat.family?"
    ));
    for button in ["Use my password instead", "Cancel", "Continue"] {
        assert!(has(&entries, Role::Button, button), "{button}");
    }
}

#[gpui::test]
fn the_password_forms_name_every_field(cx: &mut TestAppContext) {
    for (initial, title, server) in [
        (
            InitialView::Password,
            "Sign in with your password",
            "Your family's address",
        ),
        (
            InitialView::OtherServer,
            "Use another Matrix server",
            "Homeserver",
        ),
    ] {
        let (view, cx, startup) = open_signin(cx, InitialView::Default);
        startup
            .signin()
            .start_at(steps::rate_limited(Duration::from_secs(9)));
        let signin = super::signin_view(&view, cx);
        cx.update(|window, cx| {
            signin.update(cx, |signin, cx| match initial {
                InitialView::OtherServer => signin.use_another_server(window, cx),
                _ => signin.sign_in_with_password(window, cx),
            });
        });
        wait_for_step(&view, cx, "the form", |step| {
            matches!(step, SigninStep::Password(_))
        });
        let entries = redraw(cx);
        assert_all_named(&entries);
        assert!(
            has(&entries, Role::Form, title)
                || has(&entries, Role::Form, "Sign in with your password")
        );
        let fields: Vec<&str> = audit::labels(&entries, Role::TextInput)
            .into_iter()
            .chain(audit::labels(&entries, Role::PasswordInput))
            .collect();
        for field in [server, "Username or Matrix ID", "Password"] {
            let _ = title;
            if initial == InitialView::OtherServer && field == server {
                // The fixture's rate-limit form is a Family Chat one.
                continue;
            }
            assert!(fields.contains(&field), "{field}: {fields:?}");
        }
        // The rate limit's banner is an alert.
        assert!(
            audit::labels(&entries, Role::Alert)
                .iter()
                .any(|label| label.contains("Too many sign-in attempts")),
            "{:?}",
            audit::labels(&entries, Role::Alert)
        );
    }
}

#[gpui::test]
fn the_working_and_final_steps_say_what_is_happening(cx: &mut TestAppContext) {
    let (view, cx, startup) = open_signin(cx, InitialView::Default);
    startup.signin().start_at(SigninStep::Working(
        familychat_core::signin::Working::CheckingServer,
    ));
    let signin = super::signin_view(&view, cx);
    cx.update(|window, cx| {
        signin.update(cx, |signin, cx| signin.sign_in_with_password(window, cx));
    });
    wait_for_step(&view, cx, "working", |step| {
        matches!(step, SigninStep::Working(_))
    });
    let working = redraw(cx);
    assert_all_named(&working);
    assert!(has(&working, Role::Status, "Finding your server…"));

    startup.signin().start_at(steps::already_signed_in());
    cx.update(|window, cx| {
        signin.update(cx, |signin, cx| signin.sign_in_with_password(window, cx));
    });
    wait_for_step(&view, cx, "already signed in", |step| {
        matches!(step, SigninStep::AlreadySignedIn(_))
    });
    let already = redraw(cx);
    assert_all_named(&already);
    assert!(has(&already, Role::Heading, "Already signed in"));
    assert!(has(&already, Role::Button, "Back to sign-in"));
}

#[gpui::test]
fn the_profile_screens_name_their_buttons(cx: &mut TestAppContext) {
    for (profile, buttons) in [
        (FixtureProfile::KeychainLocked, ["Quit", "Try again"]),
        (FixtureProfile::LostKey, ["Quit", "Start over"]),
    ] {
        let startup = FixtureStartup::new(&[profile]);
        let (_view, cx) = open(cx, startup, InitialView::Default);
        let entries = redraw(cx);
        assert_all_named(&entries);
        assert_eq!(
            audit::labels(&entries, Role::Heading).len(),
            2,
            "{entries:?}"
        );
        for button in buttons {
            assert!(has(&entries, Role::Button, button), "{profile:?}: {button}");
        }
    }
}

#[gpui::test]
fn the_signed_in_view_reads_its_sync_state(cx: &mut TestAppContext) {
    let session = FixtureSession::new();
    session.add_account(
        &fixture_user(),
        SyncState::Suspended {
            recheck_in: Duration::from_secs(60),
        },
    );
    let signin = FixtureSignin::new(session.clone());
    let startup = FixtureStartup::with(session, signin, &[FixtureProfile::Ready]);
    let (_view, cx) = open(cx, startup, InitialView::Default);
    let entries = redraw(cx);
    assert_all_named(&entries);
    assert!(has(&entries, Role::Status, "Sync: Paused"));
    assert!(
        audit::labels(&entries, Role::Alert)
            .iter()
            .chain(audit::labels(&entries, Role::Status).iter())
            .any(|label| label.contains("Paused")),
        "the banner says the account is paused: {entries:?}"
    );
    assert!(has(&entries, Role::Heading, "Rooms"));
}

#[gpui::test]
fn the_command_palette_names_its_results(cx: &mut TestAppContext) {
    let (_view, cx, _) = open_signin(cx, InitialView::Default);
    keys("secondary-k", cx);
    let entries = redraw(cx);
    assert_all_named(&entries);
    assert!(has(&entries, Role::ListBox, "Commands"));
    assert!(
        audit::labels(&entries, Role::ListBoxOption)
            .iter()
            .any(|label| label.starts_with("Sign in with your password, ")),
    );
    assert!(
        audit::labels(&entries, Role::Status)
            .iter()
            .any(|label| label.contains(", 1 of ")),
    );
}

/// Every rooms screen the screenshot matrix shows.
fn rooms_launches() -> Vec<(&'static str, crate::RoomsLaunch)> {
    use familychat_core::rooms::RoomListFilter;
    use familychat_test_support::fixtures::rooms::{
        EMPTY_ROOM, FAMILY_ROOM, FOOTBALL_ROOM, HOLIDAY_ROOM, HOMEWORK_ROOM, INVITE_ROOM, MUM_ROOM,
        PHOTOS_ROOM, SECRETS_ROOM,
    };
    let room = |id: &str| crate::RoomsLaunch {
        room: Some(id.to_owned()),
        ..crate::RoomsLaunch::default()
    };
    vec![
        ("room list", crate::RoomsLaunch::default()),
        (
            "invites",
            crate::RoomsLaunch {
                filter: RoomListFilter::Invites,
                ..crate::RoomsLaunch::default()
            },
        ),
        ("family room", room(FAMILY_ROOM)),
        ("direct chat", room(MUM_ROOM)),
        ("rich text", room(HOLIDAY_ROOM)),
        ("media", room(PHOTOS_ROOM)),
        (
            "room changes",
            crate::RoomsLaunch {
                expand_changes: true,
                ..room(FOOTBALL_ROOM)
            },
        ),
        ("unreadable", room(SECRETS_ROOM)),
        ("not encrypted", room(HOMEWORK_ROOM)),
        ("empty room", room(EMPTY_ROOM)),
        ("invitation", room(INVITE_ROOM)),
        (
            "members",
            crate::RoomsLaunch {
                members: true,
                ..room(FAMILY_ROOM)
            },
        ),
        (
            "link dialog",
            crate::RoomsLaunch {
                link: Some((
                    "https://tickets.evil.example/claim".to_owned(),
                    "https://www.bank.example".to_owned(),
                )),
                ..room(HOLIDAY_ROOM)
            },
        ),
    ]
}

/// Opens the fixture family's rooms as `launch` says and waits for what it
/// opens to load.
pub(super) fn open_rooms_screen(
    cx: &mut TestAppContext,
    launch: crate::RoomsLaunch,
) -> &mut VisualTestContext {
    let opens_room = launch.room.is_some();
    let members = launch.members;
    let (view, cx, _session) = super::rooms::open_rooms(cx, launch);
    let rooms = super::rooms::rooms_view(&view, cx);
    if opens_room {
        super::wait_until(cx, "the room", |cx| {
            rooms.read_with(cx, |rooms, cx| {
                rooms.selected().is_some()
                    && rooms
                        .timeline()
                        .is_none_or(|timeline| timeline.read(cx).is_loaded())
            })
        });
    }
    if members {
        super::wait_until(cx, "the members", |cx| {
            rooms.read_with(cx, |rooms, _| rooms.members_open())
        });
        // The members arrive on the next frame.
        cx.run_until_parked();
    }
    cx
}

#[gpui::test]
fn every_rooms_screen_names_every_control(cx: &mut TestAppContext) {
    for (screen, launch) in rooms_launches() {
        let cx = open_rooms_screen(cx, launch);
        let entries = redraw(cx);
        assert_all_named(&entries);
        assert!(has(&entries, Role::Navigation, "Rooms"), "{screen}");
        assert!(has(&entries, Role::Heading, "Rooms"), "{screen}");
        assert!(
            audit::labels(&entries, Role::RadioButton).len() == 4,
            "{screen}: the four filters"
        );
        match screen {
            "room list" => {
                assert!(
                    audit::labels(&entries, Role::ListBoxOption)
                        .iter()
                        .any(|label| label.starts_with("Smith family, 3 unread, 1 mention")),
                    "{entries:?}"
                );
                assert!(has(&entries, Role::Heading, "Pick a room"));
            }
            "family room" => {
                assert!(has(&entries, Role::Heading, "Smith family"));
                assert!(
                    audit::labels(&entries, Role::Label)
                        .iter()
                        .any(|label| label.starts_with("Encrypted.")),
                );
                assert!(has(&entries, Role::Button, "Show the members"));
                assert!(
                    audit::labels(&entries, Role::ListItem)
                        .iter()
                        .any(|label| label.starts_with("Mum, ")),
                );
                assert!(has(&entries, Role::Label, "New messages"));
            }
            "media" => {
                assert!(
                    audit::labels(&entries, Role::Button)
                        .iter()
                        .any(|label| label.starts_with("Save ")),
                    "{entries:?}"
                );
            }
            "not encrypted" => {
                assert!(
                    audit::labels(&entries, Role::Status)
                        .iter()
                        .any(|label| label.contains("not end-to-end encrypted")),
                    "{entries:?}"
                );
            }
            "invitation" => {
                assert!(has(&entries, Role::Heading, "Join Cousins?"));
                for button in ["Decline", "Join the room"] {
                    assert!(has(&entries, Role::Button, button), "{button}");
                }
            }
            "members" => {
                assert!(has(&entries, Role::Complementary, "Members"));
                assert!(
                    audit::labels(&entries, Role::ListItem).contains(&"Mum, Admin"),
                    "{entries:?}"
                );
            }
            "link dialog" => {
                assert!(
                    has(&entries, Role::Dialog, "Open this link?"),
                    "{entries:?}"
                );
                assert!(
                    audit::labels(&entries, Role::Alert)
                        .iter()
                        .any(|label| label.starts_with("This link is not what it shows")),
                );
                for button in ["Copy Link", "Cancel", "Open Link"] {
                    assert!(has(&entries, Role::Button, button), "{button}");
                }
            }
            _ => {}
        }
    }
}
