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
