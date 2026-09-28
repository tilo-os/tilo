//! Content keys for the shell's own participants.
//!
//! A room's app routes are encrypted channels (see [`super::channel_encryption`]):
//! what travels on them is sealed under the channel's content keys, which the
//! identity service hands an app for the routes of its own launch. The shell
//! takes part in rooms too, as participants of its own: the session recorder,
//! a replay peer, record custody. They are admitted to a room like any member,
//! so they reach its content the same way, through this device's channel
//! service, and within the same bounds:
//!
//! - Keys reach only a participant admitted to the room, only for that room's
//!   channels, and never leave this device.
//! - Reading and writing are separate handles. A [`ChannelOpener`] can only
//!   open: the recorder, an Observer, holds nothing else, so it can read a
//!   room and never speak in it. A [`ChannelSealer`] is bound to one room and
//!   is handed only to a participant that may write there: a replay peer in
//!   its own replay room, record custody in a room it serves.
//! - Nothing is cached. Every open and seal asks again whether the
//!   participant is admitted and takes the keys the channel holds now, so a
//!   revoked admission, or a removed device, loses them at once.
//! - A public room keeps no group key: its content is plaintext, and a
//!   plaintext op is what an opener reads and a sealer writes there.

use collab_types::{ArtifactId, EncryptionMetadata, SignedOp};

use super::channel_encryption::{ChannelAccess, ChannelKeyProvider};
use super::collab_broker::CrdtRoute;
use super::state::{CapRole, WorkspaceAuthority as _, WorkspaceTreeHandle};

/// Who asks for a room's content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Participant {
    /// An agent participant, by the agent id it is admitted as.
    Agent(String),
    /// This device's person, by principal id.
    Person(String),
}

/// Why a room's content could not be opened or sealed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    #[error("{0:?} is not admitted to this room")]
    NotAdmitted(Participant),
    #[error("{0:?} may not write in this room")]
    ReadOnly(Participant),
    #[error("this handle is for another room")]
    OtherRoom,
    #[error("this device holds no keys for the channel yet")]
    Waiting,
    #[error("content could not be opened or sealed: {0}")]
    Crypto(String),
}

/// How a route's content travels right now.
enum Content {
    /// Plaintext: a public room, or a session with no channel service.
    Plain,
    Sealed(channel_crypto::ChannelContentKeys),
}

/// The door the shell's participants take to a room's content.
#[derive(Clone)]
pub struct ParticipantKeys {
    channels: ChannelKeyProvider,
    tree: WorkspaceTreeHandle,
}

impl ParticipantKeys {
    pub fn new(channels: ChannelKeyProvider, tree: WorkspaceTreeHandle) -> Self {
        Self { channels, tree }
    }

    /// The keys `broker`'s channel service holds.
    pub fn for_broker(
        broker: &super::collab_broker::BrokerHandle,
        tree: WorkspaceTreeHandle,
    ) -> Self {
        let channels = ChannelKeyProvider::default();
        channels.set(broker.channel_encryption());
        Self::new(channels, tree)
    }

    /// Point these keys at a channel service (or at none).
    pub fn set_channels(&self, access: Option<super::channel_encryption::ChannelKeyAccess>) {
        self.channels.set(access);
    }

    /// An open-only handle for `who`, for every room it is admitted to.
    pub fn opener(&self, who: Participant) -> ChannelOpener {
        ChannelOpener {
            keys: self.clone(),
            who,
        }
    }

    /// A sealing handle for `who` in `room` alone.
    pub fn sealer(&self, who: Participant, room: &str) -> ChannelSealer {
        ChannelSealer {
            opener: self.opener(who),
            room: room.to_owned(),
        }
    }

    /// Forget every channel of the ephemeral `room`: it is closed, and
    /// nothing it held is kept.
    pub fn forget_room(&self, room: super::state::RoomId) {
        if let Some(channels) = self.channels.get() {
            channels.forget_room(room);
        }
    }

    /// `who`'s role in the room `route` travels in, and that room's class.
    fn admission(
        &self,
        who: &Participant,
        route: &CrdtRoute,
    ) -> Option<(CapRole, collab_types::ChannelClass)> {
        let tree = self.tree.read();
        let workspace = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == route.cluster_id)?;
        let role = match who {
            Participant::Agent(agent) => workspace.role_of_agent(agent),
            Participant::Person(person) => workspace.role_of(person),
        }?;
        let class = workspace.channel_class;
        drop(tree);
        (role != CapRole::Denied).then_some((role, class))
    }

    /// What `route`'s content travels as for an admitted `who`.
    fn content(&self, who: &Participant, route: &CrdtRoute) -> Result<Content, KeyError> {
        let (_, class) = self
            .admission(who, route)
            .ok_or_else(|| KeyError::NotAdmitted(who.clone()))?;
        if !class.keeps_group_key() {
            return Ok(Content::Plain);
        }
        let Some(channels) = self.channels.get() else {
            return Ok(Content::Plain);
        };
        let room = super::state::RoomId::parse(&route.cluster_id)
            .ok_or_else(|| KeyError::NotAdmitted(who.clone()))?;
        let channel =
            collab_types::document_channel_id(&route.app_id, &route.cluster_id, &route.record_id);
        match channels.access(room, channel).0 {
            ChannelAccess::Ready(keys) => Ok(Content::Sealed(keys)),
            ChannelAccess::Waiting | ChannelAccess::Removed | ChannelAccess::Unavailable => {
                Err(KeyError::Waiting)
            }
        }
    }
}

/// Reads a room's content; can never write it.
#[derive(Clone)]
pub struct ChannelOpener {
    keys: ParticipantKeys,
    who: Participant,
}

impl ChannelOpener {
    /// The cleartext `envelope` carries on `route`.
    ///
    /// # Errors
    ///
    /// When the participant is not admitted, the keys are not held, or the
    /// payload does not open.
    pub fn open(&self, route: &CrdtRoute, envelope: &SignedOp) -> Result<Vec<u8>, KeyError> {
        let content = self.keys.content(&self.who, route)?;
        let Some(metadata) = envelope.encryption.as_ref() else {
            return Ok(envelope.payload.clone());
        };
        match content {
            Content::Plain => Err(KeyError::Crypto(
                "sealed content where the room keeps none".into(),
            )),
            Content::Sealed(keys) => keys
                .open_content(envelope.artifact, metadata, &envelope.payload)
                .map(|opened| opened.to_vec())
                .map_err(|error| KeyError::Crypto(error.to_string())),
        }
    }

    /// `envelope` with its content opened: what was said, kept on this
    /// device. Its signature covers the sealed form it arrived in.
    ///
    /// # Errors
    ///
    /// As [`Self::open`].
    pub fn opened(&self, route: &CrdtRoute, envelope: &SignedOp) -> Result<SignedOp, KeyError> {
        if envelope.encryption.is_none() {
            return Ok(envelope.clone());
        }
        let payload = self.open(route, envelope)?;
        let mut opened = envelope.clone();
        opened.payload = payload;
        opened.encryption = None;
        Ok(opened)
    }
}

/// Reads and writes one room's content.
#[derive(Clone)]
pub struct ChannelSealer {
    opener: ChannelOpener,
    room: String,
}

impl ChannelSealer {
    /// Seal `cleartext` for `artifact` on `route`: the payload to sign, and
    /// the metadata naming how it was sealed (`None` in a room that keeps
    /// no group key).
    ///
    /// # Errors
    ///
    /// When `route` is in another room, the participant may not write
    /// there, or the keys are not held.
    pub fn seal(
        &self,
        route: &CrdtRoute,
        artifact: ArtifactId,
        cleartext: &[u8],
    ) -> Result<(Vec<u8>, Option<EncryptionMetadata>), KeyError> {
        if route.cluster_id != self.room {
            return Err(KeyError::OtherRoom);
        }
        let who = &self.opener.who;
        let (role, _) = self
            .opener
            .keys
            .admission(who, route)
            .ok_or_else(|| KeyError::NotAdmitted(who.clone()))?;
        if role == CapRole::Viewer {
            return Err(KeyError::ReadOnly(who.clone()));
        }
        match self.opener.keys.content(who, route)? {
            Content::Plain => Ok((cleartext.to_vec(), None)),
            Content::Sealed(keys) => keys
                .seal_content(
                    artifact,
                    collab_types::DocumentFrame::retention(cleartext),
                    cleartext,
                )
                .map(|(metadata, sealed)| (sealed, Some(metadata)))
                .map_err(|error| KeyError::Crypto(error.to_string())),
        }
    }

    /// See [`ChannelOpener::open`].
    ///
    /// # Errors
    ///
    /// As [`ChannelOpener::open`].
    pub fn open(&self, route: &CrdtRoute, envelope: &SignedOp) -> Result<Vec<u8>, KeyError> {
        self.opener.open(route, envelope)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::{Arc, Mutex};

    use collab_types::{ChannelClass, OpClass};

    use super::*;
    use crate::shell::channel_encryption::{
        ChannelEncryption, ChannelEncryptionConfig, ChannelKeyAccess,
    };
    use crate::shell::collab_core::BrokerChannels;
    use crate::shell::recorder::{RECORDER_ADMISSION, RECORDER_AGENT};
    use crate::shell::replay::{REPLAY_ADMISSION, REPLAY_AGENT};
    use crate::shell::state::{CapOverride, Subject, Verb, WorkspaceId, WorkspaceTree};
    use tilo_action_broker::channel_encryption::testing::{Directory, Policy, key, public};

    /// The device the fixture's room lives on; its person owns the room.
    pub(crate) const DEVICE: u8 = 50;

    /// One room this device governs, with the recorder admitted as an
    /// Observer and the replay peer as a contributor, and the device's
    /// channel service holding its keys.
    pub(crate) struct Fixture {
        pub(crate) keys: ParticipantKeys,
        pub(crate) tree: WorkspaceTreeHandle,
        pub(crate) workspace: WorkspaceId,
        pub(crate) room: String,
        /// The owner: this device's person.
        pub(crate) owner: String,
        pub(crate) state: tempfile::TempDir,
    }

    impl Fixture {
        pub(crate) fn new(class: ChannelClass) -> Self {
            let owner = crate::shell::collab_broker_capnp::principal_id_for_root(
                *public(DEVICE).as_bytes(),
            );
            let mut tree = WorkspaceTree::default();
            let workspace = tree.append_empty_workspace("Notes");
            let room = tree.room_of(workspace).expect("room").to_string();
            let entry = tree
                .desktop
                .workspaces
                .iter_mut()
                .find(|entry| entry.id == workspace)
                .expect("workspace");
            entry.set_owner(owner.clone());
            entry.channel_class = class;
            for (agent, verb, id) in [
                (RECORDER_AGENT, Verb::Read, RECORDER_ADMISSION),
                (REPLAY_AGENT, Verb::Write, REPLAY_ADMISSION),
            ] {
                let cap = CapOverride::membership(Subject::agent(agent), room.clone(), verb)
                    .with_id(id.to_string());
                assert!(tree.grant_in_workspace(workspace, &owner, cap).is_some());
            }
            let tree = tree.shared();
            let state = tempfile::tempdir().expect("state dir");
            let service = ChannelEncryption::open(
                ChannelEncryptionConfig {
                    root: state.path().join("channels"),
                    vault: channel_crypto::VaultKey::new([DEVICE; 32]),
                    device_key: key(DEVICE),
                },
                Box::new(Directory {
                    governs: true,
                    steward: public(DEVICE),
                    policy: Policy::default(),
                }),
                Arc::new(Mutex::new(BrokerChannels::default())),
            )
            .expect("channel service");
            let channels = ChannelKeyProvider::default();
            channels.set(Some(ChannelKeyAccess::new(service)));
            Self {
                keys: ParticipantKeys::new(channels, Arc::clone(&tree)),
                tree,
                workspace,
                room,
                owner,
                state,
            }
        }

        pub(crate) fn route(&self) -> CrdtRoute {
            CrdtRoute::record("so.tilo.notes", &self.room, "note")
        }

        /// This device's person, the room's owner.
        pub(crate) fn person(&self) -> Participant {
            Participant::Person(self.owner.clone())
        }
    }

    fn recorder() -> Participant {
        Participant::Agent(RECORDER_AGENT.into())
    }

    pub(crate) fn replay() -> Participant {
        Participant::Agent(REPLAY_AGENT.into())
    }

    fn artifact(route: &CrdtRoute) -> ArtifactId {
        collab_types::document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id)
    }

    /// An op carrying what `sealer` sealed for `route`.
    fn sealed_op(sealer: &ChannelSealer, route: &CrdtRoute, cleartext: &[u8]) -> SignedOp {
        let (payload, encryption) = sealer
            .seal(route, artifact(route), cleartext)
            .expect("sealed");
        SignedOp::new_unsigned_with_encryption(
            payload,
            artifact(route),
            collab_types::Ed25519PublicKey::from_bytes([1; 32]),
            collab_types::document_attestation_id(
                &route.app_id,
                &route.cluster_id,
                &route.record_id,
            ),
            OpClass::Splice,
            std::time::SystemTime::now(),
            1,
            encryption.expect("an encrypted room seals"),
        )
    }

    #[test]
    fn the_recorder_opens_what_is_said_and_cannot_speak() {
        let fixture = Fixture::new(ChannelClass::Encrypted);
        let route = fixture.route();
        let said = b"a change".to_vec();
        let op = sealed_op(&fixture.keys.sealer(replay(), &fixture.room), &route, &said);
        assert_ne!(op.payload, said, "what travels is sealed");

        let opener = fixture.keys.opener(recorder());
        assert_eq!(opener.open(&route, &op), Ok(said.clone()));
        let clear = opener.opened(&route, &op).expect("opened");
        assert_eq!((clear.payload, clear.encryption), (said.clone(), None));

        assert_eq!(
            fixture
                .keys
                .sealer(recorder(), &fixture.room)
                .seal(&route, op.artifact, &said),
            Err(KeyError::ReadOnly(recorder())),
            "an Observer never writes"
        );
    }

    #[test]
    fn a_revoked_recorder_loses_its_keys_at_once() {
        let fixture = Fixture::new(ChannelClass::Encrypted);
        let route = fixture.route();
        let opener = fixture.keys.opener(recorder());
        let op = sealed_op(
            &fixture.keys.sealer(replay(), &fixture.room),
            &route,
            b"one",
        );
        assert!(opener.open(&route, &op).is_ok());

        assert!(fixture.tree.write().revoke_in_workspace(
            fixture.workspace,
            &fixture.owner,
            RECORDER_ADMISSION
        ));
        assert_eq!(
            opener.open(&route, &op),
            Err(KeyError::NotAdmitted(recorder())),
            "the handle it still holds opens nothing"
        );
    }

    #[test]
    fn keys_reach_only_admitted_participants_in_their_own_room() {
        let fixture = Fixture::new(ChannelClass::Encrypted);
        let route = fixture.route();
        let elsewhere = fixture
            .keys
            .sealer(replay(), &crate::shell::state::RoomId::new().to_string());
        assert_eq!(
            elsewhere.seal(&route, artifact(&route), b"x"),
            Err(KeyError::OtherRoom)
        );
        let op = sealed_op(&fixture.keys.sealer(replay(), &fixture.room), &route, b"x");
        let stranger = Participant::Agent("stranger".into());
        assert_eq!(
            fixture.keys.opener(stranger.clone()).open(&route, &op),
            Err(KeyError::NotAdmitted(stranger))
        );
    }

    #[test]
    fn a_public_room_is_plaintext() {
        let fixture = Fixture::new(ChannelClass::Public);
        let route = fixture.route();
        assert_eq!(
            fixture.keys.sealer(fixture.person(), &fixture.room).seal(
                &route,
                artifact(&route),
                b"open"
            ),
            Ok((b"open".to_vec(), None))
        );
    }

    #[test]
    fn a_forgotten_room_keeps_no_channel_state() {
        let fixture = Fixture::new(ChannelClass::Encrypted);
        let route = fixture.route();
        sealed_op(&fixture.keys.sealer(replay(), &fixture.room), &route, b"x");
        let kept = fixture.state.path().join("channels").join(&fixture.room);
        assert!(
            kept.exists(),
            "the steward keeps its group while the room is open"
        );
        let room = crate::shell::state::RoomId::parse(&fixture.room).expect("room id");
        fixture.keys.forget_room(room);
        assert!(!kept.exists());
    }
}
