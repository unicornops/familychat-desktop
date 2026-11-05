//! The sign-in screens over the fixture sign-in: the confirmation always
//! comes before a code is redeemed, a pasted or dropped QR image becomes a
//! link, and the forms answer only what the step asks.

use std::time::Duration;

use familychat_core::signin::{SigninProblem, SigninStep};
use familychat_test_support::fixtures::{
    FIXTURE_LINK, FixtureAnswer, FixtureEvent, FixtureOutcome, FixtureProfile, FixtureStart,
    FixtureStartup, steps,
};
use gpui::{AppContext as _, ClipboardItem, Focusable, Image, ImageFormat, TestAppContext};

use super::{
    QR_SCREENSHOT, click, keys, open, open_signin, page, signin_view, step, wait_for_step,
    wait_until,
};
use crate::InitialView;
use crate::signin::{ImageSource, Page};

fn is_confirm(step: &SigninStep) -> bool {
    matches!(step, SigninStep::Confirm(_))
}

/// The contract's rule 3 (ADR 0006): whatever the person does on the
/// confirmation short of Continue, the code is not redeemed; Continue
/// redeems it once.
#[gpui::test]
fn the_confirmation_always_comes_before_a_code_is_redeemed(cx: &mut TestAppContext) {
    let (view, cx, startup) = open_signin(cx, InitialView::PastedLink(FIXTURE_LINK.to_owned()));
    let signin = startup.signin().clone();
    wait_for_step(&view, cx, "the confirmation", is_confirm);
    let SigninStep::Confirm(confirm) = step(&view, cx) else {
        unreachable!()
    };
    assert_eq!(confirm.question, "Sign in as @ana:smith.safechat.family?");

    // Keys that would act elsewhere do nothing here: Enter on the dialog,
    // the sign-in commands, a paste.
    for keystrokes in ["enter", "space", "secondary-2", "secondary-shift-v"] {
        keys(keystrokes, cx);
    }
    cx.run_until_parked();
    std::thread::sleep(Duration::from_millis(50));
    cx.run_until_parked();
    assert!(is_confirm(&step(&view, cx)), "{:?}", step(&view, cx));
    assert_eq!(signin.redeemed(), 0);
    assert!(signin.answers().is_empty(), "{:?}", signin.answers());

    click("button-signin-confirm-continue", cx);
    wait_until(cx, "the redemption", |_| signin.redeemed() == 1);
    assert_eq!(signin.answers(), [FixtureAnswer::Continue]);
    let events = signin.events();
    let confirmed = events
        .iter()
        .position(|event| matches!(event, FixtureEvent::Showed(SigninStep::Confirm(_))))
        .expect("the confirmation showed");
    let redeemed = events
        .iter()
        .position(|event| *event == FixtureEvent::Redeemed)
        .expect("the code was redeemed");
    assert!(confirmed < redeemed);
    wait_until(cx, "the signed-in view", |cx| {
        cx.read_entity(&view, |view, _| view.signed_in().is_some())
    });
}

#[gpui::test]
fn declining_or_cancelling_the_confirmation_redeems_nothing(cx: &mut TestAppContext) {
    let (view, cx, startup) = open_signin(cx, InitialView::PastedLink(FIXTURE_LINK.to_owned()));
    let signin = startup.signin().clone();
    wait_for_step(&view, cx, "the confirmation", is_confirm);
    click("button-signin-confirm-password", cx);
    wait_for_step(
        &view,
        cx,
        "the password form",
        |step| matches!(step, SigninStep::Password(form) if form.server == "smith.safechat.family"),
    );
    assert_eq!(signin.answers(), [FixtureAnswer::UsePassword]);

    // The view never continues from the password form.
    let view_signin = signin_view(&view, cx);
    cx.update(|window, cx| {
        view_signin.update(cx, |signin, cx| signin.confirm_code(window, cx));
    });
    cx.run_until_parked();
    assert_eq!(signin.answers(), [FixtureAnswer::UsePassword]);

    // A second link, cancelled from the dialog and then with Escape.
    keys("escape", cx);
    wait_until(cx, "the welcome page", |cx| {
        page(&view, cx) == Page::Welcome
    });
    cx.update(|window, cx| {
        view_signin.update(cx, |signin, cx| {
            signin.start(
                familychat_core::signin::SigninStart::Link(FIXTURE_LINK.into()),
                window,
                cx,
            );
        });
    });
    wait_for_step(&view, cx, "the confirmation", is_confirm);
    click("button-signin-confirm-cancel", cx);
    wait_until(cx, "the welcome page", |cx| {
        page(&view, cx) == Page::Welcome
    });
    cx.update(|window, cx| {
        view_signin.update(cx, |signin, cx| {
            signin.start(
                familychat_core::signin::SigninStart::Link(FIXTURE_LINK.into()),
                window,
                cx,
            );
        });
    });
    wait_for_step(&view, cx, "the confirmation", is_confirm);
    keys("escape", cx);
    wait_until(cx, "the welcome page", |cx| {
        page(&view, cx) == Page::Welcome
    });
    assert_eq!(signin.redeemed(), 0);
    assert_eq!(
        signin.answers(),
        [
            FixtureAnswer::UsePassword,
            FixtureAnswer::Cancel,
            FixtureAnswer::Cancel,
            FixtureAnswer::Cancel,
        ]
    );
}

/// PLAN.md, section 6.7: a QR image on the clipboard is read here and
/// becomes the sign-in link, which still has to be confirmed.
#[gpui::test]
fn a_pasted_qr_image_decodes_to_the_link(cx: &mut TestAppContext) {
    let (view, cx, startup) = open_signin(cx, InitialView::CodeEntry);
    cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(
        ImageFormat::Png,
        QR_SCREENSHOT.to_vec(),
    )));
    keys("secondary-v", cx);
    wait_for_step(&view, cx, "the confirmation", is_confirm);
    let signin = startup.signin();
    assert_eq!(
        signin.events().first(),
        Some(&FixtureEvent::Started(FixtureStart::Link {
            with_code: true
        }))
    );
    assert_eq!(signin.redeemed(), 0);
    // The link went to the sign-in, not into the field.
    let signin_entity = signin_view(&view, cx);
    let field = cx.read_entity(&signin_entity, |view, _| view.link_input().clone());
    assert_eq!(
        cx.read_entity(&field, |field, _| field.text().to_owned()),
        ""
    );
}

#[gpui::test]
fn a_dropped_image_file_decodes_to_the_link(cx: &mut TestAppContext) {
    let (view, cx, startup) = open_signin(cx, InitialView::CodeEntry);
    let path =
        std::env::temp_dir().join(format!("familychat-signin-qr-{}.png", std::process::id()));
    std::fs::write(&path, QR_SCREENSHOT).expect("the image is written");
    let signin = signin_view(&view, cx);
    cx.update(|window, cx| {
        signin.update(cx, |signin, cx| {
            signin.read_image(ImageSource::File(path.clone()), window, cx);
        });
    });
    wait_for_step(&view, cx, "the confirmation", is_confirm);
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        startup.signin().events().first(),
        Some(&FixtureEvent::Started(FixtureStart::Link {
            with_code: true
        }))
    );
}

#[gpui::test]
fn an_image_without_a_code_says_so_on_the_code_page(cx: &mut TestAppContext) {
    let (view, cx, startup) = open_signin(cx, InitialView::CodeEntry);
    let signin = signin_view(&view, cx);
    cx.update(|window, cx| {
        signin.update(cx, |signin, cx| {
            signin.read_image(ImageSource::Bytes(b"not an image".to_vec()), window, cx);
        });
    });
    wait_until(cx, "the problem", |cx| {
        cx.read_entity(&signin, |signin, _| {
            !signin.is_reading_image() && signin.code_problem().is_some()
        })
    });
    assert_eq!(page(&view, cx), Page::Code);
    assert!(startup.signin().events().is_empty(), "no flow started");
}

#[gpui::test]
fn pasted_text_that_is_not_a_link_returns_to_the_code_page(cx: &mut TestAppContext) {
    let (view, cx, _startup) = open_signin(cx, InitialView::CodeEntry);
    cx.simulate_input("not a link at all");
    keys("enter", cx);
    let signin = signin_view(&view, cx);
    wait_until(cx, "the problem", |cx| {
        cx.read_entity(&signin, |signin, _| {
            signin.code_problem() == Some("That isn't a Family Chat sign-in link.")
        })
    });
    assert_eq!(page(&view, cx), Page::Code);
    // The field gave its text up when it was used.
    let field = cx.read_entity(&signin, |view, _| view.link_input().clone());
    assert_eq!(
        cx.read_entity(&field, |field, _| field.text().to_owned()),
        ""
    );
}

#[gpui::test]
fn a_used_code_falls_back_to_the_password_form(cx: &mut TestAppContext) {
    let startup = FixtureStartup::new(&[FixtureProfile::Ready]);
    startup
        .signin()
        .set_code_outcome(FixtureOutcome::Problem(SigninProblem::new(
            "That code has been used or has expired. Ask your parent for a new one.",
        )));
    let (view, cx) = open(
        cx,
        startup,
        InitialView::PastedLink(FIXTURE_LINK.to_owned()),
    );
    wait_for_step(&view, cx, "the confirmation", is_confirm);
    click("button-signin-confirm-continue", cx);
    wait_for_step(&view, cx, "the password form", |step| {
        matches!(step, SigninStep::Password(form)
            if form.problem.as_ref().is_some_and(|problem| problem.message.starts_with("That code has been used")))
    });
    // The form is the link's: its server and hint, focus in the password.
    let signin = signin_view(&view, cx);
    let password = cx.read_entity(&signin, |view, _| view.password_input().clone());
    cx.update(|window, cx| {
        assert!(password.focus_handle(cx).is_focused(window));
    });
}

#[gpui::test]
fn a_rate_limit_counts_down_and_holds_the_form(cx: &mut TestAppContext) {
    let (view, cx, startup) = open_signin(cx, InitialView::Default);
    startup
        .signin()
        .start_at(steps::rate_limited(Duration::from_secs(30)));
    let signin = signin_view(&view, cx);
    cx.update(|window, cx| {
        signin.update(cx, |signin, cx| signin.sign_in_with_password(window, cx));
    });
    wait_for_step(&view, cx, "the form", |step| {
        matches!(step, SigninStep::Password(_))
    });
    let wait = cx.read_entity(&signin, |signin, _| signin.seconds_to_wait());
    assert!(matches!(wait, Some(29 | 30)), "{wait:?}");
    cx.simulate_input("hunter2");
    keys("enter", cx);
    std::thread::sleep(Duration::from_millis(50));
    cx.run_until_parked();
    assert!(
        startup.signin().answers().is_empty(),
        "nothing is sent while the server asks to wait"
    );
}
