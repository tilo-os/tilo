//! The host's own word for the runs it executes, outside the live plane.
//!
//! A workspace's host executes the actions of agents and keys that act
//! through it, and says so: each run is present in the workspace as a member
//! of its own, and each action is narrated on its record's stream. The live
//! plane cannot carry that word where the host is no member of the plane: its
//! content keys open every member-readable record, and a host that belongs to
//! an operator never holds them. The host says it here instead, signed with
//! its own device key, on its own sessions with the workspace's devices: the
//! relay that is the host fans a frame out only when its own device said it,
//! and a device takes one only when the signature is its host's.
//!
//! What the host may say is closed: a run's presence ([`HostWords::Presence`])
//! and the narration of a run's action ([`HostWords::Narration`]), each naming
//! the run it is said for. Neither carries record content: a run's face is
//! its name, colour, what it does, the record it works in and where its last
//! change left it; narration is the action's own semantic event, never a
//! preview of the text it wrote. [`HostSaid::admissible`] holds every frame
//! to that, on the side that signs and on every side that reads.

use collab_types::WorkspaceId;
use collab_types::presence::{DESKTOP_APP, Payload, Presence, PresenceKey};
use serde::{Deserialize, Serialize};

use crate::live::{LiveRecord, SemanticFrame};

/// Version of [`HostSaid`] this build signs and reads. A frame at another
/// version is dropped: a reader never guesses at what a host meant.
pub const HOST_VOICE_VERSION: u16 = 1;

/// Largest encoded [`HostSaid`] a relay fans out or a device reads: a run's
/// face, or one action's narration, with room to spare.
pub const MAX_HOST_SAID_BYTES: usize = 40 * 1024;

/// What a run's face may show of the record it works in: one window, its own.
const MAX_RUN_WINDOWS: usize = 1;

/// Domain separation for the host's signature over a [`HostSaid`], so it can
/// never be read as a signature over anything else.
const SAID_DOMAIN: &[u8] = b"tilo-host-voice-v1\0";

/// One thing the host says for a run it executes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HostSaid {
    /// [`HOST_VOICE_VERSION`].
    pub version: u16,
    /// The workspace it is said in, as the relay's room names it.
    pub workspace_id: WorkspaceId,
    /// The host device that says it: the key the signature verifies under.
    pub host: [u8; 32],
    /// What it says.
    pub words: HostWords,
}

/// What a host may say for one of its runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum HostWords {
    /// The run's presence: it arrived, its face, or it left. Always names
    /// the run ([`Presence::on_behalf`]).
    Presence(Presence),
    /// The narration of one action the run takes in `record`. Always names
    /// the run ([`SemanticFrame::on_behalf`]).
    Narration {
        /// The record the action works in.
        record: LiveRecord,
        /// The action's semantic event.
        event: SemanticFrame,
    },
}

/// Why a [`HostSaid`] is not one a device takes.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HostSaidError {
    /// It does not decode, or is larger than [`MAX_HOST_SAID_BYTES`].
    #[error("the host's frame does not decode")]
    Codec,
    /// It is at another [`HOST_VOICE_VERSION`].
    #[error("the host's frame is at another version")]
    Version,
    /// It names another workspace than the one it arrived in.
    #[error("the host's frame names another workspace")]
    Workspace,
    /// The signature is not the named host's, or the named host is not the
    /// device it was expected from.
    #[error("the host's frame is not signed by the host")]
    Signature,
    /// It is said for no run, or says more than a run's word.
    #[error("the host's frame says more than a run's word")]
    NotARunsWord,
}

impl HostSaid {
    /// The canonical encoding, the bytes the host signs.
    ///
    /// # Errors
    ///
    /// [`HostSaidError::Codec`] when it does not encode or is too large.
    pub fn encode(&self) -> Result<Vec<u8>, HostSaidError> {
        let bytes = postcard::to_stdvec(self).map_err(|_| HostSaidError::Codec)?;
        if bytes.len() > MAX_HOST_SAID_BYTES {
            return Err(HostSaidError::Codec);
        }
        Ok(bytes)
    }

    /// Whether this is a run's word and nothing more: said for a run, a
    /// presence of the host's desktop app in its own workspace that only
    /// arrives, leaves or shows the run's face, or a narration whose event
    /// is valid. A face shows an agent, at most the one window of the record
    /// it works in, and no chat, reaction or place of a person's own.
    ///
    /// # Errors
    ///
    /// [`HostSaidError::NotARunsWord`] when it is not.
    pub fn admissible(&self) -> Result<(), HostSaidError> {
        let refused = Err(HostSaidError::NotARunsWord);
        match &self.words {
            HostWords::Presence(presence) => {
                if !is_run(presence.on_behalf.as_deref())
                    || presence.app != DESKTOP_APP
                    || !matches!(&presence.key, PresenceKey::Workspace { id } if *id == presence.cluster)
                {
                    return refused;
                }
                match &presence.payload {
                    Payload::Arrived | Payload::Left => Ok(()),
                    Payload::Shell(view)
                        if view.agent
                            && view.chat.is_none()
                            && view.reaction.is_none()
                            && view.place.is_none()
                            && view.windows.len() <= MAX_RUN_WINDOWS =>
                    {
                        Ok(())
                    }
                    _ => refused,
                }
            }
            HostWords::Narration { record, event } => {
                if !is_run(event.on_behalf.as_deref()) {
                    return refused;
                }
                // The live wire's own bounds: a valid, versioned semantic event.
                let frame = crate::live::LiveFrame::Ops(crate::live::OpsFrame::Semantic {
                    record: record.clone(),
                    event: event.clone(),
                });
                frame.encode().map(|_| ()).or(refused)
            }
        }
    }

    /// Sign the frame with `sign`, the host device's signer over the bytes
    /// it is handed: the encoding and its signature, as
    /// [`crate::transport::Envelope::HostSaid`] carries them.
    ///
    /// # Errors
    ///
    /// [`HostSaidError::NotARunsWord`] for a frame no device would take, and
    /// [`HostSaidError::Codec`] when it does not encode.
    pub fn signing_bytes(&self) -> Result<(Vec<u8>, Vec<u8>), HostSaidError> {
        self.admissible()?;
        let said = self.encode()?;
        let signed = [SAID_DOMAIN, &said].concat();
        Ok((said, signed))
    }

    /// Read the frame `said`, signed `signature`, that arrived in
    /// `workspace_id`, said by `host`: the device the reader takes its host's
    /// word from. Only a frame the host signed, of that workspace, at this
    /// version, that is a run's word ([`Self::admissible`]), is taken.
    ///
    /// # Errors
    ///
    /// Why the frame is not taken ([`HostSaidError`]).
    pub fn open(
        said: &[u8],
        signature: &[u8; 64],
        workspace_id: WorkspaceId,
        host: &[u8; 32],
    ) -> Result<Self, HostSaidError> {
        if said.len() > MAX_HOST_SAID_BYTES {
            return Err(HostSaidError::Codec);
        }
        let frame: Self = postcard::from_bytes(said).map_err(|_| HostSaidError::Codec)?;
        if frame.version != HOST_VOICE_VERSION {
            return Err(HostSaidError::Version);
        }
        if frame.workspace_id != workspace_id {
            return Err(HostSaidError::Workspace);
        }
        if frame.host != *host || !verify(host, said, signature) {
            return Err(HostSaidError::Signature);
        }
        frame.admissible()?;
        Ok(frame)
    }
}

/// Whether `run` names a run: an `agent-run:` id, as the host mints them.
fn is_run(run: Option<&str>) -> bool {
    run.is_some_and(|run| {
        run.strip_prefix("agent-run:")
            .is_some_and(|id| !id.is_empty() && id.len() <= crate::live::MAX_OBJECT_ID_BYTES * 2)
    })
}

/// Whether `signature` is `host`'s over the encoded frame `said`.
fn verify(host: &[u8; 32], said: &[u8], signature: &[u8; 64]) -> bool {
    let Ok(key) = ed25519_dalek::VerifyingKey::from_bytes(host) else {
        return false;
    };
    key.verify_strict(
        &[SAID_DOMAIN, said].concat(),
        &ed25519_dalek::Signature::from_bytes(signature),
    )
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::presence::{ShellChat, ShellPresence, ShellWindow};
    use ed25519_dalek::Signer;

    fn key(seed: u8) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
    }

    fn face() -> ShellPresence {
        ShellPresence {
            seq: 1,
            sent_at_ms: 1_700_000_000_000,
            label: "Claude Code".into(),
            agent: true,
            colour: "#44aaff".into(),
            activity: Some("Looking at the apps".into()),
            place: None,
            size: [0, 0],
            windows: Vec::new(),
            selection: None,
            chat: None,
            reaction: None,
        }
    }

    fn said(host: &ed25519_dalek::SigningKey, payload: Payload, run: Option<&str>) -> HostSaid {
        let mut presence = Presence::desktop("w1", payload);
        presence.on_behalf = run.map(str::to_owned);
        HostSaid {
            version: HOST_VOICE_VERSION,
            workspace_id: WorkspaceId::from_u128(7),
            host: host.verifying_key().to_bytes(),
            words: HostWords::Presence(presence),
        }
    }

    fn sign(host: &ed25519_dalek::SigningKey, frame: &HostSaid) -> (Vec<u8>, [u8; 64]) {
        let (said, signed) = frame.signing_bytes().unwrap();
        (said, host.sign(&signed).to_bytes())
    }

    #[test]
    fn a_device_takes_its_hosts_word_for_a_run() {
        let host = key(1);
        let frame = said(
            &host,
            Payload::Shell(Box::new(face())),
            Some("agent-run:abc"),
        );
        let (bytes, signature) = sign(&host, &frame);
        let opened =
            HostSaid::open(&bytes, &signature, WorkspaceId::from_u128(7), &host.verifying_key().to_bytes()).unwrap();
        assert_eq!(opened, frame);
    }

    #[test]
    fn another_devices_word_is_not_the_hosts() {
        let host = key(1);
        let other = key(2);
        // Signed by another device, under its own name: not the host's.
        let frame = said(&other, Payload::Arrived, Some("agent-run:abc"));
        let (bytes, signature) = sign(&other, &frame);
        assert_eq!(
            HostSaid::open(&bytes, &signature, WorkspaceId::from_u128(7), &host.verifying_key().to_bytes()),
            Err(HostSaidError::Signature)
        );
        // Signed by another device, under the host's name: not the host's.
        let frame = said(&host, Payload::Arrived, Some("agent-run:abc"));
        let (bytes, _) = sign(&host, &frame);
        let forged = other.sign(&[SAID_DOMAIN, &bytes].concat()).to_bytes();
        assert_eq!(
            HostSaid::open(&bytes, &forged, WorkspaceId::from_u128(7), &host.verifying_key().to_bytes()),
            Err(HostSaidError::Signature)
        );
    }

    #[test]
    fn a_frame_is_read_only_in_its_workspace_and_version() {
        let host = key(1);
        let frame = said(&host, Payload::Left, Some("agent-run:abc"));
        let (bytes, signature) = sign(&host, &frame);
        let pinned = host.verifying_key().to_bytes();
        assert_eq!(
            HostSaid::open(&bytes, &signature, WorkspaceId::from_u128(8), &pinned),
            Err(HostSaidError::Workspace)
        );
        let mut later = frame;
        later.version = HOST_VOICE_VERSION + 1;
        let bytes = later.encode().unwrap();
        let signature = host.sign(&[SAID_DOMAIN, &bytes].concat()).to_bytes();
        assert_eq!(
            HostSaid::open(&bytes, &signature, WorkspaceId::from_u128(7), &pinned),
            Err(HostSaidError::Version)
        );
    }

    #[test]
    fn the_host_speaks_only_for_runs_and_says_no_more_than_their_word() {
        let host = key(1);
        // Of itself: the host's own presence rides the plane, never this.
        assert_eq!(
            said(&host, Payload::Arrived, None).signing_bytes(),
            Err(HostSaidError::NotARunsWord)
        );
        assert_eq!(
            said(&host, Payload::Arrived, Some("not-a-run")).signing_bytes(),
            Err(HostSaidError::NotARunsWord)
        );
        // A person's own words: chat beside a pointer, a status, the windows
        // of a desktop.
        let mut chatty = face();
        chatty.chat = Some(ShellChat {
            text: "the record's text".into(),
            sent: true,
        });
        let mut desktop = face();
        desktop.windows = vec![
            ShellWindow {
                app: "so.tilo.notes".into(),
                record: [1; 8],
                rect: [0, 0, 1, 1],
                focused: true,
            };
            2
        ];
        let mut person = face();
        person.agent = false;
        for view in [chatty, desktop, person] {
            assert_eq!(
                said(&host, Payload::Shell(Box::new(view)), Some("agent-run:abc")).signing_bytes(),
                Err(HostSaidError::NotARunsWord)
            );
        }
        let status = Payload::Status {
            text: "a record's text".into(),
            automatic: false,
        };
        assert_eq!(
            said(&host, status, Some("agent-run:abc")).signing_bytes(),
            Err(HostSaidError::NotARunsWord)
        );
        // A frame a forging host signed anyway is still not taken.
        let frame = said(&host, Payload::Arrived, None);
        let bytes = frame.encode().unwrap();
        let signature = host.sign(&[SAID_DOMAIN, &bytes].concat()).to_bytes();
        assert_eq!(
            HostSaid::open(&bytes, &signature, WorkspaceId::from_u128(7), &host.verifying_key().to_bytes()),
            Err(HostSaidError::NotARunsWord)
        );
    }
}
