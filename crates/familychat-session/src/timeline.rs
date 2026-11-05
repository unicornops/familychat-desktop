//! A room's timeline, bridged: matrix-sdk-ui's `Timeline` items polled on
//! the Tokio runtime, mapped to [`TimelineItem`] diffs, with back-pagination
//! on request.
//!
//! An open room is subscribed in sliding sync (`set_room_subscriptions`, as
//! Element X does) for as long as its timeline is open: the room list asks
//! for only the latest event of each room, so without the subscription a
//! burst of messages would arrive as the newest one and a gap.

use std::sync::Arc;

use familychat_core::{
    diff::ListDiff,
    session::{SUBSCRIPTION_BUFFER, SessionError, TimelineSubscription},
    timeline::{PaginationState, TimelineCommand, TimelineUpdate},
};
use futures_util::StreamExt as _;
use matrix_sdk::{Room, ruma::OwnedUserId};
use matrix_sdk_ui::timeline::RoomExt as _;
use matrix_sdk_ui::{RoomListService, Timeline, unable_to_decrypt_hook::UtdHookManager};
use tokio::sync::{Mutex, mpsc};
use tracing::info;

use crate::{
    mapping::{list_diff, timeline_item},
    messaging::OpenTimelines,
};

/// Opens the room's live timeline and starts the task that bridges it. The
/// timeline is registered in `open` while it runs, so messaging actions in
/// the room work on it.
pub(crate) async fn open(
    room: Room,
    service: Arc<RoomListService>,
    open: OpenTimelines,
    utd_hook: Arc<UtdHookManager>,
) -> Result<TimelineSubscription, SessionError> {
    service.set_room_subscriptions(&[room.room_id()]).await;
    let timeline = room
        .timeline_builder()
        .with_unable_to_decrypt_hook(utd_hook)
        .build()
        .await
        .map_err(|error| SessionError::Failed(format!("The room couldn't be opened: {error}")))?;
    let (updates_tx, updates) = mpsc::channel(SUBSCRIPTION_BUFFER);
    let (commands, commands_rx) = mpsc::unbounded_channel();
    let own_user = room.own_user_id().to_owned();
    let room_id = room.room_id().to_owned();
    let timeline = Arc::new(timeline);
    open.insert(room_id.clone(), &timeline);
    tokio::spawn(async move {
        run(timeline, own_user, updates_tx, commands_rx).await;
        // The room's events no longer need to arrive in full.
        service.remove_room_subscriptions(&[&room_id]);
    });
    Ok(TimelineSubscription::new(updates, commands))
}

/// Bridges the timeline until the frontend drops its subscription.
async fn run(
    timeline: Arc<Timeline>,
    own_user: OwnedUserId,
    updates: mpsc::Sender<TimelineUpdate>,
    mut commands: mpsc::UnboundedReceiver<TimelineCommand>,
) {
    let (items, stream) = timeline.subscribe().await;
    let reset = ListDiff::Reset {
        values: items
            .iter()
            .map(|item| timeline_item(item, &own_user))
            .collect(),
    };
    if updates
        .send(TimelineUpdate::Items(vec![reset]))
        .await
        .is_err()
    {
        return;
    }
    let mut stream = std::pin::pin!(stream);
    // One pagination at a time; a request while one runs waits for it.
    let paginating = Arc::new(Mutex::new(()));
    loop {
        tokio::select! {
            batch = stream.next() => {
                let Some(batch) = batch else { break };
                let batch = batch
                    .into_iter()
                    .map(|diff| list_diff(diff, |item| timeline_item(&item, &own_user)))
                    .collect();
                if updates.send(TimelineUpdate::Items(batch)).await.is_err() {
                    break;
                }
            }
            command = commands.recv() => match command {
                Some(TimelineCommand::PaginateBackwards { events }) => {
                    tokio::spawn(paginate(
                        timeline.clone(),
                        events,
                        updates.clone(),
                        paginating.clone(),
                    ));
                }
                None => break,
            },
            () = updates.closed() => break,
        }
    }
}

/// Loads older events and reports how it went.
async fn paginate(
    timeline: Arc<Timeline>,
    events: u16,
    updates: mpsc::Sender<TimelineUpdate>,
    paginating: Arc<Mutex<()>>,
) {
    let _one_at_a_time = paginating.lock().await;
    if updates
        .send(TimelineUpdate::Pagination(PaginationState::Loading))
        .await
        .is_err()
    {
        return;
    }
    let state = match timeline.paginate_backwards(events).await {
        Ok(true) => PaginationState::ReachedStart,
        Ok(false) => PaginationState::Idle,
        Err(error) => {
            info!(%error, "loading older messages failed");
            PaginationState::Failed {
                message: "Older messages couldn't be loaded. Try again.".to_owned(),
            }
        }
    };
    let _ = updates.send(TimelineUpdate::Pagination(state)).await;
}
