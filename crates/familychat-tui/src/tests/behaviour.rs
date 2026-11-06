//! What the keys, the mouse, a paste and the session's news do.

use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use familychat_core::{commands::COMMANDS, rooms::RoomListFilter, signin::SigninStep};
use familychat_test_support::fixtures::{
    FIXTURE_LINK, FixtureAnswer, FixtureOutcome, FixtureProfile, FixtureSetup, rooms,
};

use super::{
    NOW, Screen, Variant, app_for, app_over, assert_snapshot, settled, signed_in, signed_out,
    styles_of, text_of,
};
use crate::{
    app::{App, Effect, InitialView, Prompt, Screen as AppScreen},
    rooms::Focus,
    signin::Page,
};

fn key(app: &mut App, code: KeyCode) {
    app.handle_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

fn ctrl(app: &mut App, ch: char) {
    app.handle_event(Event::Key(KeyEvent::new(
        KeyCode::Char(ch),
        KeyModifiers::CONTROL,
    )));
}

fn typed(app: &mut App, text: &str) {
    for ch in text.chars() {
        key(app, KeyCode::Char(ch));
    }
}

fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
    app.handle_event(Event::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }));
}

/// Ticks and draws until `done`.
fn until(app: &mut App, screen: &mut Screen, what: &str, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done(app) {
        assert!(Instant::now() < deadline, "never: {what}");
        app.tick();
        screen.draw(app);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn on_rooms(app: &App) -> bool {
    matches!(app.screen, AppScreen::Rooms(_))
}

fn signin_step(app: &App) -> Option<&SigninStep> {
    match &app.screen {
        AppScreen::Signin(signin) => Some(&signin.step),
        _ => None,
    }
}

fn row_of(text: &str, needle: &str) -> u16 {
    text.lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no {needle:?} on screen:\n{text}")) as u16
}

#[test]
fn a_tiny_terminal_says_it_is_too_small() {
    let (mut app, _startup) = app_for("welcome", Variant::Default);
    let text = text_of(&Screen::new(30, 4).draw(&mut app));
    assert!(text.starts_with("Family Chat needs a bigger ter"), "{text}");
}

#[test]
fn q_and_control_c_quit_and_other_keys_do_not() {
    let (mut app, _startup, _screen) = settled("welcome", 80, 24);
    key(&mut app, KeyCode::Char('x'));
    key(&mut app, KeyCode::Esc);
    ctrl(&mut app, 'q');
    let mut release = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
    release.kind = KeyEventKind::Release;
    app.handle_event(Event::Key(release));
    app.handle_event(Event::Resize(100, 30));
    assert!(!app.should_quit());
    key(&mut app, KeyCode::Char('q'));
    assert!(app.should_quit());

    let (mut app, _startup, _screen) = settled("password", 80, 24);
    typed(&mut app, "q");
    assert!(!app.should_quit(), "q is text in a form");
    ctrl(&mut app, 'c');
    assert!(app.should_quit());
}

#[test]
fn the_command_bar_shows_the_tables_keys_and_the_palette_its_names() {
    let (mut app, _startup, mut screen) = settled("welcome", 160, 48);
    let text = text_of(&screen.draw(&mut app));
    for id in [
        "SignInWithCode",
        "SignInWithPassword",
        "UseAnotherServer",
        "Quit",
    ] {
        let row = COMMANDS
            .iter()
            .find(|command| command.id == id)
            .expect("in the table");
        let bar: String = text.lines().rev().take(2).collect::<Vec<_>>().join("\n");
        assert!(
            bar.contains(&format!("{} ", row.tui_key)),
            "{id}'s key {} is not on the bar:\n{bar}",
            row.tui_key
        );
    }
    key(&mut app, KeyCode::Char(':'));
    let text = text_of(&screen.draw(&mut app));
    for name in [
        "Sign in with a code from your parent",
        "Sign in with your password",
        "Use another Matrix server",
        "Quit Family Chat",
    ] {
        assert!(text.contains(name), "{name} is not in the palette:\n{text}");
    }
    // Filtered, then run: "homeserver" is in UseAnotherServer's
    // description only.
    typed(&mut app, "homeserver");
    let buffer = screen.draw(&mut app);
    assert_snapshot(
        "palette-filtered-160x48",
        &format!("{}--- styles ---\n{}", text_of(&buffer), styles_of(&buffer)),
    );
    key(&mut app, KeyCode::Enter);
    let mut screen = Screen::new(80, 24);
    until(&mut app, &mut screen, "the other server's step", |app| {
        matches!(signin_step(app), Some(SigninStep::Server(_)))
    });
}

#[test]
fn a_pasted_code_is_used_only_after_y_on_the_confirmation() {
    let startup = signed_out().startup();
    let signin = startup.signin().clone();
    let mut app = app_over(startup, InitialView::Default, Variant::Default);
    let mut screen = Screen::new(80, 24);
    screen.settle(&mut app);
    // A paste on the welcome page is a sign-in link.
    app.handle_event(Event::Paste(FIXTURE_LINK.to_owned()));
    until(&mut app, &mut screen, "the confirmation", |app| {
        matches!(signin_step(app), Some(SigninStep::Confirm(_)))
    });
    let text = text_of(&screen.draw(&mut app));
    assert!(
        text.contains("Sign in as @ana:smith.safechat.family?"),
        "{text}"
    );
    assert!(
        !text.contains("fixture-login-token"),
        "the link never shows"
    );
    // Nothing but y redeems: Enter, a paste, other letters.
    key(&mut app, KeyCode::Enter);
    app.handle_event(Event::Paste("y".to_owned()));
    typed(&mut app, "x ");
    std::thread::sleep(Duration::from_millis(50));
    app.tick();
    assert_eq!(signin.redeemed(), 0);
    assert!(signin.answers().is_empty(), "{:?}", signin.answers());
    key(&mut app, KeyCode::Char('y'));
    until(&mut app, &mut screen, "the rooms", on_rooms);
    assert_eq!(signin.redeemed(), 1);
    assert_eq!(signin.answers(), [FixtureAnswer::Continue]);
}

#[test]
fn n_cancels_the_code_and_p_asks_for_the_password_instead() {
    let startup = signed_out().startup();
    let signin = startup.signin().clone();
    let mut app = app_over(startup, InitialView::Default, Variant::Default);
    let mut screen = Screen::new(80, 24);
    screen.settle(&mut app);
    key(&mut app, KeyCode::Char('c'));
    typed(&mut app, FIXTURE_LINK);
    let text = text_of(&screen.draw(&mut app));
    assert!(!text.contains("fixture-login-token"), "the field is masked");
    key(&mut app, KeyCode::Enter);
    until(&mut app, &mut screen, "the confirmation", |app| {
        matches!(signin_step(app), Some(SigninStep::Confirm(_)))
    });
    key(&mut app, KeyCode::Char('n'));
    until(
        &mut app,
        &mut screen,
        "the welcome page",
        |app| matches!(&app.screen, AppScreen::Signin(signin) if signin.page == Page::Welcome),
    );
    app.handle_event(Event::Paste(format!("  {FIXTURE_LINK}\n")));
    until(&mut app, &mut screen, "the confirmation again", |app| {
        matches!(signin_step(app), Some(SigninStep::Confirm(_)))
    });
    key(&mut app, KeyCode::Char('p'));
    until(&mut app, &mut screen, "the password form", |app| {
        matches!(signin_step(app), Some(SigninStep::Password(_)))
    });
    assert_eq!(signin.redeemed(), 0);
    assert_eq!(
        signin.answers(),
        [FixtureAnswer::Cancel, FixtureAnswer::UsePassword]
    );
}

#[test]
fn text_that_is_not_a_link_says_so_on_the_code_page() {
    let (mut app, _startup, mut screen) = settled("code-not-a-link", 80, 24);
    let text = text_of(&screen.draw(&mut app));
    assert!(
        text.contains("That isn't a Family Chat sign-in link."),
        "{text}"
    );
    assert!(matches!(&app.screen, AppScreen::Signin(signin) if signin.page == Page::Code));
}

#[test]
fn a_password_signs_in_and_the_rooms_follow() {
    let startup = signed_out().startup();
    let signin = startup.signin().clone();
    let mut app = app_over(startup, InitialView::Default, Variant::Default);
    let mut screen = Screen::new(80, 24);
    screen.settle(&mut app);
    key(&mut app, KeyCode::Char('p'));
    until(&mut app, &mut screen, "the password form", |app| {
        matches!(signin_step(app), Some(SigninStep::Password(_)))
    });
    typed(&mut app, "smith.safechat.family");
    key(&mut app, KeyCode::Tab);
    typed(&mut app, "ana");
    key(&mut app, KeyCode::Enter);
    typed(&mut app, "correct horse");
    let text = text_of(&screen.draw(&mut app));
    assert!(
        !text.contains("correct horse"),
        "the password is masked:\n{text}"
    );
    key(&mut app, KeyCode::Enter);
    until(&mut app, &mut screen, "the rooms", on_rooms);
    assert_eq!(
        signin.answers(),
        [FixtureAnswer::SubmitPassword {
            server: "smith.safechat.family".to_owned(),
            username: "ana".to_owned(),
            with_password: true,
        }]
    );
    assert!(!format!("{:?}", signin.events()).contains("correct horse"));
}

#[test]
fn a_rate_limit_holds_the_password_until_it_ends() {
    let startup = FixtureSetup {
        step: Some(familychat_test_support::fixtures::steps::rate_limited(
            Duration::from_secs(42),
        )),
        ..signed_out()
    }
    .startup();
    let signin = startup.signin().clone();
    let mut app = app_over(startup, InitialView::Password, Variant::Default);
    let mut screen = Screen::new(80, 24);
    screen.settle(&mut app);
    typed(&mut app, "secret");
    key(&mut app, KeyCode::Enter);
    std::thread::sleep(Duration::from_millis(30));
    app.tick();
    assert!(
        signin.answers().is_empty(),
        "nothing is sent while it waits"
    );
    let text = text_of(&screen.draw(&mut app));
    assert!(
        text.contains("You can sign in again in 42 seconds."),
        "{text}"
    );
}

#[test]
fn a_wrong_password_shows_the_sign_ins_sentence() {
    let startup = signed_out().startup();
    startup
        .signin()
        .set_password_outcome(FixtureOutcome::Problem(
            familychat_core::signin::SigninProblem::new("That username and password don't match."),
        ));
    let mut app = app_over(startup, InitialView::Password, Variant::Default);
    let mut screen = Screen::new(80, 24);
    screen.settle(&mut app);
    typed(&mut app, "smith.safechat.family");
    key(&mut app, KeyCode::Tab);
    typed(&mut app, "ana");
    key(&mut app, KeyCode::Tab);
    typed(&mut app, "wrong");
    key(&mut app, KeyCode::Enter);
    until(
        &mut app,
        &mut screen,
        "the problem",
        |app| matches!(signin_step(app), Some(SigninStep::Password(form)) if form.problem.is_some()),
    );
    let text = text_of(&screen.draw(&mut app));
    assert!(
        text.contains("That username and password don't match."),
        "{text}"
    );
    assert!(
        text.contains("smith.safechat.family"),
        "the fields are kept"
    );
}

#[test]
fn a_locked_keychain_is_tried_again_and_a_lost_key_starts_over_when_asked() {
    let startup = FixtureSetup {
        profile: FixtureProfile::KeychainLocked,
        ..signed_out()
    }
    .startup();
    let mut app = app_over(startup.clone(), InitialView::Default, Variant::Default);
    let mut screen = Screen::new(80, 24);
    let text = text_of(&screen.settle(&mut app));
    assert!(text.contains("keychain is locked"), "{text}");
    // The fixture stays locked; t asks again.
    key(&mut app, KeyCode::Char('t'));
    screen.settle(&mut app);
    assert_eq!(startup.opens(), 2);

    let startup = FixtureSetup {
        profile: FixtureProfile::LostKey,
        ..signed_out()
    }
    .startup();
    let mut app = app_over(startup.clone(), InitialView::Default, Variant::Default);
    screen.settle(&mut app);
    key(&mut app, KeyCode::Char('s'));
    assert!(matches!(app.prompt, Some(Prompt::StartOver)));
    key(&mut app, KeyCode::Char('n'));
    assert!(!startup.started_over(), "n keeps the data");
    key(&mut app, KeyCode::Char('s'));
    key(&mut app, KeyCode::Char('y'));
    let text = text_of(&screen.settle(&mut app));
    assert!(startup.started_over());
    assert!(text.contains("WELCOME"), "{text}");
    assert!(
        app.status()
            .is_some_and(|status| status.contains("unreadable-")),
        "{:?}",
        app.status()
    );
}

#[test]
fn the_keys_move_through_the_rooms_and_their_filters() {
    let startup = signed_in().startup();
    let session = startup.session().clone();
    let mut app = app_over(startup, InitialView::Default, Variant::Default);
    let mut screen = Screen::new(80, 24);
    screen.settle(&mut app);
    // Down, then Enter opens the family room (the second row).
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    let text = text_of(&screen.settle(&mut app));
    assert!(text.contains("Smith family  Encrypted"), "{text}");
    assert!(text.contains("See you then"), "{text}");
    assert_eq!(
        session.record().timelines.last().map(|room| room.as_str()),
        Some(rooms::FAMILY_ROOM)
    );
    // Esc goes back to the list.
    key(&mut app, KeyCode::Esc);
    assert!(
        app.rooms_ref()
            .is_some_and(|rooms| rooms.focus == Focus::List)
    );
    // n opens the next unread room: Grandad.
    key(&mut app, KeyCode::Char('n'));
    let text = text_of(&screen.settle(&mut app));
    assert!(text.contains("Grandad"), "{text}");
    // 2, 3, 4 and 1 are the filters.
    for (ch, filter) in [
        ('2', RoomListFilter::Unread),
        ('3', RoomListFilter::People),
        ('4', RoomListFilter::Invites),
        ('1', RoomListFilter::All),
    ] {
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Char(ch));
        screen.settle(&mut app);
        assert_eq!(session.record().filters.last(), Some(&filter));
    }
}

#[test]
fn side_by_side_the_room_follows_the_cursor() {
    let (mut app, startup, mut screen) = settled("rooms", 160, 48);
    key(&mut app, KeyCode::Down);
    let text = text_of(&screen.settle(&mut app));
    assert!(
        text.contains("Plans, photos and who's picking up whom"),
        "{text}"
    );
    assert_eq!(
        startup
            .session()
            .record()
            .timelines
            .last()
            .map(|room| room.as_str()),
        Some(rooms::FAMILY_ROOM)
    );
    // Narrower than 120 columns: the list alone.
    let mut narrow = Screen::new(119, 30);
    key(&mut app, KeyCode::Esc);
    let text = text_of(&narrow.settle(&mut app));
    assert!(!text.contains("Plans, photos"), "{text}");
}

#[test]
fn a_click_opens_a_room_and_the_wheel_scrolls() {
    let (mut app, _startup, mut screen) = settled("rooms", 80, 24);
    let text = text_of(&screen.draw(&mut app));
    let row = row_of(&text, "Smith family");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 10, row);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 10, row);
    let text = text_of(&screen.settle(&mut app));
    assert!(text.contains("See you then"), "{text}");
    let before = app
        .rooms_ref()
        .and_then(|rooms| rooms.open.as_ref())
        .map(|open| open.from_bottom);
    mouse(&mut app, MouseEventKind::ScrollUp, 10, 10);
    screen.draw(&mut app);
    let after = app
        .rooms_ref()
        .and_then(|rooms| rooms.open.as_ref())
        .map(|open| open.from_bottom);
    assert!(after > before, "{before:?} {after:?}");
}

#[test]
fn a_drag_selects_inside_the_timeline_and_copies_on_release() {
    let (mut app, _startup, mut screen) = settled("room", 80, 24);
    let text = text_of(&screen.draw(&mut app));
    let row = row_of(&text, "can someone bring my water bottle");
    let column = text
        .lines()
        .nth(usize::from(row))
        .and_then(|line| line.find("can"))
        .unwrap_or(2) as u16;
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        column,
        row,
    );
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        column + 12,
        row,
    );
    let buffer = screen.draw(&mut app);
    assert_snapshot(
        "selection-80x24",
        &format!("{}--- styles ---\n{}", text_of(&buffer), styles_of(&buffer)),
    );
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        column + 12,
        row,
    );
    assert_eq!(
        app.take_effects(),
        [Effect::Copy("can someone b".to_owned())]
    );
    assert_eq!(app.status(), Some("Copied 13 characters"));
    // Shift leaves the mouse to the terminal.
    app.handle_event(Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::SHIFT,
    }));
    assert!(app.take_effects().is_empty());
}

#[test]
fn links_ask_with_the_whole_address_and_open_only_on_y() {
    let (mut app, _startup, mut screen) = settled("link-prompt", 80, 24);
    let Some(Prompt::Link(prompt)) = app.prompt.clone() else {
        panic!("the link asks");
    };
    assert_eq!(prompt.target, "https://cottages.example/booking/4471");
    let text = text_of(&screen.draw(&mut app));
    assert!(
        text.contains("https://cottages.example/booking/4471"),
        "{text}"
    );
    key(&mut app, KeyCode::Enter);
    assert!(app.take_effects().is_empty(), "Enter doesn't open it");
    key(&mut app, KeyCode::Char('c'));
    assert_eq!(
        app.take_effects(),
        [Effect::Copy(
            "https://cottages.example/booking/4471".to_owned()
        )]
    );
    key(&mut app, KeyCode::Char('l'));
    typed(&mut app, "1");
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Char('y'));
    assert_eq!(
        app.take_effects(),
        [Effect::Open(
            "https://cottages.example/booking/4471".to_owned()
        )]
    );

    let (app, _startup, _screen) = settled("link-prompt-mismatched", 80, 24);
    let Some(Prompt::Link(prompt)) = app.prompt.clone() else {
        panic!("the link asks");
    };
    assert!(prompt.is_mismatched());
}

#[test]
fn matrix_links_navigate_in_the_app() {
    let (mut app, _startup, mut screen) = settled("room-rich", 80, 24);
    // The fourth link is #family, an alias the list doesn't name.
    key(&mut app, KeyCode::Char('l'));
    typed(&mut app, "4");
    key(&mut app, KeyCode::Enter);
    assert!(app.prompt.is_none(), "a Matrix link never asks");
    assert!(
        app.status()
            .is_some_and(|status| status.contains("isn't one of your rooms")),
        "{:?}",
        app.status()
    );
    // In the family room, the mention of Ana names her.
    let (mut app, _startup, _screen) = settled("room", 80, 24);
    let refs = app
        .rooms_ref()
        .and_then(|rooms| rooms.open.as_ref())
        .and_then(|open| open.layout.as_ref())
        .map(|layout| layout.refs.len())
        .unwrap_or(0);
    assert!(refs >= 1);
    key(&mut app, KeyCode::Char('l'));
    typed(&mut app, "1");
    key(&mut app, KeyCode::Enter);
    screen.draw(&mut app);
    assert_eq!(app.status(), Some("That's @ana:smith.safechat.family."));
}

#[test]
fn files_are_saved_where_the_person_types_and_replacing_asks() {
    let folder = std::env::temp_dir().join(format!("familychat-tui-save-{}", std::process::id()));
    std::fs::create_dir_all(&folder).expect("a folder");
    let (mut app, startup, mut screen) = settled("save-prompt", 80, 24);
    let Some(Prompt::Save { input, .. }) = app.prompt.clone() else {
        panic!("the save question");
    };
    assert!(input.ends_with("sunset.jpg"), "{input}");
    ctrl(&mut app, 'u');
    typed(&mut app, &folder.display().to_string());
    key(&mut app, KeyCode::Enter);
    screen.settle(&mut app);
    let saved = startup.session().record().saved;
    assert_eq!(
        saved,
        [("sunset.jpg".to_owned(), folder.join("sunset.jpg"))]
    );
    assert_eq!(app.status(), Some("Saved sunset.jpg."));

    // A file already there: asked first.
    std::fs::write(folder.join("sunset.jpg"), b"x").expect("a file");
    key(&mut app, KeyCode::Char('s'));
    let Some(Prompt::Save { .. }) = app.prompt else {
        panic!("s saves the selected file");
    };
    ctrl(&mut app, 'u');
    typed(&mut app, &folder.join("sunset.jpg").display().to_string());
    key(&mut app, KeyCode::Enter);
    assert!(matches!(app.prompt, Some(Prompt::Replace { .. })));
    key(&mut app, KeyCode::Char('n'));
    assert!(
        matches!(app.prompt, Some(Prompt::Save { .. })),
        "n goes back to the path"
    );
    key(&mut app, KeyCode::Esc);
    assert_eq!(startup.session().record().saved.len(), 1);
    let _ = std::fs::remove_dir_all(&folder);
}

#[test]
fn new_messages_show_in_the_status_line_and_ring_the_bell_when_asked() {
    let (mut app, startup, mut screen) = settled("rooms", 80, 24);
    key(&mut app, KeyCode::Char('b'));
    assert!(app.settings().bell);
    let session = startup.session().clone();
    let mut rooms = session.rooms();
    for room in rooms
        .iter_mut()
        .filter(|room| room.room_id.as_str() == rooms::MUM_ROOM)
    {
        room.unread.notifications += 1;
        room.unread.messages += 1;
    }
    session.set_rooms(rooms);
    until(&mut app, &mut screen, "the notice", |app| {
        app.status()
            .is_some_and(|status| status.starts_with("New message in Mum"))
    });
    assert!(app.take_effects().contains(&Effect::Bell));
    let buffer = screen.draw(&mut app);
    assert_snapshot(
        "notification-80x24",
        &format!("{}--- styles ---\n{}", text_of(&buffer), styles_of(&buffer)),
    );
}

#[test]
fn the_top_of_a_timeline_asks_for_older_messages() {
    let (mut app, startup, mut screen) = settled("room", 80, 24);
    for _ in 0..20 {
        key(&mut app, KeyCode::PageUp);
        screen.draw(&mut app);
    }
    screen.settle(&mut app);
    assert!(startup.session().record().paginations >= 1);
}

#[test]
fn the_sync_banner_offers_try_again_where_it_helps() {
    let (mut app, startup, mut screen) = settled("signed-in-offline", 80, 24);
    key(&mut app, KeyCode::Char('t'));
    screen.draw(&mut app);
    assert_eq!(startup.session().retried().len(), 1);
    // Suspended is not an error, and nothing retries it from here.
    let (mut app, startup, mut screen) = settled("signed-in-suspended", 80, 24);
    let text = text_of(&screen.draw(&mut app));
    assert!(text.contains("Chat is paused"), "{text}");
    assert!(!text.to_lowercase().contains("error"), "{text}");
    key(&mut app, KeyCode::Char('t'));
    assert!(startup.session().retried().is_empty());
}

#[test]
fn pastes_go_into_fields_only() {
    let (mut app, _startup, _screen) = settled("room", 80, 24);
    app.handle_event(Event::Paste("q".to_owned()));
    assert!(!app.should_quit(), "a paste never runs as commands");
    assert_eq!(app.status(), Some("Pasting works in text fields only"));
}

#[test]
fn the_large_fixture_lays_out_once_per_change() {
    let (mut app, _startup, mut screen) = settled("room-large", 160, 48);
    let generation = |app: &App| {
        app.rooms_ref()
            .and_then(|rooms| rooms.open.as_ref())
            .and_then(|open| open.layout.as_ref().map(|layout| layout.key))
    };
    let before = generation(&app);
    let started = Instant::now();
    for _ in 0..50 {
        screen.draw(&mut app);
    }
    assert_eq!(generation(&app), before, "the layout is kept");
    // Fifty frames of a 10,000-event room, laid out once: well under a
    // frame each even in a debug build.
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    let _ = NOW;
}

#[test]
fn another_servers_browser_sign_in_opens_only_after_y_and_prints_no_address_query() {
    use familychat_test_support::fixtures::{FIXTURE_BROWSER_URL, FIXTURE_ISSUER};

    let startup = signed_out().startup();
    let signin = startup.signin().clone();
    let mut app = app_over(startup, InitialView::OtherServer, Variant::Default);
    let mut screen = Screen::new(80, 24);
    until(&mut app, &mut screen, "the server step", |app| {
        matches!(signin_step(app), Some(SigninStep::Server(_)))
    });
    typed(&mut app, "example.org");
    key(&mut app, KeyCode::Enter);
    until(
        &mut app,
        &mut screen,
        "the server's methods",
        |app| matches!(signin_step(app), Some(SigninStep::Password(form)) if form.methods.is_some()),
    );
    // Letters on a fixed server go to the username, not to commands.
    typed(&mut app, "y");
    ctrl(&mut app, 'b');
    until(&mut app, &mut screen, "the browser question", |app| {
        matches!(signin_step(app), Some(SigninStep::ConfirmBrowser(_)))
    });
    let text = text_of(&screen.draw(&mut app));
    assert!(text.contains(FIXTURE_ISSUER), "{text}");
    assert!(app.take_effects().is_empty(), "nothing opens before y");
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Char('x'));
    assert!(matches!(
        signin_step(&app),
        Some(SigninStep::ConfirmBrowser(_))
    ));
    key(&mut app, KeyCode::Char('y'));
    until(&mut app, &mut screen, "the browser step", |app| {
        matches!(signin_step(app), Some(SigninStep::Browser(_)))
    });
    let text = text_of(&screen.draw(&mut app));
    assert!(
        !text.contains("state="),
        "the address's query is never drawn:\n{text}"
    );
    assert_eq!(
        app.take_effects(),
        [Effect::Open(FIXTURE_BROWSER_URL.to_owned())]
    );
    // Drawing again opens nothing more; `o` opens it again on request.
    screen.draw(&mut app);
    app.tick();
    assert!(app.take_effects().is_empty());
    key(&mut app, KeyCode::Char('o'));
    assert_eq!(
        app.take_effects(),
        [Effect::Open(FIXTURE_BROWSER_URL.to_owned())]
    );
    key(&mut app, KeyCode::Esc);
    until(&mut app, &mut screen, "back to the methods", |app| {
        matches!(signin_step(app), Some(SigninStep::Password(_)))
    });
    assert!(signin.answers().contains(&FixtureAnswer::UseBrowser));
    assert_eq!(
        signin
            .answers()
            .iter()
            .filter(|answer| **answer == FixtureAnswer::Continue)
            .count(),
        1
    );
}
