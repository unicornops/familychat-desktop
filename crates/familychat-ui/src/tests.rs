//! View tests on GPUI's headless test platform.

use std::time::Duration;

use familychat_core::{AppBootstrap, RuntimeSummary, ThemeAppearance, UpstreamPins};
use familychat_session::BackgroundRuntime;
use familychat_theme::{ThemeFamily, bundled};
use gpui::{TestAppContext, WindowAppearance};

use super::{ShellView, appearance_for};

fn bootstrap() -> AppBootstrap {
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

#[gpui::test]
fn the_window_opens_in_the_theme_for_its_appearance(cx: &mut TestAppContext) {
    let window = cx.add_window(|window, cx| ShellView::new(bootstrap(), window, cx));
    window
        .update(cx, |view, window, _| {
            let expected = appearance_for(window.appearance());
            assert_eq!(view.theme().appearance, expected);
            assert!(view.theme().family_name == "Family Chat");
            assert_eq!(
                view.build_line().as_ref(),
                "Version 0.0.1 · GPUI from Zed v1.27.0 (c84af5b99cbc)"
            );
        })
        .expect("the window is open");
    // A frame draws without panicking.
    cx.run_until_parked();
}

/// The interop pattern every Matrix call will use (PLAN.md, section 5.2):
/// the future runs on the dedicated Tokio runtime, which GPUI's executors
/// are not, and a GPUI task awaits its result through a runtime-agnostic
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
