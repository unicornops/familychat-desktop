//! Pointer target size (WCAG 2.2 2.5.8, Zmail's baseline): every target a
//! frame draws is at least 24×24 at the default text size.
//!
//! Targets mark themselves with `HitTargetExt::hit_target`, which records
//! them in tests, so these tests walk whatever each screen drew instead of
//! a hand-kept list.

use std::time::Duration;

use familychat_core::{signin::SigninStep, sync::SyncState};
use familychat_test_support::fixtures::{
    FIXTURE_LINK, FixtureProfile, FixtureSession, FixtureSignin, FixtureStartup, fixture_user,
    steps,
};
use gpui::{TestAppContext, VisualTestContext, px};

use super::{keys, open, open_signin, signin_view, wait_for_step};
use crate::InitialView;
use crate::components::MIN_TARGET_SIZE;
use crate::components::hit_target::recorded;

/// Redraws the window and checks every target it drew. Returns how many
/// were checked.
fn check_targets(what: &str, cx: &mut VisualTestContext) -> usize {
    recorded::take();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let mut checked = 0;
    let mut too_small = Vec::new();
    for selector in recorded::take() {
        let selector: &'static str = Box::leak(selector.into_boxed_str());
        // Recorded but not laid out (a list row outside the viewport).
        let Some(bounds) = cx.debug_bounds(selector) else {
            continue;
        };
        checked += 1;
        if bounds.size.width < px(MIN_TARGET_SIZE) || bounds.size.height < px(MIN_TARGET_SIZE) {
            too_small.push(format!(
                "{selector}: {}×{}",
                f32::from(bounds.size.width),
                f32::from(bounds.size.height)
            ));
        }
    }
    assert!(
        too_small.is_empty(),
        "{what}: targets under {MIN_TARGET_SIZE}px:\n{}",
        too_small.join("\n")
    );
    checked
}

#[gpui::test]
fn the_sign_in_targets_are_at_least_24px(cx: &mut TestAppContext) {
    let (view, cx, startup) = open_signin(cx, InitialView::Default);
    assert!(check_targets("welcome", cx) >= 4);
    keys("secondary-1", cx);
    assert!(check_targets("code", cx) >= 4);

    let signin = signin_view(&view, cx);
    cx.update(|window, cx| {
        signin.update(cx, |signin, cx| {
            signin.start(
                familychat_core::signin::SigninStart::Link(FIXTURE_LINK.into()),
                window,
                cx,
            );
        });
    });
    wait_for_step(&view, cx, "the confirmation", |step| {
        matches!(step, SigninStep::Confirm(_))
    });
    assert!(check_targets("confirmation", cx) >= 3);
    keys("escape", cx);

    startup
        .signin()
        .start_at(steps::rate_limited(Duration::from_secs(9)));
    cx.update(|window, cx| {
        signin.update(cx, |signin, cx| signin.sign_in_with_password(window, cx));
    });
    wait_for_step(&view, cx, "the form", |step| {
        matches!(step, SigninStep::Password(_))
    });
    assert!(check_targets("password", cx) >= 5);

    startup.signin().start_at(steps::already_signed_in());
    cx.update(|window, cx| {
        signin.update(cx, |signin, cx| signin.use_another_server(window, cx));
    });
    wait_for_step(&view, cx, "already", |step| {
        matches!(step, SigninStep::AlreadySignedIn(_))
    });
    assert!(check_targets("already signed in", cx) >= 2);
}

#[gpui::test]
fn the_profile_and_signed_in_targets_are_at_least_24px(cx: &mut TestAppContext) {
    for profile in [FixtureProfile::KeychainLocked, FixtureProfile::LostKey] {
        let startup = FixtureStartup::new(&[profile]);
        let (_view, cx) = open(cx, startup, InitialView::Default);
        assert!(check_targets(&format!("{profile:?}"), cx) >= 3);
    }
    let session = FixtureSession::new();
    session.add_account(
        &fixture_user(),
        SyncState::Offline {
            retry_in: Duration::from_secs(4),
        },
    );
    let signin = FixtureSignin::new(session.clone());
    let startup = FixtureStartup::with(session, signin, &[FixtureProfile::Ready]);
    let (_view, cx) = open(cx, startup, InitialView::Default);
    assert!(check_targets("signed in", cx) >= 2);
}

#[gpui::test]
fn the_command_palette_targets_are_at_least_24px(cx: &mut TestAppContext) {
    let (_view, cx, _) = open_signin(cx, InitialView::Default);
    keys("secondary-k", cx);
    assert!(check_targets("command palette", cx) >= 5);
}

#[gpui::test]
fn the_rooms_targets_are_at_least_24px(cx: &mut TestAppContext) {
    use familychat_test_support::fixtures::rooms::{
        FAMILY_ROOM, HOLIDAY_ROOM, INVITE_ROOM, PHOTOS_ROOM,
    };
    let room = |id: &str, members: bool| crate::RoomsLaunch {
        room: Some(id.to_owned()),
        members,
        ..crate::RoomsLaunch::default()
    };
    for (screen, launch, at_least) in [
        ("room list", crate::RoomsLaunch::default(), 8),
        ("family room", room(FAMILY_ROOM, true), 10),
        ("media", room(PHOTOS_ROOM, false), 10),
        ("rich text", room(HOLIDAY_ROOM, false), 8),
        ("invitation", room(INVITE_ROOM, false), 8),
        (
            "composer reply",
            crate::RoomsLaunch {
                composer: Some(crate::ComposerLaunch::Reply),
                ..room(FAMILY_ROOM, false)
            },
            12,
        ),
        (
            "composer mentions",
            crate::RoomsLaunch {
                composer: Some(crate::ComposerLaunch::Mention("thanks @".to_owned())),
                ..room(FAMILY_ROOM, false)
            },
            12,
        ),
        (
            "composer attachment",
            crate::RoomsLaunch {
                composer: Some(crate::ComposerLaunch::Attachment),
                ..room(FAMILY_ROOM, false)
            },
            12,
        ),
        (
            "message actions",
            crate::RoomsLaunch {
                message_actions: true,
                ..room(FAMILY_ROOM, false)
            },
            16,
        ),
        (
            "delete dialog",
            crate::RoomsLaunch {
                delete_prompt: true,
                ..room(FAMILY_ROOM, false)
            },
            4,
        ),
    ] {
        let cx = super::a11y::open_rooms_screen(cx, launch);
        assert!(check_targets(screen, cx) >= at_least, "{screen}");
    }
}

#[gpui::test]
fn the_rooms_targets_hold_on_a_narrow_window(cx: &mut TestAppContext) {
    use familychat_test_support::fixtures::rooms::FAMILY_ROOM;
    let cx = super::a11y::open_rooms_screen(
        cx,
        crate::RoomsLaunch {
            room: Some(FAMILY_ROOM.to_owned()),
            ..crate::RoomsLaunch::default()
        },
    );
    cx.simulate_resize(gpui::size(px(560.), px(720.)));
    cx.run_until_parked();
    assert!(check_targets("narrow room", cx) >= 3);
    assert!(cx.debug_bounds("button-room-back").is_some(), "a way back");
}

#[gpui::test]
fn the_notification_settings_and_a_forwarded_link_targets_are_at_least_24px(
    cx: &mut TestAppContext,
) {
    for (screen, initial, at_least) in [
        (
            "notification settings",
            InitialView::NotificationSettings,
            3,
        ),
        (
            "forwarded link",
            InitialView::ForwardedLink(FIXTURE_LINK.to_owned()),
            3,
        ),
    ] {
        let session = FixtureSession::new();
        session.add_account(&fixture_user(), SyncState::Running);
        session.with_family();
        let signin = FixtureSignin::new(session.clone());
        let startup = FixtureStartup::with(session, signin, &[FixtureProfile::Ready]);
        let (_view, cx) = super::open(cx, startup, initial);
        super::wait_until(cx, "the dialog", |cx| {
            cx.debug_bounds("button-notification-settings-done")
                .is_some()
                || cx.debug_bounds("button-signin-confirm-continue").is_some()
        });
        assert!(check_targets(screen, cx) >= at_least, "{screen}");
    }
}
