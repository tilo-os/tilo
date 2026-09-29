//! Memberships a room holds outside its governance.
//!
//! Two kinds of membership are never written into a room's governance, yet
//! every member knows them because each is a signed record that proves
//! itself, whichever transport delivered it:
//!
//! - **Host admissions** (`HostAdmissionV1`): a public room's delegated host
//!   admitted a person who redeemed the room's standing invitation while the
//!   owner was away. It verifies against the owner's delegation and grants
//!   the person a role no higher than Commenter, on every device their root
//!   certifies.
//! - **Recording delegations** (`RecordingDelegationV1`): a person's device
//!   lets their recorder act for them in the room. It verifies against the
//!   device's certificate and lists the recorder as an agent member for as
//!   long as the person keeps a role and the room allows recording delegates.
//!
//! Each member keeps the latest record per person (admissions) or per device
//! and agent (delegations), and decides from them, against the room's
//! current owner and capabilities, which memberships stand.

use std::collections::BTreeMap;

use collab_types::caps::{CapOverride, Principal, Subject, Verb};
use collab_types::{ChannelClass, Ed25519PublicKey, HostAdmissionV1, RecordingDelegationV1};
use serde::{Deserialize, Serialize};
use tilo_user_identity::DeviceCert;

/// Id prefix of a membership a host admission grants (as the desktop model
/// names it, so every host lists it the same way).
pub const DELEGATED_ADMISSION_CAP_PREFIX: &str =
    tilo_desktop_model::capability::DELEGATED_ADMISSION_CAP_PREFIX;

/// Upper bound on the records one room holds of each kind, so a flood of
/// valid-looking records costs a fixed amount.
const MAX_HELD: usize = 4096;

/// The signed memberships a room holds outside its governance.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldRecords {
    /// The latest host admission of each person, by their user root.
    admissions: BTreeMap<[u8; 32], HostAdmissionV1>,
    /// The latest recording delegation of each device and agent, with the
    /// certificate it carried.
    recordings: BTreeMap<([u8; 32], String), (RecordingDelegationV1, DeviceCert)>,
}

/// The id a person's host-admitted membership is listed under.
#[must_use]
pub fn admission_capability_id(member_user: &Ed25519PublicKey) -> String {
    format!(
        "{DELEGATED_ADMISSION_CAP_PREFIX}{}",
        hex::encode(member_user.as_bytes())
    )
}

impl HeldRecords {
    pub fn is_empty(&self) -> bool {
        self.admissions.is_empty() && self.recordings.is_empty()
    }

    /// Keep `admission` for `room` when it is the newest of its person.
    /// Whether it grants anything is decided when memberships are read,
    /// against the room's owner then ([`Self::admitted`]), so an admission
    /// that arrives before the room's governance is not lost.
    ///
    /// # Errors
    ///
    /// When it names another room or its room and workspace disagree.
    pub fn receive_admission(
        &mut self,
        room: &str,
        admission: HostAdmissionV1,
    ) -> Result<bool, String> {
        if admission.room != room {
            return Err("the admission names another room".into());
        }
        if !crate::room_workspace_id_matches(room, admission.workspace) {
            return Err("the admission's workspace is not its room's".into());
        }
        let person = *admission.member_user.as_bytes();
        if self
            .admissions
            .get(&person)
            .is_some_and(|held| held.admitted_at_unix_seconds >= admission.admitted_at_unix_seconds)
        {
            return Ok(false);
        }
        if !self.admissions.contains_key(&person) && self.admissions.len() >= MAX_HELD {
            return Err("the room holds too many admissions".into());
        }
        self.admissions.insert(person, admission);
        Ok(true)
    }

    /// Keep `delegation`, with the certificate `cert` it carried, for
    /// `room` when it verifies and is the newest of its device and agent.
    ///
    /// # Errors
    ///
    /// When it does not verify, names another room, or its certificate is
    /// not the delegating device's or is not current at `now_ns`.
    pub fn receive_recording(
        &mut self,
        room: &str,
        delegation: RecordingDelegationV1,
        cert: DeviceCert,
        now_ns: u64,
    ) -> Result<bool, String> {
        if delegation.room != room {
            return Err("the delegation names another room".into());
        }
        if !crate::room_workspace_id_matches(room, delegation.workspace) {
            return Err("the delegation's workspace is not its room's".into());
        }
        delegation.verify().map_err(|error| error.to_string())?;
        if cert.device_public_key != *delegation.device.as_bytes()
            || cert.user_public_key != *delegation.person.as_bytes()
        {
            return Err("the certificate is for another device or person".into());
        }
        tilo_user_identity::verify_device_cert_at(&cert, now_ns)
            .map_err(|error| error.to_string())?;
        let key = (*delegation.device.as_bytes(), delegation.agent.clone());
        if self.recordings.get(&key).is_some_and(|(held, _)| {
            held.issued_at_unix_seconds >= delegation.issued_at_unix_seconds
        }) {
            return Ok(false);
        }
        if !self.recordings.contains_key(&key) && self.recordings.len() >= MAX_HELD {
            return Err("the room holds too many recording delegations".into());
        }
        self.recordings.insert(key, (delegation, cert));
        Ok(true)
    }

    /// The host admission of the person whose user root is `user`, when one
    /// is held: every device that root certifies is theirs in the room.
    pub fn admission_of_person(&self, user: &[u8; 32]) -> Option<&HostAdmissionV1> {
        self.admissions.get(user)
    }

    /// The memberships host admissions grant in a room of `class` owned by
    /// `owner` (a person id), at `now`: one per admitted person whose
    /// admission verifies against the owner's delegation and who holds no
    /// role through `caps` already. Only a public room admits this way.
    pub fn admitted(
        &self,
        class: ChannelClass,
        owner: Option<&str>,
        caps: &[CapOverride],
        room: &str,
        now: web_time::SystemTime,
    ) -> Vec<CapOverride> {
        if class != ChannelClass::Public {
            return Vec::new();
        }
        let Some(owner_key) = owner
            .and_then(|owner| hex::decode(owner).ok())
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            .map(Ed25519PublicKey::from_bytes)
        else {
            return Vec::new();
        };
        let now_secs = now
            .duration_since(web_time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        self.admissions
            .values()
            .filter_map(|admission| {
                let role = admission.verify(owner_key, now).ok()?;
                let person = Principal::person_from_key(admission.member_user)
                    .id()
                    .to_string();
                if super::role_of(owner, caps, &person, now_secs).is_some() {
                    return None;
                }
                let verb = collab_types::caps::role_caps(role)?;
                Some(
                    CapOverride::membership(Subject::person(person), room.to_string(), verb)
                        .with_id(admission_capability_id(&admission.member_user))
                        .granted_by(Principal::Token(hex::encode(admission.invite))),
                )
            })
            .collect()
    }

    /// The agent memberships recording delegations list in a room owned by
    /// `owner` with capabilities `caps`, at `now`: each active delegation
    /// whose person still holds at least `Read` there, unless the room
    /// forbids recording delegates. `own_device`'s delegations are that
    /// device's own admissions and are not listed twice.
    pub fn recording_delegates(
        &self,
        owner: Option<&str>,
        caps: &[CapOverride],
        forbidden: bool,
        own_device: Option<[u8; 32]>,
        room: &str,
        now_secs: u64,
    ) -> BTreeMap<String, CapOverride> {
        if forbidden {
            return BTreeMap::new();
        }
        self.recordings
            .values()
            .map(|(delegation, _)| delegation)
            .filter(|delegation| {
                delegation.active && Some(*delegation.device.as_bytes()) != own_device
            })
            .filter_map(|delegation| {
                let person = Principal::person_from_key(delegation.person)
                    .id()
                    .to_string();
                let stands = super::role_of(owner, caps, &person, now_secs)
                    .and_then(collab_types::caps::role_caps)
                    .is_some_and(|held| held.implies(Verb::Read));
                if !stands {
                    return None;
                }
                let id = tilo_desktop_model::capability::remote_device_delegate_id(
                    &person,
                    delegation.device.as_bytes(),
                    &delegation.agent,
                );
                let cap = CapOverride::membership(
                    Subject::agent(delegation.agent.clone()),
                    room.to_string(),
                    Verb::Read,
                )
                .with_id(id.clone());
                Some((id, cap))
            })
            .collect()
    }

    /// The recording delegations held, each once, newest per device and
    /// agent: what a host tells a member that joins later.
    pub fn recordings(&self) -> impl Iterator<Item = &(RecordingDelegationV1, DeviceCert)> {
        self.recordings.values()
    }

    /// The host admissions held, each once, newest per person.
    pub fn admissions(&self) -> impl Iterator<Item = &HostAdmissionV1> {
        self.admissions.values()
    }
}
