//! SDK types to `familychat-core` view models. Nothing here logs: message
//! text and room names stay in the view models, which never print them.

use familychat_core::{
    diff::ListDiff,
    media::{MediaHandle, MediaInfo},
    rich_text::RichText,
    rooms::{Encryption, Membership, RoomPreview, RoomSummary, UnreadCounts},
    timeline::{
        MembershipChange, MessageBody, MessageKind, Reaction, ReplyPreview, SendProblem, SendState,
        StateChange, TimelineContent, TimelineEvent, TimelineItem, TimelineItemId,
        TimelineItemKind, UtdReason,
    },
};
use familychat_security::{file_names::sanitize_filename, matrix_html::message_text};
use matrix_sdk::{
    EncryptionState, RoomState,
    ruma::{
        UInt, UserId,
        events::{
            StateEventContentChange,
            room::{
                MediaSource,
                message::{FormattedBody, MessageFormat, MessageType},
            },
        },
    },
};
use matrix_sdk_base::{crypto::types::events::UtdCause, store::QueueWedgeError};
use matrix_sdk_ui::{
    eyeball_im::VectorDiff,
    room_list_service::RoomListItem,
    timeline::{
        AnyOtherStateEventContentChange, EncryptedMessage, EventSendState, EventTimelineItem,
        InReplyToDetails, LatestEventValue, MembershipChange as SdkMembershipChange, Message,
        MsgLikeKind, Profile, ReactionsByKeyBySender, RoomExt, TimelineDetails,
        TimelineItemContent, VirtualTimelineItem,
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

/// A room list entry as a [`RoomSummary`]: what the room knows of itself,
/// its latest message as a preview and, for an invitation, who sent it
/// (both read from the local stores; nothing is fetched).
pub(crate) async fn room_summary(room: &RoomListItem) -> RoomSummary {
    let name = room
        .cached_display_name()
        .map(|name| name.to_string())
        .or_else(|| room.name())
        .or_else(|| room.canonical_alias().map(|alias| alias.to_string()))
        .unwrap_or_else(|| room.room_id().to_string());
    let membership = match room.state() {
        RoomState::Joined => Membership::Joined,
        RoomState::Invited => Membership::Invited,
        RoomState::Knocked => Membership::Knocked,
        RoomState::Left => Membership::Left,
        RoomState::Banned => Membership::Banned,
    };
    let invited_by = if membership == Membership::Invited {
        room.invite_details().await.ok().map(|invite| {
            invite
                .inviter
                .and_then(|inviter| inviter.display_name().map(str::to_owned))
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| invite.inviter_id.to_string())
        })
    } else {
        None
    };
    let preview = match RoomExt::latest_event(&**room).await {
        LatestEventValue::Remote {
            sender,
            is_own,
            profile,
            content,
            ..
        } => one_line(&content).map(|text| RoomPreview {
            sender: profile_name(&profile, &sender),
            is_own,
            text: MessageBody::new(text),
        }),
        LatestEventValue::Local {
            sender,
            profile,
            content,
            ..
        } => one_line(&content).map(|text| RoomPreview {
            sender: profile_name(&profile, &sender),
            is_own: true,
            text: MessageBody::new(text),
        }),
        _ => None,
    };
    RoomSummary {
        room_id: room.room_id().to_owned(),
        name,
        membership,
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
        topic: room.topic().filter(|topic| !topic.trim().is_empty()),
        joined_members: room.joined_members_count(),
        preview,
        invited_by,
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

/// A sender's name: their display name when the timeline has it, else
/// their user id.
fn profile_name(profile: &TimelineDetails<Profile>, sender: &UserId) -> String {
    match profile {
        TimelineDetails::Ready(profile) => profile
            .display_name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| sender.to_string()),
        _ => sender.to_string(),
    }
}

/// The most characters a one-line preview keeps.
const ONE_LINE_CHARS: usize = 140;

/// One line of an event, for a room list preview or a reply: the first
/// line of a message's text, or what it is ("Sent an image"). `None` for
/// events that are not messages.
pub(crate) fn one_line(content: &TimelineItemContent) -> Option<String> {
    let TimelineItemContent::MsgLike(msg_like) = content else {
        return None;
    };
    let text = match &msg_like.kind {
        MsgLikeKind::Message(message) => match message.msgtype() {
            MessageType::Image(_) => "Sent an image".to_owned(),
            MessageType::Video(_) => "Sent a video".to_owned(),
            MessageType::Audio(_) => "Sent an audio clip".to_owned(),
            MessageType::File(_) => "Sent a file".to_owned(),
            MessageType::Location(_) => "Shared a location".to_owned(),
            _ => first_line(message.body()),
        },
        MsgLikeKind::Sticker(_) => "Sent a sticker".to_owned(),
        MsgLikeKind::Redacted => "Message deleted".to_owned(),
        MsgLikeKind::UnableToDecrypt(_) => "Encrypted message".to_owned(),
        MsgLikeKind::Poll(_) => "Started a poll".to_owned(),
        MsgLikeKind::Other(_) | MsgLikeKind::LiveLocation(_) => return None,
    };
    Some(text)
}

/// The first non-empty line of `text`, cut to [`ONE_LINE_CHARS`].
fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let mut cut: String = line.chars().take(ONE_LINE_CHARS).collect();
    if line.chars().count() > ONE_LINE_CHARS {
        cut.push('…');
    }
    cut
}

/// A timeline item as a core [`TimelineItem`]. `own_user` is the account's
/// user id, for its own reactions.
pub(crate) fn timeline_item(
    item: &matrix_sdk_ui::timeline::TimelineItem,
    own_user: &UserId,
) -> TimelineItem {
    let kind = if let Some(event) = item.as_event() {
        TimelineItemKind::Event(timeline_event(event, own_user))
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

fn timeline_event(event: &EventTimelineItem, own_user: &UserId) -> TimelineEvent {
    let (in_reply_to, reactions) = match event.content() {
        TimelineItemContent::MsgLike(msg_like) => (
            msg_like.in_reply_to.as_ref().map(reply_preview),
            reactions(&msg_like.reactions, own_user),
        ),
        _ => (None, Vec::new()),
    };
    TimelineEvent {
        event_id: event.event_id().map(ToOwned::to_owned),
        sender: event.sender().to_owned(),
        sender_name: match event.sender_profile() {
            TimelineDetails::Ready(profile) => profile.display_name.clone(),
            _ => None,
        },
        timestamp_ms: u64::from(event.timestamp().get()),
        is_own: event.is_own(),
        content: content(event.content(), sender_html(event).as_deref()),
        send_state: event.send_state().map(|state| match state {
            EventSendState::NotSentYet { .. } => SendState::Sending,
            EventSendState::SendingFailed {
                error,
                is_recoverable,
            } => SendState::Failed {
                recoverable: *is_recoverable,
                problem: send_problem(error),
            },
            EventSendState::Sent { .. } => SendState::Sent,
        }),
        in_reply_to,
        reactions,
    }
}

/// Why the encryption stopped a send, when it did (ADR 0009).
fn send_problem(error: &matrix_sdk::Error) -> Option<SendProblem> {
    match QueueWedgeError::from(error) {
        QueueWedgeError::InsecureDevices { .. } => Some(SendProblem::UnconfirmedDevices),
        QueueWedgeError::IdentityViolations { .. } => Some(SendProblem::IdentityChanged),
        QueueWedgeError::CrossVerificationRequired => Some(SendProblem::ConfirmThisDevice),
        _ => None,
    }
}

/// Why this device can't read a message, from matrix-sdk-ui's cause (the
/// cause the unable-to-decrypt hook reports too).
pub(crate) fn utd_reason(encrypted: &EncryptedMessage) -> UtdReason {
    match encrypted {
        EncryptedMessage::MegolmV1AesSha2 { cause, .. } => utd_cause(*cause),
        _ => UtdReason::Waiting,
    }
}

/// The view model's reason for an SDK cause.
pub(crate) fn utd_cause(cause: UtdCause) -> UtdReason {
    match cause {
        UtdCause::SentBeforeWeJoined => UtdReason::SentBeforeJoined,
        UtdCause::VerificationViolation => UtdReason::SenderIdentityChanged,
        UtdCause::UnsignedDevice => UtdReason::UnconfirmedSender,
        UtdCause::UnknownDevice => UtdReason::UnknownSender,
        UtdCause::HistoricalMessageAndBackupIsDisabled => UtdReason::HistoryNotAvailable,
        UtdCause::HistoricalMessageAndDeviceIsUnverified => UtdReason::ConfirmForHistory,
        UtdCause::WithheldForUnverifiedOrInsecureDevice => UtdReason::ConfirmThisDevice,
        UtdCause::WithheldBySender => UtdReason::WithheldBySender,
        _ => UtdReason::Waiting,
    }
}

/// The message a reply answers, as far as the timeline has it.
fn reply_preview(details: &InReplyToDetails) -> ReplyPreview {
    let (sender, text) = match &details.event {
        TimelineDetails::Ready(embedded) => (
            Some(profile_name(&embedded.sender_profile, &embedded.sender)),
            one_line(&embedded.content).map(MessageBody::new),
        ),
        _ => (None, None),
    };
    ReplyPreview {
        event_id: details.event_id.clone(),
        sender,
        text,
    }
}

/// The reactions on a message, by key, in the order they were first given.
fn reactions(by_key: &ReactionsByKeyBySender, own_user: &UserId) -> Vec<Reaction> {
    by_key
        .iter()
        .filter(|(_, senders)| !senders.is_empty())
        .map(|(key, senders)| Reaction {
            key: key.clone(),
            count: senders.len(),
            own: senders.contains_key(own_user),
            senders: senders.keys().map(ToString::to_string).collect(),
        })
        .collect()
}

fn content(content: &TimelineItemContent, sender_html: Option<&str>) -> TimelineContent {
    match content {
        TimelineItemContent::MsgLike(msg_like) => match &msg_like.kind {
            MsgLikeKind::Message(message) => message_content(message, sender_html),
            MsgLikeKind::Sticker(sticker) => TimelineContent::Sticker {
                body: MessageBody::new(sticker.content().body.clone()),
            },
            MsgLikeKind::Redacted => TimelineContent::Redacted,
            MsgLikeKind::UnableToDecrypt(encrypted) => {
                TimelineContent::UnableToDecrypt(utd_reason(encrypted))
            }
            MsgLikeKind::Poll(_) => TimelineContent::Other {
                event_type: Some("m.poll.start".to_owned()),
            },
            MsgLikeKind::Other(_) | MsgLikeKind::LiveLocation(_) => {
                TimelineContent::Other { event_type: None }
            }
        },
        TimelineItemContent::MembershipChange(change) => TimelineContent::Membership {
            user_id: change.user_id().to_owned(),
            display_name: change.display_name().filter(|name| !name.trim().is_empty()),
            change: change
                .change()
                .map_or(MembershipChange::Other, membership_change),
        },
        TimelineItemContent::ProfileChange(change) => {
            let names = change.displayname_change();
            TimelineContent::ProfileChange {
                user_id: change.user_id().to_owned(),
                old_name: names.and_then(|names| names.old.clone()),
                new_name: names.and_then(|names| names.new.clone()),
                name_changed: names.is_some(),
                avatar_changed: change.avatar_url_change().is_some(),
            }
        }
        TimelineItemContent::OtherState(state) => TimelineContent::State {
            event_type: state.content().event_type().to_string(),
            change: state_change(state.content()),
        },
        other => TimelineContent::Other {
            event_type: other.event_type_str(),
        },
    }
}

/// The sender's own formatted body (the latest edit's, if any) from the
/// event as it arrived, when it is HTML.
///
/// matrix-sdk-ui's timeline already runs ruma-html over formatted bodies,
/// and ruma-html drops an element it doesn't allow but keeps its contents:
/// `<script>alert(1)</script>` reaches us as the text `alert(1)`, after
/// which our sanitiser can no longer drop the contents with the element
/// (ADR 0007). So the HTML we sanitise comes from the event itself, and
/// the SDK's copy is the fallback (a local echo has no JSON yet).
fn sender_html(event: &EventTimelineItem) -> Option<String> {
    let (json, edit) = match event.latest_edit_json() {
        Some(json) => (json, true),
        None => (event.original_json()?, false),
    };
    let content: serde_json::Value = json.get_field("content").ok().flatten()?;
    let content = if edit {
        content.get("m.new_content")?
    } else {
        &content
    };
    formatted_html(content)
}

/// A raw content object's `formatted_body`, when its format is HTML.
fn formatted_html(content: &serde_json::Value) -> Option<String> {
    let format = content.get("format")?.as_str()?;
    if format != MessageFormat::Html.as_str() {
        return None;
    }
    content
        .get("formatted_body")?
        .as_str()
        .map(ToOwned::to_owned)
}

/// The HTML to sanitise: the sender's own, else the SDK's formatted body.
fn drawn<'a>(
    sender_html: Option<&'a str>,
    formatted: Option<&'a FormattedBody>,
) -> Option<&'a str> {
    sender_html.or_else(|| html(formatted))
}

/// The formatted body to draw: only HTML (`org.matrix.custom.html`).
fn html(formatted: Option<&FormattedBody>) -> Option<&str> {
    formatted
        .filter(|formatted| formatted.format == MessageFormat::Html)
        .map(|formatted| formatted.body.as_str())
}

/// The sender's file name, made safe to suggest when saving.
fn safe_name(name: &str) -> String {
    sanitize_filename(name)
}

fn uint_u32(value: Option<UInt>) -> Option<u32> {
    value.and_then(|value| u32::try_from(u64::from(value)).ok())
}

/// A message's content: its plain body, its rich text (the sanitised
/// formatted body, or the plain body), and its media. A media message's
/// rich text is its caption only (empty without one): the file name is the
/// media's.
fn message_content(message: &Message, sender_html: Option<&str>) -> TimelineContent {
    let msgtype = message.msgtype();
    let body = message.body();
    let (rich, media) = match msgtype {
        MessageType::Text(text) => (
            message_text(body, drawn(sender_html, text.formatted.as_ref())),
            None,
        ),
        MessageType::Notice(notice) => (
            message_text(body, drawn(sender_html, notice.formatted.as_ref())),
            None,
        ),
        MessageType::Emote(emote) => (
            message_text(body, drawn(sender_html, emote.formatted.as_ref())),
            None,
        ),
        MessageType::Image(image) => {
            let info = image.info.as_deref();
            (
                caption(
                    image.caption(),
                    drawn(sender_html, image.formatted_caption()),
                ),
                Some(MediaInfo {
                    source: handle(&image.source),
                    thumbnail: info
                        .and_then(|info| info.thumbnail_source.as_ref())
                        .map(handle),
                    filename: safe_name(image.filename()),
                    mimetype: info.and_then(|info| info.mimetype.clone()),
                    size: info.and_then(|info| info.size).map(u64::from),
                    width: uint_u32(info.and_then(|info| info.width)),
                    height: uint_u32(info.and_then(|info| info.height)),
                }),
            )
        }
        MessageType::Video(video) => {
            let info = video.info.as_deref();
            (
                caption(
                    video.caption(),
                    drawn(sender_html, video.formatted_caption()),
                ),
                Some(MediaInfo {
                    source: handle(&video.source),
                    thumbnail: info
                        .and_then(|info| info.thumbnail_source.as_ref())
                        .map(handle),
                    filename: safe_name(video.filename()),
                    mimetype: info.and_then(|info| info.mimetype.clone()),
                    size: info.and_then(|info| info.size).map(u64::from),
                    width: uint_u32(info.and_then(|info| info.width)),
                    height: uint_u32(info.and_then(|info| info.height)),
                }),
            )
        }
        MessageType::Audio(audio) => {
            let info = audio.info.as_deref();
            (
                caption(
                    audio.caption(),
                    drawn(sender_html, audio.formatted_caption()),
                ),
                Some(MediaInfo {
                    source: handle(&audio.source),
                    thumbnail: None,
                    filename: safe_name(audio.filename()),
                    mimetype: info.and_then(|info| info.mimetype.clone()),
                    size: info.and_then(|info| info.size).map(u64::from),
                    width: None,
                    height: None,
                }),
            )
        }
        MessageType::File(file) => {
            let info = file.info.as_deref();
            (
                caption(file.caption(), drawn(sender_html, file.formatted_caption())),
                Some(MediaInfo {
                    source: handle(&file.source),
                    thumbnail: info
                        .and_then(|info| info.thumbnail_source.as_ref())
                        .map(handle),
                    filename: safe_name(file.filename()),
                    mimetype: info.and_then(|info| info.mimetype.clone()),
                    size: info.and_then(|info| info.size).map(u64::from),
                    width: None,
                    height: None,
                }),
            )
        }
        _ => (RichText::plain(body), None),
    };
    TimelineContent::Message {
        kind: message_kind(msgtype),
        body: MessageBody::new(body),
        rich,
        edited: message.is_edited(),
        media,
    }
}

/// A media message's caption as rich text (empty without one).
fn caption(plain: Option<&str>, formatted_html: Option<&str>) -> RichText {
    match plain {
        Some(plain) => message_text(plain, formatted_html),
        None => RichText::default(),
    }
}

fn membership_change(change: SdkMembershipChange) -> MembershipChange {
    match change {
        SdkMembershipChange::Joined => MembershipChange::Joined,
        SdkMembershipChange::Left => MembershipChange::Left,
        SdkMembershipChange::Invited => MembershipChange::Invited,
        SdkMembershipChange::InvitationAccepted => MembershipChange::InvitationAccepted,
        SdkMembershipChange::InvitationRejected => MembershipChange::InvitationRejected,
        SdkMembershipChange::InvitationRevoked => MembershipChange::InvitationRevoked,
        SdkMembershipChange::Kicked => MembershipChange::Kicked,
        SdkMembershipChange::Banned | SdkMembershipChange::KickedAndBanned => {
            MembershipChange::Banned
        }
        SdkMembershipChange::Unbanned => MembershipChange::Unbanned,
        SdkMembershipChange::Knocked => MembershipChange::Knocked,
        SdkMembershipChange::KnockAccepted => MembershipChange::KnockAccepted,
        SdkMembershipChange::KnockRetracted => MembershipChange::KnockRetracted,
        SdkMembershipChange::KnockDenied => MembershipChange::KnockDenied,
        SdkMembershipChange::None
        | SdkMembershipChange::Error
        | SdkMembershipChange::NotImplemented => MembershipChange::Other,
    }
}

fn state_change(change: &AnyOtherStateEventContentChange) -> StateChange {
    use AnyOtherStateEventContentChange as Any;
    match change {
        Any::RoomCreate(_) => StateChange::Created,
        Any::RoomName(name) => StateChange::Name(match name {
            StateEventContentChange::Original { content, .. } => {
                Some(content.name.clone()).filter(|name| !name.trim().is_empty())
            }
            StateEventContentChange::Redacted(_) => None,
        }),
        Any::RoomTopic(topic) => StateChange::Topic(match topic {
            StateEventContentChange::Original { content, .. } => {
                Some(content.topic.clone()).filter(|topic| !topic.trim().is_empty())
            }
            StateEventContentChange::Redacted(_) => None,
        }),
        Any::RoomAvatar(_) => StateChange::Avatar,
        Any::RoomEncryption(_) => StateChange::Encryption,
        Any::RoomJoinRules(_) => StateChange::JoinRules,
        Any::RoomHistoryVisibility(_) => StateChange::HistoryVisibility,
        Any::RoomPowerLevels(_) => StateChange::PowerLevels,
        Any::RoomPinnedEvents(_) => StateChange::PinnedEvents,
        Any::RoomTombstone(_) => StateChange::Tombstone,
        _ => StateChange::Other,
    }
}

/// The session's encoding of a media source, for a [`MediaHandle`]: its
/// JSON (the `mxc://` address, and for an encrypted file its key).
pub(crate) fn handle(source: &MediaSource) -> MediaHandle {
    MediaHandle::new(serde_json::to_string(source).unwrap_or_default())
}

/// The media source a [`MediaHandle`] encodes.
pub(crate) fn source(handle: &MediaHandle) -> Option<MediaSource> {
    serde_json::from_str(handle.encoded()).ok()
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
    use matrix_sdk::ruma::{
        events::room::{name::RoomNameEventContent, topic::RoomTopicEventContent},
        mxc_uri,
    };

    use super::*;

    #[test]
    fn only_html_formatted_bodies_are_drawn() {
        let html_body = FormattedBody::html("<b>hi</b>");
        assert_eq!(html(Some(&html_body)), Some("<b>hi</b>"));
        let other = FormattedBody {
            format: MessageFormat::from("org.example.markdown"),
            body: "**hi**".to_owned(),
        };
        assert_eq!(html(Some(&other)), None);
        assert_eq!(html(None), None);
    }

    #[test]
    fn the_senders_html_is_read_from_the_raw_content() {
        let content = serde_json::json!({
            "msgtype": "m.text",
            "body": "hi",
            "format": "org.matrix.custom.html",
            "formatted_body": "<b>hi</b><script>alert(1)</script>",
        });
        assert_eq!(
            formatted_html(&content).as_deref(),
            Some("<b>hi</b><script>alert(1)</script>")
        );
        let markdown =
            serde_json::json!({"format": "org.example.markdown", "formatted_body": "**hi**"});
        assert_eq!(formatted_html(&markdown), None);
        assert_eq!(formatted_html(&serde_json::json!({"body": "hi"})), None);
        // What reaches the window has the script and its text gone.
        let rich = message_text("hi", formatted_html(&content).as_deref());
        assert!(!rich.plain_text().contains("alert"));
        assert!(rich.plain_text().contains("hi"));
    }

    #[test]
    fn previews_are_one_short_line() {
        assert_eq!(first_line("\n  first line \nsecond"), "first line");
        let long = "x".repeat(ONE_LINE_CHARS + 10);
        let cut = first_line(&long);
        assert_eq!(cut.chars().count(), ONE_LINE_CHARS + 1);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn media_handles_round_trip_plain_and_encrypted_sources() {
        let json = |source: &MediaSource| serde_json::to_string(source).expect("serialises");
        let plain = MediaSource::Plain(mxc_uri!("mxc://hs.example/abc").to_owned());
        let handle = handle(&plain);
        assert_eq!(
            source(&handle).map(|found| json(&found)),
            Some(json(&plain))
        );
        assert!(!format!("{handle:?}").contains("abc"));

        let encrypted: MediaSource = serde_json::from_value(serde_json::json!({
            "file": {
                "url": "mxc://hs.example/secret",
                "key": {
                    "kty": "oct",
                    "key_ops": ["encrypt", "decrypt"],
                    "alg": "A256CTR",
                    "k": "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc",
                    "ext": true
                },
                "iv": "AQEBAQEBAQEBAQEBAQEBAQ",
                "hashes": { "sha256": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" },
                "v": "v2"
            }
        }))
        .expect("an encrypted source");
        assert!(matches!(encrypted, MediaSource::Encrypted(_)));
        let handle = super::handle(&encrypted);
        assert_eq!(
            source(&handle).map(|found| json(&found)),
            Some(json(&encrypted))
        );
        assert!(!format!("{handle:?}").contains("secret"));
        assert!(source(&MediaHandle::new("not json")).is_none());
    }

    #[test]
    fn state_changes_name_what_changed() {
        let name = AnyOtherStateEventContentChange::RoomName(StateEventContentChange::Original {
            content: RoomNameEventContent::new("Smiths".to_owned()),
            prev_content: None,
        });
        assert_eq!(
            state_change(&name),
            StateChange::Name(Some("Smiths".to_owned()))
        );
        let cleared =
            AnyOtherStateEventContentChange::RoomTopic(StateEventContentChange::Original {
                content: RoomTopicEventContent::new(" ".to_owned()),
                prev_content: None,
            });
        assert_eq!(state_change(&cleared), StateChange::Topic(None));
    }

    #[test]
    fn every_membership_change_has_a_core_kind() {
        assert_eq!(
            membership_change(SdkMembershipChange::KickedAndBanned),
            MembershipChange::Banned
        );
        assert_eq!(
            membership_change(SdkMembershipChange::InvitationAccepted),
            MembershipChange::InvitationAccepted
        );
        assert_eq!(
            membership_change(SdkMembershipChange::NotImplemented),
            MembershipChange::Other
        );
    }

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
