//! The shelf a site keeps sealed invitations on: `/i/<handle>`.
//!
//! An invitation is about a kilobyte and a half of signed certificates,
//! transport hints and signatures, which no code to scan can carry. The
//! inviter's device seals those bytes under a key it puts in the link's
//! fragment, and leaves the ciphertext here under a random handle; whoever
//! holds the link fetches it back and opens it. The site keeps ciphertext it
//! has no key for — written on its own state disk, so a link pasted into a
//! message and read days later still opens — for as long as the invitation it
//! holds was issued, and takes it away before that when the inviter asks.
//!
//! Nothing reads the shelf: a handle is unguessable, and a fetch names the
//! one handle it wants.

use serde::{Deserialize, Serialize};

/// The path a sealed invitation is kept at, below a site's origin:
/// `/i/<handle>`.
pub const ROUTE: &str = "/i/";

/// Characters a handle is spelled in: base64url, as
/// [`tilo_workspace_join::sealed`] draws it.
pub const HANDLE_CHARS: usize = 16;

/// The most a site keeps for one invitation: an invitation's signed bytes
/// and the envelope around them.
pub const MAX_SEALED_BYTES: usize = 8 * 1024;

/// The longest a site keeps an invitation: its own life is bounded by
/// [`MAX_STORED_SECONDS`], whatever the inviter asks for.
pub const MAX_STORED_SECONDS: u64 = 30 * 24 * 60 * 60;

/// The handle `path` names, when it names a sealed invitation: the site's own
/// route, or the same route below a relay's API prefix.
#[must_use]
pub fn handle_of(path: &str) -> Option<&str> {
    let route = path.strip_prefix(crate::web::API_PREFIX).unwrap_or(path);
    let handle = route.strip_prefix(ROUTE)?;
    (handle.chars().count() == HANDLE_CHARS
        && !handle.contains('/')
        && handle
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
    .then_some(handle)
}

/// What an inviter's device leaves on the shelf.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Stored {
    /// The sealed signed invitation, base64url.
    pub sealed: String,
    /// When the invitation it holds lapses, in Unix nanoseconds.
    pub expires_at_ns: u64,
    /// The secret that takes it back, base64url. It is in no link: only the
    /// device that sealed the invitation keeps it.
    pub delete: String,
    /// The `https://` origin of the host serving the workspace the invitation
    /// is for. A link names the site that keeps the invitation, not the host
    /// it admits into, so a page opening the link is told where to ask
    /// ([`Fetched::host`]). Absent from what builds before it wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

/// What a fetch answers: the sealed invitation, and where it admits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Fetched {
    /// The sealed signed invitation, base64url.
    pub sealed: String,
    /// The origin the invitation admits into ([`Stored::host`]), when the
    /// inviter's build named it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

/// What taking an invitation back names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TakeBack {
    /// The secret the invitation was stored with.
    pub delete: String,
}

/// Why a site did not answer with the invitation asked for.
pub const GONE: u16 = 410;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handle_is_sixteen_spelled_characters_of_base64url() {
        assert_eq!(
            handle_of("/i/abcdefghijklmnop"),
            Some("abcdefghijklmnop"),
            "a site's own route"
        );
        assert_eq!(
            handle_of("/peer/v1/i/abcdefghijklmnop"),
            Some("abcdefghijklmnop"),
            "the same route below a relay's API prefix"
        );
        assert_eq!(handle_of("/i/abcdefghijklmnop/"), None);
        assert_eq!(handle_of("/i/abcdefghijklmno"), None);
        assert_eq!(handle_of("/i/abcdefghijklmnopq"), None);
        assert_eq!(handle_of("/i/abcdefghijklmno!"), None);
        assert_eq!(handle_of("/i/"), None);
        assert_eq!(handle_of("/directory"), None);
    }

    #[test]
    fn the_limits_hold_an_invitation_and_no_more() {
        // An invitation is under 2 KiB sealed; a month is as long as a site
        // keeps one.
        assert_eq!(MAX_SEALED_BYTES, 8 * 1024);
        assert_eq!(MAX_STORED_SECONDS, 30 * 24 * 60 * 60);
        assert_eq!(GONE, 410);
    }
}
