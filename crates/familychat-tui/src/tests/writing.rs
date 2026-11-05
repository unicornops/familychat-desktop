//! Writing over the fixture session (ADR 0008, "Writing"): the composer's
//! keys and pastes, mentions, replies, edits, reactions, deletions, files
//! by path with Tab completion and the upload limit, invitations,
//! leaving, the paused composer, drafts, typing notices and the external
//! editor's switch. Every change is checked in the session's record of
//! room requests, which only `familychat-actions` makes.

use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode};
use familychat_core::{
    messaging::{Draft, RoomRequest},
    timeline::MessageBody,
};
use familychat_test_support::fixtures::rooms;

use super::{
    Screen, Variant, app_for,
    behaviour::{ctrl, key, typed, until},
    settled, text_of,
};
use crate::{
    app::{App, Prompt},
    rooms::Focus,
};

fn composer_text(app: &App) -> Option<String> {
    app.rooms_ref()
        .and_then(|rooms| rooms.open.as_ref())
        .and_then(|open| open.composer.as_ref())
        .map(crate::composer::Composer::text)
}

fn focus(app: &App) -> Option<Focus> {
    app.rooms_ref().map(|rooms| rooms.focus)
}

/// The requests that reached the session so far, other than receipts and
/// typing notices.
fn writes(startup: &familychat_test_support::fixtures::FixtureStartup) -> Vec<RoomRequest> {
    startup
        .session()
        .record()
        .writes()
        .into_iter()
        .cloned()
        .collect()
}

#[test]
fn enter_sends_markdown_and_the_new_line_keys_do_not() {
    let (mut app, startup, mut screen) = settled("room", 80, 24);
    assert_eq!(
        focus(&app),
        Some(Focus::Composer),
        "the composer has the keys"
    );
    typed(&mut app, "Hello **there**");
    ctrl(&mut app, 'j');
    typed(&mut app, "q and n are text here");
    assert!(!app.should_quit());
    assert_eq!(
        composer_text(&app).as_deref(),
        Some("Hello **there**\nq and n are text here")
    );
    key(&mut app, KeyCode::Enter);
    until(&mut app, &mut screen, "the message is sent", |_| {
        writes(&startup)
            .iter()
            .any(|request| matches!(request, RoomRequest::Send { .. }))
    });
    let sent = writes(&startup);
    let Some(RoomRequest::Send { message, reply_to }) = sent.first() else {
        panic!("a send: {sent:?}");
    };
    assert!(reply_to.is_none());
    assert_eq!(
        message.body.as_str(),
        "Hello **there**\nq and n are text here"
    );
    let html = message.html.as_ref().map(MessageBody::as_str).unwrap_or("");
    assert!(html.contains("<strong>there</strong>"), "{html}");
    assert_eq!(composer_text(&app).as_deref(), Some(""), "the field clears");
    // The room heard about the typing, and that it stopped. The notices
    // go out on threads of their own, so wait for the last one.
    let typing = || -> Vec<bool> {
        startup
            .session()
            .record()
            .requests
            .iter()
            .filter_map(|(_, request)| match request {
                RoomRequest::Typing(typing) => Some(*typing),
                _ => None,
            })
            .collect()
    };
    until(&mut app, &mut screen, "the typing notice stops", |_| {
        typing().last() == Some(&false)
    });
    assert_eq!(typing().first(), Some(&true));
}

#[test]
fn esc_moves_from_the_composer_to_the_rooms_commands_and_back_to_the_list() {
    let (mut app, _startup, _screen) = settled("room", 80, 24);
    key(&mut app, KeyCode::Esc);
    assert_eq!(focus(&app), Some(Focus::Room));
    // i, the table's FocusComposer, goes back.
    key(&mut app, KeyCode::Char('i'));
    assert_eq!(focus(&app), Some(Focus::Composer));
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Esc);
    assert_eq!(focus(&app), Some(Focus::List));
}

#[test]
fn mentions_complete_and_go_with_the_message() {
    let (mut app, startup, mut screen) = settled("room", 80, 24);
    typed(&mut app, "Thanks @gr");
    let text = text_of(&screen.settle(&mut app));
    assert!(text.contains("@grandad:smith.safechat.family"), "{text}");
    key(&mut app, KeyCode::Tab);
    typed(&mut app, "!");
    assert_eq!(composer_text(&app).as_deref(), Some("Thanks Grandad !"));
    key(&mut app, KeyCode::Enter);
    until(&mut app, &mut screen, "the message is sent", |_| {
        !writes(&startup).is_empty()
    });
    let Some(RoomRequest::Send { message, .. }) = writes(&startup).into_iter().next() else {
        panic!("a send");
    };
    assert_eq!(message.mentions, [rooms::user("grandad")]);
    assert!(
        message
            .html
            .as_ref()
            .is_some_and(|html| html.as_str().contains("matrix.to")),
        "the mention is a link"
    );
}

#[test]
fn reply_react_edit_and_delete_go_through_the_actions() {
    let (mut app, startup, mut screen) = settled("room", 80, 24);
    // Mum's "Dinner at 7", the newest message from someone else.
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('['));
    key(&mut app, KeyCode::Char('['));
    let status = app.status().unwrap_or_default().to_owned();
    assert!(status.contains("from Mum"), "{status}");
    key(&mut app, KeyCode::Char('R'));
    assert_eq!(focus(&app), Some(Focus::Composer));
    let text = text_of(&screen.settle(&mut app));
    assert!(text.contains("Replying to Mum"), "{text}");
    typed(&mut app, "Will do");
    key(&mut app, KeyCode::Enter);
    until(&mut app, &mut screen, "the reply is sent", |_| {
        !writes(&startup).is_empty()
    });
    let Some(RoomRequest::Send {
        reply_to: Some(answered),
        ..
    }) = writes(&startup).into_iter().next()
    else {
        panic!("a reply: {:?}", writes(&startup));
    };

    // + and a number react to the selected message.
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('['));
    key(&mut app, KeyCode::Char('['));
    key(&mut app, KeyCode::Char('['));
    key(&mut app, KeyCode::Char('+'));
    assert!(matches!(app.prompt, Some(Prompt::React(_))));
    key(&mut app, KeyCode::Char('1'));
    until(&mut app, &mut screen, "the reaction is sent", |_| {
        writes(&startup)
            .iter()
            .any(|request| matches!(request, RoomRequest::ToggleReaction { .. }))
    });
    let reaction = writes(&startup)
        .into_iter()
        .find_map(|request| match request {
            RoomRequest::ToggleReaction { target, key } => Some((target, key)),
            _ => None,
        });
    let (target, emoji) = reaction.expect("a reaction");
    assert_eq!(emoji, "👍");
    assert!(target.event_id.is_some());
    let _ = answered;

    // E edits the newest of the account's messages it can.
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('E'));
    let editing = composer_text(&app).unwrap_or_default();
    assert!(!editing.is_empty(), "the old text is in the field");
    ctrl(&mut app, 'j');
    typed(&mut app, "(changed)");
    key(&mut app, KeyCode::Enter);
    until(&mut app, &mut screen, "the edit is sent", |_| {
        writes(&startup)
            .iter()
            .any(|request| matches!(request, RoomRequest::Edit { .. }))
    });

    // D asks first; only y deletes, and only one of the account's own.
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('D'));
    assert!(app.prompt.is_none(), "nothing selected: nothing asked");
    let mut own = false;
    for _ in 0..12 {
        key(&mut app, KeyCode::Char('['));
        if app.selected_choice().is_some_and(|choice| choice.own) {
            own = true;
            break;
        }
    }
    assert!(own, "one of Ana's messages is in the room");
    key(&mut app, KeyCode::Char('D'));
    assert!(matches!(app.prompt, Some(Prompt::Delete { .. })));
    let text = text_of(&screen.draw(&mut app));
    assert!(text.contains("Delete this message?"), "{text}");
    key(&mut app, KeyCode::Enter);
    assert!(
        matches!(app.prompt, Some(Prompt::Delete { .. })),
        "Enter doesn't answer"
    );
    key(&mut app, KeyCode::Char('y'));
    until(&mut app, &mut screen, "the message is deleted", |_| {
        writes(&startup)
            .iter()
            .any(|request| matches!(request, RoomRequest::Redact { .. }))
    });
}

#[test]
fn files_by_path_complete_with_tab_and_are_checked_before_they_wait() {
    let folder = std::env::temp_dir().join(format!(
        "familychat-tui-attach-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&folder).expect("a folder");
    std::fs::write(folder.join("holiday-plans.txt"), b"the plans").expect("a file");
    let (mut app, startup, mut screen) = settled("room", 80, 24);
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('A'));
    assert!(matches!(app.prompt, Some(Prompt::Attach { .. })));
    ctrl(&mut app, 'u');
    typed(
        &mut app,
        &format!("{}{}hol", folder.display(), std::path::MAIN_SEPARATOR),
    );
    key(&mut app, KeyCode::Tab);
    let Some(Prompt::Attach { input, .. }) = app.prompt.clone() else {
        panic!("still asking");
    };
    assert!(input.ends_with("holiday-plans.txt"), "{input}");
    key(&mut app, KeyCode::Enter);
    assert_eq!(focus(&app), Some(Focus::Composer));
    let text = text_of(&screen.settle(&mut app));
    assert!(text.contains("[file] holiday-plans.txt"), "{text}");
    assert!(
        writes(&startup).is_empty(),
        "nothing goes until the message does"
    );
    assert!(startup.session().record().media_limit_asks >= 1);
    key(&mut app, KeyCode::Enter);
    until(&mut app, &mut screen, "the file is sent", |_| {
        writes(&startup)
            .iter()
            .any(|request| matches!(request, RoomRequest::SendFile { .. }))
    });

    // Over the server's limit: refused before it is read, nothing waits.
    let (mut app, startup, mut screen) = settled("composer-file-too-large", 80, 24);
    let text = text_of(&screen.settle(&mut app));
    assert!(text.contains("this server takes files up to"), "{text}");
    assert!(!text.contains("[file]"), "{text}");
    key(&mut app, KeyCode::Enter);
    screen.settle(&mut app);
    assert!(writes(&startup).is_empty());

    // A path that isn't a file says so and keeps asking.
    let (mut app, _startup, _screen) = settled("attach-prompt", 80, 24);
    typed(&mut app, "no-such-file.txt");
    key(&mut app, KeyCode::Enter);
    assert!(matches!(app.prompt, Some(Prompt::Attach { .. })));
    assert!(
        app.status()
            .is_some_and(|status| status.contains("no file at"))
    );
    let _ = std::fs::remove_dir_all(&folder);
}

#[test]
fn the_paused_composer_is_read_only_and_says_why() {
    let (mut app, startup, mut screen) = settled("composer-paused", 80, 24);
    assert_eq!(focus(&app), Some(Focus::Room), "nothing to type into");
    let text = text_of(&screen.draw(&mut app));
    assert!(text.contains("Chat is paused"), "{text}");
    key(&mut app, KeyCode::Char('i'));
    assert_eq!(focus(&app), Some(Focus::Room));
    key(&mut app, KeyCode::Char('R'));
    key(&mut app, KeyCode::Char('+'));
    assert!(app.prompt.is_none());
    // The palette doesn't offer writing either.
    let names: Vec<&str> = app.commands().iter().map(|entry| entry.label).collect();
    assert!(
        !names.contains(&"Reply") && !names.contains(&"Write"),
        "{names:?}"
    );
    screen.settle(&mut app);
    assert!(writes(&startup).is_empty(), "nothing is sent while paused");
}

#[test]
fn an_invitation_is_answered_and_the_room_opens_to_write_in() {
    let (mut app, startup, mut screen) = settled("room-invite", 80, 24);
    assert!(composer_text(&app).is_none(), "no composer before joining");
    key(&mut app, KeyCode::Char('a'));
    until(&mut app, &mut screen, "the invitation is accepted", |_| {
        !startup.session().record().invites.is_empty()
    });
    assert_eq!(
        startup.session().record().invites,
        [(
            familychat_core::OwnedRoomId::try_from(rooms::INVITE_ROOM).expect("an id"),
            true
        )]
    );
    until(&mut app, &mut screen, "the room is joined", |app| {
        composer_text(app).is_some()
    });

    let (mut app, startup, mut screen) = settled("room-invite", 80, 24);
    key(&mut app, KeyCode::Char('x'));
    until(&mut app, &mut screen, "the invitation is declined", |_| {
        startup.session().record().invites.len() == 1
    });
    assert!(!startup.session().record().invites[0].1);
}

#[test]
fn leaving_asks_first() {
    let (mut app, startup, mut screen) = settled("leave-prompt", 80, 24);
    let text = text_of(&screen.draw(&mut app));
    assert!(text.contains("Leave Smith family?"), "{text}");
    key(&mut app, KeyCode::Char('n'));
    assert!(app.prompt.is_none());
    key(&mut app, KeyCode::Char('L'));
    key(&mut app, KeyCode::Char('y'));
    until(&mut app, &mut screen, "the room is left", |_| {
        writes(&startup).contains(&RoomRequest::Leave)
    });
}

#[test]
fn a_kept_draft_comes_back_and_typing_keeps_it_again() {
    let (mut app, startup) = app_for("rooms", Variant::Default);
    let account = familychat_core::session::SessionApi::accounts(startup.session().as_ref())
        .borrow()[0]
        .key
        .clone();
    startup.session().set_draft(
        &account,
        rooms::FAMILY_ROOM,
        Draft {
            text: MessageBody::new("half a thought"),
            mentions: Vec::new(),
            reply_to: None,
            editing: None,
        },
    );
    let mut screen = Screen::new(80, 24);
    screen.settle(&mut app);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    screen.settle(&mut app);
    assert_eq!(composer_text(&app).as_deref(), Some("half a thought"));
    typed(&mut app, ", finished");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !startup.session().record().drafts.iter().any(|(_, draft)| {
        draft
            .as_ref()
            .is_some_and(|draft| draft.text.as_str() == "half a thought, finished")
    }) {
        assert!(Instant::now() < deadline, "the draft was never kept");
        app.tick();
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_external_editor_is_off_unless_asked_for_and_its_text_is_filtered() {
    let (mut app, _startup, _screen) = settled("room", 80, 24);
    app.handle_event(Event::Key(crossterm::event::KeyEvent::new(
        KeyCode::Char('7'),
        crossterm::event::KeyModifiers::CONTROL,
    )));
    assert!(app.take_effects().is_empty(), "no editor without --editor");
    assert!(
        app.status()
            .is_some_and(|status| status.contains("external editor is off"))
    );
    app.external_edit_done(Ok("From the editor\x1b[2J\x1b]0;pwned\x07".to_owned()));
    assert_eq!(composer_text(&app).as_deref(), Some("From the editor"));
    assert_eq!(focus(&app), Some(Focus::Composer));
    app.external_edit_done(Err(anyhow::anyhow!("vi exited with 1")));
    assert_eq!(composer_text(&app).as_deref(), Some("From the editor"));
}

#[test]
fn a_message_the_encryption_stopped_goes_anyway_on_s() {
    let (mut app, startup, mut screen) = settled("send-problem", 80, 24);
    key(&mut app, KeyCode::Char('S'));
    until(&mut app, &mut screen, "sent anyway", |_| {
        writes(&startup)
            .iter()
            .any(|request| matches!(request, RoomRequest::SendAnyway(_)))
    });
}

#[test]
fn an_identity_warning_is_answered_with_w() {
    let (mut app, startup, mut screen) = settled("identity-warnings", 80, 24);
    key(&mut app, KeyCode::Char('w'));
    until(&mut app, &mut screen, "the warning is answered", |app| {
        app.rooms_ref()
            .and_then(|rooms| rooms.open.as_ref())
            .is_some_and(|open| open.warnings.len() == 1)
    });
    assert!(startup.session().encryption_asks().contains(
        &familychat_test_support::fixtures::encryption::EncryptionAsk::Acknowledge(rooms::user(
            "grandad"
        ))
    ));
}
