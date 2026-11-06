//! The session's half of the messaging actions (ADR 0012): carrying out
//! `familychat-core`'s [`RoomRequest`]s with matrix-sdk-ui's `Timeline`
//! and the SDK's send queue, new rooms, the server's upload limit and
//! well-known defaults.
//!
//! Every message, reaction, edit, deletion and file goes through the
//! room's send queue (through the open `Timeline` when the room is open,
//! so the local echo shows at once): the request returns as soon as it is
//! queued, and one that can't go because the server can't be reached waits
//! in the queue until the account's supervisor turns the queue back on
//! (`account`'s `supervise`). The rules (Markdown, the sanitiser's check,
//! the upload limit, location removal, the encryption default, nothing
//! while paused) are `familychat-actions`'; this module only does what it
//! is asked.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError, Weak},
    time::Duration,
};

use familychat_core::{
    messaging::{
        MediaLimits, MessageRef, NewRoom, OutgoingFile, OutgoingMessage, RoomDone, RoomRequest,
        ServerDefaults,
    },
    session::SessionError,
};
use matrix_sdk::{
    Client, EncryptionState, Room,
    attachment::{AttachmentConfig, AttachmentInfo, BaseFileInfo, BaseImageInfo},
    room::{
        Receipts,
        edit::EditedContent,
        reply::{EnforceThread, Reply},
    },
    ruma::{
        OwnedRoomId, UInt,
        api::client::room::create_room::v3::{Request as CreateRoomRequest, RoomPreset},
        events::{
            AnyMessageLikeEventContent, InitialStateEvent, Mentions,
            room::{
                encryption::{
                    PossiblyRedactedRoomEncryptionEventContent, RoomEncryptionEventContent,
                },
                message::{
                    AddMentions, RoomMessageEventContent, RoomMessageEventContentWithoutRelation,
                },
            },
        },
    },
};
use matrix_sdk_base::{
    RoomInfo, RoomInfoNotableUpdateReasons, crypto::LocalTrust, store::QueueWedgeError,
};
use matrix_sdk_ui::{
    Timeline,
    timeline::{EventSendState, RoomExt as _, TimelineEventItemId},
};
use tracing::{info, warn};

use crate::failure::Failure;

/// How long the well-known is waited for.
const WELL_KNOWN_TIMEOUT: Duration = Duration::from_secs(10);
/// The largest well-known read.
const WELL_KNOWN_MAX_BYTES: usize = 64 * 1024;

/// The timelines the frontends have open, by room, so an action in an
/// open room works on the timeline it is drawn from (its local echo shows
/// there at once, and its items can be found by their ids).
#[derive(Debug, Default, Clone)]
pub(crate) struct OpenTimelines(Arc<Mutex<HashMap<OwnedRoomId, Weak<Timeline>>>>);

impl OpenTimelines {
    pub(crate) fn insert(&self, room: OwnedRoomId, timeline: &Arc<Timeline>) {
        let mut open = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        open.retain(|_, timeline| timeline.strong_count() > 0);
        open.insert(room, Arc::downgrade(timeline));
    }

    fn get(&self, room: &OwnedRoomId) -> Option<Arc<Timeline>> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(room)
            .and_then(Weak::upgrade)
    }

    /// The room's open timeline, or a new one for this action.
    async fn for_room(&self, room: &Room) -> Result<Arc<Timeline>, SessionError> {
        if let Some(timeline) = self.get(&room.room_id().to_owned()) {
            return Ok(timeline);
        }
        room.timeline().await.map(Arc::new).map_err(|error| {
            info!(%error, "opening a timeline for an action failed");
            SessionError::Failed("The room couldn't be opened.".to_owned())
        })
    }
}

/// The session error for a failed SDK call: a pause and a refused size
/// are their own, anything else is `fallback`.
fn session_error(error: &matrix_sdk::Error, fallback: &str) -> SessionError {
    match Failure::from_sdk(error) {
        Failure::Suspended => SessionError::Paused,
        _ if is_too_large(error) => SessionError::TooLarge,
        _ => SessionError::Failed(fallback.to_owned()),
    }
}

fn is_too_large(error: &matrix_sdk::Error) -> bool {
    matches!(
        error.as_client_api_error().and_then(|api| api.error_kind()),
        Some(matrix_sdk::ruma::api::error::ErrorKind::TooLarge)
    )
}

/// The content of an outgoing message, with its mentions.
pub(crate) fn message_content(message: &OutgoingMessage) -> RoomMessageEventContentWithoutRelation {
    let mut content = match &message.html {
        Some(html) => RoomMessageEventContent::text_html(message.body.as_str(), html.as_str()),
        None => RoomMessageEventContent::text_plain(message.body.as_str()),
    };
    content.mentions = Some(Mentions::with_user_ids(message.mentions.iter().cloned()));
    content.into()
}

/// The timeline item a request is about.
async fn item_id(
    timeline: &Timeline,
    target: &MessageRef,
) -> Result<TimelineEventItemId, SessionError> {
    if let Some(event_id) = &target.event_id {
        return Ok(TimelineEventItemId::EventId(event_id.clone()));
    }
    timeline
        .items()
        .await
        .iter()
        .find(|item| item.unique_id().0 == target.item.0)
        .and_then(|item| item.as_event().map(|event| event.identifier()))
        .ok_or_else(|| SessionError::Failed("That message isn't in the room any more.".to_owned()))
}

/// Carries out a room request.
pub(crate) async fn room_action(
    room: Room,
    timelines: &OpenTimelines,
    request: RoomRequest,
) -> Result<RoomDone, SessionError> {
    if request_sends_event(&request) {
        confirm_encryption(&room).await;
    }
    match request {
        RoomRequest::Send { message, reply_to } => {
            let timeline = timelines.for_room(&room).await?;
            let content = message_content(&message);
            let sent = match reply_to {
                Some(event_id) => timeline.send_reply(content, event_id).await,
                None => {
                    timeline
                        .send(AnyMessageLikeEventContent::RoomMessage(
                            content.with_relation(None),
                        ))
                        .await
                }
            };
            sent.map(|_| RoomDone::Done).map_err(|error| {
                info!(%error, "queueing a message failed");
                SessionError::Failed("The message couldn't be sent. Try again.".to_owned())
            })
        }
        RoomRequest::Edit { target, message } => {
            let timeline = timelines.for_room(&room).await?;
            let item = item_id(&timeline, &target).await?;
            timeline
                .edit(&item, EditedContent::RoomMessage(message_content(&message)))
                .await
                .map(|()| RoomDone::Done)
                .map_err(|error| {
                    info!(%error, "editing a message failed");
                    SessionError::Failed("The message couldn't be changed. Try again.".to_owned())
                })
        }
        RoomRequest::ToggleReaction { target, key } => {
            let timeline = timelines.for_room(&room).await?;
            let item = item_id(&timeline, &target).await?;
            timeline
                .toggle_reaction(&item, &key)
                .await
                .map(RoomDone::Reaction)
                .map_err(|error| {
                    info!(%error, "a reaction failed");
                    SessionError::Failed("The reaction couldn't be sent. Try again.".to_owned())
                })
        }
        RoomRequest::Redact { target, reason } => {
            let timeline = timelines.for_room(&room).await?;
            let item = item_id(&timeline, &target).await?;
            timeline
                .redact(&item, reason.as_deref())
                .await
                .map(|()| RoomDone::Done)
                .map_err(|error| {
                    info!(%error, "deleting a message failed");
                    SessionError::Failed("The message couldn't be deleted. Try again.".to_owned())
                })
        }
        RoomRequest::MarkRead => {
            let timeline = timelines.for_room(&room).await?;
            let Some(latest) = timeline.latest_event_id().await else {
                return Ok(RoomDone::Done);
            };
            timeline
                .send_multiple_receipts(
                    Receipts::new()
                        .fully_read_marker(latest.clone())
                        .public_read_receipt(latest),
                )
                .await
                .map(|()| RoomDone::Done)
                .map_err(|error| session_error(&error, "The room couldn't be marked as read."))
        }
        RoomRequest::Typing(typing) => room
            .typing_notice(typing)
            .await
            .map(|()| RoomDone::Done)
            .map_err(|error| session_error(&error, "The typing notice couldn't be sent.")),
        RoomRequest::SendFile { file, reply_to } => send_file(&room, file, reply_to).await,
        RoomRequest::Invite(user) => room
            .invite_user_by_id(&user)
            .await
            .map(|()| RoomDone::Done)
            .map_err(|error| {
                info!(%error, "an invitation failed");
                session_error(&error, "The invitation couldn't be sent. Try again.")
            }),
        RoomRequest::AnswerInvite(accept) => {
            let answered = if accept {
                room.join().await
            } else {
                room.leave().await
            };
            answered.map(|()| RoomDone::Done).map_err(|error| {
                info!(%error, accept, "answering an invitation failed");
                session_error(
                    &error,
                    if accept {
                        "The invitation couldn't be accepted. Try again."
                    } else {
                        "The invitation couldn't be declined. Try again."
                    },
                )
            })
        }
        RoomRequest::Leave => room
            .leave()
            .await
            .map(|()| RoomDone::Done)
            .map_err(|error| {
                info!(%error, "leaving a room failed");
                session_error(&error, "Family Chat couldn't leave the room. Try again.")
            }),
        RoomRequest::SendAnyway(target) => {
            let timeline = timelines.for_room(&room).await?;
            send_anyway(&room, &timeline, &target).await
        }
    }
}

/// Whether `request` puts a message-like event in the send queue that
/// would be encrypted in an encrypted room: a message, an edit, a file, or
/// a stopped message sent anyway. (Reactions and redactions are never
/// encrypted by the SDK; receipts and typing notices aren't room events.)
fn request_sends_event(request: &RoomRequest) -> bool {
    matches!(
        request,
        RoomRequest::Send { .. }
            | RoomRequest::Edit { .. }
            | RoomRequest::SendFile { .. }
            | RoomRequest::SendAnyway(_)
    )
}

/// Makes sure a room the SDK takes as not encrypted really isn't before
/// anything is queued in it (ADR 0009, "A room that looks not encrypted").
///
/// matrix-sdk 0.19.1 marks a room's encryption state as known whenever a
/// sliding sync answer asked for `m.room.encryption`, whether or not the
/// answer carried it, and the send queue then encrypts only if it has
/// seen that event. A server can answer for a room before its
/// `m.room.encryption` exists (Palpo, while it is still making the room),
/// or never send the event again after an answer the client didn't get,
/// so "not encrypted" here is only the sync's guess. The guess is thrown
/// away and the server is asked (`GET .../state/m.room.encryption/`); an
/// encryption event it has sticks in the room's info, so the queue
/// encrypts. If the server can't be asked now (offline), the state stays
/// unknown, and the send queue asks the server itself before it sends
/// anything. Rooms the SDK already knows are encrypted cost nothing.
pub(crate) async fn confirm_encryption(room: &Room) {
    if !matches!(room.encryption_state(), EncryptionState::NotEncrypted) {
        return;
    }
    update_room_info(room, |info| info.mark_encryption_state_missing()).await;
    match room.request_encryption_state().await {
        Ok(()) => {
            let encrypted = room.encryption_state().is_encrypted();
            if encrypted {
                warn!(
                    "a room the sync took as not encrypted is encrypted on the server; \
                     its messages will be encrypted"
                );
            } else {
                info!("the server confirmed the room isn't encrypted");
            }
        }
        Err(error) => info!(
            %error,
            "the room's encryption couldn't be checked now; the send queue asks before sending"
        ),
    }
}

/// Changes the room's info, saved to the SDK's store; if the store can't
/// take it, the change is still made in memory so this session acts on it.
async fn update_room_info(room: &Room, change: impl Fn(&mut RoomInfo)) {
    let apply = |mut info: RoomInfo| {
        change(&mut info);
        (info, RoomInfoNotableUpdateReasons::empty())
    };
    if let Err(error) = room.update_and_save_room_info(apply).await {
        warn!(%error, "the room's info couldn't be saved; changing it in memory");
        room.update_room_info(apply).await;
    }
}

/// Sends a message the encryption stopped (ADR 0009), as Element X's
/// "Send anyway" does: a verified person's unconfirmed devices get the
/// room key too (their local trust is set to "ignored"), or the changed
/// identity's old verification is withdrawn; then the message is put back
/// in the queue.
async fn send_anyway(
    room: &Room,
    timeline: &Timeline,
    target: &MessageRef,
) -> Result<RoomDone, SessionError> {
    let not_there =
        || SessionError::Failed("That message isn't waiting to be sent any more.".to_owned());
    let items = timeline.items().await;
    let event = items
        .iter()
        .find(|item| item.unique_id().0 == target.item.0)
        .and_then(|item| item.as_event().cloned())
        .ok_or_else(not_there)?;
    let Some(EventSendState::SendingFailed { error, .. }) = event.send_state() else {
        return Err(not_there());
    };
    let failed =
        || SessionError::Failed("The message couldn't be sent anyway. Try again.".to_owned());
    let encryption = room.client().encryption();
    match QueueWedgeError::from(error.as_ref()) {
        QueueWedgeError::InsecureDevices { user_device_map } => {
            for (user_id, devices) in user_device_map {
                for device_id in devices {
                    if let Ok(Some(device)) = encryption.get_device(&user_id, &device_id).await {
                        device
                            .set_local_trust(LocalTrust::Ignored)
                            .await
                            .map_err(|error| {
                                warn!(%error, "a device's trust couldn't be set");
                                failed()
                            })?;
                    }
                }
            }
        }
        QueueWedgeError::IdentityViolations { users } => {
            for user_id in users {
                if let Ok(Some(identity)) = encryption.get_user_identity(&user_id).await {
                    let result = if identity.has_verification_violation() {
                        identity.withdraw_verification().await
                    } else {
                        identity.pin().await
                    };
                    result.map_err(|error| {
                        warn!(%error, "an identity change couldn't be acknowledged");
                        failed()
                    })?;
                }
            }
        }
        _ => return Err(failed()),
    }
    let handle = event.local_echo_send_handle().ok_or_else(not_there)?;
    handle.unwedge().await.map_err(|error| {
        info!(%error, "putting the message back in the queue failed");
        failed()
    })?;
    info!("sending a stopped message anyway");
    Ok(RoomDone::Done)
}

/// Queues a file: the send queue uploads it and then sends its event,
/// both retried when the connection comes back.
async fn send_file(
    room: &Room,
    file: OutgoingFile,
    reply_to: Option<matrix_sdk::ruma::OwnedEventId>,
) -> Result<RoomDone, SessionError> {
    let mime: mime::Mime = file
        .mime_type
        .parse()
        .unwrap_or(mime::APPLICATION_OCTET_STREAM);
    let size = UInt::try_from(file.data.len() as u64).ok();
    let info = if mime.type_() == mime::IMAGE {
        AttachmentInfo::Image(BaseImageInfo {
            width: file.image_size.map(|(width, _)| UInt::from(width)),
            height: file.image_size.map(|(_, height)| UInt::from(height)),
            size,
            blurhash: None,
            is_animated: None,
        })
    } else {
        AttachmentInfo::File(BaseFileInfo { size })
    };
    let mut config = AttachmentConfig::new().info(info);
    config.reply = reply_to.map(|event_id| Reply {
        event_id,
        enforce_thread: EnforceThread::MaybeThreaded,
        add_mentions: AddMentions::Yes,
    });
    room.send_queue()
        .send_attachment(file.file_name.clone(), mime, file.data.to_vec(), config)
        .await
        .map(|_| RoomDone::Done)
        .map_err(|error| {
            info!(%error, "queueing a file failed");
            SessionError::Failed("The file couldn't be sent. Try again.".to_owned())
        })
}

/// Makes a room or direct chat. A room made encrypted is handed back only
/// once the SDK's room info holds its `m.room.encryption` (the content
/// this request put in the initial state), so nothing sent into it can go
/// out in plaintext, whatever the sync says meanwhile: a sync answer
/// without the event never removes one the room has (ADR 0009).
pub(crate) async fn create_room(
    client: &Client,
    room: NewRoom,
) -> Result<OwnedRoomId, SessionError> {
    let encryption = room
        .encrypted
        .then(RoomEncryptionEventContent::with_recommended_defaults);
    let mut request = CreateRoomRequest::new();
    request.name = room.name;
    request.invite = room.invite;
    request.is_direct = room.direct;
    request.preset = Some(if room.direct {
        RoomPreset::TrustedPrivateChat
    } else {
        RoomPreset::PrivateChat
    });
    if let Some(content) = &encryption {
        request.initial_state =
            vec![InitialStateEvent::with_empty_state_key(content.clone()).to_raw_any()];
    }
    let created = client.create_room(request).await.map_err(|error| {
        info!(%error, "making a room failed");
        session_error(&error, "The room couldn't be made. Try again.")
    })?;
    if let Some(content) = encryption {
        match possibly_redacted(&content) {
            Some(content) => {
                update_room_info(&created, |info| {
                    info.set_encryption_event(Some(content.clone()));
                    info.mark_encryption_state_synced();
                })
                .await;
            }
            // Never expected; the guard before every send still asks the
            // server.
            None => warn!("the new room's encryption couldn't be recorded"),
        }
    }
    Ok(created.room_id().to_owned())
}

/// The form of an `m.room.encryption` content the SDK's room info keeps
/// (the same JSON; the type is non-exhaustive, so it is read back).
fn possibly_redacted(
    content: &RoomEncryptionEventContent,
) -> Option<PossiblyRedactedRoomEncryptionEventContent> {
    serde_json::to_value(content)
        .and_then(serde_json::from_value)
        .ok()
}

/// The server's upload limit (`/_matrix/client/v1/media/config`), which the
/// SDK fetches once and keeps.
pub(crate) async fn media_limits(client: &Client) -> Result<MediaLimits, SessionError> {
    client
        .load_or_fetch_max_upload_size()
        .await
        .map(|size| MediaLimits {
            max_upload_bytes: Some(u64::from(size)),
        })
        .map_err(|error| {
            info!(%error, "the server's upload limit couldn't be read");
            session_error(&error, "The server's upload limit couldn't be read.")
        })
}

/// Where the account's server publishes its well-known: the server name's
/// own origin, or the homeserver's when they are the same host.
pub(crate) fn well_known_url(client: &Client) -> Option<reqwest::Url> {
    let server = client.user_id()?.server_name();
    let homeserver = client.homeserver();
    let homeserver_authority = match homeserver.port() {
        Some(port) => format!("{}:{port}", homeserver.host_str()?),
        None => homeserver.host_str()?.to_owned(),
    };
    let base = if homeserver_authority.eq_ignore_ascii_case(server.as_str()) {
        homeserver.clone()
    } else {
        reqwest::Url::parse(&format!("https://{server}/")).ok()?
    };
    base.join("/.well-known/matrix/client").ok()
}

/// `io.element.e2ee.default` in a well-known document.
pub(crate) fn encryption_default(well_known: &serde_json::Value) -> Option<bool> {
    well_known.get("io.element.e2ee")?.get("default")?.as_bool()
}

/// What the server's well-known says about new rooms. A well-known that
/// can't be read says nothing, so new rooms stay encrypted.
pub(crate) async fn server_defaults(client: &Client) -> ServerDefaults {
    let Some(url) = well_known_url(client) else {
        return ServerDefaults::default();
    };
    let fetched = async {
        let response = client
            .http_client()
            .get(url)
            .timeout(WELL_KNOWN_TIMEOUT)
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let bytes = response.bytes().await.ok()?;
        if bytes.len() > WELL_KNOWN_MAX_BYTES {
            return None;
        }
        serde_json::from_slice::<serde_json::Value>(&bytes).ok()
    }
    .await;
    ServerDefaults {
        encryption_default: fetched.as_ref().and_then(encryption_default),
    }
}

#[cfg(test)]
mod tests {
    use familychat_core::timeline::MessageBody;
    use matrix_sdk::ruma::{OwnedUserId, events::room::message::MessageType};

    use super::*;

    #[test]
    fn the_encryption_default_is_read_only_when_the_server_says_it() {
        let off = serde_json::json!({"m.homeserver": {"base_url": "https://hs"}, "io.element.e2ee": {"default": false}});
        let on = serde_json::json!({"io.element.e2ee": {"default": true}});
        let silent = serde_json::json!({"m.homeserver": {"base_url": "https://hs"}});
        let odd = serde_json::json!({"io.element.e2ee": {"default": "no"}});
        assert_eq!(encryption_default(&off), Some(false));
        assert_eq!(encryption_default(&on), Some(true));
        assert_eq!(encryption_default(&silent), None);
        assert_eq!(encryption_default(&odd), None);
    }

    #[test]
    fn messages_carry_their_html_and_mentions() {
        let mum = OwnedUserId::try_from("@mum:smith.safechat.family").expect("an id");
        let message = OutgoingMessage {
            body: MessageBody::new("Mum: dinner"),
            html: Some(MessageBody::new(
                "<a href=\"https://matrix.to/#/@mum:smith.safechat.family\">Mum</a>: dinner",
            )),
            mentions: vec![mum.clone()],
        };
        let content = message_content(&message);
        let MessageType::Text(text) = &content.msgtype else {
            panic!("a text message");
        };
        assert_eq!(text.body, "Mum: dinner");
        assert!(
            text.formatted
                .as_ref()
                .is_some_and(|formatted| formatted.body.contains("matrix.to"))
        );
        assert!(
            content
                .mentions
                .as_ref()
                .is_some_and(|mentions| mentions.user_ids.contains(&mum))
        );
        let plain = message_content(&OutgoingMessage::plain("hi"));
        let MessageType::Text(text) = &plain.msgtype else {
            panic!("a text message");
        };
        assert!(text.formatted.is_none());
    }

    // --- A room's encryption before anything is sent (ADR 0009) ---------

    use matrix_sdk::{SessionMeta, SessionTokens, authentication::matrix::MatrixSession};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, path_regex},
    };

    const ROOM: &str = "!fresh:mock.test";

    /// A signed-in client of a mocked server (in-memory stores), which
    /// answers `createRoom` with [`ROOM`].
    async fn mocked_client(server: &MockServer) -> Client {
        Mock::given(method("GET"))
            .and(path("/_matrix/client/versions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"versions": ["v1.11"]})),
            )
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/_matrix/client/v3/createRoom"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"room_id": ROOM})),
            )
            .mount(server)
            .await;
        let http = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("a plain client");
        let client = Client::builder()
            .homeserver_url(server.uri())
            .http_client(http)
            .build()
            .await
            .expect("a client");
        client
            .restore_session(MatrixSession {
                meta: SessionMeta {
                    user_id: "@alice:mock.test".try_into().expect("a user id"),
                    device_id: "MOCKDEVICE".into(),
                },
                tokens: SessionTokens {
                    access_token: "access".to_owned(),
                    refresh_token: None,
                },
            })
            .await
            .expect("a session");
        client
    }

    fn new_room(encrypted: bool) -> NewRoom {
        NewRoom {
            name: Some("Fresh".to_owned()),
            invite: Vec::new(),
            direct: false,
            encrypted,
        }
    }

    /// What a sliding sync answer that asked for `m.room.encryption` and
    /// didn't get it does to the room (matrix-sdk-base's
    /// `RoomInfo::handle_encryption_state`): the state counts as known.
    async fn sync_answer_without_the_event(room: &Room) {
        update_room_info(room, |info| info.mark_encryption_state_synced()).await;
    }

    /// The `GET .../state/m.room.encryption/` the guard makes.
    fn encryption_state_request() -> wiremock::MockBuilder {
        Mock::given(method("GET")).and(path_regex(r"/state/m\.room\.encryption/?$"))
    }

    #[tokio::test]
    async fn a_room_made_encrypted_is_encrypted_before_any_sync_and_stays_so() {
        let server = MockServer::start().await;
        let client = mocked_client(&server).await;
        let made = create_room(&client, new_room(true)).await.expect("made");
        let room = client.get_room(&made).expect("the room");
        assert!(
            room.encryption_state().is_encrypted(),
            "the room is encrypted as it is handed back: {:?}",
            room.encryption_state()
        );
        sync_answer_without_the_event(&room).await;
        assert!(
            room.encryption_state().is_encrypted(),
            "a sync answer without the event leaves it encrypted"
        );
        // The guard has nothing to ask about.
        confirm_encryption(&room).await;
        let asked = server.received_requests().await.unwrap_or_default();
        assert!(
            !asked
                .iter()
                .any(|request| request.url.path().contains("m.room.encryption")),
            "an encrypted room costs no request"
        );
        let create = asked
            .iter()
            .find(|request| request.url.path().ends_with("/createRoom"))
            .expect("the createRoom request");
        let body: serde_json::Value = serde_json::from_slice(&create.body).expect("JSON");
        assert_eq!(body["initial_state"][0]["type"], "m.room.encryption");
    }

    #[tokio::test]
    async fn a_room_the_sync_took_as_not_encrypted_is_asked_about_before_a_send() {
        let server = MockServer::start().await;
        let client = mocked_client(&server).await;
        encryption_state_request()
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"algorithm": "m.megolm.v1.aes-sha2"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        // A room this client didn't make encrypted, which the sync then
        // took as not encrypted while the server has its encryption.
        let made = create_room(&client, new_room(false)).await.expect("made");
        let room = client.get_room(&made).expect("the room");
        sync_answer_without_the_event(&room).await;
        assert!(matches!(
            room.encryption_state(),
            EncryptionState::NotEncrypted
        ));
        confirm_encryption(&room).await;
        assert!(
            room.encryption_state().is_encrypted(),
            "the server's answer makes the room encrypted: {:?}",
            room.encryption_state()
        );
        sync_answer_without_the_event(&room).await;
        assert!(room.encryption_state().is_encrypted(), "and it sticks");
    }

    #[tokio::test]
    async fn a_room_the_server_says_is_not_encrypted_stays_so() {
        let server = MockServer::start().await;
        let client = mocked_client(&server).await;
        encryption_state_request()
            .respond_with(ResponseTemplate::new(404).set_body_json(
                serde_json::json!({"errcode": "M_NOT_FOUND", "error": "Event not found."}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let made = create_room(&client, new_room(false)).await.expect("made");
        let room = client.get_room(&made).expect("the room");
        sync_answer_without_the_event(&room).await;
        confirm_encryption(&room).await;
        assert!(matches!(
            room.encryption_state(),
            EncryptionState::NotEncrypted
        ));
    }

    #[tokio::test]
    async fn a_room_whose_encryption_cant_be_checked_is_left_for_the_queue_to_ask() {
        let server = MockServer::start().await;
        let client = mocked_client(&server).await;
        encryption_state_request()
            .respond_with(
                ResponseTemplate::new(403).set_body_json(
                    serde_json::json!({"errcode": "M_FORBIDDEN", "error": "Not now."}),
                ),
            )
            .mount(&server)
            .await;
        let made = create_room(&client, new_room(false)).await.expect("made");
        let room = client.get_room(&made).expect("the room");
        sync_answer_without_the_event(&room).await;
        confirm_encryption(&room).await;
        // Unknown: the send queue's `latest_encryption_state` asks the
        // server again before it sends, rather than sending in plaintext.
        assert!(
            room.encryption_state().is_unknown(),
            "{:?}",
            room.encryption_state()
        );
    }
}
