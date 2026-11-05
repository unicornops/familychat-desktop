//! [`LiveSession`]: the [`SessionApi`] the frontends use, over the
//! [`AccountManager`]. Each call spawns its work on the background runtime
//! and returns the channel its answer arrives on.

use std::{path::PathBuf, sync::Arc};

use familychat_core::{
    media::{DecodedImage, MediaError, MediaInfo},
    rooms::{MemberRole, Membership, RoomListFilter, RoomMember, sort_members},
    session::{
        AccountKey, AccountView, MediaReply, Reply, RoomListSubscription, SessionApi, SessionError,
        TimelineSubscription,
    },
    sync::AccountStatus,
};
use matrix_sdk::{
    Client, RoomMemberships,
    ruma::{
        OwnedRoomId, RoomId,
        events::room::{member::MembershipState, power_levels::UserPowerLevel},
    },
};
use tokio::{
    runtime::Handle,
    sync::{oneshot, watch},
};

use tracing::info;

use crate::{account::Ready, manager::AccountManager, media, room_list, timeline};

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
            timeline::open(room).await
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

    fn answer_invite(&self, account: &AccountKey, room: &RoomId, accept: bool) -> Reply<()> {
        let room: OwnedRoomId = room.to_owned();
        self.reply(account, move |ready| async move {
            let room = ready
                .client
                .get_room(&room)
                .ok_or(SessionError::UnknownRoom)?;
            let answered = if accept {
                room.join().await
            } else {
                room.leave().await
            };
            answered.map_err(|error| {
                info!(%error, accept, "answering an invitation failed");
                SessionError::Failed(if accept {
                    "The invitation couldn't be accepted. Try again.".to_owned()
                } else {
                    "The invitation couldn't be declined. Try again.".to_owned()
                })
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

    fn retry_sync(&self, account: &AccountKey) {
        if let Some(account) = self.manager.account_by_key(account) {
            account.retry_sync();
        }
    }
}
