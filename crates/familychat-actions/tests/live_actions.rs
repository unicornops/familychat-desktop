//! The messaging actions against the live homeserver harness (ADR 0004,
//! ADR 0012): Palpo at Family Chat's image. Start it first:
//!
//! ```bash
//! scripts/homeserver-harness.sh up
//! cargo test -p familychat-actions --test live_actions -- --ignored
//! scripts/homeserver-harness.sh down
//! ```
//!
//! Each member is a real `LiveSession` over its own profile, signed in
//! with the harness's password login and handed to the account manager;
//! what they do goes through `familychat-actions`, exactly as the window
//! does it. The offline test stops the harness's front end, so it runs on
//! its own: the others share a read lock it takes for writing.

use std::{
    path::PathBuf,
    sync::{Arc, LazyLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail, ensure};
use familychat_actions::{ActionEvent, Actions, ComposerState, FileSource, Refusal, SendTarget};
use familychat_core::{
    OwnedEventId, OwnedRoomId, OwnedUserId,
    diff::apply_all,
    messaging::{Draft, MessageRef},
    rich_text::RichText,
    rooms::{Encryption, RoomListFilter, RoomSummary},
    session::{SessionApi, TimelineSubscription},
    sync::{AccountStatus, SyncState},
    timeline::{
        MessageBody, SendState, TimelineContent, TimelineEvent, TimelineItem, TimelineItemKind,
        TimelineUpdate,
    },
};
use familychat_security::{
    AccountId, AccountSession, AuthKind, Keychain, MemoryKeychain, Secret,
    image_metadata::has_location,
};
use familychat_session::{AccountManager, AnyHomeserver, LiveSession, SessionConfig, account_key};
use familychat_store::{DataDir, Profile, ProfileState};
use familychat_test_support::{
    HomeserverHarness, TestUser,
    photo::{PHOTO_LATITUDE, photo_with_location},
};
use tokio::sync::{RwLock, mpsc, watch};

/// How long an account may take to reach "running".
const SYNC_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a message may take to reach the other member.
const ARRIVAL: Duration = Duration::from_secs(30);

/// The offline test stops the front end every other test talks through.
static FRONT_END: LazyLock<RwLock<()>> = LazyLock::new(|| RwLock::new(()));

fn harness() -> Result<HomeserverHarness> {
    HomeserverHarness::connect()
}

/// A throwaway profile with the mock keychain, removed when dropped.
struct TestProfile {
    root: PathBuf,
    keychain: Arc<MemoryKeychain>,
}

impl TestProfile {
    fn new(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self {
            root: std::env::temp_dir().join(format!(
                "familychat-actions-{name}-{}-{nanos}",
                std::process::id()
            )),
            keychain: Arc::new(MemoryKeychain::new()),
        }
    }

    fn manager(&self, http: reqwest::Client) -> Result<AccountManager> {
        let keychain: Arc<dyn Keychain> = self.keychain.clone();
        let profile = match Profile::open(&DataDir::at(&self.root), keychain)? {
            ProfileState::Ready(profile) => profile,
            other => bail!("the test profile did not open: {other:?}"),
        };
        Ok(AccountManager::new(
            profile,
            SessionConfig::with_http_client(http),
        ))
    }
}

impl Drop for TestProfile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// One member of a test: their own profile, session and actions.
struct Member {
    _profile: TestProfile,
    manager: Arc<AccountManager>,
    session: Arc<LiveSession>,
    id: AccountId,
    actions: Actions,
    events: mpsc::UnboundedReceiver<ActionEvent>,
}

impl Member {
    async fn sign_in(harness: &HomeserverHarness, user: &TestUser, name: &str) -> Result<Self> {
        let profile = TestProfile::new(name);
        let manager = Arc::new(profile.manager(harness.client().clone())?);
        let id = manager
            .add_account(
                AccountSession {
                    user_id: user.user_id.clone(),
                    device_id: user.session.device_id.clone(),
                    homeserver_url: harness.hs1_url().to_owned(),
                    access_token: Secret::new(user.session.access_token().to_owned()),
                    refresh_token: user
                        .session
                        .refresh_token()
                        .map(|token| Secret::new(token.to_owned())),
                    auth_kind: AuthKind::Password,
                    oauth_client_id: None,
                },
                &AnyHomeserver,
            )
            .await?;
        let session = Arc::new(LiveSession::new(
            manager.clone(),
            tokio::runtime::Handle::current(),
        ));
        let (actions, events) = Actions::channel(session.clone(), account_key(id));
        let member = Self {
            _profile: profile,
            manager,
            session,
            id,
            actions,
            events,
        };
        member
            .until_sync("a running sync", |state| *state == SyncState::Running)
            .await?;
        Ok(member)
    }

    fn status(&self) -> Result<watch::Receiver<AccountStatus>> {
        self.manager
            .account(self.id)
            .map(|account| account.status())
            .context("the account is open")
    }

    async fn until_sync(
        &self,
        what: &str,
        accept: impl Fn(&SyncState) -> bool,
    ) -> Result<SyncState> {
        let mut status = self.status()?;
        let waited = tokio::time::timeout(SYNC_TIMEOUT, async {
            loop {
                let current = status.borrow_and_update().sync.clone();
                if accept(&current) {
                    return Ok(current);
                }
                if status.changed().await.is_err() {
                    bail!("the account went away while waiting for {what}");
                }
            }
        })
        .await;
        match waited {
            Ok(result) => result,
            Err(_) => bail!("no {what}; the account is {:?}", status.borrow().sync),
        }
    }

    fn retry_sync(&self) {
        if let Some(account) = self.manager.account(self.id) {
            account.retry_sync();
        }
    }

    /// The next action event.
    async fn event(&mut self) -> Result<ActionEvent> {
        tokio::time::timeout(Duration::from_secs(30), self.events.recv())
            .await
            .context("no action event within 30 seconds")?
            .context("the actions went away")
    }

    /// Waits for the room to be in the account's list, and returns it as
    /// `accept` takes it.
    async fn room(
        &self,
        room: &OwnedRoomId,
        accept: impl Fn(&RoomSummary) -> bool,
    ) -> Result<RoomSummary> {
        let mut list = self
            .session
            .room_list(&account_key(self.id), RoomListFilter::All)
            .await
            .context("the room list answers")??;
        let mut rooms: Vec<RoomSummary> = Vec::new();
        tokio::time::timeout(ARRIVAL, async {
            loop {
                if let Some(found) = rooms
                    .iter()
                    .find(|summary| summary.room_id == *room && accept(summary))
                {
                    return Ok(found.clone());
                }
                let batch = list.updates.recv().await.context("the room list stopped")?;
                apply_all(&mut rooms, batch)?;
            }
        })
        .await
        .with_context(|| format!("{room} never showed as expected in the room list"))?
    }

    async fn timeline(&self, room: &OwnedRoomId) -> Result<Followed> {
        let subscription = self
            .session
            .timeline(&account_key(self.id), room)
            .await
            .context("the timeline answers")??;
        let mut followed = Followed {
            subscription,
            items: Vec::new(),
        };
        followed.next().await?;
        Ok(followed)
    }

    async fn shutdown(self) {
        self.manager.shutdown().await;
    }
}

/// A timeline the test follows.
struct Followed {
    subscription: TimelineSubscription,
    items: Vec<TimelineItem>,
}

impl Followed {
    async fn next(&mut self) -> Result<()> {
        match self
            .subscription
            .updates
            .recv()
            .await
            .context("the timeline closed")?
        {
            TimelineUpdate::Items(diffs) => apply_all(&mut self.items, diffs)?,
            TimelineUpdate::Pagination(_) => {}
        }
        Ok(())
    }

    fn events(&self) -> impl Iterator<Item = (&TimelineItem, &TimelineEvent)> {
        self.items.iter().filter_map(|item| match &item.kind {
            TimelineItemKind::Event(event) => Some((item, event)),
            _ => None,
        })
    }

    /// The message whose plain body is `body`.
    fn message(&self, body: &str) -> Option<(&TimelineItem, &TimelineEvent)> {
        self.events().find(|(_, event)| {
            matches!(&event.content, TimelineContent::Message { body: found, .. } if found.as_str() == body)
        })
    }

    /// The event with `event_id`.
    fn event(&self, event_id: &OwnedEventId) -> Option<&TimelineEvent> {
        self.events()
            .map(|(_, event)| event)
            .find(|event| event.event_id.as_ref() == Some(event_id))
    }

    async fn until(&mut self, what: &str, done: impl Fn(&Self) -> bool) -> Result<()> {
        let result = tokio::time::timeout(ARRIVAL, async {
            while !done(self) {
                self.next().await?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await;
        match result {
            Ok(inner) => inner,
            Err(_) => bail!("no {what} within {ARRIVAL:?}"),
        }
    }

    /// Waits until the message `body` has an event id, and returns it.
    async fn sent(&mut self, body: &str) -> Result<(MessageRef, OwnedEventId)> {
        self.until(&format!("{body:?} with an event id"), |timeline| {
            timeline
                .message(body)
                .is_some_and(|(_, event)| event.event_id.is_some())
        })
        .await?;
        let (item, event) = self.message(body).context("the message")?;
        let event_id = event.event_id.clone().context("an event id")?;
        Ok((
            MessageRef {
                item: item.id.clone(),
                event_id: Some(event_id.clone()),
            },
            event_id,
        ))
    }
}

fn draft(text: &str) -> Draft {
    Draft {
        text: MessageBody::new(text),
        ..Draft::default()
    }
}

/// A room `owner` makes with `others` invited and joined.
async fn shared_room(
    harness: &HomeserverHarness,
    owner: &TestUser,
    others: &[&TestUser],
    name: &str,
) -> Result<OwnedRoomId> {
    let invite: Vec<&str> = others.iter().map(|user| user.user_id.as_str()).collect();
    let room = harness
        .create_room(
            &owner.session,
            &format!("{name} {}", HomeserverHarness::unique_localpart("room")),
            &invite,
        )
        .await?;
    for other in others {
        harness.join_room(&other.session, &room).await?;
    }
    Ok(room.try_into()?)
}

fn expect_done(event: &ActionEvent) -> Result<()> {
    ensure!(
        matches!(event, ActionEvent::Done { .. }),
        "the action did not go: {event:?}"
    );
    Ok(())
}

/// Palpo: Alice and Bob, each on their own Family Chat session, send,
/// reply, edit, react and redact through `familychat-actions`, and each
/// sees what the other did; receipts and typing notices reach the server.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn messages_round_trip_between_two_members() -> Result<()> {
    let _front_end = FRONT_END.read().await;
    let harness = harness()?;
    let alice_user = harness.new_user("actions-alice").await?;
    let bob_user = harness.new_user("actions-bob").await?;
    let room = shared_room(&harness, &alice_user, &[&bob_user], "Round trip").await?;
    let mut alice = Member::sign_in(&harness, &alice_user, "alice").await?;
    let mut bob = Member::sign_in(&harness, &bob_user, "bob").await?;
    alice.room(&room, |_| true).await?;
    bob.room(&room, |_| true).await?;
    let mut alice_sees = alice.timeline(&room).await?;
    let mut bob_sees = bob.timeline(&room).await?;

    // Send, with Markdown.
    let marker = HomeserverHarness::unique_localpart("hello");
    let first = format!("**hello** {marker}");
    alice
        .actions
        .send(&room, &draft(&first), SendTarget::New)
        .await;
    expect_done(&alice.event().await?)?;
    let (_, first_id) = bob_sees.sent(&first).await?;
    let (_, received) = bob_sees.message(&first).context("the message")?;
    let TimelineContent::Message { rich, .. } = &received.content else {
        bail!("not a message");
    };
    ensure!(
        *rich != RichText::plain(&first),
        "the Markdown arrived as plain text"
    );

    // Reply.
    let answer = format!("hi alice {marker}");
    bob.actions
        .send(&room, &draft(&answer), SendTarget::Reply(first_id.clone()))
        .await;
    expect_done(&bob.event().await?)?;
    alice_sees
        .until("Bob's reply", |timeline| {
            timeline.message(&answer).is_some_and(|(_, event)| {
                event
                    .in_reply_to
                    .as_ref()
                    .is_some_and(|reply| reply.event_id == first_id)
            })
        })
        .await?;

    // Edit.
    let (own, _) = alice_sees.sent(&first).await?;
    let edited = format!("hello again {marker}");
    alice
        .actions
        .send(&room, &draft(&edited), SendTarget::Edit(own.clone()))
        .await;
    expect_done(&alice.event().await?)?;
    bob_sees
        .until("Alice's edit", |timeline| {
            timeline.event(&first_id).is_some_and(|event| {
                matches!(&event.content, TimelineContent::Message { body, edited: true, .. } if body.as_str() == edited)
            })
        })
        .await?;

    // React, and take the reaction back.
    let on_first = MessageRef {
        item: bob_sees
            .events()
            .find(|(_, event)| event.event_id.as_ref() == Some(&first_id))
            .map(|(item, _)| item.id.clone())
            .context("Bob's item")?,
        event_id: Some(first_id.clone()),
    };
    bob.actions
        .toggle_reaction(&room, on_first.clone(), "👍")
        .await;
    expect_done(&bob.event().await?)?;
    alice_sees
        .until("Bob's reaction", |timeline| {
            timeline.event(&first_id).is_some_and(|event| {
                event
                    .reactions
                    .iter()
                    .any(|reaction| reaction.key == "👍" && reaction.count == 1 && !reaction.own)
            })
        })
        .await?;
    bob.actions.toggle_reaction(&room, on_first, "👍").await;
    expect_done(&bob.event().await?)?;
    alice_sees
        .until("the reaction taken back", |timeline| {
            timeline
                .event(&first_id)
                .is_some_and(|event| event.reactions.is_empty())
        })
        .await?;

    // Receipts and typing reach the server.
    bob.actions.typing(&room, true).await;
    bob.actions.mark_read(&room).await;
    harness
        .wait_for_access("a typing notice", |line| {
            line.method == "PUT" && line.path.contains("/typing/") && line.status == 200
        })
        .await?;
    harness
        .wait_for_access("the read marker and receipt", |line| {
            line.method == "POST" && line.path.ends_with("/read_markers") && line.status == 200
        })
        .await?;

    // Redact.
    alice.actions.redact(&room, own, None).await;
    expect_done(&alice.event().await?)?;
    bob_sees
        .until("the deletion", |timeline| {
            timeline
                .event(&first_id)
                .is_some_and(|event| matches!(event.content, TimelineContent::Redacted))
        })
        .await?;

    ensure!(
        alice.events.try_recv().is_err(),
        "Alice heard more than expected"
    );
    ensure!(
        bob.events.try_recv().is_err(),
        "Bob heard more than expected"
    );
    alice.shutdown().await;
    bob.shutdown().await;
    Ok(())
}

/// A message sent while the server can't be reached waits in the send
/// queue and goes when the front end is back.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn offline_sends_go_out_when_the_server_is_back() -> Result<()> {
    let _front_end = FRONT_END.write().await;
    let harness = harness()?;
    let alice_user = harness.new_user("offline-alice").await?;
    let bob_user = harness.new_user("offline-bob").await?;
    let room = shared_room(&harness, &bob_user, &[&alice_user], "Offline").await?;
    let mut alice = Member::sign_in(&harness, &alice_user, "offline").await?;
    alice.room(&room, |_| true).await?;
    let mut alice_sees = alice.timeline(&room).await?;

    harness.stop_front_end().await?;
    let started = async {
        let text = format!(
            "sent offline {}",
            HomeserverHarness::unique_localpart("queued")
        );
        alice
            .actions
            .send(&room, &draft(&text), SendTarget::New)
            .await;
        // Queued: the local echo shows at once.
        expect_done(&alice.event().await?)?;
        alice_sees
            .until("the local echo", |timeline| {
                timeline.message(&text).is_some_and(|(_, event)| {
                    event.event_id.is_none()
                        && matches!(
                            event.send_state,
                            Some(
                                SendState::Sending
                                    | SendState::Failed {
                                        recoverable: true,
                                        ..
                                    }
                            )
                        )
                })
            })
            .await?;
        alice
            .until_sync("the offline state", |state| {
                matches!(state, SyncState::Offline { .. })
            })
            .await?;
        // Long enough for the queue to have tried and failed.
        tokio::time::sleep(Duration::from_secs(3)).await;
        let unsent = alice_sees
            .message(&text)
            .is_some_and(|(_, event)| event.event_id.is_none());
        ensure!(unsent, "the message went while the server was down");
        Ok::<_, anyhow::Error>(text)
    }
    .await;
    // The front end comes back whatever happened.
    harness.start_front_end().await?;
    let text = started?;
    alice.retry_sync();
    alice
        .until_sync("a running sync after the outage", |state| {
            *state == SyncState::Running
        })
        .await?;
    alice_sees.sent(&text).await?;
    let arrived = tokio::time::timeout(ARRIVAL, async {
        loop {
            let events = harness
                .recent_events(&bob_user.session, room.as_str())
                .await?;
            if events
                .iter()
                .any(|event| event["content"]["body"].as_str() == Some(text.as_str()))
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await;
    ensure!(arrived.is_ok(), "Bob never got the queued message");
    alice.shutdown().await;
    Ok(())
}

/// A curfew: the paused account's composer is read-only and nothing is
/// sent; when the pause ends, writing works again.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn a_paused_account_cannot_send_until_the_pause_ends() -> Result<()> {
    let _front_end = FRONT_END.read().await;
    let harness = harness()?;
    let child_user = harness.new_user("actions-curfew").await?;
    let parent = harness.new_user("actions-parent").await?;
    let room = shared_room(&harness, &parent, &[&child_user], "Curfew").await?;
    let mut child = Member::sign_in(&harness, &child_user, "curfew").await?;
    let summary = child.room(&room, |_| true).await?;

    harness.set_suspended(&child_user.user_id, true).await?;
    // Wake the child's long-polling sync, so its next request meets the
    // suspension.
    harness
        .send_text(&parent.session, room.as_str(), "time for bed")
        .await?
        .ok("the parent's message")?;
    child
        .until_sync("the paused state", |state| {
            matches!(state, SyncState::Suspended { .. })
        })
        .await?;
    let status = child.status()?.borrow().clone();
    ensure!(ComposerState::of(&status, &summary) == ComposerState::Paused);
    child
        .actions
        .send(&room, &draft("but I'm not tired"), SendTarget::New)
        .await;
    let refused = child.event().await?;
    ensure!(
        matches!(
            &refused,
            ActionEvent::Refused {
                refusal: Refusal::Paused,
                ..
            }
        ),
        "{refused:?}"
    );
    ensure!(
        refused
            .message()
            .is_some_and(|message| message.starts_with("Chat is paused")),
        "{refused:?}"
    );

    harness.set_suspended(&child_user.user_id, false).await?;
    child.retry_sync();
    child
        .until_sync("a running sync after the curfew", |state| {
            *state == SyncState::Running
        })
        .await?;
    let status = child.status()?.borrow().clone();
    ensure!(ComposerState::of(&status, &summary) == ComposerState::Writable);
    let morning = format!(
        "good morning {}",
        HomeserverHarness::unique_localpart("awake")
    );
    child
        .actions
        .send(&room, &draft(&morning), SendTarget::New)
        .await;
    expect_done(&child.event().await?)?;
    let arrived = tokio::time::timeout(ARRIVAL, async {
        loop {
            let events = harness
                .recent_events(&parent.session, room.as_str())
                .await?;
            if events
                .iter()
                .any(|event| event["content"]["body"].as_str() == Some(morning.as_str()))
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await;
    ensure!(arrived.is_ok(), "the parent never got the morning message");
    child.shutdown().await;
    Ok(())
}

/// The media uploads in the front end's access log (`/_matrix/media/*/upload`
/// or `/_matrix/client/*/media/upload`; not the encryption keys' uploads,
/// which every new device makes).
fn uploads(harness: &HomeserverHarness) -> Result<usize> {
    Ok(harness
        .access_log()?
        .iter()
        .filter(|line| {
            line.method == "POST" && line.path.contains("/media/") && line.path.ends_with("/upload")
        })
        .count())
}

/// A file over Palpo's 11 MiB is refused before any upload request (the
/// access log shows only the media config read), and a photo arrives
/// without its location: the bytes Bob downloads have no GPS in them.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn files_are_checked_first_and_photos_lose_their_location() -> Result<()> {
    let _front_end = FRONT_END.read().await;
    let harness = harness()?;
    let alice_user = harness.new_user("files-alice").await?;
    let bob_user = harness.new_user("files-bob").await?;
    let room = shared_room(&harness, &bob_user, &[&alice_user], "Files").await?;
    let mut alice = Member::sign_in(&harness, &alice_user, "files").await?;
    alice.room(&room, |_| true).await?;
    let dir = std::env::temp_dir().join(format!(
        "familychat-live-files-{}-{}",
        std::process::id(),
        HomeserverHarness::unique_localpart("dir")
    ));
    std::fs::create_dir_all(&dir)?;

    // Over the limit: refused before anything is uploaded.
    let big = dir.join("holiday.mov");
    std::fs::write(&big, vec![0u8; 11 * 1024 * 1024 + 1])?;
    let before = uploads(&harness)?;
    alice
        .actions
        .send_file(&room, FileSource::Path(big), None)
        .await;
    let refused = alice.event().await?;
    let ActionEvent::Refused {
        refusal: Refusal::TooLarge { limit, .. },
        ..
    } = &refused
    else {
        bail!("an over-limit file was not refused: {refused:?}");
    };
    ensure!(*limit == 11 * 1024 * 1024, "the limit read was {limit}");
    harness
        .wait_for_access("the media config read", |line| {
            line.method == "GET" && line.path == "/_matrix/client/v1/media/config"
        })
        .await?;
    tokio::time::sleep(Duration::from_secs(1)).await;
    ensure!(
        uploads(&harness)? == before,
        "something was uploaded for a file over the limit"
    );

    // A photo with a location: what arrives has none.
    let photo = photo_with_location();
    ensure!(
        has_location(&photo),
        "the test photo has no location to remove"
    );
    let path = dir.join("bedroom.jpg");
    std::fs::write(&path, &photo)?;
    alice
        .actions
        .send_file(&room, FileSource::Path(path), None)
        .await;
    expect_done(&alice.event().await?)?;
    let event = tokio::time::timeout(ARRIVAL, async {
        loop {
            let events = harness
                .recent_events(&bob_user.session, room.as_str())
                .await?;
            if let Some(event) = events.into_iter().find(|event| {
                event["content"]["msgtype"].as_str() == Some("m.image")
                    && event["content"]["body"].as_str() == Some("bedroom.jpg")
            }) {
                return Ok::<_, anyhow::Error>(event);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .context("Bob never got the photo")??;
    let mxc = event["content"]["url"]
        .as_str()
        .context("the photo is not encrypted in this room, so it has a url")?;
    let uploaded = harness.download_media(&bob_user.session, mxc).await?;
    ensure!(
        uploaded.starts_with(&[0xFF, 0xD8]),
        "the upload is not a JPEG"
    );
    ensure!(
        !has_location(&uploaded),
        "the uploaded photo has a location"
    );
    ensure!(
        !String::from_utf8_lossy(&uploaded).contains(PHOTO_LATITUDE),
        "the uploaded photo names the latitude"
    );
    ensure!(
        uploaded.len() < photo.len(),
        "the uploaded photo is not smaller than the original"
    );
    ensure!(
        uploads(&harness)? == before + 1,
        "one upload, for the photo"
    );
    let _ = std::fs::remove_dir_all(&dir);
    alice.shutdown().await;
    Ok(())
}

/// A direct chat made through the actions is encrypted: Palpo's
/// well-known doesn't turn encryption off.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn new_direct_chats_are_encrypted() -> Result<()> {
    let _front_end = FRONT_END.read().await;
    let harness = harness()?;
    let alice_user = harness.new_user("dm-alice").await?;
    let bob_user = harness.new_user("dm-bob").await?;
    let mut alice = Member::sign_in(&harness, &alice_user, "dm").await?;
    let bob_id = OwnedUserId::try_from(bob_user.user_id.as_str())?;
    alice.actions.create_direct_chat(bob_id).await;
    let created = alice.event().await?;
    let ActionEvent::RoomCreated {
        room, direct: true, ..
    } = created
    else {
        bail!("no direct chat: {created:?}");
    };
    // The room list can show the new room before its encryption state and
    // the account's `m.direct` data have synced (briefly not encrypted, or
    // not yet direct), so wait for both; the room was made with
    // `m.room.encryption` in its initial state and marked direct, and a
    // room that never shows as both fails on the timeout.
    alice
        .room(&room, |summary| {
            summary.encryption == Encryption::Encrypted && summary.is_direct
        })
        .await
        .context("the new chat never showed as an encrypted direct chat")?;
    alice.shutdown().await;
    Ok(())
}
