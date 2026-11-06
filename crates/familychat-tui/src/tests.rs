//! The terminal UI on ratatui's `TestBackend`, over
//! `familychat-test-support`'s fixtures with a fixed clock: a snapshot of
//! every fixture view at 80x24 and 160x48 (in `src/snapshots`), each with
//! its colour, `NO_COLOR` and ASCII variants; and the keys, mouse, copy,
//! paste, links, files, sign-in and notices.
//!
//! Each snapshot file holds the screen's text, a map of its styles (one
//! character a cell: `R` reversed, `B` bold, `I` italic, `U` underlined,
//! `X` struck through, a lower-case letter for a colour: `r`ed, `g`reen,
//! `y`ellow, `c`yan, `k` grey), the styles again under `NO_COLOR` (whose
//! text must be the same and whose map has no colour), and the text in
//! ASCII. `FAMILYCHAT_UPDATE_SNAPSHOTS=1 cargo test -p familychat-tui`
//! rewrites them; review the diff.

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{Datelike, TimeZone, Utc};
use familychat_core::{signin::SigninStep, sync::SyncState};
use familychat_test_support::fixtures::{
    FixtureProfile, FixtureRooms, FixtureSetup, FixtureStartup, steps,
};
use ratatui::{
    Terminal,
    backend::TestBackend,
    buffer::Buffer,
    style::{Color, Modifier},
};

use crate::{
    app::{App, AppOptions, Clock, Environment, InitialView, Settings},
    fixture_views::{FixtureView, VIEWS, ViewProfile, ViewRooms, ViewStep, fixture_view},
    view::draw,
};

mod behaviour;
mod hostile;

/// Thursday 5 November 2026, 12:00 UTC: the fixtures' times count back
/// from it.
pub(crate) const NOW: u64 = 1_793_880_000_000;

fn utc(timestamp_ms: u64) -> Option<chrono::DateTime<Utc>> {
    Utc.timestamp_millis_opt(i64::try_from(timestamp_ms).ok()?)
        .single()
}

/// `familychat-core`'s day wording, in UTC.
fn utc_day(timestamp_ms: u64, now_ms: u64) -> String {
    let (Some(day), Some(now)) = (utc(timestamp_ms), utc(now_ms)) else {
        return "Some time ago".to_owned();
    };
    match now
        .date_naive()
        .signed_duration_since(day.date_naive())
        .num_days()
    {
        0 => "Today".to_owned(),
        1 => "Yesterday".to_owned(),
        2..=6 => day.format("%A").to_string(),
        _ if day.year() == now.year() => day.format("%-d %B").to_string(),
        _ => day.format("%-d %B %Y").to_string(),
    }
}

fn utc_time(timestamp_ms: u64) -> String {
    utc(timestamp_ms)
        .map(|time| time.format("%H:%M").to_string())
        .unwrap_or_default()
}

/// The fixed clock every test draws with.
pub(crate) fn test_clock() -> Clock {
    Clock {
        now_ms: || NOW,
        time: utc_time,
        day: utc_day,
    }
}

/// Where the save question starts in the tests.
pub(crate) fn test_downloads() -> PathBuf {
    PathBuf::from("/home/ana/Downloads")
}

/// The test-support state a fixture view stands for.
pub(crate) fn setup(view: &FixtureView) -> FixtureSetup {
    FixtureSetup {
        profile: match view.profile {
            ViewProfile::Ready => FixtureProfile::Ready,
            ViewProfile::KeychainLocked => FixtureProfile::KeychainLocked,
            ViewProfile::KeychainUnreachable => FixtureProfile::KeychainUnreachable,
            ViewProfile::LostKey => FixtureProfile::LostKey,
        },
        step: view.step.map(|step| match step {
            ViewStep::Redeeming => {
                SigninStep::Working(familychat_core::signin::Working::RedeemingCode)
            }
            ViewStep::CheckingServer => {
                SigninStep::Working(familychat_core::signin::Working::CheckingServer)
            }
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
        now: NOW,
    }
}

/// The environment of a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Variant {
    /// Colours and Unicode.
    Default,
    /// `NO_COLOR`.
    NoColor,
    /// ASCII drawing.
    Ascii,
}

impl Variant {
    fn env(self) -> Environment {
        Environment {
            color: self != Self::NoColor,
            unicode: self != Self::Ascii,
            over_ssh: false,
            macos: false,
        }
    }
}

/// A terminal UI over `startup`.
pub(crate) fn app_over(
    startup: Arc<FixtureStartup>,
    initial: InitialView,
    variant: Variant,
) -> App {
    App::new(AppOptions {
        startup,
        env: variant.env(),
        clock: test_clock(),
        settings: Settings::default(),
        version: "0.0.0-test".to_owned(),
        initial,
        downloads: test_downloads(),
    })
}

/// A terminal UI on a fixture view, and its startup (for the session and
/// the sign-in's records).
pub(crate) fn app_for(view: &str, variant: Variant) -> (App, Arc<FixtureStartup>) {
    let fixture = fixture_view(view).unwrap_or_else(|| panic!("no fixture view {view}"));
    let startup = setup(&fixture).startup();
    (app_over(startup.clone(), fixture.initial, variant), startup)
}

/// A test terminal of this size.
pub(crate) struct Screen {
    terminal: Terminal<TestBackend>,
}

impl Screen {
    pub(crate) fn new(width: u16, height: u16) -> Self {
        Self {
            terminal: Terminal::new(TestBackend::new(width, height)).expect("a test terminal"),
        }
    }

    /// Draws once and returns the buffer.
    pub(crate) fn draw(&mut self, app: &mut App) -> Buffer {
        self.terminal
            .draw(|frame| draw(app, frame))
            .expect("the frame draws");
        self.terminal.backend().buffer().clone()
    }

    /// Draws and ticks until everything asked for has arrived and nothing
    /// changes, then draws the result.
    pub(crate) fn settle(&mut self, app: &mut App) -> Buffer {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut quiet = 0;
        loop {
            self.draw(app);
            app.tick();
            if app.is_settled() {
                quiet += 1;
                if quiet >= 10 {
                    break;
                }
            } else {
                quiet = 0;
            }
            assert!(Instant::now() < deadline, "the screen never settled");
            std::thread::sleep(Duration::from_millis(5));
        }
        self.draw(app)
    }
}

/// The buffer's text, one line per row, trailing spaces trimmed. On
/// Windows a path's backslashes read as slashes, so one snapshot serves.
pub(crate) fn text_of(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut out = String::new();
    for y in 0..area.height {
        let line: String = (0..area.width).map(|x| buffer[(x, y)].symbol()).collect();
        out.push_str(line.trim_end());
        out.push('\n');
    }
    if cfg!(windows) {
        out = out.replace('\\', "/");
    }
    out
}

fn color_letter(color: Color) -> Option<char> {
    Some(match color {
        Color::Reset => return None,
        Color::Red => 'r',
        Color::Green => 'g',
        Color::Yellow => 'y',
        Color::Cyan => 'c',
        Color::DarkGray => 'k',
        _ => 'o',
    })
}

/// The buffer's styles, one character a cell.
pub(crate) fn styles_of(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut out = String::new();
    for y in 0..area.height {
        let line: String = (0..area.width)
            .map(|x| {
                let cell = &buffer[(x, y)];
                if let Some(letter) = color_letter(cell.fg).or_else(|| color_letter(cell.bg)) {
                    return letter;
                }
                let modifier = cell.modifier;
                if modifier.contains(Modifier::REVERSED) {
                    'R'
                } else if modifier.contains(Modifier::BOLD) {
                    'B'
                } else if modifier.contains(Modifier::ITALIC) {
                    'I'
                } else if modifier.contains(Modifier::UNDERLINED) {
                    'U'
                } else if modifier.contains(Modifier::CROSSED_OUT) {
                    'X'
                } else {
                    ' '
                }
            })
            .collect();
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Characters only the Unicode drawing uses.
const UNICODE_DRAWING: &[char] = &['─', '│', '…', '•', '›', '▒', '┃', '┌', '┐', '└', '┘', '·'];

/// Compares (or, with `FAMILYCHAT_UPDATE_SNAPSHOTS`, writes)
/// `src/snapshots/<name>.txt`.
pub(crate) fn assert_snapshot(name: &str, actual: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/snapshots")
        .join(format!("{name}.txt"));
    if std::env::var_os("FAMILYCHAT_UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("the directory");
        std::fs::write(&path, actual).expect("the snapshot is written");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "no snapshot {}; run with FAMILYCHAT_UPDATE_SNAPSHOTS=1 and review it\n{actual}",
            path.display()
        )
    });
    // Git may check text files out with CRLF on Windows.
    let expected = expected.replace("\r\n", "\n");
    assert_eq!(
        actual, expected,
        "screen {name} differs from its snapshot:\n{actual}"
    );
}

/// Draws `view` at `width`x`height` in every variant, checks the
/// variants against each other and the snapshot.
fn snapshot_view(view: &str, width: u16, height: u16) {
    let mut drawn = Vec::new();
    for variant in [Variant::Default, Variant::NoColor, Variant::Ascii] {
        let (mut app, _startup) = app_for(view, variant);
        let buffer = Screen::new(width, height).settle(&mut app);
        drawn.push((text_of(&buffer), styles_of(&buffer)));
    }
    let [
        (text, styles),
        (no_color_text, no_color_styles),
        (ascii_text, _),
    ] = &drawn[..]
    else {
        unreachable!("three variants")
    };
    assert_eq!(text, no_color_text, "{view}: NO_COLOR changed the text");
    assert!(
        !no_color_styles.chars().any(|ch| ch.is_ascii_lowercase()),
        "{view}: NO_COLOR drew a colour:\n{no_color_styles}"
    );
    let unicode: Vec<char> = ascii_text
        .chars()
        .filter(|ch| UNICODE_DRAWING.contains(ch))
        .collect();
    assert!(
        unicode.is_empty(),
        "{view}: the ASCII variant drew {unicode:?}:\n{ascii_text}"
    );
    for (variant, (text, _)) in ["default", "NO_COLOR", "ASCII"].iter().zip(&drawn) {
        crate::tests::hostile::assert_clean(&format!("{view} {variant}"), text);
    }
    let file = format!(
        "{text}--- styles ---\n{styles}--- NO_COLOR styles ---\n{no_color_styles}--- ASCII ---\n{ascii_text}"
    );
    assert_snapshot(&format!("{view}-{width}x{height}"), &file);
}

macro_rules! snapshots {
    ($($test:ident => $view:literal),* $(,)?) => {
        $(
            #[test]
            fn $test() {
                snapshot_view($view, 80, 24);
                snapshot_view($view, 160, 48);
            }
        )*

        /// Every fixture view has its snapshot test, and nothing else.
        #[test]
        fn every_view_has_a_snapshot_test() {
            let tested = [$($view),*];
            for view in VIEWS {
                assert!(tested.contains(view), "{view} has no snapshot test");
            }
            for view in tested {
                assert!(VIEWS.contains(&view), "{view} is not a fixture view");
            }
        }
    };
}

snapshots! {
    snapshot_welcome => "welcome",
    snapshot_command_palette => "command-palette",
    snapshot_code => "code",
    snapshot_code_typed => "code-typed",
    snapshot_code_not_a_link => "code-not-a-link",
    snapshot_confirm => "confirm",
    snapshot_redeeming => "redeeming",
    snapshot_code_used_or_expired => "code-used-or-expired",
    snapshot_code_not_used => "code-not-used",
    snapshot_already_signed_in => "already-signed-in",
    snapshot_password => "password",
    snapshot_password_wrong => "password-wrong",
    snapshot_password_rate_limited => "password-rate-limited",
    snapshot_password_not_family_chat => "password-not-family-chat",
    snapshot_checking_server => "checking-server",
    snapshot_other_server => "other-server",
    snapshot_other_server_methods => "other-server-methods",
    snapshot_other_server_browser_only => "other-server-browser-only",
    snapshot_browser_confirm => "browser-confirm",
    snapshot_browser_waiting => "browser-waiting",
    snapshot_keychain_locked => "keychain-locked",
    snapshot_keychain_unavailable => "keychain-unavailable",
    snapshot_lost_key => "lost-key",
    snapshot_rooms => "rooms",
    snapshot_signed_in_connecting => "signed-in-connecting",
    snapshot_signed_in_offline => "signed-in-offline",
    snapshot_signed_in_suspended => "signed-in-suspended",
    snapshot_signed_in_signed_out => "signed-in-signed-out",
    snapshot_signed_in_not_supported => "signed-in-not-supported",
    snapshot_rooms_unread => "rooms-unread",
    snapshot_rooms_people => "rooms-people",
    snapshot_rooms_invites => "rooms-invites",
    snapshot_rooms_filter_empty => "rooms-filter-empty",
    snapshot_rooms_empty => "rooms-empty",
    snapshot_rooms_loading => "rooms-loading",
    snapshot_room => "room",
    snapshot_room_dm => "room-dm",
    snapshot_room_rich => "room-rich",
    snapshot_room_spoilers => "room-spoilers",
    snapshot_room_media => "room-media",
    snapshot_room_changes => "room-changes",
    snapshot_room_unreadable => "room-unreadable",
    snapshot_room_not_encrypted => "room-not-encrypted",
    snapshot_room_empty => "room-empty",
    snapshot_room_members => "room-members",
    snapshot_room_invite => "room-invite",
    snapshot_link_prompt => "link-prompt",
    snapshot_link_prompt_mismatched => "link-prompt-mismatched",
    snapshot_save_prompt => "save-prompt",
    snapshot_room_large => "room-large",
    snapshot_hostile => "hostile",
    snapshot_hostile_room => "hostile-room",
}

/// A screen drawn in the default variant, settled.
pub(crate) fn settled(view: &str, width: u16, height: u16) -> (App, Arc<FixtureStartup>, Screen) {
    let (mut app, startup) = app_for(view, Variant::Default);
    let mut screen = Screen::new(width, height);
    screen.settle(&mut app);
    (app, startup, screen)
}

/// A setup with nobody signed in.
pub(crate) fn signed_out() -> FixtureSetup {
    FixtureSetup::new(NOW)
}

/// A setup signed in with the family.
pub(crate) fn signed_in() -> FixtureSetup {
    FixtureSetup {
        account: Some(SyncState::Running),
        ..FixtureSetup::new(NOW)
    }
}
