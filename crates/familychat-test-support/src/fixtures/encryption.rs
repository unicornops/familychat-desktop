//! The fixture session's end-to-end encryption (ADR 0009): each account's
//! [`EncryptionStatus`], scripted verification flows, the Smiths' devices,
//! recovery with a fixed key, room key files and identity warnings. It says
//! what the app says (`familychat-core`'s wording) and records what the
//! window asked for, never a secret.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use familychat_core::{
    OwnedRoomId, OwnedUserId, UserId,
    encryption::{
        AuthOutcome, DeviceTrust, DeviceView, EncryptionStatus, IdentitySetup, IdentityWarning,
        IdentityWarningKind, KeyImport, QrModules, RecoveryState, SasEmoji, SecretText,
        VerificationAnswer, VerificationFlow, VerificationStep, VerifyingWith, wording,
    },
    session::{AccountKey, IncomingVerification, SUBSCRIPTION_BUFFER, SessionError},
};
use tokio::sync::{mpsc, watch};

use super::{lock, rooms};

/// The recovery key the fixture makes, and accepts.
pub const FIXTURE_RECOVERY_KEY: &str =
    "EsTj 3yST y93F SLpB jJsz eAXc 2XzA ygD3 w69H fGaN TKBj jXEd";

/// The password the fixture's user-interactive auth accepts.
pub const FIXTURE_PASSWORD: &str = "fixture-password";

/// The seven emoji the fixture's verifications compare.
pub fn fixture_emoji() -> Vec<SasEmoji> {
    [
        ("🐶", "Dog"),
        ("🔑", "Key"),
        ("🍕", "Pizza"),
        ("🚀", "Rocket"),
        ("🎸", "Guitar"),
        ("☂️", "Umbrella"),
        ("🦋", "Butterfly"),
    ]
    .into_iter()
    .map(|(symbol, description)| SasEmoji {
        symbol: symbol.to_owned(),
        description: description.to_owned(),
    })
    .collect()
}

/// A QR code's shape, drawn procedurally: finder patterns in three corners
/// and a deterministic pattern elsewhere (not a scannable code).
pub fn fixture_qr() -> QrModules {
    const WIDTH: usize = 33;
    let finder = |x: usize, y: usize| -> Option<bool> {
        let corners = [(0, 0), (WIDTH - 7, 0), (0, WIDTH - 7)];
        corners.iter().find_map(|&(cx, cy)| {
            if x >= cx && x < cx + 7 && y >= cy && y < cy + 7 {
                let (dx, dy) = (x - cx, y - cy);
                let ring = dx == 0 || dy == 0 || dx == 6 || dy == 6;
                let core = (2..=4).contains(&dx) && (2..=4).contains(&dy);
                Some(ring || core)
            } else if x + 1 >= cx && x <= cx + 7 && y + 1 >= cy && y <= cy + 7 {
                Some(false)
            } else {
                None
            }
        })
    };
    let dark = (0..WIDTH * WIDTH)
        .map(|index| {
            let (x, y) = (index % WIDTH, index / WIDTH);
            finder(x, y).unwrap_or_else(|| {
                (x * 7 + y * 13 + (x * y) % 5).is_multiple_of(3) || (x + y) % 11 == 0
            })
        })
        .collect();
    QrModules { width: WIDTH, dark }
}

/// The Smiths' devices: Ana's laptop (this one), her phone, and an old
/// tablet she never confirmed.
pub fn fixture_devices() -> Vec<DeviceView> {
    let now = rooms::now_ms();
    vec![
        DeviceView {
            device_id: "FAMLAPTOP".to_owned(),
            display_name: Some("Family Chat (Linux)".to_owned()),
            is_this_device: true,
            verified: true,
            last_seen_ms: Some(now),
        },
        DeviceView {
            device_id: "ANAPHONE".to_owned(),
            display_name: Some("Ana's phone".to_owned()),
            is_this_device: false,
            verified: true,
            last_seen_ms: Some(now.saturating_sub(2 * 60 * 60 * 1000)),
        },
        DeviceView {
            device_id: "OLDTABLET".to_owned(),
            display_name: Some("Old tablet".to_owned()),
            is_this_device: false,
            verified: false,
            last_seen_ms: Some(now.saturating_sub(40 * 24 * 60 * 60 * 1000)),
        },
    ]
}

/// What the window asked of the fixture's encryption (never a secret).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncryptionAsk {
    /// `set_up_identity`, with or without a password.
    SetUpIdentity {
        /// A password was given.
        with_password: bool,
    },
    /// `verify_this_device`.
    VerifyThisDevice,
    /// `verify_device`.
    VerifyDevice(String),
    /// A verification answer.
    Answer(VerificationAnswer),
    /// `enable_recovery`.
    EnableRecovery,
    /// `recover`, with the right key or not.
    Recover {
        /// The key was the fixture's.
        right_key: bool,
    },
    /// `devices`.
    Devices,
    /// `sign_out_device`.
    SignOutDevice {
        /// The device.
        device_id: String,
        /// A password was given.
        with_password: bool,
    },
    /// `export_room_keys`, to this path.
    Export(PathBuf),
    /// `import_room_keys`, from this path.
    Import(PathBuf),
    /// `acknowledge_identity`.
    Acknowledge(OwnedUserId),
    /// `reset_recovery_key`.
    ResetRecoveryKey,
    /// `reset_identity`, with or without a password.
    ResetIdentity {
        /// A password was given.
        with_password: bool,
    },
    /// `stopped_message`.
    StoppedMessage,
}

/// The new recovery key a reset gives (not the first one).
pub const FIXTURE_NEW_RECOVERY_KEY: &str =
    "EsU7 Xq2d Lm4P 9vRt Hc8B wN3y Ka6F Jp5D Zs1G Tb4M Qe7V Rx2H";

/// Who an unconfirmed-devices stop waits on: Ana's own old tablet, and
/// Dad's new phone.
pub fn fixture_stopped_devices() -> Vec<familychat_core::encryption::UnconfirmedDevice> {
    use familychat_core::encryption::UnconfirmedDevice;
    vec![
        UnconfirmedDevice {
            user_id: OwnedUserId::try_from("@ana:smith.safechat.family").expect("an id"),
            user_name: Some("Ana".to_owned()),
            device_id: "OLDTABLET".to_owned(),
            device_name: Some("Old tablet".to_owned()),
            own: true,
        },
        UnconfirmedDevice {
            user_id: OwnedUserId::try_from("@dad:smith.safechat.family").expect("an id"),
            user_name: Some("Dad".to_owned()),
            device_id: "DADPHONE2".to_owned(),
            device_name: Some("Dad's new phone".to_owned()),
            own: false,
        },
    ]
}

/// The fixture session's encryption.
#[derive(Default)]
pub(crate) struct FixtureEncryption {
    statuses: Mutex<BTreeMap<AccountKey, watch::Sender<EncryptionStatus>>>,
    devices: Mutex<Option<Vec<DeviceView>>>,
    warnings: Mutex<BTreeMap<OwnedRoomId, watch::Sender<Vec<IdentityWarning>>>>,
    first_step: Arc<Mutex<Option<VerificationStep>>>,
    requests: Mutex<Option<mpsc::Sender<IncomingVerification>>>,
    /// Requests made before a frontend listened, delivered when one does.
    pending: Mutex<Vec<IncomingVerification>>,
    asks: Arc<Mutex<Vec<EncryptionAsk>>>,
    fail_with: Mutex<Option<SessionError>>,
}

/// A confirmed device of an account with recovery: what every fixture
/// account starts with, so screens that aren't about encryption show none
/// of its banners.
pub fn settled() -> EncryptionStatus {
    EncryptionStatus {
        device: DeviceTrust::Verified,
        identity: IdentitySetup::Ready,
        recovery: RecoveryState::Enabled,
        other_devices: true,
    }
}

impl FixtureEncryption {
    pub(crate) fn add_account(&self, account: &AccountKey) {
        lock(&self.statuses).insert(account.clone(), watch::channel(settled()).0);
    }

    pub(crate) fn set_status(&self, account: &AccountKey, status: EncryptionStatus) {
        if let Some(sender) = lock(&self.statuses).get(account) {
            sender.send_replace(status);
        }
    }

    fn update(&self, account: &AccountKey, change: impl FnOnce(&mut EncryptionStatus)) {
        if let Some(sender) = lock(&self.statuses).get(account) {
            sender.send_modify(change);
        }
    }

    pub(crate) fn status(&self, account: &AccountKey) -> Option<watch::Receiver<EncryptionStatus>> {
        lock(&self.statuses)
            .get(account)
            .map(watch::Sender::subscribe)
    }

    pub(crate) fn set_first_step(&self, step: Option<VerificationStep>) {
        *lock(&self.first_step) = step;
    }

    pub(crate) fn set_warnings(&self, room: OwnedRoomId, warnings: Vec<IdentityWarning>) {
        lock(&self.warnings)
            .entry(room)
            .or_insert_with(|| watch::channel(Vec::new()).0)
            .send_replace(warnings);
    }

    pub(crate) fn fail(&self, error: Option<SessionError>) {
        *lock(&self.fail_with) = error;
    }

    pub(crate) fn asks(&self) -> Vec<EncryptionAsk> {
        lock(&self.asks).clone()
    }

    fn ask(&self, ask: EncryptionAsk) -> Result<(), SessionError> {
        lock(&self.asks).push(ask);
        match lock(&self.fail_with).clone() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn known(&self, account: &AccountKey) -> Result<(), SessionError> {
        if lock(&self.statuses).contains_key(account) {
            Ok(())
        } else {
            Err(SessionError::UnknownAccount)
        }
    }

    pub(crate) fn set_up_identity(
        &self,
        account: &AccountKey,
        password: Option<SecretText>,
    ) -> Result<AuthOutcome, SessionError> {
        self.known(account)?;
        let with_password = password.is_some();
        self.ask(EncryptionAsk::SetUpIdentity { with_password })?;
        let outcome = match password {
            None => AuthOutcome::PasswordNeeded,
            Some(password) if password.expose() == FIXTURE_PASSWORD => AuthOutcome::Done,
            Some(_) => AuthOutcome::WrongPassword,
        };
        self.update(account, |status| {
            if outcome == AuthOutcome::Done {
                status.identity = IdentitySetup::Ready;
                status.device = DeviceTrust::Verified;
            } else {
                status.identity = IdentitySetup::NeedsPassword;
            }
        });
        Ok(outcome)
    }

    /// A scripted verification: each answer moves it on as a real one
    /// would, on a thread of its own.
    pub(crate) fn flow(
        &self,
        account: &AccountKey,
        with: VerifyingWith,
        incoming: bool,
    ) -> VerificationFlow {
        let start = lock(&self.first_step).take().unwrap_or(if incoming {
            VerificationStep::Incoming
        } else {
            VerificationStep::Waiting
        });
        let (steps, steps_rx) = watch::channel(start);
        let (answers_tx, mut answers) = mpsc::unbounded_channel();
        let asks = self.asks.clone();
        let verified = lock(&self.statuses).get(account).cloned();
        let own_device = !incoming && with.device_id.is_none();
        let name = format!("{}-verification", env!("CARGO_PKG_NAME"));
        let spawned = thread::Builder::new().name(name).spawn(move || {
            while let Some(answer) = answers.blocking_recv() {
                lock(&asks).push(EncryptionAsk::Answer(answer));
                let current = steps.borrow().clone();
                let next = match (answer, current) {
                    (VerificationAnswer::Accept, VerificationStep::Incoming) => {
                        VerificationStep::Ready {
                            qr: Some(fixture_qr()),
                        }
                    }
                    (VerificationAnswer::CompareEmoji, VerificationStep::Ready { .. }) => {
                        VerificationStep::CompareEmoji(fixture_emoji())
                    }
                    (VerificationAnswer::Match, VerificationStep::CompareEmoji(_))
                    | (VerificationAnswer::ConfirmScanned, VerificationStep::Scanned) => {
                        VerificationStep::Done
                    }
                    (VerificationAnswer::NoMatch, VerificationStep::CompareEmoji(_)) => {
                        VerificationStep::Cancelled(wording::cancelled("m.mismatched_sas", true))
                    }
                    (VerificationAnswer::Cancel, step) if !step.is_final() => {
                        VerificationStep::Cancelled(wording::cancelled("m.user", true))
                    }
                    (_, step) => step,
                };
                if next == VerificationStep::Done
                    && own_device
                    && let Some(status) = &verified
                {
                    status.send_modify(|status| status.device = DeviceTrust::Verified);
                }
                let done = next.is_final();
                steps.send_replace(next);
                if done {
                    break;
                }
            }
            // Keep the last step for whoever still looks at it.
            thread::sleep(Duration::from_millis(1));
        });
        if let Err(error) = spawned {
            panic!("the fixture verification thread starts: {error}");
        }
        VerificationFlow::new(with, steps_rx, answers_tx)
    }

    pub(crate) fn verify_this_device(
        &self,
        account: &AccountKey,
        user: OwnedUserId,
    ) -> Result<VerificationFlow, SessionError> {
        self.known(account)?;
        self.ask(EncryptionAsk::VerifyThisDevice)?;
        Ok(self.flow(
            account,
            VerifyingWith {
                user_id: user,
                device_id: None,
                device_name: None,
                own: true,
            },
            false,
        ))
    }

    pub(crate) fn verify_device(
        &self,
        account: &AccountKey,
        user: OwnedUserId,
        device_id: &str,
    ) -> Result<VerificationFlow, SessionError> {
        self.known(account)?;
        self.ask(EncryptionAsk::VerifyDevice(device_id.to_owned()))?;
        let name = self
            .device_list()
            .into_iter()
            .find(|device| device.device_id == device_id)
            .and_then(|device| device.display_name);
        Ok(self.flow(
            account,
            VerifyingWith {
                user_id: user,
                device_id: Some(device_id.to_owned()),
                device_name: name,
                own: true,
            },
            false,
        ))
    }

    pub(crate) fn subscribe_requests(&self) -> mpsc::Receiver<IncomingVerification> {
        let (sender, receiver) = mpsc::channel(SUBSCRIPTION_BUFFER);
        for pending in lock(&self.pending).drain(..) {
            let _ = sender.try_send(pending);
        }
        *lock(&self.requests) = Some(sender);
        receiver
    }

    /// Another of the account's devices asks to verify. Before a frontend
    /// listens, the request waits for one (`true`); `false` when the one
    /// listening has gone.
    pub(crate) fn push_request(&self, account: &AccountKey, user: OwnedUserId) -> bool {
        let flow = self.flow(
            account,
            VerifyingWith {
                user_id: user,
                device_id: Some("ANAPHONE".to_owned()),
                device_name: Some("Ana's phone".to_owned()),
                own: true,
            },
            true,
        );
        let requests = lock(&self.requests);
        match requests.as_ref() {
            Some(sender) => sender.try_send((account.clone(), flow)).is_ok(),
            None => {
                lock(&self.pending).push((account.clone(), flow));
                true
            }
        }
    }

    pub(crate) fn enable_recovery(&self, account: &AccountKey) -> Result<SecretText, SessionError> {
        self.known(account)?;
        self.ask(EncryptionAsk::EnableRecovery)?;
        self.update(account, |status| status.recovery = RecoveryState::Enabled);
        Ok(SecretText::from(FIXTURE_RECOVERY_KEY))
    }

    pub(crate) fn recover(
        &self,
        account: &AccountKey,
        key: SecretText,
    ) -> Result<(), SessionError> {
        self.known(account)?;
        let typed: String = key.expose().split_whitespace().collect();
        let right_key = typed == FIXTURE_RECOVERY_KEY.split_whitespace().collect::<String>();
        drop(key);
        self.ask(EncryptionAsk::Recover { right_key })?;
        if typed.is_empty() {
            return Err(SessionError::Failed(wording::NO_RECOVERY_KEY.to_owned()));
        }
        if !right_key {
            return Err(SessionError::Failed(wording::WRONG_RECOVERY_KEY.to_owned()));
        }
        self.update(account, |status| {
            status.device = DeviceTrust::Verified;
            status.recovery = RecoveryState::Enabled;
        });
        Ok(())
    }

    pub(crate) fn reset_recovery_key(
        &self,
        account: &AccountKey,
    ) -> Result<SecretText, SessionError> {
        self.known(account)?;
        self.ask(EncryptionAsk::ResetRecoveryKey)?;
        self.update(account, |status| status.recovery = RecoveryState::Enabled);
        Ok(SecretText::from(FIXTURE_NEW_RECOVERY_KEY))
    }

    pub(crate) fn reset_identity(
        &self,
        account: &AccountKey,
        password: Option<SecretText>,
    ) -> Result<AuthOutcome, SessionError> {
        self.known(account)?;
        self.ask(EncryptionAsk::ResetIdentity {
            with_password: password.is_some(),
        })?;
        Ok(match password {
            None => AuthOutcome::PasswordNeeded,
            Some(password) if password.expose() == FIXTURE_PASSWORD => {
                // A new identity: this device made it, so it is confirmed,
                // and the backup went with the old one.
                self.update(account, |status| {
                    status.device = DeviceTrust::Verified;
                    status.recovery = RecoveryState::Disabled;
                });
                AuthOutcome::Done
            }
            Some(_) => AuthOutcome::WrongPassword,
        })
    }

    pub(crate) fn stopped(&self, account: &AccountKey) -> Result<(), SessionError> {
        self.known(account)?;
        self.ask(EncryptionAsk::StoppedMessage)
    }

    fn device_list(&self) -> Vec<DeviceView> {
        lock(&self.devices).clone().unwrap_or_else(fixture_devices)
    }

    pub(crate) fn devices(&self, account: &AccountKey) -> Result<Vec<DeviceView>, SessionError> {
        self.known(account)?;
        self.ask(EncryptionAsk::Devices)?;
        Ok(self.device_list())
    }

    pub(crate) fn sign_out_device(
        &self,
        account: &AccountKey,
        device_id: &str,
        password: Option<SecretText>,
    ) -> Result<AuthOutcome, SessionError> {
        self.known(account)?;
        self.ask(EncryptionAsk::SignOutDevice {
            device_id: device_id.to_owned(),
            with_password: password.is_some(),
        })?;
        Ok(match password {
            None => AuthOutcome::PasswordNeeded,
            Some(password) if password.expose() == FIXTURE_PASSWORD => {
                let mut devices = self.device_list();
                devices.retain(|device| device.device_id != device_id);
                *lock(&self.devices) = Some(devices);
                AuthOutcome::Done
            }
            Some(_) => AuthOutcome::WrongPassword,
        })
    }

    pub(crate) fn export(
        &self,
        account: &AccountKey,
        path: PathBuf,
        passphrase: SecretText,
    ) -> Result<(), SessionError> {
        self.known(account)?;
        let short = passphrase.expose().chars().count() < 8;
        drop(passphrase);
        self.ask(EncryptionAsk::Export(path))?;
        if short {
            return Err(SessionError::Failed(
                wording::PASSPHRASE_TOO_SHORT.to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn import(
        &self,
        account: &AccountKey,
        path: PathBuf,
        passphrase: SecretText,
    ) -> Result<KeyImport, SessionError> {
        self.known(account)?;
        let empty = passphrase.expose().is_empty();
        drop(passphrase);
        self.ask(EncryptionAsk::Import(path))?;
        if empty {
            return Err(SessionError::Failed(wording::NO_PASSPHRASE.to_owned()));
        }
        Ok(KeyImport {
            imported: 12,
            total: 14,
        })
    }

    pub(crate) fn warnings(&self, room: &OwnedRoomId) -> watch::Receiver<Vec<IdentityWarning>> {
        lock(&self.warnings)
            .entry(room.clone())
            .or_insert_with(|| watch::channel(Vec::new()).0)
            .subscribe()
    }

    pub(crate) fn acknowledge(&self, user: &UserId) -> Result<(), SessionError> {
        self.ask(EncryptionAsk::Acknowledge(user.to_owned()))?;
        for sender in lock(&self.warnings).values() {
            sender.send_modify(|warnings| warnings.retain(|warning| warning.user_id != user));
        }
        Ok(())
    }
}

/// The family room's identity warnings: Grandad reset his identity, and
/// Mum, whom Ana verified, did too.
pub fn family_warnings() -> Vec<IdentityWarning> {
    vec![
        IdentityWarning {
            user_id: rooms::user("grandad"),
            display_name: Some("Grandad".to_owned()),
            kind: IdentityWarningKind::Changed,
        },
        IdentityWarning {
            user_id: rooms::user("mum"),
            display_name: Some("Mum".to_owned()),
            kind: IdentityWarningKind::VerificationLost,
        },
    ]
}
