//! What the sign-in says (PLAN.md, section 5.3: the wording lives next to
//! the rules that produce it, so both frontends say the same thing). The
//! contract fixes two sentences: [`NOT_FAMILY_CHAT`] and the confirmation
//! question; the used-or-expired sentence is PLAN.md's.

use std::time::Duration;

use familychat_core::UserId;

/// The contract's refusal copy, for a homeserver outside the allowlist.
pub const NOT_FAMILY_CHAT: &str = "This server isn't a Family Chat server";
/// A code the server refused with a 401 or 403.
pub const CODE_USED_OR_EXPIRED: &str =
    "That code has been used or has expired. Ask your parent for a new one.";
/// Under the confirmation question.
pub const CONFIRM_DETAIL: &str =
    "Only continue if this is your account and you asked your parent for this code.";
/// The code signed in someone other than the link names; it was signed out.
pub const CODE_FOR_ANOTHER_ACCOUNT: &str = "That code is for a different account than the link names, so Family Chat didn't use it. Ask your parent for a new one.";
/// The link carried a code Family Chat would not send (its `hs` was not a
/// Family Chat server, or the link was incomplete).
pub const CODE_NOT_USED: &str = "Family Chat couldn't use the code in that link. Sign in with your password, or ask your parent for a new code.";
/// Any other failure of a code.
pub const CODE_FAILED: &str = "Family Chat couldn't sign in with that code. Sign in with your password, or ask your parent for a new code.";
/// A second code while one is being redeemed.
pub const CODE_BUSY: &str =
    "Family Chat is already signing in with a code. Wait for it to finish, then try again.";
/// Text that is not a sign-in link.
pub const NOT_A_LINK: &str = "That isn't a Family Chat sign-in link.";
/// A server name that is not a bare host.
pub const INVALID_SERVER: &str =
    "That isn't a server address. Type it like yourfamily.safechat.family.";
/// What the Family Chat server field shows when it is empty. Never the
/// apex `safechat.family`, which is the website (the Android review).
pub const FAMILY_SERVER_PLACEHOLDER: &str = "yourfamily.safechat.family";
/// Another server's homeserver that is not HTTPS.
pub const NOT_HTTPS: &str = "Family Chat connects to servers over HTTPS only.";
/// A password the server refused.
pub const WRONG_PASSWORD: &str = "That username and password don't match.";
/// A deactivated account.
pub const DEACTIVATED: &str = "This account has been deactivated.";
/// Any other refusal of a password sign-in.
pub const SIGNIN_FAILED: &str = "The server didn't accept the sign-in. Try again later.";
/// No username.
pub const NO_USERNAME: &str = "Type your username.";
/// No password.
pub const NO_PASSWORD: &str = "Type your password.";
/// A full Matrix ID on another server than the one typed.
pub const SERVER_MISMATCH: &str =
    "That Matrix ID is on a different server from the address you typed.";
/// A username that is not a localpart or a Matrix ID.
pub const INVALID_USERNAME: &str = "That isn't a username Family Chat can use.";
/// A server without native sliding sync (PLAN.md, Open decision 6).
pub const NOT_SUPPORTED: &str =
    "This server doesn't support the sync Family Chat needs (native sliding sync).";
/// The keychain or the store could not keep the sign-in.
pub const COULD_NOT_SAVE: &str = "Family Chat couldn't save this sign-in. Try again.";

// --- Phase 14: the browser and QR codes (ADR 0010) ----------------------------

/// What "Use another Matrix server"'s server field shows when empty.
pub const OTHER_SERVER_PLACEHOLDER: &str = "matrix.org";
/// A server that offers nothing the app can use.
pub const NO_METHODS: &str =
    "This server doesn't offer a way to sign in that Family Chat supports.";
/// A Family Chat server that took no password and offers the browser.
pub const USE_BROWSER_INSTEAD: &str =
    "Your server signs you in through your browser, not with a password here.";
/// The question before a browser opens.
pub const CONFIRM_BROWSER: &str = "Continue in your browser?";
/// Under it.
pub const CONFIRM_BROWSER_DETAIL: &str =
    "Your browser opens at this address, where you sign in. Only continue if it's your server's.";
/// The question before a browser opens to approve a new device.
pub const CONFIRM_APPROVE: &str = "Approve the new device in your browser?";
/// Under it.
pub const CONFIRM_APPROVE_DETAIL: &str = "Your browser opens at your server's account page, where you approve the new device. Only continue if the codes matched.";
/// While the browser sign-in is open.
pub const BROWSER_WAITING: &str = "Finish signing in with your browser, then come back here.";
/// While a new device is approved in the browser.
pub const APPROVE_WAITING: &str =
    "Approve the new device in your browser. Family Chat sends it your keys once you have.";
/// The sign-in was declined in the browser.
pub const BROWSER_DECLINED: &str = "The sign-in was declined in your browser.";
/// Anything else that went wrong with a browser sign-in.
pub const BROWSER_FAILED: &str =
    "Family Chat couldn't finish the sign-in from your browser. Try again.";
/// The browser took too long.
pub const BROWSER_TIMED_OUT: &str = "The sign-in in your browser took too long. Try again.";
/// A second browser or QR sign-in while one is running.
pub const SIGNIN_BUSY: &str =
    "Family Chat is already signing in. Wait for it to finish, then try again.";
/// The QR code expired.
pub const QR_EXPIRED: &str = "The QR code expired. Try again for a new one.";
/// A check code that is not two digits.
pub const CHECK_CODE_INVALID: &str = "Type the two numbers your other device shows.";
/// The check code did not match.
pub const CHECK_CODE_MISMATCH: &str = "That code didn't match the other device's, so Family Chat stopped. Try again with a new QR code.";
/// The other device declined.
pub const QR_DECLINED: &str = "The sign-in was declined on your other device.";
/// The server can't do QR code sign-in.
pub const QR_NOT_SUPPORTED: &str = "Your server doesn't support signing in with a QR code.";
/// Anything else that went wrong with a QR code sign-in.
pub const QR_FAILED: &str = "Family Chat couldn't sign in with the QR code. Try again.";
/// A new device signed in.
pub const LINKED: &str = "Your new device is signed in, with your keys.";
/// The account's server can't sign a new device in with a QR code.
pub const LINK_NOT_SUPPORTED: &str =
    "This account's server doesn't support signing in a new device with a QR code.";
/// The account's identity is not confirmed here, so there are no keys to send.
pub const LINK_NO_KEYS: &str =
    "Confirm it's you on this computer first: the new device gets its keys from here.";
/// The new device asked to open an address that is not the server's.
pub const LINK_WRONG_ADDRESS: &str = "The new device asked Family Chat to open an address that isn't your server's, so Family Chat stopped.";
/// The new device's sign-in was declined or cancelled.
pub const LINK_DECLINED: &str = "The new device's sign-in was declined.";
/// Anything else that went wrong signing a new device in.
pub const LINK_FAILED: &str = "Family Chat couldn't sign the new device in. Try again.";

/// The confirmation question for a code (contract rule 3).
pub fn confirm_user(user_id: &UserId) -> String {
    format!("Sign in as {user_id}?")
}

/// The confirmation question for a code with no usable hint.
pub fn confirm_server(server: &str) -> String {
    format!("Sign in to {server}?")
}

/// The account is already signed in on this computer (contract rule 2).
pub fn already_signed_in(user_id: &UserId) -> String {
    format!("You're already signed in as {user_id} on this computer.")
}

/// The server could not be reached.
pub fn unreachable(server: &str) -> String {
    format!("Family Chat couldn't reach {server}. Check your connection and try again.")
}

/// The server is not a Matrix server.
pub fn not_a_matrix_server(server: &str) -> String {
    format!("Family Chat couldn't find a Matrix server at {server}.")
}

/// `M_LIMIT_EXCEEDED`, with the wait when the server gave one. The
/// sign-in never retries on its own.
pub fn rate_limited(wait: Option<Duration>) -> String {
    match wait.map(|wait| wait.as_secs_f64().ceil() as u64) {
        Some(0 | 1) => "Too many sign-in attempts. Try again in a second.".to_owned(),
        Some(seconds @ 2..=90) => {
            format!("Too many sign-in attempts. Try again in {seconds} seconds.")
        }
        Some(seconds) => {
            let minutes = seconds.div_ceil(60);
            format!("Too many sign-in attempts. Try again in {minutes} minutes.")
        }
        None => "Too many sign-in attempts. Try again in a little while.".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limits_say_how_long() {
        assert_eq!(
            rate_limited(Some(Duration::from_millis(4200))),
            "Too many sign-in attempts. Try again in 5 seconds."
        );
        assert_eq!(
            rate_limited(Some(Duration::from_millis(300))),
            "Too many sign-in attempts. Try again in a second."
        );
        assert_eq!(
            rate_limited(Some(Duration::from_secs(600))),
            "Too many sign-in attempts. Try again in 10 minutes."
        );
        assert_eq!(
            rate_limited(None),
            "Too many sign-in attempts. Try again in a little while."
        );
    }

    #[test]
    fn the_browser_and_qr_sentences_name_no_secret_and_end_cleanly() {
        for sentence in [
            NO_METHODS,
            USE_BROWSER_INSTEAD,
            CONFIRM_BROWSER_DETAIL,
            CONFIRM_APPROVE_DETAIL,
            BROWSER_WAITING,
            APPROVE_WAITING,
            BROWSER_DECLINED,
            BROWSER_FAILED,
            BROWSER_TIMED_OUT,
            SIGNIN_BUSY,
            QR_EXPIRED,
            CHECK_CODE_INVALID,
            CHECK_CODE_MISMATCH,
            QR_DECLINED,
            QR_NOT_SUPPORTED,
            QR_FAILED,
            LINKED,
            LINK_NOT_SUPPORTED,
            LINK_NO_KEYS,
            LINK_WRONG_ADDRESS,
            LINK_DECLINED,
            LINK_FAILED,
        ] {
            assert!(sentence.ends_with('.'), "{sentence}");
            assert!(!sentence.contains("  "), "{sentence}");
            // No platform words: the same sentence on every OS (and in the
            // terminal), so the snapshots hold everywhere.
            for word in ["click", "tap", "Windows", "macOS", "Linux"] {
                assert!(!sentence.contains(word), "{sentence}");
            }
        }
        assert!(CONFIRM_BROWSER.ends_with('?') && CONFIRM_APPROVE.ends_with('?'));
    }

    #[test]
    fn the_contract_sentences() {
        assert_eq!(NOT_FAMILY_CHAT, "This server isn't a Family Chat server");
        let user = <&UserId>::try_from("@ana:smith.safechat.family").expect("valid");
        assert_eq!(confirm_user(user), "Sign in as @ana:smith.safechat.family?");
        assert_eq!(confirm_server("smith.ie"), "Sign in to smith.ie?");
        assert!(!FAMILY_SERVER_PLACEHOLDER.starts_with("safechat.family"));
    }
}
