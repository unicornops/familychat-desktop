//! The terminal UI's fixture views: every screen the snapshot tests draw
//! and `familychat launch-tui-fixture VIEW` opens (PLAN.md, section 9).
//!
//! A view is data: how the profile opens, the sign-in step to show, a
//! signed-in account's sync state and rooms, and where the UI starts.
//! `familychat-test-support`'s fixtures make it real; this crate never
//! depends on them, so the snapshot tests and the app's launcher each map
//! these names onto them (a test in `familychat-app` checks that every
//! view here has snapshots, and the launcher opens every one). Only test
//! builds and the `fixture-views` feature (which the app's `ui-fixture`
//! turns on, never in a release) have this module.

use std::time::Duration;

use familychat_core::{rooms::RoomListFilter, sync::SyncState};

use crate::app::{InitialView, RoomsLaunch};

/// A pasted sign-in link with a code, as a parent's QR code carries it
/// (`familychat-test-support`'s `FIXTURE_LINK`).
pub const FIXTURE_LINK: &str = "https://safechat.family/app/login?account_provider=smith.safechat.family&login_hint=mxid:%40ana%3Asmith.safechat.family&hs=smith.safechat.family&token=fixture-login-token";

/// How the profile opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewProfile {
    /// Ready.
    Ready,
    /// The keychain is locked.
    KeychainLocked,
    /// The keychain can't be reached.
    KeychainUnreachable,
    /// The data's key is gone.
    LostKey,
}

/// A canned sign-in step (`familychat-test-support`'s `steps`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewStep {
    /// Redeeming a code.
    Redeeming,
    /// Finding the server.
    CheckingServer,
    /// The password form after a used or expired code.
    CodeUsedOrExpired,
    /// The password form after a code that couldn't be used.
    CodeNotUsed,
    /// A wrong password.
    WrongPassword,
    /// Rate-limited for this long.
    RateLimited(Duration),
    /// A server outside the allowlist.
    NotFamilyChat,
    /// The account is signed in already.
    AlreadySignedIn,
    /// Another server's ways to sign in: the browser, a QR code and the
    /// password (ADR 0010).
    Methods,
    /// A server whose only way is the browser.
    BrowserOnly,
    /// "Continue in your browser?".
    ConfirmBrowser,
    /// Waiting for the browser.
    Browser,
}

/// A signed-in account's rooms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewRooms {
    /// The Smith family's.
    Family,
    /// The family's, without the invitation.
    FamilyNoInvites,
    /// None.
    None,
    /// Still loading.
    Loading,
    /// 1,000 rooms, 10,000 events.
    Large,
    /// Rooms full of terminal escape sequences.
    Hostile,
}

/// A fixture view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureView {
    /// How the profile opens.
    pub profile: ViewProfile,
    /// The sign-in's first step, whatever it starts with.
    pub step: Option<ViewStep>,
    /// A signed-in account in this sync state.
    pub account: Option<SyncState>,
    /// Its rooms.
    pub rooms: ViewRooms,
    /// Where the UI starts.
    pub initial: InitialView,
}

/// Every fixture view, in order.
pub const VIEWS: &[&str] = &[
    "welcome",
    "command-palette",
    "code",
    "code-typed",
    "code-not-a-link",
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
    "other-server-methods",
    "other-server-browser-only",
    "browser-confirm",
    "browser-waiting",
    "keychain-locked",
    "keychain-unavailable",
    "lost-key",
    "rooms",
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
    "room-spoilers",
    "room-media",
    "room-changes",
    "room-unreadable",
    "room-not-encrypted",
    "room-empty",
    "room-members",
    "room-invite",
    "link-prompt",
    "link-prompt-mismatched",
    "save-prompt",
    "room-large",
    "hostile",
    "hostile-room",
];

/// The family's rooms, by id (`familychat-test-support`'s `rooms`).
mod ids {
    pub const FAMILY: &str = "!family:smith.safechat.family";
    pub const MUM: &str = "!mum:smith.safechat.family";
    pub const HOMEWORK: &str = "!homework:smith.safechat.family";
    pub const HOLIDAY: &str = "!holiday:smith.safechat.family";
    pub const PHOTOS: &str = "!photos:smith.safechat.family";
    pub const FOOTBALL: &str = "!football:matrix.example.org";
    pub const INVITE: &str = "!cousins:smith.safechat.family";
    pub const EMPTY: &str = "!new:smith.safechat.family";
    pub const SECRETS: &str = "!secrets:smith.safechat.family";
    pub const HOSTILE: &str = "!hostile:smith.safechat.family";
}

impl FixtureView {
    fn signin(initial: InitialView) -> Self {
        Self {
            profile: ViewProfile::Ready,
            step: None,
            account: None,
            rooms: ViewRooms::Family,
            initial,
        }
    }

    fn at(mut self, step: ViewStep) -> Self {
        self.step = Some(step);
        self
    }

    fn profile(profile: ViewProfile) -> Self {
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

    fn rooms(launch: RoomsLaunch) -> Self {
        Self {
            initial: InitialView::Rooms(launch),
            ..Self::signed_in(SyncState::Running)
        }
    }

    fn room(room: &str) -> Self {
        Self::rooms(RoomsLaunch {
            room: Some(room.to_owned()),
            ..RoomsLaunch::default()
        })
    }

    fn filtered(filter: RoomListFilter) -> Self {
        Self::rooms(RoomsLaunch {
            filter,
            ..RoomsLaunch::default()
        })
    }

    fn with_rooms(mut self, rooms: ViewRooms) -> Self {
        self.rooms = rooms;
        self
    }
}

/// The view called `name`.
pub fn fixture_view(name: &str) -> Option<FixtureView> {
    let link = || InitialView::PastedLink(FIXTURE_LINK.to_owned());
    let password = || InitialView::Password;
    Some(match name {
        "welcome" => FixtureView::signin(InitialView::Default),
        "command-palette" => FixtureView::signin(InitialView::Palette),
        "code" => FixtureView::signin(InitialView::CodeEntry),
        "code-typed" => FixtureView::signin(InitialView::TypedLink(FIXTURE_LINK.to_owned())),
        "code-not-a-link" => FixtureView::signin(InitialView::PastedLink(
            "Here's the code for your laptop: 4F7Q-2K".to_owned(),
        )),
        "confirm" => FixtureView::signin(link()),
        "redeeming" => FixtureView::signin(link()).at(ViewStep::Redeeming),
        "code-used-or-expired" => FixtureView::signin(link()).at(ViewStep::CodeUsedOrExpired),
        "code-not-used" => FixtureView::signin(link()).at(ViewStep::CodeNotUsed),
        "already-signed-in" => FixtureView::signin(link()).at(ViewStep::AlreadySignedIn),
        "password" => FixtureView::signin(password()),
        "password-wrong" => FixtureView::signin(password()).at(ViewStep::WrongPassword),
        "password-rate-limited" => {
            FixtureView::signin(password()).at(ViewStep::RateLimited(Duration::from_secs(42)))
        }
        "password-not-family-chat" => FixtureView::signin(password()).at(ViewStep::NotFamilyChat),
        "checking-server" => FixtureView::signin(password()).at(ViewStep::CheckingServer),
        "other-server" => FixtureView::signin(InitialView::OtherServer),
        "other-server-methods" => {
            FixtureView::signin(InitialView::OtherServer).at(ViewStep::Methods)
        }
        "other-server-browser-only" => {
            FixtureView::signin(InitialView::OtherServer).at(ViewStep::BrowserOnly)
        }
        "browser-confirm" => {
            FixtureView::signin(InitialView::OtherServer).at(ViewStep::ConfirmBrowser)
        }
        "browser-waiting" => FixtureView::signin(InitialView::OtherServer).at(ViewStep::Browser),
        "keychain-locked" => FixtureView::profile(ViewProfile::KeychainLocked),
        "keychain-unavailable" => FixtureView::profile(ViewProfile::KeychainUnreachable),
        "lost-key" => FixtureView::profile(ViewProfile::LostKey),
        "rooms" => FixtureView::signed_in(SyncState::Running),
        "signed-in-connecting" => FixtureView::signed_in(SyncState::Connecting),
        "signed-in-offline" => FixtureView::signed_in(SyncState::Offline {
            retry_in: Duration::from_secs(8),
        }),
        "signed-in-suspended" => FixtureView::signed_in(SyncState::Suspended {
            recheck_in: Duration::from_secs(120),
        }),
        "signed-in-signed-out" => {
            FixtureView::signed_in(SyncState::SignedOut { soft_logout: false })
        }
        "signed-in-not-supported" => FixtureView::signed_in(SyncState::NotSupported),
        "rooms-unread" => FixtureView::filtered(RoomListFilter::Unread),
        "rooms-people" => FixtureView::filtered(RoomListFilter::People),
        "rooms-invites" => FixtureView::filtered(RoomListFilter::Invites),
        "rooms-filter-empty" => {
            FixtureView::filtered(RoomListFilter::Invites).with_rooms(ViewRooms::FamilyNoInvites)
        }
        "rooms-empty" => FixtureView::signed_in(SyncState::Running).with_rooms(ViewRooms::None),
        "rooms-loading" => {
            FixtureView::signed_in(SyncState::Connecting).with_rooms(ViewRooms::Loading)
        }
        "room" => FixtureView::room(ids::FAMILY),
        "room-dm" => FixtureView::room(ids::MUM),
        "room-rich" => FixtureView::room(ids::HOLIDAY),
        "room-spoilers" => FixtureView::rooms(RoomsLaunch {
            room: Some(ids::HOLIDAY.to_owned()),
            spoilers: true,
            ..RoomsLaunch::default()
        }),
        "room-media" => FixtureView::room(ids::PHOTOS),
        "room-changes" => FixtureView::room(ids::FOOTBALL),
        "room-unreadable" => FixtureView::room(ids::SECRETS),
        "room-not-encrypted" => FixtureView::room(ids::HOMEWORK),
        "room-empty" => FixtureView::room(ids::EMPTY),
        "room-members" => FixtureView::rooms(RoomsLaunch {
            room: Some(ids::FAMILY.to_owned()),
            members: true,
            ..RoomsLaunch::default()
        }),
        "room-invite" => FixtureView::room(ids::INVITE),
        "link-prompt" => FixtureView::rooms(RoomsLaunch {
            room: Some(ids::HOLIDAY.to_owned()),
            reference: Some(1),
            ..RoomsLaunch::default()
        }),
        "link-prompt-mismatched" => FixtureView::rooms(RoomsLaunch {
            room: Some(ids::HOLIDAY.to_owned()),
            reference: Some(3),
            ..RoomsLaunch::default()
        }),
        "save-prompt" => FixtureView::rooms(RoomsLaunch {
            room: Some(ids::PHOTOS.to_owned()),
            save: Some(7),
            ..RoomsLaunch::default()
        }),
        "room-large" => FixtureView::room(ids::FAMILY).with_rooms(ViewRooms::Large),
        "hostile" => FixtureView::signed_in(SyncState::Running).with_rooms(ViewRooms::Hostile),
        "hostile-room" => FixtureView::rooms(RoomsLaunch {
            room: Some(ids::HOSTILE.to_owned()),
            members: true,
            ..RoomsLaunch::default()
        })
        .with_rooms(ViewRooms::Hostile),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_view_is_defined_and_others_are_not() {
        for view in VIEWS {
            assert!(fixture_view(view).is_some(), "{view}");
        }
        assert!(fixture_view("inbox").is_none());
        let unique: std::collections::BTreeSet<_> = VIEWS.iter().collect();
        assert_eq!(unique.len(), VIEWS.len());
    }
}
