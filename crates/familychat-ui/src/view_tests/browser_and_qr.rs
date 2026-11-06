//! "Use another Matrix server"'s server step, the server's ways to sign in,
//! the browser (opened only after Continue in the dialog that shows the
//! whole address), QR codes and check codes, and signing a new device in
//! over the rooms (ADR 0010), over the fixture sign-in; with each screen's
//! names and 24 px targets.

use familychat_core::signin::{BrowserPurpose, QrPurpose, SigninStep};
use std::sync::Arc;

use familychat_core::sync::SyncState;
use familychat_test_support::fixtures::{
    FIXTURE_BROWSER_URL, FIXTURE_CHECK_CODE, FIXTURE_ISSUER, FIXTURE_OTHER_SERVER, FixtureAnswer,
    FixtureProfile, FixtureSession, FixtureSignin, FixtureStartup, fixture_user, steps,
};
use gpui::{AppContext as _, Role, TestAppContext};

use super::a11y::redraw;
use super::hit_targets::check_targets;
use super::{click, open, open_signin, signin_view, wait_for_step, wait_until};
use crate::InitialView;
use crate::a11y::audit;

fn names(entries: &[(Role, String)], role: Role) -> Vec<String> {
    audit::labels(entries, role)
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// A signed-in profile whose sign-in starts at `step`, if given.
fn signed_in(step: Option<SigninStep>) -> (Arc<FixtureStartup>, Arc<FixtureSignin>) {
    let session = FixtureSession::new();
    session.add_account(&fixture_user(), SyncState::Running);
    session.with_family();
    let signin = FixtureSignin::new(session.clone());
    if let Some(step) = step {
        signin.start_at(step);
    }
    let startup = FixtureStartup::with(session, signin.clone(), &[FixtureProfile::Ready]);
    (startup, signin)
}

fn assert_named(entries: &[(Role, String)]) {
    for (role, label) in entries {
        assert!(!label.trim().is_empty(), "a {role:?} has no name");
    }
}

#[gpui::test]
fn another_server_shows_only_its_ways_and_the_browser_opens_after_continue(
    cx: &mut TestAppContext,
) {
    let (view, cx, startup) = open_signin(cx, InitialView::OtherServer);
    let signin = startup.signin().clone();
    wait_for_step(&view, cx, "the server step", |step| {
        matches!(step, SigninStep::Server(_))
    });
    let server_step = redraw(cx);
    assert_named(&server_step);
    assert!(names(&server_step, Role::TextInput).contains(&"Homeserver".to_owned()));
    assert!(check_targets("the server step", cx) >= 2);

    let signin_entity = signin_view(&view, cx);
    let server = cx.read_entity(&signin_entity, |signin, _| signin.server_input().clone());
    cx.update(|window, cx| server.update(cx, |input, cx| input.focus(window, cx)));
    cx.simulate_input(FIXTURE_OTHER_SERVER);
    click("button-signin-server-continue", cx);
    wait_for_step(
        &view,
        cx,
        "the server's ways",
        |step| matches!(step, SigninStep::Password(form) if form.methods.is_some()),
    );
    assert_eq!(
        signin.answers(),
        [FixtureAnswer::ChooseServer(FIXTURE_OTHER_SERVER.to_owned())]
    );
    let methods = redraw(cx);
    assert_named(&methods);
    for button in [
        "Continue in your browser",
        "Sign in with a QR code",
        "Change server",
        "Sign in",
    ] {
        assert!(
            names(&methods, Role::Button)
                .iter()
                .any(|name| name == button),
            "{button}: {:?}",
            names(&methods, Role::Button)
        );
    }
    assert!(check_targets("the server's ways", cx) >= 6);

    click("button-signin-use-browser", cx);
    wait_for_step(&view, cx, "the browser question", |step| {
        matches!(step, SigninStep::ConfirmBrowser(_))
    });
    let dialog = redraw(cx);
    assert_named(&dialog);
    assert!(dialog.contains(&(Role::Dialog, "Continue in your browser?".to_owned())));
    assert!(
        matches!(super::step(&view, cx), SigninStep::ConfirmBrowser(confirm) if confirm.address == FIXTURE_ISSUER),
        "the dialog shows the whole address"
    );
    assert!(check_targets("the browser question", cx) >= 2);
    assert_eq!(cx.opened_url(), None, "nothing opens before the answer");
    click("button-signin-confirm-browser-cancel", cx);
    wait_for_step(&view, cx, "the ways again", |step| {
        matches!(step, SigninStep::Password(_))
    });
    assert_eq!(cx.opened_url(), None, "Cancel opens nothing");

    click("button-signin-use-browser", cx);
    wait_for_step(&view, cx, "the browser question", |step| {
        matches!(step, SigninStep::ConfirmBrowser(_))
    });
    click("button-signin-confirm-browser-continue", cx);
    wait_for_step(&view, cx, "the browser", |step| {
        matches!(step, SigninStep::Browser(_))
    });
    assert_eq!(
        cx.opened_url().as_deref(),
        Some(FIXTURE_BROWSER_URL),
        "opened once Continue was clicked"
    );
    let waiting = redraw(cx);
    assert_named(&waiting);
    assert!(
        waiting.iter().all(|(_, label)| !label.contains("state=")),
        "the address's query is never shown: {waiting:?}"
    );
    assert!(check_targets("the browser", cx) >= 2);
    click("button-signin-browser-cancel", cx);
    wait_for_step(&view, cx, "the ways again", |step| {
        matches!(step, SigninStep::Password(_))
    });
    assert_eq!(
        signin
            .answers()
            .iter()
            .filter(|answer| **answer == FixtureAnswer::Continue)
            .count(),
        1
    );

    click("button-signin-change-server", cx);
    wait_for_step(&view, cx, "the server step again", |step| {
        matches!(step, SigninStep::Server(_))
    });
}

#[gpui::test]
fn a_browser_step_the_view_did_not_confirm_opens_nothing(cx: &mut TestAppContext) {
    let (view, cx, startup) = open_signin(cx, InitialView::Default);
    startup.signin().start_at(steps::browser());
    let signin = signin_view(&view, cx);
    cx.update(|window, cx| {
        signin.update(cx, |signin, cx| signin.use_another_server(window, cx));
    });
    wait_for_step(&view, cx, "the browser", |step| {
        matches!(step, SigninStep::Browser(_))
    });
    assert_eq!(cx.opened_url(), None);
    click("button-signin-browser-again", cx);
    assert_eq!(
        cx.opened_url().as_deref(),
        Some(FIXTURE_BROWSER_URL),
        "only when asked"
    );
}

#[gpui::test]
fn a_qr_code_sign_in_takes_the_check_code(cx: &mut TestAppContext) {
    let (view, cx, startup) = open_signin(cx, InitialView::Default);
    let signin = startup.signin().clone();
    signin.start_at(steps::methods());
    let entity = signin_view(&view, cx);
    cx.update(|window, cx| {
        entity.update(cx, |signin, cx| signin.use_another_server(window, cx));
    });
    wait_for_step(&view, cx, "the ways", |step| {
        matches!(step, SigninStep::Password(_))
    });
    click("button-signin-use-qr", cx);
    wait_for_step(
        &view,
        cx,
        "the QR code",
        |step| matches!(step, SigninStep::QrCode(qr) if qr.purpose == QrPurpose::SignIn),
    );
    let qr = redraw(cx);
    assert_named(&qr);
    assert!(
        names(&qr, Role::Image)
            .iter()
            .any(|name| name.contains("QR code"))
    );
    assert!(check_targets("the QR code", cx) >= 1);

    assert!(signin.scan());
    wait_for_step(&view, cx, "the check code", |step| {
        matches!(step, SigninStep::CheckCode(_))
    });
    let form = redraw(cx);
    assert_named(&form);
    assert!(names(&form, Role::TextInput).contains(&"Check code".to_owned()));
    assert!(check_targets("the check code", cx) >= 3);
    let input = cx.read_entity(&entity, |signin, _| signin.check_code_input().clone());
    cx.update(|window, cx| input.update(cx, |input, cx| input.focus(window, cx)));
    cx.simulate_input("17");
    click("button-signin-check-code-continue", cx);
    wait_for_step(
        &view,
        cx,
        "the problem",
        |step| matches!(step, SigninStep::CheckCode(form) if form.problem.is_some()),
    );
    cx.update(|window, cx| input.update(cx, |input, cx| input.focus(window, cx)));
    cx.simulate_input(FIXTURE_CHECK_CODE);
    click("button-signin-check-code-continue", cx);
    wait_until(cx, "the signed-in view", |cx| {
        cx.read_entity(&view, |view, _| view.signed_in().is_some())
    });
    assert!(signin.answers().contains(&FixtureAnswer::SubmitCheckCode(
        FIXTURE_CHECK_CODE.to_owned()
    )));
}

#[gpui::test]
fn signing_a_new_device_in_shows_over_the_rooms_and_goes_back_to_them(cx: &mut TestAppContext) {
    let (startup, signin) = signed_in(None);
    let (view, cx) = open(cx, startup, InitialView::LinkDevice);
    wait_for_step(
        &view,
        cx,
        "the new device's QR code",
        |step| matches!(step, SigninStep::QrCode(qr) if qr.purpose == QrPurpose::LinkDevice),
    );
    assert!(cx.read_entity(&view, |view, _| view.over_rooms()));
    let qr = redraw(cx);
    assert_named(&qr);
    assert!(signin.scan());
    wait_for_step(&view, cx, "the check code", |step| {
        matches!(step, SigninStep::CheckCode(_))
    });
    let entity = signin_view(&view, cx);
    let input = cx.read_entity(&entity, |signin, _| signin.check_code_input().clone());
    cx.update(|window, cx| input.update(cx, |input, cx| input.focus(window, cx)));
    cx.simulate_input(FIXTURE_CHECK_CODE);
    click("button-signin-check-code-continue", cx);
    wait_for_step(
        &view,
        cx,
        "the approval question",
        |step| matches!(step, SigninStep::ConfirmBrowser(confirm) if confirm.purpose == BrowserPurpose::ApproveDevice),
    );
    let dialog = redraw(cx);
    assert_named(&dialog);
    assert_eq!(cx.opened_url(), None);
    click("button-signin-confirm-browser-continue", cx);
    wait_for_step(
        &view,
        cx,
        "the approval in the browser",
        |step| matches!(step, SigninStep::Browser(browser) if browser.purpose == BrowserPurpose::ApproveDevice),
    );
    assert!(cx.opened_url().is_some());
    click("button-signin-browser-cancel", cx);
    wait_until(cx, "the rooms again", |cx| {
        cx.read_entity(&view, |view, _| view.signed_in().is_some())
    });
}

#[gpui::test]
fn the_end_of_signing_a_new_device_in_is_named(cx: &mut TestAppContext) {
    let (startup, _signin) = signed_in(Some(steps::device_linked()));
    let (view, cx) = open(cx, startup, InitialView::LinkDevice);
    wait_for_step(&view, cx, "done", |step| {
        matches!(step, SigninStep::DeviceLinked(_))
    });
    let done = redraw(cx);
    assert_named(&done);
    assert!(names(&done, Role::Heading).contains(&"New device signed in".to_owned()));
    assert!(check_targets("done", cx) >= 1);
    click("button-signin-link-done", cx);
    wait_until(cx, "the rooms again", |cx| {
        cx.read_entity(&view, |view, _| view.signed_in().is_some())
    });
}
