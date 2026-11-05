//! Links that reach the running window from outside (ADR 0013): a second
//! launch's link (through the app's single-instance listener, here its
//! request channel) or one macOS hands over. Each starts a sign-in that
//! shows the confirmation first; nothing is redeemed without Continue,
//! over the sign-in screen or over the rooms.

use std::time::Duration;

use familychat_core::{
    requests::{AppRequest, RequestSender},
    signin::{SecretText, SigninStep, Working},
    sync::SyncState,
};
use familychat_test_support::fixtures::{
    FIXTURE_LINK, FixtureAnswer, FixtureEvent, FixtureProfile, FixtureSession, FixtureSignin,
    FixtureStart, FixtureStartup, fixture_user,
};
use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};

use super::{click, keys, open_with_requests, step, wait_for_step, wait_until};
use crate::shell::{FINISH_SIGNING_IN, Screen};
use crate::{InitialView, ShellView};

fn forward(requests: &RequestSender, link: &str, cx: &mut VisualTestContext) {
    requests
        .try_send(AppRequest::OpenLink(SecretText::from(link)))
        .expect("the window takes requests");
    cx.run_until_parked();
}

fn is_confirm(step: &SigninStep) -> bool {
    matches!(step, SigninStep::Confirm(_))
}

/// The window signed in to the fixture account, with its request sender.
fn signed_in(
    cx: &mut TestAppContext,
) -> (
    Entity<ShellView>,
    &mut VisualTestContext,
    std::sync::Arc<FixtureStartup>,
    RequestSender,
) {
    let session = FixtureSession::new();
    session.add_account(&fixture_user(), SyncState::Running);
    session.with_family();
    let signin = FixtureSignin::new(session.clone());
    let startup = FixtureStartup::with(session, signin, &[FixtureProfile::Ready]);
    let (view, cx, requests) = open_with_requests(cx, startup.clone(), InitialView::Default);
    assert!(cx.read_entity(&view, |view, _| view.signed_in().is_some()));
    (view, cx, startup, requests)
}

fn settle(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    std::thread::sleep(Duration::from_millis(50));
    cx.run_until_parked();
}

#[gpui::test]
fn a_forwarded_link_shows_the_confirmation_and_redeems_only_after_continue(
    cx: &mut TestAppContext,
) {
    let startup = FixtureStartup::new(&[FixtureProfile::Ready]);
    let (view, cx, requests) = open_with_requests(cx, startup.clone(), InitialView::Default);
    let signin = startup.signin().clone();
    forward(&requests, FIXTURE_LINK, cx);
    wait_for_step(&view, cx, "the confirmation", is_confirm);
    assert_eq!(
        signin.events().first(),
        Some(&FixtureEvent::Started(FixtureStart::Link {
            with_code: true
        }))
    );
    // The keys that act elsewhere, and time, redeem nothing.
    for keystrokes in ["enter", "space"] {
        keys(keystrokes, cx);
    }
    settle(cx);
    assert!(is_confirm(&step(&view, cx)));
    assert_eq!(signin.redeemed(), 0);

    click("button-signin-confirm-continue", cx);
    wait_until(cx, "the redemption", |_| signin.redeemed() == 1);
    assert_eq!(signin.answers(), [FixtureAnswer::Continue]);
}

#[gpui::test]
fn a_link_forwarded_while_signed_in_asks_over_the_rooms_and_cancel_goes_back(
    cx: &mut TestAppContext,
) {
    let (view, cx, startup, requests) = signed_in(cx);
    let signin = startup.signin().clone();
    forward(&requests, FIXTURE_LINK, cx);
    wait_for_step(&view, cx, "the confirmation", is_confirm);
    assert!(cx.read_entity(&view, |view, _| view.over_rooms()));
    settle(cx);
    assert_eq!(signin.redeemed(), 0, "nothing without the confirmation");

    click("button-signin-confirm-cancel", cx);
    wait_until(cx, "the rooms again", |cx| {
        cx.read_entity(&view, |view, _| view.signed_in().is_some())
    });
    assert!(!cx.read_entity(&view, |view, _| view.over_rooms()));
    assert_eq!(signin.redeemed(), 0);
    assert_eq!(signin.answers(), [FixtureAnswer::Cancel]);
}

#[gpui::test]
fn a_link_that_arrives_before_the_profile_opens_waits_for_it(cx: &mut TestAppContext) {
    let startup = FixtureStartup::new(&[FixtureProfile::KeychainLocked, FixtureProfile::Ready]);
    let (view, cx, requests) = open_with_requests(cx, startup.clone(), InitialView::Default);
    assert!(cx.read_entity(&view, |view, _| matches!(view.screen(), Screen::Profile(_))));
    forward(&requests, FIXTURE_LINK, cx);
    assert!(
        startup.signin().events().is_empty(),
        "nothing starts before the profile is open"
    );
    click("button-profile-retry", cx);
    wait_for_step(&view, cx, "the confirmation", is_confirm);
    assert_eq!(startup.signin().redeemed(), 0);
}

#[gpui::test]
fn a_link_never_interrupts_a_sign_in_that_is_using_a_code(cx: &mut TestAppContext) {
    let startup = FixtureStartup::new(&[FixtureProfile::Ready]);
    let (view, cx, requests) = open_with_requests(cx, startup.clone(), InitialView::Default);
    startup
        .signin()
        .start_at(SigninStep::Working(Working::RedeemingCode));
    forward(&requests, FIXTURE_LINK, cx);
    wait_for_step(&view, cx, "the code in use", |step| {
        matches!(step, SigninStep::Working(_))
    });
    forward(&requests, FIXTURE_LINK, cx);
    settle(cx);
    assert_eq!(
        startup
            .signin()
            .events()
            .iter()
            .filter(|event| matches!(event, FixtureEvent::Started(_)))
            .count(),
        1,
        "the second link did not start another sign-in"
    );
    let toasts = cx.read_entity(&view, |view, _| view.toasts().clone());
    let messages: Vec<String> = cx.read_entity(&toasts, |toasts, _| {
        toasts
            .visible()
            .into_iter()
            .map(|(_, message)| message.to_string())
            .collect()
    });
    assert!(
        messages.contains(&FINISH_SIGNING_IN.to_owned()),
        "{messages:?}"
    );
}

#[gpui::test]
fn a_bare_second_launch_only_brings_the_window_forward(cx: &mut TestAppContext) {
    let (view, cx, startup, requests) = signed_in(cx);
    requests
        .try_send(AppRequest::Activate)
        .expect("the window takes requests");
    settle(cx);
    assert!(startup.signin().events().is_empty());
    assert!(cx.read_entity(&view, |view, _| view.signed_in().is_some()));
}

#[gpui::test]
fn the_fixture_view_of_a_forwarded_link_is_the_confirmation_over_the_rooms(
    cx: &mut TestAppContext,
) {
    let session = FixtureSession::new();
    session.add_account(&fixture_user(), SyncState::Running);
    session.with_family();
    let signin = FixtureSignin::new(session.clone());
    let startup = FixtureStartup::with(session, signin, &[FixtureProfile::Ready]);
    let (view, cx, _requests) = open_with_requests(
        cx,
        startup.clone(),
        InitialView::ForwardedLink(FIXTURE_LINK.to_owned()),
    );
    wait_for_step(&view, cx, "the confirmation", is_confirm);
    assert!(cx.read_entity(&view, |view, _| view.over_rooms()));
    assert_eq!(startup.signin().redeemed(), 0);
}
