//! Login codes: the short code a person's tab shows and their agent types,
//! and the stand-in approver a host keeps for a person who set Tilo up
//! through an agent.
//!
//! A code is a rendezvous, not a credential of its own: it names one
//! pairing in progress between one tab and one computer, through the host
//! that made it, and that pairing is the one identity pairing flow (a
//! certification request a device signs, answered with a certificate).
//!
//! - **Setting up** (`Set up with your agent`): the tab holds the person's
//!   root. It asks its host for a code naming its pairing link
//!   ([`CreateCode::link`]); the agent runs `tilo login <code>`, which
//!   resolves the code ([`Resolved::link`]) and asks to be certified with it;
//!   the tab approves, as it approves any device that answers its invite.
//! - **Signing in again** (`Returning? Sign in with your agent`): the tab
//!   holds no identity of the person yet. It asks for a code naming nothing
//!   ([`CreateCode::link`] `None`); `tilo login <code>` on a computer that is
//!   already a device of the person resolves it ([`Resolved::sign_in`]) and
//!   claims it for that person ([`Claim`]); the tab learns whom to ask
//!   ([`Polled::claimed`]) and asks to be certified with that invite; the
//!   person's stand-in on the host ([`STAND_IN_ROUTE`]) approves exactly
//!   that request.
//!
//! A code is single use (the first resolve spends it), lasts
//! [`CODE_LIFETIME_MS`], and only the tab that made it can follow it (with
//! the [`Created::follow`] secret only that tab got). Once resolved, the
//! computer reports what it does ([`Step`]) with the [`Resolved::report`]
//! secret, and the tab shows each step as it happens.
//!
//! Routes (all `POST`, JSON bodies, below [`crate::web::API_PREFIX`]):
//!
//! | Route | Body | Answer |
//! |---|---|---|
//! | [`CREATE_ROUTE`] | [`CreateCode`] | [`Created`] |
//! | [`RESOLVE_ROUTE`] | [`Resolve`] | [`Resolved`]; `404` unknown, spent or expired; `429` too many misses |
//! | [`STEP_ROUTE`] | [`Report`] | `204` |
//! | [`CLAIM_ROUTE`] | [`Claim`] | [`Claimed`]; `403` not a device of a person with a stand-in here |
//! | [`FOLLOW_ROUTE`] | [`Follow`] | [`Polled`] |
//! | [`STAND_IN_ROUTE`] | [`StandInRequest`] | [`StandInReply`] |
//!
//! # The stand-in
//!
//! A person who set Tilo up through an agent has their root in one browser
//! tab and nowhere else until they keep it with a passkey. So that clearing
//! that tab's storage does not lose them, their tab has the host make a key
//! the host holds (the stand-in) and certifies it with the root as an
//! approver of new devices of that person (`tilo-user-identity`'s stand-in
//! capability). What it can do:
//!
//! - certify a new device of that one person, with member capabilities and a
//!   browser's lifetime, when a device of the person already certified asks
//!   for exactly that request ([`Claim`]);
//!
//! and nothing else: it is no device of the person (it reaches no workspace
//! and holds no membership), it admits no hosts, approves no grants, certifies
//! no other approver, and it expires ([`STAND_IN_LIFETIME_MS`]). When the
//! person first keeps their root with a passkey, their root revokes it
//! through the ordinary revocation path ([`StandInRequest::Retire`]): it can
//! approve nothing from then on, and every device it certified before stays
//! valid, as a revocation takes effect from its time on.

use serde::{Deserialize, Serialize};

/// Makes a code.
pub const CREATE_ROUTE: &str = "/login-code";
/// Resolves a code, spending it.
pub const RESOLVE_ROUTE: &str = "/login-code/resolve";
/// A computer's report of what it did with a code it resolved.
pub const STEP_ROUTE: &str = "/login-code/step";
/// A device of a person claims a sign-in code for that person.
pub const CLAIM_ROUTE: &str = "/login-code/claim";
/// The tab that made a code follows it.
pub const FOLLOW_ROUTE: &str = "/login-code/follow";
/// The person's stand-in approver on this host.
pub const STAND_IN_ROUTE: &str = "/stand-in";

/// How long a code can be resolved.
pub const CODE_LIFETIME_MS: u64 = 10 * 60 * 1000;
/// How long a resolved code keeps its steps for the tab to follow.
pub const FOLLOW_LIFETIME_MS: u64 = 30 * 60 * 1000;
/// The longest a stand-in's certificate may last.
pub const STAND_IN_LIFETIME_MS: u64 = 30 * 24 * 60 * 60 * 1000;
/// The longest pairing link a code names.
pub const MAX_LINK_CHARS: usize = 2048;
/// The longest agent name a report carries.
pub const MAX_AGENT_CHARS: usize = 64;

/// The letters a code is made of: no 0/O, 1/I/L or U, so a code read aloud
/// or typed from a screenshot is not misread.
pub const CODE_ALPHABET: &[u8; 30] = b"23456789ABCDEFGHJKMNPQRSTVWXYZ";
/// Letters in a code: `K7Q-MX2P`.
pub const CODE_LETTERS: usize = 7;
/// Letters before the dash.
pub const CODE_DASH_AT: usize = 3;
/// A code as written, with its dash.
const CODE_CHARS: usize = 8;

/// A code from `random` bytes (one letter each), as the person reads it.
#[must_use]
pub fn code_from(random: [u8; CODE_LETTERS]) -> String {
    let mut code = String::with_capacity(CODE_CHARS);
    for (index, byte) in random.iter().enumerate() {
        if index == CODE_DASH_AT {
            code.push('-');
        }
        let letter = CODE_ALPHABET
            .get(
                usize::from(*byte)
                    .checked_rem(CODE_ALPHABET.len())
                    .unwrap_or(0),
            )
            .copied()
            .unwrap_or(b'2');
        code.push(char::from(letter));
    }
    code
}

/// `typed` as the code it is (`k7q mx2p`, `K7QMX2P` → `K7Q-MX2P`), or `None`
/// when it is not one.
#[must_use]
pub fn normalize_code(typed: &str) -> Option<String> {
    let letters: Vec<u8> = typed
        .bytes()
        .filter(|byte| !matches!(byte, b'-' | b' ' | b'\t'))
        .map(|byte| byte.to_ascii_uppercase())
        .collect();
    if letters.len() != CODE_LETTERS || !letters.iter().all(|byte| CODE_ALPHABET.contains(byte)) {
        return None;
    }
    let mut code = String::with_capacity(CODE_CHARS);
    for (index, byte) in letters.iter().enumerate() {
        if index == CODE_DASH_AT {
            code.push('-');
        }
        code.push(char::from(*byte));
    }
    Some(code)
}

/// A tab asks for a code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateCode {
    /// Setting up: the tab's pairing link (a Tilo link with `?pair=`), which
    /// the computer resolving the code asks to be certified with. `None`:
    /// the tab asks to be signed in by a device of the person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

/// A code made.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Created {
    /// `K7Q-MX2P`.
    pub code: String,
    /// The secret only this tab holds, to follow the code (hex, 32 bytes).
    pub follow: String,
    /// Unix-epoch milliseconds after which the code no longer resolves.
    pub expires_at_ms: u64,
}

/// A computer resolves a code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resolve {
    /// As typed; the host normalizes it ([`normalize_code`]).
    pub code: String,
}

/// What a code names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resolved {
    /// Setting up: the tab's pairing link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Signing in: the tab asks a device of the person to claim it.
    #[serde(default)]
    pub sign_in: bool,
    /// The secret to report steps and claim with (hex, 32 bytes).
    pub report: String,
}

/// What the computer did, as the tab shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Step {
    /// The code was resolved: the agent has the `tilo` command.
    Resolved,
    /// The computer is a device of the person now.
    LoggedIn,
    /// The agent has Tilo's tools (its MCP server is registered).
    ToolsAdded,
    /// The agent must restart before its tools load.
    Restart,
    /// The agent entered the person's workspace.
    Entered,
    /// The tab that asked to be signed in was approved.
    Approved,
    /// Something failed; the report's `detail` says what.
    Failed,
}

/// A computer reports a step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub report: String,
    pub step: Step,
    /// The agent running the computer, as `tilo login` found it
    /// (`Claude Code`), at most [`MAX_AGENT_CHARS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// One sentence for the person, for [`Step::Failed`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A device of a person claims a sign-in code for them: the tab then asks
/// to be certified as that person's device, and their stand-in approves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    pub report: String,
    /// The claiming device's certificate, postcard as hex.
    pub cert: String,
    /// Unix-epoch nanoseconds the claim was signed at.
    pub signed_at_ns: u64,
    /// The device key's Ed25519 signature over [`claim_message`], hex.
    pub signature: String,
}

/// What a device signs to claim the code resolved with `report`.
#[must_use]
pub fn claim_message(report: &str, signed_at_ns: u64) -> Vec<u8> {
    let mut message = b"tilo-login-claim-v1\0".to_vec();
    message.extend_from_slice(report.as_bytes());
    message.extend_from_slice(&signed_at_ns.to_le_bytes());
    message
}

/// A claim taken.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claimed {
    /// Unix-epoch milliseconds until which the tab may ask to be certified.
    pub expires_at_ms: u64,
}

/// The tab follows its code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Follow {
    pub follow: String,
}

/// Where a code is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Polled {
    /// Whether a computer resolved it.
    pub resolved: bool,
    /// Whether it expired before anyone resolved it (or its steps expired).
    pub expired: bool,
    /// The agent, once a report named it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The steps reported, in order, each at most once.
    #[serde(default)]
    pub steps: Vec<Step>,
    /// The latest failure's sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Signing in: whom the claiming device is a device of, once claimed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed: Option<ClaimedFor>,
}

/// Whom a sign-in code was claimed for, and the invite to ask with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimedFor {
    /// The person's root, hex.
    pub user: String,
    /// The invite the tab's certification request names, hex (16 bytes).
    pub invite: String,
    /// The claiming device's label (`Claude Code on build-box`).
    pub by: String,
}

/// What a person's root asks of their stand-in on this host. Each is signed
/// by the root ([`StandInRequest::signed_message`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
pub enum StandInRequest {
    /// Make (or name again) the key the host holds as this person's
    /// stand-in. Answers [`StandInReply::Key`].
    Key {
        user: String,
        signed_at_ns: u64,
        signature: String,
    },
    /// Keep the stand-in's root-signed certificate (postcard, hex): from
    /// now on it approves claimed sign-ins. Answers [`StandInReply::Kept`].
    Certify { cert: String },
    /// The root is kept with a passkey now: forget the stand-in's key. The
    /// revocation itself travels as every revocation of the person does.
    /// Answers [`StandInReply::Retired`].
    Retire {
        user: String,
        signed_at_ns: u64,
        signature: String,
    },
}

impl StandInRequest {
    /// What the root signs for [`Self::Key`] and [`Self::Retire`]: the
    /// operation, the person's root and the time.
    #[must_use]
    pub fn signed_message(op: &str, user: &[u8; 32], signed_at_ns: u64) -> Vec<u8> {
        let mut message = b"tilo-stand-in-v1\0".to_vec();
        message.extend_from_slice(op.as_bytes());
        message.push(0);
        message.extend_from_slice(user);
        message.extend_from_slice(&signed_at_ns.to_le_bytes());
        message
    }
}

/// The host's answer about a stand-in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum StandInReply {
    /// The stand-in's public key, hex, for the root to certify.
    Key { key: String },
    /// The certificate is kept: the stand-in approves until it expires.
    Kept { expires_at_ms: u64 },
    /// The key is forgotten.
    Retired,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_reads_back_however_it_is_typed() {
        let code = code_from([0, 1, 2, 3, 4, 5, 29]);
        assert_eq!(code, "234-567Z");
        assert_eq!(normalize_code("234567z").as_deref(), Some("234-567Z"));
        assert_eq!(normalize_code(" 234 567Z ").as_deref(), Some("234-567Z"));
        assert_eq!(normalize_code("234-567"), None);
        assert_eq!(normalize_code("O34-567Z"), None);
        assert_eq!(normalize_code("234-5671"), None);
    }

    #[test]
    fn every_letter_is_one_a_person_cannot_misread() {
        for confusable in b"01OILU" {
            assert!(!CODE_ALPHABET.contains(confusable));
        }
    }
}
