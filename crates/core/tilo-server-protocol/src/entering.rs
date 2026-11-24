//! A device entering a workspace it is a stranger to, through the workspace's
//! entry or with an invitation: what the host's `workspace.entry` action takes,
//! proved by the device's key over the connection it was asked on, and what
//! that action answers ([`Entered`]).
//!
//! The entry lets the person in as its HTTP form did: an invitation bound to
//! their user root and the one-time token that admits them, both kept on the
//! host, and the workspace admits the device with that invitation as it
//! admits every redemption. An invitation the device holds itself (a pasted
//! or short link, or one a host handed a member device for a workspace of
//! the person's own) comes in the entry instead, with the token of the
//! owner's pre-admission it was issued under when there is one
//! ([`Through::Invitation`]): an invitation is the approval of whoever made
//! it, so it admits at once or not at all.
//!
//! A device that is already in a session with the host redeems an invitation
//! the same way, on another stream of the same connection: that stream's
//! first message is the entry, and the stream ends with its answer.
//!
//! **The proof.** A host's challenge used to make the device's proofs fresh:
//! the device signed the host's nonce (`HelloAck2`) and the workspace's join
//! challenge, a round trip after its hello. The entry's proof
//! ([`Entry::proof`]) is the device's signature over [`entry_digest`] instead:
//! the entry, under keying material both ends export from the TLS session of
//! the QUIC connection that carries it ([`ENTRY_BINDING_LABEL`], RFC 5705).
//! That material is derived from both ends' handshake randomness and their
//! key exchange, so it is unique to the connection and nobody outside it
//! learns it: a captured proof proves nothing on any other connection, as an
//! answer to one host challenge proved nothing under another. The host checks
//! it against its own export before the entry is asked, and then admits the
//! device without a challenge of the workspace's own
//! (`verify_connection_redemption`). An entry rides only a connection that
//! has such a session; the host refuses it anywhere else.
//!
//! **The admission is the device's own.** No host signs anyone into a
//! workspace: the device writes its own admission on the workspace's member
//! list and signs it. An entry that would let the device in is first
//! answered [`Entered::Sign`], with the member list and how to come back (the
//! invitation the entry issued it, or as it came); the device asks again with
//! the changes it signed ([`Entering::admission`]): its redemption of the
//! invitation, or, for a further device of someone the workspace has in it,
//! its own device row. The host passes them on unchanged, and every device of
//! the workspace judges them under the device that signed them.
//!
//! A server reads nothing else from an entering session until the workspace
//! admitted it. A client sends its first requests right behind its hello; the
//! host reads them only once it answered [`Entered::Admitted`]. The client
//! checks the host's `HelloAck` before anything else it reads.
//!
//! The entry also carries the device's request to join the workspace's
//! key group, which its key service prepares before they know anything of the workspace's governance. The host
//! hands it to the workspace's channel key holders as it admits the device,
//! as it hands every member's, so the Welcome that keys the device is the
//! first thing its download carries, not something it asks for once the
//! governance it downloads names it. The answer names the group that seals
//! the workspace's shared desktop, so the device opens it with those keys
//! before the governance that decides it arrives.

use collab_types::WorkspaceId;
use serde::{Deserialize, Serialize};

use crate::directory::EntryRefused;
use crate::workspaces::Role;

/// How many entries a host runs at once, across every connection: the same
/// bound its iroh join service holds its exchanges to.
pub const MAX_ENTERING: usize = 64;

/// How long one entry may take, from the hello to the host's answer: the
/// iroh join service's bound on one exchange.
pub const ENTERING_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// The label both ends export the connection's keying material under
/// (RFC 5705), with an empty context: what [`entry_digest`] binds an entry to.
pub const ENTRY_BINDING_LABEL: &[u8] = b"tilo/session-entry/v1";

/// The domain of the device's signature over an entry ([`entry_digest`]).
const ENTRY_PROOF_DOMAIN: &[u8] = b"tilo/session-entry-proof/v1\0";

/// What a device that is not in the workspace yet asks for in its hello: to
/// come into `workspace`, through its entry or with an invitation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entering {
    /// The workspace the device enters: its canonical UUID. An invitation's
    /// must name the same.
    pub workspace: String,
    /// How the workspace's people see the person.
    pub name: String,
    /// This entry, the same on every connection that asks for it, so an
    /// answer lost with its connection is answered again, not refused as an
    /// invitation already redeemed.
    pub request_id: [u8; 32],
    /// How the device comes in.
    pub through: Through,
    /// The workspace changes the device signed to let itself in, written on
    /// the member list [`Entered::Sign`] answered: empty on the first ask.
    pub admission: Vec<Vec<u8>>,
}

/// How a device comes into a workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Through {
    /// The workspace's entry, which lets the person in by the workspace's own
    /// rules.
    Entry,
    /// An invitation the device holds: its signed bytes, and the one-time
    /// token of the owner's pre-admission it was issued under, if any.
    Invitation {
        #[serde(with = "serde_bytes")]
        invitation: Vec<u8>,
        #[serde(with = "serde_bytes")]
        token: Option<Vec<u8>>,
    },
}

/// An entry as the hello carries it: what the device asks, its request for
/// the workspace's channel keys, and its proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub entering: Entering,
    /// The device's signed request for the keys of the workspace's group, as
    /// it would publish it once
    /// a member: handed to the group's key holders once the workspace
    /// admitted the device. `None` from a device whose channels keep no
    /// state, which asks as a member later. A request for another group, or
    /// from another device, is not handed on.
    pub access: Option<collab_types::MlsAccessRequestV1>,
    /// The device's signature over [`entry_digest`] of this entry, under the
    /// connection's keying material: it proves the device holds its key on
    /// this connection and asks exactly this.
    #[serde(with = "serde_bytes")]
    pub proof: [u8; 64],
}

/// What a device signs to enter as `entering` asks, on the connection whose
/// keying material ([`ENTRY_BINDING_LABEL`]) is `binding`, as `device`.
#[must_use]
pub fn entry_digest(binding: &[u8; 32], device: &[u8; 32], entering: &Entering) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(ENTRY_PROOF_DOMAIN);
    hasher.update(binding);
    hasher.update(device);
    let (invitation, token): (&[u8], Option<&[u8]>) = match &entering.through {
        Through::Entry => (&[], None),
        Through::Invitation { invitation, token } => (invitation, token.as_deref()),
    };
    let field = |hasher: &mut blake3::Hasher, field: &[u8]| {
        hasher.update(&u64::try_from(field.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(field);
    };
    field(&mut hasher, entering.workspace.as_bytes());
    field(&mut hasher, entering.name.as_bytes());
    hasher.update(&entering.request_id);
    hasher.update(&[u8::from(matches!(entering.through, Through::Entry))]);
    field(&mut hasher, invitation);
    hasher.update(&[u8::from(token.is_some())]);
    field(&mut hasher, token.unwrap_or_default());
    *hasher.finalize().as_bytes()
}

/// Why an entry or its workspace did not let a device in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnterRefusal {
    /// The entry did not let the person in, as it typed it, with how long it
    /// asks to wait before the next try when it asked.
    Entry {
        refused: EntryRefused,
        retry_after_secs: Option<u64>,
    },
    /// The entry let the person in and the workspace did not admit this
    /// device, in its own words: a device key it knows under another
    /// certificate, an invitation already spent.
    Workspace(String),
}

impl EnterRefusal {
    /// What happened, in plain words.
    #[must_use]
    pub fn reason(&self) -> &str {
        match self {
            Self::Entry { refused, .. } => &refused.error,
            Self::Workspace(reason) => reason,
        }
    }
}

/// The host's answer to an entry: the `workspace.entry` action's `entered`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Entered {
    /// The workspace admitted the device: from here on the session is a
    /// member's.
    Admitted {
        /// The workspace's UUID.
        workspace: String,
        /// Its server workspace.
        workspace_id: WorkspaceId,
        /// Its name.
        name: String,
        /// What the person may do there.
        role: Role,
        /// The host's own signed address for the workspace (a
        /// `tilo_workspace_join::TransportHint`, postcard): where a device that
        /// keeps the workspace's state itself (a desktop) takes it in, by the
        /// membership exchange a member's devices use, now that the workspace
        /// admitted it. A page takes the workspace on this session and reads
        /// nothing here. `None` from a host that has no such address.
        #[serde(with = "serde_bytes")]
        host: Option<Vec<u8>>,
    },
    /// The workspace did not admit the device. The host closes the session.
    Refused(EnterRefusal),
    /// The workspace lets the device in once it signs its own admission:
    /// written on `governance`, the workspace's member list as encoded
    /// neighbour messages, and asked again `through` (the invitation the
    /// entry issued it, or as the device came) with those changes as its
    /// [`Entering::admission`].
    Sign {
        governance: Vec<Vec<u8>>,
        through: Through,
    },
}

#[cfg(test)]
mod tests {
    use super::{EnterRefusal, Entered};
    use crate::directory::{EntryRefusalCode, EntryRefused};

    /// An entry's refusal reaches the device whatever it names: an empty one, and
    /// one offering a wait list with a contact and mail, both read back as sent.
    #[test]
    fn an_entry_refusal_reads_back_whatever_it_names() {
        let plain = EntryRefused {
            code: EntryRefusalCode::Refused,
            error: "no".into(),
            ..EntryRefused::default()
        };
        let waiting = EntryRefused {
            code: EntryRefusalCode::InvitationOnly,
            error: "by invitation".into(),
            wait_list: true,
            contact: Some("entry@example.org".into()),
            mail: true,
        };
        for refused in [plain, waiting] {
            for retry_after_secs in [None, Some(30)] {
                let step = Entered::Refused(EnterRefusal::Entry {
                    refused: refused.clone(),
                    retry_after_secs,
                });
                let bytes = postcard::to_allocvec(&step).unwrap();
                assert_eq!(postcard::from_bytes::<Entered>(&bytes).unwrap(), step);
            }
        }
    }
}
