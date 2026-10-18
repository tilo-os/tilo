//! The rooms this relay lists as public workspaces, and the door that lets
//! anyone in with the role their owner chose.
//!
//! Making a room public is one of its governance settings
//! ([`governance::SETTINGS`]): its owner, or anyone the room lets change its
//! settings, publishes a [`Listing`] (a description and the default role)
//! through the room's `publish` action and takes it back with `unpublish`.
//! The relay keeps the listings beside its rooms (`listings.json`), lists
//! them in its directory ([`Rooms::directory`]) and answers each room's door
//! ([`Rooms::admit_listed`]): a person who asks gets an invitation only they
//! can redeem and the one-time token of a pre-admission at the default role,
//! so their redemption is admitted at once, as the room's owner decided when
//! they published it. The listing is that decision; no one is asked per
//! join. The host signs each such grant with its own device key, and a
//! redemption is admitted only under a grant the owner or this host's door
//! signed while the room is public.
//!
//! Moderation is the room's own: a banned person is refused at the door, the
//! room's moderators remove and ban through its governance, each admission
//! is a [`RoomNotice::PreAdmitted`] the owner sees, and a report of the
//! workspace through its door is a [`RoomNotice::Reported`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tilo_action_policy::roles::governance;
use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::directory::{
    DirectoryEntry, DoorJoin, DoorReport, Face, Listing, MAX_FACES, door_path,
};
use tilo_relay_protocol::rooms::{PreAdmission, RoomNotice};
use tilo_relay_session::rooms::{IssueInvitationRequest, IssuedInvitation};
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

use super::authority::RoomOp;
use super::{CollaborationError, Rooms};

/// Where the listings are kept, under the rooms' directory.
const LISTINGS_FILE: &str = "listings.json";
/// How long a door's invitation and its token stay good: time for the
/// person's device to redeem it now, not to keep it.
const DOOR_INVITATION_NS: u64 = 10 * 60 * 1_000_000_000;
/// How a door's admission names where the person came from, in the
/// owner's notices.
pub(crate) const DOOR_LABEL: &str = "the public directory";

#[derive(Default, Serialize, Deserialize)]
struct Stored {
    /// Each public room's listing, by room UUID.
    rooms: BTreeMap<String, Listing>,
}

/// The public rooms, and when someone last changed something in each.
pub(super) struct Listings {
    path: PathBuf,
    stored: std::sync::Mutex<Stored>,
    /// When a member last wrote to each room, Unix milliseconds, since this
    /// relay started: the directory's activity hint.
    active: std::sync::Mutex<BTreeMap<String, u64>>,
    /// The room UUID after each publish or unpublish.
    changes: tokio::sync::broadcast::Sender<String>,
}

impl Listings {
    /// The listings kept under `dir`; none yet, or none readable, is none.
    pub(super) fn open(dir: &Path) -> Self {
        let path = dir.join(LISTINGS_FILE);
        let stored = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                tracing::warn!(path = %path.display(), %error, "the public listings do not parse; listing nothing");
                Stored::default()
            }),
            Err(_) => Stored::default(),
        };
        Self {
            path,
            stored: std::sync::Mutex::new(stored),
            active: std::sync::Mutex::default(),
            changes: tokio::sync::broadcast::channel(16).0,
        }
    }

    fn get(&self, room: &str) -> Option<Listing> {
        self.stored.lock_recover().rooms.get(room).cloned()
    }

    fn listed(&self) -> Vec<(String, Listing)> {
        self.stored
            .lock_recover()
            .rooms
            .iter()
            .map(|(room, listing)| (room.clone(), listing.clone()))
            .collect()
    }

    /// List `room` with `listing`, or take it out of the directory.
    fn set(&self, room: &str, listing: Option<Listing>) -> Result<(), String> {
        let mut stored = self.stored.lock_recover();
        let changed = match listing {
            Some(listing) => stored.rooms.insert(room.to_owned(), listing.clone()) != Some(listing),
            None => stored.rooms.remove(room).is_some(),
        };
        if !changed {
            return Ok(());
        }
        let bytes = serde_json::to_vec_pretty(&*stored).map_err(|error| error.to_string())?;
        tilo_files::native::atomic::write(&self.path, &bytes).map_err(|error| error.to_string())?;
        drop(stored);
        let _ = self.changes.send(room.to_owned());
        Ok(())
    }

    /// Someone changed something in `room` at `now_ms`.
    pub(super) fn touch(&self, room: &str, now_ms: u64) {
        self.active.lock_recover().insert(room.to_owned(), now_ms);
    }

    fn last_active(&self, room: &str) -> Option<u64> {
        self.active.lock_recover().get(room).copied()
    }
}

/// The people of `members` in the room now, by `online` user roots: their
/// faces (at most [`MAX_FACES`]) and how many there are.
fn faces(
    members: &[tilo_peer_state::GovernedMember],
    online: &BTreeSet<[u8; 32]>,
) -> (Vec<Face>, u32) {
    let here: Vec<_> = members
        .iter()
        .filter(|member| !member.removed && online.contains(&member.user_public_key))
        .collect();
    let count = u32::try_from(here.len()).unwrap_or(u32::MAX);
    let faces = here
        .into_iter()
        .take(MAX_FACES)
        .map(|member| Face {
            user_public_key: hex::encode(member.user_public_key),
            name: member.display_name.clone().unwrap_or_default(),
        })
        .collect();
    (faces, count)
}

fn person(hex_key: &str) -> Result<[u8; 32], CollaborationError> {
    let mut key = [0; 32];
    hex::decode_to_slice(hex_key, &mut key)
        .map_err(|_| CollaborationError::new("a person is 64 hex characters"))?;
    Ok(key)
}

impl Rooms {
    /// Make `room` public with `listing`, as `device` asks: the room's
    /// governance must let its person change the room's settings.
    ///
    /// # Errors
    ///
    /// The room is not governed here, the person may not change its
    /// settings, the listing is not one that may be published, or it could
    /// not be kept.
    pub async fn publish(
        &self,
        room: &str,
        device: &DeviceCert,
        listing: Listing,
    ) -> Result<Listing, CollaborationError> {
        let listing = listing
            .checked()
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        {
            let governed = self.room(room).await?;
            let governed = governed.lock().await;
            self.authorize(
                device,
                Some(&governed),
                RoomOp::Govern(governance::SETTINGS),
                tilo_clock::unix_now_ns(),
            )?;
        }
        self.inner
            .listings
            .set(room, Some(listing.clone()))
            .map_err(CollaborationError::new)?;
        tracing::info!(%room, role = ?listing.default_role, "workspace made public");
        Ok(listing)
    }

    /// Take `room` out of the directory, as `device` asks. Its members stay;
    /// nobody new gets in through its door.
    ///
    /// # Errors
    ///
    /// As [`Self::publish`].
    pub async fn unpublish(
        &self,
        room: &str,
        device: &DeviceCert,
    ) -> Result<(), CollaborationError> {
        {
            let governed = self.room(room).await?;
            let governed = governed.lock().await;
            self.authorize(
                device,
                Some(&governed),
                RoomOp::Govern(governance::SETTINGS),
                tilo_clock::unix_now_ns(),
            )?;
        }
        self.inner
            .listings
            .set(room, None)
            .map_err(CollaborationError::new)?;
        tracing::info!(%room, "workspace no longer public");
        Ok(())
    }

    /// The listing `room` is public with, if it is.
    #[must_use]
    pub fn listing(&self, room: &str) -> Option<Listing> {
        self.inner.listings.get(room)
    }

    /// The rooms public here now.
    #[must_use]
    pub fn published_rooms(&self) -> Vec<String> {
        self.inner
            .listings
            .listed()
            .into_iter()
            .map(|(room, _)| room)
            .collect()
    }

    /// The room UUID after each publish or unpublish.
    #[must_use]
    pub fn listing_changes(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.inner.listings.changes.subscribe()
    }

    /// Keep the directory's activity hint: each member write marks its room
    /// active, for as long as the rooms are governed here.
    pub(super) fn follow_activity(&self) {
        let mut writes = self.record_writes();
        let rooms = std::sync::Arc::downgrade(&self.inner);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        runtime.spawn(async move {
            loop {
                match writes.recv().await {
                    Ok(write) => match rooms.upgrade() {
                        Some(inner) => inner.listings.touch(&write.room, tilo_clock::unix_now_ms()),
                        None => return,
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
    }

    /// Who of `room`'s members is in it now by `online` (user roots with an
    /// open session here): some of their faces, how many they are, and how
    /// many people belong to the room.
    ///
    /// # Errors
    ///
    /// The room is not governed here.
    pub async fn presence(
        &self,
        room: &str,
        online: &BTreeSet<[u8; 32]>,
    ) -> Result<(Vec<Face>, u32, u32), CollaborationError> {
        let summary = self.room(room).await?.lock().await.summary();
        let (here, here_count) = faces(&summary.members, online);
        let members = summary
            .members
            .iter()
            .filter(|member| !member.removed)
            .count();
        Ok((here, here_count, u32::try_from(members).unwrap_or(u32::MAX)))
    }

    /// The public rooms as the directory lists them, the people of each in
    /// it now by `online` (user roots with an open session here).
    pub async fn directory(&self, online: &BTreeSet<[u8; 32]>) -> Vec<DirectoryEntry> {
        let mut entries = Vec::new();
        for (room, listing) in self.inner.listings.listed() {
            let Ok(governed) = self.room(&room).await else {
                continue;
            };
            let summary = governed.lock().await.summary();
            let (here, here_count) = faces(&summary.members, online);
            let members = summary
                .members
                .iter()
                .filter(|member| !member.removed)
                .count();
            entries.push(DirectoryEntry {
                door: door_path(&room),
                room,
                name: summary.name,
                description: listing.description,
                default_role: listing.default_role,
                here,
                here_count,
                members: u32::try_from(members).unwrap_or(u32::MAX),
                last_active_ms: self.inner.listings.last_active(&summary.room),
                community: false,
            });
        }
        entries
    }

    /// Let the person `join` names into the public room `room`: an
    /// invitation only they can redeem, and the token of a pre-admission at
    /// the room's default role, under the owner's listing. The door answers
    /// them as a [`tilo_relay_protocol::directory::DoorAnswer`].
    ///
    /// # Errors
    ///
    /// The room is not public here, the person is banned from it, or the
    /// invitation could not be issued and kept.
    pub async fn admit_listed(
        &self,
        room: &str,
        join: &DoorJoin,
    ) -> Result<(IssuedInvitation, String), CollaborationError> {
        let listing = self
            .listing(room)
            .ok_or_else(|| CollaborationError::new("that workspace is not public"))?;
        let user = person(&join.user_public_key)?;
        let now = tilo_clock::unix_now_ns();
        let token = hex::encode(rand::random::<[u8; 32]>());
        let mut grant = PreAdmission {
            room: room.to_owned(),
            role: listing.default_role,
            token_sha256: Sha256::digest(token.as_bytes()).into(),
            label: DOOR_LABEL.to_owned(),
            expires_at_ns: now.saturating_add(DOOR_INVITATION_NS),
            owner_user_public_key: [0; 32],
            signature: [0; 64],
        };
        // The door's grant is signed by this host's device key, which the
        // redemption checks against the host and the room's listing
        // ([`Rooms::pre_admission_signer`]).
        grant
            .sign(&self.inner.host.key)
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        let issued = {
            let governed = self.room(room).await?;
            let mut governed = governed.lock().await;
            let policy = governed
                .peer()
                .permissions(self.inner.host.cert.device_public_key, SystemTime::now())
                .map_err(CollaborationError::new)?
                .policy;
            if policy.is_blocked(&hex::encode(user)) {
                return Err(CollaborationError::new(
                    "you were banned from this workspace",
                ));
            }
            let host = self.inner.host.cert.clone();
            let issued = self.stage_issue(
                &mut governed,
                &host,
                IssueInvitationRequest {
                    request_id: rand::random(),
                    role: super::pages::member_role(listing.default_role),
                    recipient_user_public_key: Some(user),
                    invitee_name: tilo_user_identity::display_name(&join.display_name)
                        .ok()
                        .map(str::to_owned),
                    expires_at_ns: grant.expires_at_ns,
                    inviter_name: None,
                },
                now,
            )?;
            governed
                .admissions
                .record(
                    issued.invite_id,
                    grant,
                    now,
                    self.inner.host.max_pre_admissions,
                )
                .map_err(CollaborationError::new)?;
            issued
        };
        self.written(room, true).await?;
        Ok((issued, token))
    }

    /// Whether `grant`, recorded for an invitation to `room`, was signed by
    /// someone who may admit people there: the room's owner (this relay's
    /// person), or this host's own door while the room is public.
    ///
    /// # Errors
    ///
    /// The signature does not verify, or its signer may not admit anyone.
    pub(super) fn pre_admission_signer(
        &self,
        room: &str,
        grant: &PreAdmission,
    ) -> Result<(), String> {
        grant.verify()?;
        let host = &self.inner.host.cert;
        if grant.owner_user_public_key == host.user_public_key {
            return Ok(());
        }
        if grant.owner_user_public_key == host.device_public_key
            && grant.label == DOOR_LABEL
            && self.listing(room).is_some()
        {
            return Ok(());
        }
        Err("the pre-admission is signed by neither the room's owner nor its host's door".into())
    }

    /// Tell the people who run the public room `room` that someone reported
    /// it: a notice its owner and moderators read beside the room's
    /// admissions.
    ///
    /// # Errors
    ///
    /// The room is not public here, the report is not one, or it could not
    /// be kept.
    pub async fn report_listed(
        &self,
        room: &str,
        report: &DoorReport,
    ) -> Result<(), CollaborationError> {
        if self.listing(room).is_none() {
            return Err(CollaborationError::new("that workspace is not public"));
        }
        let reporter = person(&report.reporter_user_public_key)?;
        let reason = report.reason().map_err(CollaborationError::new)?.to_owned();
        let governed = self.room(room).await?;
        let mut governed = governed.lock().await;
        tracing::warn!(%room, reporter = %hex::encode(reporter), %reason, "a public workspace was reported");
        governed
            .admissions
            .notice(RoomNotice::Reported {
                user_public_key: reporter,
                reason,
                at_ns: tilo_clock::unix_now_ns(),
            })
            .map_err(CollaborationError::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_relay_protocol::rooms::RoomRole;

    fn member(key: u8, name: &str, removed: bool) -> tilo_peer_state::GovernedMember {
        tilo_peer_state::GovernedMember {
            user_public_key: [key; 32],
            role: tilo_peer_state::join::MemberRole::Commenter,
            display_name: Some(name.into()),
            active_device_keys: Vec::new(),
            removed,
            administrator: false,
        }
    }

    #[test]
    fn only_members_in_the_room_now_are_its_faces() {
        let members = vec![
            member(1, "Ana", false),
            member(2, "Ben", false),
            member(3, "Gone", true),
        ];
        let online: BTreeSet<_> = [[1; 32], [3; 32], [9; 32]].into_iter().collect();
        let (shown, count) = faces(&members, &online);
        assert_eq!(
            count, 1,
            "a removed member online is not here, nor a stranger"
        );
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].name, "Ana");
        assert_eq!(shown[0].user_public_key, hex::encode([1; 32]));
    }

    #[test]
    fn listings_are_kept_and_each_change_is_announced_once() {
        let dir = tempfile::tempdir().unwrap();
        let listings = Listings::open(dir.path());
        let mut changes = listings.changes.subscribe();
        let listing = Listing {
            description: "Coffee and tea".into(),
            default_role: RoomRole::Commenter,
        };
        listings.set("room-a", Some(listing.clone())).unwrap();
        listings.set("room-a", Some(listing.clone())).unwrap();
        assert_eq!(changes.try_recv().ok().as_deref(), Some("room-a"));
        assert!(
            changes.try_recv().is_err(),
            "the same listing again changes nothing"
        );
        let reopened = Listings::open(dir.path());
        assert_eq!(reopened.get("room-a"), Some(listing));
        listings.set("room-a", None).unwrap();
        assert_eq!(Listings::open(dir.path()).get("room-a"), None);
        assert_eq!(changes.try_recv().ok().as_deref(), Some("room-a"));
    }
}

#[cfg(test)]
mod door_tests {
    use ed25519_dalek::Signer as _;
    use tilo_actions::remote::{
        ActionHost as _, Caller, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
        RemoteActionTarget,
    };
    use tilo_peer_state::join::MemberRole;
    use tilo_relay_protocol::directory::{DoorJoin, Listing};
    use tilo_relay_protocol::rooms::{
        JoinReply, JoinStep, PreAdmission, RoomRole, join_proof_digest,
    };
    use tilo_relay_session::rooms::RemoveMemberRequest;
    use tilo_user_identity::DeviceCert;

    use super::super::actions::{HostRoomActions, PUBLISH, UNPUBLISH};
    use crate::rooms::Rooms;
    use crate::rooms::tests::{governor, guest_device, view_read_relay};

    async fn settled<T: Send + 'static>(pending: Pending<T>) -> T {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        pending.then(move |value| {
            let _ = sender.send(value);
        });
        tilo_test_env::within("the governance answer", receiver)
            .await
            .unwrap()
            .expect("answered")
    }

    fn invocation(room: &str, verb: &str, argument: serde_json::Value) -> InvokeRequest {
        let serde_json::Value::Object(argument) = argument else {
            panic!("arguments are an object");
        };
        InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget::room_governance(room, verb),
            argument: if argument.is_empty() {
                InvokeArgument::None
            } else {
                InvokeArgument::Json(argument)
            },
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        }
    }

    /// The owner invokes the room's governance `verb` with `argument`.
    async fn govern(
        rooms: &Rooms,
        workspace: u64,
        owner: &DeviceCert,
        request: InvokeRequest,
    ) -> serde_json::Value {
        let host = HostRoomActions::new();
        host.attach(rooms.clone());
        let caller = Caller::authenticated(owner.user_public_key, owner.device_public_key);
        let receipt = settled(host.in_workspace(workspace).invoke(&caller, request))
            .await
            .unwrap();
        match receipt.status {
            ReceiptStatus::Completed { output } => output.value.unwrap_or_default(),
            other => panic!("governance completes at once: {other:?}"),
        }
    }

    /// A stranger asks the door of `room` and redeems what it answered, as
    /// a Tilo computer does: the reply, and their certificate.
    async fn walk_in(rooms: &Rooms, room: &str) -> (Result<JoinReply, String>, DeviceCert) {
        let (key, cert) = guest_device();
        let join = DoorJoin {
            user_public_key: hex::encode(cert.user_public_key),
            display_name: "Cy".into(),
        };
        let reply = walk_in_as(rooms, room, &join, &key, &cert).await;
        (reply, cert)
    }

    async fn walk_in_as(
        rooms: &Rooms,
        room: &str,
        join: &DoorJoin,
        key: &ed25519_dalek::SigningKey,
        cert: &DeviceCert,
    ) -> Result<JoinReply, String> {
        let (issued, token) = rooms
            .admit_listed(room, join)
            .await
            .map_err(|error| error.to_string())?;
        Ok(redeem(rooms, issued.signed_invitation, token, key, cert).await)
    }

    /// Redeem `invitation` with the door's `token`, as the person's device.
    async fn redeem(
        rooms: &Rooms,
        invitation: Vec<u8>,
        token: String,
        key: &ed25519_dalek::SigningKey,
        cert: &DeviceCert,
    ) -> JoinReply {
        let JoinReply::Challenge { challenge } = rooms
            .page_join(JoinStep::Ask {
                invitation,
                device_cert: cert.clone(),
                request_id: rand::random(),
            })
            .await
        else {
            panic!("no challenge");
        };
        let signature = key.sign(&join_proof_digest(&challenge)).to_bytes();
        rooms
            .page_join(JoinStep::ProvePreAdmitted {
                challenge,
                signature,
                name: "Cy".into(),
                token: token.into_bytes(),
            })
            .await
    }

    #[tokio::test]
    async fn a_door_grant_with_a_forged_or_zero_signature_admits_nobody() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let relay = view_read_relay(dir.path(), &key, &owner).await;
        let rooms = relay.rooms().unwrap();
        let room = rooms.create("Plaza").await.unwrap().room;
        rooms
            .publish(
                &room,
                &owner,
                Listing {
                    description: String::new(),
                    default_role: RoomRole::Editor,
                },
            )
            .await
            .unwrap();
        let host_device = owner.device_public_key;
        let forgeries: [(&str, Box<dyn FnOnce(&mut PreAdmission) + Send>); 3] = [
            (
                "a zero signature",
                Box::new(|grant: &mut PreAdmission| grant.signature = [0; 64]),
            ),
            (
                "a stranger's key",
                Box::new(|grant: &mut PreAdmission| {
                    grant
                        .sign(&ed25519_dalek::SigningKey::from_bytes(&[77; 32]))
                        .unwrap();
                }),
            ),
            (
                "a stranger's signature in the host's name",
                Box::new(move |grant: &mut PreAdmission| {
                    grant
                        .sign(&ed25519_dalek::SigningKey::from_bytes(&[78; 32]))
                        .unwrap();
                    grant.owner_user_public_key = host_device;
                }),
            ),
        ];
        for (what, forge) in forgeries {
            let (guest_key, guest) = guest_device();
            let join = DoorJoin {
                user_public_key: hex::encode(guest.user_public_key),
                display_name: "Mallory".into(),
            };
            let (issued, token) = rooms.admit_listed(&room, &join).await.unwrap();
            rooms
                .room(&room)
                .await
                .unwrap()
                .lock()
                .await
                .admissions
                .tamper(issued.invite_id, forge);
            let reply = redeem(rooms, issued.signed_invitation, token, &guest_key, &guest).await;
            assert!(
                !matches!(reply, JoinReply::Admitted { .. }),
                "{what} admitted someone: {reply:?}"
            );
            let members = rooms.summary(&room).await.unwrap().members;
            assert!(
                members
                    .iter()
                    .all(|member| member.user_public_key != guest.user_public_key),
                "{what}: not a member"
            );
        }

        // The door's own grant, as the host signed it, admits.
        let (guest_key, guest) = guest_device();
        let join = DoorJoin {
            user_public_key: hex::encode(guest.user_public_key),
            display_name: "Cy".into(),
        };
        assert!(matches!(
            walk_in_as(rooms, &room, &join, &guest_key, &guest).await,
            Ok(JoinReply::Admitted { .. })
        ));
        relay.shutdown().await;
    }

    // USER REQUIREMENT (2026-10-19): "you just set a default role for people,
    // all of this has been solved hundreds of times by hundreds of
    // applications" — the owner makes the workspace public with a default
    // role, and everyone who joins from the directory gets exactly that role,
    // with nobody deciding each join.
    #[tokio::test]
    async fn whoever_joins_a_public_workspace_gets_the_default_role_its_owner_set() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let relay = view_read_relay(dir.path(), &key, &owner).await;
        let rooms = relay.rooms().unwrap();
        let summary = rooms.create("Coffee").await.unwrap();
        let room = summary.room.clone();

        let answer = govern(
            rooms,
            summary.workspace_id,
            &owner,
            invocation(
                &room,
                PUBLISH,
                serde_json::json!({ "default_role": "commenter", "description": "Talk about coffee" }),
            ),
        )
        .await;
        assert_eq!(answer["public"], true);
        assert_eq!(answer["default_role"], "commenter");

        let listed = rooms.directory(&std::collections::BTreeSet::new()).await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Coffee");
        assert_eq!(listed[0].description, "Talk about coffee");
        assert_eq!(listed[0].default_role, RoomRole::Commenter);

        for _ in 0..2 {
            let (reply, cert) = walk_in(rooms, &room).await;
            match reply.unwrap() {
                JoinReply::Admitted { role, .. } => assert_eq!(role, RoomRole::Commenter),
                other => panic!("the door admits at once: {other:?}"),
            }
            let members = rooms.summary(&room).await.unwrap().members;
            let joined = members
                .iter()
                .find(|member| member.user_public_key == cert.user_public_key)
                .expect("a member now");
            assert_eq!(joined.role, MemberRole::Commenter);
        }

        // Another default from now on: the next person gets it.
        rooms
            .publish(
                &room,
                &owner,
                Listing {
                    description: "Talk about coffee".into(),
                    default_role: RoomRole::Viewer,
                },
            )
            .await
            .unwrap();
        let (reply, _) = walk_in(rooms, &room).await;
        assert!(
            matches!(
                reply.unwrap(),
                JoinReply::Admitted {
                    role: RoomRole::Viewer,
                    ..
                }
            ),
            "the default is read at the door, not when the listing was first made"
        );
        relay.shutdown().await;
    }

    #[tokio::test]
    async fn a_private_workspace_has_no_door_and_is_not_listed() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let relay = view_read_relay(dir.path(), &key, &owner).await;
        let rooms = relay.rooms().unwrap();
        let summary = rooms.create("Team").await.unwrap();
        let room = summary.room.clone();
        let (reply, _) = walk_in(rooms, &room).await;
        assert!(reply.is_err(), "never published: no door");

        rooms
            .publish(
                &room,
                &owner,
                Listing {
                    description: String::new(),
                    default_role: RoomRole::Editor,
                },
            )
            .await
            .unwrap();
        assert_eq!(rooms.published_rooms(), vec![room.clone()]);
        let answer = govern(
            rooms,
            summary.workspace_id,
            &owner,
            invocation(&room, UNPUBLISH, serde_json::json!({})),
        )
        .await;
        assert_eq!(answer["public"], false);
        assert!(
            rooms
                .directory(&std::collections::BTreeSet::new())
                .await
                .is_empty()
        );
        let (reply, _) = walk_in(rooms, &room).await;
        assert!(reply.is_err(), "unpublished: the door is shut again");

        // A moderator handed out at the door is refused outright.
        assert!(
            rooms
                .publish(
                    &room,
                    &owner,
                    Listing {
                        description: String::new(),
                        default_role: RoomRole::Moderator,
                    },
                )
                .await
                .is_err()
        );
        relay.shutdown().await;
    }

    #[tokio::test]
    async fn a_banned_person_is_turned_away_at_the_door_and_others_cannot_publish() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let relay = view_read_relay(dir.path(), &key, &owner).await;
        let rooms = relay.rooms().unwrap();
        let room = rooms.create("Plaza").await.unwrap().room;
        rooms
            .publish(
                &room,
                &owner,
                Listing {
                    description: String::new(),
                    default_role: RoomRole::Editor,
                },
            )
            .await
            .unwrap();
        let (guest_key, guest) = guest_device();
        let join = DoorJoin {
            user_public_key: hex::encode(guest.user_public_key),
            display_name: "Troll".into(),
        };
        assert!(matches!(
            walk_in_as(rooms, &room, &join, &guest_key, &guest).await,
            Ok(JoinReply::Admitted { .. })
        ));
        assert!(
            rooms.unpublish(&room, &guest).await.is_err(),
            "an Editor who joined does not change the room's settings"
        );
        rooms
            .ban_member(
                &room,
                &owner,
                RemoveMemberRequest {
                    request_id: [7; 32],
                    user_public_key: guest.user_public_key,
                },
            )
            .await
            .unwrap();
        let again = rooms.admit_listed(&room, &join).await;
        assert!(again.is_err(), "banned: the door refuses them");
        let reported = rooms
            .report_listed(
                &room,
                &tilo_relay_protocol::directory::DoorReport {
                    reporter_user_public_key: hex::encode([5; 32]),
                    reason: "spam in general".into(),
                },
            )
            .await;
        assert!(reported.is_ok(), "{reported:?}");
        relay.shutdown().await;
    }
}
