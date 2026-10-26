//! `tilo-authd`'s socket protocol: one JSON object per line each way.

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// Longest request line the daemon reads. Secrets are short; anything
/// longer is not a request.
pub const MAX_LINE: usize = 16 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Request {
    /// Check a person's secret. The peer is root or that person.
    Verify {
        user: String,
        secret: Zeroizing<String>,
    },
    /// Whether a person's secret is checked here, and what they can sign in
    /// with. The peer is root or that person.
    Status { user: String },
    /// Move a person who still signs in with the old password onto a user
    /// key, proven by that password. `Verify` does the same on the way; this
    /// is the explicit form. The peer is root or that person.
    Enroll {
        user: String,
        secret: Zeroizing<String>,
    },
    /// Change a person's secret, proven by the current one, or by their
    /// recovery key when the peer is root.
    SetSecret {
        user: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current: Option<Zeroizing<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recovery: Option<Zeroizing<String>>,
        new: Zeroizing<String>,
    },
    /// Add a recovery wrapper, proven by the current secret. Root only.
    AddRecovery {
        user: String,
        secret: Zeroizing<String>,
        recovery: Zeroizing<String>,
    },
    /// The person signed in at boot whose session has not started yet.
    /// Answered with [`PendingSignIn`] or `null`; for the greeter's user.
    PendingSignIn,
    /// Use the boot sign-in for `user`, once. Root only (the greeter's PAM
    /// service runs as root).
    ClaimBootSignIn { user: String },
    /// Whether `user`'s key is held for a session of theirs running now,
    /// which is when a fingerprint may confirm them (sign-in.md §4.3).
    /// Answered in `key_held`. The peer is root, that person or the greeter.
    KeyHeld { user: String },
}

/// Why a request did not succeed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Refusal {
    /// The secret is not this person's.
    Wrong,
    /// The TPM refuses tries until its lockout decays.
    LockedOut,
    /// No usable record and no old password to check against.
    NotEnrolled,
    /// The asking process may not ask this about this person.
    NotPermitted,
    /// A new secret is too weak to stand alone on a machine without a TPM.
    WeakSecret,
    /// No boot sign-in for this person is waiting.
    NoBootSignIn,
    /// The request could not be carried out; trying again later may work.
    Unavailable,
    /// The request was not understood.
    BadRequest,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Refusal>,
    /// Wrong secrets left before the TPM makes everyone wait.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tries_left: Option<u32>,
    /// Seconds until another try is possible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_in_s: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrolled: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub factors: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_held: Option<bool>,
}

impl Reply {
    pub fn success() -> Self {
        Self {
            ok: true,
            ..Self::default()
        }
    }

    pub fn refused(refusal: Refusal) -> Self {
        Self {
            ok: false,
            error: Some(refusal),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingSignIn {
    pub name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_replies_have_their_documented_wire_form() {
        let request: Request =
            serde_json::from_str(r#"{"op":"verify","user":"ada","secret":"123456"}"#).unwrap();
        assert!(matches!(request, Request::Verify { ref user, ref secret }
            if user == "ada" && secret.as_str() == "123456"));
        assert!(matches!(
            serde_json::from_str::<Request>(r#"{"op":"pending-sign-in"}"#).unwrap(),
            Request::PendingSignIn
        ));
        assert_eq!(
            serde_json::to_string(&Reply {
                tries_left: Some(9),
                retry_in_s: Some(7200),
                ..Reply::refused(Refusal::Wrong)
            })
            .unwrap(),
            r#"{"ok":false,"error":"wrong","tries_left":9,"retry_in_s":7200}"#
        );
        assert_eq!(
            serde_json::to_string(&Reply::success()).unwrap(),
            r#"{"ok":true}"#
        );
        assert_eq!(
            serde_json::to_string(&Some(PendingSignIn { name: "ada".into() })).unwrap(),
            r#"{"name":"ada"}"#
        );
        assert_eq!(
            serde_json::to_string(&None::<PendingSignIn>).unwrap(),
            "null"
        );
    }
}
