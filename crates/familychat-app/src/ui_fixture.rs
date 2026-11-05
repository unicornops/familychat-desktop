//! `familychat launch-ui-fixture VIEW [dark|light]`: the window on a
//! fixture state, for the screenshot matrix
//! (`scripts/capture-ui-screenshots.sh --matrix`, PLAN.md section 9) and
//! for looking at a screen by hand. Zmail's `ui_fixture.rs` is the pattern.
//!
//! The fixture is `familychat-test-support`'s: a `FixtureStartup` over a
//! `FixtureSession` and a `FixtureSignin`, which say what the app says
//! (the sign-in's and the store's own sentences) and never touch the
//! network, the keychain or the profile. Only builds with the `ui-fixture`
//! feature have the command; release builds never do
//! (`scripts/check-release-features.sh`).

use std::{sync::Arc, time::Duration};

use anyhow::{Result, bail};
use familychat_core::{
    ThemeAppearance,
    rooms::RoomListFilter,
    signin::{SigninStep, Working},
    sync::SyncState,
};
use familychat_test_support::fixtures::{
    FIXTURE_LINK, FixtureProfile, FixtureSession, FixtureSignin, FixtureStartup, fixture_user,
    rooms::{self, LARGE_EVENTS, LARGE_ROOMS},
    steps,
};
use familychat_ui::{InitialView, LaunchOptions, RoomsLaunch};

/// Every fixture view, in the screenshot matrix's order.
pub(crate) const VIEWS: &[&str] = &[
    "welcome",
    "command-palette",
    "code",
    "code-not-a-link",
    "code-no-qr",
    "confirm",
    "redeeming",
    "code-used-or-expired",
    "code-not-used",
    "already-signed-in",
    "password",
    "password-wrong",
    "password-rate-limited",
    "password-not-family-chat",
    "checking-server",
    "other-server",
    "keychain-locked",
    "keychain-unavailable",
    "lost-key",
    "signed-in",
    "signed-in-connecting",
    "signed-in-offline",
    "signed-in-suspended",
    "signed-in-signed-out",
    "signed-in-not-supported",
    "rooms-unread",
    "rooms-people",
    "rooms-invites",
    "rooms-filter-empty",
    "rooms-empty",
    "rooms-loading",
    "room",
    "room-dm",
    "room-rich",
    "room-media",
    "room-changes",
    "room-changes-expanded",
    "room-unreadable",
    "room-not-encrypted",
    "room-empty",
    "room-members",
    "room-invite",
    "link-prompt",
    "link-prompt-mismatched",
    "room-large",
];

/// The rooms a signed-in fixture has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rooms {
    /// The Smith family's.
    Family,
    /// The family's, without the invitation.
    FamilyNoInvites,
    /// None at all.
    None,
    /// Still loading.
    Loading,
    /// The 1,000-room, 10,000-event fixture.
    Large,
}

/// What a fixture view opens on.
#[derive(Debug)]
struct Fixture {
    profile: FixtureProfile,
    initial_view: InitialView,
    /// The first step the next sign-in shows.
    step: Option<SigninStep>,
    /// A signed-in account in this sync state.
    account: Option<SyncState>,
    /// Its rooms.
    rooms: Rooms,
}

impl Fixture {
    fn signin(initial_view: InitialView) -> Self {
        Self {
            profile: FixtureProfile::Ready,
            initial_view,
            step: None,
            account: None,
            rooms: Rooms::Family,
        }
    }

    fn at(mut self, step: SigninStep) -> Self {
        self.step = Some(step);
        self
    }

    fn profile(profile: FixtureProfile) -> Self {
        Self {
            profile,
            ..Self::signin(InitialView::Default)
        }
    }

    fn signed_in(sync: SyncState) -> Self {
        Self {
            account: Some(sync),
            ..Self::signin(InitialView::Default)
        }
    }

    /// The family's rooms, opened as `launch` says.
    fn rooms(launch: RoomsLaunch) -> Self {
        Self {
            initial_view: InitialView::Rooms(launch),
            ..Self::signed_in(SyncState::Running)
        }
    }

    /// The family's rooms with `room` open.
    fn room(room: &str) -> Self {
        Self::rooms(RoomsLaunch {
            room: Some(room.to_owned()),
            ..RoomsLaunch::default()
        })
    }

    fn with_rooms(mut self, rooms: Rooms) -> Self {
        self.rooms = rooms;
        self
    }
}

/// The family's room list on `filter`.
fn filtered(filter: RoomListFilter) -> Fixture {
    Fixture::rooms(RoomsLaunch {
        filter,
        ..RoomsLaunch::default()
    })
}

fn fixture(view: &str) -> Result<Fixture> {
    let link = || InitialView::PastedLink(FIXTURE_LINK.to_owned());
    Ok(match view {
        "welcome" => Fixture::signin(InitialView::Default),
        "command-palette" => Fixture::signin(InitialView::CommandPalette),
        "code" => Fixture::signin(InitialView::CodeEntry),
        "code-not-a-link" => Fixture::signin(InitialView::PastedLink(
            "Here's the code for your laptop: 4F7Q-2K".to_owned(),
        )),
        "code-no-qr" => Fixture::signin(InitialView::CodeEntryNoQrCode),
        "confirm" => Fixture::signin(link()),
        "redeeming" => Fixture::signin(link()).at(SigninStep::Working(Working::RedeemingCode)),
        "code-used-or-expired" => Fixture::signin(link()).at(steps::code_used_or_expired()),
        "code-not-used" => Fixture::signin(link()).at(steps::code_not_used()),
        "already-signed-in" => Fixture::signin(link()).at(steps::already_signed_in()),
        "password" => Fixture::signin(InitialView::Password),
        "password-wrong" => Fixture::signin(InitialView::Password).at(steps::wrong_password()),
        "password-rate-limited" => {
            Fixture::signin(InitialView::Password).at(steps::rate_limited(Duration::from_secs(42)))
        }
        "password-not-family-chat" => {
            Fixture::signin(InitialView::Password).at(steps::not_family_chat())
        }
        "checking-server" => {
            Fixture::signin(InitialView::Password).at(SigninStep::Working(Working::CheckingServer))
        }
        "other-server" => Fixture::signin(InitialView::OtherServer),
        "keychain-locked" => Fixture::profile(FixtureProfile::KeychainLocked),
        "keychain-unavailable" => Fixture::profile(FixtureProfile::KeychainUnreachable),
        "lost-key" => Fixture::profile(FixtureProfile::LostKey),
        "signed-in" => Fixture::signed_in(SyncState::Running),
        "signed-in-connecting" => Fixture::signed_in(SyncState::Connecting),
        "signed-in-offline" => Fixture::signed_in(SyncState::Offline {
            retry_in: Duration::from_secs(8),
        }),
        "signed-in-suspended" => Fixture::signed_in(SyncState::Suspended {
            recheck_in: Duration::from_secs(120),
        }),
        "signed-in-signed-out" => Fixture::signed_in(SyncState::SignedOut { soft_logout: false }),
        "signed-in-not-supported" => Fixture::signed_in(SyncState::NotSupported),
        "rooms-unread" => filtered(RoomListFilter::Unread),
        "rooms-people" => filtered(RoomListFilter::People),
        "rooms-invites" => filtered(RoomListFilter::Invites),
        "rooms-filter-empty" => {
            filtered(RoomListFilter::Invites).with_rooms(Rooms::FamilyNoInvites)
        }
        "rooms-empty" => Fixture::signed_in(SyncState::Running).with_rooms(Rooms::None),
        "rooms-loading" => Fixture::signed_in(SyncState::Connecting).with_rooms(Rooms::Loading),
        "room" => Fixture::room(rooms::FAMILY_ROOM),
        "room-dm" => Fixture::room(rooms::MUM_ROOM),
        "room-rich" => Fixture::room(rooms::HOLIDAY_ROOM),
        "room-media" => Fixture::room(rooms::PHOTOS_ROOM),
        "room-changes" => Fixture::room(rooms::FOOTBALL_ROOM),
        "room-changes-expanded" => Fixture::rooms(RoomsLaunch {
            room: Some(rooms::FOOTBALL_ROOM.to_owned()),
            expand_changes: true,
            ..RoomsLaunch::default()
        }),
        "room-unreadable" => Fixture::room(rooms::SECRETS_ROOM),
        "room-not-encrypted" => Fixture::room(rooms::HOMEWORK_ROOM),
        "room-empty" => Fixture::room(rooms::EMPTY_ROOM),
        "room-members" => Fixture::rooms(RoomsLaunch {
            room: Some(rooms::FAMILY_ROOM.to_owned()),
            members: true,
            ..RoomsLaunch::default()
        }),
        "room-invite" => Fixture::room(rooms::INVITE_ROOM),
        "link-prompt" => Fixture::rooms(RoomsLaunch {
            room: Some(rooms::HOLIDAY_ROOM.to_owned()),
            link: Some((
                "https://cottages.example/booking/4471".to_owned(),
                "cottages.example".to_owned(),
            )),
            ..RoomsLaunch::default()
        }),
        "link-prompt-mismatched" => Fixture::rooms(RoomsLaunch {
            room: Some(rooms::HOLIDAY_ROOM.to_owned()),
            link: Some((
                "https://tickets.evil.example/claim".to_owned(),
                "https://www.bank.example".to_owned(),
            )),
            ..RoomsLaunch::default()
        }),
        "room-large" => Fixture::room(rooms::FAMILY_ROOM).with_rooms(Rooms::Large),
        other => bail!(
            "unknown fixture view `{other}`; the views are: {}",
            VIEWS.join(", ")
        ),
    })
}

/// Parses `[dark|light]`.
pub(crate) fn parse_theme(theme: Option<&str>) -> Result<Option<ThemeAppearance>> {
    Ok(match theme {
        None => None,
        Some("dark") => Some(ThemeAppearance::Dark),
        Some("light") => Some(ThemeAppearance::Light),
        Some(other) => bail!("the theme is `dark` or `light`, not `{other}`"),
    })
}

/// Checks that `view` is a fixture view.
pub(crate) fn check_view(view: &str) -> Result<()> {
    fixture(view).map(|_| ())
}

/// The startup and launch options for `view`.
pub(crate) fn startup(
    view: &str,
    appearance: Option<ThemeAppearance>,
) -> Result<(Arc<FixtureStartup>, LaunchOptions)> {
    let fixture = fixture(view)?;
    let session = FixtureSession::new();
    if let Some(sync) = fixture.account {
        session.add_account(&fixture_user(), sync);
        match fixture.rooms {
            Rooms::Family => session.with_family(),
            Rooms::FamilyNoInvites => {
                session.with_family();
                session.set_rooms(
                    session
                        .rooms()
                        .into_iter()
                        .filter(|room| !room.is_invite())
                        .collect(),
                );
            }
            Rooms::None => {}
            Rooms::Loading => session.set_rooms_loading(true),
            Rooms::Large => session.with_large(LARGE_ROOMS, LARGE_EVENTS),
        }
    }
    let signin = FixtureSignin::new(session.clone());
    if let Some(step) = fixture.step {
        signin.start_at(step);
    }
    let startup = FixtureStartup::with(session, signin, &[fixture.profile]);
    Ok((
        startup,
        LaunchOptions {
            initial_view: fixture.initial_view,
            appearance,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_view_has_a_fixture_and_others_do_not() {
        for view in VIEWS {
            startup(view, None).unwrap_or_else(|error| panic!("{view}: {error}"));
        }
        let error = check_view("inbox").expect_err("not a view");
        assert!(error.to_string().contains("signed-in-suspended"));
        assert_eq!(
            parse_theme(Some("dark")).ok(),
            Some(Some(ThemeAppearance::Dark))
        );
        assert!(parse_theme(Some("blue")).is_err());
    }

    /// The screenshot script captures every view in dark and light, and
    /// nothing else.
    #[test]
    fn the_screenshot_matrix_covers_every_view_in_both_themes() {
        let script = include_str!("../../../scripts/capture-ui-screenshots.sh");
        let start = script.find("\nMATRIX=(").expect("the script has a matrix");
        let end = start + script[start..].find("\n)").expect("the matrix ends");
        let matrix: Vec<(&str, &str)> = script[start..end]
            .lines()
            .map(str::trim)
            .filter_map(|line| line.strip_prefix('"')?.strip_suffix('"'))
            .filter_map(|entry| {
                let mut fields = entry.split_whitespace();
                let _name = fields.next()?;
                Some((fields.next()?, fields.next()?))
            })
            .collect();
        for view in VIEWS {
            for theme in ["dark", "light"] {
                assert!(
                    matrix.contains(&(view, theme)),
                    "the matrix lacks {view} in {theme}"
                );
            }
        }
        for (view, _) in &matrix {
            assert!(VIEWS.contains(view), "{view} is not a fixture view");
        }
    }
}
