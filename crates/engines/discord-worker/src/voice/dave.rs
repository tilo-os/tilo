//! DAVE, Discord's end-to-end encryption for voice (daveprotocol.com), on
//! top of the `davey` session: the voice gateway's opcodes 11, 13 and 21-31
//! in, the answers out, and each media frame through the group's keys.
//!
//! The voice server is the MLS group's external sender. It proposes adding
//! and removing members; every member commits, the server picks one commit
//! per epoch and announces it as a transition, members say when they are
//! ready, and the server executes it. Transition 0 is executed at once.
//! A downgrade to protocol version 0 (a client without DAVE joined) moves
//! receivers to passthrough first and senders on execution.

#![allow(
    clippy::indexing_slicing,
    reason = "serde_json::Value's string index answers Null for anything missing and never panics"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU16;

use davey::errors::ProcessCommitError;
use davey::{DaveSession, MediaType, ProposalsOperationType};
use serde_json::{Value, json};

/// The highest protocol version the worker speaks, said in Identify.
pub const MAX_VERSION: u16 = davey::DAVE_PROTOCOL_VERSION;
/// How long receivers accept unencrypted frames around a transition that
/// turns encryption on or off, in seconds.
const PASSTHROUGH_SECONDS: u32 = 10;
const DOWNGRADE_PASSTHROUGH_SECONDS: u32 = 120;

/// A frame for the voice gateway.
#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    Json(Value),
    /// Opcode first, then its payload (client frames carry no sequence).
    Binary(Vec<u8>),
}

pub struct Dave {
    user: u64,
    channel: u64,
    /// The protocol version media is sent with now; 0 is transport only.
    version: u16,
    session: Option<DaveSession>,
    /// Announced transitions not executed yet, with their version.
    pending: BTreeMap<u16, u16>,
    /// Who the voice gateway says is in the call, the worker included: an
    /// Add proposal for anyone else is refused.
    recognized: BTreeSet<u64>,
}

impl Dave {
    #[must_use]
    pub fn new(user: u64, channel: u64) -> Self {
        Self {
            user,
            channel,
            version: 0,
            session: None,
            pending: BTreeMap::new(),
            recognized: BTreeSet::from([user]),
        }
    }

    /// The protocol version media goes out with.
    #[must_use]
    pub const fn version(&self) -> u16 {
        self.version
    }

    /// Whether frames can be sent: transport only, or the group's keys are
    /// in place.
    #[must_use]
    pub fn can_send(&self) -> bool {
        self.version == 0 || self.session.as_ref().is_some_and(DaveSession::is_ready)
    }

    /// The voice privacy code people compare in Discord's client.
    #[must_use]
    pub fn privacy_code(&self) -> Option<String> {
        self.session
            .as_ref()
            .and_then(DaveSession::voice_privacy_code)
            .map(str::to_owned)
    }

    /// Session Description named the protocol version the call starts
    /// with: make the MLS session and offer a key package.
    pub fn start(&mut self, version: u16) -> Vec<Frame> {
        self.version = version;
        self.reinit()
    }

    /// A JSON opcode of the voice gateway.
    pub fn json(&mut self, op: u64, data: &Value) -> Vec<Frame> {
        match op {
            11 => {
                self.recognized.extend(
                    data["user_ids"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|id| id.as_str()?.parse::<u64>().ok()),
                );
                Vec::new()
            }
            13 => {
                if let Some(id) = data["user_id"].as_str().and_then(|id| id.parse().ok()) {
                    self.recognized.remove(&id);
                }
                Vec::new()
            }
            21 => {
                let (Some(transition), Some(version)) = (
                    number(&data["transition_id"]),
                    number(&data["protocol_version"]),
                ) else {
                    return Vec::new();
                };
                self.pending.insert(transition, version);
                if transition == 0 {
                    self.execute(0);
                    Vec::new()
                } else {
                    if version == 0
                        && let Some(session) = &mut self.session
                    {
                        session.set_passthrough_mode(true, Some(DOWNGRADE_PASSTHROUGH_SECONDS));
                    }
                    vec![ready(transition)]
                }
            }
            22 => {
                if let Some(transition) = number(&data["transition_id"]) {
                    self.execute(transition);
                }
                Vec::new()
            }
            24 => {
                // Epoch 1 is a new group: start over with a new key package.
                if data["epoch"].as_u64() == Some(1) {
                    self.version = number(&data["protocol_version"]).unwrap_or(self.version);
                    return self.reinit();
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// A binary opcode: `frame` is the opcode and its payload, the
    /// sequence number already taken off.
    pub fn binary(&mut self, frame: &[u8]) -> Vec<Frame> {
        let Some((&op, payload)) = frame.split_first() else {
            return Vec::new();
        };
        match op {
            25 => {
                if let Some(session) = &mut self.session {
                    let _ = session.set_external_sender(payload);
                }
                Vec::new()
            }
            27 => self.proposals(payload),
            29 => self.transition_with(payload, |session, commit| {
                match session.process_commit(commit) {
                    Ok(()) => Processed::Done,
                    // Not a member yet: the welcome that follows is ours.
                    Err(ProcessCommitError::NoGroup | ProcessCommitError::PendingGroup) => {
                        Processed::NotOurs
                    }
                    Err(_) => Processed::Invalid,
                }
            }),
            30 => self.transition_with(payload, |session, welcome| {
                match session.process_welcome(welcome) {
                    Ok(()) => Processed::Done,
                    Err(_) => Processed::Invalid,
                }
            }),
            _ => Vec::new(),
        }
    }

    fn proposals(&mut self, payload: &[u8]) -> Vec<Frame> {
        let Some((&kind, proposals)) = payload.split_first() else {
            return Vec::new();
        };
        let kind = if kind == 0 {
            ProposalsOperationType::APPEND
        } else {
            ProposalsOperationType::REVOKE
        };
        let recognized: Vec<u64> = self.recognized.iter().copied().collect();
        let Some(session) = &mut self.session else {
            return Vec::new();
        };
        match session.process_proposals(kind, proposals, Some(&recognized)) {
            Ok(Some(commit)) => {
                let mut frame = vec![28];
                frame.extend_from_slice(&commit.commit);
                if let Some(welcome) = commit.welcome {
                    frame.extend_from_slice(&welcome);
                }
                vec![Frame::Binary(frame)]
            }
            Ok(None) | Err(_) => Vec::new(),
        }
    }

    /// An announced commit (29) or a welcome (30): `transition id, message`.
    /// What cannot be processed is reported as invalid and the worker
    /// rejoins the group with a fresh key package.
    fn transition_with(
        &mut self,
        payload: &[u8],
        process: impl FnOnce(&mut DaveSession, &[u8]) -> Processed,
    ) -> Vec<Frame> {
        let Some((id, message)) = payload.split_first_chunk::<2>() else {
            return Vec::new();
        };
        let transition = u16::from_be_bytes(*id);
        let Some(session) = &mut self.session else {
            return Vec::new();
        };
        match process(session, message) {
            Processed::NotOurs => return Vec::new(),
            Processed::Done if transition == 0 => return Vec::new(),
            Processed::Done => {
                self.pending.insert(transition, self.version.max(1));
                return vec![ready(transition)];
            }
            Processed::Invalid => {}
        }
        let mut frames = vec![Frame::Json(
            json!({"op": 31, "d": {"transition_id": transition}}),
        )];
        frames.extend(self.reinit());
        frames
    }

    fn execute(&mut self, transition: u16) {
        let Some(version) = self.pending.remove(&transition) else {
            return;
        };
        let was = std::mem::replace(&mut self.version, version);
        if transition > 0
            && was == 0
            && version != 0
            && let Some(session) = &mut self.session
        {
            session.set_passthrough_mode(true, Some(PASSTHROUGH_SECONDS));
        }
    }

    /// A new MLS session for the current version and its key package, or
    /// passthrough when the call is transport only.
    fn reinit(&mut self) -> Vec<Frame> {
        let Some(version) = NonZeroU16::new(self.version) else {
            if let Some(session) = &mut self.session {
                let _ = session.reset();
                session.set_passthrough_mode(true, Some(PASSTHROUGH_SECONDS));
            }
            return Vec::new();
        };
        let made = if let Some(session) = &mut self.session {
            session
                .reinit(version, self.user, self.channel, None)
                .is_ok()
        } else if let Ok(session) = DaveSession::new(version, self.user, self.channel, None) {
            self.session = Some(session);
            true
        } else {
            false
        };
        let package = made
            .then(|| {
                self.session
                    .as_mut()
                    .and_then(|session| session.create_key_package().ok())
            })
            .flatten();
        package
            .map(|package| {
                let mut frame = vec![26];
                frame.extend_from_slice(&package);
                Frame::Binary(frame)
            })
            .into_iter()
            .collect()
    }

    /// End-to-end encrypt one Opus frame; `None` while the group's keys
    /// are not in place.
    pub fn encrypt(&mut self, opus: &[u8]) -> Option<Vec<u8>> {
        if self.version == 0 {
            return Some(opus.to_vec());
        }
        let session = self.session.as_mut()?;
        session
            .encrypt_opus(opus)
            .ok()
            .map(std::borrow::Cow::into_owned)
    }

    /// The Opus frame `user` sent, decrypted; frames during a transition
    /// pass through as the protocol allows.
    pub fn decrypt(&mut self, user: u64, frame: &[u8]) -> Option<Vec<u8>> {
        match &mut self.session {
            Some(session) => match session.decrypt(user, MediaType::AUDIO, frame) {
                Ok(opus) => Some(opus),
                Err(_) if self.version == 0 && !is_protocol_frame(frame) => Some(frame.to_vec()),
                Err(_) => None,
            },
            None if self.version == 0 => Some(frame.to_vec()),
            None => None,
        }
    }
}

enum Processed {
    Done,
    NotOurs,
    Invalid,
}

/// Whether `frame` ends with DAVE's magic marker.
fn is_protocol_frame(frame: &[u8]) -> bool {
    frame.ends_with(&[0xfa, 0xfa])
}

fn ready(transition: u16) -> Frame {
    Frame::Json(json!({"op": 23, "d": {"transition_id": transition}}))
}

fn number(value: &Value) -> Option<u16> {
    value.as_u64().and_then(|n| u16::try_from(n).ok())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::arithmetic_side_effects,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

    #[test]
    fn transport_only_passes_frames_through() {
        let mut dave = Dave::new(2000, 1102);
        assert!(dave.start(0).is_empty(), "no key package without DAVE");
        assert!(dave.can_send());
        assert_eq!(dave.encrypt(b"opus").unwrap(), b"opus");
        assert_eq!(dave.decrypt(7, b"opus").unwrap(), b"opus");
    }

    #[test]
    fn version_one_offers_a_key_package_and_waits_for_the_group() {
        let mut dave = Dave::new(2000, 1102);
        let frames = dave.start(1);
        assert!(matches!(frames.as_slice(), [Frame::Binary(f)] if f[0] == 26));
        assert!(!dave.can_send(), "no keys before the group exists");
        assert!(dave.encrypt(b"opus").is_none());
    }

    #[test]
    fn a_downgrade_is_acknowledged_and_executed() {
        let mut dave = Dave::new(2000, 1102);
        dave.start(1);
        let answer = dave.json(21, &json!({"transition_id": 4, "protocol_version": 0}));
        assert_eq!(
            answer,
            vec![Frame::Json(json!({"op": 23, "d": {"transition_id": 4}}))]
        );
        assert_eq!(dave.version(), 1, "senders switch only on execution");
        dave.json(22, &json!({"transition_id": 4}));
        assert_eq!(dave.version(), 0);
        assert_eq!(dave.encrypt(b"opus").unwrap(), b"opus");
    }

    #[test]
    fn a_new_epoch_starts_over_and_bad_commits_are_reported() {
        let mut dave = Dave::new(2000, 1102);
        dave.start(1);
        let frames = dave.json(24, &json!({"epoch": 1, "protocol_version": 1}));
        assert!(matches!(frames.as_slice(), [Frame::Binary(f)] if f[0] == 26));
        assert!(
            dave.binary(&[29, 0, 9, 1, 2, 3]).is_empty(),
            "a commit for a group the worker is not in yet is not its business"
        );
        let frames = dave.binary(&[30, 0, 9, 1, 2, 3]);
        assert_eq!(
            frames[0],
            Frame::Json(json!({"op": 31, "d": {"transition_id": 9}}))
        );
        assert!(
            matches!(&frames[1], Frame::Binary(f) if f[0] == 26),
            "a fresh key package follows"
        );
    }
}
