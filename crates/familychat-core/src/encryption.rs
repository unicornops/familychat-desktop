//! End-to-end encryption as both frontends see it (PLAN.md, section 5.7;
//! ADR 0009): whether this device is confirmed, whether the account has a
//! cross-signing identity and recovery, the verification flows a frontend
//! draws step by step, the account's devices, room key files, the identity
//! warnings a room shows, and why a message can't be read.
//!
//! All crypto is the matrix-rust-sdk's, in `familychat-session`; nothing
//! here names a matrix-sdk type. Like the sign-in, the wording lives here,
//! next to the states, so the window and the terminal UI say the same
//! thing. Secrets in transit (a password answering the server's
//! user-interactive auth, a recovery key, a key file's passphrase) are
//! [`SecretText`]: `Debug` prints `[redacted]` and the text is wiped when
//! dropped. No step, status or error carries one.

use std::fmt;

use ruma::OwnedUserId;
use tokio::sync::{mpsc, watch};

pub use crate::signin::SecretText;

// ---------------------------------------------------------------------------
// The account's encryption status
// ---------------------------------------------------------------------------

/// Whether this device is confirmed: signed by the account's own
/// cross-signing identity, so the account's other devices and the people
/// it talks to trust it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeviceTrust {
    /// Not known yet (the keys are still being fetched).
    #[default]
    Unknown,
    /// Confirmed.
    Verified,
    /// Not confirmed: "Confirm it's you" with another device or the
    /// recovery key.
    Unverified,
}

/// The account's cross-signing identity, as far as this device knows it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum IdentitySetup {
    /// Not known yet.
    #[default]
    Checking,
    /// The account has an identity (made here or on another device).
    Ready,
    /// The account has none yet, and this device is making it.
    SettingUp,
    /// The server asked for the account's password before the identity
    /// can be made, and this device has none (a sign-in with a code). The
    /// way forward is [`SessionApi::set_up_identity`] with the password.
    ///
    /// [`SessionApi::set_up_identity`]: crate::session::SessionApi::set_up_identity
    NeedsPassword,
    /// Making it failed for another reason; the sentence to show, with
    /// Try again.
    Failed(String),
}

/// Recovery: the account's secrets (cross-signing and the key backup's
/// key) kept on the server, encrypted with a recovery key only the person
/// holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecoveryState {
    /// Not known yet.
    #[default]
    Unknown,
    /// Set up, and this device has every secret.
    Enabled,
    /// Not set up.
    Disabled,
    /// Set up, but this device is missing some secrets: entering the
    /// recovery key fetches them.
    Incomplete,
}

/// An account's encryption, as it changes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EncryptionStatus {
    /// This device.
    pub device: DeviceTrust,
    /// The account's identity.
    pub identity: IdentitySetup,
    /// Recovery.
    pub recovery: RecoveryState,
    /// The account has another device that could confirm this one.
    pub other_devices: bool,
}

impl EncryptionStatus {
    /// Whether "Confirm it's you" applies: the account has an identity and
    /// this device isn't confirmed by it.
    pub fn needs_confirmation(&self) -> bool {
        self.identity == IdentitySetup::Ready && self.device == DeviceTrust::Unverified
    }

    /// Whether the recovery key can confirm this device.
    pub fn can_use_recovery_key(&self) -> bool {
        matches!(
            self.recovery,
            RecoveryState::Enabled | RecoveryState::Incomplete
        )
    }

    /// Whether to offer setting recovery up: this device is confirmed and
    /// the account has no recovery yet.
    pub fn offers_recovery_setup(&self) -> bool {
        self.device == DeviceTrust::Verified && self.recovery == RecoveryState::Disabled
    }
}

/// How something that needs the server's user-interactive auth went
/// (making the identity, signing another device out).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthOutcome {
    /// Done.
    Done,
    /// The server asks for the account's password, and none was given.
    PasswordNeeded,
    /// The server refused the password given.
    WrongPassword,
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

/// Who a verification is with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyingWith {
    /// Their user id (the account's own, for one of its devices).
    pub user_id: OwnedUserId,
    /// The other device, once known.
    pub device_id: Option<String>,
    /// The other device's name, once known.
    pub device_name: Option<String>,
    /// It is one of the account's own devices.
    pub own: bool,
}

impl VerifyingWith {
    /// "your other device", or the device's name.
    pub fn label(&self) -> String {
        match (&self.device_name, &self.device_id) {
            (Some(name), _) if !name.trim().is_empty() => name.clone(),
            (_, Some(id)) => id.clone(),
            _ if self.own => "your other device".to_owned(),
            _ => self.user_id.to_string(),
        }
    }
}

/// One emoji of the seven both devices show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SasEmoji {
    /// The emoji.
    pub symbol: String,
    /// Its name (the spec's English description).
    pub description: String,
}

/// A verification QR code, as modules to draw: `width` by `width`, dark
/// where `true`, row by row. It carries a one-time secret of the flow, so
/// `Debug` shows its size only.
#[derive(Clone, PartialEq, Eq)]
pub struct QrModules {
    /// Modules per side.
    pub width: usize,
    /// `width * width` modules, row by row.
    pub dark: Vec<bool>,
}

impl QrModules {
    /// Whether the module at `x`, `y` is dark.
    pub fn is_dark(&self, x: usize, y: usize) -> bool {
        x < self.width && self.dark.get(y * self.width + x).copied().unwrap_or(false)
    }
}

impl fmt::Debug for QrModules {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "QrModules({0}x{0})", self.width)
    }
}

/// What a frontend draws for a verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationStep {
    /// Our request is out; waiting for the other device to answer it.
    Waiting,
    /// Another device asks to verify: [`VerificationAnswer::Accept`] or
    /// [`VerificationAnswer::Cancel`].
    Incoming,
    /// Both sides are ready. `qr`, when there is one, is shown for the
    /// other device (a phone) to scan; [`VerificationAnswer::CompareEmoji`]
    /// compares emoji instead.
    Ready {
        /// The QR code to show.
        qr: Option<QrModules>,
    },
    /// Setting up the emoji comparison.
    Starting,
    /// Compare these with the other device: [`VerificationAnswer::Match`]
    /// or [`VerificationAnswer::NoMatch`].
    CompareEmoji(Vec<SasEmoji>),
    /// Confirmed here; waiting for the other device to confirm too.
    Confirming,
    /// The other device scanned this one's QR code: confirm it says so
    /// ([`VerificationAnswer::ConfirmScanned`]).
    Scanned,
    /// Verified (final).
    Done,
    /// Stopped, with why (final).
    Cancelled(String),
}

impl VerificationStep {
    /// Whether the verification has ended.
    pub fn is_final(&self) -> bool {
        matches!(self, Self::Done | Self::Cancelled(_))
    }
}

/// What a person answers in a verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationAnswer {
    /// Accept an incoming request.
    Accept,
    /// Compare emoji instead of scanning.
    CompareEmoji,
    /// The emoji match.
    Match,
    /// The emoji don't match (the verification is cancelled).
    NoMatch,
    /// The other device says it scanned this one's code.
    ConfirmScanned,
    /// Stop.
    Cancel,
}

/// One verification in progress. Dropping it does not cancel it: send
/// [`VerificationAnswer::Cancel`].
#[derive(Debug, Clone)]
pub struct VerificationFlow {
    /// Who it is with.
    pub with: VerifyingWith,
    steps: watch::Receiver<VerificationStep>,
    answers: mpsc::UnboundedSender<VerificationAnswer>,
}

impl VerificationFlow {
    /// A flow over its channels (for implementations of
    /// [`SessionApi`](crate::session::SessionApi)).
    pub fn new(
        with: VerifyingWith,
        steps: watch::Receiver<VerificationStep>,
        answers: mpsc::UnboundedSender<VerificationAnswer>,
    ) -> Self {
        Self {
            with,
            steps,
            answers,
        }
    }

    /// The steps, as they change.
    pub fn steps(&self) -> watch::Receiver<VerificationStep> {
        self.steps.clone()
    }

    /// The current step.
    pub fn step(&self) -> VerificationStep {
        self.steps.borrow().clone()
    }

    /// Sends an answer; `false` when the flow has ended.
    pub fn answer(&self, answer: VerificationAnswer) -> bool {
        self.answers.send(answer).is_ok()
    }
}

// ---------------------------------------------------------------------------
// Devices, key files and identity warnings
// ---------------------------------------------------------------------------

/// One of the account's devices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceView {
    /// Its device id.
    pub device_id: String,
    /// The name it has, when it has one.
    pub display_name: Option<String>,
    /// This device.
    pub is_this_device: bool,
    /// Confirmed by the account's identity.
    pub verified: bool,
    /// When the server last saw it, in milliseconds since the Unix epoch.
    pub last_seen_ms: Option<u64>,
}

impl DeviceView {
    /// Its name, or its id.
    pub fn label(&self) -> String {
        self.display_name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| self.device_id.clone())
    }
}

/// Why a message the encryption stopped is waiting, and who it is
/// waiting on (ADR 0009, ADR 0015): the "Review" of Send anyway, so the
/// person sees the devices or people before sending to them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoppedMessage {
    /// Why it stopped.
    pub problem: crate::timeline::SendProblem,
    /// The unconfirmed devices it would go to, for
    /// `SendProblem::UnconfirmedDevices`.
    pub devices: Vec<UnconfirmedDevice>,
    /// The people whose identity changed, for
    /// `SendProblem::IdentityChanged`.
    pub people: Vec<StoppedPerson>,
}

/// A verified person's device they haven't confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnconfirmedDevice {
    /// Whose it is.
    pub user_id: OwnedUserId,
    /// Their display name in the room, when known.
    pub user_name: Option<String>,
    /// Its device id.
    pub device_id: String,
    /// Its name, when it has one.
    pub device_name: Option<String>,
    /// The account's own device.
    pub own: bool,
}

impl UnconfirmedDevice {
    /// Who it belongs to: "You" for the account's own, else the name or id.
    pub fn owner_label(&self) -> String {
        if self.own {
            return "You".to_owned();
        }
        self.user_name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| self.user_id.to_string())
    }

    /// The device: its name, or its id.
    pub fn device_label(&self) -> String {
        self.device_name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| self.device_id.clone())
    }
}

/// A verified person whose identity changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoppedPerson {
    /// Who.
    pub user_id: OwnedUserId,
    /// Their display name in the room, when known.
    pub name: Option<String>,
}

impl StoppedPerson {
    /// Their name, or their id.
    pub fn label(&self) -> String {
        self.name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| self.user_id.to_string())
    }
}

/// How a room key import went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyImport {
    /// Keys this device didn't have and now does.
    pub imported: u64,
    /// Keys in the file.
    pub total: u64,
}

impl KeyImport {
    /// What to say about it.
    pub fn message(self) -> String {
        match (self.imported, self.total) {
            (_, 0) => "The file has no keys in it.".to_owned(),
            (0, _) => "This device already had every key in the file.".to_owned(),
            (1, total) => format!("Imported 1 of {total} keys."),
            (imported, total) => format!("Imported {imported} of {total} keys."),
        }
    }
}

/// What changed about someone's identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IdentityWarningKind {
    /// Their identity was reset (someone not verified): new messages to
    /// them wait until the change is acknowledged.
    Changed,
    /// Someone verified reset their identity: nothing is sent to them until
    /// their verification is withdrawn.
    VerificationLost,
}

/// A warning a room shows about one member's identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IdentityWarning {
    /// Whose identity changed.
    pub user_id: OwnedUserId,
    /// Their display name, when known.
    pub display_name: Option<String>,
    /// What changed.
    pub kind: IdentityWarningKind,
}

impl IdentityWarning {
    fn who(&self) -> String {
        self.display_name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| self.user_id.to_string())
    }

    /// The banner's sentence.
    pub fn message(&self) -> String {
        let who = self.who();
        match self.kind {
            IdentityWarningKind::Changed => format!(
                "{who}'s identity was reset. If you didn't expect this, check with them in person \
                 before you share anything private."
            ),
            IdentityWarningKind::VerificationLost => format!(
                "{who}'s verified identity was reset, so nothing is sent to them. Check with them \
                 in person, then withdraw the old verification to send again."
            ),
        }
    }

    /// The banner's button.
    pub fn action_label(&self) -> &'static str {
        match self.kind {
            IdentityWarningKind::Changed => "OK",
            IdentityWarningKind::VerificationLost => "Withdraw verification",
        }
    }
}

// ---------------------------------------------------------------------------
// Unable to decrypt
// ---------------------------------------------------------------------------

/// Why this device can't read a message, from matrix-sdk-ui's
/// unable-to-decrypt cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum UtdReason {
    /// No better explanation: the key may still arrive.
    #[default]
    Waiting,
    /// It was sent before the account joined the room.
    SentBeforeJoined,
    /// The sender's verified identity changed.
    SenderIdentityChanged,
    /// The sender's device isn't confirmed by its owner.
    UnconfirmedSender,
    /// The sender's device couldn't be checked.
    UnknownSender,
    /// It was sent before this device existed, and the account has no key
    /// backup to fetch its key from.
    HistoryNotAvailable,
    /// It was sent before this device existed; once this device is
    /// confirmed, the key backup has its key.
    ConfirmForHistory,
    /// The sender keeps its keys from devices that aren't confirmed.
    ConfirmThisDevice,
    /// The sender didn't share its key with this device.
    WithheldBySender,
}

impl UtdReason {
    /// What the timeline says instead of the message.
    pub fn message(self) -> &'static str {
        match self {
            Self::Waiting => {
                "Waiting for this message's key. It shows once a device that can read it shares \
                 the key."
            }
            Self::SentBeforeJoined => "You can't read this message: it was sent before you joined.",
            Self::SenderIdentityChanged => {
                "This message is hidden: the sender's verified identity has changed."
            }
            Self::UnconfirmedSender => {
                "This message is hidden: it was sent from a device its owner hasn't confirmed."
            }
            Self::UnknownSender => {
                "This message is hidden: the device that sent it couldn't be checked."
            }
            Self::HistoryNotAvailable => {
                "This message was sent before this device signed in, and your account keeps no \
                 backup of its keys."
            }
            Self::ConfirmForHistory => {
                "This message was sent before this device signed in. Confirm it's you with \
                 another device or your recovery key to read older messages."
            }
            Self::ConfirmThisDevice => {
                "The sender shares keys only with confirmed devices. Confirm it's you to read it."
            }
            Self::WithheldBySender => "The sender didn't share this message's key with you.",
        }
    }
}

// ---------------------------------------------------------------------------
// Wording
// ---------------------------------------------------------------------------

/// What the screens and dialogs say, shared by both frontends.
pub mod wording {
    /// The confirmation screen's title.
    pub const CONFIRM_TITLE: &str = "Confirm it's you";
    /// Under it.
    pub const CONFIRM_DETAIL: &str = "Confirm this device so it can read your encrypted messages \
         and your family knows it's really you.";
    /// The option to verify with another device.
    pub const USE_ANOTHER_DEVICE: &str = "Use another device";
    /// Under it.
    pub const USE_ANOTHER_DEVICE_DETAIL: &str = "Compare emoji with a phone or computer that is \
         already signed in, or scan a code with it.";
    /// The option to enter the recovery key.
    pub const USE_RECOVERY_KEY: &str = "Enter your recovery key";
    /// Under it.
    pub const USE_RECOVERY_KEY_DETAIL: &str = "Use the recovery key you saved when you set up \
         recovery.";
    /// Skipping.
    pub const SKIP: &str = "Skip for now";
    /// The warning before skipping.
    pub const SKIP_WARNING_TITLE: &str = "Skip confirming?";
    /// Its sentence.
    pub const SKIP_WARNING: &str = "Until you confirm this device, it can't read messages sent \
         before it signed in, and other people may see a warning next to your messages. You can \
         confirm later from the command palette.";
    /// The banner while this device isn't confirmed.
    pub const NOT_CONFIRMED_BANNER: &str =
        "This device isn't confirmed yet, so some messages may not be readable here.";
    /// The page asking for the password.
    pub const PASSWORD_TITLE: &str = "Your server needs your password";
    /// Its sentence.
    pub const PASSWORD_DETAIL: &str = "Your family's server asks for your password before this \
         device can set up encryption. If you signed in with a code and don't have a password, \
         ask your parent to set one for you in the Family Chat panel, then enter it here.";
    /// The wrong password.
    pub const WRONG_PASSWORD: &str = "That password isn't right. Try again.";
    /// No password typed.
    pub const NO_PASSWORD: &str = "Type your password.";
    /// The identity couldn't be made.
    pub const SETUP_FAILED: &str =
        "Encryption couldn't be set up on this device. Check your connection and try again.";
    /// The banner offering recovery.
    pub const RECOVERY_BANNER: &str = "Set up recovery so you can read your messages on a new \
         device even if this one is lost.";
    /// The recovery dialog's title.
    pub const RECOVERY_TITLE: &str = "Set up recovery";
    /// Its sentence.
    pub const RECOVERY_DETAIL: &str = "Family Chat makes a recovery key and keeps an encrypted \
         copy of your message keys on your server. Only the recovery key can open it.";
    /// Showing the key.
    pub const RECOVERY_KEY_TITLE: &str = "Save your recovery key";
    /// Under it.
    pub const RECOVERY_KEY_DETAIL: &str = "This is the only time it is shown. Keep it somewhere \
         safe, such as a password manager. Anyone with it can read your messages.";
    /// Entering the key.
    pub const ENTER_KEY_TITLE: &str = "Enter your recovery key";
    /// Under it.
    pub const ENTER_KEY_DETAIL: &str =
        "Type or paste the recovery key you saved. Spaces don't matter.";
    /// A key that doesn't open recovery.
    pub const WRONG_RECOVERY_KEY: &str = "That recovery key doesn't match. Check it and try again.";
    /// No key typed.
    pub const NO_RECOVERY_KEY: &str = "Type or paste your recovery key.";
    /// The devices dialog's title.
    pub const DEVICES_TITLE: &str = "Your devices";
    /// Signing another device out.
    pub const SIGN_OUT_DEVICE_DETAIL: &str = "It stops getting your messages. Its own copy of \
         your messages stays on it until someone signs in there again.";
    /// The server needs the password to sign a device out.
    pub const SIGN_OUT_NEEDS_PASSWORD: &str = "Your server asks for your password to sign out \
         another device. If you signed in with a code, ask your parent to sign the device out \
         from the Family Chat panel.";
    /// Exporting keys.
    pub const EXPORT_TITLE: &str = "Export room keys";
    /// Under it.
    pub const EXPORT_DETAIL: &str = "Saves the keys to your encrypted messages in a file \
         protected by a passphrase, so another device or a new address for your family can read \
         them. Anyone with the file and the passphrase can read your messages.";
    /// Importing keys.
    pub const IMPORT_TITLE: &str = "Import room keys";
    /// Under it.
    pub const IMPORT_DETAIL: &str =
        "Reads keys from a file exported earlier, with the passphrase it was saved with.";
    /// No passphrase typed.
    pub const NO_PASSPHRASE: &str = "Type a passphrase.";
    /// The two passphrases differ.
    pub const PASSPHRASES_DIFFER: &str = "The passphrases don't match.";
    /// A passphrase too short.
    pub const PASSPHRASE_TOO_SHORT: &str = "Use a passphrase of at least 8 characters.";
    /// The file couldn't be read with that passphrase.
    pub const IMPORT_FAILED: &str = "That file couldn't be read with this passphrase. Check \
         both and try again.";
    /// The keys couldn't be saved.
    pub const EXPORT_FAILED: &str = "The keys couldn't be saved there. Choose another place.";
    /// The keys were saved.
    pub const EXPORTED: &str = "Room keys saved.";
    /// An incoming request.
    pub const INCOMING_TITLE: &str = "Verification request";
    /// The verification dialog's title.
    pub const VERIFY_TITLE: &str = "Verify this device";
    /// Waiting for the other device.
    pub const WAITING: &str = "Open Family Chat on your other device and accept the request.";
    /// Ready, with a QR code.
    pub const SCAN_THIS: &str = "Scan this code with your other device, or compare emoji instead.";
    /// Ready, without a QR code.
    pub const READY_NO_QR: &str = "Compare emoji with your other device to finish.";
    /// Comparing.
    pub const COMPARE: &str = "Check that these emoji appear on the other device, in the same \
         order.";
    /// Waiting for the other device to confirm.
    pub const CONFIRMING: &str = "Waiting for the other device to confirm…";
    /// The other device scanned the code.
    pub const SCANNED: &str = "Does the other device say the code was scanned?";
    /// Done.
    pub const DONE: &str = "Verified. This device can now read your encrypted messages.";
    /// Done, for another device.
    pub const DONE_OTHER: &str = "Verified. That device is now confirmed.";
    /// A send stopped because a device isn't confirmed.
    pub const UNCONFIRMED_DEVICES_NOT_SENT: &str =
        "Not sent: someone in this room has a device they haven't confirmed.";
    /// A send stopped because a verified identity changed.
    pub const IDENTITY_CHANGED_NOT_SENT: &str =
        "Not sent: a verified person in this room has a new identity.";
    /// A send stopped because this device isn't confirmed.
    pub const CONFIRM_THIS_DEVICE_NOT_SENT: &str = "Not sent: confirm it's you first.";
    /// Sending anyway.
    pub const SEND_ANYWAY: &str = "Send anyway";
    /// The review of a stopped message.
    pub const REVIEW_TITLE: &str = "Send to unconfirmed devices?";
    /// Under it, for unconfirmed devices.
    pub const REVIEW_DEVICES: &str = "These devices haven't been confirmed by the person they \
         belong to. If you send anyway, they can read this message. Confirm your own devices in \
         Settings, Sessions, to stop this happening again.";
    /// The review of a stopped message, for changed identities.
    pub const REVIEW_PEOPLE_TITLE: &str = "Send to a new identity?";
    /// Under it.
    pub const REVIEW_PEOPLE: &str = "These people have a new identity since you verified them. \
         Sending anyway withdraws that verification. Check with them, in person or by phone, if \
         you weren't expecting it.";
    /// Resetting the recovery key.
    pub const RESET_KEY_TITLE: &str = "Change your recovery key?";
    /// Under it.
    pub const RESET_KEY_DETAIL: &str = "Family Chat makes a new recovery key and the old one \
         stops working. Your messages stay readable on the devices you have now.";
    /// Resetting the identity.
    pub const RESET_IDENTITY_TITLE: &str = "Reset your identity?";
    /// Under it.
    pub const RESET_IDENTITY_DETAIL: &str = "Only do this if you have no other confirmed device \
         and no recovery key. Your key backup is deleted, every device you confirmed has to be \
         confirmed again, your family sees that your identity changed, and messages sent before \
         now may stay unreadable here.";
    /// After a reset.
    pub const IDENTITY_RESET: &str = "Your identity was reset. Set up recovery again.";
    /// When the reset needs a sign-in in the browser (OAuth, Phase 14).
    pub const RESET_NEEDS_BROWSER: &str = "Your server wants you to approve this in a browser, \
         which Family Chat can't do yet. Use another Matrix app to reset your identity.";

    /// Why a verification stopped, from the spec's cancel code.
    pub fn cancelled(code: &str, by_us: bool) -> String {
        match code {
            "m.mismatched_sas" | "m.key_mismatch" => {
                "The emoji didn't match, so the device wasn't verified. Try again, and if they \
                 still don't match, someone may be listening in."
                    .to_owned()
            }
            "m.timeout" => "The verification took too long. Start it again.".to_owned(),
            "m.user" if by_us => "You stopped the verification.".to_owned(),
            "m.user" => "The other device stopped the verification.".to_owned(),
            "m.accepted" => "Another of your devices answered the request.".to_owned(),
            _ => "The verification stopped. Start it again.".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmation_needs_an_identity_and_an_unconfirmed_device() {
        let mut status = EncryptionStatus {
            device: DeviceTrust::Unverified,
            identity: IdentitySetup::Ready,
            ..EncryptionStatus::default()
        };
        assert!(status.needs_confirmation());
        status.identity = IdentitySetup::NeedsPassword;
        assert!(!status.needs_confirmation());
        status.identity = IdentitySetup::Ready;
        status.device = DeviceTrust::Verified;
        assert!(!status.needs_confirmation());
        status.recovery = RecoveryState::Disabled;
        assert!(status.offers_recovery_setup());
        assert!(!status.can_use_recovery_key());
        status.recovery = RecoveryState::Incomplete;
        assert!(status.can_use_recovery_key());
    }

    #[test]
    fn qr_codes_and_secrets_never_print() {
        let qr = QrModules {
            width: 2,
            dark: vec![true, false, false, true],
        };
        assert_eq!(format!("{qr:?}"), "QrModules(2x2)");
        assert!(qr.is_dark(0, 0) && qr.is_dark(1, 1) && !qr.is_dark(1, 0));
        assert!(!qr.is_dark(5, 0));
        assert_eq!(format!("{:?}", SecretText::from("EsTc 1234")), "[redacted]");
    }

    #[test]
    fn every_reason_and_warning_has_a_sentence() {
        for reason in [
            UtdReason::Waiting,
            UtdReason::SentBeforeJoined,
            UtdReason::SenderIdentityChanged,
            UtdReason::UnconfirmedSender,
            UtdReason::UnknownSender,
            UtdReason::HistoryNotAvailable,
            UtdReason::ConfirmForHistory,
            UtdReason::ConfirmThisDevice,
            UtdReason::WithheldBySender,
        ] {
            assert!(reason.message().ends_with('.'), "{reason:?}");
        }
        let warning = IdentityWarning {
            user_id: OwnedUserId::try_from("@mum:smith.safechat.family").expect("an id"),
            display_name: Some("Mum".to_owned()),
            kind: IdentityWarningKind::VerificationLost,
        };
        assert!(warning.message().starts_with("Mum's verified identity"));
        assert_eq!(warning.action_label(), "Withdraw verification");
        assert_eq!(
            KeyImport {
                imported: 3,
                total: 5
            }
            .message(),
            "Imported 3 of 5 keys."
        );
        assert!(wording::cancelled("m.mismatched_sas", false).contains("didn't match"));
        assert!(VerificationStep::Done.is_final());
        assert!(!VerificationStep::Waiting.is_final());
    }
}
