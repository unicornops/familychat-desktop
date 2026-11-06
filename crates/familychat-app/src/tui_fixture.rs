//! `familychat launch-tui-fixture VIEW`: the terminal UI on a fixture
//! view, for looking at a screen by hand in a real terminal (ADR 0008).
//! Zmail's `launch-tui-fixture` is the pattern.
//!
//! The views are `familychat-tui`'s (`fixture_views`), the states behind
//! them `familychat-test-support`'s fixtures: no profile, keychain or
//! network. Only builds with the `ui-fixture` feature have the command;
//! release builds never do (`scripts/check-release-features.sh`).

use std::time::Duration;

use anyhow::{Result, bail};
use familychat_core::signin::{SigninStep, Working};
use familychat_test_support::fixtures::{
    FixtureProfile, FixtureRooms, FixtureSetup, rooms::now_ms, steps,
};
use familychat_tui::fixture_views::{
    FixtureView, VIEWS, ViewProfile, ViewRooms, ViewStep, fixture_view,
};

/// Checks that `view` is a terminal fixture view.
pub(crate) fn check_view(view: &str) -> Result<()> {
    match fixture_view(view) {
        Some(_) => Ok(()),
        None => bail!(
            "unknown terminal fixture view `{view}`; the views are: {}",
            VIEWS.join(", ")
        ),
    }
}

/// The fixture state behind `view`, its times counted back from `now`.
pub(crate) fn setup(view: &FixtureView, now: u64) -> FixtureSetup {
    FixtureSetup {
        profile: match view.profile {
            ViewProfile::Ready => FixtureProfile::Ready,
            ViewProfile::KeychainLocked => FixtureProfile::KeychainLocked,
            ViewProfile::KeychainUnreachable => FixtureProfile::KeychainUnreachable,
            ViewProfile::LostKey => FixtureProfile::LostKey,
        },
        step: view.step.map(|step| match step {
            ViewStep::Redeeming => SigninStep::Working(Working::RedeemingCode),
            ViewStep::CheckingServer => SigninStep::Working(Working::CheckingServer),
            ViewStep::CodeUsedOrExpired => steps::code_used_or_expired(),
            ViewStep::CodeNotUsed => steps::code_not_used(),
            ViewStep::WrongPassword => steps::wrong_password(),
            ViewStep::RateLimited(wait) => steps::rate_limited(wait),
            ViewStep::NotFamilyChat => steps::not_family_chat(),
            ViewStep::AlreadySignedIn => steps::already_signed_in(),
            ViewStep::Methods => steps::methods(),
            ViewStep::BrowserOnly => steps::browser_only(),
            ViewStep::ConfirmBrowser => steps::confirm_browser(),
            ViewStep::Browser => steps::browser(),
        }),
        account: view.account.clone(),
        rooms: match view.rooms {
            ViewRooms::Family => FixtureRooms::Family,
            ViewRooms::FamilyNoInvites => FixtureRooms::FamilyNoInvites,
            ViewRooms::None => FixtureRooms::None,
            ViewRooms::Loading => FixtureRooms::Loading,
            ViewRooms::Large => FixtureRooms::Large,
            ViewRooms::Hostile => FixtureRooms::Hostile,
        },
        now,
    }
}

/// Runs the terminal UI on `view`. Every few seconds a message arrives in
/// Mum's room, so the status line's notice and the bell can be seen.
pub(crate) fn launch(view: &str, version: &str) -> Result<()> {
    let Some(fixture) = fixture_view(view) else {
        bail!("unknown terminal fixture view `{view}`");
    };
    let startup = setup(&fixture, now_ms()).startup();
    let session = startup.session().clone();
    std::thread::Builder::new()
        .name("familychat-tui-fixture-news".to_owned())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(20));
                let mut rooms = session.rooms();
                for room in rooms.iter_mut().filter(|room| {
                    room.room_id.as_str() == familychat_test_support::fixtures::rooms::MUM_ROOM
                }) {
                    room.unread.notifications += 1;
                    room.unread.messages += 1;
                }
                session.set_rooms(rooms);
            }
        })?;
    familychat_tui::run(familychat_tui::Launch {
        startup,
        version: version.to_owned(),
        settings: familychat_tui::Settings::default(),
        initial: fixture.initial,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_view_has_a_fixture_and_others_do_not() {
        for view in VIEWS {
            check_view(view).unwrap_or_else(|error| panic!("{view}: {error}"));
            let fixture = fixture_view(view).expect("a view");
            drop(setup(&fixture, now_ms()).startup());
        }
        assert!(check_view("inbox").is_err());
    }

    /// Every view has its snapshots, at both sizes, in familychat-tui.
    #[test]
    fn every_view_has_snapshots() {
        let snapshots = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../familychat-tui/src/snapshots");
        for view in VIEWS {
            for size in ["80x24", "160x48"] {
                let file = snapshots.join(format!("{view}-{size}.txt"));
                assert!(file.is_file(), "{} is missing", file.display());
            }
        }
    }
}
