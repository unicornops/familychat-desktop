//! Notifications, bridged (PLAN.md, section 5.9; ADR 0013): matrix-sdk's
//! notification handler on every account's client, which the SDK calls
//! during sync for each event its push rules say to notify about (the
//! event decrypted first where the account has the keys), turned into
//! `familychat-core`'s [`IncomingNotification`] and sent to the one
//! frontend listening.
//!
//! Left out: the account's own messages, events sent before the session
//! started (less a minute's grace for clocks that disagree: a sync that
//! catches up after a restart shows its unread badges, not a notification
//! storm), and an event already reported (sliding sync can send a room's
//! latest events again when it is opened). Nothing here logs a name or a
//! message.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use familychat_core::{
    notifications::{IncomingNotification, NotificationContent},
    session::{AccountKey, SUBSCRIPTION_BUFFER},
};
use matrix_sdk::{
    Client, Room, RoomState,
    deserialized_responses::RawAnySyncOrStrippedTimelineEvent,
    ruma::{
        OwnedEventId, OwnedRoomId, UserId,
        events::{
            AnyStrippedStateEvent, AnySyncMessageLikeEvent, AnySyncTimelineEvent,
            SyncMessageLikeEvent,
            room::{
                member::MembershipState,
                message::{MessageType, Relation},
            },
        },
        push::Action,
    },
    sync::Notification,
};
use tokio::sync::mpsc::{self, error::TrySendError};
use tracing::debug;

/// How far before the session started an event may have been sent and
/// still notify.
const STALE_GRACE: Duration = Duration::from_secs(60);

/// How many reported events are remembered, to report each once.
const REMEMBERED_EVENTS: usize = 512;

/// Where the session's notifications go: the latest receiver a frontend
/// asked for.
#[derive(Debug, Clone, Default)]
pub(crate) struct NotificationFeed {
    sender: Arc<Mutex<Option<mpsc::Sender<IncomingNotification>>>>,
}

impl NotificationFeed {
    /// A new receiver, which replaces the previous one.
    pub(crate) fn subscribe(&self) -> mpsc::Receiver<IncomingNotification> {
        let (sender, receiver) = mpsc::channel(SUBSCRIPTION_BUFFER);
        *self.sender.lock().unwrap_or_else(PoisonError::into_inner) = Some(sender);
        receiver
    }

    /// Sends `incoming` if a frontend is listening and keeping up.
    pub(crate) fn send(&self, incoming: IncomingNotification) {
        let sender = self
            .sender
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let Some(sender) = sender else {
            return;
        };
        match sender.try_send(incoming) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                debug!("dropped a notification: the frontend is behind");
            }
            Err(TrySendError::Closed(_)) => {}
        }
    }
}

/// What has been reported, so each event is reported once.
#[derive(Debug, Default)]
struct Reported {
    events: VecDeque<OwnedEventId>,
    invites: VecDeque<OwnedRoomId>,
}

impl Reported {
    /// `true` the first time `id` is seen.
    fn first_event(&mut self, id: &OwnedEventId) -> bool {
        first(&mut self.events, id)
    }

    /// `true` the first time an invitation to `room` is seen.
    fn first_invite(&mut self, room: &OwnedRoomId) -> bool {
        first(&mut self.invites, room)
    }
}

fn first<T: PartialEq + Clone>(seen: &mut VecDeque<T>, value: &T) -> bool {
    if seen.contains(value) {
        return false;
    }
    if seen.len() == REMEMBERED_EVENTS {
        seen.pop_front();
    }
    seen.push_back(value.clone());
    true
}

/// Milliseconds since the Unix epoch, now.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Registers the notification handler on `client`, for `account`.
pub(crate) async fn register(client: &Client, account: AccountKey, feed: NotificationFeed) {
    let cutoff_ms =
        now_ms().saturating_sub(u64::try_from(STALE_GRACE.as_millis()).unwrap_or(u64::MAX));
    let reported = Arc::new(Mutex::new(Reported::default()));
    client
        .register_notification_handler(
            move |notification: Notification, room: Room, client: Client| {
                let account = account.clone();
                let feed = feed.clone();
                let reported = reported.clone();
                async move {
                    if let Some(incoming) =
                        incoming(account, &notification, &room, &client, cutoff_ms, &reported).await
                    {
                        feed.send(incoming);
                    }
                }
            },
        )
        .await;
}

/// The notification as a frontend gets it, or `None` when it is left out.
async fn incoming(
    account: AccountKey,
    notification: &Notification,
    room: &Room,
    client: &Client,
    cutoff_ms: u64,
    reported: &Mutex<Reported>,
) -> Option<IncomingNotification> {
    if !notification.actions.iter().any(Action::should_notify) {
        return None;
    }
    let own = client.user_id()?;
    let (sender, content) = match &notification.event {
        RawAnySyncOrStrippedTimelineEvent::Sync(raw) => {
            let event = raw.deserialize().ok()?;
            let sent_ms = u64::from(event.origin_server_ts().get());
            if event.sender() == own || sent_ms < cutoff_ms {
                return None;
            }
            let first = reported
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .first_event(&event.event_id().to_owned());
            if !first {
                return None;
            }
            (event.sender().to_owned(), content(&event))
        }
        RawAnySyncOrStrippedTimelineEvent::Stripped(raw) => {
            let AnyStrippedStateEvent::RoomMember(member) = raw.deserialize().ok()? else {
                return None;
            };
            if member.content.membership != MembershipState::Invite
                || member.state_key != *own
                || room.state() != RoomState::Invited
            {
                return None;
            }
            let first = reported
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .first_invite(&room.room_id().to_owned());
            if !first {
                return None;
            }
            (member.sender, NotificationContent::Invite)
        }
    };
    Some(IncomingNotification {
        account,
        room_id: room.room_id().to_owned(),
        room_name: room_name(room).await,
        sender_name: sender_name(room, &sender).await,
        is_direct: room.is_direct().await.unwrap_or(false),
        content,
    })
}

/// The room's name, as the room list shows it.
async fn room_name(room: &Room) -> String {
    if let Some(name) = room.cached_display_name() {
        return name.to_string();
    }
    match room.display_name().await {
        Ok(name) => name.to_string(),
        Err(_) => room.name().unwrap_or_else(|| room.room_id().to_string()),
    }
}

/// The sender's display name in the room, or their user id.
async fn sender_name(room: &Room, sender: &UserId) -> String {
    room.get_member_no_sync(sender)
        .await
        .ok()
        .flatten()
        .and_then(|member| member.display_name().map(str::to_owned))
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| sender.to_string())
}

/// What an event is, for a notification.
pub(crate) fn content(event: &AnySyncTimelineEvent) -> NotificationContent {
    let AnySyncTimelineEvent::MessageLike(event) = event else {
        return NotificationContent::Unknown;
    };
    match event {
        AnySyncMessageLikeEvent::RoomMessage(SyncMessageLikeEvent::Original(message)) => {
            // An edit carries the new text in `m.new_content`.
            let msgtype = match &message.content.relates_to {
                Some(Relation::Replacement(replacement)) => &replacement.new_content.msgtype,
                _ => &message.content.msgtype,
            };
            match msgtype {
                MessageType::Text(text) => {
                    NotificationContent::Text(without_reply_fallback(&text.body))
                }
                MessageType::Notice(notice) => {
                    NotificationContent::Text(without_reply_fallback(&notice.body))
                }
                MessageType::Emote(emote) => NotificationContent::Emote(emote.body.clone()),
                MessageType::Image(_) => NotificationContent::Image,
                MessageType::Video(_) => NotificationContent::Video,
                MessageType::Audio(_) => NotificationContent::Audio,
                MessageType::File(_) => NotificationContent::File,
                MessageType::Location(_) => NotificationContent::Location,
                _ => NotificationContent::Unknown,
            }
        }
        AnySyncMessageLikeEvent::Sticker(SyncMessageLikeEvent::Original(_)) => {
            NotificationContent::Sticker
        }
        _ => NotificationContent::Unknown,
    }
}

/// A reply's plain body without the quoted fallback older clients put in
/// front of it (`> <@ana:example.org> quoted`, then a blank line).
fn without_reply_fallback(body: &str) -> String {
    if !body.starts_with("> ") {
        return body.to_owned();
    }
    let mut lines = body.lines();
    for line in lines.by_ref() {
        if !line.starts_with('>') {
            break;
        }
    }
    let rest: Vec<&str> = lines.collect();
    if rest.is_empty() {
        body.to_owned()
    } else {
        rest.join("\n")
    }
}

/// How many unread messages call for the account's attention: the
/// notifying unread messages of its joined rooms (the room list's badges)
/// and one per invitation. Spaces are left out, as the room list leaves
/// them out.
pub(crate) fn unread_total(client: &Client) -> u64 {
    client
        .rooms()
        .iter()
        .filter(|room| !room.is_space())
        .map(|room| match room.state() {
            RoomState::Joined => room.num_unread_notifications(),
            RoomState::Invited => 1,
            _ => 0,
        })
        .fold(0, u64::saturating_add)
}

#[cfg(test)]
mod tests {
    use matrix_sdk::ruma::serde::Raw;
    use serde_json::json;

    use super::*;

    fn event(content: serde_json::Value) -> AnySyncTimelineEvent {
        Raw::<AnySyncTimelineEvent>::from_json_string(
            json!({
                "type": "m.room.message",
                "event_id": "$event:smith.safechat.family",
                "sender": "@ana:smith.safechat.family",
                "origin_server_ts": 1_700_000_000_000_u64,
                "content": content,
            })
            .to_string(),
        )
        .expect("valid JSON")
        .deserialize()
        .expect("a valid event")
    }

    #[test]
    fn each_kind_of_message_is_named() {
        let cases = [
            (
                json!({"msgtype": "m.text", "body": "hello"}),
                NotificationContent::Text("hello".to_owned()),
            ),
            (
                json!({"msgtype": "m.notice", "body": "notice"}),
                NotificationContent::Text("notice".to_owned()),
            ),
            (
                json!({"msgtype": "m.emote", "body": "waves"}),
                NotificationContent::Emote("waves".to_owned()),
            ),
            (
                json!({"msgtype": "m.image", "body": "a.png", "url": "mxc://x/y"}),
                NotificationContent::Image,
            ),
            (
                json!({"msgtype": "m.video", "body": "a.mp4", "url": "mxc://x/y"}),
                NotificationContent::Video,
            ),
            (
                json!({"msgtype": "m.audio", "body": "a.ogg", "url": "mxc://x/y"}),
                NotificationContent::Audio,
            ),
            (
                json!({"msgtype": "m.file", "body": "a.pdf", "url": "mxc://x/y"}),
                NotificationContent::File,
            ),
            (
                json!({"msgtype": "m.location", "body": "here", "geo_uri": "geo:1,2"}),
                NotificationContent::Location,
            ),
            (
                json!({"msgtype": "org.example.custom", "body": "?"}),
                NotificationContent::Unknown,
            ),
        ];
        for (body, expected) in cases {
            assert_eq!(content(&event(body.clone())), expected, "{body}");
        }
    }

    #[test]
    fn an_edit_shows_its_new_text_and_a_reply_its_own() {
        let edit = event(json!({
            "msgtype": "m.text",
            "body": "* fixed",
            "m.new_content": {"msgtype": "m.text", "body": "fixed"},
            "m.relates_to": {"rel_type": "m.replace", "event_id": "$old:smith.safechat.family"},
        }));
        assert_eq!(
            content(&edit),
            NotificationContent::Text("fixed".to_owned())
        );
        let reply = event(json!({
            "msgtype": "m.text",
            "body": "> <@ben:smith.safechat.family> the question\n> second line\n\nthe answer",
        }));
        assert_eq!(
            content(&reply),
            NotificationContent::Text("the answer".to_owned())
        );
        assert_eq!(without_reply_fallback("> only a quote"), "> only a quote");
        assert_eq!(without_reply_fallback("plain"), "plain");
    }

    #[test]
    fn an_encrypted_event_is_a_new_message() {
        let encrypted = Raw::<AnySyncTimelineEvent>::from_json_string(
            json!({
                "type": "m.room.encrypted",
                "event_id": "$event:smith.safechat.family",
                "sender": "@ana:smith.safechat.family",
                "origin_server_ts": 1_700_000_000_000_u64,
                "content": {
                    "algorithm": "m.megolm.v1.aes-sha2",
                    "ciphertext": "AwgA",
                    "device_id": "DEVICE",
                    "sender_key": "key",
                    "session_id": "session",
                },
            })
            .to_string(),
        )
        .expect("valid JSON")
        .deserialize()
        .expect("a valid event");
        assert_eq!(content(&encrypted), NotificationContent::Unknown);
    }

    #[test]
    fn each_event_and_invitation_is_reported_once() {
        let mut reported = Reported::default();
        let id: OwnedEventId = "$a:smith.safechat.family".try_into().expect("an event id");
        assert!(reported.first_event(&id));
        assert!(!reported.first_event(&id));
        let room: OwnedRoomId = "!r:smith.safechat.family".try_into().expect("a room id");
        assert!(reported.first_invite(&room));
        assert!(!reported.first_invite(&room));
        for index in 0..REMEMBERED_EVENTS {
            let other: OwnedEventId = format!("$e{index}:smith.safechat.family")
                .try_into()
                .expect("an event id");
            reported.first_event(&other);
        }
        assert!(reported.first_event(&id), "only the latest are remembered");
    }

    #[test]
    fn the_feed_goes_to_the_latest_receiver_and_drops_when_full() {
        let feed = NotificationFeed::default();
        let incoming = |text: &str| IncomingNotification {
            account: AccountKey("a".to_owned()),
            room_id: "!r:smith.safechat.family".try_into().expect("a room id"),
            room_name: "Room".to_owned(),
            sender_name: "Ana".to_owned(),
            is_direct: false,
            content: NotificationContent::Text(text.to_owned()),
        };
        feed.send(incoming("nobody listens"));
        let mut first = feed.subscribe();
        feed.send(incoming("one"));
        assert_eq!(
            first.try_recv().map(|n| n.content),
            Ok(NotificationContent::Text("one".to_owned()))
        );
        let mut second = feed.subscribe();
        feed.send(incoming("two"));
        assert!(first.try_recv().is_err(), "the old receiver is replaced");
        assert!(second.try_recv().is_ok());
        for _ in 0..SUBSCRIPTION_BUFFER + 10 {
            feed.send(incoming("flood"));
        }
        let mut received = 0;
        while second.try_recv().is_ok() {
            received += 1;
        }
        assert_eq!(received, SUBSCRIPTION_BUFFER);
    }
}
