//! [`LiveSession`]: the [`SessionApi`] the frontends use, over the
//! [`AccountManager`]. Each call spawns its work on the background runtime
//! and returns the channel its answer arrives on.

use std::{path::PathBuf, sync::Arc};

use familychat_core::{
    encryption::{
        AuthOutcome, DeviceView, EncryptionStatus, IdentityWarning, KeyImport, SecretText,
        VerificationFlow,
    },
    media::{DecodedImage, MediaError, MediaInfo},
    messaging::{Draft, MediaLimits, Mention, NewRoom, RoomDone, RoomRequest, ServerDefaults},
    notifications::IncomingNotification,
    rooms::{MemberRole, Membership, RoomListFilter, RoomMember, sort_members},
    session::{
        AccountKey, AccountView, MediaReply, Reply, RoomListSubscription, SessionApi, SessionError,
        TimelineSubscription,
    },
    sync::AccountStatus,
    timeline::MessageBody,
};
use matrix_sdk::{
    Client, RoomMemberships,
    ruma::{
        OwnedEventId, OwnedRoomId, OwnedUserId, RoomId, UserId,
        events::room::{member::MembershipState, power_levels::UserPowerLevel},
    },
};
use serde_json::{Value, json};
use tokio::{
    runtime::Handle,
    sync::{mpsc, oneshot, watch},
};

use tracing::info;

use crate::{
    account::Ready, encryption, manager::AccountManager, media, messaging, room_list, timeline,
};

/// A draft as the app store keeps it (encrypted, ADR 0003).
fn draft_json(draft: &Draft) -> Value {
    json!({
        "text": draft.text.as_str(),
        "mentions": draft.mentions.iter().map(|mention| json!({
            "user_id": mention.user_id.as_str(),
            "label": mention.label,
        })).collect::<Vec<_>>(),
        "reply_to": draft.reply_to.as_ref().map(|event| event.as_str()),
        "editing": draft.editing.as_ref().map(|event| event.as_str()),
    })
}

/// A draft from the app store; anything unreadable in it is left out.
fn draft_from_json(value: &Value) -> Option<Draft> {
    let event = |field: &str| {
        value
            .get(field)
            .and_then(Value::as_str)
            .and_then(|id| OwnedEventId::try_from(id).ok())
    };
    Some(Draft {
        text: MessageBody::new(value.get("text")?.as_str()?),
        mentions: value
            .get("mentions")
            .and_then(Value::as_array)
            .map(|mentions| {
                mentions
                    .iter()
                    .filter_map(|mention| {
                        Some(Mention {
                            user_id: OwnedUserId::try_from(mention.get("user_id")?.as_str()?)
                                .ok()?,
                            label: mention.get("label")?.as_str()?.to_owned(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        reply_to: event("reply_to"),
        editing: event("editing"),
    })
}

/// The live sessions of one profile.
#[derive(Debug, Clone)]
pub struct LiveSession {
    manager: Arc<AccountManager>,
    handle: Handle,
}

impl LiveSession {
    /// The session over `manager`, doing its work on `handle` (the
    /// [`BackgroundRuntime`](crate::BackgroundRuntime)'s).
    pub fn new(manager: Arc<AccountManager>, handle: Handle) -> Self {
        Self { manager, handle }
    }

    /// The account manager.
    pub fn manager(&self) -> &Arc<AccountManager> {
        &self.manager
    }

    fn ready(&self, account: &AccountKey) -> Result<Ready, SessionError> {
        self.manager
            .account_by_key(account)
            .ok_or(SessionError::UnknownAccount)?
            .ready()
            .ok_or(SessionError::NotReady)
    }

    /// Spawns `work` with the account's open client and answers with its
    /// result.
    fn reply<T, F>(
        &self,
        account: &AccountKey,
        work: impl FnOnce(Ready) -> F + Send + 'static,
    ) -> Reply<T>
    where
        T: Send + 'static,
        F: Future<Output = Result<T, SessionError>> + Send + 'static,
    {
        let (answer, reply) = oneshot::channel();
        match self.ready(account) {
            Ok(ready) => {
                self.handle.spawn(async move {
                    let _ = answer.send(work(ready).await);
                });
            }
            Err(error) => {
                let _ = answer.send(Err(error));
            }
        }
        reply
    }
}

impl LiveSession {
    /// Runs `work` (the app store: disk and crypto) on the runtime's
    /// blocking pool and answers with its result.
    fn blocking_reply<T>(
        &self,
        account: &AccountKey,
        work: impl FnOnce() -> Result<T, SessionError> + Send + 'static,
    ) -> Reply<T>
    where
        T: Send + 'static,
    {
        let (answer, reply) = oneshot::channel();
        if self.manager.account_by_key(account).is_none() {
            let _ = answer.send(Err(SessionError::UnknownAccount));
            return reply;
        }
        self.handle.spawn_blocking(move || {
            let _ = answer.send(work());
        });
        reply
    }

    /// Spawns media work with the account's client and answers with its
    /// result; an account that isn't open can't fetch anything.
    fn media_reply<T, F>(
        &self,
        account: &AccountKey,
        work: impl FnOnce(Client) -> F + Send + 'static,
    ) -> MediaReply<T>
    where
        T: Send + 'static,
        F: Future<Output = Result<T, MediaError>> + Send + 'static,
    {
        let (answer, reply) = oneshot::channel();
        match self.ready(account) {
            Ok(ready) => {
                self.handle.spawn(async move {
                    let _ = answer.send(work(ready.client).await);
                });
            }
            Err(_) => {
                let _ = answer.send(Err(MediaError::Unavailable));
            }
        }
        reply
    }
}

impl SessionApi for LiveSession {
    fn accounts(&self) -> watch::Receiver<Vec<AccountView>> {
        self.manager.accounts()
    }

    fn account_status(&self, account: &AccountKey) -> Option<watch::Receiver<AccountStatus>> {
        self.manager
            .account_by_key(account)
            .map(|account| account.status())
    }

    fn room_list(
        &self,
        account: &AccountKey,
        filter: RoomListFilter,
    ) -> Reply<RoomListSubscription> {
        self.reply(account, move |ready| async move {
            room_list::open(ready.sync.room_list_service(), filter).await
        })
    }

    fn timeline(&self, account: &AccountKey, room: &RoomId) -> Reply<TimelineSubscription> {
        let room: OwnedRoomId = room.to_owned();
        self.reply(account, move |ready| async move {
            let room = ready
                .client
                .get_room(&room)
                .ok_or(SessionError::UnknownRoom)?;
            timeline::open(
                room,
                ready.sync.room_list_service(),
                ready.timelines.clone(),
                ready.utd_hook.clone(),
            )
            .await
        })
    }

    fn members(&self, account: &AccountKey, room: &RoomId) -> Reply<Vec<RoomMember>> {
        let room: OwnedRoomId = room.to_owned();
        self.reply(account, move |ready| async move {
            let room = ready
                .client
                .get_room(&room)
                .ok_or(SessionError::UnknownRoom)?;
            let own = room.own_user_id().to_owned();
            let members = room
                .members(RoomMemberships::JOIN | RoomMemberships::INVITE)
                .await
                .map_err(|error| {
                    info!(%error, "loading a room's members failed");
                    SessionError::Failed("The members couldn't be loaded. Try again.".to_owned())
                })?;
            let mut members: Vec<RoomMember> = members
                .iter()
                .map(|member| RoomMember {
                    user_id: member.user_id().to_owned(),
                    display_name: member
                        .display_name()
                        .map(str::to_owned)
                        .filter(|name| !name.trim().is_empty()),
                    membership: match member.membership() {
                        MembershipState::Invite => Membership::Invited,
                        MembershipState::Knock => Membership::Knocked,
                        MembershipState::Leave => Membership::Left,
                        MembershipState::Ban => Membership::Banned,
                        _ => Membership::Joined,
                    },
                    role: match member.power_level() {
                        UserPowerLevel::Infinite => MemberRole::Admin,
                        UserPowerLevel::Int(level) => MemberRole::from_power_level(level.into()),
                        _ => MemberRole::Member,
                    },
                    is_own: member.user_id() == own,
                })
                .collect();
            sort_members(&mut members);
            Ok(members)
        })
    }

    fn room_action(
        &self,
        account: &AccountKey,
        room: &RoomId,
        request: RoomRequest,
    ) -> Reply<RoomDone> {
        let room: OwnedRoomId = room.to_owned();
        self.reply(account, move |ready| async move {
            let room = ready
                .client
                .get_room(&room)
                .ok_or(SessionError::UnknownRoom)?;
            messaging::room_action(room, &ready.timelines, request).await
        })
    }

    fn create_room(&self, account: &AccountKey, room: NewRoom) -> Reply<OwnedRoomId> {
        self.reply(account, move |ready| async move {
            messaging::create_room(&ready.client, room).await
        })
    }

    fn media_limits(&self, account: &AccountKey) -> Reply<MediaLimits> {
        self.reply(account, move |ready| async move {
            messaging::media_limits(&ready.client).await
        })
    }

    fn server_defaults(&self, account: &AccountKey) -> Reply<ServerDefaults> {
        self.reply(account, move |ready| async move {
            Ok(messaging::server_defaults(&ready.client).await)
        })
    }

    fn draft(&self, account: &AccountKey, room: &RoomId) -> Reply<Option<Draft>> {
        let room = room.to_string();
        let manager = self.manager.clone();
        let key = account.clone();
        self.blocking_reply(account, move || {
            let id = manager
                .account_by_key(&key)
                .ok_or(SessionError::UnknownAccount)?
                .id();
            let stored: Option<Value> = manager
                .with_profile(|profile| profile.draft(id, &room))
                .map_err(|error| {
                    info!(%error, "reading a draft failed");
                    SessionError::Failed("Your draft couldn't be opened.".to_owned())
                })?;
            Ok(stored.as_ref().and_then(draft_from_json))
        })
    }

    fn save_draft(&self, account: &AccountKey, room: &RoomId, draft: Option<Draft>) -> Reply<()> {
        let room = room.to_string();
        let manager = self.manager.clone();
        let key = account.clone();
        self.blocking_reply(account, move || {
            let id = manager
                .account_by_key(&key)
                .ok_or(SessionError::UnknownAccount)?
                .id();
            let value = draft.as_ref().map(draft_json);
            manager
                .with_profile(|profile| profile.set_draft(id, &room, value.as_ref()))
                .map_err(|error| {
                    info!(%error, "keeping a draft failed");
                    SessionError::Failed("Your draft couldn't be kept.".to_owned())
                })
        })
    }

    fn image(&self, account: &AccountKey, media: &MediaInfo) -> MediaReply<Arc<DecodedImage>> {
        let media = media.clone();
        self.media_reply(account, move |client| async move {
            media::image(&client, &media).await
        })
    }

    fn save_file(&self, account: &AccountKey, media: &MediaInfo, path: PathBuf) -> MediaReply<()> {
        let media = media.clone();
        self.media_reply(account, move |client| async move {
            media::save_file(&client, &media, path).await
        })
    }

    fn notifications(&self) -> mpsc::Receiver<IncomingNotification> {
        self.manager.notification_feed().subscribe()
    }

    fn unread_total(&self, account: &AccountKey) -> Option<watch::Receiver<u64>> {
        self.manager
            .account_by_key(account)
            .map(|account| account.unread())
    }

    fn retry_sync(&self, account: &AccountKey) {
        if let Some(account) = self.manager.account_by_key(account) {
            account.retry_sync();
        }
    }
    fn encryption(&self, account: &AccountKey) -> Option<watch::Receiver<EncryptionStatus>> {
        self.manager
            .account_by_key(account)
            .map(|account| account.encryption())
    }

    fn set_up_identity(
        &self,
        account: &AccountKey,
        password: Option<SecretText>,
    ) -> Reply<AuthOutcome> {
        let shared = self
            .manager
            .account_by_key(account)
            .map(|account| account.encryption_shared());
        let password = encryption::password_secret(password);
        self.reply(account, move |ready| async move {
            let shared = shared.ok_or(SessionError::UnknownAccount)?;
            encryption::set_up_identity(&ready.client, &shared, password).await
        })
    }

    fn verify_this_device(&self, account: &AccountKey) -> Reply<VerificationFlow> {
        self.reply(account, move |ready| async move {
            encryption::verify_this_device(&ready.client).await
        })
    }

    fn verify_device(&self, account: &AccountKey, device_id: &str) -> Reply<VerificationFlow> {
        let device_id = device_id.to_owned();
        self.reply(account, move |ready| async move {
            encryption::verify_device(&ready.client, &device_id).await
        })
    }

    fn verification_requests(
        &self,
    ) -> mpsc::Receiver<familychat_core::session::IncomingVerification> {
        self.manager.verification_feed().subscribe()
    }

    fn enable_recovery(&self, account: &AccountKey) -> Reply<SecretText> {
        self.reply(account, move |ready| async move {
            encryption::enable_recovery(&ready.client).await
        })
    }

    fn recover(&self, account: &AccountKey, recovery_key: SecretText) -> Reply<()> {
        self.reply(account, move |ready| async move {
            encryption::recover(&ready.client, recovery_key).await
        })
    }

    fn devices(&self, account: &AccountKey) -> Reply<Vec<DeviceView>> {
        self.reply(account, move |ready| async move {
            encryption::devices(&ready.client).await
        })
    }

    fn sign_out_device(
        &self,
        account: &AccountKey,
        device_id: &str,
        password: Option<SecretText>,
    ) -> Reply<AuthOutcome> {
        let device_id = device_id.to_owned();
        let password = encryption::password_secret(password);
        self.reply(account, move |ready| async move {
            encryption::sign_out_device(&ready.client, &device_id, password).await
        })
    }

    fn export_room_keys(
        &self,
        account: &AccountKey,
        path: PathBuf,
        passphrase: SecretText,
    ) -> Reply<()> {
        self.reply(account, move |ready| async move {
            encryption::export_room_keys(&ready.client, path, passphrase).await
        })
    }

    fn import_room_keys(
        &self,
        account: &AccountKey,
        path: PathBuf,
        passphrase: SecretText,
    ) -> Reply<KeyImport> {
        self.reply(account, move |ready| async move {
            encryption::import_room_keys(&ready.client, path, passphrase).await
        })
    }

    fn identity_warnings(
        &self,
        account: &AccountKey,
        room: &RoomId,
    ) -> Reply<watch::Receiver<Vec<IdentityWarning>>> {
        let room: OwnedRoomId = room.to_owned();
        self.reply(account, move |ready| async move {
            let room = ready
                .client
                .get_room(&room)
                .ok_or(SessionError::UnknownRoom)?;
            encryption::identity_warnings(room).await
        })
    }

    fn acknowledge_identity(&self, account: &AccountKey, user: &UserId) -> Reply<()> {
        let user = user.to_owned();
        self.reply(account, move |ready| async move {
            encryption::acknowledge_identity(&ready.client, &user).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drafts_round_trip_through_the_stored_form() {
        let draft = Draft {
            text: MessageBody::new("Mum, are we still on?"),
            mentions: vec![Mention {
                user_id: OwnedUserId::try_from("@mum:smith.safechat.family").expect("an id"),
                label: "Mum".to_owned(),
            }],
            reply_to: Some(OwnedEventId::try_from("$reply:smith.safechat.family").expect("an id")),
            editing: None,
        };
        assert_eq!(draft_from_json(&draft_json(&draft)), Some(draft));
        assert_eq!(draft_from_json(&json!({"mentions": []})), None);
    }
}
