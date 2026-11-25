//! [`DeviceCert`] — the signed statement binding a device key to a
//! user identity, plus [`DeviceCapabilities`] describing what the
//! device may do on behalf of the user.
//!
//! The signing preimage constant [`DEVCERT_PREIMAGE_TAG`] is frozen
//! wire ABI — see `signing_preimage_is_frozen_abi` in `lib.rs`.
//!
//! # A tab signed in by a device of its person
//!
//! A browser tab that holds no identity is signed in by one of the person's
//! devices: the device signs the tab's [`DeviceCert`] with its own key,
//! carrying its root-signed certificate as the tab's [`DeviceCert::issuer`].
//! [`verify_device_cert`] accepts such a certificate only when it grants at
//! most what a browser is granted
//! ([`crate::CertificationPolicy::browser`]: a member, for 30 days), names the
//! issuer's person, was issued while the issuer's certificate was valid, and
//! the issuer is a device the person's root certified itself. The chain is one
//! step: a certificate a device signed signs nothing.
//!
//! # A person's stand-in approver
//!
//! A person whose root lives in one browser tab only (they set Tilo up
//! through an agent and have not kept their root with a passkey yet) would
//! lose every way to add a device if that tab's storage were cleared. So the
//! tab has the host it uses make a key the host holds — the *stand-in* — and
//! certifies it with the root ([`DeviceCapabilities::stand_in`]).
//!
//! What a stand-in can do: sign a [`DeviceCert`] for a new device of that one
//! person ([`sign_device_cert`] with the stand-in's key), carrying the
//! stand-in's own root-signed certificate as its [`DeviceCert::issuer`].
//! [`verify_device_cert`] accepts such a
//! certificate only when it grants at most what a browser is granted
//! ([`crate::CertificationPolicy::browser`]: a member, for 30 days), names the
//! stand-in's person, was issued while the stand-in's certificate was valid,
//! and the stand-in's certificate is root-signed and has no issuer itself.
//!
//! What it cannot do: anything else. Its certificate grants no capability:
//! it is no device of the person, so [`verify_device_cert`] refuses it as one
//! (only [`verify_stand_in_cert`] takes it); it reaches no workspace, holds
//! no membership, is no host, admits no hosts, reads nothing, carries no
//! agent's grant and administers nothing. It cannot certify another stand-in
//! or anything beyond a member, and it expires within
//! [`STAND_IN_LIFETIME_NS`].
//!
//! How it ends: it expires, or — when the person first keeps their root with
//! a passkey — the root revokes the stand-in's key through the ordinary
//! [`crate::RevocationEntry`]. From the revocation's time on, a certificate it
//! issues is refused ([`crate::RevocationList::refuses`]); every device it
//! certified before stays valid, as a revocation takes effect from its time.

use collab_types::signer::SignError;
use ed25519_dalek::Verifier;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

use crate::canonical::{CanonicalError, signing_digest};
use crate::sign::StatementSigner;

/// Signing-domain tag for every `DeviceCert`: `tilo-devcert-v1` + NUL. The
/// capabilities and, for a certificate an issuer signed, the issuer's whole
/// certificate are part of the signed bytes, so one tag covers every kind.
pub const DEVCERT_PREIMAGE_TAG: &[u8] = b"tilo-devcert-v1\0";

/// The longest a stand-in approver's certificate may last.
pub const STAND_IN_LIFETIME_NS: u64 = 30 * 24 * 60 * 60 * 1_000_000_000;

/// The longest an agent's grant certificate may last. A grant is renewed by
/// the person approving the agent again, so a key that leaks from the
/// service holding it is bounded by this, not by the service.
///
/// A person's server key ([`DeviceCapabilities::server_key`]) is the one
/// exception: a backend that runs unattended would break silently each time
/// its key ran out, so it lasts until the person revokes it, or until an
/// expiry they chose. What bounds a leaked one is revocation, which every
/// host checks at each handshake and which ends every key it minted, and a
/// leak is cheap to find and end: its token carries a prefix secret scanners
/// look for, it is shown once, and the person's keys list shows when it was
/// last used and which device approved it, with a revoke button beside it.
/// The keys a server key mints for one workspace stay within this bound.
pub const MAX_AGENT_GRANT_LIFETIME_NS: u64 = 90 * 24 * 60 * 60 * 1_000_000_000;

/// The longest a placed device's certificate may last
/// ([`DeviceCapabilities::records`]): the host that placed the device
/// certifies it again to keep it, and a certificate that leaked from where
/// the device ran ends soon after.
pub const MAX_PLACED_DEVICE_LIFETIME_NS: u64 = 2 * 60 * 60 * 1_000_000_000;

/// The most records one placed device's certificate names
/// ([`PlacedRecords`]).
pub const MAX_PLACED_RECORDS: usize = 64;

/// First byte of a foreign device's capabilities in binary encodings.
const FOREIGN_BINARY_MARKER: u8 = 2;

/// First byte of capabilities that grant an agent's grant or more than being
/// foreign in binary encodings, followed by one byte of [`EXTENDED_FOREIGN`] |
/// [`EXTENDED_AGENT`] | [`EXTENDED_STAND_IN`] |
/// [`EXTENDED_ISSUED`] | [`EXTENDED_SERVER_KEY`] | [`EXTENDED_BROWSER_PAGES`] |
/// [`EXTENDED_WORKSPACE_SERVICE`], the agent's scope when
/// [`EXTENDED_AGENT`] is set (a
/// tag byte, then a workspace's 16 bytes for one workspace), and then the
/// four flags. A bit this build does not know is refused, except bit 2
/// ([`RETIRED_ADMINISTER`]).
const EXTENDED_BINARY_MARKER: u8 = 3;
const EXTENDED_FOREIGN: u8 = 1;
/// Bit 2 let a device drive the person's other devices. Grants on a device's resource decide that
/// now, so the bit is never written; a certificate an earlier build signed with it still reads, as
/// one without it.
const RETIRED_ADMINISTER: u8 = 2;
const EXTENDED_AGENT: u8 = 4;
const EXTENDED_STAND_IN: u8 = 8;
/// Only inside a certificate: its issuer's certificate follows its
/// signature (see [`DeviceCert::issuer`]).
const EXTENDED_ISSUED: u8 = 16;
/// The agent's grant is a person's server key (see
/// [`DeviceCapabilities::server_key`]).
const EXTENDED_SERVER_KEY: u8 = 32;
/// The agent's grant may operate web pages in Browser (see
/// [`DeviceCapabilities::browser_pages`]).
const EXTENDED_BROWSER_PAGES: u8 = 64;
/// The key is a device's own workspace service (see
/// [`DeviceCapabilities::workspace_service`]).
const EXTENDED_WORKSPACE_SERVICE: u8 = 128;
/// First byte of a device's capabilities in binary encodings when its
/// certificate reaches one workspace alone
/// ([`DeviceCapabilities::workspace`]): the workspace's 16 bytes follow, then
/// the capabilities in any of the forms above. It is what [`BINARY_LAYOUT`] 2
/// added. A build of layout 1 reads this byte as a capability flag, which may
/// only be 0 or 1, so it refuses such a certificate whole rather than reading
/// it as one that reaches every workspace.
const WORKSPACE_BINARY_MARKER: u8 = 4;
/// First byte of a device's capabilities in binary encodings when its
/// certificate reaches the records the host that placed it named, alone
/// ([`DeviceCapabilities::records`]): the workspace's 16 bytes follow, then
/// the number of records as one byte (1 to [`MAX_PLACED_RECORDS`], postcard's
/// length prefix for that many), then each record's 32-byte channel in
/// ascending order, then the capabilities in the extended form (such a
/// certificate always names its issuer). It is what [`BINARY_LAYOUT`] 3 added. A build of layout 2 names
/// this byte a later layout where it asks [`binary_layout_of`], and reads it as
/// a capability flag that is neither 0 nor 1 where it decodes it, so it refuses
/// such a certificate whole rather than reading it as a device of the person.
const PLACED_BINARY_MARKER: u8 = 5;
/// First byte of a placement grant's capabilities in binary encodings
/// ([`DeviceCapabilities::placing`]): the workspace, the count and the
/// channels follow as after [`PLACED_BINARY_MARKER`], then the capabilities in
/// the extended form. [`BINARY_LAYOUT`] 3 added it with the placed marker, and a
/// build of layout 2 refuses it whole the same way.
const PLACING_BINARY_MARKER: u8 = 6;
/// First byte of an agent's grant's capabilities in binary encodings
/// ([`AgentScope::Agent`]): the grant follows (its agent's id, its workspaces and its apps, as
/// postcard writes an [`AgentGrant`]), then the extended form with [`EXTENDED_AGENT`] set and no
/// scope after its byte. It is what [`BINARY_LAYOUT`] 4 added. A build of layout 3 names this
/// byte a later layout where it asks [`binary_layout_of`], and reads it as a capability flag
/// that is neither 0 nor 1 where it decodes it, so it refuses such a certificate whole rather
/// than reading it as anyone's.
const AGENT_BINARY_MARKER: u8 = 7;
const SCOPE_ALL: u8 = 0;
const SCOPE_WORKSPACE: u8 = 1;
const SCOPE_AGENT: u8 = 2;

/// The version of the binary certificate layout this build reads and writes:
/// the fields in [`DeviceCert`]'s order, the capabilities as four flags after
/// an optional [`FOREIGN_BINARY_MARKER`] or [`EXTENDED_BINARY_MARKER`], the
/// whole after [`WORKSPACE_BINARY_MARKER`] and a workspace for a certificate
/// that reaches that workspace alone. Each device's identity files, every
/// host's stores and every signature hold certificates in it, so the layout
/// only changes with a new version: a new first capabilities byte above the
/// markers names it (`persist::DEVICE_CERT`).
///
/// Layout 2 added [`WORKSPACE_BINARY_MARKER`]. Every certificate layout 1
/// wrote reads in layout 2 as the same certificate and writes back to the same
/// bytes, so its signature holds and the files people's devices keep are
/// current as they are. A certificate that reaches one workspace alone exists
/// only in layout 2, and a build of layout 1 refuses it: as newer than it
/// reads, where it asks [`binary_layout_of`], and as a capability flag that is
/// neither 0 nor 1 where it decodes it, so no build can take it for a device
/// that reaches every workspace.
///
/// Layout 3 added [`PLACED_BINARY_MARKER`] and [`PLACING_BINARY_MARKER`] the
/// same way: what layouts 1 and 2 wrote reads in layout 3 as it is, and a placed
/// device's certificate and a placement grant exist only in layout 3, which a
/// build of layout 2 refuses whole.
///
/// Layout 4 added [`AGENT_BINARY_MARKER`]: an agent's grant names its agent by its id, so a
/// workspace records the key as that agent of its person. What layouts 1 to 3 wrote reads in
/// layout 4 as it is (an agent's key layout 3 named by the hash of its id included, as
/// [`AgentScope::AgentHash`]), and a build of layout 3 refuses an agent's grant of layout 4 whole.
pub const BINARY_LAYOUT: u32 = 4;

/// The binary layout version of the certificate encoded in `bytes`: what its
/// first capabilities byte names, which is [`BINARY_LAYOUT`] for every
/// certificate this build reads, those layout 1 wrote included. A byte above
/// the markers this layout knows names a later layout than [`BINARY_LAYOUT`].
///
/// # Errors
///
/// [`CanonicalError::Deserialize`] when `bytes` end before the capabilities.
pub fn binary_layout_of(bytes: &[u8]) -> Result<u32, CanonicalError> {
    let (_, rest) = postcard::take_from_bytes::<([u8; 32], [u8; 32], String)>(bytes)
        .map_err(|error| CanonicalError::Deserialize(error.to_string()))?;
    match rest.first() {
        Some(0..=AGENT_BINARY_MARKER) => Ok(BINARY_LAYOUT),
        Some(_) => Ok(BINARY_LAYOUT.saturating_add(1)),
        None => Err(CanonicalError::Deserialize(
            "a device certificate ends before its capabilities".into(),
        )),
    }
}

/// What the key of an agent's grant reaches: the person approved an agent
/// (through a service that holds the key for it) to act in these of their
/// workspaces, and nowhere else.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AgentScope {
    /// Every workspace of the person, and making new ones.
    AllWorkspaces,
    /// One workspace, by its id: a UUID's 16 bytes.
    Workspace([u8; 16]),
    /// One agent of the person's: the key acts as that agent, by its id, where the grant says.
    /// Whichever device holds such a key, a workspace it enters records it as this agent of its
    /// person, so one agent stays one agent on every device. Its expiry is the certificate's own,
    /// which an agent's grant always has ([`MAX_AGENT_GRANT_LIFETIME_NS`]).
    Agent(AgentGrant),
    /// What [`BINARY_LAYOUT`] 3 named one agent's key by: the hash of its id
    /// ([`AgentScope::agent_of`]), in one workspace. A certificate signed so reads and verifies
    /// as it was signed; no build issues it any more.
    AgentHash {
        /// The workspace's UUID, 16 bytes.
        workspace: [u8; 16],
        /// [`AgentScope::agent_of`] the agent's id.
        agent: [u8; 32],
    },
}

/// The longest agent id an [`AgentGrant`] names.
pub const MAX_AGENT_ID_BYTES: usize = 64;
/// The most workspaces one [`AgentGrant`] reaches.
pub const MAX_AGENT_WORKSPACES: usize = 16;
/// The most apps one [`AgentGrant`] names.
pub const MAX_AGENT_APPS: usize = 64;
/// The longest app id an [`AgentGrant`] names.
pub const MAX_AGENT_APP_BYTES: usize = 128;

/// One agent's grant: which agent of the person the key acts as, and its authority. How the
/// agent is shown (its name and colour) is its record in each workspace, never part of a
/// certificate.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentGrant {
    /// The agent's id, 1..=[`MAX_AGENT_ID_BYTES`] bytes: what a workspace records it under for
    /// its person.
    pub agent: String,
    /// The workspaces it acts in, by their UUIDs' bytes: 1..=[`MAX_AGENT_WORKSPACES`], none
    /// nil, ascending and each once.
    pub workspaces: Vec<[u8; 16]>,
    /// The apps it acts in, by their ids, ascending and each once; empty for every app of
    /// those workspaces.
    pub apps: Vec<String>,
}

impl AgentGrant {
    /// Whether the grant keeps its bounds: a certificate carrying it is signed and read only
    /// when it does.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        !self.agent.is_empty()
            && self.agent.len() <= MAX_AGENT_ID_BYTES
            && !self.workspaces.is_empty()
            && self.workspaces.len() <= MAX_AGENT_WORKSPACES
            && self.workspaces.iter().all(|id| *id != [0; 16])
            && self.workspaces.is_sorted_by(|a, b| a < b)
            && self.apps.len() <= MAX_AGENT_APPS
            && self
                .apps
                .iter()
                .all(|app| !app.is_empty() && app.len() <= MAX_AGENT_APP_BYTES)
            && self.apps.is_sorted_by(|a, b| a < b)
    }

    /// Whether the agent may act in `app`: every app when the grant names none.
    #[must_use]
    pub fn permits_app(&self, app: &str) -> bool {
        self.apps.is_empty() || self.apps.iter().any(|named| named == app)
    }
}

impl AgentScope {
    /// One agent's key in one workspace, in every app there: `agent` is the agent's id.
    #[must_use]
    pub fn agent_in(workspace: [u8; 16], agent: &str) -> Self {
        Self::Agent(AgentGrant {
            agent: agent.to_owned(),
            workspaces: vec![workspace],
            apps: Vec::new(),
        })
    }

    /// What an [`AgentScope::AgentHash`] grant names an agent by: the hash of its id.
    #[must_use]
    pub fn agent_of(agent: &str) -> [u8; 32] {
        *blake3::hash(agent.as_bytes()).as_bytes()
    }

    /// The agent's grant, where the key acts as one agent it names by its id.
    #[must_use]
    pub const fn agent_grant(&self) -> Option<&AgentGrant> {
        match self {
            Self::Agent(grant) => Some(grant),
            Self::AllWorkspaces | Self::Workspace(_) | Self::AgentHash { .. } => None,
        }
    }

    /// Whether the grant was certified for the agent whose id is `agent` alone. A grant that
    /// names no agent is no agent's own key.
    #[must_use]
    pub fn is_agent(&self, agent: &str) -> bool {
        match self {
            Self::Agent(grant) => grant.agent == agent,
            Self::AgentHash { agent: named, .. } => *named == Self::agent_of(agent),
            Self::AllWorkspaces | Self::Workspace(_) => false,
        }
    }

    /// The workspaces the grant reaches, by their ids; `None` for one reaching them all.
    #[must_use]
    pub fn workspaces(&self) -> Option<&[[u8; 16]]> {
        match self {
            Self::AllWorkspaces => None,
            Self::Workspace(id) | Self::AgentHash { workspace: id, .. } => {
                Some(std::slice::from_ref(id))
            }
            Self::Agent(grant) => Some(&grant.workspaces),
        }
    }

    /// The one workspace the grant reaches, by its id; `None` for one reaching them all or
    /// more than one.
    #[must_use]
    pub fn workspace(&self) -> Option<[u8; 16]> {
        match self.workspaces()? {
            [one] => Some(*one),
            _ => None,
        }
    }

    /// The canonical UUID of the one workspace the grant reaches; `None` for one reaching them
    /// all or more than one. Whether a grant reaches a workspace is [`Self::reaches`]'s answer,
    /// never this one's.
    #[must_use]
    pub fn workspace_name(&self) -> Option<String> {
        self.workspace().map(|id| uuid_text(&id))
    }

    /// Whether the grant reaches the workspace whose id is `id`.
    #[must_use]
    pub fn reaches_id(&self, id: &[u8; 16]) -> bool {
        self.workspaces().is_none_or(|ids| ids.contains(id))
    }

    /// The scope as text: `all`, the workspace's canonical UUID, that UUID, `/` and the
    /// agent's hash in hex, or `agent:` and the agent's id.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Self::AllWorkspaces => "all".to_owned(),
            Self::Workspace(id) => uuid_text(id),
            Self::AgentHash { workspace, agent } => {
                format!("{}/{}", uuid_text(workspace), hex::encode(agent))
            }
            Self::Agent(grant) => format!("agent:{}", grant.agent),
        }
    }

    /// The scope a key is asked to reach by text: `all`, or a canonical (lower-case,
    /// hyphenated) UUID other than the nil one, alone or followed by `/` and an agent's hash in
    /// lower-case hex. An agent's grant names its agent by id, which no text here does.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        if text == "all" {
            return Some(Self::AllWorkspaces);
        }
        let Some((workspace, agent)) = text.split_once('/') else {
            return uuid_bytes(text).map(Self::Workspace);
        };
        let lower = agent
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        Some(Self::AgentHash {
            workspace: uuid_bytes(workspace)?,
            agent: lower
                .then(|| hex::decode(agent).ok()?.try_into().ok())
                .flatten()?,
        })
    }

    /// Whether the grant reaches the workspace `workspace` names (its UUID).
    #[must_use]
    pub fn reaches(&self, workspace: &str) -> bool {
        match self.workspaces() {
            None => true,
            Some(ids) => uuid_bytes(workspace).is_some_and(|asked| ids.contains(&asked)),
        }
    }

    /// Whether a grant of `self` reaches no more than `asked` does, so it
    /// may stand for what a person was asked to approve: the same scope, or —
    /// where they were asked to approve all their workspaces — one of them.
    /// A wider grant never stands for a narrower request.
    #[must_use]
    pub fn within(&self, asked: &Self) -> bool {
        match asked {
            Self::AllWorkspaces => true,
            Self::Workspace(id) => self.workspaces() == Some(std::slice::from_ref(id)),
            Self::Agent(_) | Self::AgentHash { .. } => self == asked,
        }
    }

    /// Whether the grant may make workspaces: only one that reaches all of
    /// them, so a workspace it makes is one it reaches.
    #[must_use]
    pub const fn makes_workspaces(&self) -> bool {
        matches!(self, Self::AllWorkspaces)
    }
}

/// `id` as a canonical UUID.
fn uuid_text(id: &[u8; 16]) -> String {
    let group = |range: std::ops::Range<usize>| hex::encode(id.get(range).unwrap_or_default());
    format!(
        "{}-{}-{}-{}-{}",
        group(0..4),
        group(4..6),
        group(6..8),
        group(8..10),
        group(10..16)
    )
}

/// The 16 bytes of a canonical, non-nil UUID.
fn uuid_bytes(text: &str) -> Option<[u8; 16]> {
    let groups: Vec<&str> = text.split('-').collect();
    let lengths = [8, 4, 4, 4, 12];
    if groups.len() != lengths.len()
        || groups.iter().zip(lengths).any(|(group, length)| {
            group.len() != length
                || !group
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    {
        return None;
    }
    let bytes: [u8; 16] = hex::decode(groups.concat()).ok()?.try_into().ok()?;
    (bytes != [0; 16]).then_some(bytes)
}

/// A confined device's workspace as JSON names it: its canonical UUID.
#[derive(Clone, Copy)]
struct WorkspaceOnly([u8; 16]);

impl Serialize for WorkspaceOnly {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&uuid_text(&self.0))
    }
}

impl<'de> Deserialize<'de> for WorkspaceOnly {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        uuid_bytes(&text).map(Self).ok_or_else(|| {
            serde::de::Error::invalid_value(
                serde::de::Unexpected::Str(&text),
                &"a canonical workspace UUID",
            )
        })
    }
}

/// An agent's grant as JSON names it: its workspaces by their canonical UUIDs.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentGrantJson {
    agent: String,
    workspaces: Vec<WorkspaceOnly>,
    apps: Vec<String>,
}

impl Serialize for AgentScope {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Agent(grant) if serializer.is_human_readable() => AgentGrantJson {
                agent: grant.agent.clone(),
                workspaces: grant.workspaces.iter().copied().map(WorkspaceOnly).collect(),
                apps: grant.apps.clone(),
            }
            .serialize(serializer),
            // A binary form carries an agent's grant inside a certificate's capabilities alone,
            // which encode it themselves.
            Self::Agent(_) => Err(serde::ser::Error::custom(
                "an agent's grant is encoded within a certificate's capabilities",
            )),
            _ => serializer.serialize_str(&self.name()),
        }
    }
}

impl<'de> Deserialize<'de> for AgentScope {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Scope;
        impl<'de> serde::de::Visitor<'de> for Scope {
            type Value = AgentScope;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(
                    "`all`, a canonical workspace UUID, one followed by `/` and an agent's hash, \
                     or an agent's grant",
                )
            }

            fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<AgentScope, E> {
                AgentScope::parse(text)
                    .ok_or_else(|| E::invalid_value(serde::de::Unexpected::Str(text), &self))
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<AgentScope, A::Error> {
                let json =
                    AgentGrantJson::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                let grant = AgentGrant {
                    agent: json.agent,
                    workspaces: json.workspaces.into_iter().map(|WorkspaceOnly(id)| id).collect(),
                    apps: json.apps,
                };
                if !grant.is_well_formed() {
                    return Err(serde::de::Error::custom(
                        "an agent's grant names its agent, one to 16 workspaces and at most 64 \
                         apps, each once and in order",
                    ));
                }
                Ok(AgentScope::Agent(grant))
            }
        }
        if deserializer.is_human_readable() {
            deserializer.deserialize_any(Scope)
        } else {
            deserializer.deserialize_str(Scope)
        }
    }
}

/// What a device may do on behalf of its user.
///
/// The flags map directly to enforceable policy checks in
/// `crates/desktop/session/` and `crates/core/fs/`. A user's *primary* device (the one
/// that holds the root private key) gets `can_own_workspaces:
/// true, can_admit_devices: true`, their other devices get
/// `can_own_workspaces: true` but `can_admit_devices: false`. Whether a
/// device holds a workspace's keys or only stores it, and which work it
/// takes, is the workspace's and the device's setting, not the certificate's.
///
/// Capabilities are baked into the cert at issue time — they do not
/// change over the cert's lifetime. To upgrade a device from
/// "member" to "owner", the primary revokes the old cert and issues
/// a new one.
// Each field is an independently-meaningful policy bit, not a set of
// mutually-exclusive states an enum could collapse — and this struct's
// field layout is part of the frozen `DEVCERT_PREIMAGE_TAG` wire ABI
// (it's embedded in `DeviceCertView`), so changing its shape to appease
// the lint would break every certificate already issued in the field.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent policy flags, frozen as part of the DeviceCert wire ABI"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCapabilities {
    /// May create/delete/rename workspaces on the user's directory.
    /// When false, the device can still read + edit content on
    /// workspaces already assigned to it.
    pub can_own_workspaces: bool,

    /// May add a device to the person's fleet by approving the ticket it
    /// brings. Typically only the user's primary device — a phone should not
    /// be able to admit another phone into the fleet.
    pub can_admit_devices: bool,

    /// The device runs a Tilo runtime on an operating system Tilo does not
    /// control (stock macOS, another Linux desktop). Workspace policy caps
    /// the role such a device may act with.
    pub foreign: bool,

    /// The key belongs to an agent's grant, not to a machine of the person:
    /// a service (the person's hosting provider's MCP server, say) holds it
    /// so an agent the person approved acts for them, in the workspaces the
    /// scope names and nowhere else. Such a certificate grants none of the
    /// flags above, expires within [`MAX_AGENT_GRANT_LIFETIME_NS`] unless it
    /// is a server key ([`Self::server_key`]), administers no workspace, and is
    /// revoked like any device.
    pub agent: Option<AgentScope>,

    /// The key is a person's stand-in approver, held by a host for a person
    /// whose root is in one browser tab only: it may sign certificates of
    /// new member devices of that person and nothing else (see the module).
    /// Such a certificate grants none of the flags above and no agent's
    /// grant, always expires (within [`STAND_IN_LIFETIME_NS`]), and is no
    /// device: [`verify_device_cert`] refuses it, [`verify_stand_in_cert`]
    /// takes it.
    pub stand_in: bool,

    /// The agent's grant is a key the person made for their own programs, a
    /// server key: one reaching all their workspaces, one reaching one of
    /// them, or one such a key minted. It acts as the person wherever it
    /// reaches, and lasts until they revoke it, or until an expiry they chose
    /// (see [`MAX_AGENT_GRANT_LIFETIME_NS`]). The grant an agent gets through
    /// a person's consent never is one. The marker is part of the signed
    /// encoding, so a certificate cannot gain or lose it.
    pub server_key: bool,

    /// The key may operate web pages in Browser: navigate, fill, select,
    /// click, press, scroll, point and drag on the pages of its workspaces'
    /// Browser windows without asking the person each time. Submitting a
    /// page, and every other action Browser holds for the person, still asks
    /// them. Only an agent's grant carries it, and it is part of the signed
    /// encoding.
    pub browser_pages: bool,

    /// The key is a device's own workspace service: the workspaces that
    /// device makes are held there, and the service writes them under this
    /// key, never the device's, so the two never number their changes from
    /// one count. The person's device it serves beside issues it
    /// ([`prepare_service_cert`]) and is named as its [`DeviceCert::issuer`];
    /// its capabilities are that device's own, never more, and it lasts no
    /// longer than that device's certificate. Revoking the device ends its
    /// service. It is part of the signed encoding.
    pub workspace_service: bool,

    /// The one workspace this device reaches, by its id (a UUID's 16 bytes),
    /// or `None` for a device that reaches every workspace its person's
    /// grants let it. A workspace's own VM is such a device: one of the
    /// person's, listed with the rest and revoked like them, whose key is
    /// refused in every other workspace and holds no workspace's keys. Such
    /// a certificate grants nothing else but being foreign, is signed by the
    /// person's root itself, and issues nothing. It is part of the signed
    /// encoding ([`WORKSPACE_BINARY_MARKER`]), so a certificate cannot lose it.
    pub workspace: Option<[u8; 16]>,

    /// The records of one workspace this device reaches, and nothing else, or
    /// `None` for a device that is not confined to records. A host that
    /// places a device to do work for a person certifies it with its own
    /// device key for exactly the records the work needs: as one of the
    /// person's devices, or under the person's placement grant for those
    /// records ([`Self::placing`]). The device writes and reads those as the
    /// person, and history names them. It is no member of the workspace, is
    /// listed among no members, and every device that keeps the workspace
    /// confines it by this field alone. Such a certificate grants nothing
    /// else, names the host's certificate as its issuer, always expires
    /// (within [`MAX_PLACED_DEVICE_LIFETIME_NS`] and no later than its
    /// issuer), and issues nothing. It is part of the signed encoding
    /// ([`PLACED_BINARY_MARKER`]), so a certificate cannot lose it.
    pub records: Option<PlacedRecords>,

    /// A placement grant: the person lets the host whose device key this is
    /// place devices for these records of one workspace, though the host is
    /// none of their devices (a hosting provider's server). The host then
    /// signs a placed device's certificate ([`Self::records`]) with its own
    /// key, for records within these, naming this certificate as its issuer.
    /// Such a certificate grants nothing else, is signed by the person's root
    /// or by one of their devices the root signed (ending no later than that
    /// device), is no device of the person ([`verify_device_cert`] refuses it),
    /// and is revoked like any device: from the revocation on, nothing it
    /// placed is admitted. It is part of the signed encoding
    /// ([`PLACING_BINARY_MARKER`]).
    pub placing: Option<PlacedRecords>,

    /// Grants a certificate signed before they were retired may carry, which mean nothing now
    /// (see [`RetiredFlags`]). New certificates set none.
    pub retired: RetiredFlags,
}

/// Two grants certificates carried until they were retired: serving the person always on, and
/// reading every workspace of the person. Neither grants anything now. A certificate signed with
/// one set keeps it, read and written exactly as signed: its signature covers those bytes, and
/// people's devices still hold such certificates, which must keep loading. Nothing outside this
/// module sets one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RetiredFlags {
    act_as_host: bool,
    read_all_workspaces: bool,
}

impl RetiredFlags {
    /// Whether the certificate was signed with either retired grant: one that may carry nothing
    /// else (an agent's grant, a stand-in, a workspace service) is refused with one.
    #[must_use]
    pub const fn any(self) -> bool {
        self.act_as_host || self.read_all_workspaces
    }
}

/// The records of one workspace a placed device reaches
/// ([`DeviceCapabilities::records`]): the workspace by its id, and each record
/// by the channel every protocol of the workspace tells it by. Never empty,
/// sorted and without repeats, so each set of records has one encoding, and
/// at most [`MAX_PLACED_RECORDS`] of them.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PlacedRecords {
    workspace: [u8; 16],
    channels: Vec<[u8; 32]>,
}

impl PlacedRecords {
    /// The records `channels` of the workspace `workspace` (a UUID's 16
    /// bytes), in ascending order and each once.
    ///
    /// # Errors
    ///
    /// [`DeviceCertError::InvalidPlacedDevice`] for the nil workspace, a
    /// channel of all zeroes, no channel, or more than
    /// [`MAX_PLACED_RECORDS`] of them.
    pub fn new(
        workspace: [u8; 16],
        channels: impl IntoIterator<Item = [u8; 32]>,
    ) -> Result<Self, DeviceCertError> {
        let mut channels: Vec<[u8; 32]> = channels.into_iter().collect();
        channels.sort_unstable();
        channels.dedup();
        let records = Self {
            workspace,
            channels,
        };
        if records.is_well_formed() {
            Ok(records)
        } else {
            Err(DeviceCertError::InvalidPlacedDevice)
        }
    }

    /// Whether these name a workspace and between one and
    /// [`MAX_PLACED_RECORDS`] records of it, each once and in order.
    fn is_well_formed(&self) -> bool {
        self.workspace != [0; 16]
            && !self.channels.is_empty()
            && self.channels.len() <= MAX_PLACED_RECORDS
            && self.channels.iter().all(|channel| *channel != [0; 32])
            && self.channels.is_sorted_by(|before, after| before < after)
    }

    /// The workspace's id, a UUID's 16 bytes.
    #[must_use]
    pub const fn workspace(&self) -> &[u8; 16] {
        &self.workspace
    }

    /// The canonical UUID of the workspace the records are in.
    #[must_use]
    pub fn workspace_name(&self) -> String {
        uuid_text(&self.workspace)
    }

    /// Each record's channel, in ascending order.
    #[must_use]
    pub fn channels(&self) -> &[[u8; 32]] {
        &self.channels
    }

    /// Whether the record told by `channel` is one of these.
    #[must_use]
    pub fn reaches(&self, channel: &[u8; 32]) -> bool {
        self.channels.binary_search(channel).is_ok()
    }

    /// Whether these are records of the workspace `granted` names, each one of
    /// those: what a placement grant for `granted` may place a device for.
    #[must_use]
    pub fn within(&self, granted: &Self) -> bool {
        self.workspace == granted.workspace
            && self.channels.iter().all(|channel| granted.reaches(channel))
    }
}

/// A placed device's records as JSON names them: the workspace's canonical
/// UUID and each channel in hex, in the order the binary form keeps.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlacedJson {
    workspace: WorkspaceOnly,
    channels: Vec<String>,
}

impl PlacedJson {
    fn of(records: &PlacedRecords) -> Self {
        Self {
            workspace: WorkspaceOnly(records.workspace),
            channels: records.channels.iter().map(hex::encode).collect(),
        }
    }

    fn records<E: serde::de::Error>(self) -> Result<PlacedRecords, E> {
        let mut channels = Vec::with_capacity(self.channels.len());
        for text in &self.channels {
            let mut channel = [0u8; 32];
            hex::decode_to_slice(text, &mut channel).map_err(|_| {
                E::invalid_value(
                    serde::de::Unexpected::Str(text),
                    &"a record's channel as 64 hex digits",
                )
            })?;
            channels.push(channel);
        }
        let records = PlacedRecords {
            workspace: self.workspace.0,
            channels,
        };
        if !records.is_well_formed() {
            return Err(E::invalid_value(
                serde::de::Unexpected::Seq,
                &"between one and 64 records' channels, none all zeroes, in ascending order",
            ));
        }
        Ok(records)
    }
}

/// The v1 JSON shape plus the optional foreign marker, omitted when false so
/// native capabilities serialise exactly as before.
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors the frozen DeviceCapabilities JSON fields"
)]
#[derive(Serialize, Deserialize)]
#[serde(rename = "DeviceCapabilities", deny_unknown_fields)]
struct CapabilitiesJson {
    can_own_workspaces: bool,
    can_admit_devices: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    foreign: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent: Option<AgentScope>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    stand_in: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    server_key: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    browser_pages: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    workspace_service: bool,
    /// Written only when set: a reader that does not know it refuses the
    /// whole certificate (`deny_unknown_fields`), never reads it as one that
    /// reaches every workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workspace: Option<WorkspaceOnly>,
    /// Written only when set, like `workspace`: a reader that does not know it
    /// refuses the whole certificate, never reads it as a device of the person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    records: Option<PlacedJson>,
    /// Written only when set, like `records`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    placing: Option<PlacedJson>,
    /// Retired ([`RetiredFlags`]): read from certificates signed with them, written only when set.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    can_act_as_host: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    can_read_all_workspaces: bool,
}

impl Serialize for DeviceCapabilities {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            return CapabilitiesJson {
                can_own_workspaces: self.can_own_workspaces,
                can_admit_devices: self.can_admit_devices,
                foreign: self.foreign,
                agent: self.agent.clone(),
                stand_in: self.stand_in,
                server_key: self.server_key,
                browser_pages: self.browser_pages,
                workspace_service: self.workspace_service,
                workspace: self.workspace.map(WorkspaceOnly),
                records: self.records.as_ref().map(PlacedJson::of),
                placing: self.placing.as_ref().map(PlacedJson::of),
                can_act_as_host: self.retired.act_as_host,
                can_read_all_workspaces: self.retired.read_all_workspaces,
            }
            .serialize(serializer);
        }
        serialize_binary_capabilities(self, false, serializer)
    }
}

/// The scope bytes `capabilities`' agent grant writes after the extended marker, and the named
/// grant written after its own marker instead, which nothing else confines besides.
fn binary_agent_scope(
    capabilities: &DeviceCapabilities,
) -> Result<(Vec<u8>, Option<&AgentGrant>), &'static str> {
    // An agent's grant goes first, after its own marker, and its extended form names no scope
    // again.
    let mut named = None;
    let scope: Vec<u8> = match &capabilities.agent {
        None => Vec::new(),
        Some(AgentScope::AllWorkspaces) => vec![SCOPE_ALL],
        Some(AgentScope::Workspace(id)) => [&[SCOPE_WORKSPACE][..], id].concat(),
        Some(AgentScope::AgentHash { workspace, agent }) => {
            [&[SCOPE_AGENT][..], workspace, agent].concat()
        }
        Some(AgentScope::Agent(grant)) => {
            if !grant.is_well_formed() {
                return Err(
                    "an agent's grant names its agent, one to 16 workspaces and at most 64 apps, \
                     each once and in order",
                );
            }
            named = Some(grant);
            Vec::new()
        }
    };
    if named.is_some()
        && (capabilities.workspace.is_some()
            || capabilities.records.is_some()
            || capabilities.placing.is_some())
    {
        return Err("an agent's grant reaches the workspaces it names, and no device's confinement");
    }
    Ok((scope, named))
}

/// The marker and records `capabilities` confine a placed device or a placement grant to, when
/// they are well formed and confine it to nothing else.
fn binary_records(
    capabilities: &DeviceCapabilities,
) -> Result<(u8, Option<&PlacedRecords>), &'static str> {
    let (marker, records) = match (&capabilities.records, &capabilities.placing) {
        (Some(_), Some(_)) => {
            return Err("a certificate is a placed device's or a placement grant, never both");
        }
        (Some(records), None) => (PLACED_BINARY_MARKER, Some(records)),
        (None, Some(granted)) => (PLACING_BINARY_MARKER, Some(granted)),
        (None, None) => (PLACED_BINARY_MARKER, None),
    };
    if records.is_some() && capabilities.workspace.is_some() {
        return Err("a device reaches placed records or one workspace, never both");
    }
    if records.is_some_and(|records| !records.is_well_formed()) {
        return Err("a placed device's records are between one and 64, each once and in order");
    }
    Ok((marker, records))
}

/// The positional binary form of `capabilities`; `issued` marks a
/// certificate whose issuer's certificate follows its signature.
fn serialize_binary_capabilities<S: serde::Serializer>(
    capabilities: &DeviceCapabilities,
    issued: bool,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeTuple;
    // Positional binary formats (postcard) encode each bool as one byte;
    // a foreign device prefixes the four flags with the marker byte, and
    // anything more with the extended marker and its byte. The last two
    // flags are the retired ones, kept where every stored certificate has
    // them.
    let flags = [
        capabilities.can_own_workspaces,
        capabilities.can_admit_devices,
        capabilities.retired.act_as_host,
        capabilities.retired.read_all_workspaces,
    ];
    let (scope, named) = binary_agent_scope(capabilities).map_err(serde::ser::Error::custom)?;
    let bit = |set: bool, bit: u8| if set { bit } else { 0 };
    let extended = bit(capabilities.foreign, EXTENDED_FOREIGN)
        | bit(capabilities.agent.is_some(), EXTENDED_AGENT)
        | bit(capabilities.stand_in, EXTENDED_STAND_IN)
        | bit(issued, EXTENDED_ISSUED)
        | bit(capabilities.server_key, EXTENDED_SERVER_KEY)
        | bit(capabilities.browser_pages, EXTENDED_BROWSER_PAGES)
        | bit(capabilities.workspace_service, EXTENDED_WORKSPACE_SERVICE);
    // A placed device and a placement grant always take the extended form,
    // which is the one form their marker is followed by.
    let extended_form = extended & !EXTENDED_FOREIGN != 0
        || capabilities.records.is_some()
        || capabilities.placing.is_some();
    let len = if extended_form {
        scope.len().saturating_add(6)
    } else if capabilities.foreign {
        5
    } else {
        4
    };
    let workspace = capabilities.workspace;
    let (marker, records) = binary_records(capabilities).map_err(serde::ser::Error::custom)?;
    let len = len.saturating_add(if workspace.is_some() { 17 } else { 0 });
    let len = len.saturating_add(if named.is_some() { 2 } else { 0 });
    let len = len.saturating_add(records.map_or(0, |records| {
        18_usize.saturating_add(records.channels.len().saturating_mul(32))
    }));
    let mut tuple = serializer.serialize_tuple(len)?;
    if let Some(records) = records {
        tuple.serialize_element(&marker)?;
        for byte in &records.workspace {
            tuple.serialize_element(byte)?;
        }
        let count = u8::try_from(records.channels.len())
            .map_err(|_| serde::ser::Error::custom("more records than a certificate names"))?;
        tuple.serialize_element(&count)?;
        for byte in records.channels.iter().flatten() {
            tuple.serialize_element(byte)?;
        }
    }
    if let Some(grant) = named {
        tuple.serialize_element(&AGENT_BINARY_MARKER)?;
        tuple.serialize_element(grant)?;
    }
    if let Some(id) = &workspace {
        tuple.serialize_element(&WORKSPACE_BINARY_MARKER)?;
        for byte in id {
            tuple.serialize_element(byte)?;
        }
    }
    if extended_form {
        tuple.serialize_element(&EXTENDED_BINARY_MARKER)?;
        tuple.serialize_element(&extended)?;
        for byte in &scope {
            tuple.serialize_element(byte)?;
        }
    } else if capabilities.foreign {
        tuple.serialize_element(&FOREIGN_BINARY_MARKER)?;
    }
    for flag in flags {
        tuple.serialize_element(&flag)?;
    }
    tuple.end()
}

impl<'de> Deserialize<'de> for DeviceCapabilities {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if deserializer.is_human_readable() {
            let json = CapabilitiesJson::deserialize(deserializer)?;
            return Ok(Self {
                can_own_workspaces: json.can_own_workspaces,
                can_admit_devices: json.can_admit_devices,
                foreign: json.foreign,
                agent: json.agent,
                stand_in: json.stand_in,
                server_key: json.server_key,
                browser_pages: json.browser_pages,
                workspace_service: json.workspace_service,
                workspace: json.workspace.map(|WorkspaceOnly(id)| id),
                records: json.records.map(PlacedJson::records).transpose()?,
                placing: json.placing.map(PlacedJson::records).transpose()?,
                retired: RetiredFlags {
                    act_as_host: json.can_act_as_host,
                    read_all_workspaces: json.can_read_all_workspaces,
                },
            });
        }
        let BinaryCapabilitiesOf(capabilities, issued) =
            BinaryCapabilitiesOf::deserialize(deserializer)?;
        if issued {
            // Only a certificate has an issuer to follow.
            return Err(serde::de::Error::custom(
                "capabilities outside a certificate name no issuer",
            ));
        }
        Ok(capabilities)
    }
}

/// Capabilities as a certificate's binary form carries them: with whether an
/// issuer's certificate follows the certificate's signature.
struct BinaryCapabilitiesOf(DeviceCapabilities, bool);

impl<'de> Deserialize<'de> for BinaryCapabilitiesOf {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Postcard reads only the elements the visitor asks for, so the upper
        // bound admits the four native flags, marker + four flags, and the
        // extended marker + its byte + an agent's scope (up to 49 bytes) +
        // four flags, each after the workspace marker and a workspace's 16
        // bytes, or after the placed marker, a workspace's 16 bytes, a count
        // and its channels, read as one element of their own
        // ([`PlacedChannels`]).
        deserializer.deserialize_tuple(MAX_BINARY_CAPABILITIES, BinaryCapabilities)
    }
}

struct BinaryCapabilities;

/// The most elements a certificate's binary capabilities are read as: a placed
/// device's marker, workspace, count and its channels as one element, then the
/// extended form with an agent's scope and four flags. A reader that bounds
/// what it decodes is told no more than that, whatever follows.
const MAX_BINARY_CAPABILITIES: usize = 19 + 2 + 2 + 49 + 4;

/// A placed device's channels in a certificate's binary form: `self.0` of them,
/// 32 bytes each, read as one element whose length the count before it gives.
#[derive(Clone, Copy)]
struct PlacedChannels(usize);

impl<'de> serde::de::DeserializeSeed<'de> for PlacedChannels {
    type Value = Vec<[u8; 32]>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_tuple(self.0.saturating_mul(32), self)
    }
}

impl<'de> serde::de::Visitor<'de> for PlacedChannels {
    type Value = Vec<[u8; 32]>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} channels of 32 bytes", self.0)
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error;
        let mut channels = Vec::with_capacity(self.0);
        for index in 0..self.0 {
            let mut channel = [0u8; 32];
            for (at, byte) in channel.iter_mut().enumerate() {
                *byte = seq.next_element::<u8>()?.ok_or_else(|| {
                    A::Error::invalid_length(index.saturating_mul(32).saturating_add(at), &self)
                })?;
            }
            channels.push(channel);
        }
        Ok(channels)
    }
}

/// The next byte of `seq`, counted in `read` for the error a short sequence names.
fn capability_byte<'de, A: serde::de::SeqAccess<'de>>(
    seq: &mut A,
    read: &mut usize,
) -> Result<u8, A::Error> {
    use serde::de::Error;
    let index = *read;
    *read = read.saturating_add(1);
    seq.next_element::<u8>()?
        .ok_or_else(|| A::Error::invalid_length(index, &"four capability flags"))
}

/// The records a placed device's or a placement grant's marker is followed by: the workspace, the
/// count, then the channels, well formed.
fn placed_records<'de, A: serde::de::SeqAccess<'de>>(
    seq: &mut A,
    read: &mut usize,
) -> Result<PlacedRecords, A::Error> {
    use serde::de::Error;
    let workspace = workspace_id::<A::Error>(&mut || capability_byte(seq, read))?;
    let count = usize::from(capability_byte(seq, read)?);
    if count == 0 || count > MAX_PLACED_RECORDS {
        return Err(A::Error::invalid_length(
            count,
            &"between one and 64 records of a placed device",
        ));
    }
    let channels = seq
        .next_element_seed(PlacedChannels(count))?
        .ok_or_else(|| A::Error::invalid_length(*read, &"a placed device's channels"))?;
    *read = read.saturating_add(1);
    let records = PlacedRecords {
        workspace,
        channels,
    };
    if !records.is_well_formed() {
        return Err(A::Error::invalid_value(
            serde::de::Unexpected::Bytes(&workspace),
            &"records' channels, none all zeroes, each once and in ascending order",
        ));
    }
    Ok(records)
}

/// Reads a workspace's 16 bytes, refusing the nil UUID: no workspace has it,
/// so a certificate naming it would name nothing.
fn workspace_id<E: serde::de::Error>(
    next: &mut impl FnMut() -> Result<u8, E>,
) -> Result<[u8; 16], E> {
    let mut id = [0u8; 16];
    for byte in &mut id {
        *byte = next()?;
    }
    if id == [0; 16] {
        return Err(E::invalid_value(
            serde::de::Unexpected::Bytes(&id),
            &"a workspace id other than the nil UUID",
        ));
    }
    Ok(id)
}

/// Reads an agent grant's scope: its tag, then the workspace and agent it
/// names.
fn agent_scope<E: serde::de::Error>(
    next: &mut impl FnMut() -> Result<u8, E>,
) -> Result<AgentScope, E> {
    match next()? {
        SCOPE_ALL => Ok(AgentScope::AllWorkspaces),
        SCOPE_WORKSPACE => Ok(AgentScope::Workspace(workspace_id(next)?)),
        SCOPE_AGENT => {
            let workspace = workspace_id(next)?;
            let mut agent = [0u8; 32];
            for byte in &mut agent {
                *byte = next()?;
            }
            Ok(AgentScope::AgentHash { workspace, agent })
        }
        other => Err(E::invalid_value(
            serde::de::Unexpected::Unsigned(other.into()),
            &"an agent scope of all workspaces, one, or one agent in one",
        )),
    }
}

/// The named agent grant an agent's marker is followed by, well formed, and the extended marker
/// that must follow it: nothing confines an agent's grant besides.
fn named_agent_grant<'de, A: serde::de::SeqAccess<'de>>(
    seq: &mut A,
    read: &mut usize,
) -> Result<AgentGrant, A::Error> {
    use serde::de::Error;
    let grant: AgentGrant = seq
        .next_element()?
        .ok_or_else(|| A::Error::invalid_length(*read, &"an agent's grant"))?;
    *read = read.saturating_add(1);
    if !grant.is_well_formed() {
        return Err(A::Error::custom(
            "an agent's grant names its agent, one to 16 workspaces and at most 64 apps, \
             each once and in order",
        ));
    }
    let first = capability_byte(seq, read)?;
    if first != EXTENDED_BINARY_MARKER {
        return Err(A::Error::invalid_value(
            serde::de::Unexpected::Unsigned(first.into()),
            &"the extended capabilities of an agent's grant",
        ));
    }
    Ok(grant)
}

/// The extended capability bits after the extended marker, and the agent scope they grant: the
/// `named` grant read before them, or the scope that follows them. `placed` marks a placed
/// device's or a placement grant's, which take the extended form with no bit of their own.
fn extended_capabilities<E: serde::de::Error>(
    next: &mut impl FnMut() -> Result<u8, E>,
    named: Option<AgentGrant>,
    placed: bool,
) -> Result<(u8, Option<AgentScope>), E> {
    let written = next()?;
    let extended = written & !RETIRED_ADMINISTER;
    // Only an agent's grant, a stand-in, an issuer, a server key,
    // Browser pages and a workspace service need the extended form; anything else has exactly
    // one encoding already.
    let known = EXTENDED_FOREIGN
        | EXTENDED_AGENT
        | EXTENDED_STAND_IN
        | EXTENDED_ISSUED
        | EXTENDED_SERVER_KEY
        | EXTENDED_BROWSER_PAGES
        | EXTENDED_WORKSPACE_SERVICE;
    if extended & !known != 0
        || (written == extended && extended & !EXTENDED_FOREIGN == 0 && !placed)
    {
        return Err(E::invalid_value(
            serde::de::Unexpected::Unsigned(extended.into()),
            &"extended capabilities that grant an agent's grant or a stand-in, \
              or name an issuer",
        ));
    }
    let agent = match (extended & EXTENDED_AGENT == 0, named) {
        (true, None) => None,
        (true, Some(_)) => {
            return Err(E::invalid_value(
                serde::de::Unexpected::Unsigned(extended.into()),
                &"the extended capabilities of an agent's grant, which grant it",
            ));
        }
        (false, Some(grant)) => Some(AgentScope::Agent(grant)),
        (false, None) => Some(agent_scope::<E>(next)?),
    };
    Ok((extended, agent))
}

impl<'de> serde::de::Visitor<'de> for BinaryCapabilities {
    type Value = BinaryCapabilitiesOf;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .write_str("four capability flags, optionally after the foreign or extended marker")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error;
        // Each element is one byte but a placed device's channels; `read`
        // counts them for the error a short sequence names.
        let mut read = 0usize;
        let mut first = capability_byte(&mut seq, &mut read)?;
        let grant = first == PLACING_BINARY_MARKER;
        let records = if first == PLACED_BINARY_MARKER || grant {
            let records = placed_records(&mut seq, &mut read)?;
            first = capability_byte(&mut seq, &mut read)?;
            // The extended form follows, and nothing confines it twice.
            if first != EXTENDED_BINARY_MARKER {
                return Err(A::Error::invalid_value(
                    serde::de::Unexpected::Unsigned(first.into()),
                    &"the extended capabilities of a placed device or a placement grant",
                ));
            }
            Some(records)
        } else {
            None
        };
        let (records, placing) = if grant {
            (None, records)
        } else {
            (records, None)
        };
        let named = if first == AGENT_BINARY_MARKER {
            let grant = named_agent_grant(&mut seq, &mut read)?;
            first = EXTENDED_BINARY_MARKER;
            Some(grant)
        } else {
            None
        };
        let mut next = || capability_byte(&mut seq, &mut read);
        let workspace = if first == WORKSPACE_BINARY_MARKER {
            let id = workspace_id::<A::Error>(&mut next)?;
            first = next()?;
            if first == WORKSPACE_BINARY_MARKER {
                return Err(A::Error::invalid_value(
                    serde::de::Unexpected::Unsigned(first.into()),
                    &"one workspace a device reaches",
                ));
            }
            Some(id)
        } else {
            None
        };
        let (extended, agent, flags) = match first {
            FOREIGN_BINARY_MARKER => (EXTENDED_FOREIGN, None, [next()?, next()?, next()?, next()?]),
            EXTENDED_BINARY_MARKER => {
                let placed = records.is_some() || placing.is_some();
                let (extended, agent) = extended_capabilities(&mut next, named, placed)?;
                (extended, agent, [next()?, next()?, next()?, next()?])
            }
            _ => (0, None, [first, next()?, next()?, next()?]),
        };
        let flag = |byte: u8| match byte {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(A::Error::invalid_value(
                serde::de::Unexpected::Unsigned(other.into()),
                &"a capability flag byte of 0 or 1",
            )),
        };
        Ok(BinaryCapabilitiesOf(
            DeviceCapabilities {
                can_own_workspaces: flag(flags[0])?,
                can_admit_devices: flag(flags[1])?,
                foreign: extended & EXTENDED_FOREIGN != 0,
                agent,
                stand_in: extended & EXTENDED_STAND_IN != 0,
                server_key: extended & EXTENDED_SERVER_KEY != 0,
                browser_pages: extended & EXTENDED_BROWSER_PAGES != 0,
                workspace_service: extended & EXTENDED_WORKSPACE_SERVICE != 0,
                workspace,
                records,
                placing,
                retired: RetiredFlags {
                    act_as_host: flag(flags[2])?,
                    read_all_workspaces: flag(flags[3])?,
                },
            },
            extended & EXTENDED_ISSUED != 0,
        ))
    }
}

impl DeviceCapabilities {
    /// Capabilities that grant nothing: what an agent device's certificate
    /// carries, and what the named sets below add to.
    #[must_use]
    pub const fn nothing() -> Self {
        Self {
            can_own_workspaces: false,
            can_admit_devices: false,
            foreign: false,
            agent: None,
            stand_in: false,
            server_key: false,
            browser_pages: false,
            workspace_service: false,
            workspace: None,
            records: None,
            placing: None,
            retired: RetiredFlags {
                act_as_host: false,
                read_all_workspaces: false,
            },
        }
    }

    /// The capabilities of a device that reaches `workspace` (its id, a
    /// UUID's 16 bytes) alone: a workspace's own VM (see [`Self::workspace`]).
    #[must_use]
    pub fn workspace_device(workspace: [u8; 16]) -> Self {
        Self {
            workspace: Some(workspace),
            ..Self::nothing()
        }
    }

    /// The capabilities of a device placed for `records` alone (see
    /// [`Self::records`]).
    #[must_use]
    pub fn placed(records: PlacedRecords) -> Self {
        Self {
            records: Some(records),
            ..Self::nothing()
        }
    }

    /// The capabilities of a placement grant for `records` (see
    /// [`Self::placing`]).
    #[must_use]
    pub fn placing(records: PlacedRecords) -> Self {
        Self {
            placing: Some(records),
            ..Self::nothing()
        }
    }

    /// Whether a device with these capabilities reaches the workspace
    /// `workspace` names (its canonical UUID): every one, unless it is
    /// confined to one ([`Self::workspace`]) or to records of one
    /// ([`Self::records`]); then that one alone. A name that is no canonical
    /// UUID is no workspace a confined device reaches.
    #[must_use]
    pub fn reaches_workspace(&self, workspace: &str) -> bool {
        match self
            .workspace
            .or_else(|| self.records.as_ref().map(|records| records.workspace))
        {
            None => true,
            Some(id) => uuid_bytes(workspace).is_some_and(|named| named == id),
        }
    }

    /// The canonical UUID of the one workspace a confined device reaches
    /// ([`Self::workspace`], or the workspace of its placed records,
    /// [`Self::records`]); `None` for one that reaches every workspace.
    #[must_use]
    pub fn workspace_name(&self) -> Option<String> {
        self.workspace
            .or_else(|| self.records.as_ref().map(|records| records.workspace))
            .as_ref()
            .map(uuid_text)
    }

    /// The records a placed device reaches in the workspace `workspace` names
    /// (its canonical UUID; [`Self::records`]): `None` for a device that is
    /// not placed, or was placed in another workspace.
    #[must_use]
    pub fn placed_in(&self, workspace: &str) -> Option<&PlacedRecords> {
        self.records
            .as_ref()
            .filter(|records| uuid_bytes(workspace).is_some_and(|named| named == records.workspace))
    }

    /// Whether these capabilities grant nothing `held` does not: a renewal
    /// may narrow what a device may do, never widen it. A certificate that
    /// stops being foreign may not become so, and vice versa.
    #[must_use]
    pub fn grants_no_more_than(&self, held: &Self) -> bool {
        let within = |renewed: bool, held: bool| !renewed || held;
        within(self.can_own_workspaces, held.can_own_workspaces)
            && within(self.can_admit_devices, held.can_admit_devices)
            && (self.foreign || !held.foreign)
            && self.agent == held.agent
            && self.stand_in == held.stand_in
            && self.server_key == held.server_key
            && within(self.browser_pages, held.browser_pages)
            && self.workspace_service == held.workspace_service
            && self.workspace == held.workspace
            && self.records == held.records
            && self.placing == held.placing
    }

    /// The capabilities of an agent's grant reaching `scope`: nothing but
    /// the scope (see [`Self::agent`]).
    #[must_use]
    pub fn agent_grant(scope: AgentScope) -> Self {
        Self {
            agent: Some(scope),
            ..Self::nothing()
        }
    }

    /// The capabilities of a person's server key: an agent's grant reaching
    /// all their workspaces that lasts until revoked (see
    /// [`Self::server_key`]).
    #[must_use]
    pub fn server_key_grant() -> Self {
        Self::own_key_grant(AgentScope::AllWorkspaces)
    }

    /// The capabilities of a key a person made for their own programs,
    /// reaching `scope`: a server key ([`Self::server_key`]), which acts as
    /// the person there.
    #[must_use]
    pub fn own_key_grant(scope: AgentScope) -> Self {
        Self {
            server_key: true,
            ..Self::agent_grant(scope)
        }
    }

    /// The same capabilities, operating web pages in Browser where `allowed`
    /// (see [`Self::browser_pages`]): what a person ticking the box on a key
    /// gives it, and what a key hands the keys it mints.
    #[must_use]
    pub const fn with_browser_pages(mut self, allowed: bool) -> Self {
        self.browser_pages = allowed;
        self
    }

    /// Whether these are a key the person made for their own programs
    /// ([`Self::server_key`]) that reaches the workspace `workspace` (its
    /// UUID, or `all` for the account): where it acts as the person.
    #[must_use]
    pub fn own_key_in(&self, workspace: &str) -> bool {
        self.server_key && self.agent.as_ref().is_some_and(|scope| scope.reaches(workspace))
    }

    /// Whether these are an agent's grant's capabilities that hold: the
    /// scope, and no other flag but being a server key. `true` for every
    /// certificate that is neither a grant's nor a server key's.
    #[must_use]
    pub const fn agent_grant_holds(&self) -> bool {
        if self.server_key && self.agent.is_none() {
            return false;
        }
        self.agent.is_none()
            || !(self.can_own_workspaces
                || self.can_admit_devices
                || self.foreign
                || self.stand_in
                || self.workspace_service
                || self.workspace.is_some()
                || self.records.is_some()
                || self.placing.is_some()
                || self.retired.any())
    }

    /// The capabilities of the workspace service of a device with `device`'s
    /// capabilities: it owns workspaces when the device does, is foreign when
    /// it is, and may do nothing else (see [`Self::workspace_service`]).
    #[must_use]
    pub fn service_of(device: &Self) -> Self {
        Self {
            can_own_workspaces: device.can_own_workspaces,
            foreign: device.foreign,
            workspace_service: true,
            ..Self::nothing()
        }
    }

    /// The capabilities of a person's stand-in approver: nothing but being
    /// one (see [`Self::stand_in`]).
    #[must_use]
    pub fn stand_in_approver() -> Self {
        Self {
            stand_in: true,
            ..Self::nothing()
        }
    }

    /// A device of the person on a Tilo-controlled system: it owns
    /// workspaces and is not foreign. The person's devices of this kind
    /// find, dial and trust each other.
    #[must_use]
    pub const fn is_personal_device(&self) -> bool {
        self.can_own_workspaces && !self.foreign
    }

    /// Whether a workspace hosted by a device with these capabilities has
    /// that host keep the workspace's keys, wherever it runs (a person's
    /// device or a server): the workspace's records are then sealed, and each
    /// member opens them itself. The workspace host and every member decide
    /// this the same way.
    #[must_use]
    pub const fn holds_workspace_keys(&self) -> bool {
        self.can_own_workspaces
    }

    /// Capability set for the user's primary device (holds root
    /// private key, admits new devices).
    pub fn primary() -> Self {
        Self {
            can_own_workspaces: true,
            can_admit_devices: true,
            ..Self::nothing()
        }
    }

    /// Capability set for a secondary device — a phone, tablet, or
    /// laptop the user opts into the fleet but does not want to be
    /// able to admit new devices.
    pub fn member() -> Self {
        Self {
            can_own_workspaces: true,
            ..Self::nothing()
        }
    }
}

/// The name a certificate carries for a machine the person never named: the
/// one that set the machine up owns it, and every other account on it
/// shares it.
pub const UNNAMED_PRIMARY_DEVICE: &str = "Primary device";

/// The name a certificate carries for a machine another account set up
/// ([`UNNAMED_PRIMARY_DEVICE`]'s counterpart).
pub const UNNAMED_SHARED_DEVICE: &str = "Shared device";

/// What the person calls a device whose certificate names it `label`:
/// `None` when they never named it, since provisioning fills in a stand-in
/// that is no name of theirs.
#[must_use]
pub fn device_name(label: &str) -> Option<&str> {
    let label = label.trim();
    (!label.is_empty() && label != UNNAMED_PRIMARY_DEVICE && label != UNNAMED_SHARED_DEVICE)
        .then_some(label)
}

/// Signed statement: "the user identity with public key
/// `user_public_key` recognises `device_public_key` as one of its
/// devices, with the given `capabilities`."
///
/// The signature is Ed25519, produced by the *user root* signing
/// key over `blake3(DEVCERT_PREIMAGE_TAG || postcard(view))`, where
/// `view` is the `DeviceCertView` omitting the `signature` field (see
/// [`device_cert_signing_digest`]). A certificate a person's stand-in issued is signed by the stand-in's key
/// instead, and carries the stand-in's certificate as its [`Self::issuer`].
///
/// Cert chain: for anything the device does to be trusted by a
/// peer, that peer must (a) verify this signature with
/// [`verify_device_cert`], (b) check the cert has not expired, and (c)
/// check the [`crate::RevocationList`] does not refuse it
/// ([`crate::RevocationList::refuses`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCert {
    /// User root public key that owns / signs this cert. 32 bytes.
    pub user_public_key: [u8; 32],

    /// Certified device's public key. 32 bytes.
    pub device_public_key: [u8; 32],

    /// Human-readable label for the device — "Framework 16",
    /// "Studio phone", "Home server". Not authoritative; a peer's
    /// UI may display this label as-is.
    pub device_label: String,

    /// What this device may do on behalf of the user. See
    /// [`DeviceCapabilities`].
    pub capabilities: DeviceCapabilities,

    /// Nanoseconds since Unix epoch — when the cert was signed.
    /// Peers use it for tie-breaks (a later-issued cert supersedes
    /// an earlier one for the same device) and for display.
    pub issued_at_ns: u64,

    /// Optional expiry. `None` means the cert is valid until
    /// revoked. Practical presets: forever for a laptop, 30 days
    /// for a temporary shared device, 24 hours for a bring-your-
    /// own-lab-machine.
    pub expires_at_ns: Option<u64>,

    /// Ed25519 signature. 64 bytes. Covers everything else in this
    /// struct via `blake3(DEVCERT_PREIMAGE_TAG || postcard(view))`.
    pub signature: [u8; 64],

    /// The root-signed certificate that signed this one, or `None` for one
    /// the root signed itself: a device of the person signing a browser tab
    /// in (see the module), a person's stand-in approver, or an
    /// all-workspaces agent's grant that minted a grant for one workspace
    /// ([`prepare_minted_grant`]). A placed device's issuer is the host that
    /// placed it: one of the person's devices, or the person's placement grant
    /// to that host, which may itself name the device of theirs that signed it.
    /// Encoded only when present; its signed bytes include the issuer's whole
    /// certificate.
    pub issuer: Option<Box<DeviceCert>>,
}

/// The JSON shape: the v1 fields plus the issuer, omitted when absent.
#[derive(Serialize)]
#[serde(rename = "DeviceCert")]
struct DeviceCertJsonOut<'a> {
    user_public_key: &'a [u8; 32],
    device_public_key: &'a [u8; 32],
    device_label: &'a str,
    capabilities: &'a DeviceCapabilities,
    issued_at_ns: u64,
    expires_at_ns: Option<u64>,
    signature: &'a serde_bytes::Bytes,
    #[serde(skip_serializing_if = "Option::is_none")]
    issuer: Option<&'a DeviceCert>,
}

#[derive(Deserialize)]
#[serde(rename = "DeviceCert", deny_unknown_fields)]
struct DeviceCertJsonIn {
    user_public_key: [u8; 32],
    device_public_key: [u8; 32],
    device_label: String,
    capabilities: DeviceCapabilities,
    issued_at_ns: u64,
    expires_at_ns: Option<u64>,
    #[serde(with = "serde_bytes")]
    signature: [u8; 64],
    #[serde(default)]
    issuer: Option<Box<DeviceCert>>,
}

const DEVICE_CERT_FIELDS: &[&str] = &[
    "user_public_key",
    "device_public_key",
    "device_label",
    "capabilities",
    "issued_at_ns",
    "expires_at_ns",
    "signature",
    "issuer",
];

/// A certificate's capabilities in its binary form, with the bit that says
/// an issuer's certificate follows the signature.
struct CapabilitiesInCert<'a>(&'a DeviceCapabilities, bool);

impl Serialize for CapabilitiesInCert<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serialize_binary_capabilities(self.0, self.1, serializer)
    }
}

impl Serialize for DeviceCert {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        if serializer.is_human_readable() {
            return DeviceCertJsonOut {
                user_public_key: &self.user_public_key,
                device_public_key: &self.device_public_key,
                device_label: &self.device_label,
                capabilities: &self.capabilities,
                issued_at_ns: self.issued_at_ns,
                expires_at_ns: self.expires_at_ns,
                signature: serde_bytes::Bytes::new(&self.signature),
                issuer: self.issuer.as_deref(),
            }
            .serialize(serializer);
        }
        // Positional binary formats: the seven fields as the derived form
        // wrote them, and the issuer after the signature only when the
        // capabilities' marker says one follows.
        let issued = self.issuer.is_some();
        let mut fields = serializer.serialize_struct("DeviceCert", if issued { 8 } else { 7 })?;
        fields.serialize_field("user_public_key", &self.user_public_key)?;
        fields.serialize_field("device_public_key", &self.device_public_key)?;
        fields.serialize_field("device_label", &self.device_label)?;
        fields.serialize_field(
            "capabilities",
            &CapabilitiesInCert(&self.capabilities, issued),
        )?;
        fields.serialize_field("issued_at_ns", &self.issued_at_ns)?;
        fields.serialize_field("expires_at_ns", &self.expires_at_ns)?;
        fields.serialize_field("signature", serde_bytes::Bytes::new(&self.signature))?;
        if let Some(issuer) = &self.issuer {
            fields.serialize_field("issuer", issuer)?;
        } else {
            fields.skip_field("issuer")?;
        }
        fields.end()
    }
}

impl<'de> Deserialize<'de> for DeviceCert {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if deserializer.is_human_readable() {
            let json = DeviceCertJsonIn::deserialize(deserializer)?;
            // One step, but for a placed device under a grant one of the person's devices signed:
            // its issuer is that grant, whose own issuer names none.
            let placed = json.capabilities.records.is_some();
            if json.issuer.as_ref().is_some_and(|issuer| {
                issuer.issuer.as_ref().is_some_and(|by| {
                    !placed || issuer.capabilities.placing.is_none() || by.issuer.is_some()
                })
            }) {
                return Err(serde::de::Error::custom(
                    "a certificate's issuer has no issuer itself",
                ));
            }
            return Ok(Self {
                user_public_key: json.user_public_key,
                device_public_key: json.device_public_key,
                device_label: json.device_label,
                capabilities: json.capabilities,
                issued_at_ns: json.issued_at_ns,
                expires_at_ns: json.expires_at_ns,
                signature: json.signature,
                issuer: json.issuer,
            });
        }
        serde::de::DeserializeSeed::deserialize(BinaryCert::Certificate, deserializer)
    }
}

/// Reads a certificate's binary form, at the step of its chain it is read at. An issuer's
/// certificate names no issuer of its own, but for the placement grant a placed device names,
/// which one of the person's devices may have signed: decoding never recurses past two levels.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BinaryCert {
    /// The certificate itself.
    Certificate,
    /// The issuer of a placed device's certificate: a device of the person, or a placement grant
    /// that may name the device that signed it.
    IssuerOfPlaced,
    /// Any other issuer, which names none.
    Issuer,
}

impl<'de> serde::de::DeserializeSeed<'de> for BinaryCert {
    type Value = DeviceCert;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<DeviceCert, D::Error> {
        deserializer.deserialize_struct("DeviceCert", DEVICE_CERT_FIELDS, self)
    }
}

impl<'de> serde::de::Visitor<'de> for BinaryCert {
    type Value = DeviceCert;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a device certificate")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<DeviceCert, A::Error> {
        use serde::de::Error;
        let missing = |index: usize| A::Error::invalid_length(index, &"a device certificate");
        let user_public_key: [u8; 32] = seq.next_element()?.ok_or_else(|| missing(0))?;
        let device_public_key: [u8; 32] = seq.next_element()?.ok_or_else(|| missing(1))?;
        let device_label: String = seq.next_element()?.ok_or_else(|| missing(2))?;
        let BinaryCapabilitiesOf(capabilities, names_issuer) =
            seq.next_element()?.ok_or_else(|| missing(3))?;
        let issued_at_ns: u64 = seq.next_element()?.ok_or_else(|| missing(4))?;
        let expires_at_ns: Option<u64> = seq.next_element()?.ok_or_else(|| missing(5))?;
        let signature: serde_bytes::ByteArray<64> =
            seq.next_element()?.ok_or_else(|| missing(6))?;
        let issuer = if names_issuer {
            let next = match self {
                Self::Certificate if capabilities.records.is_some() => Self::IssuerOfPlaced,
                Self::Certificate => Self::Issuer,
                Self::IssuerOfPlaced if capabilities.placing.is_some() => Self::Issuer,
                Self::IssuerOfPlaced | Self::Issuer => {
                    return Err(A::Error::custom(
                        "a certificate's issuer has no issuer itself",
                    ));
                }
            };
            Some(Box::new(
                seq.next_element_seed(next)?.ok_or_else(|| missing(7))?,
            ))
        } else {
            None
        };
        Ok(DeviceCert {
            user_public_key,
            device_public_key,
            device_label,
            capabilities,
            issued_at_ns,
            expires_at_ns,
            signature: signature.into_array(),
            issuer,
        })
    }
}

/// Signing view — everything except the signature. Used to build
/// the preimage that `signature` covers.
#[derive(Serialize)]
struct DeviceCertView<'a> {
    user_public_key: &'a [u8; 32],
    device_public_key: &'a [u8; 32],
    device_label: &'a str,
    capabilities: &'a DeviceCapabilities,
    issued_at_ns: u64,
    expires_at_ns: Option<u64>,
}

/// Signing view of a certificate an issuer signed: the view, then the
/// issuer's whole certificate.
#[derive(Serialize)]
struct IssuedCertView<'a> {
    cert: DeviceCertView<'a>,
    issuer: &'a DeviceCert,
}

impl DeviceCert {
    /// Build the signing view for this cert.
    fn view(&self) -> DeviceCertView<'_> {
        DeviceCertView {
            user_public_key: &self.user_public_key,
            device_public_key: &self.device_public_key,
            device_label: &self.device_label,
            capabilities: &self.capabilities,
            issued_at_ns: self.issued_at_ns,
            expires_at_ns: self.expires_at_ns,
        }
    }

    /// Returns `true` if the cert's `expires_at_ns` has passed at
    /// `now_ns`. A cert with `expires_at_ns == None` never expires.
    pub fn is_expired_at(&self, now_ns: u64) -> bool {
        matches!(self.expires_at_ns, Some(e) if now_ns >= e)
    }

    /// The device this certificate writes for: its own, or the device that
    /// certified it as its workspace service
    /// ([`DeviceCapabilities::workspace_service`]). Signatures are the
    /// caller's to check.
    #[must_use]
    pub fn writes_for(&self) -> [u8; 32] {
        match &self.issuer {
            Some(device) if self.capabilities.workspace_service => device.device_public_key,
            _ => self.device_public_key,
        }
    }

    /// Whether this certificate writes for the device `device`
    /// ([`Self::writes_for`]).
    #[must_use]
    pub fn is_of_device(&self, device: &[u8; 32]) -> bool {
        self.writes_for() == *device
    }

    /// The key whose signature this certificate carries: its issuer's when
    /// one signed it, else the person's root.
    #[must_use]
    pub fn signer_public_key(&self) -> [u8; 32] {
        self.issuer
            .as_ref()
            .map_or(self.user_public_key, |issuer| issuer.device_public_key)
    }

    /// Whether the person's root signed this certificate itself, for one of
    /// their devices: no issuer, and no agent's grant, workspace service,
    /// stand-in, placement grant or device confined to one workspace or placed
    /// for records. The root's own statement
    /// that the key is a device of theirs, which is the authority to add
    /// another.
    #[must_use]
    pub fn is_root_signed_device(&self) -> bool {
        self.issuer.is_none()
            && self.capabilities.workspace.is_none()
            && self.capabilities.records.is_none()
            && self.capabilities.placing.is_none()
            && self.capabilities.agent.is_none()
            && !self.capabilities.stand_in
            && !self.capabilities.workspace_service
    }

    /// Whether `self` is `held` reissued for the same device by the same
    /// person's root, later, granting nothing more: the one certificate that
    /// replaces another instead of adding a device. Validity and signature
    /// are the caller's to check where each certificate is taken in.
    ///
    /// This is the single rule a workspace, a fleet and a person's devices
    /// share: a renewal keeps the device the member it was, so it may never
    /// widen what it grants or come from another root.
    #[must_use]
    pub fn renews(&self, held: &Self) -> bool {
        self.device_public_key == held.device_public_key
            && self.user_public_key == held.user_public_key
            && self.issued_at_ns > held.issued_at_ns
            && self.capabilities.grants_no_more_than(&held.capabilities)
    }
}

/// Failure modes for cert signing/verification.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum DeviceCertError {
    /// The user root public key bytes do not decode to a valid
    /// Ed25519 curve point.
    #[error("invalid user root public key (not a point on the curve)")]
    InvalidUserPublicKey,

    /// The signature did not verify under the declared user root
    /// key. Either the wrong root signed, the bytes were tampered
    /// with, or the cert was signed by an impostor.
    #[error("device-cert signature did not verify")]
    BadSignature,

    /// The `expires_at_ns` deadline has passed. Not a signature
    /// failure; the cert is cryptographically valid but no longer
    /// admissible.
    #[error("device cert has expired")]
    Expired,

    /// The certificate's claimed issuance time has not arrived.
    #[error("device cert is not yet valid")]
    NotYetValid,

    /// A finite expiry must be strictly later than issuance.
    #[error("device cert has an invalid validity interval")]
    InvalidValidity,

    /// An agent's grant certificate grants more than its scope, or does
    /// not expire within [`MAX_AGENT_GRANT_LIFETIME_NS`] and is no server key
    /// reaching all workspaces.
    #[error(
        "an agent's grant certificate must grant only its scope and expire within 90 days, \
         unless it is a server key reaching all workspaces"
    )]
    InvalidAgentGrant,

    /// A stand-in's certificate grants more than being one, does not expire
    /// within [`STAND_IN_LIFETIME_NS`], or names an issuer.
    #[error("a stand-in's certificate must grant nothing else and expire within 30 days")]
    InvalidStandIn,

    /// A stand-in's certificate was offered as a device's: a stand-in is
    /// no device of the person.
    #[error("a stand-in approver is no device")]
    StandInIsNoDevice,

    /// A certificate exceeds what its issuer may issue: a stand-in issues a
    /// member for a browser's lifetime, an all-workspaces grant issues a grant
    /// for one workspace that ends no later than it, each of its own person,
    /// root-signed and valid when it issued.
    #[error("a certificate exceeds what its issuer may issue")]
    InvalidIssuer,

    /// A certificate that reaches one workspace alone grants more than that
    /// (and being foreign), names an issuer, or certifies the root's own key.
    #[error(
        "a device that reaches one workspace alone must grant nothing more and be certified by \
         its person's root"
    )]
    InvalidWorkspaceDevice,

    /// A placed device's certificate grants more than its records, names no
    /// record or more than [`MAX_PLACED_RECORDS`], does not expire within
    /// [`MAX_PLACED_DEVICE_LIFETIME_NS`] or after its issuer, or was not
    /// issued, while its issuer was valid, by one of its person's devices or
    /// by a host under their placement grant for those records, for a key that
    /// is neither the issuer's nor the root's.
    #[error(
        "a placed device must reach its records and nothing more, be issued by a device of its \
         person's or under their placement grant, and expire within two hours"
    )]
    InvalidPlacedDevice,

    /// A placement grant grants more than its records, certifies the root's
    /// own key, or was signed by neither the person's root nor one of their
    /// devices the root signed (while that was valid, ending no later).
    #[error(
        "a placement grant must grant its records and nothing more, and be signed by its \
         person's root or one of their devices"
    )]
    InvalidPlacingGrant,

    /// A placement grant was offered as a device's: the host it was given to
    /// is no device of the person.
    #[error("a placement grant is no device")]
    PlacingGrantIsNoDevice,

    /// Canonical-encoding failure. Effectively unreachable but
    /// propagated for hygiene.
    #[error("canonicalisation failed: {0}")]
    Canonical(#[from] CanonicalError),

    /// The key that was asked to sign refused or failed.
    #[error("the key did not sign: {0}")]
    Signer(#[from] SignError),
}

/// Whether `capabilities` a key is certified with at `issued_at_ns` until
/// `expires_at_ns` are an agent's grant that holds: a grant reaches its
/// workspaces and nothing else, and ends within
/// [`MAX_AGENT_GRANT_LIFETIME_NS`] of its issue, unless it is a server key,
/// which lasts as long as the person chose. Capabilities that are no grant at
/// all hold by this rule.
///
/// Every path that certifies a key as an agent's grant asks this before it
/// signs, and every verifier asks it again, so a grant one of them takes is a
/// grant all of them take.
///
/// # Errors
///
/// [`DeviceCertError::InvalidAgentGrant`] for a grant that grants more than its
/// scope, or that outlives an agent's grant without being a server key.
pub fn check_agent_grant(
    capabilities: &DeviceCapabilities,
    issued_at_ns: u64,
    expires_at_ns: Option<u64>,
) -> Result<(), DeviceCertError> {
    if capabilities.agent.is_none() && !capabilities.server_key {
        return Ok(());
    }
    let bounded = capabilities.server_key
        || expires_at_ns.is_some_and(|expiry| {
            expiry > issued_at_ns
                && expiry.saturating_sub(issued_at_ns) <= MAX_AGENT_GRANT_LIFETIME_NS
        });
    if bounded && capabilities.agent_grant_holds() {
        Ok(())
    } else {
        Err(DeviceCertError::InvalidAgentGrant)
    }
}

/// Whether a stand-in's certificate holds its shape: being a stand-in and
/// nothing else, root-signed, expiring within [`STAND_IN_LIFETIME_NS`].
fn check_stand_in(cert: &DeviceCert) -> Result<(), DeviceCertError> {
    if !cert.capabilities.stand_in {
        return Ok(());
    }
    let bounded = cert.expires_at_ns.is_some_and(|expiry| {
        expiry > cert.issued_at_ns
            && expiry.saturating_sub(cert.issued_at_ns) <= STAND_IN_LIFETIME_NS
    });
    if bounded
        && cert.issuer.is_none()
        && cert.capabilities == DeviceCapabilities::stand_in_approver()
        && cert.device_public_key != cert.user_public_key
    {
        Ok(())
    } else {
        Err(DeviceCertError::InvalidStandIn)
    }
}

/// Whether a certificate an issuer signed stays within what that issuer may
/// issue: a device's or a stand-in's browser members, or a grant's
/// one-workspace grants. The signatures are checked by the caller.
fn check_issued(cert: &DeviceCert, issuer: &DeviceCert) -> Result<(), DeviceCertError> {
    if cert.capabilities.records.is_some() {
        return check_placed_device(cert, issuer);
    }
    if cert.capabilities.placing.is_some() {
        // Its issuer was checked with its shape ([`check_placing_grant`]).
        return Ok(());
    }
    if issuer.capabilities.agent.is_some() {
        return check_minted(cert, issuer);
    }
    if cert.capabilities.workspace_service {
        return check_service(cert, issuer);
    }
    let browser = crate::CertificationPolicy::browser();
    let issuer_valid_then = issuer.issued_at_ns <= cert.issued_at_ns
        && issuer
            .expires_at_ns
            .is_none_or(|expiry| cert.issued_at_ns < expiry);
    let lifetime_bounded = cert.expires_at_ns.is_some_and(|expiry| {
        expiry > cert.issued_at_ns
            && expiry.saturating_sub(cert.issued_at_ns) <= browser.lifetime_ns
    });
    let may_issue = if issuer.capabilities.stand_in {
        issuer.issuer.is_none() && check_stand_in(issuer).is_ok()
    } else {
        issuer.is_root_signed_device()
    };
    if may_issue
        && issuer.user_public_key == cert.user_public_key
        && issuer_valid_then
        && lifetime_bounded
        && cert.capabilities.grants_no_more_than(&browser.ceiling)
        && cert.capabilities.workspace.is_none()
        && cert.device_public_key != issuer.device_public_key
        && cert.device_public_key != cert.user_public_key
    {
        Ok(())
    } else {
        Err(DeviceCertError::InvalidIssuer)
    }
}

/// Whether a grant `cert` that the grant `issuer` minted stays within it.
/// The issuer reaches all of its person's workspaces, is root-signed and was
/// valid when it minted. `cert` is an agent's grant for one workspace and
/// nothing else, a server key exactly when its issuer is one (a key the
/// person made mints keys of theirs), and ends no later than the issuer nor
/// [`MAX_AGENT_GRANT_LIFETIME_NS`] after it was minted; it always expires, even
/// when its issuer is a server key that does not. Its key is neither the
/// issuer's nor the root's. A grant can only narrow what it was given, and
/// the chain is never longer than one step.
fn check_minted(cert: &DeviceCert, issuer: &DeviceCert) -> Result<(), DeviceCertError> {
    let issuer_valid_then = issuer.issued_at_ns <= cert.issued_at_ns
        && issuer
            .expires_at_ns
            .is_none_or(|expiry| cert.issued_at_ns < expiry);
    let ends_within = cert.expires_at_ns.is_some_and(|minted| {
        minted > cert.issued_at_ns
            && minted.saturating_sub(cert.issued_at_ns) <= MAX_AGENT_GRANT_LIFETIME_NS
            && issuer.expires_at_ns.is_none_or(|parent| minted <= parent)
    });
    let one_workspace = match &cert.capabilities.agent {
        Some(scope @ AgentScope::Workspace(_)) => {
            cert.capabilities
                == DeviceCapabilities {
                    retired: RetiredFlags::default(),
                    server_key: issuer.capabilities.server_key,
                    ..DeviceCapabilities::agent_grant(scope.clone())
                        .with_browser_pages(cert.capabilities.browser_pages)
                }
        }
        _ => false,
    };
    let issuer_reaches_all = issuer.capabilities
        == DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces)
            .with_browser_pages(issuer.capabilities.browser_pages)
        || issuer.capabilities
            == DeviceCapabilities::server_key_grant()
                .with_browser_pages(issuer.capabilities.browser_pages);
    let pages_within = !cert.capabilities.browser_pages || issuer.capabilities.browser_pages;
    if issuer_reaches_all
        && issuer.issuer.is_none()
        && issuer.user_public_key == cert.user_public_key
        && issuer_valid_then
        && ends_within
        && one_workspace
        && pages_within
        && cert.device_public_key != issuer.device_public_key
        && cert.device_public_key != cert.user_public_key
    {
        check_agent_grant(
            &issuer.capabilities,
            issuer.issued_at_ns,
            issuer.expires_at_ns,
        )
    } else {
        Err(DeviceCertError::InvalidIssuer)
    }
}

/// Whether a device confined to one workspace holds its shape: that and
/// being foreign, nothing more, signed by its person's root for a key that is
/// not the root's.
fn check_workspace_device(cert: &DeviceCert) -> Result<(), DeviceCertError> {
    let Some(workspace) = cert.capabilities.workspace else {
        return Ok(());
    };
    let shape = DeviceCapabilities {
        foreign: cert.capabilities.foreign,
        ..DeviceCapabilities::workspace_device(workspace)
    };
    if cert.capabilities == shape
        && workspace != [0; 16]
        && cert.issuer.is_none()
        && cert.device_public_key != cert.user_public_key
    {
        Ok(())
    } else {
        Err(DeviceCertError::InvalidWorkspaceDevice)
    }
}

/// Every shape rule a certificate's signer is held to before it signs and a
/// verifier before it checks the signature.
fn check_shape(cert: &DeviceCert) -> Result<(), DeviceCertError> {
    check_agent_grant(&cert.capabilities, cert.issued_at_ns, cert.expires_at_ns)?;
    check_stand_in(cert)?;
    check_workspace_device(cert)?;
    check_placing_grant(cert)?;
    match &cert.issuer {
        Some(issuer) => check_issued(cert, issuer),
        // A workspace service is always its device's: the device issues it; a
        // placed device is always the host's that placed it.
        None if cert.capabilities.workspace_service || cert.capabilities.records.is_some() => {
            Err(DeviceCertError::InvalidIssuer)
        }
        None => Ok(()),
    }
}

/// Whether `issuer` was valid when `cert` was issued.
fn valid_when_issued(cert: &DeviceCert, issuer: &DeviceCert) -> bool {
    issuer.issued_at_ns <= cert.issued_at_ns
        && issuer
            .expires_at_ns
            .is_none_or(|expiry| cert.issued_at_ns < expiry)
}

/// Whether `cert` ends no later than `issuer`: never, when `issuer` never
/// ends, else when it ends, at the latest.
fn ends_with(cert: &DeviceCert, issuer: &DeviceCert) -> bool {
    match (cert.expires_at_ns, issuer.expires_at_ns) {
        (_, None) => true,
        (Some(ends), Some(issuer_ends)) => ends <= issuer_ends,
        (None, Some(_)) => false,
    }
}

/// Whether `cert`, when it is a placement grant, has its shape: its records
/// and nothing else, for a key that is not the root's, signed by the root or by
/// one of the person's devices the root signed, valid then and ending no later
/// than the grant, for a key that is not its own.
fn check_placing_grant(cert: &DeviceCert) -> Result<(), DeviceCertError> {
    let Some(records) = &cert.capabilities.placing else {
        return Ok(());
    };
    let signed_by = cert.issuer.as_deref().is_none_or(|device| {
        device.is_root_signed_device()
            && device.user_public_key == cert.user_public_key
            && valid_when_issued(cert, device)
            && ends_with(cert, device)
            && cert.device_public_key != device.device_public_key
    });
    if records.is_well_formed()
        && cert.capabilities == DeviceCapabilities::placing(records.clone())
        && cert.device_public_key != cert.user_public_key
        && signed_by
    {
        Ok(())
    } else {
        Err(DeviceCertError::InvalidPlacingGrant)
    }
}

/// Whether `cert`, a placed device's, has its shape: its records and nothing
/// else, issued by the host that placed it while the host's certificate was
/// valid, expiring within [`MAX_PLACED_DEVICE_LIFETIME_NS`] and no later than
/// that certificate, for a key that is neither the host's nor the root's. The
/// host's certificate is one of the person's devices, or the person's placement
/// grant to the host for records these are within.
fn check_placed_device(cert: &DeviceCert, issuer: &DeviceCert) -> Result<(), DeviceCertError> {
    let Some(records) = &cert.capabilities.records else {
        return Ok(());
    };
    let ends_within = cert.expires_at_ns.is_some_and(|expiry| {
        expiry > cert.issued_at_ns
            && expiry.saturating_sub(cert.issued_at_ns) <= MAX_PLACED_DEVICE_LIFETIME_NS
    }) && ends_with(cert, issuer);
    let may_place = match &issuer.capabilities.placing {
        Some(granted) => check_placing_grant(issuer).is_ok() && records.within(granted),
        None => issuer.is_root_signed_device(),
    };
    if records.is_well_formed()
        && cert.capabilities == DeviceCapabilities::placed(records.clone())
        && may_place
        && issuer.user_public_key == cert.user_public_key
        && valid_when_issued(cert, issuer)
        && ends_within
        && cert.device_public_key != issuer.device_public_key
        && cert.device_public_key != cert.user_public_key
    {
        Ok(())
    } else {
        Err(DeviceCertError::InvalidPlacedDevice)
    }
}

/// The unsigned placement grant the person's root (`user_public_key`) signs to
/// let the host whose device key is `host` place devices for `records`, from
/// `issued_at_ns` until `expires_at_ns` (or until it is revoked). Sign it with
/// [`sign_device_cert`] and the root, or have one of the person's devices sign
/// it instead ([`prepare_placing_grant_by`]).
#[must_use]
pub fn prepare_placing_grant(
    user_public_key: [u8; 32],
    host: [u8; 32],
    records: PlacedRecords,
    label: &str,
    issued_at_ns: u64,
    expires_at_ns: Option<u64>,
) -> DeviceCert {
    DeviceCert {
        user_public_key,
        device_public_key: host,
        device_label: label.to_owned(),
        capabilities: DeviceCapabilities::placing(records),
        issued_at_ns,
        expires_at_ns,
        signature: [0; 64],
        issuer: None,
    }
}

/// [`prepare_placing_grant`] signed by `device`, one of the person's devices
/// their root signed, ending no later than it. Sign it with
/// [`sign_device_cert`] and the device's key.
#[must_use]
pub fn prepare_placing_grant_by(
    device: &DeviceCert,
    host: [u8; 32],
    records: PlacedRecords,
    label: &str,
    issued_at_ns: u64,
    expires_at_ns: Option<u64>,
) -> DeviceCert {
    DeviceCert {
        issuer: Some(Box::new(device.clone())),
        ..prepare_placing_grant(
            device.user_public_key,
            host,
            records,
            label,
            issued_at_ns,
            expires_at_ns,
        )
    }
}

/// The unsigned certificate the host whose certificate is `issuer` signs to
/// make `key` a device placed for `records` alone, from `issued_at_ns` until
/// `expires_at_ns` (at most [`MAX_PLACED_DEVICE_LIFETIME_NS`] later, and no
/// later than `issuer` ends). `issuer` is the host's own certificate as one of
/// the person's devices, or the person's placement grant to the host for
/// records `records` are within. Sign it with [`sign_device_cert`] and the
/// host's device key.
#[must_use]
pub fn prepare_placed_device_cert(
    issuer: &DeviceCert,
    key: [u8; 32],
    records: PlacedRecords,
    label: &str,
    issued_at_ns: u64,
    expires_at_ns: u64,
) -> DeviceCert {
    DeviceCert {
        user_public_key: issuer.user_public_key,
        device_public_key: key,
        device_label: label.to_owned(),
        capabilities: DeviceCapabilities::placed(records),
        issued_at_ns,
        expires_at_ns: Some(expires_at_ns),
        signature: [0; 64],
        issuer: Some(Box::new(issuer.clone())),
    }
}

/// Whether `cert`, a device's workspace service, stays within the device
/// `issuer` that issued it. The issuer is one of the person's own devices,
/// root-signed, valid when it issued. The service has that device's
/// capabilities and nothing else, ends no later than the device's
/// certificate, and its key is neither the device's nor the root's. The chain
/// is never longer than one step.
fn check_service(cert: &DeviceCert, issuer: &DeviceCert) -> Result<(), DeviceCertError> {
    let issuer_is_a_device = issuer.is_root_signed_device();
    let issuer_valid_then = issuer.issued_at_ns <= cert.issued_at_ns
        && issuer
            .expires_at_ns
            .is_none_or(|expiry| cert.issued_at_ns < expiry);
    let ends_within = match (cert.expires_at_ns, issuer.expires_at_ns) {
        (_, None) => true,
        (Some(service), Some(device)) => service <= device,
        (None, Some(_)) => false,
    };
    if issuer_is_a_device
        && issuer.user_public_key == cert.user_public_key
        && issuer_valid_then
        && ends_within
        && cert.capabilities == DeviceCapabilities::service_of(&issuer.capabilities)
        && cert.device_public_key != issuer.device_public_key
        && cert.device_public_key != cert.user_public_key
    {
        Ok(())
    } else {
        Err(DeviceCertError::InvalidIssuer)
    }
}

/// The unsigned certificate the person's device `issuer` signs to make `key`
/// its own workspace service, from `issued_at_ns` until the device's own
/// certificate ends (see [`DeviceCapabilities::workspace_service`]). Sign it
/// with [`sign_device_cert`] or [`sign_device_cert_async`] and the device's
/// key.
#[must_use]
pub fn prepare_service_cert(
    issuer: &DeviceCert,
    key: [u8; 32],
    label: &str,
    issued_at_ns: u64,
) -> DeviceCert {
    DeviceCert {
        user_public_key: issuer.user_public_key,
        device_public_key: key,
        device_label: label.to_owned(),
        capabilities: DeviceCapabilities::service_of(&issuer.capabilities),
        issued_at_ns,
        expires_at_ns: issuer.expires_at_ns,
        signature: [0; 64],
        issuer: Some(Box::new(issuer.clone())),
    }
}

/// The unsigned certificate a person's root (`user_public_key`) signs to
/// make `stand_in_public_key`, a key a host holds, their stand-in approver
/// from `issued_at_ns` until `expires_at_ns` (at most
/// [`STAND_IN_LIFETIME_NS`] later). Sign it with [`sign_device_cert`], or
/// have an external root signer sign [`device_cert_signing_digest`] of it.
#[must_use]
pub fn prepare_stand_in_cert(
    user_public_key: [u8; 32],
    stand_in_public_key: [u8; 32],
    label: &str,
    issued_at_ns: u64,
    expires_at_ns: u64,
) -> DeviceCert {
    DeviceCert {
        user_public_key,
        device_public_key: stand_in_public_key,
        device_label: label.to_owned(),
        capabilities: DeviceCapabilities::stand_in_approver(),
        issued_at_ns,
        expires_at_ns: Some(expires_at_ns),
        signature: [0; 64],
        issuer: None,
    }
}

/// The unsigned certificate the all-workspaces grant `issuer` signs to make
/// `key` a grant reaching only `workspace`, from `issued_at_ns` until
/// `expires_at_ns`, which may be no later than the issuer's own expiry nor
/// [`MAX_AGENT_GRANT_LIFETIME_NS`] after `issued_at_ns`. It is a server key
/// when the issuer is one.
/// Sign it with [`sign_device_cert`] and the issuer's key.
#[must_use]
pub fn prepare_minted_grant(
    issuer: &DeviceCert,
    key: [u8; 32],
    workspace: [u8; 16],
    label: &str,
    issued_at_ns: u64,
    expires_at_ns: u64,
) -> DeviceCert {
    DeviceCert {
        user_public_key: issuer.user_public_key,
        device_public_key: key,
        device_label: label.to_owned(),
        capabilities: DeviceCapabilities {
            server_key: issuer.capabilities.server_key,
            ..DeviceCapabilities::agent_grant(AgentScope::Workspace(workspace))
                .with_browser_pages(issuer.capabilities.browser_pages)
        },
        issued_at_ns,
        expires_at_ns: Some(expires_at_ns),
        signature: [0; 64],
        issuer: Some(Box::new(issuer.clone())),
    }
}

/// Sign a device cert: with the user's root signing key, or — for a
/// certificate whose [`DeviceCert::issuer`] names a stand-in — with the
/// stand-in's key ([`DeviceCert::signer_public_key`]).
///
/// Fills the cert's `signature` field in place. Callers can call
/// this multiple times on the same cert idempotently — the signing
/// view omits the signature so the second call reproduces the first.
///
/// # Errors
///
/// Returns [`DeviceCertError::BadSignature`] when `signer` is not the
/// cert's signer, or the issuer's certificate does not verify;
/// [`DeviceCertError::InvalidValidity`], [`DeviceCertError::InvalidAgentGrant`],
/// [`DeviceCertError::InvalidStandIn`] or [`DeviceCertError::InvalidIssuer`]
/// for a certificate no verifier would take;
/// [`DeviceCertError::Signer`] when the key refuses; or
/// [`DeviceCertError::Canonical`] if the cert cannot be canonically
/// encoded (effectively unreachable).
pub fn sign_device_cert(
    cert: &mut DeviceCert,
    signer: &dyn StatementSigner,
) -> Result<(), DeviceCertError> {
    let digest = prepare_cert_signature(cert, signer.public_key())?;
    cert.signature = signer.sign_digest(&digest)?;
    Ok(())
}

/// [`sign_device_cert`] with a key that answers only later: a page's device key
/// lives in `WebCrypto` ownership and signs through a promise, the same bytes
/// under the same checks.
///
/// # Errors
///
/// As [`sign_device_cert`].
pub async fn sign_device_cert_async(
    cert: &mut DeviceCert,
    signer: &dyn collab_types::signer::DeviceSigner,
) -> Result<(), DeviceCertError> {
    let digest = prepare_cert_signature(cert, signer.device_public_key())?;
    cert.signature = signer.sign(digest.to_vec()).await?;
    Ok(())
}

/// The digest `cert`'s signer of `signer_public_key` signs, every check applied
/// first: what [`sign_device_cert`] signs, and what a signer that answers only
/// later is asked for the same bytes.
fn prepare_cert_signature(
    cert: &DeviceCert,
    signer_public_key: [u8; 32],
) -> Result<[u8; 32], DeviceCertError> {
    // Guard: refuse to sign a cert whose signer is not the signing key.
    // Mis-signing this would produce a syntactically valid but
    // unverifiable cert.
    if cert.signer_public_key() != signer_public_key {
        return Err(DeviceCertError::BadSignature);
    }
    if cert
        .expires_at_ns
        .is_some_and(|expiry| expiry <= cert.issued_at_ns)
    {
        return Err(DeviceCertError::InvalidValidity);
    }
    check_shape(cert)?;
    if let Some(issuer) = &cert.issuer {
        verify_signed(issuer)?;
    }
    device_cert_signing_digest(cert)
}

/// Return the domain-separated digest an external signer must sign for this
/// certificate.
///
/// This is the broker-friendly counterpart to [`sign_device_cert`]. It lets a
/// caller ask a non-exportable user-root capability to sign the exact wire-ABI
/// preimage without copying the private key into another file. Callers must
/// attach the resulting signature and validate it with [`verify_device_cert`]
/// (or [`verify_stand_in_cert`]) before persisting the certificate.
///
/// # Errors
///
/// Returns [`DeviceCertError::Canonical`] if the cert cannot be
/// canonically encoded (effectively unreachable).
pub fn device_cert_signing_digest(cert: &DeviceCert) -> Result<[u8; 32], DeviceCertError> {
    match &cert.issuer {
        Some(issuer) => signing_digest(
            DEVCERT_PREIMAGE_TAG,
            &IssuedCertView {
                cert: cert.view(),
                issuer,
            },
        ),
        None => signing_digest(DEVCERT_PREIMAGE_TAG, &cert.view()),
    }
    .map_err(Into::into)
}

/// Verify a device cert: its root's signature, or — for one a person's
/// stand-in issued — the stand-in's signature and the root's on the
/// stand-in's certificate, within what a stand-in may issue (see the
/// module). A stand-in's own certificate is no device's, and is refused
/// here ([`DeviceCertError::StandInIsNoDevice`]); see
/// [`verify_stand_in_cert`].
///
/// This is the cryptographic check *only*. Callers should also
/// verify:
///
/// - `now_ns >= cert.issued_at_ns` (unless clock skew is being
///   tolerated),
/// - `!cert.is_expired_at(now_ns)`,
/// - `!revocation_list.refuses(cert)`.
///
/// The convenience [`verify_device_cert_at`] bundles those time checks
/// with the signature verification.
///
/// # Errors
///
/// Returns [`DeviceCertError::InvalidUserPublicKey`] if
/// the signer's key is not a valid Ed25519 point,
/// [`DeviceCertError::BadSignature`] if a signature does not verify, or the
/// shape errors [`sign_device_cert`] names.
pub fn verify_device_cert(cert: &DeviceCert) -> Result<(), DeviceCertError> {
    if cert.capabilities.stand_in {
        return Err(DeviceCertError::StandInIsNoDevice);
    }
    if cert.capabilities.placing.is_some() {
        return Err(DeviceCertError::PlacingGrantIsNoDevice);
    }
    verify_signed(cert)
}

/// Verify a placement grant at `now_ns` ([`DeviceCapabilities::placing`]): its
/// shape, every signature, and its validity then. Its revocation is the
/// caller's to check, as a device's is.
///
/// # Errors
///
/// [`DeviceCertError::InvalidPlacingGrant`] for a certificate that is no
/// placement grant, what [`verify_device_cert`] returns for its signatures, or
/// [`DeviceCertError::NotYetValid`] / [`DeviceCertError::Expired`].
pub fn verify_placing_grant_at(cert: &DeviceCert, now_ns: u64) -> Result<(), DeviceCertError> {
    verify_placing_grant(cert)?;
    check_validity_at(cert, now_ns)
}

/// [`verify_placing_grant_at`] whenever: its shape and every signature.
///
/// # Errors
///
/// As [`verify_placing_grant_at`], but for its validity in time.
pub fn verify_placing_grant(cert: &DeviceCert) -> Result<(), DeviceCertError> {
    if cert.capabilities.placing.is_none() {
        return Err(DeviceCertError::InvalidPlacingGrant);
    }
    verify_signed(cert)
}

/// Verify a person's stand-in approver's certificate: root-signed, a
/// stand-in and nothing else, expiring within [`STAND_IN_LIFETIME_NS`].
///
/// # Errors
///
/// [`DeviceCertError::InvalidStandIn`] for a certificate that is no
/// stand-in's, or what [`verify_device_cert`] returns.
pub fn verify_stand_in_cert(cert: &DeviceCert) -> Result<(), DeviceCertError> {
    if !cert.capabilities.stand_in {
        return Err(DeviceCertError::InvalidStandIn);
    }
    verify_signed(cert)
}

/// [`verify_stand_in_cert`] plus its validity at `now_ns`: what a host
/// checks before its stand-in approves anything.
///
/// # Errors
///
/// What [`verify_stand_in_cert`] returns, or
/// [`DeviceCertError::NotYetValid`] / [`DeviceCertError::Expired`].
pub fn verify_stand_in_cert_at(cert: &DeviceCert, now_ns: u64) -> Result<(), DeviceCertError> {
    verify_stand_in_cert(cert)?;
    check_validity_at(cert, now_ns)
}

/// The shape and every signature of `cert`, whatever it certifies.
fn verify_signed(cert: &DeviceCert) -> Result<(), DeviceCertError> {
    check_shape(cert)?;
    if let Some(issuer) = &cert.issuer {
        verify_signed(issuer)?;
    }
    let signer = cert.signer_public_key();
    let digest = device_cert_signing_digest(cert)?;
    // Hosts re-check the same certificates on every request and pump turn.
    verify_remembered(&signer, &digest, &cert.signature, false).map_err(|error| match error {
        RememberedRefusal::Key => DeviceCertError::InvalidUserPublicKey,
        RememberedRefusal::Signature => DeviceCertError::BadSignature,
    })
}

/// Why [`verify_remembered`] refused a signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RememberedRefusal {
    /// The signer's key is not an Ed25519 public key.
    Key,
    /// The signature is not the key's over the message.
    Signature,
}

/// Check that `signature` is `key`'s Ed25519 signature over `message`
/// (strictly, as [`ed25519_dalek::VerifyingKey::verify_strict`], when
/// `strict`).
///
/// A signature check is a pure function of the key, what was signed and the
/// signature: an exact one that verified before verifies again. A workspace's
/// governance is read again with every change to it, and holds a signature
/// for every member (their certificate, their invitation, the membership
/// operations their admission wrote), so checking each anew made one more
/// admission cost a signature check per member. Only successes are
/// remembered, by the message itself when it is a digest and by its BLAKE3
/// hash otherwise; every other input takes the full check.
///
/// # Errors
///
/// [`RememberedRefusal`] says why the signature does not verify.
pub fn verify_remembered(
    key: &[u8; 32],
    message: &[u8],
    signature: &[u8; 64],
    strict: bool,
) -> Result<(), RememberedRefusal> {
    let mut seen_as = [0u8; 129];
    seen_as[..32].copy_from_slice(key);
    match <&[u8; 32]>::try_from(message) {
        Ok(digest) => seen_as[32..64].copy_from_slice(digest),
        Err(_) => seen_as[32..64].copy_from_slice(blake3::hash(message).as_bytes()),
    }
    seen_as[64..128].copy_from_slice(signature);
    seen_as[128] = u8::from(strict);
    if verified_signatures()
        .lock()
        .is_ok_and(|seen| seen.contains(&seen_as))
    {
        return Ok(());
    }
    let vk = ed25519_dalek::VerifyingKey::from_bytes(key).map_err(|_| RememberedRefusal::Key)?;
    let sig = ed25519_dalek::Signature::from_bytes(signature);
    add_signature_check();
    let verified = if strict {
        vk.verify_strict(message, &sig)
    } else {
        vk.verify(message, &sig)
    };
    verified.map_err(|_| RememberedRefusal::Signature)?;
    if let Ok(mut seen) = verified_signatures().lock() {
        if seen.len() >= VERIFIED_SIGNATURE_LIMIT {
            seen.clear();
        }
        seen.insert(seen_as);
    }
    Ok(())
}

static SIGNATURE_CHECKS: AtomicU64 = AtomicU64::new(0);

/// Count a signature [`verify_remembered`] checked in full.
fn add_signature_check() {
    SIGNATURE_CHECKS.fetch_add(1, Ordering::Relaxed);
}

/// The signatures [`verify_remembered`] checked in full in this process:
/// what a test that a cost does not grow with a workspace counts.
#[must_use]
pub fn signature_checks() -> u64 {
    SIGNATURE_CHECKS.load(Ordering::Relaxed)
}

/// Take back `checks` counted as checked, what a test added while it checked
/// something else. Never a way to count less than was checked.
#[doc(hidden)]
pub fn retract_signature_checks(checks: u64) {
    if checks == 0 {
        return;
    }
    let _ = SIGNATURE_CHECKS.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |counted| {
        Some(counted.saturating_sub(checks))
    });
}

/// Forget every signature [`verify_remembered`] remembers, as a process
/// that just started holds none: what a test of a newcomer's own work starts
/// from, where the device that wrote what it reads ran in the same process.
#[doc(hidden)]
pub fn forget_verified_signatures() {
    if let Ok(mut seen) = verified_signatures().lock() {
        seen.clear();
    }
}

/// Room for every signature a workspace of thousands of members holds, so
/// re-reading its governance finds each one remembered.
const VERIFIED_SIGNATURE_LIMIT: usize = 65_536;

pub(crate) fn verified_signatures()
-> &'static std::sync::Mutex<std::collections::HashSet<[u8; 129]>> {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<[u8; 129]>>> =
        std::sync::OnceLock::new();
    SEEN.get_or_init(Default::default)
}

/// Full validation: signature + not-before + expiry.
///
/// Revocation is NOT checked here — the caller passes in a
/// [`crate::RevocationList`] separately because the list lives in
/// the user's directory CRDT, not in the cert itself. See
/// [`crate::RevocationList::refuses`].
///
/// # Errors
///
/// Returns anything [`verify_device_cert`] can return, or
/// [`DeviceCertError::NotYetValid`] if `now_ns` predates issuance,
/// [`DeviceCertError::InvalidValidity`] for a backwards/empty finite interval,
/// or [`DeviceCertError::Expired`] if `now_ns` is at or past expiry.
pub fn verify_device_cert_at(cert: &DeviceCert, now_ns: u64) -> Result<(), DeviceCertError> {
    verify_device_cert(cert)?;
    check_validity_at(cert, now_ns)
}

fn check_validity_at(cert: &DeviceCert, now_ns: u64) -> Result<(), DeviceCertError> {
    if cert
        .expires_at_ns
        .is_some_and(|expiry| expiry <= cert.issued_at_ns)
    {
        return Err(DeviceCertError::InvalidValidity);
    }
    if now_ns < cert.issued_at_ns {
        return Err(DeviceCertError::NotYetValid);
    }
    if cert.is_expired_at(now_ns) {
        return Err(DeviceCertError::Expired);
    }
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod device_name_tests {
    use super::{UNNAMED_PRIMARY_DEVICE, UNNAMED_SHARED_DEVICE, device_name};

    tilo_test_env::scenario! {
        fn a_device_name_is_what_the_person_gave_and_never_a_stand_in;

        phase a_name_the_person_gave_is_the_name;
        {
            assert_eq!(device_name("  Studio phone "), Some("Studio phone"));
            assert_eq!(device_name("Laptop"), Some("Laptop"));
        }

        phase the_stand_in_provisioning_fills_in_is_no_name;
        {
            assert_eq!(device_name(UNNAMED_PRIMARY_DEVICE), None);
            assert_eq!(device_name(UNNAMED_SHARED_DEVICE), None);
            assert_eq!(device_name("   "), None);
            assert_eq!(device_name(""), None);
        }
    }
}
