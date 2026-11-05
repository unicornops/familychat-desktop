//! Typed secrets that never print and are wiped from memory when dropped.
//!
//! A [`Secret`] holds anything [`Zeroize`] can wipe: a token, a
//! passphrase's bytes. Its `Debug` prints `[redacted]` and nothing else
//! (not even the length), it has no `Display`, `Serialize` or `Clone`
//! unless asked for, and the value is reached only through
//! [`Secret::expose`], so every use is visible in review.
//!
//! The passphrase handling follows Zmail's `zmail-security` `vault.rs`
//! (`StoreKey`: random bytes from the OS, kept as hexadecimal, a `Debug`
//! that hides them), GPL-3.0-only like this project.

use std::fmt;

use zeroize::{Zeroize, Zeroizing};

/// What `Debug` prints for every secret type.
pub const REDACTED: &str = "[redacted]";

/// Length of every passphrase, in bytes (PLAN.md, section 5.4).
pub const PASSPHRASE_LEN: usize = 32;

/// A secret value, wiped when dropped. `Debug` prints [`REDACTED`].
pub struct Secret<T: Zeroize>(T);

impl<T: Zeroize> Secret<T> {
    /// Wraps `value`.
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// The secret itself. Keep the borrow short, and never log it.
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T: Zeroize> Drop for Secret<T> {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl<T: Zeroize> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl<T: Zeroize + Clone> Clone for Secret<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl From<String> for Secret<String> {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl PartialEq for Secret<String> {
    /// Compares in time that depends only on the lengths, so comparing a
    /// token read back from the keychain does not leak how much matched.
    fn eq(&self, other: &Self) -> bool {
        constant_time_eq(self.0.as_bytes(), other.0.as_bytes())
    }
}

impl Eq for Secret<String> {}

/// A passphrase: [`PASSPHRASE_LEN`] random bytes from the OS CSPRNG, kept
/// in the keychain as lower-case hexadecimal. The app store uses the bytes
/// as a raw key; matrix-sdk's stores (Phase 3) take the hexadecimal text.
pub struct Passphrase(Secret<[u8; PASSPHRASE_LEN]>);

impl Passphrase {
    /// A new passphrase from the OS CSPRNG.
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut bytes = Zeroizing::new([0; PASSPHRASE_LEN]);
        getrandom::fill(bytes.as_mut())?;
        Ok(Self(Secret::new(*bytes)))
    }

    /// The raw bytes.
    pub fn bytes(&self) -> &[u8; PASSPHRASE_LEN] {
        self.0.expose()
    }

    /// The passphrase as the keychain stores it (and as matrix-sdk's stores
    /// take it): lower-case hexadecimal. Wiped when dropped.
    pub fn to_hex(&self) -> Zeroizing<String> {
        Zeroizing::new(hex(self.bytes()))
    }

    /// Reads a passphrase from the keychain's hexadecimal text; `None` when
    /// the text is not exactly [`PASSPHRASE_LEN`] bytes of hexadecimal.
    pub fn from_hex(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.len() != PASSPHRASE_LEN * 2 {
            return None;
        }
        let mut bytes = Zeroizing::new([0; PASSPHRASE_LEN]);
        for (index, byte) in bytes.iter_mut().enumerate() {
            let pair = text.get(index * 2..index * 2 + 2)?;
            *byte = u8::from_str_radix(pair, 16).ok()?;
        }
        Some(Self(Secret::new(*bytes)))
    }
}

impl fmt::Debug for Passphrase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl PartialEq for Passphrase {
    fn eq(&self, other: &Self) -> bool {
        constant_time_eq(self.bytes(), other.bytes())
    }
}

impl Eq for Passphrase {}

/// Lower-case hexadecimal of `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

/// Whether `a` and `b` are equal, in time that depends only on their
/// lengths.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_prints_nothing_of_the_secret() {
        let token = Secret::new("syt_c2VjcmV0_token".to_owned());
        assert_eq!(format!("{token:?}"), REDACTED);
        assert_eq!(format!("{token:#?}"), REDACTED);
        // The same for every length, so Debug does not even leak that.
        assert_eq!(format!("{:?}", Secret::new(String::new())), REDACTED);
        let bytes = Secret::new([7u8; 4]);
        assert_eq!(format!("{bytes:?}"), REDACTED);

        let passphrase = Passphrase::generate().expect("the OS CSPRNG works");
        assert_eq!(format!("{passphrase:?}"), REDACTED);
        assert!(!format!("{passphrase:?}").contains(passphrase.to_hex().as_str()));

        // Inside a derived Debug, the field shows as redacted too.
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Login {
            user: &'static str,
            token: Secret<String>,
        }
        let login = Login {
            user: "@alice:example.org",
            token: Secret::new("hunter2-token".to_owned()),
        };
        let debug = format!("{login:?}");
        assert!(!debug.contains("hunter2"), "{debug}");
        assert!(debug.contains(REDACTED), "{debug}");
    }

    #[test]
    fn passphrases_are_random_and_round_trip_through_hex() {
        let first = Passphrase::generate().expect("random");
        let second = Passphrase::generate().expect("random");
        assert_ne!(first, second);
        assert_ne!(first.bytes(), &[0; PASSPHRASE_LEN]);
        let text = first.to_hex();
        assert_eq!(text.len(), PASSPHRASE_LEN * 2);
        assert!(
            text.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_eq!(Passphrase::from_hex(&text).expect("parses"), first);
        assert_eq!(
            Passphrase::from_hex(&format!(" {}\n", text.as_str())).expect("trimmed"),
            first
        );
    }

    #[test]
    fn malformed_passphrase_text_is_refused() {
        assert!(Passphrase::from_hex("").is_none());
        assert!(Passphrase::from_hex("abcd").is_none());
        assert!(Passphrase::from_hex(&"zz".repeat(PASSPHRASE_LEN)).is_none());
        assert!(Passphrase::from_hex(&"ab".repeat(PASSPHRASE_LEN + 1)).is_none());
        // Multi-byte characters never split a pair into a panic.
        assert!(Passphrase::from_hex(&"é".repeat(PASSPHRASE_LEN)).is_none());
    }

    #[test]
    fn secrets_compare_by_value() {
        let a = Secret::new("same".to_owned());
        assert_eq!(a, Secret::new("same".to_owned()));
        assert_ne!(a, Secret::new("diff".to_owned()));
        assert_ne!(a, Secret::new("longer".to_owned()));
        assert_eq!(a.clone(), a);
    }

    /// A value that records being wiped, so the test can see `Drop` call
    /// `zeroize` without reading freed memory.
    struct Witness(std::sync::Arc<std::sync::atomic::AtomicBool>);

    impl Zeroize for Witness {
        fn zeroize(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[test]
    fn dropping_a_secret_wipes_it() {
        let wiped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let secret = Secret::new(Witness(wiped.clone()));
        assert!(!wiped.load(std::sync::atomic::Ordering::SeqCst));
        drop(secret);
        assert!(wiped.load(std::sync::atomic::Ordering::SeqCst));
    }
}
