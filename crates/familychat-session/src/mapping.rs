//! SDK types to `familychat-core` view models. Nothing here logs: message
//! text and room names stay in the view models, which never print them.

use familychat_core::{
    diff::ListDiff,
    rooms::{Encryption, Membership, RoomSummary, UnreadCounts},
    timeline::{
        MessageBody, MessageKind, SendState, TimelineContent, TimelineEvent, TimelineItem,
        TimelineItemId, TimelineItemKind,
    },
};
use matrix_sdk::{EncryptionState, RoomState, ruma::events::room::message::MessageType};
use matrix_sdk_ui::{
    eyeball_im::VectorDiff,
    room_list_service::RoomListItem,
    timeline::{
        EventSendState, EventTimelineItem, MsgLikeKind, TimelineDetails, TimelineItemContent,
        VirtualTimelineItem,
    },
};

/// An SDK list change as a core [`ListDiff`].
pub(crate) fn list_diff<T, U>(diff: VectorDiff<T>, mut convert: impl FnMut(T) -> U) -> ListDiff<U>
where
    T: Clone,
{
    match diff {
        VectorDiff::Append { values } => ListDiff::Append {
            values: values.into_iter().map(&mut convert).collect(),
        },
        VectorDiff::Clear => ListDiff::Clear,
        VectorDiff::PushFront { value } => ListDiff::PushFront {
            value: convert(value),
        },
        VectorDiff::PushBack { value } => ListDiff::PushBack {
            value: convert(value),
        },
        VectorDiff::PopFront => ListDiff::PopFront,
        VectorDiff::PopBack => ListDiff::PopBack,
        VectorDiff::Insert { index, value } => ListDiff::Insert {
            index,
            value: convert(value),
        },
        VectorDiff::Set { index, value } => ListDiff::Set {
            index,
            value: convert(value),
        },
        VectorDiff::Remove { index } => ListDiff::Remove { index },
        VectorDiff::Truncate { length } => ListDiff::Truncate { length },
        VectorDiff::Reset { values } => ListDiff::Reset {
            values: values.into_iter().map(convert).collect(),
        },
    }
}

/// A room list entry as a [`RoomSummary`].
pub(crate) fn room_summary(room: &RoomListItem) -> RoomSummary {
    let name = room
        .cached_display_name()
        .map(|name| name.to_string())
        .or_else(|| room.name())
        .or_else(|| room.canonical_alias().map(|alias| alias.to_string()))
        .unwrap_or_else(|| room.room_id().to_string());
    RoomSummary {
        room_id: room.room_id().to_owned(),
        name,
        membership: match room.state() {
            RoomState::Joined => Membership::Joined,
            RoomState::Invited => Membership::Invited,
            RoomState::Knocked => Membership::Knocked,
            RoomState::Left => Membership::Left,
            RoomState::Banned => Membership::Banned,
        },
        is_direct: room.is_dm(),
        encryption: encryption(room.encryption_state()),
        unread: UnreadCounts {
            messages: room.num_unread_messages(),
            notifications: room.num_unread_notifications(),
            mentions: room.num_unread_mentions(),
            marked_unread: room.is_marked_unread(),
        },
        last_activity_ms: room
            .latest_event_timestamp()
            .map(|timestamp| u64::from(timestamp.get())),
    }
}

fn encryption(state: EncryptionState) -> Encryption {
    if state.is_encrypted() {
        Encryption::Encrypted
    } else if state.is_unknown() {
        Encryption::Unknown
    } else {
        Encryption::NotEncrypted
    }
}

/// A timeline item as a core [`TimelineItem`].
pub(crate) fn timeline_item(item: &matrix_sdk_ui::timeline::TimelineItem) -> TimelineItem {
    let kind = if let Some(event) = item.as_event() {
        TimelineItemKind::Event(timeline_event(event))
    } else {
        match item.as_virtual() {
            Some(VirtualTimelineItem::DateDivider(timestamp)) => TimelineItemKind::DateDivider {
                timestamp_ms: u64::from(timestamp.get()),
            },
            Some(VirtualTimelineItem::ReadMarker) => TimelineItemKind::ReadMarker,
            Some(VirtualTimelineItem::TimelineStart) | None => TimelineItemKind::TimelineStart,
        }
    };
    TimelineItem {
        id: TimelineItemId(item.unique_id().0.clone()),
        kind,
    }
}

fn timeline_event(event: &EventTimelineItem) -> TimelineEvent {
    TimelineEvent {
        event_id: event.event_id().map(ToOwned::to_owned),
        sender: event.sender().to_owned(),
        sender_name: match event.sender_profile() {
            TimelineDetails::Ready(profile) => profile.display_name.clone(),
            _ => None,
        },
        timestamp_ms: u64::from(event.timestamp().get()),
        is_own: event.is_own(),
        content: content(event.content()),
        send_state: event.send_state().map(|state| match state {
            EventSendState::NotSentYet { .. } => SendState::Sending,
            EventSendState::SendingFailed { is_recoverable, .. } => SendState::Failed {
                recoverable: *is_recoverable,
            },
            EventSendState::Sent { .. } => SendState::Sent,
        }),
    }
}

fn content(content: &TimelineItemContent) -> TimelineContent {
    match content {
        TimelineItemContent::MsgLike(msg_like) => match &msg_like.kind {
            MsgLikeKind::Message(message) => TimelineContent::Message {
                kind: message_kind(message.msgtype()),
                body: MessageBody::new(message.body()),
                edited: message.is_edited(),
            },
            MsgLikeKind::Sticker(sticker) => TimelineContent::Sticker {
                body: MessageBody::new(sticker.content().body.clone()),
            },
            MsgLikeKind::Redacted => TimelineContent::Redacted,
            MsgLikeKind::UnableToDecrypt(_) => TimelineContent::UnableToDecrypt,
            MsgLikeKind::Poll(_) => TimelineContent::Other {
                event_type: Some("m.poll.start".to_owned()),
            },
            MsgLikeKind::Other(_) | MsgLikeKind::LiveLocation(_) => {
                TimelineContent::Other { event_type: None }
            }
        },
        TimelineItemContent::MembershipChange(change) => TimelineContent::Membership {
            user_id: change.user_id().to_owned(),
        },
        TimelineItemContent::ProfileChange(change) => TimelineContent::ProfileChange {
            user_id: change.user_id().to_owned(),
        },
        TimelineItemContent::OtherState(state) => TimelineContent::State {
            event_type: state.content().event_type().to_string(),
        },
        other => TimelineContent::Other {
            event_type: other.event_type_str(),
        },
    }
}

fn message_kind(msgtype: &MessageType) -> MessageKind {
    match msgtype {
        MessageType::Text(_) => MessageKind::Text,
        MessageType::Notice(_) | MessageType::ServerNotice(_) => MessageKind::Notice,
        MessageType::Emote(_) => MessageKind::Emote,
        MessageType::Image(_) => MessageKind::Image,
        MessageType::Video(_) => MessageKind::Video,
        MessageType::Audio(_) => MessageKind::Audio,
        MessageType::File(_) => MessageKind::File,
        MessageType::Location(_) => MessageKind::Location,
        _ => MessageKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_changes_keep_their_positions() {
        let diff = list_diff(VectorDiff::Insert { index: 2, value: 7 }, |value: i32| {
            value * 2
        });
        assert_eq!(
            diff,
            ListDiff::Insert {
                index: 2,
                value: 14
            }
        );
        let diff = list_diff(VectorDiff::Truncate { length: 1 }, |value: i32| value);
        assert_eq!(diff, ListDiff::Truncate { length: 1 });
        let diff = list_diff(
            VectorDiff::Reset {
                values: vec![1, 2].into(),
            },
            |value: i32| value + 1,
        );
        assert_eq!(diff, ListDiff::Reset { values: vec![2, 3] });
    }
}
