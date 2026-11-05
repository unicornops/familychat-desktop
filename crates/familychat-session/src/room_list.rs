//! The room list, bridged: matrix-sdk-ui's `RoomListService` entries with
//! dynamic adapters (filters and paging), polled on the Tokio runtime,
//! mapped to [`RoomSummary`] diffs and sent over a bounded channel.

use std::sync::Arc;

use familychat_core::{
    diff::ListDiff,
    rooms::{RoomListCommand, RoomListFilter, RoomListLoading, RoomSummary},
    session::{RoomListSubscription, SUBSCRIPTION_BUFFER, SessionError},
};
use futures_util::StreamExt as _;
use matrix_sdk_ui::{
    RoomListService,
    room_list_service::{
        RoomList, RoomListLoadingState,
        filters::{
            self, BoxedFilterFn, ReadReceiptsCategory, RoomCategory, new_filter_all,
            new_filter_category, new_filter_deduplicate_versions, new_filter_invite,
            new_filter_non_left, new_filter_not, new_filter_read_receipts,
        },
    },
};
use tokio::sync::{mpsc, watch};

use crate::mapping::{list_diff, room_summary};

/// Rooms per page of the list.
pub(crate) const PAGE_SIZE: usize = 50;

/// The SDK filter for a core filter. Spaces are never listed (they arrive
/// with spaces support, PLAN.md section 14), and upgraded rooms show once.
pub(crate) fn sdk_filter(filter: RoomListFilter) -> BoxedFilterFn {
    let mut parts: Vec<BoxedFilterFn> = vec![
        Box::new(new_filter_deduplicate_versions()),
        Box::new(new_filter_not(Box::new(filters::new_filter_space()))),
    ];
    match filter {
        RoomListFilter::All => parts.push(Box::new(new_filter_non_left())),
        RoomListFilter::Unread => {
            parts.push(Box::new(new_filter_non_left()));
            parts.push(Box::new(new_filter_read_receipts(
                ReadReceiptsCategory::Notifications,
            )));
        }
        RoomListFilter::People => {
            parts.push(Box::new(new_filter_non_left()));
            parts.push(Box::new(new_filter_not(Box::new(new_filter_invite()))));
            parts.push(Box::new(new_filter_category(RoomCategory::People)));
        }
        RoomListFilter::Invites => parts.push(Box::new(new_filter_invite())),
    }
    Box::new(new_filter_all(parts))
}

/// Opens the room list and starts the task that bridges it.
pub(crate) async fn open(
    service: Arc<RoomListService>,
    filter: RoomListFilter,
) -> Result<RoomListSubscription, SessionError> {
    let list = service
        .all_rooms()
        .await
        .map_err(|error| SessionError::Failed(format!("The room list isn't available: {error}")))?;
    let (updates_tx, updates) = mpsc::channel(SUBSCRIPTION_BUFFER);
    let (loading_tx, loading) = watch::channel(RoomListLoading::NotLoaded);
    let (commands, commands_rx) = mpsc::unbounded_channel();
    tokio::spawn(run(list, filter, updates_tx, loading_tx, commands_rx));
    Ok(RoomListSubscription::new(updates, loading, commands))
}

fn loading(state: RoomListLoadingState) -> RoomListLoading {
    match state {
        RoomListLoadingState::NotLoaded => RoomListLoading::NotLoaded,
        RoomListLoadingState::Loaded {
            maximum_number_of_rooms,
        } => RoomListLoading::Loaded {
            total: maximum_number_of_rooms,
        },
    }
}

/// Bridges the list until the frontend drops its subscription.
async fn run(
    list: RoomList,
    filter: RoomListFilter,
    updates: mpsc::Sender<Vec<ListDiff<RoomSummary>>>,
    loading_tx: watch::Sender<RoomListLoading>,
    mut commands: mpsc::UnboundedReceiver<RoomListCommand>,
) {
    let mut loading_state = list.loading_state();
    loading_tx.send_replace(loading(loading_state.get()));
    let (entries, controller) = list.entries_with_dynamic_adapters(PAGE_SIZE);
    controller.set_filter(sdk_filter(filter));
    let mut entries = std::pin::pin!(entries);
    let mut loading_open = true;
    loop {
        tokio::select! {
            batch = entries.next() => {
                let Some(batch) = batch else { break };
                let batch: Vec<_> = batch
                    .into_iter()
                    .map(|diff| list_diff(diff, |room| room_summary(&room)))
                    .collect();
                if updates.send(batch).await.is_err() {
                    break;
                }
            }
            state = loading_state.next(), if loading_open => match state {
                Some(state) => {
                    loading_tx.send_replace(loading(state));
                }
                None => loading_open = false,
            },
            command = commands.recv() => match command {
                Some(RoomListCommand::SetFilter(filter)) => {
                    controller.set_filter(sdk_filter(filter));
                }
                Some(RoomListCommand::LoadMore) => controller.add_one_page(),
                Some(RoomListCommand::FirstPageOnly) => controller.reset_to_one_page(),
                None => break,
            },
            () = updates.closed() => break,
        }
    }
}
