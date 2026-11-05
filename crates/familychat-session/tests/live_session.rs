//! The session against the live homeserver harness (ADR 0004): Palpo at
//! Family Chat's image, and Synapse with refresh tokens. Start it first:
//!
//! ```bash
//! scripts/homeserver-harness.sh up
//! cargo test -p familychat-session --test live_session -- --ignored
//! scripts/homeserver-harness.sh down
//! ```
//!
//! The "sign-in" here is the harness's password login, whose session is
//! handed to the account manager as a new account; the real sign-in flows
//! are Phase 4's. The manager's HTTP client is the harness's, which trusts
//! only the harness CA (injected through `SessionConfig`).

mod common;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use common::{
    TestProfile, account_session, assert_not_in_files, captured_logs, log_text, png, wait_for_sync,
};
use familychat_core::{
    OwnedRoomId,
    diff::apply_all,
    rooms::{RoomListFilter, RoomSummary},
    session::{SessionApi, TimelineSubscription},
    sync::SyncState,
    timeline::{
        MessageKind, PaginationState, TimelineContent, TimelineEvent, TimelineItem,
        TimelineItemKind, TimelineUpdate,
    },
};
use familychat_security::{AccountId, keychain::load_session};
use familychat_session::{AccountManager, AnyHomeserver, LiveSession, account_key};
use familychat_test_support::{HomeserverHarness, TestUser};

/// How long an account may take to reach "running".
const SYNC_TIMEOUT: Duration = Duration::from_secs(60);

fn harness() -> Result<HomeserverHarness> {
    HomeserverHarness::connect()
}

fn session_for(user: &TestUser, homeserver: &str) -> familychat_security::AccountSession {
    account_session(
        &user.user_id,
        &user.session.device_id,
        homeserver,
        user.session.access_token(),
        user.session.refresh_token(),
    )
}

async fn running(manager: &AccountManager, id: AccountId) -> Result<()> {
    let account = manager.account(id).context("the account is open")?;
    let mut status = account.status();
    wait_for_sync(&mut status, SYNC_TIMEOUT, "a running sync", |state| {
        *state == SyncState::Running
    })
    .await?;
    Ok(())
}

/// Waits until the room list holds `room`.
async fn room_appears(
    session: &LiveSession,
    id: AccountId,
    room: &OwnedRoomId,
) -> Result<RoomSummary> {
    let mut list = session
        .room_list(&account_key(id), RoomListFilter::All)
        .await
        .context("the room list answers")??;
    let mut rooms = Vec::new();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(found) = rooms
                .iter()
                .find(|summary: &&RoomSummary| summary.room_id == *room)
            {
                return Ok(found.clone());
            }
            let batch = list.updates.recv().await.context("the room list stopped")?;
            apply_all(&mut rooms, batch)?;
        }
    })
    .await
    .with_context(|| {
        format!(
            "{room} did not appear in the room list; it has {} rooms",
            rooms.len()
        )
    })?
}

/// A timeline the test follows: the items, as the updates say.
struct Followed {
    subscription: TimelineSubscription,
    items: Vec<TimelineItem>,
    pagination: PaginationState,
}

impl Followed {
    async fn open(session: &LiveSession, id: AccountId, room: &OwnedRoomId) -> Result<Self> {
        let subscription = session
            .timeline(&account_key(id), room)
            .await
            .context("the timeline answers")??;
        let mut followed = Self {
            subscription,
            items: Vec::new(),
            pagination: PaginationState::Idle,
        };
        followed.next().await?;
        Ok(followed)
    }

    async fn next(&mut self) -> Result<()> {
        match self
            .subscription
            .updates
            .recv()
            .await
            .context("the timeline closed")?
        {
            TimelineUpdate::Items(diffs) => apply_all(&mut self.items, diffs)?,
            TimelineUpdate::Pagination(state) => self.pagination = state,
        }
        Ok(())
    }

    fn events(&self) -> impl Iterator<Item = &TimelineEvent> {
        self.items.iter().filter_map(|item| match &item.kind {
            TimelineItemKind::Event(event) => Some(event),
            _ => None,
        })
    }

    /// The message whose plain body is `body`.
    fn message(&self, body: &str) -> Option<&TimelineEvent> {
        self.events().find(|event| {
            matches!(&event.content, TimelineContent::Message { body: found, .. } if found.as_str() == body)
        })
    }

    fn bodies(&self) -> Vec<String> {
        self.items
            .iter()
            .filter_map(|item| match &item.kind {
                TimelineItemKind::Event(event) => match &event.content {
                    TimelineContent::Message { body, .. } => Some(body.as_str().to_owned()),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    async fn until(
        &mut self,
        timeout: Duration,
        what: &str,
        done: impl Fn(&Self) -> bool,
    ) -> Result<()> {
        let result = tokio::time::timeout(timeout, async {
            while !done(self) {
                self.next().await?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await;
        match result {
            Ok(inner) => inner,
            Err(_) => bail!(
                "no {what} within {timeout:?}; the timeline has {} messages",
                self.bodies().len()
            ),
        }
    }
}

/// Palpo (Family Chat's server): an account signed in through test support
/// syncs, restores after a restart without signing in again, lists the
/// seeded room, streams another member's message within ten seconds, loads
/// older messages, keeps no message text in its stores, and logs no
/// message text or token.
#[tokio::test]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn an_account_restores_syncs_and_streams_its_rooms_and_timeline() -> Result<()> {
    captured_logs();
    let harness = harness()?;
    let alice = harness.new_user("session-alice").await?;
    let bob = harness.new_user("session-bob").await?;
    let marker = HomeserverHarness::unique_localpart("plaintext");
    let room_name = format!("Seeded {marker}");
    let room: OwnedRoomId = harness
        .create_room(&bob.session, &room_name, &[alice.user_id.as_str()])
        .await?
        .try_into()?;
    harness.join_room(&alice.session, room.as_str()).await?;
    const OLDER: usize = 40;
    for index in 0..OLDER {
        harness
            .send_text(
                &bob.session,
                room.as_str(),
                &format!("older {index} {marker}"),
            )
            .await?
            .ok("an older message")?;
    }

    let test = TestProfile::new("live-restore");
    let http = harness.client().clone();

    // Signed in through test support: the harness's login, handed over.
    let id = {
        let manager = Arc::new(test.manager(http.clone())?);
        let id = manager
            .add_account(session_for(&alice, harness.hs1_url()), &AnyHomeserver)
            .await?;
        running(&manager, id).await?;
        let session = LiveSession::new(manager.clone(), tokio::runtime::Handle::current());
        let summary = room_appears(&session, id, &room).await?;
        ensure!(
            summary.name == room_name,
            "the room is named {:?}",
            summary.name
        );
        manager.shutdown().await;
        id
    };

    // A restart: a new manager over the same profile and keychain, with no
    // sign-in.
    let manager = Arc::new(test.manager(http.clone())?);
    let restored = manager.restore_all().await?;
    ensure!(
        restored.len() == 1 && restored[0].id == id && restored[0].result.is_ok(),
        "{restored:?}"
    );
    running(&manager, id).await?;
    let session = LiveSession::new(manager.clone(), tokio::runtime::Handle::current());
    room_appears(&session, id, &room).await?;

    let mut timeline = Followed::open(&session, id, &room).await?;
    let live = format!("live {marker}");
    let sent = Instant::now();
    harness
        .send_text(&bob.session, room.as_str(), &live)
        .await?
        .ok("bob's live message")?;
    timeline
        .until(Duration::from_secs(10), "live message", |timeline| {
            timeline.bodies().contains(&live)
        })
        .await?;
    println!(
        "the live message reached the timeline in {:?}",
        sent.elapsed()
    );

    let first = format!("older 0 {marker}");
    let before = timeline.bodies().len();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !timeline.bodies().contains(&first) {
        ensure!(
            timeline.pagination != PaginationState::ReachedStart,
            "reached the start without {first:?}: {:?}",
            timeline.bodies()
        );
        ensure!(
            Instant::now() < deadline,
            "back-pagination did not load {first:?}"
        );
        ensure!(
            timeline.subscription.paginate_backwards(20),
            "the timeline closed"
        );
        timeline
            .until(Duration::from_secs(30), "a pagination result", |timeline| {
                !matches!(
                    timeline.pagination,
                    PaginationState::Idle | PaginationState::Loading
                ) || timeline.bodies().contains(&first)
            })
            .await
            .or_else(|error| {
                // Idle after loading means "more to load"; ask again.
                if timeline.pagination == PaginationState::Idle {
                    Ok(())
                } else {
                    Err(error)
                }
            })?;
        if let PaginationState::Failed { message } = &timeline.pagination {
            bail!("back-pagination failed: {message}");
        }
    }
    let after = timeline.bodies().len();
    ensure!(
        after > before,
        "back-pagination loaded nothing ({before} then {after})"
    );
    println!("back-pagination: {before} messages, then {after}");
    drop(timeline);
    manager.shutdown().await;
    drop(session);
    drop(manager);

    // The SDK's stores are encrypted: no message text in their files.
    let account_dir = test.dir().account_dir(id);
    let scanned = assert_not_in_files(&account_dir, &marker)?;
    ensure!(
        scanned > 0,
        "no store files under {}",
        account_dir.display()
    );
    assert_not_in_files(&account_dir, &room_name)?;
    println!("scanned {scanned} bytes of store files for message text");

    // The log saw the sign-in and the sync, and none of the secrets.
    let text = log_text();
    ensure!(
        text.contains("added the account") && text.contains("restored the account"),
        "the log capture saw nothing of the session"
    );
    for secret in [
        marker.as_str(),
        alice.session.access_token(),
        bob.session.access_token(),
        alice.password.as_str(),
    ] {
        ensure!(
            !text.contains(secret),
            "the log contains a secret or a message"
        );
    }
    Ok(())
}

/// A curfew (PLAN.md section 6.8): on the pinned Palpo a suspended
/// account's sync is refused with `M_USER_SUSPENDED` (family-chat decision
/// 11), and the account shows the suspended state, not an error; lifted,
/// a retry brings the sync back.
#[tokio::test]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn a_suspended_account_shows_the_suspended_state() -> Result<()> {
    captured_logs();
    let harness = harness()?;
    let child = harness.new_user("session-curfew").await?;
    let parent = harness.new_user("session-parent").await?;
    let room = harness
        .create_room(&parent.session, "Curfew", &[child.user_id.as_str()])
        .await?;
    harness.join_room(&child.session, &room).await?;

    let test = TestProfile::new("live-suspended");
    let manager = test.manager(harness.client().clone())?;
    let id = manager
        .add_account(session_for(&child, harness.hs1_url()), &AnyHomeserver)
        .await?;
    running(&manager, id).await?;
    let account = manager.account(id).context("the account")?;
    let mut status = account.status();

    harness.set_suspended(&child.user_id, true).await?;
    // Wake the child's long-polling sync, so its next request meets the
    // suspension.
    harness
        .send_text(&parent.session, &room, "time for bed")
        .await?
        .ok("the parent's message")?;
    let state = wait_for_sync(
        &mut status,
        Duration::from_secs(90),
        "the suspended state",
        |state| {
            matches!(state, SyncState::Suspended { .. }) || matches!(state, SyncState::Error { .. })
        },
    )
    .await?;
    ensure!(
        matches!(state, SyncState::Suspended { .. }),
        "a suspension showed as {state:?}"
    );

    harness.set_suspended(&child.user_id, false).await?;
    account.retry_sync();
    wait_for_sync(
        &mut status,
        SYNC_TIMEOUT,
        "a running sync after the curfew",
        |state| *state == SyncState::Running,
    )
    .await?;
    manager.shutdown().await;
    Ok(())
}

/// Synapse with refresh tokens (20-second access tokens in the harness):
/// the SDK's rotated tokens reach the keychain, and an account restored
/// from them after a restart keeps syncing and keeps rotating.
#[tokio::test]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn rotated_tokens_are_persisted_and_survive_a_restart() -> Result<()> {
    captured_logs();
    let harness = harness()?;
    let user = harness.new_synapse_user("session-refresh").await?;
    let first_refresh = user
        .session
        .refresh_token()
        .context("Synapse issued no refresh token; check the harness's refreshable_access_token_lifetime")?
        .to_owned();
    let test = TestProfile::new("live-refresh");
    let http = harness.client().clone();

    let rotated = |id: AccountId, previous: String| {
        let keychain = test.keychain.clone();
        async move {
            let deadline = Instant::now() + Duration::from_secs(150);
            loop {
                let stored = load_session(keychain.as_ref(), id)?.context("the stored session")?;
                let refresh = stored
                    .refresh_token
                    .as_ref()
                    .map(|token| token.expose().clone())
                    .context("the stored session lost its refresh token")?;
                if refresh != previous {
                    return Ok::<_, anyhow::Error>(refresh);
                }
                ensure!(
                    Instant::now() < deadline,
                    "no token rotation reached the keychain in 150 s"
                );
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    };

    let (id, second_refresh) = {
        let manager = test.manager(http.clone())?;
        let id = manager
            .add_account(session_for(&user, harness.synapse_url()), &AnyHomeserver)
            .await?;
        running(&manager, id).await?;
        let second = rotated(id, first_refresh.clone()).await?;
        let stored = load_session(test.keychain.as_ref(), id)?.context("the stored session")?;
        ensure!(stored.access_token.expose() != user.session.access_token());
        running(&manager, id).await?;
        manager.shutdown().await;
        (id, second)
    };
    println!("the first rotation reached the keychain");

    let manager = test.manager(http)?;
    let restored = manager.restore_all().await?;
    ensure!(
        restored.len() == 1 && restored[0].result.is_ok(),
        "{restored:?}"
    );
    running(&manager, id).await?;
    // The persisted refresh token is the live one: the next rotation works
    // (the first refresh token has been used up).
    rotated(id, second_refresh).await?;
    running(&manager, id).await?;
    manager.shutdown().await;
    println!("the restored account rotated its tokens again");

    let text = log_text();
    ensure!(
        !text.contains(&first_refresh),
        "the log contains a refresh token"
    );
    ensure!(
        !text.contains(&user.password),
        "the log contains a password"
    );
    Ok(())
}

/// Palpo (Family Chat's server): another member's plain message, HTML
/// message and image reach the account's timeline through `SessionApi`;
/// the HTML arrives as the sanitised rich-text tree (bold, and its link),
/// the image as its media, which `SessionApi::image` fetches through
/// authenticated media only and decodes; the members list both; and the
/// log holds none of the text.
#[tokio::test]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn messages_and_an_image_from_another_user_appear() -> Result<()> {
    captured_logs();
    let harness = harness()?;
    let alice = harness.new_user("media-alice").await?;
    let bob = harness.new_user("media-bob").await?;
    let marker = HomeserverHarness::unique_localpart("rich");
    let room: OwnedRoomId = harness
        .create_room(
            &bob.session,
            &format!("Media {marker}"),
            &[alice.user_id.as_str()],
        )
        .await?
        .try_into()?;
    harness.join_room(&alice.session, room.as_str()).await?;

    let test = TestProfile::new("live-media");
    let manager = Arc::new(test.manager(harness.client().clone())?);
    let id = manager
        .add_account(session_for(&alice, harness.hs1_url()), &AnyHomeserver)
        .await?;
    running(&manager, id).await?;
    let session = LiveSession::new(manager.clone(), tokio::runtime::Handle::current());
    room_appears(&session, id, &room).await?;
    let mut timeline = Followed::open(&session, id, &room).await?;

    let plain = format!("plain {marker}");
    harness
        .send_text(&bob.session, room.as_str(), &plain)
        .await?
        .ok("bob's plain message")?;
    let html_plain = format!("bold {marker} https://example.org/{marker}");
    let html = format!(
        "<strong>bold {marker}</strong> <a href=\"https://example.org/{marker}\">the link</a>\
         <script>alert(1)</script>"
    );
    harness
        .send_html(&bob.session, room.as_str(), &html_plain, &html)
        .await?
        .ok("bob's HTML message")?;
    let (width, height) = (6, 4);
    let bytes = png(width, height);
    let size = bytes.len();
    let mxc = harness
        .upload_media(&bob.session, bytes, "image/png", "pixels.png")
        .await?;
    let media_id = mxc.rsplit('/').next().unwrap_or_default().to_owned();
    harness
        .send_image(
            &bob.session,
            room.as_str(),
            &mxc,
            "pixels.png",
            "image/png",
            (width, height),
            size,
        )
        .await?
        .ok("bob's image")?;

    timeline
        .until(Duration::from_secs(20), "the three messages", |timeline| {
            timeline.message(&plain).is_some()
                && timeline.message(&html_plain).is_some()
                && timeline.message("pixels.png").is_some()
        })
        .await?;

    // The plain message, from bob.
    let event = timeline.message(&plain).context("the plain message")?;
    ensure!(event.sender.as_str() == bob.user_id && !event.is_own);
    let TimelineContent::Message { kind, rich, .. } = &event.content else {
        bail!("not a message");
    };
    ensure!(*kind == MessageKind::Text && rich.plain_text() == plain);

    // The HTML message: sanitised into the tree, script gone.
    let event = timeline.message(&html_plain).context("the HTML message")?;
    let TimelineContent::Message { rich, .. } = &event.content else {
        bail!("not a message");
    };
    let text = rich.plain_text();
    ensure!(
        text.contains(&format!("bold {marker}")) && text.contains("the link"),
        "the formatted body was not drawn: {} blocks",
        rich.blocks.len()
    );
    ensure!(!text.contains("alert"), "the script's text survived");
    let bold = rich.blocks.iter().any(|block| {
        block
            .spans
            .iter()
            .any(|span| span.style.bold && block.text[span.range.clone()].contains(&marker))
    });
    ensure!(bold, "no bold span");
    ensure!(
        rich.links
            .iter()
            .any(|link| link.href == format!("https://example.org/{marker}")),
        "the link was lost"
    );

    // The image: its media, fetched and decoded.
    let event = timeline.message("pixels.png").context("the image")?;
    let TimelineContent::Message {
        kind,
        media: Some(media),
        ..
    } = &event.content
    else {
        bail!("the image has no media");
    };
    ensure!(*kind == MessageKind::Image, "{kind:?}");
    ensure!(
        media.filename == "pixels.png"
            && media.declared_size() == Some((width, height))
            && media.mimetype.as_deref() == Some("image/png"),
        "{media:?}"
    );
    let image = session
        .image(&account_key(id), media)
        .await
        .context("the image answers")?
        .map_err(|error| anyhow::anyhow!("the image did not decode: {error}"))?;
    // The file itself, or a server thumbnail of it: the same shape.
    ensure!(
        image.width * height == image.height * width && !image.frames.is_empty(),
        "the image is {}x{}",
        image.width,
        image.height
    );
    println!("the image decoded at {}x{}", image.width, image.height);
    // Every request for it went to the authenticated endpoints.
    harness
        .wait_for_access("the image download", |line| line.path.contains(&media_id))
        .await?;
    let requests: Vec<String> = harness
        .access_log()?
        .into_iter()
        .filter(|line| line.path.contains(&media_id))
        .map(|line| line.path)
        .collect();
    ensure!(
        requests
            .iter()
            .all(|path| path.starts_with("/_matrix/client/v1/media/")),
        "unauthenticated media requests: {requests:?}"
    );

    // Both members, by name or id.
    let members = session
        .members(&account_key(id), &room)
        .await
        .context("the members answer")??;
    for user in [&alice.user_id, &bob.user_id] {
        ensure!(
            members.iter().any(|member| member.user_id.as_str() == user),
            "{user} is not listed"
        );
    }
    ensure!(members.iter().filter(|member| member.is_own).count() == 1);

    drop(timeline);
    manager.shutdown().await;
    let log = log_text();
    for secret in [marker.as_str(), alice.session.access_token()] {
        ensure!(
            !log.contains(secret),
            "the log contains a message or a token"
        );
    }
    Ok(())
}
