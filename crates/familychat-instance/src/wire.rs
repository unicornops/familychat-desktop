//! The hand-off's messages (PLAN.md, section 5.5): one length-prefixed
//! JSON request from the second launch, one length-prefixed JSON reply
//! from the running window.
//!
//! ```text
//! u32 big-endian length, then that many bytes of JSON
//! request: {"v":1,"link":"familychat://safechat.family/app/login?..."}
//!          {"v":1}                       (come forward, nothing else)
//! reply:   {"v":1,"accepted":true}
//! ```
//!
//! The receiver is strict: a frame over [`MAX_FRAME_BYTES`], JSON that is
//! not exactly one of these (unknown fields, another version), a link
//! over [`MAX_LINK_BYTES`] or not shaped like a sign-in link, or a request
//! that doesn't arrive within [`READ_TIMEOUT`] is refused, and nothing is
//! delivered. No error carries any of the message's text, and the buffers
//! a link passed through are wiped.

use std::time::Duration;

use familychat_core::{
    requests::{AppRequest, MAX_LINK_BYTES, looks_like_signin_link},
    signin::SecretText,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use zeroize::Zeroize;

/// The protocol's version.
pub const PROTOCOL_VERSION: u32 = 1;

/// The largest message either side accepts.
pub const MAX_FRAME_BYTES: usize = 8 * 1024;

/// How long the running window waits for a request, and a second launch
/// for the reply.
pub const READ_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRequest {
    v: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    link: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReply {
    v: u32,
    accepted: bool,
}

/// Why a message was refused. Carries none of its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    /// The length prefix is over [`MAX_FRAME_BYTES`].
    #[error("the message is too large")]
    TooLarge,
    /// Not the JSON the protocol expects.
    #[error("the message is malformed")]
    Malformed,
    /// Another protocol version.
    #[error("the message is from another version of Family Chat")]
    Version,
    /// The link is too long, or not shaped like a sign-in link.
    #[error("the message's link is not a sign-in link")]
    NotALink,
    /// The connection closed before the message was whole.
    #[error("the connection closed early")]
    Closed,
    /// Nothing arrived in time.
    #[error("the message did not arrive in time")]
    TimedOut,
}

/// The request's JSON, framed. The caller wipes it once sent.
pub(crate) fn encode_request(request: &AppRequest) -> Vec<u8> {
    let link = match request {
        AppRequest::OpenLink(link) => Some(link.expose().to_owned()),
        AppRequest::Activate => None,
    };
    let mut wire = WireRequest {
        v: PROTOCOL_VERSION,
        link,
    };
    let json = serde_json::to_vec(&wire).unwrap_or_default();
    if let Some(link) = wire.link.as_mut() {
        link.zeroize();
    }
    let framed = frame(&json);
    wipe(json);
    framed
}

/// A request's JSON (unframed), checked.
pub(crate) fn decode_request(json: &[u8]) -> Result<AppRequest, WireError> {
    // An object, not the same fields as an array (which serde would also
    // read into the struct).
    if json.first() != Some(&b'{') {
        return Err(WireError::Malformed);
    }
    let wire: WireRequest = serde_json::from_slice(json).map_err(|_| WireError::Malformed)?;
    if wire.v != PROTOCOL_VERSION {
        if let Some(mut link) = wire.link {
            link.zeroize();
        }
        return Err(WireError::Version);
    }
    match wire.link {
        None => Ok(AppRequest::Activate),
        Some(link) => {
            let link = SecretText::new(link);
            if link.expose().len() > MAX_LINK_BYTES || !looks_like_signin_link(link.expose()) {
                return Err(WireError::NotALink);
            }
            Ok(AppRequest::OpenLink(link))
        }
    }
}

/// A reply, framed.
pub(crate) fn encode_reply(accepted: bool) -> Vec<u8> {
    let json = serde_json::to_vec(&WireReply {
        v: PROTOCOL_VERSION,
        accepted,
    })
    .unwrap_or_default();
    frame(&json)
}

/// A reply's JSON (unframed): whether the window took the request.
pub(crate) fn decode_reply(json: &[u8]) -> Result<bool, WireError> {
    if json.first() != Some(&b'{') {
        return Err(WireError::Malformed);
    }
    let reply: WireReply = serde_json::from_slice(json).map_err(|_| WireError::Malformed)?;
    if reply.v != PROTOCOL_VERSION {
        return Err(WireError::Version);
    }
    Ok(reply.accepted)
}

fn frame(json: &[u8]) -> Vec<u8> {
    let length = u32::try_from(json.len()).unwrap_or(u32::MAX);
    let mut framed = Vec::with_capacity(4 + json.len());
    framed.extend_from_slice(&length.to_be_bytes());
    framed.extend_from_slice(json);
    framed
}

/// Wipes a buffer that held a link.
pub(crate) fn wipe(mut bytes: Vec<u8>) {
    bytes.zeroize();
}

/// Reads one frame (within [`READ_TIMEOUT`]).
pub(crate) async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Vec<u8>, WireError> {
    tokio::time::timeout(READ_TIMEOUT, async {
        let mut length = [0_u8; 4];
        reader
            .read_exact(&mut length)
            .await
            .map_err(|_| WireError::Closed)?;
        let length = usize::try_from(u32::from_be_bytes(length)).unwrap_or(usize::MAX);
        if length > MAX_FRAME_BYTES {
            return Err(WireError::TooLarge);
        }
        let mut body = vec![0_u8; length];
        if reader.read_exact(&mut body).await.is_err() {
            wipe(body);
            return Err(WireError::Closed);
        }
        Ok(body)
    })
    .await
    .map_err(|_| WireError::TimedOut)?
}

/// Writes `bytes` and flushes.
pub(crate) async fn write_all<W: AsyncWrite + Unpin>(
    writer: &mut W,
    bytes: &[u8],
) -> std::io::Result<()> {
    writer.write_all(bytes).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINK: &str = "familychat://safechat.family/app/login?account_provider=smith.safechat.family&login_hint=mxid:@ana:smith.safechat.family&hs=hs1.smith.safechat.family&token=secret-token";

    fn unframed(framed: &[u8]) -> &[u8] {
        &framed[4..]
    }

    #[test]
    fn requests_round_trip() {
        let framed = encode_request(&AppRequest::OpenLink(SecretText::from(LINK)));
        let length = u32::from_be_bytes([framed[0], framed[1], framed[2], framed[3]]);
        assert_eq!(usize::try_from(length).ok(), Some(framed.len() - 4));
        match decode_request(unframed(&framed)) {
            Ok(AppRequest::OpenLink(link)) => assert_eq!(link.expose(), LINK),
            other => panic!("expected the link, got {other:?}"),
        }
        let framed = encode_request(&AppRequest::Activate);
        assert_eq!(unframed(&framed), br#"{"v":1}"#);
        assert!(matches!(
            decode_request(unframed(&framed)),
            Ok(AppRequest::Activate)
        ));
        assert_eq!(decode_reply(unframed(&encode_reply(true))), Ok(true));
        assert_eq!(decode_reply(unframed(&encode_reply(false))), Ok(false));
    }

    #[test]
    fn anything_else_is_refused() {
        let long = format!("{LINK}{}", "a".repeat(MAX_LINK_BYTES));
        let cases: [(&[u8], WireError); 9] = [
            (b"", WireError::Malformed),
            (b"not json", WireError::Malformed),
            (br#"{"v":1,"link":"x","extra":true}"#, WireError::Malformed),
            (
                br#"{"link":"familychat://safechat.family/app/login"}"#,
                WireError::Malformed,
            ),
            (br#"{"v":"1"}"#, WireError::Malformed),
            (br#"[1]"#, WireError::Malformed),
            (br#"{"v":2}"#, WireError::Version),
            (
                br#"{"v":1,"link":"https://evil.example/app/login?token=x"}"#,
                WireError::NotALink,
            ),
            (
                br#"{"v":1,"link":"familychat://x\u0000y"}"#,
                WireError::NotALink,
            ),
        ];
        for (json, expected) in cases {
            let decoded = decode_request(json);
            assert!(
                matches!(&decoded, Err(error) if *error == expected),
                "{}: {decoded:?}",
                String::from_utf8_lossy(json)
            );
        }
        let json = serde_json::to_vec(&WireRequest {
            v: 1,
            link: Some(long),
        })
        .expect("serialises");
        assert!(matches!(decode_request(&json), Err(WireError::NotALink)));
    }

    #[test]
    fn errors_print_no_text() {
        for error in [
            WireError::TooLarge,
            WireError::Malformed,
            WireError::Version,
            WireError::NotALink,
            WireError::Closed,
            WireError::TimedOut,
        ] {
            let text = format!("{error} {error:?}");
            assert!(!text.contains("token") && !text.contains("familychat:"));
        }
    }

    #[tokio::test]
    async fn frames_over_the_limit_are_refused_before_reading_them() {
        let mut huge = Vec::new();
        huge.extend_from_slice(&u32::MAX.to_be_bytes());
        let mut reader = huge.as_slice();
        assert_eq!(read_frame(&mut reader).await, Err(WireError::TooLarge));
        let mut short = Vec::new();
        short.extend_from_slice(&10_u32.to_be_bytes());
        short.extend_from_slice(b"abc");
        let mut reader = short.as_slice();
        assert_eq!(read_frame(&mut reader).await, Err(WireError::Closed));
    }
}
