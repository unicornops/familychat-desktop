//! Notifications (PLAN.md, section 5.9; ADR 0013): what the session
//! reports is posted as a system notification for the rooms the person
//! isn't looking at, a click opens the room, hidden content says "New
//! message", and the window's title carries the unread total. GPUI's test
//! platform records the posted notifications and simulates the click; the
//! operating systems' own delivery is on the manual checklist.

use std::sync::Arc;

use familychat_core::{
    OwnedRoomId,
    notifications::{IncomingNotification, NotificationContent},
    session::AccountKey,
    settings::{NotificationSettings, SettingsApi as _},
    sync::SyncState,
};
use familychat_notify::{FLOOD_TAG, MAX_NOTIFICATIONS_PER_MINUTE, NEW_MESSAGE};
use familychat_test_support::fixtures::{
    FixtureProfile, FixtureSession, FixtureSignin, FixtureStartup, fixture_user,
    rooms::{FAMILY_ROOM, MUM_ROOM},
};
use gpui::{
    AppContext as _, Entity, SystemNotificationResponse, TestAppContext, VisualTestContext,
};

use super::{click, keys, open, rooms::rooms_view, wait_until};
use crate::notifications::BURST_WAIT;
use crate::{InitialView, RoomsLaunch, ShellView};

struct Setup {
    session: Arc<FixtureSession>,
    startup: Arc<FixtureStartup>,
    account: AccountKey,
}

fn setup(hidden: bool) -> Setup {
    let session = FixtureSession::new();
    let account = session.add_account(&fixture_user(), SyncState::Running);
    session.with_family();
    let signin = FixtureSignin::new(session.clone());
    let startup = FixtureStartup::with(session.clone(), signin, &[FixtureProfile::Ready]);
    if hidden {
        startup.set_notification_settings(NotificationSettings {
            enabled: true,
            show_content: false,
        });
    }
    Setup {
        session,
        startup,
        account,
    }
}

fn open_on<'a>(
    cx: &'a mut TestAppContext,
    setup: &Setup,
    launch: RoomsLaunch,
) -> (Entity<ShellView>, &'a mut VisualTestContext) {
    let (view, cx) = open(cx, setup.startup.clone(), InitialView::Rooms(launch));
    wait_until(cx, "the room list", |cx| {
        rooms_view(&view, cx).read_with(cx, |rooms, _| !rooms.rooms().is_empty())
    });
    (view, cx)
}

fn room(id: &str) -> OwnedRoomId {
    id.try_into().expect("a room id")
}

fn message(setup: &Setup, room_id: &str, room_name: &str, text: &str) -> IncomingNotification {
    IncomingNotification {
        account: setup.account.clone(),
        room_id: room(room_id),
        room_name: room_name.to_owned(),
        sender_name: "Mum".to_owned(),
        is_direct: false,
        content: NotificationContent::Text(text.to_owned()),
    }
}

/// Reports `batch` as the account's sync would, and lets the window post.
fn report(setup: &Setup, batch: Vec<IncomingNotification>, cx: &mut VisualTestContext) {
    for incoming in batch {
        assert!(setup.session.notify(incoming), "the window listens");
    }
    cx.run_until_parked();
    cx.executor().advance_clock(BURST_WAIT * 2);
    cx.run_until_parked();
}

fn shown(cx: &mut VisualTestContext) -> Vec<gpui::SystemNotification> {
    cx.shown_system_notifications()
}

fn click_notification(tag: &str, cx: &mut VisualTestContext) {
    cx.simulate_system_notification_response(SystemNotificationResponse {
        tag: tag.to_owned().into(),
        action_id: None,
    });
    cx.run_until_parked();
}

fn selected(view: &Entity<ShellView>, cx: &mut VisualTestContext) -> Option<String> {
    rooms_view(view, cx).read_with(cx, |rooms, _| rooms.selected().map(ToString::to_string))
}

#[gpui::test]
fn a_background_rooms_message_is_posted_and_a_click_opens_the_room(cx: &mut TestAppContext) {
    let setup = setup(false);
    let (view, cx) = open_on(cx, &setup, RoomsLaunch::default());
    cx.deactivate_window();
    report(
        &setup,
        vec![message(
            &setup,
            FAMILY_ROOM,
            "Smith family",
            "Dinner at six",
        )],
        cx,
    );
    let posted = shown(cx);
    assert_eq!(posted.len(), 1, "{posted:?}");
    assert_eq!(posted[0].title.as_ref(), "Smith family");
    assert_eq!(posted[0].body.as_ref(), "Mum: Dinner at six");
    assert!(posted[0].actions.is_empty());
    assert!(
        !posted[0].tag.contains("smith") && !posted[0].tag.contains('!'),
        "the tag holds no Matrix id: {}",
        posted[0].tag
    );
    assert_eq!(selected(&view, cx), None);

    click_notification(&posted[0].tag, cx);
    wait_until(cx, "the room", |cx| {
        selected(&view, cx).as_deref() == Some(FAMILY_ROOM)
    });
    assert!(
        cx.update(|window, _| window.is_window_active()),
        "the window came forward"
    );
    assert!(
        cx.dismissed_system_notifications()
            .iter()
            .any(|tag| *tag == posted[0].tag),
        "the room's notification came down"
    );
}

#[gpui::test]
fn nothing_is_posted_for_the_room_in_front_of_the_person(cx: &mut TestAppContext) {
    let setup = setup(false);
    let (view, cx) = open_on(
        cx,
        &setup,
        RoomsLaunch {
            room: Some(FAMILY_ROOM.to_owned()),
            ..RoomsLaunch::default()
        },
    );
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    assert!(cx.update(|window, _| window.is_window_active()));
    assert_eq!(selected(&view, cx).as_deref(), Some(FAMILY_ROOM));
    report(
        &setup,
        vec![
            message(&setup, FAMILY_ROOM, "Smith family", "in front"),
            message(&setup, MUM_ROOM, "Mum", "elsewhere"),
        ],
        cx,
    );
    let posted = shown(cx);
    assert_eq!(posted.len(), 1, "only the other room: {posted:?}");
    assert_eq!(posted[0].title.as_ref(), "Mum");

    // With the window in the background, the open room notifies too.
    cx.deactivate_window();
    report(
        &setup,
        vec![message(&setup, FAMILY_ROOM, "Smith family", "now")],
        cx,
    );
    let posted = shown(cx);
    assert_eq!(posted.len(), 2);
    assert_eq!(posted[1].body.as_ref(), "Mum: now");
}

#[gpui::test]
fn hidden_content_says_new_message_and_the_setting_can_be_changed(cx: &mut TestAppContext) {
    let setup = setup(true);
    let (view, cx) = open_on(cx, &setup, RoomsLaunch::default());
    cx.deactivate_window();
    report(
        &setup,
        vec![message(
            &setup,
            FAMILY_ROOM,
            "Smith family",
            "the secret plan",
        )],
        cx,
    );
    let posted = shown(cx);
    assert_eq!(posted[0].title.as_ref(), "Smith family");
    assert_eq!(posted[0].body.as_ref(), format!("Mum: {NEW_MESSAGE}"));
    assert!(!posted[0].body.contains("secret"));

    // Settings → Notifications shows the setting and changes it in the
    // profile; the main window follows.
    cx.update(|window, _| window.activate_window());
    keys("secondary-alt-n", cx);
    let (root, mut settings_cx) = super::settings::settings_window(cx);
    assert_eq!(
        root.read_with(&settings_cx, |root, _| root.section()),
        crate::SettingsSection::Notifications
    );
    assert!(!root.read_with(&settings_cx, |root, _| root.shown_settings().0.show_content));
    click("button-settings-notifications-text", &mut settings_cx);
    assert!(
        setup
            .startup
            .settings()
            .notification_settings()
            .borrow()
            .show_content,
        "saved in the profile's settings"
    );
    assert!(cx.read_entity(&view, |view, _| view.notification_settings().show_content));
    keys("escape", &mut settings_cx);
    assert!(
        cx.update(|_, cx| crate::settings::window(cx)).is_none(),
        "Escape closed Settings"
    );

    cx.deactivate_window();
    report(
        &setup,
        vec![message(&setup, MUM_ROOM, "Mum", "now visible")],
        cx,
    );
    assert_eq!(
        shown(cx).last().map(|posted| posted.body.to_string()),
        Some("Mum: now visible".to_owned())
    );
}

#[gpui::test]
fn the_window_title_carries_the_unread_total(cx: &mut TestAppContext) {
    let setup = setup(false);
    let (_view, cx) = open_on(cx, &setup, RoomsLaunch::default());
    assert_eq!(cx.window_title().as_deref(), Some("Family Chat"));
    setup.session.set_unread(&setup.account, 4);
    cx.run_until_parked();
    assert_eq!(cx.window_title().as_deref(), Some("Family Chat (4)"));
    setup.session.set_unread(&setup.account, 0);
    cx.run_until_parked();
    assert_eq!(cx.window_title().as_deref(), Some("Family Chat"));
}

#[gpui::test]
fn a_room_outside_the_list_opens_from_its_notification(cx: &mut TestAppContext) {
    let setup = setup(false);
    let (view, cx) = open_on(cx, &setup, RoomsLaunch::default());
    cx.deactivate_window();
    let elsewhere = "!elsewhere:smith.safechat.family";
    report(
        &setup,
        vec![message(&setup, elsewhere, "Far away", "hello")],
        cx,
    );
    let posted = shown(cx);
    click_notification(&posted[0].tag, cx);
    wait_until(cx, "the room", |cx| {
        selected(&view, cx).as_deref() == Some(elsewhere)
    });
}

#[gpui::test]
fn a_click_on_an_unknown_notification_only_brings_the_window_forward(cx: &mut TestAppContext) {
    let setup = setup(false);
    let (view, cx) = open_on(cx, &setup, RoomsLaunch::default());
    cx.deactivate_window();
    click_notification("familychat-room-from-an-earlier-run", cx);
    assert!(cx.update(|window, _| window.is_window_active()));
    assert_eq!(selected(&view, cx), None);
}

#[gpui::test]
fn a_flood_is_capped(cx: &mut TestAppContext) {
    let setup = setup(false);
    let (_view, cx) = open_on(cx, &setup, RoomsLaunch::default());
    cx.deactivate_window();
    // Thirty rooms, five messages each, in bursts.
    for burst in 0..5 {
        let batch = (0..30)
            .map(|index| {
                message(
                    &setup,
                    &format!("!flood{index}:smith.safechat.family"),
                    "Flood",
                    &format!("message {burst}"),
                )
            })
            .collect();
        report(&setup, batch, cx);
    }
    let posted = shown(cx);
    assert_eq!(
        posted.len(),
        MAX_NOTIFICATIONS_PER_MINUTE + 1,
        "{:?}",
        posted
            .iter()
            .map(|posted| posted.tag.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        posted.last().map(|posted| posted.tag.as_ref()),
        Some(FLOOD_TAG)
    );
}
