//! End-to-end encryption against the live homeserver harness (ADR 0004,
//! ADR 0009): Palpo at Family Chat's image. Start it first:
//!
//! ```bash
//! scripts/homeserver-harness.sh up
//! cargo test -p familychat-session --test live_encryption -- --ignored --nocapture
//! scripts/homeserver-harness.sh down
//! ```
//!
//! Every device is a real `LiveSession` over its own profile, driven
//! through `SessionApi` as the window drives it:
//!
//! - two devices of one account verify each other by emoji, and the new
//!   one reads the history sent before it existed;
//! - recovery set up on one device lets a new device read the history with
//!   the recovery key;
//! - an account signed in with a code (no password) makes its
//!   cross-signing identity on Palpo; the test prints which way it went
//!   (MSC3967, or a demand for the password that the password answers), for
//!   family-chat #257;
//! - room keys exported from one device and imported on another decrypt
//!   the history there, and a wrong passphrase is refused;
//! - a message sent the moment a room is made encrypted is stored on the
//!   server as `m.room.encrypted`, without its text, on Palpo and Synapse.

mod common;

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use common::{TestProfile, captured_logs, log_text, wait_for_sync};
use familychat_core::{
    OwnedRoomId,
    diff::apply_all,
    encryption::{
        AuthOutcome, DeviceTrust, EncryptionStatus, IdentitySetup, SecretText, VerificationAnswer,
        VerificationFlow, VerificationStep, wording,
    },
    messaging::{NewRoom, OutgoingMessage, RoomRequest},
    session::{AccountKey, SessionApi},
    sync::SyncState,
    timeline::{SendState, TimelineContent, TimelineItem, TimelineItemKind, TimelineUpdate},
};
use familychat_security::{AccountSession, AuthKind, Secret};
use familychat_session::{AccountManager, AnyHomeserver, LiveSession, account_key};
use familychat_test_support::{HomeserverHarness, Session, TestUser};
use tokio::sync::{mpsc, watch};

/// How long an account may take to sync, or a step of a verification to
/// arrive.
const WAIT: Duration = Duration::from_secs(90);

/// How long a request to confirm a device waits for the other device
/// before it is cancelled and sent again.
const ASK_AGAIN: Duration = Duration::from_secs(15);

fn harness() -> Result<HomeserverHarness> {
    HomeserverHarness::connect()
}

/// One device of an account: its own profile, manager and session.
struct Device {
    _profile: TestProfile,
    manager: Arc<AccountManager>,
    session: Arc<LiveSession>,
    key: AccountKey,
}

impl Device {
    /// Opens `session` (a sign-in made through the harness) as a new
    /// account on a profile of its own; `password` is a password sign-in's,
    /// handed on as the sign-in does (ADR 0009).
    async fn open(
        harness: &HomeserverHarness,
        name: &str,
        user_id: &str,
        session: &Session,
        auth_kind: AuthKind,
        password: Option<&str>,
    ) -> Result<Self> {
        Self::open_on(
            harness,
            harness.hs1_url(),
            name,
            user_id,
            session,
            auth_kind,
            password,
        )
        .await
    }

    /// [`Device::open`] on the homeserver at `homeserver_url` (hs1 or
    /// Synapse).
    async fn open_on(
        harness: &HomeserverHarness,
        homeserver_url: &str,
        name: &str,
        user_id: &str,
        session: &Session,
        auth_kind: AuthKind,
        password: Option<&str>,
    ) -> Result<Self> {
        let profile = TestProfile::new(name);
        let manager = Arc::new(profile.manager(harness.client().clone())?);
        let id = manager
            .add_account_with_password(
                AccountSession {
                    user_id: user_id.to_owned(),
                    device_id: session.device_id.clone(),
                    homeserver_url: homeserver_url.to_owned(),
                    access_token: Secret::new(session.access_token().to_owned()),
                    refresh_token: session
                        .refresh_token()
                        .map(|token| Secret::new(token.to_owned())),
                    auth_kind,
                    oauth_client_id: None,
                },
                &AnyHomeserver,
                password.map(|password| Secret::new(password.to_owned())),
            )
            .await?;
        let live = Arc::new(LiveSession::new(
            manager.clone(),
            tokio::runtime::Handle::current(),
        ));
        let device = Self {
            _profile: profile,
            manager,
            session: live,
            key: account_key(id),
        };
        let mut status = device
            .manager
            .account(id)
            .context("the account is open")?
            .status();
        wait_for_sync(&mut status, WAIT, "a running sync", |state| {
            *state == SyncState::Running
        })
        .await?;
        Ok(device)
    }

    fn encryption(&self) -> Result<watch::Receiver<EncryptionStatus>> {
        self.session
            .encryption(&self.key)
            .context("the account has an encryption status")
    }

    /// Waits until the encryption status passes `accept`.
    async fn until(
        &self,
        what: &str,
        accept: impl Fn(&EncryptionStatus) -> bool,
    ) -> Result<EncryptionStatus> {
        let mut status = self.encryption()?;
        let waited = tokio::time::timeout(WAIT, async {
            loop {
                let current = status.borrow_and_update().clone();
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
            Err(_) => bail!(
                "no {what} within {WAIT:?}; the status is {:?}",
                status.borrow()
            ),
        }
    }

    /// Makes an encrypted room and sends `texts` in it, waiting until the
    /// server has each.
    async fn room_with_history(&self, texts: &[&str]) -> Result<OwnedRoomId> {
        let room = self
            .session
            .create_room(
                &self.key,
                NewRoom {
                    name: Some("Encrypted history".to_owned()),
                    invite: Vec::new(),
                    direct: false,
                    encrypted: true,
                },
            )
            .await
            .context("create_room answers")??;
        let mut timeline = Followed::open(self, &room).await?;
        for text in texts {
            self.session
                .room_action(
                    &self.key,
                    &room,
                    RoomRequest::Send {
                        message: OutgoingMessage::plain(text),
                        reply_to: None,
                    },
                )
                .await
                .context("room_action answers")??;
        }
        timeline
            .until("every message sent", |items| {
                texts.iter().all(|text| {
                    message(items, text)
                        .is_some_and(|state| state == Some(SendState::Sent) || state.is_none())
                })
            })
            .await?;
        Ok(room)
    }

    /// Waits until the room's timeline shows `text` decrypted. On a
    /// failure, the log's lines about backups and decryption come with the
    /// error (the log is the app's own, redacted).
    async fn reads(&self, room: &OwnedRoomId, text: &str) -> Result<()> {
        let mut timeline = Followed::open(self, room).await?;
        let read = timeline
            .until(&format!("{text:?} decrypted"), |items| {
                message(items, text).is_some()
            })
            .await;
        read.map_err(|error| {
            let log = log_text();
            let lines: Vec<&str> = log
                .lines()
                .filter(|line| {
                    let line = line.to_ascii_lowercase();
                    line.contains("backup")
                        || line.contains("decrypt")
                        || line.contains("room key")
                        || line.contains("secret")
                })
                .collect();
            let tail = lines[lines.len().saturating_sub(60)..].join("\n");
            error.context(format!("the log about keys and backups:\n{tail}"))
        })
    }

    /// The account's client, for the facts `SessionApi` doesn't report
    /// (the backup's upload, the server's view of the keys).
    fn client(&self) -> Result<matrix_sdk::Client> {
        let id = self.key.0.parse().context("an account id")?;
        self.manager
            .account(id)
            .and_then(|account| account.client().cloned())
            .context("the account's client")
    }

    async fn shutdown(self) {
        self.manager.shutdown().await;
    }
}

/// The send state of the message `text`, if the timeline shows it as text
/// (`Some(None)`: an event from the server, with no send state).
fn message(items: &[TimelineItem], text: &str) -> Option<Option<SendState>> {
    items.iter().find_map(|item| match &item.kind {
        TimelineItemKind::Event(event) => match &event.content {
            TimelineContent::Message { body, .. } if body.as_str() == text => {
                Some(event.send_state)
            }
            _ => None,
        },
        _ => None,
    })
}

/// A timeline the test follows.
struct Followed {
    updates: mpsc::Receiver<TimelineUpdate>,
    items: Vec<TimelineItem>,
    _subscription: familychat_core::session::TimelineControls,
}

impl Followed {
    async fn open(device: &Device, room: &OwnedRoomId) -> Result<Self> {
        // A device signed in a moment ago may not have the room yet.
        let subscription = tokio::time::timeout(WAIT, async {
            loop {
                match device.session.timeline(&device.key, room).await {
                    Ok(Ok(subscription)) => return Ok(subscription),
                    Ok(Err(familychat_core::session::SessionError::UnknownRoom)) => {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                    Ok(Err(error)) => bail!("the timeline couldn't open: {error}"),
                    Err(_) => bail!("the session went away"),
                }
            }
        })
        .await
        .context("the room never reached the device")??;
        let (updates, controls) = subscription.split();
        let mut followed = Self {
            updates,
            items: Vec::new(),
            _subscription: controls,
        };
        followed.next().await?;
        Ok(followed)
    }

    async fn next(&mut self) -> Result<()> {
        match self.updates.recv().await.context("the timeline closed")? {
            TimelineUpdate::Items(diffs) => apply_all(&mut self.items, diffs)?,
            TimelineUpdate::Pagination(_) => {}
        }
        Ok(())
    }

    async fn until(&mut self, what: &str, done: impl Fn(&[TimelineItem]) -> bool) -> Result<()> {
        let waited = tokio::time::timeout(WAIT, async {
            while !done(&self.items) {
                self.next().await?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await;
        match waited {
            Ok(result) => result,
            Err(_) => {
                let shown: Vec<String> = self
                    .items
                    .iter()
                    .filter_map(|item| match &item.kind {
                        TimelineItemKind::Event(event) => Some(format!("{:?}", event.content)),
                        _ => None,
                    })
                    .collect();
                bail!("no {what} within {WAIT:?}; the timeline shows {shown:?}")
            }
        }
    }
}

/// Waits for a verification step that passes `accept`.
async fn step(
    flow: &VerificationFlow,
    what: &str,
    accept: impl Fn(&VerificationStep) -> bool,
) -> Result<VerificationStep> {
    let mut steps = flow.steps();
    let waited = tokio::time::timeout(WAIT, async {
        loop {
            let current = steps.borrow_and_update().clone();
            if accept(&current) {
                return Ok(current);
            }
            if let VerificationStep::Cancelled(why) = &current {
                bail!("the verification stopped while waiting for {what}: {why}");
            }
            if steps.changed().await.is_err() {
                bail!("the verification went away while waiting for {what}");
            }
        }
    })
    .await;
    match waited {
        Ok(result) => result,
        Err(_) => bail!("no {what} within {WAIT:?}; the step is {:?}", flow.step()),
    }
}

/// Waits until the device's key backup is on and has every room key.
async fn backed_up(device: &Device) -> Result<()> {
    let client = device.client()?;
    let backups = client.encryption().backups();
    tokio::time::timeout(WAIT, async {
        while !backups.are_enabled().await {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .context("the key backup was never turned on")?;
    backups
        .wait_for_steady_state()
        .await
        .context("the room keys reach the backup")?;
    Ok(())
}

/// A second device of `user`, signed in with the password.
async fn second_device(harness: &HomeserverHarness, user: &TestUser, name: &str) -> Result<Device> {
    let session = harness
        .login_password(&user.user_id, &user.password)
        .await?;
    Device::open(
        harness,
        name,
        &user.user_id,
        &session,
        AuthKind::Password,
        Some(&user.password),
    )
    .await
}

/// Waits until `device` sees another of the account's devices signed by
/// the account's identity: matrix-sdk sends a request to confirm this
/// device only to those, so a request made before then reaches no one.
/// On a timeout it says which devices it saw, and how.
async fn knows_a_signed_sibling(device: &Device) -> Result<()> {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let devices = device
            .session
            .devices(&device.key)
            .await
            .context("devices answers")?
            .map_err(|error| anyhow::anyhow!("the devices couldn't be listed: {error}"))?;
        if devices
            .iter()
            .any(|seen| !seen.is_this_device && seen.verified)
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            let seen: Vec<String> = devices
                .iter()
                .map(|seen| {
                    format!(
                        "{} (this device: {}, signed: {})",
                        seen.device_id, seen.is_this_device, seen.verified
                    )
                })
                .collect();
            bail!("no signed sibling device within {WAIT:?}; the device saw {seen:?}");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[tokio::test]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn two_devices_verify_each_other_by_emoji_and_history_decrypts() -> Result<()> {
    let _logs = captured_logs();
    let harness = harness()?;
    let user = harness.new_user("verify").await?;
    let first = Device::open(
        &harness,
        "verify-first",
        &user.user_id,
        &user.session,
        AuthKind::Password,
        Some(&user.password),
    )
    .await?;
    first
        .until("a confirmed first device", |status| {
            status.identity == IdentitySetup::Ready && status.device == DeviceTrust::Verified
        })
        .await?;
    let room = first
        .room_with_history(&["Sent before the laptop existed"])
        .await?;
    backed_up(&first).await?;

    let second = second_device(&harness, &user, "verify-second").await?;
    second
        .until(
            "a device asked to confirm",
            EncryptionStatus::needs_confirmation,
        )
        .await?;
    // A device that has just signed in may not know its siblings, or their
    // signatures, yet, and a verification request goes only to the signed
    // devices it knows.
    knows_a_signed_sibling(&second).await?;
    let mut incoming = first.session.verification_requests();
    // matrix-sdk 0.19.1 drops a request from a device the receiving device
    // has no keys for yet ("Could not retrieve the device data for the
    // incoming verification request, ignoring it"), and the first device
    // learns the new device's keys only when its sync reports the device
    // list change. So ask again, as a person would after cancelling, until
    // the request gets through or the wait runs out.
    let deadline = tokio::time::Instant::now() + WAIT;
    let mut asked = 0;
    let (ours, account, theirs) = loop {
        asked += 1;
        let ours = second
            .session
            .verify_this_device(&second.key)
            .await
            .context("verify_this_device answers")??;
        match tokio::time::timeout(ASK_AGAIN, incoming.recv()).await {
            Ok(received) => {
                let (account, theirs) = received.context("the requests stopped")?;
                break (ours, account, theirs);
            }
            Err(_) if tokio::time::Instant::now() < deadline => {
                ours.answer(VerificationAnswer::Cancel);
            }
            Err(_) => {
                // Say what the devices logged about verification, so a
                // failure shows whether the request was sent, received or
                // dropped.
                let seen: Vec<String> = log_text()
                    .lines()
                    .filter(|line| {
                        let line = line.to_lowercase();
                        line.contains("verif")
                            || line.contains("to-device")
                            || line.contains("to_device")
                    })
                    .map(str::to_owned)
                    .collect();
                bail!(
                    "no request reached the first device in {asked} tries within {WAIT:?}; the verification log: {seen:#?}"
                );
            }
        }
    };
    eprintln!("the verification request got through on try {asked}");
    ensure!(account == first.key, "the request is for the first account");
    step(&theirs, "the incoming request", |step| {
        *step == VerificationStep::Incoming
    })
    .await?;
    theirs.answer(VerificationAnswer::Accept);
    step(&ours, "ready", |step| {
        matches!(step, VerificationStep::Ready { .. })
    })
    .await?;
    ours.answer(VerificationAnswer::CompareEmoji);
    let VerificationStep::CompareEmoji(our_emoji) = step(&ours, "our emoji", |step| {
        matches!(step, VerificationStep::CompareEmoji(_))
    })
    .await?
    else {
        bail!("no emoji");
    };
    let VerificationStep::CompareEmoji(their_emoji) = step(&theirs, "their emoji", |step| {
        matches!(step, VerificationStep::CompareEmoji(_))
    })
    .await?
    else {
        bail!("no emoji");
    };
    ensure!(our_emoji.len() == 7, "seven emoji: {our_emoji:?}");
    ensure!(
        our_emoji == their_emoji,
        "both devices show the same emoji: {our_emoji:?} and {their_emoji:?}"
    );
    ours.answer(VerificationAnswer::Match);
    theirs.answer(VerificationAnswer::Match);
    step(&ours, "done here", |step| *step == VerificationStep::Done).await?;
    step(&theirs, "done there", |step| {
        *step == VerificationStep::Done
    })
    .await?;
    second
        .until("the second device confirmed", |status| {
            status.device == DeviceTrust::Verified
        })
        .await?;
    eprintln!(
        "verified by emoji: {}",
        our_emoji
            .iter()
            .map(|emoji| emoji.description.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    );

    second
        .reads(&room, "Sent before the laptop existed")
        .await
        .context("the history sent before the second device decrypts there")?;

    // Nothing secret reaches the log.
    let log = log_text();
    ensure!(!log.contains(&user.password), "the password is in the log");
    first.shutdown().await;
    second.shutdown().await;
    Ok(())
}

#[tokio::test]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn recovery_set_up_on_one_device_lets_a_new_device_read_history() -> Result<()> {
    let _logs = captured_logs();
    let harness = harness()?;
    let user = harness.new_user("recovery").await?;
    let first = Device::open(
        &harness,
        "recovery-first",
        &user.user_id,
        &user.session,
        AuthKind::Password,
        Some(&user.password),
    )
    .await?;
    first
        .until("a confirmed first device", |status| {
            status.identity == IdentitySetup::Ready && status.device == DeviceTrust::Verified
        })
        .await?;
    let room = first.room_with_history(&["Kept safe by recovery"]).await?;
    let key = first
        .session
        .enable_recovery(&first.key)
        .await
        .context("enable_recovery answers")??;
    ensure!(!key.expose().trim().is_empty(), "a recovery key was made");
    let recovery_key = key.expose().to_owned();
    first
        .until("recovery on", |status| {
            status.recovery == familychat_core::encryption::RecoveryState::Enabled
        })
        .await?;

    let second = second_device(&harness, &user, "recovery-second").await?;
    second
        .until(
            "a device asked to confirm",
            EncryptionStatus::needs_confirmation,
        )
        .await?;
    let wrong = second
        .session
        .recover(
            &second.key,
            SecretText::from("EsTc 0000 0000 0000 0000 0000"),
        )
        .await
        .context("recover answers")?;
    ensure!(
        wrong.as_ref().err().map(ToString::to_string).as_deref()
            == Some(wording::WRONG_RECOVERY_KEY),
        "a wrong key is refused as such: {wrong:?}"
    );
    second
        .session
        .recover(&second.key, SecretText::new(recovery_key.clone()))
        .await
        .context("recover answers")??;
    second
        .until("the second device confirmed", |status| {
            status.device == DeviceTrust::Verified
        })
        .await?;
    second
        .reads(&room, "Kept safe by recovery")
        .await
        .context("the history decrypts on the new device")?;

    let log = log_text();
    ensure!(
        !log.contains(&recovery_key),
        "the recovery key is never logged"
    );
    let compact: String = recovery_key.split_whitespace().collect();
    ensure!(!log.contains(&compact), "the recovery key is never logged");
    first.shutdown().await;
    second.shutdown().await;
    Ok(())
}

#[tokio::test]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn a_code_only_account_makes_its_cross_signing_identity_on_palpo() -> Result<()> {
    let _logs = captured_logs();
    let harness = harness()?;
    let localpart = HomeserverHarness::unique_localpart("codeonly");
    let password = format!("{localpart}-password");
    let mxid = harness.create_user(&localpart, &password).await?;
    let token = harness.mint_login_token(&mxid, 300_000).await?;
    let login = harness.login_with_token(&token).await?.ok("token login")?;
    let session = Session::from_login_body(&login.body)?;

    // Signed in with a code: the device has no password to give.
    let device = Device::open(
        &harness,
        "code-only",
        &mxid,
        &session,
        AuthKind::LoginToken,
        None,
    )
    .await?;
    let settled = device
        .until("the identity made, or the password asked for", |status| {
            matches!(
                status.identity,
                IdentitySetup::Ready | IdentitySetup::NeedsPassword | IdentitySetup::Failed(_)
            )
        })
        .await?;
    let outcome = match settled.identity {
        IdentitySetup::Ready => {
            device
                .until("the code-only device confirmed", |status| {
                    status.device == DeviceTrust::Verified
                })
                .await?;
            "MSC3967 honoured: the first cross-signing upload went through without user-interactive auth"
        }
        IdentitySetup::NeedsPassword => {
            // The UX's way forward: the password, typed on the page.
            let answered = device
                .session
                .set_up_identity(&device.key, Some(SecretText::new(password.clone())))
                .await
                .context("set_up_identity answers")??;
            ensure!(
                answered == AuthOutcome::Done,
                "the password answers: {answered:?}"
            );
            device
                .until("the identity with the password", |status| {
                    status.identity == IdentitySetup::Ready
                        && status.device == DeviceTrust::Verified
                })
                .await?;
            "Palpo asked for user-interactive auth on the first cross-signing upload; the password page answered it"
        }
        IdentitySetup::Failed(message) => bail!("making the identity failed: {message}"),
        other => bail!("unexpected identity state {other:?}"),
    };

    // The server has the identity: its master key is published.
    let client = device.client()?;
    let user_id = matrix_sdk::ruma::OwnedUserId::try_from(mxid.as_str())?;
    let identity = client
        .encryption()
        .request_user_identity(&user_id)
        .await
        .context("the keys query")?
        .context("the server has the account's identity")?;
    ensure!(
        identity.is_verified(),
        "this device trusts its own identity"
    );
    eprintln!("CODE-ONLY CROSS-SIGNING OUTCOME (family-chat #257): {outcome}");
    ensure!(
        !log_text().contains(token.secret()),
        "the login token is never logged"
    );
    device.shutdown().await;
    Ok(())
}

#[tokio::test]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn room_keys_exported_on_one_device_decrypt_history_on_another() -> Result<()> {
    let _logs = captured_logs();
    let harness = harness()?;
    let user = harness.new_user("keyfile").await?;
    let first = Device::open(
        &harness,
        "keyfile-first",
        &user.user_id,
        &user.session,
        AuthKind::Password,
        Some(&user.password),
    )
    .await?;
    first
        .until("a confirmed first device", |status| {
            status.identity == IdentitySetup::Ready
        })
        .await?;
    let room = first
        .room_with_history(&["Moving to our own domain"])
        .await?;
    let stored = first_message_on_server(&harness, harness.hs1_url(), &user.session, &room).await?;
    ensure!(
        stored["type"] == "m.room.encrypted"
            && !stored.to_string().contains("Moving to our own domain"),
        "the history went out without encryption: the server stored {}",
        stored["type"]
    );
    let dir = std::env::temp_dir().join(format!(
        "familychat-keyfile-{}",
        HomeserverHarness::unique_localpart("x")
    ));
    std::fs::create_dir_all(&dir)?;
    let file = dir.join("room-keys.txt");
    let passphrase = "correct horse battery staple";
    let short = first
        .session
        .export_room_keys(&first.key, file.clone(), SecretText::from("short"))
        .await
        .context("export answers")?;
    ensure!(short.is_err(), "a short passphrase is refused");
    first
        .session
        .export_room_keys(&first.key, file.clone(), SecretText::from(passphrase))
        .await
        .context("export answers")??;
    let exported = std::fs::read_to_string(&file)?;
    ensure!(
        exported.contains("BEGIN MEGOLM SESSION DATA"),
        "the file is a Megolm key export"
    );
    ensure!(
        !exported.contains("Moving to our own domain"),
        "the file holds keys, not messages"
    );

    // A device that is neither verified nor has the recovery key.
    let second = second_device(&harness, &user, "keyfile-second").await?;
    let wrong = second
        .session
        .import_room_keys(
            &second.key,
            file.clone(),
            SecretText::from("not the passphrase"),
        )
        .await
        .context("import answers")?;
    ensure!(
        wrong.as_ref().err().map(ToString::to_string).as_deref() == Some(wording::IMPORT_FAILED),
        "a wrong passphrase is refused as such: {wrong:?}"
    );
    let imported = second
        .session
        .import_room_keys(&second.key, file.clone(), SecretText::from(passphrase))
        .await
        .context("import answers")??;
    ensure!(
        imported.total >= 1,
        "the export held no room keys ({imported:?}): the history was sent without \
         encryption, as no Megolm session was ever made for it"
    );
    ensure!(
        imported.imported >= 1,
        "the keys were imported: {imported:?}"
    );
    eprintln!("room key round trip: {}", imported.message());
    second
        .reads(&room, "Moving to our own domain")
        .await
        .context("the imported keys decrypt the history")?;
    ensure!(
        !log_text().contains(passphrase),
        "the passphrase is never logged"
    );
    let _ = std::fs::remove_dir_all(&dir);
    first.shutdown().await;
    second.shutdown().await;
    Ok(())
}

/// The first message-like event in `room` as the server at
/// `homeserver_url` stores it (`m.room.message` or `m.room.encrypted`),
/// waited for: the send queue sends a moment after the request returns.
async fn first_message_on_server(
    harness: &HomeserverHarness,
    homeserver_url: &str,
    session: &Session,
    room: &OwnedRoomId,
) -> Result<serde_json::Value> {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let events = harness
            .recent_events_on(homeserver_url, session, room.as_str())
            .await?;
        if let Some(event) = events.into_iter().rev().find(|event| {
            event.get("state_key").is_none()
                && matches!(
                    event["type"].as_str(),
                    Some("m.room.message" | "m.room.encrypted")
                )
        }) {
            return Ok(event);
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("no message reached the server in {room} within {WAIT:?}");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// How many rooms each server gets: the plaintext send was a race with
/// the sync, so one room may miss it.
const FRESH_ROOMS: usize = 8;

/// Every encryption state the SDK gives each of the client's rooms, in
/// order, read every few milliseconds from before a room is made: whether
/// a sync ever made a room look not encrypted, whatever the send did.
struct StateWatch {
    seen: Arc<std::sync::Mutex<std::collections::HashMap<OwnedRoomId, Vec<String>>>>,
    task: tokio::task::JoinHandle<()>,
}

impl StateWatch {
    fn start(client: matrix_sdk::Client) -> Self {
        let seen = Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
            OwnedRoomId,
            Vec<String>,
        >::new()));
        let record = seen.clone();
        let task = tokio::spawn(async move {
            loop {
                for room in client.rooms() {
                    let state = format!("{:?}", room.encryption_state());
                    let mut seen = record
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let states = seen.entry(room.room_id().to_owned()).or_default();
                    if states.last() != Some(&state) {
                        states.push(state);
                    }
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        });
        Self { seen, task }
    }

    fn states(&self, room: &OwnedRoomId) -> Vec<String> {
        self.seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(room)
            .cloned()
            .unwrap_or_default()
    }
}

impl Drop for StateWatch {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// On the server at `homeserver_url`, `user` makes [`FRESH_ROOMS`]
/// encrypted rooms through `SessionApi` and sends into each at once, as
/// the window does. Returns what went wrong, one line per room.
async fn sends_at_once(
    harness: &HomeserverHarness,
    homeserver_url: &str,
    server: &str,
    user: &TestUser,
) -> Result<Vec<String>> {
    let device = Device::open_on(
        harness,
        homeserver_url,
        &format!("fresh-{}", server.to_ascii_lowercase()),
        &user.user_id,
        &user.session,
        AuthKind::Password,
        Some(&user.password),
    )
    .await?;
    let client = device.client()?;
    // The server is read as another sign-in without a refresh token: the
    // device's own access token rotates (Synapse's last 20 seconds).
    let reader = harness
        .login_password_on(homeserver_url, &user.user_id, &user.password, false)
        .await?;
    let watch = StateWatch::start(client.clone());
    let mut problems = Vec::new();
    let mut looked_unencrypted = 0;
    for round in 1..=FRESH_ROOMS {
        let text = format!(
            "a secret for a fresh room {}",
            HomeserverHarness::unique_localpart("text")
        );
        let room = device
            .session
            .create_room(
                &device.key,
                NewRoom {
                    name: Some(format!("Fresh room {round}")),
                    invite: Vec::new(),
                    direct: false,
                    encrypted: true,
                },
            )
            .await
            .context("create_room answers")??;
        let before = client.get_room(&room).map(|room| room.encryption_state());
        device
            .session
            .room_action(
                &device.key,
                &room,
                RoomRequest::Send {
                    message: OutgoingMessage::plain(&text),
                    reply_to: None,
                },
            )
            .await
            .context("room_action answers")??;
        let stored = first_message_on_server(harness, homeserver_url, &reader, &room).await?;
        let after = client.get_room(&room).map(|room| room.encryption_state());
        let kind = stored["type"].as_str().unwrap_or("no type").to_owned();
        let states = watch.states(&room);
        if states.iter().any(|state| state == "NotEncrypted") {
            looked_unencrypted += 1;
        }
        eprintln!(
            "{server}, room {round}: the SDK said {before:?} as the message was queued and \
             {after:?} once it was on the server (every state seen: {states:?}); the server \
             stored {kind}"
        );
        if kind != "m.room.encrypted" {
            problems.push(format!(
                "{server}, room {round}: the server stored {kind} (the SDK said {before:?})"
            ));
        }
        if stored.to_string().contains(&text) {
            problems.push(format!(
                "{server}, room {round}: the server has the message's text"
            ));
        }
    }
    eprintln!(
        "{server}: {looked_unencrypted} of {FRESH_ROOMS} rooms made encrypted looked not \
         encrypted to the SDK at some point"
    );
    drop(watch);
    device.shutdown().await;
    Ok(problems)
}

/// A message sent the moment an encrypted room is made goes out encrypted.
/// A sync answer that comes while the server is still making the room can
/// lack its `m.room.encryption`, and matrix-sdk then takes the room as not
/// encrypted until a later answer says otherwise; a message sent in
/// between went out in plaintext (the key export test's empty export).
/// The server is asked what it stored, on Palpo and on Synapse.
#[tokio::test]
#[ignore = "needs the homeserver harness: scripts/homeserver-harness.sh up"]
async fn messages_sent_at_once_into_a_new_encrypted_room_are_encrypted() -> Result<()> {
    let _logs = captured_logs();
    let harness = harness()?;
    let palpo = harness.new_user("fresh-room").await?;
    let mut problems = sends_at_once(&harness, harness.hs1_url(), "Palpo", &palpo).await?;
    let synapse = harness.new_synapse_user("fresh-room").await?;
    problems.extend(sends_at_once(&harness, harness.synapse_url(), "Synapse", &synapse).await?);
    ensure!(
        problems.is_empty(),
        "messages went out without encryption:\n{}",
        problems.join("\n")
    );
    Ok(())
}
