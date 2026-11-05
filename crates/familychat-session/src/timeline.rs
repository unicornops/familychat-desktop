//! A room's timeline, bridged: matrix-sdk-ui's `Timeline` items polled on
//! the Tokio runtime, mapped to [`TimelineItem`] diffs, with back-pagination
//! on request.

use std::sync::Arc;

use familychat_core::{
    diff::ListDiff,
    session::{SUBSCRIPTION_BUFFER, SessionError, TimelineSubscription},
    timeline::{PaginationState, TimelineCommand, TimelineUpdate},
};
use futures_util::StreamExt as _;
use matrix_sdk::Room;
use matrix_sdk_ui::Timeline;
use matrix_sdk_ui::timeline::RoomExt as _;
use tokio::sync::{Mutex, mpsc};
use tracing::info;

use crate::mapping::{list_diff, timeline_item};

/// Opens the room's live timeline and starts the task that bridges it.
pub(crate) async fn open(room: Room) -> Result<TimelineSubscription, SessionError> {
    let timeline = room
        .timeline()
        .await
        .map_err(|error| SessionError::Failed(format!("The room couldn't be opened: {error}")))?;
    let (updates_tx, updates) = mpsc::channel(SUBSCRIPTION_BUFFER);
    let (commands, commands_rx) = mpsc::unbounded_channel();
    tokio::spawn(run(Arc::new(timeline), updates_tx, commands_rx));
    Ok(TimelineSubscription::new(updates, commands))
}

/// Bridges the timeline until the frontend drops its subscription.
async fn run(
    timeline: Arc<Timeline>,
    updates: mpsc::Sender<TimelineUpdate>,
    mut commands: mpsc::UnboundedReceiver<TimelineCommand>,
) {
    let (items, stream) = timeline.subscribe().await;
    let reset = ListDiff::Reset {
        values: items.iter().map(|item| timeline_item(item)).collect(),
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
                    .map(|diff| list_diff(diff, |item| timeline_item(&item)))
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
