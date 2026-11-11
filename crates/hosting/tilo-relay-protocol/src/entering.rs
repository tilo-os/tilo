//! A device entering a workspace through its door on the session it opens
//! with the workspace's host, rather than in requests of its own before it.
//!
//! The device is a stranger to the host until the workspace admits it, so its
//! session carries the entry and nothing else until then:
//!
//! ```text
//! client → server:  Hello { .., entering: Some(Entering) }
//! server → client:  HelloAck, then EnterStep::Challenge (or ::Refused, and the end)
//! client → server:  HelloAck2, then EnterProof
//! server → client:  Envelope::Entered, then the session, or nothing more
//! ```
//!
//! The door lets the person in as its HTTP form did: an invitation bound to
//! their user root and the one-time token that admits them, both kept on the
//! host. The challenge is the workspace's own join challenge for that
//! invitation and this device, and the proof is the device's signature over
//! it, so the workspace admits exactly as it admits every redemption. The
//! handshake's own proof (`HelloAck2`) shows the device holds its key before
//! the workspace changes for it.
//!
//! A server reads nothing else from an entering session until the workspace
//! admitted it: a frame that is not the next step of the entry ends the
//! session. A client may send its first requests right behind its proof; the
//! host reads them only once it answered [`Entered::Admitted`].

use serde::{Deserialize, Serialize};

use crate::directory::DoorRefused;
use crate::workspaces::WorkspaceRole;

/// How many entries a host runs at once, across every connection: the same
/// bound its iroh join service holds its exchanges to.
pub const MAX_ENTERING: usize = 64;

/// How long one entry may take, from the hello to the host's answer: the
/// iroh join service's bound on one exchange.
pub const ENTERING_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// What a device that is not in the workspace yet asks for in its hello: to
/// come in through `workspace`'s door.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entering {
    /// The workspace whose door the device enters by: its canonical UUID.
    pub workspace: String,
    /// How the workspace's people see the person.
    pub name: String,
    /// This entry, the same on every connection that asks for it, so an
    /// answer lost with its connection is answered again, not refused as an
    /// invitation already redeemed.
    pub request_id: [u8; 32],
}

/// The host's step of an entry, right after its `HelloAck`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnterStep {
    /// The workspace's join challenge for the invitation its door issued and
    /// this device: what [`EnterProof`] signs.
    Challenge {
        #[serde(with = "serde_bytes")]
        challenge: Vec<u8>,
    },
    /// The workspace already admitted this device under this entry, on a
    /// connection that ended before its answer arrived: the device proves its
    /// key (`HelloAck2`) and sends no [`EnterProof`].
    In,
    /// The door does not let the person in. The host closes the session.
    Refused(EnterRefusal),
}

/// The device's signature over the challenge's join proof digest
/// (`tilo_relay_protocol::workspaces::join_proof_digest`), right after its
/// `HelloAck2`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnterProof {
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

/// Why a door or its workspace did not let a device in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnterRefusal {
    /// The door did not let the person in, as it typed it, with how long it
    /// asks to wait before the next try when it asked.
    Door {
        refused: DoorRefused,
        retry_after_secs: Option<u64>,
    },
    /// The door let the person in and the workspace did not admit this
    /// device, in its own words: a device key it knows under another
    /// certificate, an invitation already spent.
    Workspace(String),
}

impl EnterRefusal {
    /// What happened, in plain words.
    #[must_use]
    pub fn reason(&self) -> &str {
        match self {
            Self::Door { refused, .. } => &refused.error,
            Self::Workspace(reason) => reason,
        }
    }
}

/// The host's answer to an entry, the first envelope of the session it opened
/// ([`crate::Envelope::Entered`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Entered {
    /// The workspace admitted the device: from here on the session is a
    /// member's.
    Admitted {
        /// The workspace's UUID.
        workspace: String,
        /// Its relay workspace.
        workspace_id: u64,
        /// Its name.
        name: String,
        /// What the person may do there.
        role: WorkspaceRole,
    },
    /// The workspace did not admit the device. The host closes the session.
    Refused(EnterRefusal),
}
