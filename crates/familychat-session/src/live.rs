//! [`LiveSession`]: the [`SessionApi`] the frontends use, over the
//! [`AccountManager`]. Each call spawns its work on the background runtime
//! and returns the channel its answer arrives on.

use std::sync::Arc;

use familychat_core::{
    rooms::RoomListFilter,
    session::{
        AccountKey, AccountView, Reply, RoomListSubscription, SessionApi, SessionError,
        TimelineSubscription,
    },
    sync::AccountStatus,
};
use matrix_sdk::ruma::{OwnedRoomId, RoomId};
use tokio::{
    runtime::Handle,
    sync::{oneshot, watch},
};

use crate::{account::Ready, manager::AccountManager, room_list, timeline};

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

    fn retry_sync(&self, account: &AccountKey) {
        if let Some(account) = self.manager.account_by_key(account) {
            account.retry_sync();
        }
    }
}
