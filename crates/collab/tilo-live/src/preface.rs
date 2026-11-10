//! The admission preface: the three header bytes and the grant that open a
//! `moq-lite` session, and the host's one-byte answer.
//!
//! The carrier runs the exchange over its transport ([`crate::carrier`]); the
//! page runs the same bytes over its WebTransport session
//! (`tilo_session_host::live`). Both encode and read the preface here, so the
//! two carriers cannot drift: one layout, one version, one set of answers.
//!
//! Client to host, on the first bidirectional stream, exactly once:
//!
//! | Bytes | Field | Value |
//! | --- | --- | --- |
//! | 1 | version | [`PREFACE_VERSION`] |
//! | 2 | length | big-endian `u16`, `1..=`[`MAX_GRANT_BYTES`] |
//! | length | grant | opaque grant bytes issued by the host |
//!
//! Host to client, on the same stream:
//!
//! | Byte | Meaning |
//! | --- | --- |
//! | [`STATUS_ADMITTED`] | Admitted. Origins are attached; start `moq-lite` now. |
//! | [`STATUS_DENIED`] | Refused; the host closes with [`CLOSE_DENIED`]. |
//! | [`STATUS_REVOKED`] | Authority lapsed; close with [`CLOSE_REVOKED`]. |

/// The WebTransport subprotocol and `moq-lite` draft every Tilo `MoQ` route
/// negotiates. No other version is offered or accepted.
pub const PROTOCOL: &str = "moq-lite-06";
/// The one path a carrier's WebTransport listener serves. A session on any
/// other path, or behind a query, is refused before the preface.
pub const PATH: &str = "/moq/voice";
/// Admission preface version byte.
pub const PREFACE_VERSION: u8 = 1;
/// Largest grant the preface carries. Routes may apply a smaller ceiling.
pub const MAX_GRANT_BYTES: usize = 8192;
/// Preface reply: admitted.
pub const STATUS_ADMITTED: u8 = 0;
/// Preface reply: refused.
pub const STATUS_DENIED: u8 = 1;
/// Control message after admission: authority lapsed, close now.
pub const STATUS_REVOKED: u8 = 2;
/// WebTransport close code for a missing, malformed or late preface.
pub const CLOSE_PREFACE: u32 = 0x7400;
/// WebTransport close code for a refused grant.
pub const CLOSE_DENIED: u32 = 0x7401;
/// WebTransport close code for a session whose authority lapsed.
pub const CLOSE_REVOKED: u32 = 0x7402;
/// How long a refused session stays open for its client to read the
/// refusal and close it.
pub const DENIED_GRACE: std::time::Duration = std::time::Duration::from_secs(1);
/// How long a refused client waits for the host's close reason after the
/// refusal byte itself was lost. Longer than [`DENIED_GRACE`], so a host that
/// is still holding the session open for the client is not read as a lost
/// connection.
pub const DENIED_REPORT: std::time::Duration = std::time::Duration::from_secs(2);

/// The preface bytes carrying `grant`, or `None` when the grant is empty or
/// larger than [`MAX_GRANT_BYTES`].
#[must_use]
pub fn preface(grant: &[u8]) -> Option<Vec<u8>> {
    let length = u16::try_from(grant.len())
        .ok()
        .filter(|length| *length > 0 && usize::from(*length) <= MAX_GRANT_BYTES)?;
    let [high, low] = length.to_be_bytes();
    let mut bytes = Vec::with_capacity(grant.len().saturating_add(3));
    bytes.extend_from_slice(&[PREFACE_VERSION, high, low]);
    bytes.extend_from_slice(grant);
    Some(bytes)
}

/// The grant length [`preface`] announced in its header, or `None` when the
/// header is not this version or names no usable length.
#[must_use]
pub fn grant_length(header: [u8; 3]) -> Option<usize> {
    let [version, high, low] = header;
    let length = usize::from(u16::from_be_bytes([high, low]));
    (version == PREFACE_VERSION && length > 0 && length <= MAX_GRANT_BYTES).then_some(length)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preface_names_the_version_and_the_grant_length() {
        let bytes = preface(b"grant").unwrap();
        assert_eq!(bytes, [1, 0, 5, b'g', b'r', b'a', b'n', b't']);
        assert_eq!(grant_length([bytes[0], bytes[1], bytes[2]]), Some(5));
    }

    #[test]
    fn an_empty_or_oversized_grant_has_no_preface() {
        assert!(preface(&[]).is_none());
        assert!(preface(&vec![0; MAX_GRANT_BYTES.saturating_add(1)]).is_none());
        assert!(preface(&vec![0; MAX_GRANT_BYTES]).is_some());
    }

    #[test]
    fn a_header_of_another_version_or_length_is_refused() {
        assert_eq!(grant_length([2, 0, 1]), None);
        assert_eq!(grant_length([1, 0, 0]), None);
        assert_eq!(grant_length([1, 0xff, 0xff]), None);
    }
}
