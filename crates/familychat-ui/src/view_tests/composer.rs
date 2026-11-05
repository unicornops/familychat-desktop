//! The composer and the message actions (PLAN.md, section 13.7; ADR 0012)
//! over the fixture family: Enter sends and Shift+Enter starts a line,
//! mentions come from the members, replies and edits show and cancel,
//! files wait as cards (and one over the server's limit is refused before
//! anything is asked of the room), the composer is read-only while the
//! account is paused, reactions, deleting asks first, drafts come back,
//! and the room hears typing and read receipts. Everything is checked in
//! what the fixture session was asked for.

use std::{sync::Arc, time::Duration};

use familychat_actions::{ComposerState, DRAFT_SAVE_DELAY, FileSource};
use familychat_core::{
    OwnedUserId,
    messaging::{Draft, RoomRequest},
    session::SessionApi,
    sync::SyncState,
    timeline::MessageBody,
};
use familychat_test_support::fixtures::{FIXTURE_UPLOAD_LIMIT, rooms::FAMILY_ROOM};
use gpui::{Entity, TestAppContext, VisualTestContext};

use super::rooms::{open_rooms, rooms_view, timeline};
use super::{click, keys, wait_until};
use crate::rooms::{ComposerView, Target};
use crate::{RoomsLaunch, ShellView};

fn family() -> RoomsLaunch {
    RoomsLaunch {
        room: Some(FAMILY_ROOM.to_owned()),
        ..RoomsLaunch::default()
    }
}

/// The open room's composer, once its timeline has loaded.
fn composer(view: &Entity<ShellView>, cx: &mut VisualTestContext) -> Entity<ComposerView> {
    let _ = timeline(view, cx);
    let rooms = rooms_view(view, cx);
    rooms.read_with(cx, |rooms, _| {
        rooms.composer().cloned().expect("the room has a composer")
    })
}

/// The room requests that write (not receipts or typing).
fn writes(session: &familychat_test_support::fixtures::FixtureSession) -> Vec<RoomRequest> {
    session.record().writes().into_iter().cloned().collect()
}

#[gpui::test]
fn enter_sends_and_shift_enter_starts_a_new_line(cx: &mut TestAppContext) {
    let (view, cx, session) = open_rooms(cx, family());
    let composer = composer(&view, cx);
    keys("secondary-l", cx);
    cx.simulate_input("See you");
    keys("shift-enter", cx);
    cx.simulate_input("at **7**");
    assert_eq!(
        composer.read_with(cx, |composer, cx| composer.text(cx)),
        "See you\nat **7**"
    );
    keys("enter", cx);
    wait_until(cx, "the message sent", |_| !writes(&session).is_empty());
    let writes = writes(&session);
    let [
        RoomRequest::Send {
            message,
            reply_to: None,
        },
    ] = writes.as_slice()
    else {
        panic!("{writes:?}");
    };
    assert_eq!(message.body.as_str(), "See you\nat **7**");
    assert!(
        message
            .html
            .as_ref()
            .is_some_and(|html| html.as_str().contains("<strong>7</strong>"))
    );
    assert!(composer.read_with(cx, |composer, cx| composer.text(cx).is_empty()));
    // The room heard the typing start, and stop when it went.
    let requests: Vec<_> = session
        .record()
        .requests
        .into_iter()
        .map(|(_, request)| request)
        .collect();
    assert!(
        requests.contains(&RoomRequest::Typing(true)),
        "{requests:?}"
    );
    assert!(
        requests.contains(&RoomRequest::Typing(false)),
        "{requests:?}"
    );
}

#[gpui::test]
fn an_empty_composer_sends_nothing(cx: &mut TestAppContext) {
    let (view, cx, session) = open_rooms(cx, family());
    let _composer = composer(&view, cx);
    keys("secondary-l", cx);
    cx.simulate_input("   ");
    keys("enter", cx);
    click("button-composer-send", cx);
    assert!(writes(&session).is_empty());
}

#[gpui::test]
fn mentions_come_from_the_rooms_members(cx: &mut TestAppContext) {
    let (view, cx, session) = open_rooms(cx, family());
    let composer = composer(&view, cx);
    keys("secondary-l", cx);
    cx.simulate_input("thanks @");
    wait_until(cx, "the members", |cx| {
        composer.read_with(cx, |composer, _| !composer.suggested().is_empty())
    });
    let all = composer.read_with(cx, |composer, _| composer.suggested());
    assert!(
        !all.iter().any(|name| name == "Ana"),
        "the account itself: {all:?}"
    );
    cx.simulate_input("gr");
    assert_eq!(
        composer.read_with(cx, |composer, _| composer.suggested()),
        ["Grandad"]
    );
    keys("enter", cx);
    assert_eq!(
        composer.read_with(cx, |composer, cx| composer.text(cx)),
        "thanks Grandad "
    );
    assert!(
        writes(&session).is_empty(),
        "picking a mention sends nothing"
    );
    cx.simulate_input("for the lift");
    keys("enter", cx);
    wait_until(cx, "the message sent", |_| !writes(&session).is_empty());
    let writes = writes(&session);
    let [RoomRequest::Send { message, .. }] = writes.as_slice() else {
        panic!("{writes:?}");
    };
    let grandad = OwnedUserId::try_from("@grandad:smith.safechat.family").expect("an id");
    assert_eq!(message.mentions, [grandad]);
    assert!(
        message
            .html
            .as_ref()
            .is_some_and(|html| html.as_str().contains("matrix.to/#/@grandad"))
    );
}

#[gpui::test]
fn escape_closes_the_mentions_then_the_reply(cx: &mut TestAppContext) {
    let (view, cx, session) = open_rooms(cx, family());
    let composer = composer(&view, cx);
    keys("secondary-shift-r", cx);
    assert!(matches!(
        composer.read_with(cx, |composer, _| composer.target().clone()),
        Target::Reply { .. }
    ));
    cx.simulate_input("@");
    wait_until(cx, "the members", |cx| {
        composer.read_with(cx, |composer, _| !composer.suggested().is_empty())
    });
    keys("escape", cx);
    assert!(composer.read_with(cx, |composer, _| composer.suggested().is_empty()));
    assert!(matches!(
        composer.read_with(cx, |composer, _| composer.target().clone()),
        Target::Reply { .. }
    ));
    keys("escape", cx);
    assert_eq!(
        composer.read_with(cx, |composer, _| composer.target().clone()),
        Target::New
    );
    assert!(writes(&session).is_empty());
}

#[gpui::test]
fn a_reply_names_the_message_it_answers(cx: &mut TestAppContext) {
    let (view, cx, session) = open_rooms(cx, family());
    let composer = composer(&view, cx);
    // The newest message is Ana's own; Reply answers the selected one.
    keys("secondary-alt-up secondary-alt-up", cx);
    keys("secondary-shift-r", cx);
    let Target::Reply {
        event_id, sender, ..
    } = composer.read_with(cx, |composer, _| composer.target().clone())
    else {
        panic!("not a reply");
    };
    assert_eq!(sender, "Mum");
    cx.simulate_input("Not late!");
    keys("enter", cx);
    wait_until(cx, "the reply sent", |_| !writes(&session).is_empty());
    assert!(matches!(
        writes(&session).as_slice(),
        [RoomRequest::Send { reply_to: Some(answered), .. }] if *answered == event_id
    ));
    assert_eq!(
        composer.read_with(cx, |composer, _| composer.target().clone()),
        Target::New
    );
}

#[gpui::test]
fn up_in_an_empty_composer_edits_the_newest_own_message(cx: &mut TestAppContext) {
    let (view, cx, session) = open_rooms(cx, family());
    let composer = composer(&view, cx);
    keys("secondary-l", cx);
    keys("up", cx);
    let Target::Edit { target, .. } =
        composer.read_with(cx, |composer, _| composer.target().clone())
    else {
        panic!("not editing");
    };
    assert_eq!(
        composer.read_with(cx, |composer, cx| composer.text(cx)),
        "See you then"
    );
    cx.simulate_input(" at 7");
    keys("enter", cx);
    wait_until(cx, "the edit sent", |_| !writes(&session).is_empty());
    let writes = writes(&session);
    let [
        RoomRequest::Edit {
            target: edited,
            message,
        },
    ] = writes.as_slice()
    else {
        panic!("{writes:?}");
    };
    assert_eq!(*edited, target);
    assert_eq!(message.body.as_str(), "See you then at 7");
    // Escape while editing puts the field back.
    keys("up escape", cx);
    assert!(composer.read_with(cx, |composer, cx| composer.text(cx).is_empty()));
}

#[gpui::test]
fn a_file_over_the_limit_is_refused_in_the_composer(cx: &mut TestAppContext) {
    let (view, cx, session) = open_rooms(cx, family());
    let composer = composer(&view, cx);
    composer.update(cx, |composer, cx| {
        composer.add_file(
            FileSource::Bytes {
                name: "Holiday video.mov".to_owned(),
                data: Arc::from(vec![
                    0u8;
                    usize::try_from(FIXTURE_UPLOAD_LIMIT).expect("fits")
                        + 4096
                ]),
            },
            cx,
        );
    });
    wait_until(cx, "the refusal", |cx| {
        composer.read_with(cx, |composer, _| composer.error().is_some())
    });
    let error = composer.read_with(cx, |composer, _| composer.error().map(str::to_owned));
    assert_eq!(
        error.as_deref(),
        Some("That file is 11 MB, and this server takes files up to 11 MB. Nothing was sent.")
    );
    assert!(composer.read_with(cx, |composer, _| composer.files().is_empty()));
    assert!(
        session
            .record()
            .requests
            .iter()
            .all(|(_, request)| !matches!(request, RoomRequest::SendFile { .. }))
    );
    click("button-composer-dismiss-error", cx);
    assert!(composer.read_with(cx, |composer, _| composer.error().is_none()));
}

#[gpui::test]
fn a_pending_file_goes_with_the_message(cx: &mut TestAppContext) {
    let (view, cx, session) = open_rooms(cx, family());
    let composer = composer(&view, cx);
    composer.update(cx, |composer, cx| {
        composer.add_file(
            FileSource::Bytes {
                name: "notes.txt".to_owned(),
                data: Arc::from(b"bring boots".as_slice()),
            },
            cx,
        );
    });
    wait_until(cx, "the file waiting", |cx| {
        composer.read_with(cx, |composer, _| !composer.files().is_empty())
    });
    assert!(writes(&session).is_empty(), "nothing is sent before Send");
    keys("secondary-l", cx);
    cx.simulate_input("the list");
    click("button-composer-send", cx);
    wait_until(cx, "the file and the message sent", |_| {
        writes(&session).len() == 2
    });
    // The file waits for the upload limit, the text doesn't: either may
    // be queued first.
    let writes = writes(&session);
    assert!(
        writes.iter().any(
            |write| matches!(write, RoomRequest::SendFile { file, .. } if file.file_name == "notes.txt")
        ),
        "{writes:?}"
    );
    assert!(writes.iter().any(
        |write| matches!(write, RoomRequest::Send { message, .. } if message.body.as_str() == "the list")
    ));
    assert!(composer.read_with(cx, |composer, _| composer.files().is_empty()));
}

#[gpui::test]
fn the_composer_is_read_only_while_paused(cx: &mut TestAppContext) {
    let (view, cx, session) = open_rooms(cx, family());
    let composer = composer(&view, cx);
    let account = session
        .accounts()
        .borrow()
        .first()
        .map(|account| account.key.clone())
        .expect("an account");
    session.set_sync(
        &account,
        SyncState::Suspended {
            recheck_in: Duration::from_secs(60),
        },
    );
    wait_until(cx, "the pause", |cx| {
        composer.read_with(cx, |composer, _| composer.state() == ComposerState::Paused)
    });
    keys("secondary-l", cx);
    cx.simulate_input("let me talk");
    keys("enter", cx);
    click("button-composer-send", cx);
    assert!(composer.read_with(cx, |composer, cx| composer.text(cx).is_empty()));
    assert!(writes(&session).is_empty());
    let entries = super::a11y::redraw(cx);
    assert!(
        crate::a11y::audit::labels(&entries, gpui::Role::Alert)
            .iter()
            .chain(crate::a11y::audit::labels(&entries, gpui::Role::Status).iter())
            .any(|label| label.contains("Chat is paused")),
        "the banner says Chat is paused"
    );
    session.set_sync(&account, SyncState::Running);
    wait_until(cx, "the pause ending", |cx| {
        composer.read_with(cx, |composer, _| {
            composer.state() == ComposerState::Writable
        })
    });
    keys("secondary-l", cx);
    cx.simulate_input("morning");
    keys("enter", cx);
    wait_until(cx, "the message sent", |_| !writes(&session).is_empty());
}

#[gpui::test]
fn reactions_come_from_the_picker_and_the_chips(cx: &mut TestAppContext) {
    let (view, cx, session) = open_rooms(cx, family());
    let timeline = timeline(&view, cx);
    keys("secondary-shift-l", cx);
    assert!(timeline.read_with(cx, |timeline, _| timeline.picker_open()));
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let quick = (0..64)
        .map(|row| format!("timeline-quick-reaction-{row}-0"))
        .find(|selector| {
            let selector: &'static str = Box::leak(selector.clone().into_boxed_str());
            cx.debug_bounds(selector).is_some()
        })
        .expect("the picker shows");
    click(Box::leak(quick.into_boxed_str()), cx);
    wait_until(cx, "the reaction", |_| !writes(&session).is_empty());
    assert!(matches!(
        writes(&session).as_slice(),
        [RoomRequest::ToggleReaction { key, .. }] if key == "👍"
    ));
    assert!(!timeline.read_with(cx, |timeline, _| timeline.picker_open()));
    // A reaction under a message takes back (or adds) the account's own.
    let chip = (0..64)
        .flat_map(|row| (0..4).map(move |index| format!("timeline-reaction-{row}-{index}")))
        .find(|selector| {
            let selector: &'static str = Box::leak(selector.clone().into_boxed_str());
            cx.debug_bounds(selector).is_some()
        })
        .expect("a reaction shows");
    click(Box::leak(chip.into_boxed_str()), cx);
    wait_until(cx, "the second reaction", |_| writes(&session).len() == 2);
}

#[gpui::test]
fn deleting_a_message_asks_first(cx: &mut TestAppContext) {
    let (view, cx, session) = open_rooms(cx, family());
    let rooms = rooms_view(&view, cx);
    let _ = timeline(&view, cx);
    // Nothing selected: the command says how.
    keys("secondary-shift-backspace", cx);
    assert!(rooms.read_with(cx, |rooms, _| rooms.delete_prompt().is_none()));
    keys("secondary-alt-up", cx);
    keys("secondary-shift-backspace", cx);
    let asked = rooms.read_with(cx, |rooms, _| rooms.delete_prompt().cloned());
    assert_eq!(
        asked.map(|choice| choice.text),
        Some("See you then".to_owned())
    );
    click("button-delete-cancel", cx);
    assert!(writes(&session).is_empty(), "cancelling deletes nothing");
    keys("secondary-shift-backspace", cx);
    click("button-delete-confirm", cx);
    wait_until(cx, "the deletion", |_| !writes(&session).is_empty());
    assert!(matches!(
        writes(&session).as_slice(),
        [RoomRequest::Redact { reason: None, .. }]
    ));
}

#[gpui::test]
fn leaving_a_room_asks_first(cx: &mut TestAppContext) {
    let (view, cx, session) = open_rooms(cx, family());
    let rooms = rooms_view(&view, cx);
    let _ = timeline(&view, cx);
    click("button-room-leave", cx);
    click("button-leave-cancel", cx);
    assert!(writes(&session).is_empty());
    keys("secondary-shift-w", cx);
    click("button-leave-confirm", cx);
    wait_until(cx, "the room left", |cx| {
        rooms.read_with(cx, |rooms, _| {
            rooms.selected().is_none()
                && rooms
                    .rooms()
                    .iter()
                    .all(|room| room.room_id.as_str() != FAMILY_ROOM)
        })
    });
    assert_eq!(writes(&session), [RoomRequest::Leave]);
}

#[gpui::test]
fn a_kept_draft_comes_back_and_typing_keeps_it(cx: &mut TestAppContext) {
    let (view, cx, session) = {
        let session = familychat_test_support::fixtures::FixtureSession::new();
        let account = session.add_account(
            &familychat_test_support::fixtures::fixture_user(),
            SyncState::Running,
        );
        session.with_family();
        session.set_draft(
            &account,
            FAMILY_ROOM,
            Draft {
                text: MessageBody::new("half a thought"),
                ..Draft::default()
            },
        );
        let signin = familychat_test_support::fixtures::FixtureSignin::new(session.clone());
        let startup = familychat_test_support::fixtures::FixtureStartup::with(
            session.clone(),
            signin,
            &[familychat_test_support::fixtures::FixtureProfile::Ready],
        );
        let (view, cx) = super::open(cx, startup, crate::InitialView::Rooms(family()));
        (view, cx, session)
    };
    let composer = composer(&view, cx);
    wait_until(cx, "the draft", |cx| {
        composer.read_with(cx, |composer, cx| composer.text(cx) == "half a thought")
    });
    keys("secondary-l end", cx);
    cx.simulate_input(", finished");
    cx.executor()
        .advance_clock(DRAFT_SAVE_DELAY + Duration::from_millis(100));
    wait_until(cx, "the draft kept", |_| {
        !session.record().drafts.is_empty()
    });
    let kept = session.record().drafts.last().cloned();
    assert_eq!(
        kept.and_then(|(_, draft)| draft)
            .map(|draft| draft.text.as_str().to_owned()),
        Some("half a thought, finished".to_owned())
    );
}

#[gpui::test]
fn reading_the_room_moves_the_read_marker(cx: &mut TestAppContext) {
    let (view, cx, session) = open_rooms(cx, family());
    let timeline = timeline(&view, cx);
    // Only a window in front reads; new messages arriving while it is
    // follow the newest one.
    cx.update(|window, _| window.activate_window());
    let items = timeline.read_with(cx, |timeline, _| timeline.items().to_vec());
    session.set_timeline(FAMILY_ROOM, items);
    wait_until(cx, "the receipt", |_| {
        session
            .record()
            .requests
            .iter()
            .any(|(_, request)| *request == RoomRequest::MarkRead)
    });
}
