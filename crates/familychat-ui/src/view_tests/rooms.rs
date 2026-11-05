//! The rooms and the timeline (PLAN.md, section 13.6) over the fixture
//! family: the room list and its filters, opening rooms and the commands
//! that move between them, invitations, the member list, the timeline's
//! rows, pictures (never SVG), saving files, and links: Matrix links open
//! in the app, every other link shows its whole address and asks first,
//! and the rest are refused.

use std::{path::PathBuf, sync::Arc};

use familychat_core::{
    links::LinkPrompt,
    rooms::RoomListFilter,
    sync::SyncState,
    timeline::{TimelineItemKind, TimelineRow},
};
use familychat_test_support::fixtures::{
    FixtureProfile, FixtureSession, FixtureSignin, FixtureStartup, fixture_user,
    rooms::{
        FAMILY_ROOM, FOOTBALL_ROOM, HOLIDAY_ROOM, INVITE_ROOM, MUM_ROOM, PHOTOS_ROOM, SVG_HANDLE,
    },
};
use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};

use super::{click, keys, open, wait_until};
use crate::rooms::{RoomsView, TimelineView};
use crate::{InitialView, RoomsLaunch, ShellView};

/// The window signed in to the fixture family, opened as `launch` says.
pub(super) fn open_rooms(
    cx: &mut TestAppContext,
    launch: RoomsLaunch,
) -> (
    Entity<ShellView>,
    &mut VisualTestContext,
    Arc<FixtureSession>,
) {
    let session = FixtureSession::new();
    session.add_account(&fixture_user(), SyncState::Running);
    session.with_family();
    let signin = FixtureSignin::new(session.clone());
    let startup = FixtureStartup::with(session.clone(), signin, &[FixtureProfile::Ready]);
    let (view, cx) = open(cx, startup, InitialView::Rooms(launch));
    wait_until(cx, "the room list", |cx| {
        rooms_view(&view, cx).read_with(cx, |rooms, _| !rooms.rooms().is_empty())
    });
    (view, cx, session)
}

/// The rooms view.
pub(super) fn rooms_view(
    view: &Entity<ShellView>,
    cx: &mut VisualTestContext,
) -> Entity<RoomsView> {
    let signed_in = cx
        .read_entity(view, |view, _| view.signed_in().cloned())
        .expect("the signed-in view shows");
    cx.read_entity(&signed_in, |signed_in, _| signed_in.rooms().clone())
}

/// The open room's timeline, once it has loaded.
pub(super) fn timeline(
    view: &Entity<ShellView>,
    cx: &mut VisualTestContext,
) -> Entity<TimelineView> {
    let rooms = rooms_view(view, cx);
    wait_until(cx, "the timeline", |cx| {
        rooms.read_with(cx, |rooms, cx| {
            rooms
                .timeline()
                .is_some_and(|timeline| timeline.read(cx).is_loaded())
        })
    });
    rooms.read_with(cx, |rooms, _| {
        rooms.timeline().cloned().expect("a room is open")
    })
}

fn selected(view: &Entity<ShellView>, cx: &mut VisualTestContext) -> Option<String> {
    let rooms = rooms_view(view, cx);
    rooms.read_with(cx, |rooms, _| rooms.selected().map(|room| room.to_string()))
}

fn room(id: &str) -> RoomsLaunch {
    RoomsLaunch {
        room: Some(id.to_owned()),
        ..RoomsLaunch::default()
    }
}

#[gpui::test]
fn the_room_list_filters_and_opens_rooms(cx: &mut TestAppContext) {
    let (view, cx, session) = open_rooms(cx, RoomsLaunch::default());
    let rooms = rooms_view(&view, cx);
    let all = rooms.read_with(cx, |rooms, _| rooms.rooms().len());
    assert_eq!(all, session.rooms().len(), "every room shows");
    assert!(selected(&view, cx).is_none(), "no room opens on its own");

    click("button-rooms-filter-unread", cx);
    wait_until(cx, "the unread rooms", |cx| {
        rooms.read_with(cx, |rooms, _| {
            rooms.filter() == RoomListFilter::Unread
                && rooms.rooms().len() < all
                && rooms.rooms().iter().all(|room| room.unread.is_unread())
        })
    });
    assert_eq!(
        session.record().filters,
        [RoomListFilter::All, RoomListFilter::Unread]
    );

    keys("secondary-alt-3", cx);
    wait_until(cx, "the direct chats", |cx| {
        rooms.read_with(cx, |rooms, _| {
            rooms.filter() == RoomListFilter::People
                && rooms.rooms().iter().all(|room| room.is_direct)
        })
    });
    click("room-row-0", cx);
    let open = selected(&view, cx).expect("a room opened");
    assert_eq!(open, MUM_ROOM);
    let timeline = timeline(&view, cx);
    assert!(timeline.read_with(cx, |timeline, _| !timeline.items().is_empty()));
    assert_eq!(session.record().timelines.len(), 1);
}

#[gpui::test]
fn the_room_commands_move_through_the_list(cx: &mut TestAppContext) {
    let (view, cx, _session) = open_rooms(cx, room(FAMILY_ROOM));
    assert_eq!(selected(&view, cx).as_deref(), Some(FAMILY_ROOM));
    keys("alt-down", cx);
    assert_eq!(selected(&view, cx).as_deref(), Some(MUM_ROOM));
    keys("alt-up", cx);
    assert_eq!(selected(&view, cx).as_deref(), Some(FAMILY_ROOM));
    // The next unread room below the family room is Grandad's.
    keys("alt-shift-down", cx);
    let unread = selected(&view, cx).expect("an unread room");
    let rooms = rooms_view(&view, cx);
    assert!(rooms.read_with(cx, |rooms, _| {
        rooms
            .rooms()
            .iter()
            .any(|room| room.room_id.as_str() == unread && room.unread.is_unread())
    }));
    keys("secondary-.", cx);
    assert!(rooms.read_with(cx, |rooms, _| rooms.members_open()));
    keys("secondary-.", cx);
    assert!(rooms.read_with(cx, |rooms, _| !rooms.members_open()));
}

#[gpui::test]
fn the_timeline_folds_room_changes_and_finds_the_read_marker(cx: &mut TestAppContext) {
    let (view, cx, _session) = open_rooms(cx, room(FOOTBALL_ROOM));
    let timeline = timeline(&view, cx);
    let groups = timeline.read_with(cx, |timeline, _| {
        timeline
            .rows()
            .iter()
            .filter(|row| matches!(row, TimelineRow::StateGroup { .. }))
            .count()
    });
    assert_eq!(groups, 2, "two runs of room changes fold");
    let first_group = timeline.read_with(cx, |timeline, _| {
        timeline
            .rows()
            .iter()
            .position(|row| matches!(row, TimelineRow::StateGroup { .. }))
            .expect("a group")
    });
    // Scroll it into view, then open it.
    timeline.update(cx, |timeline, _| {
        timeline.list_state().scroll_to_reveal_item(first_group);
    });
    let toggle: &'static str =
        Box::leak(format!("button-state-group-toggle-{first_group}").into_boxed_str());
    click(toggle, cx);
    assert!(timeline.read_with(cx, |timeline, _| timeline.is_expanded(first_group)));
    click(toggle, cx);
    assert!(!timeline.read_with(cx, |timeline, _| timeline.is_expanded(first_group)));
}

#[gpui::test]
fn jumping_to_the_oldest_unread_message_finds_the_read_marker(cx: &mut TestAppContext) {
    let (view, cx, _session) = open_rooms(cx, room(FAMILY_ROOM));
    let timeline = timeline(&view, cx);
    let has_marker = timeline.read_with(cx, |timeline, _| {
        timeline
            .items()
            .iter()
            .any(|item| item.kind == TimelineItemKind::ReadMarker)
    });
    assert!(has_marker);
    keys("shift-pageup", cx);
    assert!(!timeline.read_with(cx, |timeline, _| timeline.list_state().is_following_tail()));
    keys("secondary-end", cx);
    assert!(timeline.read_with(cx, |timeline, _| timeline.list_state().is_following_tail()));
}

#[gpui::test]
fn an_invitation_is_accepted_or_declined_from_the_list(cx: &mut TestAppContext) {
    let (view, cx, session) = open_rooms(cx, RoomsLaunch::default());
    let rooms = rooms_view(&view, cx);
    let invite = rooms.read_with(cx, |rooms, _| {
        rooms
            .rooms()
            .iter()
            .position(|room| room.is_invite())
            .expect("the fixture has an invitation")
    });
    let accept: &'static str = Box::leak(format!("button-invite-accept-{invite}").into_boxed_str());
    click(accept, cx);
    wait_until(cx, "the invitation accepted", |cx| {
        rooms.read_with(cx, |rooms, _| {
            rooms
                .rooms()
                .iter()
                .any(|room| room.room_id.as_str() == INVITE_ROOM && !room.is_invite())
        })
    });
    assert_eq!(session.record().invites.len(), 1);
    assert!(session.record().invites[0].1, "accepted");
    let toasts = cx.read_entity(&view, |view, _| view.toasts().clone());
    let shown = cx.read_entity(&toasts, |toasts, _| toasts.visible());
    assert!(
        shown
            .iter()
            .any(|(_, message)| message.contains("You joined Cousins")),
        "{shown:?}"
    );
}

#[gpui::test]
fn an_invitation_opens_its_answer_instead_of_a_timeline(cx: &mut TestAppContext) {
    let (view, cx, session) = open_rooms(cx, room(INVITE_ROOM));
    let rooms = rooms_view(&view, cx);
    assert!(rooms.read_with(cx, |rooms, _| rooms.timeline().is_none()));
    click("button-invite-decline", cx);
    wait_until(cx, "the invitation declined", |cx| {
        rooms.read_with(cx, |rooms, _| {
            rooms
                .rooms()
                .iter()
                .all(|room| room.room_id.as_str() != INVITE_ROOM)
        })
    });
    assert_eq!(session.record().invites.len(), 1);
    assert!(!session.record().invites[0].1, "declined");
    assert!(session.record().timelines.is_empty());
}

#[gpui::test]
fn every_other_link_shows_its_whole_address_and_asks_first(cx: &mut TestAppContext) {
    let (view, cx, _session) = open_rooms(cx, room(HOLIDAY_ROOM));
    let rooms = rooms_view(&view, cx);
    let target = "https://cottages.example/booking/4471?ref=family";
    cx.update(|window, cx| {
        rooms.update(cx, |rooms, cx| {
            rooms.open_link(target, "cottages.example", window, cx);
        });
    });
    cx.run_until_parked();
    let prompt: LinkPrompt = rooms
        .read_with(cx, |rooms, _| rooms.link_prompt().cloned())
        .expect("the dialog asks");
    assert_eq!(prompt.target, target, "the whole address");
    assert!(!prompt.is_mismatched());
    assert_eq!(cx.opened_url(), None, "nothing opens before the answer");
    click("button-link-cancel", cx);
    assert!(rooms.read_with(cx, |rooms, _| rooms.link_prompt().is_none()));
    assert_eq!(cx.opened_url(), None, "Cancel opens nothing");

    cx.update(|window, cx| {
        rooms.update(cx, |rooms, cx| {
            rooms.open_link(
                "https://tickets.evil.example/claim",
                "https://www.bank.example",
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();
    let prompt = rooms
        .read_with(cx, |rooms, _| rooms.link_prompt().cloned())
        .expect("the dialog asks");
    assert!(prompt.is_mismatched(), "the look-alike is flagged");
    click("button-link-open", cx);
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://tickets.evil.example/claim"),
        "only after the answer"
    );
}

#[gpui::test]
fn matrix_links_open_in_the_app_and_others_are_refused(cx: &mut TestAppContext) {
    let (view, cx, _session) = open_rooms(cx, room(HOLIDAY_ROOM));
    let rooms = rooms_view(&view, cx);
    cx.update(|window, cx| {
        rooms.update(cx, |rooms, cx| {
            rooms.open_link(
                &format!("https://matrix.to/#/{MUM_ROOM}"),
                "Mum",
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();
    assert_eq!(selected(&view, cx).as_deref(), Some(MUM_ROOM));
    assert!(rooms.read_with(cx, |rooms, _| rooms.link_prompt().is_none()));

    for href in [
        "javascript:alert(1)",
        "file:///etc/passwd",
        "data:text/html,x",
    ] {
        cx.update(|window, cx| {
            rooms.update(cx, |rooms, cx| rooms.open_link(href, "x", window, cx));
        });
        cx.run_until_parked();
        assert!(
            rooms.read_with(cx, |rooms, _| rooms.link_prompt().is_none()),
            "{href} asks nothing"
        );
    }
    assert_eq!(cx.opened_url(), None);
}

#[gpui::test]
fn pictures_come_from_the_session_and_svg_is_never_asked_for(cx: &mut TestAppContext) {
    let (view, cx, session) = open_rooms(cx, room(PHOTOS_ROOM));
    let _timeline = timeline(&view, cx);
    let rooms = rooms_view(&view, cx);
    let images = rooms.read_with(cx, |rooms, _| rooms.images().clone());
    wait_until(cx, "a picture", |cx| {
        images.read_with(cx, |images, _| images.ready_count() > 0)
    });
    let asked = session.record().images;
    assert!(!asked.is_empty());
    assert!(
        !asked.iter().any(|handle| handle == SVG_HANDLE),
        "the SVG was never fetched: {asked:?}"
    );
}

#[gpui::test]
fn a_file_is_saved_where_the_person_chooses(cx: &mut TestAppContext) {
    let (view, cx, session) = open_rooms(cx, room(PHOTOS_ROOM));
    let media = familychat_test_support::fixtures::rooms::family_timeline_for(
        PHOTOS_ROOM,
        familychat_test_support::fixtures::rooms::now_ms(),
    )
    .into_iter()
    .find_map(|item| match item.kind {
        TimelineItemKind::Event(event) => match event.content {
            familychat_core::timeline::TimelineContent::Message {
                kind: familychat_core::timeline::MessageKind::File,
                media,
                ..
            } => media,
            _ => None,
        },
        _ => None,
    })
    .expect("the fixture has a file");
    let timeline = timeline(&view, cx);
    timeline.update(cx, |_, cx| {
        cx.emit(crate::rooms::TimelineViewEvent::Save(media.clone()));
    });
    cx.run_until_parked();
    assert!(cx.did_prompt_for_new_path(), "the person chooses where");
    let chosen = PathBuf::from("/fixture/Downloads").join(&media.filename);
    let answer = chosen.clone();
    cx.simulate_new_path_selection(move |_| Some(answer));
    wait_until(cx, "the save", |_| !session.record().saved.is_empty());
    assert_eq!(
        session.record().saved,
        [(media.filename.clone(), chosen)],
        "saved under the safe name, where chosen"
    );
}
