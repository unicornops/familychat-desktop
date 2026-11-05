//! View tests on GPUI's headless test platform (PLAN.md, section 9), over
//! `familychat-test-support`'s fixtures: the window opens the profile
//! through a `FixtureStartup`, signs in through a `FixtureSignin` and shows
//! a `FixtureSession`'s accounts.
//!
//! The fixture sign-in runs each flow on a thread of its own, as the live
//! one runs on the Tokio runtime, so the tests allow parking and wait for
//! the steps to arrive ([`wait_until`]).

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use familychat_core::{
    AppBootstrap, RuntimeSummary, ThemeAppearance, UpstreamPins, signin::SigninStep,
    sync::SyncState,
};
use familychat_session::BackgroundRuntime;
use familychat_test_support::fixtures::{
    FIXTURE_MOVED_TO, FixtureAnswer, FixtureEvent, FixtureProfile, FixtureSession, FixtureSignin,
    FixtureStart, FixtureStartup, fixture_user,
};
use familychat_theme::{ThemeFamily, bundled};
use gpui::{
    AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext, WindowAppearance,
    WindowDecorations,
};

use crate::shell::Screen;
use crate::signin::{Page, SigninView};
use crate::{InitialView, LaunchOptions, ShellView, appearance_for, decorations_for};

mod a11y;
mod forwarded_links;
mod hit_targets;
mod notifications;
mod perf;
mod rooms;
mod signin;

pub(super) fn bootstrap() -> AppBootstrap {
    let summary = |json: &str| {
        let family = ThemeFamily::from_json_str(json).expect("the bundled theme parses");
        let theme = family
            .default_theme()
            .expect("the bundled theme has a variant");
        family.summary(theme)
    };
    AppBootstrap {
        app_version: "0.0.1".to_owned(),
        runtime: RuntimeSummary {
            worker_threads: 1,
            bridge_verified: true,
        },
        dark_theme: summary(bundled::DARK),
        light_theme: summary(bundled::LIGHT),
        upstream: UpstreamPins {
            zed_commit: "c84af5b99cbc366d675064e52b23664a015b6e35".to_owned(),
            zed_tag: "v1.27.0".to_owned(),
        },
    }
}

/// The app's actions and keys, as `run` binds them, and the identity
/// notifications are posted under (GPUI's test platform records
/// notifications only once there is one).
fn init(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::actions::register(cx);
        crate::keymap::bind_keymap(cx);
        cx.set_app_identity(
            familychat_core::identity::APP_ID,
            familychat_core::identity::APP_NAME,
        );
    });
}

/// The window over `startup`, opened on `initial_view`.
pub(super) fn open(
    cx: &mut TestAppContext,
    startup: Arc<FixtureStartup>,
    initial_view: InitialView,
) -> (Entity<ShellView>, &mut VisualTestContext) {
    let (view, cx, _requests) = open_with_requests(cx, startup, initial_view);
    (view, cx)
}

/// The window over `startup`, opened on `initial_view`, with the sender
/// second launches' requests arrive on (as the app's listener sends them).
pub(super) fn open_with_requests(
    cx: &mut TestAppContext,
    startup: Arc<FixtureStartup>,
    initial_view: InitialView,
) -> (
    Entity<ShellView>,
    &mut VisualTestContext,
    familychat_core::requests::RequestSender,
) {
    // The fixture's flows answer from their own threads.
    cx.executor().allow_parking();
    init(cx);
    let (sender, receiver) = familychat_core::requests::request_channel();
    let inbox = crate::RequestInbox::new(sender.clone(), receiver);
    let (view, cx) = cx.add_window_view(move |window, cx| {
        ShellView::new(
            bootstrap(),
            startup,
            LaunchOptions {
                initial_view,
                appearance: Some(ThemeAppearance::Dark),
                requests: Some(inbox),
            },
            window,
            cx,
        )
    });
    cx.run_until_parked();
    (view, cx, sender)
}

/// The window on a ready profile with no account, on `initial_view`.
pub(super) fn open_signin(
    cx: &mut TestAppContext,
    initial_view: InitialView,
) -> (
    Entity<ShellView>,
    &mut VisualTestContext,
    Arc<FixtureStartup>,
) {
    let startup = FixtureStartup::new(&[FixtureProfile::Ready]);
    let (view, cx) = open(cx, startup.clone(), initial_view);
    (view, cx, startup)
}

/// Runs the window until `done` holds, for up to five seconds.
pub(super) fn wait_until(
    cx: &mut VisualTestContext,
    what: &str,
    mut done: impl FnMut(&mut VisualTestContext) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The sign-in view.
pub(super) fn signin_view(
    view: &Entity<ShellView>,
    cx: &mut VisualTestContext,
) -> Entity<SigninView> {
    cx.read_entity(view, |view, _| view.signin().cloned())
        .expect("the sign-in shows")
}

/// The sign-in's current step.
pub(super) fn step(view: &Entity<ShellView>, cx: &mut VisualTestContext) -> SigninStep {
    let signin = signin_view(view, cx);
    cx.read_entity(&signin, |signin, _| signin.step().clone())
}

/// The sign-in's page.
pub(super) fn page(view: &Entity<ShellView>, cx: &mut VisualTestContext) -> Page {
    let signin = signin_view(view, cx);
    cx.read_entity(&signin, |signin, _| signin.page())
}

/// Waits for a step that `matches`.
pub(super) fn wait_for_step(
    view: &Entity<ShellView>,
    cx: &mut VisualTestContext,
    what: &str,
    matches: impl Fn(&SigninStep) -> bool,
) {
    wait_until(cx, what, |cx| matches(&step(view, cx)));
}

/// Clicks the target `selector` (a `hit_target`'s debug selector).
pub(super) fn click(selector: &'static str, cx: &mut VisualTestContext) {
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}

/// Presses `keystrokes`.
pub(super) fn keys(keystrokes: &str, cx: &mut VisualTestContext) {
    cx.simulate_keystrokes(keystrokes);
    cx.run_until_parked();
}

/// The sign-in QR code fixture as a PNG (see `qr::tests`).
pub(super) const QR_SCREENSHOT: &[u8] = include_bytes!("testdata/signin-qr-screenshot.png");

#[test]
fn every_os_appearance_maps_to_a_bundled_theme() {
    assert_eq!(
        appearance_for(WindowAppearance::Light),
        ThemeAppearance::Light
    );
    assert_eq!(
        appearance_for(WindowAppearance::VibrantLight),
        ThemeAppearance::Light
    );
    assert_eq!(
        appearance_for(WindowAppearance::Dark),
        ThemeAppearance::Dark
    );
    assert_eq!(
        appearance_for(WindowAppearance::VibrantDark),
        ThemeAppearance::Dark
    );
}

#[test]
fn kde_gets_its_own_title_bar_and_the_rest_draw_one() {
    let is_server = |requested, desktop| {
        matches!(
            decorations_for(requested, desktop),
            WindowDecorations::Server
        )
    };
    assert!(!is_server(None, Some("GNOME")));
    assert!(!is_server(None, Some("ubuntu:GNOME")));
    assert!(!is_server(None, None));
    assert!(is_server(None, Some("KDE")));
    assert!(is_server(Some("server"), Some("GNOME")));
    assert!(!is_server(Some("client"), Some("KDE")));
}

#[gpui::test]
fn the_window_draws_in_the_pinned_theme(cx: &mut TestAppContext) {
    let (view, cx, _) = open_signin(cx, InitialView::Default);
    cx.read_entity(&view, |view, _| {
        assert_eq!(view.theme().appearance, ThemeAppearance::Dark);
        assert_eq!(view.theme().family_name, "Family Chat");
    });
}

/// The interop pattern every Matrix call uses (PLAN.md, section 5.2): the
/// future runs on the dedicated Tokio runtime, which GPUI's executors are
/// not, and a GPUI task awaits its result through a runtime-agnostic
/// channel.
#[gpui::test]
async fn a_gpui_task_awaits_a_future_that_ran_on_tokio(cx: &mut TestAppContext) {
    // The result arrives from another thread, so the test scheduler must
    // wait for it instead of failing as soon as nothing is runnable.
    cx.executor().allow_parking();
    let runtime = BackgroundRuntime::start(1).expect("the runtime starts");
    let (tx, rx) = tokio::sync::oneshot::channel();
    runtime.handle().spawn(async move {
        // Tokio's timer panics outside a Tokio runtime, so reaching the
        // send proves where the future ran.
        tokio::time::sleep(Duration::from_millis(10)).await;
        let on_tokio = tokio::runtime::Handle::try_current().is_ok();
        let thread = std::thread::current().name().map(str::to_owned);
        let _ = tx.send((on_tokio, thread));
    });

    let task = cx.spawn(async move |_cx| rx.await);
    let (on_tokio, thread) = task.await.expect("the Tokio task sends its result");

    assert!(on_tokio);
    assert_eq!(thread.as_deref(), Some("familychat-tokio-worker"));
    drop(runtime);
}

#[gpui::test]
fn a_locked_keychain_retries_until_the_profile_opens(cx: &mut TestAppContext) {
    let startup = FixtureStartup::new(&[FixtureProfile::KeychainLocked, FixtureProfile::Ready]);
    let (view, cx) = open(cx, startup.clone(), InitialView::Default);
    cx.read_entity(&view, |view, _| {
        assert!(matches!(
            view.screen(),
            Screen::Profile(crate::profile_screens::ProfileScreen::Keychain(_))
        ));
    });
    click("button-profile-retry", cx);
    wait_until(cx, "the sign-in", |cx| {
        cx.read_entity(&view, |view, _| view.signin().is_some())
    });
    assert_eq!(startup.opens(), 2);
    assert!(!startup.started_over());
}

#[gpui::test]
fn a_lost_key_starts_over_and_says_where_the_data_went(cx: &mut TestAppContext) {
    let startup = FixtureStartup::new(&[FixtureProfile::LostKey]);
    let (view, cx) = open(cx, startup.clone(), InitialView::Default);
    cx.read_entity(&view, |view, _| {
        assert!(matches!(
            view.screen(),
            Screen::Profile(crate::profile_screens::ProfileScreen::LostKey(_))
        ));
    });
    click("button-profile-start-over", cx);
    wait_until(cx, "the sign-in", |cx| {
        cx.read_entity(&view, |view, _| view.signin().is_some())
    });
    assert!(startup.started_over());
    let toasts = cx.read_entity(&view, |view, _| view.toasts().clone());
    let shown = cx.read_entity(&toasts, |toasts, _| toasts.visible());
    assert!(
        shown
            .iter()
            .any(|(_, message)| message.contains(FIXTURE_MOVED_TO)),
        "{shown:?}"
    );
}

#[gpui::test]
fn a_signed_in_account_shows_its_sync_state(cx: &mut TestAppContext) {
    let session = FixtureSession::new();
    let account = session.add_account(
        &fixture_user(),
        SyncState::Suspended {
            recheck_in: Duration::from_secs(60),
        },
    );
    let signin = FixtureSignin::new(session.clone());
    let startup = FixtureStartup::with(session.clone(), signin, &[FixtureProfile::Ready]);
    let (view, cx) = open(cx, startup, InitialView::Default);
    let signed_in = cx
        .read_entity(&view, |view, _| view.signed_in().cloned())
        .expect("the signed-in view shows");
    cx.read_entity(&signed_in, |view, _| {
        assert_eq!(view.status().sync.label(), "Paused");
    });
    // Suspended rechecks on its own: there is nothing to retry.
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(cx.debug_bounds("button-signed-in-retry").is_none());

    session.set_sync(
        &account,
        SyncState::Offline {
            retry_in: Duration::from_secs(4),
        },
    );
    wait_until(cx, "offline", |cx| {
        cx.read_entity(&signed_in, |view, _| {
            view.status().sync.label() == "Offline"
        })
    });
    click("button-signed-in-retry", cx);
    assert_eq!(session.retried(), [account]);
}

#[gpui::test]
fn the_command_palette_runs_the_sign_in_commands(cx: &mut TestAppContext) {
    let (view, cx, startup) = open_signin(cx, InitialView::Default);
    keys("secondary-k", cx);
    let palette = cx
        .read_entity(&view, |view, _| view.palette().cloned())
        .expect("the palette opens");
    let labels = cx.read_entity(&palette, |palette, _| palette.matching_labels());
    for expected in [
        "Sign in with a code from your parent",
        "Sign in with your password",
        "Use another Matrix server",
        "Quit Family Chat",
    ] {
        assert!(labels.iter().any(|label| label == expected), "{labels:?}");
    }
    assert!(!labels.iter().any(|label| label == "Command palette"));
    cx.update(|_, cx| palette.update(cx, |palette, cx| palette.set_query("password", cx)));
    let labels = cx.read_entity(&palette, |palette, _| palette.matching_labels());
    assert_eq!(
        labels.first().map(String::as_str),
        Some("Sign in with your password")
    );
    keys("enter", cx);
    wait_for_step(&view, cx, "the password form", |step| {
        matches!(step, SigninStep::Password(_))
    });
    assert!(cx.read_entity(&view, |view, _| view.palette().is_none()));
    assert_eq!(
        startup.signin().events().first(),
        Some(&FixtureEvent::Started(FixtureStart::FamilyChatPassword))
    );
}

#[gpui::test]
fn a_finished_sign_in_shows_the_account(cx: &mut TestAppContext) {
    let (view, cx, startup) = open_signin(cx, InitialView::Password);
    wait_for_step(&view, cx, "the password form", |step| {
        matches!(step, SigninStep::Password(_))
    });
    cx.simulate_input("smith.safechat.family");
    keys("tab", cx);
    cx.simulate_input("ana");
    keys("tab", cx);
    cx.simulate_input("hunter2");
    keys("enter", cx);
    wait_until(cx, "the signed-in view", |cx| {
        cx.read_entity(&view, |view, _| view.signed_in().is_some())
    });
    assert_eq!(
        startup.signin().answers(),
        [FixtureAnswer::SubmitPassword {
            server: "smith.safechat.family".to_owned(),
            username: "ana".to_owned(),
            with_password: true,
        }]
    );
}
