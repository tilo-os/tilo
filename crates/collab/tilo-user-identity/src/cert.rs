//! [`DeviceCert`] — the signed statement binding a device key to a
//! user identity, plus [`DeviceCapabilities`] describing what the
//! device may do on behalf of the user.
//!
//! The signing preimage constant [`DEVCERT_PREIMAGE_TAG`] is frozen
//! wire ABI — see `signing_preimage_is_frozen_abi` in `lib.rs`.
//!
//! # A person's stand-in approver
//!
//! A person whose root lives in one browser tab only (they set Tilo up
//! through an agent and have not kept their root with a passkey yet) would
//! lose every way to add a device if that tab's storage were cleared. So the
//! tab has the host it uses make a key the host holds — the *stand-in* — and
//! certifies it with the root ([`DeviceCapabilities::stand_in`],
//! [`DEVCERT_V5_PREIMAGE_TAG`]).
//!
//! What a stand-in can do: sign a [`DeviceCert`] for a new device of that one
//! person ([`sign_device_cert`] with the stand-in's key), carrying the
//! stand-in's own root-signed certificate as its [`DeviceCert::issuer`]
//! ([`DEVCERT_V6_PREIMAGE_TAG`]). [`verify_device_cert`] accepts such a
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

use ed25519_dalek::{Signer, Verifier};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::{CanonicalError, signing_digest};

/// Signing-domain tag for `DeviceCert`. Frozen wire ABI:
/// `tilo-devcert-v1` + NUL. Any change breaks in-field
/// certificates.
pub const DEVCERT_PREIMAGE_TAG: &[u8] = b"tilo-devcert-v1\0";

/// Signing-domain tag for a certificate whose capabilities mark a foreign
/// device (see [`DeviceCapabilities::foreign`]). A foreign certificate never
/// verifies under [`DEVCERT_PREIMAGE_TAG`], so a verifier that predates the
/// marker cannot accept it as native.
pub const DEVCERT_V2_PREIMAGE_TAG: &[u8] = b"tilo-devcert-v2\0";

/// Signing-domain tag for a certificate that grants administration (see
/// [`DeviceCapabilities::can_administer_devices`]), foreign or not. It never
/// verifies under the v1 or v2 tags, so no older verifier can take it for a
/// certificate that grants less.
pub const DEVCERT_V3_PREIMAGE_TAG: &[u8] = b"tilo-devcert-v3\0";

/// Signing-domain tag for a certificate a person gave an agent's grant (see
/// [`DeviceCapabilities::agent`]). It never verifies under an older tag, so
/// no verifier that predates grants can take one for a device of the person.
pub const DEVCERT_V4_PREIMAGE_TAG: &[u8] = b"tilo-devcert-v4\0";

/// Signing-domain tag for a person's stand-in approver's certificate (see
/// [`DeviceCapabilities::stand_in`]). It never verifies under an older tag,
/// so no verifier that predates stand-ins can take one for a device.
pub const DEVCERT_V5_PREIMAGE_TAG: &[u8] = b"tilo-devcert-v5\0";

/// Signing-domain tag for a certificate a stand-in or a grant issued (see
/// [`DeviceCert::issuer`]). Its preimage binds the stand-in's whole
/// certificate, signature included, so the certificate cannot be moved to
/// another issuer; no verifier that predates it takes it, as its binary
/// encoding carries a marker they refuse.
pub const DEVCERT_V6_PREIMAGE_TAG: &[u8] = b"tilo-devcert-v6\0";

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

/// First byte of a foreign device's capabilities in binary encodings. A
/// verifier that predates the marker decodes that position as a `bool` and
/// refuses any byte other than 0 or 1.
const FOREIGN_BINARY_MARKER: u8 = 2;

/// First byte of capabilities that grant administration or an agent's grant
/// in binary encodings, followed by one byte of [`EXTENDED_FOREIGN`] |
/// [`EXTENDED_ADMINISTER`] | [`EXTENDED_AGENT`] | [`EXTENDED_STAND_IN`] |
/// [`EXTENDED_ISSUED`] | [`EXTENDED_SERVER_KEY`], the agent's scope when
/// [`EXTENDED_AGENT`] is set (a
/// tag byte, then a workspace's 16 bytes for one workspace), and then the
/// four flags. Verifiers that predate it refuse the byte, as they refuse
/// [`FOREIGN_BINARY_MARKER`]; verifiers that predate a bit refuse it too.
const EXTENDED_BINARY_MARKER: u8 = 3;
const EXTENDED_FOREIGN: u8 = 1;
const EXTENDED_ADMINISTER: u8 = 2;
const EXTENDED_AGENT: u8 = 4;
const EXTENDED_STAND_IN: u8 = 8;
/// Only inside a certificate: its issuer's certificate follows its
/// signature (see [`DeviceCert::issuer`]).
const EXTENDED_ISSUED: u8 = 16;
/// The agent's grant is a person's server key (see
/// [`DeviceCapabilities::server_key`]).
const EXTENDED_SERVER_KEY: u8 = 32;
const SCOPE_ALL: u8 = 0;
const SCOPE_WORKSPACE: u8 = 1;

/// What the key of an agent's grant reaches: the person approved an agent
/// (through a service that holds the key for it) to act in these of their
/// workspaces, and nowhere else.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentScope {
    /// Every workspace of the person, and making new ones.
    AllWorkspaces,
    /// One workspace, by its id: a UUID's 16 bytes.
    Workspace([u8; 16]),
}

impl AgentScope {
    /// The scope as text: `all`, or the workspace's canonical UUID.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Self::AllWorkspaces => "all".to_owned(),
            Self::Workspace(id) => uuid_text(id),
        }
    }

    /// The scope [`Self::name`] wrote: `all`, or a canonical (lower-case,
    /// hyphenated) UUID other than the nil one.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        if text == "all" {
            return Some(Self::AllWorkspaces);
        }
        uuid_bytes(text).map(Self::Workspace)
    }

    /// Whether the grant reaches the workspace `workspace` names (its UUID).
    #[must_use]
    pub fn reaches(&self, workspace: &str) -> bool {
        match self {
            Self::AllWorkspaces => true,
            Self::Workspace(id) => uuid_bytes(workspace).is_some_and(|asked| asked == *id),
        }
    }

    /// Whether a grant of `self` reaches no more than `asked` does, so it
    /// may stand for what a person was asked to approve: the same scope, or —
    /// where they were asked to approve all their workspaces — one of them.
    /// A wider grant never stands for a narrower request.
    #[must_use]
    pub fn within(self, asked: Self) -> bool {
        match asked {
            Self::AllWorkspaces => true,
            Self::Workspace(_) => self == asked,
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

impl Serialize for AgentScope {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.name())
    }
}

impl<'de> Deserialize<'de> for AgentScope {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).ok_or_else(|| {
            serde::de::Error::invalid_value(
                serde::de::Unexpected::Str(&text),
                &"`all` or a canonical workspace UUID",
            )
        })
    }
}

/// What a device may do on behalf of its user.
///
/// The four booleans map directly to enforceable policy checks in
/// `crates/session/session/` and `crates/system/fs/`. Currently a user's *primary* device (the one
/// that holds the root private key) gets `can_own_workspaces:
/// true, can_admit_devices: true`, phones/tablets get
/// `can_own_workspaces: true` but `can_admit_devices: false`, and
/// dedicated home-server hosts get `can_act_as_host: true`.
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

    /// May mint enrollment tokens for new devices. Typically only
    /// the user's primary device — a phone should not be able to
    /// admit another phone into the fleet.
    pub can_admit_devices: bool,

    /// May act as an always-on host (pins workspaces, relays for
    /// other devices). Only true for dedicated servers and
    /// desktop-class machines the user has consciously marked
    /// "host".
    pub can_act_as_host: bool,

    /// May read every workspace on the user's directory, regardless
    /// of per-workspace membership. Reserved for the user's primary
    /// devices and hosts they explicitly trust with the full data
    /// set.
    pub can_read_all_workspaces: bool,

    /// The device runs a Tilo runtime on an operating system Tilo does not
    /// control (stock macOS, another Linux desktop). Workspace policy caps
    /// the role such a device may act with. A foreign certificate is signed
    /// under [`DEVCERT_V2_PREIMAGE_TAG`] and encoded so that verifiers which
    /// predate this field reject it; a native one keeps the v1 encoding.
    pub foreign: bool,

    /// May administer the person's other devices: open their device-admin
    /// route (`tilo-device-admin/1`) and drive the dev daemon behind it.
    /// Granted on purpose to a developer's machine, never by a preset. A
    /// certificate that grants it is signed under
    /// [`DEVCERT_V3_PREIMAGE_TAG`] and encoded so that verifiers which predate
    /// this field reject it; every other certificate keeps its encoding.
    pub can_administer_devices: bool,

    /// The key belongs to an agent's grant, not to a machine of the person:
    /// a service (the person's hosting provider's agent door, say) holds it
    /// so an agent the person approved acts for them, in the workspaces the
    /// scope names and nowhere else. Such a certificate grants none of the
    /// flags above, expires within [`MAX_AGENT_GRANT_LIFETIME_NS`] unless it
    /// is a server key ([`Self::server_key`]), governs no workspace, and is
    /// revoked like any device. It is signed under [`DEVCERT_V4_PREIMAGE_TAG`]
    /// and encoded so that verifiers which predate this field reject it.
    pub agent: Option<AgentScope>,

    /// The key is a person's stand-in approver, held by a host for a person
    /// whose root is in one browser tab only: it may sign certificates of
    /// new member devices of that person and nothing else (see the module).
    /// Such a certificate grants none of the flags above and no agent's
    /// grant, always expires (within [`STAND_IN_LIFETIME_NS`]), and is no
    /// device: [`verify_device_cert`] refuses it, [`verify_stand_in_cert`]
    /// takes it. It is signed under [`DEVCERT_V5_PREIMAGE_TAG`] and encoded so
    /// that verifiers which predate this field reject it.
    pub stand_in: bool,

    /// The agent's grant is a server key the person created for their own
    /// backend, reaching all their workspaces: it lasts until they revoke it,
    /// or until an expiry they chose (see [`MAX_AGENT_GRANT_LIFETIME_NS`]).
    /// Only an all-workspaces grant is one; the grant an agent gets through
    /// a person's consent never is. The marker is part of the signed
    /// encoding, so a certificate cannot gain or lose it, and verifiers that
    /// predate it reject it.
    pub server_key: bool,
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
    can_act_as_host: bool,
    can_read_all_workspaces: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    foreign: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    can_administer_devices: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent: Option<AgentScope>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    stand_in: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    server_key: bool,
}

impl Serialize for DeviceCapabilities {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            return CapabilitiesJson {
                can_own_workspaces: self.can_own_workspaces,
                can_admit_devices: self.can_admit_devices,
                can_act_as_host: self.can_act_as_host,
                can_read_all_workspaces: self.can_read_all_workspaces,
                foreign: self.foreign,
                can_administer_devices: self.can_administer_devices,
                agent: self.agent,
                stand_in: self.stand_in,
                server_key: self.server_key,
            }
            .serialize(serializer);
        }
        serialize_binary_capabilities(self, false, serializer)
    }
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
    // anything more with the extended marker and its byte.
    let flags = [
        capabilities.can_own_workspaces,
        capabilities.can_admit_devices,
        capabilities.can_act_as_host,
        capabilities.can_read_all_workspaces,
    ];
    let scope: Vec<u8> = match capabilities.agent {
        None => Vec::new(),
        Some(AgentScope::AllWorkspaces) => vec![SCOPE_ALL],
        Some(AgentScope::Workspace(id)) => [&[SCOPE_WORKSPACE][..], &id].concat(),
    };
    let bit = |set: bool, bit: u8| if set { bit } else { 0 };
    let extended = bit(capabilities.foreign, EXTENDED_FOREIGN)
        | bit(capabilities.can_administer_devices, EXTENDED_ADMINISTER)
        | bit(capabilities.agent.is_some(), EXTENDED_AGENT)
        | bit(capabilities.stand_in, EXTENDED_STAND_IN)
        | bit(issued, EXTENDED_ISSUED)
        | bit(capabilities.server_key, EXTENDED_SERVER_KEY);
    let extended_form = extended & !EXTENDED_FOREIGN != 0;
    let len = if extended_form {
        scope.len().saturating_add(6)
    } else if capabilities.foreign {
        5
    } else {
        4
    };
    let mut tuple = serializer.serialize_tuple(len)?;
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
                can_act_as_host: json.can_act_as_host,
                can_read_all_workspaces: json.can_read_all_workspaces,
                foreign: json.foreign,
                can_administer_devices: json.can_administer_devices,
                agent: json.agent,
                stand_in: json.stand_in,
                server_key: json.server_key,
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
        // extended marker + its byte + an agent's scope (up to 17 bytes) +
        // four flags.
        deserializer.deserialize_tuple(23, BinaryCapabilities)
    }
}

struct BinaryCapabilities;

impl<'de> serde::de::Visitor<'de> for BinaryCapabilities {
    type Value = BinaryCapabilitiesOf;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .write_str("four capability flags, optionally after the foreign or extended marker")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error;
        // Each element is one byte; `read` counts them for the error a short
        // sequence names.
        let mut read = 0usize;
        let mut next = || {
            let index = read;
            read = read.saturating_add(1);
            seq.next_element::<u8>()?
                .ok_or_else(|| A::Error::invalid_length(index, &"four capability flags"))
        };
        let first = next()?;
        let (extended, agent, flags) = match first {
            FOREIGN_BINARY_MARKER => (EXTENDED_FOREIGN, None, [next()?, next()?, next()?, next()?]),
            EXTENDED_BINARY_MARKER => {
                let extended = next()?;
                // Only administration, an agent's grant, a stand-in and an
                // issuer need the extended form; anything else has exactly
                // one encoding already.
                let known = EXTENDED_FOREIGN
                    | EXTENDED_ADMINISTER
                    | EXTENDED_AGENT
                    | EXTENDED_STAND_IN
                    | EXTENDED_ISSUED
                    | EXTENDED_SERVER_KEY;
                if extended & !known != 0 || extended & !EXTENDED_FOREIGN == 0 {
                    return Err(A::Error::invalid_value(
                        serde::de::Unexpected::Unsigned(extended.into()),
                        &"extended capabilities that grant administration, an agent's grant \
                          or a stand-in, or name an issuer",
                    ));
                }
                let agent = if extended & EXTENDED_AGENT == 0 {
                    None
                } else {
                    match next()? {
                        SCOPE_ALL => Some(AgentScope::AllWorkspaces),
                        SCOPE_WORKSPACE => {
                            let mut id = [0u8; 16];
                            for byte in &mut id {
                                *byte = next()?;
                            }
                            if id == [0; 16] {
                                return Err(A::Error::invalid_value(
                                    serde::de::Unexpected::Bytes(&id),
                                    &"a workspace id other than the nil UUID",
                                ));
                            }
                            Some(AgentScope::Workspace(id))
                        }
                        other => {
                            return Err(A::Error::invalid_value(
                                serde::de::Unexpected::Unsigned(other.into()),
                                &"an agent scope of all workspaces or one",
                            ));
                        }
                    }
                };
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
                can_act_as_host: flag(flags[2])?,
                can_read_all_workspaces: flag(flags[3])?,
                foreign: extended & EXTENDED_FOREIGN != 0,
                can_administer_devices: extended & EXTENDED_ADMINISTER != 0,
                agent,
                stand_in: extended & EXTENDED_STAND_IN != 0,
                server_key: extended & EXTENDED_SERVER_KEY != 0,
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
            can_act_as_host: false,
            can_read_all_workspaces: false,
            foreign: false,
            can_administer_devices: false,
            agent: None,
            stand_in: false,
            server_key: false,
        }
    }

    /// Whether these capabilities grant nothing `held` does not: a renewal
    /// may narrow what a device may do, never widen it. A certificate that
    /// stops being foreign may not become so, and vice versa.
    #[must_use]
    pub fn grants_no_more_than(&self, held: &Self) -> bool {
        let within = |renewed: bool, held: bool| !renewed || held;
        within(self.can_own_workspaces, held.can_own_workspaces)
            && within(self.can_admit_devices, held.can_admit_devices)
            && within(self.can_act_as_host, held.can_act_as_host)
            && within(self.can_read_all_workspaces, held.can_read_all_workspaces)
            && within(self.can_administer_devices, held.can_administer_devices)
            && (self.foreign || !held.foreign)
            && self.agent == held.agent
            && self.stand_in == held.stand_in
            && self.server_key == held.server_key
    }

    /// The capabilities of an agent's grant reaching `scope`: nothing but
    /// the scope (see [`Self::agent`]).
    #[must_use]
    pub const fn agent_grant(scope: AgentScope) -> Self {
        Self {
            agent: Some(scope),
            ..Self::nothing()
        }
    }

    /// The capabilities of a person's server key: an agent's grant reaching
    /// all their workspaces that lasts until revoked (see
    /// [`Self::server_key`]).
    #[must_use]
    pub const fn server_key_grant() -> Self {
        Self {
            server_key: true,
            ..Self::agent_grant(AgentScope::AllWorkspaces)
        }
    }

    /// Whether these are an agent's grant's capabilities that hold: the
    /// scope, and no other flag but being a server key that reaches all
    /// workspaces. `true` for every certificate that is neither a grant's nor
    /// a server key's.
    #[must_use]
    pub const fn agent_grant_holds(&self) -> bool {
        if self.server_key && !matches!(self.agent, Some(AgentScope::AllWorkspaces)) {
            return false;
        }
        self.agent.is_none()
            || !(self.can_own_workspaces
                || self.can_admit_devices
                || self.can_act_as_host
                || self.can_read_all_workspaces
                || self.foreign
                || self.can_administer_devices
                || self.stand_in)
    }

    /// The capabilities of a person's stand-in approver: nothing but being
    /// one (see [`Self::stand_in`]).
    #[must_use]
    pub const fn stand_in_approver() -> Self {
        Self {
            stand_in: true,
            ..Self::nothing()
        }
    }

    /// A device the person uses themselves on a Tilo-controlled system: it
    /// owns workspaces and is neither a host nor foreign. The person's
    /// devices of this kind find, dial and trust each other.
    #[must_use]
    pub const fn is_personal_device(&self) -> bool {
        self.can_own_workspaces && !self.can_act_as_host && !self.foreign
    }

    /// Whether a workspace governed by a device with these capabilities has
    /// that governor take part in its channels, wherever it runs (a person's
    /// device or a relay): the workspace's records are then sealed channels
    /// each member opens itself, never documents the governor commits in the
    /// clear. The workspace host and every member decide this the same way.
    #[must_use]
    pub const fn takes_part_in_workspace_channels(&self) -> bool {
        self.can_own_workspaces
    }

    /// The name the old shell and the room host still call it by; it goes
    /// when they do.
    #[doc(hidden)]
    #[must_use]
    pub const fn takes_part_in_room_channels(&self) -> bool {
        self.takes_part_in_workspace_channels()
    }

    /// Capability set for the user's primary device (holds root
    /// private key, admits new devices, reads everything).
    pub fn primary() -> Self {
        Self {
            can_own_workspaces: true,
            can_admit_devices: true,
            can_read_all_workspaces: true,
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

    /// Capability set for an always-on host (home server / cloud
    /// tenant). Reads everything so it can pin all workspaces; does
    /// not own or admit — those stay with the human's primary
    /// devices.
    pub fn host() -> Self {
        Self {
            can_act_as_host: true,
            can_read_all_workspaces: true,
            ..Self::nothing()
        }
    }

    /// Capability set for a developer's machine that administers the
    /// person's devices and does nothing else: it owns, admits, hosts and
    /// reads nothing.
    pub fn administrator() -> Self {
        Self {
            can_administer_devices: true,
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
/// `view` is the `DeviceCertView` omitting the `signature` field. A
/// foreign device's certificate uses [`DEVCERT_V2_PREIMAGE_TAG`] instead,
/// and so on for the later tags (see [`device_cert_signing_digest`]). A
/// certificate a person's stand-in issued is signed by the stand-in's key
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
    /// the root signed itself: a person's stand-in approver (see the module),
    /// or an all-workspaces agent's grant that minted a grant for one
    /// workspace ([`prepare_minted_grant`]).
    /// Encoded only when present, so every other certificate encodes and
    /// signs exactly as before; signed under [`DEVCERT_V6_PREIMAGE_TAG`].
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
            if json
                .issuer
                .as_ref()
                .is_some_and(|issuer| issuer.issuer.is_some())
            {
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
        serde::de::DeserializeSeed::deserialize(BinaryCert { nested: false }, deserializer)
    }
}

/// Reads a certificate's binary form. An issuer's certificate (`nested`)
/// names no issuer of its own, so decoding never recurses past one level.
#[derive(Clone, Copy)]
struct BinaryCert {
    nested: bool,
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
            if self.nested {
                return Err(A::Error::custom(
                    "a certificate's issuer has no issuer itself",
                ));
            }
            Some(Box::new(
                seq.next_element_seed(BinaryCert { nested: true })?
                    .ok_or_else(|| missing(7))?,
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

    /// The key whose signature this certificate carries: its stand-in's
    /// or grant's when one issued it, else the person's root.
    #[must_use]
    pub fn signer_public_key(&self) -> [u8; 32] {
        self.issuer
            .as_ref()
            .map_or(self.user_public_key, |issuer| issuer.device_public_key)
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

    /// Canonical-encoding failure. Effectively unreachable but
    /// propagated for hygiene.
    #[error("canonicalisation failed: {0}")]
    Canonical(#[from] CanonicalError),
}

/// Whether `cert`'s shape holds: an agent's grant grants only its scope and
/// expires within [`MAX_AGENT_GRANT_LIFETIME_NS`] of its issue, unless it is
/// a server key, which lasts as long as the person chose.
fn check_agent_grant(cert: &DeviceCert) -> Result<(), DeviceCertError> {
    if cert.capabilities.agent.is_none() && !cert.capabilities.server_key {
        return Ok(());
    }
    let bounded = cert.capabilities.server_key
        || cert.expires_at_ns.is_some_and(|expiry| {
            expiry.saturating_sub(cert.issued_at_ns) <= MAX_AGENT_GRANT_LIFETIME_NS
        });
    if bounded && cert.capabilities.agent_grant_holds() {
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
/// issue: a stand-in's members, or a grant's one-workspace grants. The
/// signatures are checked by the caller.
fn check_issued(cert: &DeviceCert, issuer: &DeviceCert) -> Result<(), DeviceCertError> {
    if issuer.capabilities.agent.is_some() {
        return check_minted(cert, issuer);
    }
    let browser = crate::CertificationPolicy::browser();
    let issuer_valid_then = issuer.issued_at_ns <= cert.issued_at_ns
        && issuer
            .expires_at_ns
            .is_some_and(|expiry| cert.issued_at_ns < expiry);
    let lifetime_bounded = cert.expires_at_ns.is_some_and(|expiry| {
        expiry > cert.issued_at_ns
            && expiry.saturating_sub(cert.issued_at_ns) <= browser.lifetime_ns
    });
    if issuer.capabilities.stand_in
        && issuer.issuer.is_none()
        && issuer.user_public_key == cert.user_public_key
        && issuer_valid_then
        && lifetime_bounded
        && cert.capabilities.grants_no_more_than(&browser.ceiling)
        && cert.device_public_key != issuer.device_public_key
        && cert.device_public_key != cert.user_public_key
    {
        check_stand_in(issuer)
    } else {
        Err(DeviceCertError::InvalidIssuer)
    }
}

/// Whether a grant `cert` that the grant `issuer` minted stays within it.
/// The issuer reaches all of its person's workspaces, is root-signed and was
/// valid when it minted. `cert` is an agent's grant for one workspace and
/// nothing else, and ends no later than the issuer; it always expires, even
/// when its issuer is a server key that does not. Its key is neither the
/// issuer's nor the root's. A grant can only narrow what it was given, and
/// the chain is never longer than one step.
fn check_minted(cert: &DeviceCert, issuer: &DeviceCert) -> Result<(), DeviceCertError> {
    let issuer_valid_then = issuer.issued_at_ns <= cert.issued_at_ns
        && issuer
            .expires_at_ns
            .is_none_or(|expiry| cert.issued_at_ns < expiry);
    let ends_within = cert.expires_at_ns.is_some_and(|minted| {
        minted > cert.issued_at_ns && issuer.expires_at_ns.is_none_or(|parent| minted <= parent)
    });
    let one_workspace = match cert.capabilities.agent {
        Some(scope @ AgentScope::Workspace(_)) => {
            cert.capabilities == DeviceCapabilities::agent_grant(scope)
        }
        _ => false,
    };
    let issuer_reaches_all = issuer.capabilities
        == DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces)
        || issuer.capabilities == DeviceCapabilities::server_key_grant();
    if issuer_reaches_all
        && issuer.issuer.is_none()
        && issuer.user_public_key == cert.user_public_key
        && issuer_valid_then
        && ends_within
        && one_workspace
        && cert.device_public_key != issuer.device_public_key
        && cert.device_public_key != cert.user_public_key
    {
        check_agent_grant(issuer)
    } else {
        Err(DeviceCertError::InvalidIssuer)
    }
}

/// Every shape rule a certificate's signer is held to before it signs and a
/// verifier before it checks the signature.
fn check_shape(cert: &DeviceCert) -> Result<(), DeviceCertError> {
    check_agent_grant(cert)?;
    check_stand_in(cert)?;
    match &cert.issuer {
        Some(issuer) => check_issued(cert, issuer),
        None => Ok(()),
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
/// [`MAX_AGENT_GRANT_LIFETIME_NS`] after `issued_at_ns`.
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
        capabilities: DeviceCapabilities::agent_grant(AgentScope::Workspace(workspace)),
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
/// Returns [`DeviceCertError::BadSignature`] if `signing_key` is not the
/// cert's signer, or the issuer's certificate does not verify;
/// [`DeviceCertError::InvalidValidity`], [`DeviceCertError::InvalidAgentGrant`],
/// [`DeviceCertError::InvalidStandIn`] or [`DeviceCertError::InvalidIssuer`]
/// for a certificate no verifier would take; or
/// [`DeviceCertError::Canonical`] if the cert cannot be canonically
/// encoded (effectively unreachable).
pub fn sign_device_cert(
    cert: &mut DeviceCert,
    signing_key: &ed25519_dalek::SigningKey,
) -> Result<(), DeviceCertError> {
    // Guard: refuse to sign a cert whose signer is not the signing key.
    // Mis-signing this would produce a syntactically valid but
    // unverifiable cert.
    if cert.signer_public_key() != signing_key.verifying_key().to_bytes() {
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

    let digest = device_cert_signing_digest(cert)?;
    let sig = signing_key.sign(&digest);
    cert.signature = sig.to_bytes();
    Ok(())
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
    if let Some(issuer) = &cert.issuer {
        return signing_digest(
            DEVCERT_V6_PREIMAGE_TAG,
            &IssuedCertView {
                cert: cert.view(),
                issuer,
            },
        )
        .map_err(Into::into);
    }
    let tag = if cert.capabilities.stand_in {
        DEVCERT_V5_PREIMAGE_TAG
    } else if cert.capabilities.agent.is_some() {
        DEVCERT_V4_PREIMAGE_TAG
    } else if cert.capabilities.can_administer_devices {
        DEVCERT_V3_PREIMAGE_TAG
    } else if cert.capabilities.foreign {
        DEVCERT_V2_PREIMAGE_TAG
    } else {
        DEVCERT_PREIMAGE_TAG
    };
    signing_digest(tag, &cert.view()).map_err(Into::into)
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
    SIGNATURE_CHECKS.with(|checks| checks.set(checks.get().saturating_add(1)));
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

thread_local! {
    static SIGNATURE_CHECKS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The signatures [`verify_remembered`] checked in full on this thread since
/// the last call: what a test that a cost does not grow with a workspace
/// counts.
#[must_use]
pub fn take_signature_checks() -> u64 {
    SIGNATURE_CHECKS.with(|checks| checks.replace(0))
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
mod tests {
    use super::*;
    use crate::UserIdentity;

    fn fresh_cert(caps: DeviceCapabilities) -> (ed25519_dalek::SigningKey, DeviceCert) {
        let (root_sk, user) = UserIdentity::generate("Alice", 1_720_000_000_000_000_000);
        let (_dev_sk, dev) =
            crate::DeviceIdentity::generate("Framework 16", 1_720_000_000_000_000_100);
        let mut cert = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: dev.public_key,
            device_label: dev.label,
            capabilities: caps,
            issued_at_ns: 1_720_000_000_000_000_200,
            expires_at_ns: None,
            signature: [0u8; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root_sk).unwrap();
        (root_sk, cert)
    }

    /// Certificates sign and verify, honour expiry, round-trip canonically, reproduce their
    /// signature vector, and every capability preset is distinct.
    #[allow(
        clippy::similar_names,
        reason = "the _sk/_pk keypair naming convention is deliberate and used throughout this crate's tests"
    )]
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn device_certificates_sign_verify_expire_and_round_trip() {
        // Phase: sign_and_verify.
        {
            let (_root, cert) = fresh_cert(DeviceCapabilities::primary());
            verify_device_cert(&cert).unwrap();
        }

        // Phase: expiry_semantics.
        {
            let (root_sk, user) = UserIdentity::generate("Alice", 1);
            let (_dev_sk, dev) = crate::DeviceIdentity::generate("dev", 2);
            let mut cert = DeviceCert {
                user_public_key: user.public_key,
                device_public_key: dev.public_key,
                device_label: dev.label,
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 100,
                expires_at_ns: Some(200),
                signature: [0u8; 64],
                issuer: None,
            };
            sign_device_cert(&mut cert, &root_sk).unwrap();
            assert_eq!(
                verify_device_cert_at(&cert, 99).unwrap_err(),
                DeviceCertError::NotYetValid
            );
            // Before expiry
            verify_device_cert_at(&cert, 150).unwrap();
            // Exactly at expiry (`now >= expires_at_ns` — exclusive on
            // the left side, inclusive on the right).
            assert_eq!(
                verify_device_cert_at(&cert, 200).unwrap_err(),
                DeviceCertError::Expired
            );
            // After expiry
            assert_eq!(
                verify_device_cert_at(&cert, 300).unwrap_err(),
                DeviceCertError::Expired
            );

            cert.expires_at_ns = Some(cert.issued_at_ns);
            assert_eq!(
                sign_device_cert(&mut cert, &root_sk).unwrap_err(),
                DeviceCertError::InvalidValidity
            );
        }

        // Phase: cert_round_trips_through_canonical.
        {
            let (_root, cert) = fresh_cert(DeviceCapabilities::host());
            let bytes = crate::canonical::canonical_bytes(&cert).unwrap();
            let back: DeviceCert = crate::canonical::from_canonical_bytes(&bytes).unwrap();
            assert_eq!(cert, back);
            verify_device_cert(&back).unwrap();
        }

        // Phase: signature_vector_is_reproducible.
        // Signature outputs must be reproducible from the same inputs
        // (Ed25519 is deterministic and we build the preimage
        // deterministically). Locks in the wire vectors — if this ever
        // fails, the signing preimage has drifted.
        // `root_sk`/`root_pk` follows this crate's established signing-key /
        // public-key naming convention; see the reason on `directory::tests`.
        {
            use ed25519_dalek::SigningKey;

            // Fixed-seed keypair for the vector. This is the ONLY test
            // that uses a fixed seed; production always uses UnwrapErr(SysRng).
            let seed = [1u8; 32];
            let root_sk = SigningKey::from_bytes(&seed);
            let root_pk = root_sk.verifying_key().to_bytes();

            let device_pk = [2u8; 32];

            let mut cert = DeviceCert {
                user_public_key: root_pk,
                device_public_key: device_pk,
                device_label: "test-device".to_string(),
                capabilities: DeviceCapabilities {
                    can_own_workspaces: true,
                    can_admit_devices: false,
                    can_act_as_host: false,
                    can_read_all_workspaces: false,
                    foreign: false,
                    can_administer_devices: false,
                    agent: None,
                    stand_in: false,
                    server_key: false,
                },
                issued_at_ns: 1_000_000,
                expires_at_ns: None,
                signature: [0u8; 64],
                issuer: None,
            };
            sign_device_cert(&mut cert, &root_sk).unwrap();

            // Re-sign an identical view; must yield the same signature
            // (Ed25519 is deterministic, so this is a strong wire-ABI
            // check — any drift in the preimage or postcard encoding
            // would flip these bytes).
            let sig_1 = cert.signature;
            let mut cert2 = cert.clone();
            cert2.signature = [0u8; 64];
            sign_device_cert(&mut cert2, &root_sk).unwrap();
            assert_eq!(sig_1, cert2.signature);
        }

        // Phase: all_capability_presets_are_distinct.
        {
            // Each preset must produce a different signature — otherwise
            // a peer could substitute one preset for another without
            // touching the sig.
            let (root_sk, user) = UserIdentity::generate("Alice", 1);
            let (_dev_sk, dev) = crate::DeviceIdentity::generate("dev", 2);

            let build = |caps: DeviceCapabilities| {
                let mut c = DeviceCert {
                    user_public_key: user.public_key,
                    device_public_key: dev.public_key,
                    device_label: dev.label.clone(),
                    capabilities: caps,
                    issued_at_ns: 3,
                    expires_at_ns: None,
                    signature: [0u8; 64],
                    issuer: None,
                };
                sign_device_cert(&mut c, &root_sk).unwrap();
                c.signature
            };

            let s_primary = build(DeviceCapabilities::primary());
            let s_member = build(DeviceCapabilities::member());
            let s_host = build(DeviceCapabilities::host());
            assert_ne!(s_primary, s_member);
            assert_ne!(s_member, s_host);
            assert_ne!(s_primary, s_host);
        }
    }

    /// Tampering with the capability, device key, signature or user key, or signing by a different
    /// root, fails verification.
    #[test]
    fn tampering_with_any_certificate_field_or_root_fails_verification() {
        // Phase: tampered_capability_fails.
        {
            let (_root, mut cert) = fresh_cert(DeviceCapabilities::member());
            cert.capabilities.can_admit_devices = true;
            assert_eq!(
                verify_device_cert(&cert).unwrap_err(),
                DeviceCertError::BadSignature
            );
        }

        // Phase: tampered_device_pubkey_fails.
        {
            let (_root, mut cert) = fresh_cert(DeviceCapabilities::primary());
            cert.device_public_key[0] ^= 0x01;
            assert_eq!(
                verify_device_cert(&cert).unwrap_err(),
                DeviceCertError::BadSignature
            );
        }

        // Phase: tampered_signature_fails.
        {
            let (_root, mut cert) = fresh_cert(DeviceCapabilities::primary());
            cert.signature[0] ^= 0x01;
            assert_eq!(
                verify_device_cert(&cert).unwrap_err(),
                DeviceCertError::BadSignature
            );
        }

        // Phase: tampered_user_pubkey_fails.
        {
            let (_root, mut cert) = fresh_cert(DeviceCapabilities::primary());
            // Flip a bit in the embedded user pubkey. The signature no
            // longer verifies under the new pubkey.
            cert.user_public_key[0] ^= 0x01;
            let err = verify_device_cert(&cert).unwrap_err();
            assert!(matches!(
                err,
                DeviceCertError::BadSignature | DeviceCertError::InvalidUserPublicKey
            ));
        }

        // Phase: signed_by_different_root_fails.
        {
            // Alice's cert signed with Bob's root key — mismatch caught
            // by the pre-sign guard (BadSignature).
            let (_, alice) = UserIdentity::generate("Alice", 1);
            let (bob_sk, _) = UserIdentity::generate("Bob", 2);
            let (_, dev) = crate::DeviceIdentity::generate("dev", 3);
            let mut cert = DeviceCert {
                user_public_key: alice.public_key,
                device_public_key: dev.public_key,
                device_label: dev.label,
                capabilities: DeviceCapabilities::primary(),
                issued_at_ns: 4,
                expires_at_ns: None,
                signature: [0u8; 64],
                issuer: None,
            };
            assert_eq!(
                sign_device_cert(&mut cert, &bob_sk).unwrap_err(),
                DeviceCertError::BadSignature
            );
        }
    }

    fn golden_cert(foreign: bool) -> DeviceCert {
        golden_cert_with(DeviceCapabilities {
            foreign,
            ..DeviceCapabilities::member()
        })
    }

    fn golden_cert_with(capabilities: DeviceCapabilities) -> DeviceCert {
        let root_sk = ed25519_dalek::SigningKey::from_bytes(&[1u8; 32]);
        let mut cert = DeviceCert {
            user_public_key: root_sk.verifying_key().to_bytes(),
            device_public_key: [2u8; 32],
            device_label: "golden".to_string(),
            capabilities,
            issued_at_ns: 1_000_000,
            expires_at_ns: Some(2_000_000),
            signature: [0u8; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root_sk).unwrap();
        cert
    }

    const GOLDEN_V1_JSON: &str = r#"{"user_public_key":[138,136,227,221,116,9,241,149,253,82,219,45,60,186,93,114,202,103,9,191,29,148,18,27,243,116,136,1,180,15,111,92],"device_public_key":[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2],"device_label":"golden","capabilities":{"can_own_workspaces":true,"can_admit_devices":false,"can_act_as_host":false,"can_read_all_workspaces":false},"issued_at_ns":1000000,"expires_at_ns":2000000,"signature":[193,94,11,63,27,213,214,122,35,157,141,24,157,200,120,75,180,243,189,153,177,189,224,147,47,142,23,15,234,170,239,9,175,129,176,168,99,254,102,174,97,65,248,91,187,181,93,141,241,211,8,123,41,66,110,172,36,46,39,75,189,14,237,1]}"#;
    const GOLDEN_V1_POSTCARD: &str = "8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c020202020202020202020202020202020202020202020202020202020202020206676f6c64656e01000000c0843d0180897a40c15e0b3f1bd5d67a239d8d189dc8784bb4f3bd99b1bde0932f8e170feaaaef09af81b0a863fe66ae6141f85bbbb55d8df1d3087b29426eac242e274bbd0eed01";
    const GOLDEN_V1_DIGEST: &str =
        "0d8271d3aadb5438af000ff5f96b6355102b57a271885ed26142fb698884e623";

    /// The certificate encodings and signing digest as they were before the
    /// foreign marker existed, pinned from the pre-change build.
    #[test]
    fn native_certificates_keep_their_v1_encoding_and_digest() {
        let cert = golden_cert(false);
        assert_eq!(serde_json::to_string(&cert).unwrap(), GOLDEN_V1_JSON);
        assert_eq!(
            hex::encode(crate::canonical::canonical_bytes(&cert).unwrap()),
            GOLDEN_V1_POSTCARD
        );
        assert_eq!(
            hex::encode(device_cert_signing_digest(&cert).unwrap()),
            GOLDEN_V1_DIGEST
        );
        let from_json: DeviceCert = serde_json::from_str(GOLDEN_V1_JSON).unwrap();
        let from_postcard: DeviceCert =
            crate::canonical::from_canonical_bytes(&hex::decode(GOLDEN_V1_POSTCARD).unwrap())
                .unwrap();
        assert_eq!(from_json, cert);
        assert_eq!(from_postcard, cert);
        verify_device_cert(&from_postcard).unwrap();
    }

    #[test]
    fn foreign_certificates_sign_under_their_own_tag() {
        let foreign = golden_cert(true);
        verify_device_cert(&foreign).unwrap();
        assert_eq!(
            device_cert_signing_digest(&foreign).unwrap(),
            signing_digest(DEVCERT_V2_PREIMAGE_TAG, &foreign.view()).unwrap()
        );

        let json = serde_json::to_string(&foreign).unwrap();
        assert!(json.contains(r#""foreign":true"#));
        assert_eq!(serde_json::from_str::<DeviceCert>(&json).unwrap(), foreign);
        let bytes = crate::canonical::canonical_bytes(&foreign).unwrap();
        assert_eq!(
            crate::canonical::from_canonical_bytes::<DeviceCert>(&bytes).unwrap(),
            foreign
        );

        let mut stripped = foreign;
        stripped.capabilities.foreign = false;
        assert_eq!(
            verify_device_cert(&stripped).unwrap_err(),
            DeviceCertError::BadSignature
        );
        let mut marked = golden_cert(false);
        marked.capabilities.foreign = true;
        assert_eq!(
            verify_device_cert(&marked).unwrap_err(),
            DeviceCertError::BadSignature
        );
    }

    /// A frozen copy of the certificate types as they were before the
    /// foreign marker, standing in for a verifier built from an older tree.
    mod v1 {
        #[allow(
            clippy::struct_excessive_bools,
            reason = "a frozen copy of the v1 wire type"
        )]
        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct DeviceCapabilities {
            pub can_own_workspaces: bool,
            pub can_admit_devices: bool,
            pub can_act_as_host: bool,
            pub can_read_all_workspaces: bool,
        }

        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct DeviceCert {
            pub user_public_key: [u8; 32],
            pub device_public_key: [u8; 32],
            pub device_label: String,
            pub capabilities: DeviceCapabilities,
            pub issued_at_ns: u64,
            pub expires_at_ns: Option<u64>,
            #[serde(with = "serde_bytes")]
            pub signature: [u8; 64],
        }
    }

    /// An agent's grant signs under its own tag, round-trips with its scope
    /// in both encodings, must expire and grant nothing else, survives no
    /// change of scope, and is refused by verifiers that predate it.
    #[test]
    fn agent_grants_carry_their_scope_expire_and_old_verifiers_refuse_them() {
        let workspace = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0";
        let one = AgentScope::parse(workspace).unwrap();
        assert_eq!(one.name(), workspace);
        assert!(one.reaches(workspace));
        assert!(!one.reaches("0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f1"));
        assert!(!one.makes_workspaces());
        assert!(AgentScope::AllWorkspaces.reaches(workspace));
        assert!(AgentScope::AllWorkspaces.makes_workspaces());
        // One workspace narrows what all of them was, and never the reverse.
        assert!(one.within(AgentScope::AllWorkspaces));
        assert!(one.within(one));
        assert!(AgentScope::AllWorkspaces.within(AgentScope::AllWorkspaces));
        assert!(!AgentScope::AllWorkspaces.within(one));
        for malformed in [
            "",
            "ALL",
            "0F1E2D3C-4B5A-6978-8796-A5B4C3D2E1F0",
            "00000000-0000-0000-0000-000000000000",
            "0f1e2d3c4b5a69788796a5b4c3d2e1f0",
        ] {
            assert_eq!(AgentScope::parse(malformed), None, "{malformed}");
        }

        for scope in [AgentScope::AllWorkspaces, one] {
            let grant = golden_cert_with(DeviceCapabilities::agent_grant(scope));
            verify_device_cert(&grant).unwrap();
            assert_eq!(
                device_cert_signing_digest(&grant).unwrap(),
                signing_digest(DEVCERT_V4_PREIMAGE_TAG, &grant.view()).unwrap()
            );
            let json = serde_json::to_string(&grant).unwrap();
            assert_eq!(serde_json::from_str::<DeviceCert>(&json).unwrap(), grant);
            let bytes = postcard::to_allocvec(&(&grant, [9u8; 32])).unwrap();
            let (back, nonce): (DeviceCert, [u8; 32]) = postcard::from_bytes(&bytes).unwrap();
            assert_eq!((back, nonce), (grant.clone(), [9u8; 32]));
            // Verifiers that predate grants stop at the marker.
            assert!(postcard::from_bytes::<(v1::DeviceCert, [u8; 32])>(&bytes).is_err());
            assert!(serde_json::from_str::<v1::DeviceCert>(&json).is_err());
            // Another scope under the same signature does not verify.
            let mut moved = grant.clone();
            moved.capabilities.agent = Some(match scope {
                AgentScope::AllWorkspaces => one,
                AgentScope::Workspace(_) => AgentScope::AllWorkspaces,
            });
            assert_eq!(
                verify_device_cert(&moved),
                Err(DeviceCertError::BadSignature)
            );
            // A grant is not renewed into another scope or a person's device.
            let mut wider = grant.clone();
            wider.capabilities = DeviceCapabilities::member();
            wider.issued_at_ns += 1;
            assert!(!wider.renews(&grant));
        }

        let root = ed25519_dalek::SigningKey::from_bytes(&[1u8; 32]);
        let unsigned = |capabilities: DeviceCapabilities, expires_at_ns: Option<u64>| DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: [2u8; 32],
            device_label: "Claude Code via mcp.example".to_owned(),
            capabilities,
            issued_at_ns: 1_000,
            expires_at_ns,
            signature: [0u8; 64],
            issuer: None,
        };
        // A grant that never expires, outlives the bound, or grants a flag
        // besides its scope is neither signed nor taken.
        let wide = DeviceCapabilities {
            can_read_all_workspaces: true,
            ..DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces)
        };
        for (capabilities, expiry) in [
            (DeviceCapabilities::agent_grant(one), None),
            (
                DeviceCapabilities::agent_grant(one),
                Some(1_000 + MAX_AGENT_GRANT_LIFETIME_NS + 1),
            ),
            (wide, Some(2_000)),
        ] {
            let mut cert = unsigned(capabilities, expiry);
            assert_eq!(
                sign_device_cert(&mut cert, &root),
                Err(DeviceCertError::InvalidAgentGrant)
            );
            // Signed anyway by a root that skips the check: still refused.
            cert.signature = root
                .sign(&signing_digest(DEVCERT_V4_PREIMAGE_TAG, &cert.view()).unwrap())
                .to_bytes();
            assert_eq!(
                verify_device_cert(&cert),
                Err(DeviceCertError::InvalidAgentGrant)
            );
        }
        let mut bounded = unsigned(
            DeviceCapabilities::agent_grant(one),
            Some(1_000 + MAX_AGENT_GRANT_LIFETIME_NS),
        );
        sign_device_cert(&mut bounded, &root).unwrap();
        verify_device_cert_at(&bounded, 2_000).unwrap();
        assert_eq!(
            verify_device_cert_at(&bounded, 1_000 + MAX_AGENT_GRANT_LIFETIME_NS),
            Err(DeviceCertError::Expired)
        );
    }

    const DAY_NS: u64 = 24 * 60 * 60 * 1_000_000_000;

    /// A root `seed`'s stand-in (key `[seed + 2; 32]`), valid from 1 000 ns
    /// for its longest lifetime.
    fn make_stand_in(seed: u8) -> (ed25519_dalek::SigningKey, DeviceCert) {
        let root = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[seed.wrapping_add(2); 32]);
        let mut cert = prepare_stand_in_cert(
            root.verifying_key().to_bytes(),
            key.verifying_key().to_bytes(),
            "Stand-in on tilo.example",
            1_000,
            1_000 + STAND_IN_LIFETIME_NS,
        );
        sign_device_cert(&mut cert, &root).unwrap();
        (key, cert)
    }

    /// An unsigned member certificate `issuer` issues for device `[9; 32]` at
    /// `issued_at_ns`, lasting a browser's lifetime.
    fn issued_by(issuer: &DeviceCert, issued_at_ns: u64) -> DeviceCert {
        DeviceCert {
            user_public_key: issuer.user_public_key,
            device_public_key: [9; 32],
            device_label: "Browser".to_owned(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns,
            expires_at_ns: Some(
                issued_at_ns.saturating_add(crate::CertificationPolicy::browser().lifetime_ns),
            ),
            signature: [0; 64],
            issuer: Some(Box::new(issuer.clone())),
        }
    }

    /// `cert` signed by `key` under the digest a signer that skips every
    /// shape check would sign.
    fn forced(mut cert: DeviceCert, key: &ed25519_dalek::SigningKey) -> DeviceCert {
        cert.signature = key
            .sign(&device_cert_signing_digest(&cert).unwrap())
            .to_bytes();
        cert
    }

    /// A stand-in's certificate verifies as one, is no device, carries
    /// nothing besides being a stand-in, and always expires within 30 days.
    #[test]
    fn a_stand_in_is_certified_by_its_root_and_is_no_device() {
        let (key, cert) = make_stand_in(1);
        verify_stand_in_cert(&cert).unwrap();
        verify_stand_in_cert_at(&cert, 2_000).unwrap();
        assert_eq!(
            verify_stand_in_cert_at(&cert, 1_000 + STAND_IN_LIFETIME_NS),
            Err(DeviceCertError::Expired)
        );
        assert_eq!(
            verify_device_cert(&cert),
            Err(DeviceCertError::StandInIsNoDevice)
        );
        assert_eq!(
            verify_device_cert_at(&cert, 2_000),
            Err(DeviceCertError::StandInIsNoDevice)
        );
        assert_eq!(
            device_cert_signing_digest(&cert).unwrap(),
            signing_digest(DEVCERT_V5_PREIMAGE_TAG, &cert.view()).unwrap()
        );
        // A device's certificate is no stand-in's.
        assert_eq!(
            verify_stand_in_cert(&golden_cert(false)),
            Err(DeviceCertError::InvalidStandIn)
        );
        // Its marker does not come off under the same signature.
        let mut stripped = cert.clone();
        stripped.capabilities.stand_in = false;
        assert_eq!(
            verify_device_cert(&stripped),
            Err(DeviceCertError::BadSignature)
        );

        // It round-trips in both encodings, and verifiers that predate it
        // refuse it.
        let json = serde_json::to_string(&cert).unwrap();
        assert!(json.contains(r#""stand_in":true"#));
        assert_eq!(serde_json::from_str::<DeviceCert>(&json).unwrap(), cert);
        assert!(serde_json::from_str::<v1::DeviceCert>(&json).is_err());
        let wire = postcard::to_allocvec(&(&cert, [9u8; 32])).unwrap();
        let (back, nonce): (DeviceCert, [u8; 32]) = postcard::from_bytes(&wire).unwrap();
        assert_eq!((back, nonce), (cert.clone(), [9u8; 32]));
        assert!(postcard::from_bytes::<(v1::DeviceCert, [u8; 32])>(&wire).is_err());

        // Any other capability beside it, no expiry, a longer life, or its
        // root's own key is neither signed nor taken.
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let mut others = Vec::new();
        for widen in [
            |c: &mut DeviceCapabilities| c.can_own_workspaces = true,
            |c: &mut DeviceCapabilities| c.can_admit_devices = true,
            |c: &mut DeviceCapabilities| c.can_act_as_host = true,
            |c: &mut DeviceCapabilities| c.can_read_all_workspaces = true,
            |c: &mut DeviceCapabilities| c.foreign = true,
            |c: &mut DeviceCapabilities| c.can_administer_devices = true,
            |c: &mut DeviceCapabilities| c.agent = Some(AgentScope::AllWorkspaces),
        ] {
            let mut widened = cert.clone();
            widen(&mut widened.capabilities);
            others.push(widened);
        }
        let mut forever = cert.clone();
        forever.expires_at_ns = None;
        let mut longer = cert.clone();
        longer.expires_at_ns = Some(1_001 + STAND_IN_LIFETIME_NS);
        let mut itself = cert.clone();
        itself.device_public_key = cert.user_public_key;
        others.extend([forever, longer, itself]);
        for mut other in others {
            assert!(sign_device_cert(&mut other, &root).is_err(), "{other:?}");
            let other = forced(other, &root);
            assert!(verify_stand_in_cert(&other).is_err(), "{other:?}");
            assert!(verify_device_cert(&other).is_err(), "{other:?}");
        }

        // A stand-in certifies no other stand-in.
        let mut second = prepare_stand_in_cert(
            cert.user_public_key,
            [8; 32],
            "Another stand-in",
            2_000,
            2_000 + DAY_NS,
        );
        second.issuer = Some(Box::new(cert));
        assert!(sign_device_cert(&mut second, &key).is_err());
        let second = forced(second, &key);
        assert!(verify_stand_in_cert(&second).is_err());
        assert!(verify_device_cert(&second).is_err());
    }

    /// A member certificate a stand-in issued verifies, binds its issuer, and
    /// round-trips; verifiers that predate it refuse it.
    #[test]
    fn a_stand_in_issues_member_certificates_bound_to_it() {
        let (key, stand_in) = make_stand_in(1);
        let mut cert = issued_by(&stand_in, 2_000);
        sign_device_cert(&mut cert, &key).unwrap();
        verify_device_cert(&cert).unwrap();
        verify_device_cert_at(&cert, 3_000).unwrap();
        // Devices it certified outlive it.
        verify_device_cert_at(&cert, 1_000 + STAND_IN_LIFETIME_NS + 500).unwrap();
        assert_eq!(cert.signer_public_key(), stand_in.device_public_key);
        assert_eq!(
            device_cert_signing_digest(&cert).unwrap(),
            signing_digest(
                DEVCERT_V6_PREIMAGE_TAG,
                &IssuedCertView {
                    cert: cert.view(),
                    issuer: &stand_in,
                }
            )
            .unwrap()
        );
        // A browser's certificate a stand-in issued answers its request as
        // the browser policy allows.
        let mut narrower = issued_by(&stand_in, 2_000);
        narrower.capabilities.foreign = true;
        narrower.expires_at_ns = Some(2_000 + DAY_NS);
        sign_device_cert(&mut narrower, &key).unwrap();
        verify_device_cert(&narrower).unwrap();

        let json = serde_json::to_string(&cert).unwrap();
        assert!(json.contains(r#""issuer":{"#));
        assert_eq!(serde_json::from_str::<DeviceCert>(&json).unwrap(), cert);
        assert!(serde_json::from_str::<v1::DeviceCert>(&json).is_err());
        let wire = postcard::to_allocvec(&(&cert, [9u8; 32])).unwrap();
        let (back, nonce): (DeviceCert, [u8; 32]) = postcard::from_bytes(&wire).unwrap();
        assert_eq!((back, nonce), (cert.clone(), [9u8; 32]));
        assert!(postcard::from_bytes::<(v1::DeviceCert, [u8; 32])>(&wire).is_err());
        // Capabilities alone name no issuer.
        let caps = &wire[32 + 32 + 1 + "Browser".len()..];
        assert_eq!(caps[0], EXTENDED_BINARY_MARKER);
        assert!(postcard::from_bytes::<DeviceCapabilities>(caps).is_err());

        // The issuer is bound: another certificate of the same stand-in key,
        // or a changed one, breaks the signature.
        let mut moved = cert.clone();
        let mut renewed = stand_in;
        renewed.issued_at_ns = 1_500;
        renewed = forced(renewed, &ed25519_dalek::SigningKey::from_bytes(&[1; 32]));
        verify_stand_in_cert(&renewed).unwrap();
        moved.issuer = Some(Box::new(renewed));
        assert_eq!(
            verify_device_cert(&moved),
            Err(DeviceCertError::BadSignature)
        );
        let mut tampered = cert.clone();
        if let Some(issuer) = tampered.issuer.as_mut() {
            issuer.signature[0] ^= 1;
        }
        assert_eq!(
            verify_device_cert(&tampered),
            Err(DeviceCertError::BadSignature)
        );
        // Without its issuer it is a root certificate the root never signed.
        let mut orphan = cert;
        orphan.issuer = None;
        assert_eq!(
            verify_device_cert(&orphan),
            Err(DeviceCertError::BadSignature)
        );
    }

    /// A stand-in issues no more than a member for a browser's lifetime, for
    /// its own person, while it is valid, with its own key; and nothing it
    /// issued can issue in turn.
    #[test]
    fn a_stand_in_issues_nothing_beyond_its_bounds() {
        let (key, stand_in) = make_stand_in(1);
        let (other_key, other_person) = make_stand_in(11);
        let golden_device;
        let mut refused: Vec<(DeviceCert, &ed25519_dalek::SigningKey)> = Vec::new();

        let mut more = issued_by(&stand_in, 2_000);
        more.capabilities = DeviceCapabilities::primary();
        refused.push((more, &key));
        for caps in [
            DeviceCapabilities::host(),
            DeviceCapabilities::administrator(),
            DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
            DeviceCapabilities::stand_in_approver(),
        ] {
            let mut cert = issued_by(&stand_in, 2_000);
            cert.capabilities = caps;
            refused.push((cert, &key));
        }
        let mut longer = issued_by(&stand_in, 2_000);
        longer.expires_at_ns = longer.expires_at_ns.map(|expiry| expiry + 1);
        refused.push((longer, &key));
        let mut forever = issued_by(&stand_in, 2_000);
        forever.expires_at_ns = None;
        refused.push((forever, &key));
        // Another person's device, under this person's stand-in.
        let mut foreign_person = issued_by(&stand_in, 2_000);
        foreign_person.user_public_key = other_person.user_public_key;
        refused.push((foreign_person, &key));
        // This person's device, under another person's stand-in.
        let mut other_stand_in = issued_by(&other_person, 2_000);
        other_stand_in.user_public_key = stand_in.user_public_key;
        refused.push((other_stand_in, &other_key));
        // Before the stand-in was certified, and once it expired.
        refused.push((issued_by(&stand_in, 999), &key));
        refused.push((issued_by(&stand_in, 1_000 + STAND_IN_LIFETIME_NS), &key));
        // Its own key, or the root's, as the device.
        let mut own = issued_by(&stand_in, 2_000);
        own.device_public_key = stand_in.device_public_key;
        refused.push((own, &key));
        let mut root = issued_by(&stand_in, 2_000);
        root.device_public_key = stand_in.user_public_key;
        refused.push((root, &key));
        // A device it certified certifies nothing.
        let mut member = issued_by(&stand_in, 2_000);
        sign_device_cert(&mut member, &key).unwrap();
        let device_key = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let mut chained = issued_by(&member, 3_000);
        chained.device_public_key = [10; 32];
        refused.push((chained, &device_key));
        // A root-signed member is no stand-in either.
        let root_member = golden_cert(false);
        golden_device = ed25519_dalek::SigningKey::from_bytes(&[2; 32]);
        let mut under_member = issued_by(&root_member, 1_500_000);
        under_member.device_public_key = [10; 32];
        refused.push((under_member, &golden_device));

        for (cert, signer) in refused {
            let mut signed = cert.clone();
            assert!(sign_device_cert(&mut signed, signer).is_err(), "{cert:?}");
            let forced = forced(cert, signer);
            assert!(verify_device_cert(&forced).is_err(), "{forced:?}");
        }

        // Signed by any key but the stand-in's, it does not verify.
        let mut cert = issued_by(&stand_in, 2_000);
        let root_key = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        assert_eq!(
            sign_device_cert(&mut cert, &root_key),
            Err(DeviceCertError::BadSignature)
        );
        let by_root = forced(cert, &root_key);
        assert_eq!(
            verify_device_cert(&by_root),
            Err(DeviceCertError::BadSignature)
        );
    }

    /// A certificate whose issuer names an issuer of its own is refused by
    /// the binary decoder before it recurses, and by the JSON one.
    #[test]
    fn an_issuer_with_an_issuer_does_not_decode() {
        let (key, stand_in) = make_stand_in(1);
        let mut member = issued_by(&stand_in, 2_000);
        sign_device_cert(&mut member, &key).unwrap();
        let mut chained = issued_by(&member, 3_000);
        chained.device_public_key = [10; 32];
        let wire = postcard::to_allocvec(&chained).unwrap();
        assert!(postcard::from_bytes::<DeviceCert>(&wire).is_err());
        let json = serde_json::to_string(&chained).unwrap();
        assert!(serde_json::from_str::<DeviceCert>(&json).is_err());
    }

    /// Root `[1; 32]`'s all-workspaces grant (key `[4; 32]`) from 1 000 ns,
    /// lasting `lifetime`.
    fn make_grant(lifetime: u64) -> (ed25519_dalek::SigningKey, DeviceCert) {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "Visa backend".to_owned(),
            capabilities: DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
            issued_at_ns: 1_000,
            expires_at_ns: Some(lifetime.saturating_add(1_000)),
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root).unwrap();
        (key, cert)
    }

    /// An all-workspaces grant mints a grant for one workspace that verifies
    /// as one, under its issuer, and round-trips both encodings.
    #[test]
    fn a_grant_mints_a_narrower_grant() {
        let (key, grant) = make_grant(DAY_NS);
        let mut minted = prepare_minted_grant(&grant, [12; 32], [7; 16], "Jane Doe", 2_000, 3_000);
        sign_device_cert(&mut minted, &key).unwrap();
        verify_device_cert_at(&minted, 2_500).unwrap();
        assert_eq!(minted.signer_public_key(), grant.device_public_key);
        assert_eq!(
            minted.capabilities.agent,
            Some(AgentScope::Workspace([7; 16]))
        );
        let wire = postcard::to_allocvec(&minted).unwrap();
        assert_eq!(postcard::from_bytes::<DeviceCert>(&wire).unwrap(), minted);
        let json = serde_json::to_string(&minted).unwrap();
        assert_eq!(serde_json::from_str::<DeviceCert>(&json).unwrap(), minted);
        // The root's own signature does not stand in for the grant's.
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        assert_eq!(
            verify_device_cert(&forced(minted.clone(), &root)),
            Err(DeviceCertError::BadSignature)
        );
    }

    /// A minted grant never reaches more, lasts longer, or chains further
    /// than its issuer, and only an all-workspaces grant mints.
    #[test]
    fn a_minted_grant_stays_within_its_issuer() {
        let (key, grant) = make_grant(DAY_NS);
        let refused = |cert: DeviceCert| {
            assert_eq!(
                verify_device_cert(&forced(cert.clone(), &key)),
                Err(DeviceCertError::InvalidIssuer)
            );
            let mut signed = cert;
            assert_eq!(
                sign_device_cert(&mut signed, &key),
                Err(DeviceCertError::InvalidIssuer)
            );
        };
        let minted = || prepare_minted_grant(&grant, [12; 32], [7; 16], "Jane", 2_000, 3_000);
        // All workspaces again: wider than a mint may be.
        let mut wide = minted();
        wide.capabilities = DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces);
        refused(wide);
        // Anything besides the scope.
        let mut member = minted();
        member.capabilities = DeviceCapabilities::member();
        refused(member);
        // Past its issuer's expiry.
        let mut longer = minted();
        longer.expires_at_ns = Some(1_000 + DAY_NS + 1);
        refused(longer);
        // Before its issuer was valid.
        let mut early = minted();
        early.issued_at_ns = 500;
        refused(early);
        // The issuer's own key.
        let mut same = minted();
        same.device_public_key = grant.device_public_key;
        refused(same);
        // Another person's grant.
        let mut other = minted();
        other.user_public_key = [3; 32];
        refused(other);
        // A one-workspace grant mints nothing.
        let mut narrow = grant.clone();
        narrow.capabilities = DeviceCapabilities::agent_grant(AgentScope::Workspace([7; 16]));
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        sign_device_cert(&mut narrow, &root).unwrap();
        let mut from_narrow =
            prepare_minted_grant(&narrow, [12; 32], [7; 16], "Jane", 2_000, 3_000);
        assert_eq!(
            sign_device_cert(&mut from_narrow, &key),
            Err(DeviceCertError::InvalidIssuer)
        );
        // A minted grant mints nothing: the chain does not decode.
        let mut first = minted();
        sign_device_cert(&mut first, &key).unwrap();
        let chained = prepare_minted_grant(&first, [13; 32], [7; 16], "Jane", 2_500, 2_900);
        let encoded = postcard::to_allocvec(&chained).unwrap();
        assert!(postcard::from_bytes::<DeviceCert>(&encoded).is_err());
    }

    /// Revoking a grant refuses every grant it minted, whenever minted.
    #[test]
    fn revoking_a_grant_ends_what_it_minted() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let (key, grant) = make_grant(DAY_NS);
        let mut minted = prepare_minted_grant(&grant, [12; 32], [7; 16], "Jane", 2_000, 9_000);
        sign_device_cert(&mut minted, &key).unwrap();
        let mut entry = crate::RevocationEntry {
            user_public_key: grant.user_public_key,
            revoked_device_public_key: grant.device_public_key,
            revoked_at_ns: 5_000,
            reason: crate::RevocationReason::Retired,
            signature: [0; 64],
        };
        crate::sign_revocation_entry(&mut entry, &root).unwrap();
        let mut revocations = crate::RevocationList::new();
        assert!(!revocations.refuses(&minted));
        revocations.insert(entry).unwrap();
        assert!(revocations.refuses(&grant));
        assert!(revocations.refuses(&minted));
        assert!(revocations.refuses_at(&minted, 2_500));
    }

    /// Root `[1; 32]`'s server key (key `[4; 32]`) from 1 000 ns until
    /// `expires_at_ns`, signed.
    fn make_server_key(expires_at_ns: Option<u64>) -> (ed25519_dalek::SigningKey, DeviceCert) {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "Visa backend".to_owned(),
            capabilities: DeviceCapabilities::server_key_grant(),
            issued_at_ns: 1_000,
            expires_at_ns,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root).unwrap();
        (key, cert)
    }

    /// A server key reaches all workspaces and lasts until revoked, or until
    /// the expiry its person chose; its marker is in both encodings and under
    /// its signature, and verifiers that predate it refuse it. A grant
    /// without the marker still expires within the bound.
    #[test]
    fn a_server_key_lasts_until_revoked_or_its_chosen_expiry() {
        let capabilities = DeviceCapabilities::server_key_grant();
        assert_eq!(
            postcard::to_allocvec(&capabilities).unwrap(),
            [
                EXTENDED_BINARY_MARKER,
                EXTENDED_AGENT | 32,
                SCOPE_ALL,
                0,
                0,
                0,
                0
            ]
        );
        assert_eq!(
            serde_json::to_string(&capabilities).unwrap(),
            r#"{"can_own_workspaces":false,"can_admit_devices":false,"can_act_as_host":false,"can_read_all_workspaces":false,"agent":"all","server_key":true}"#
        );
        // A bit the decoder does not know is refused, as decoders before the
        // marker refuse the marker's.
        assert!(
            postcard::from_bytes::<DeviceCapabilities>(&[3, EXTENDED_AGENT | 64, 0, 0, 0, 0, 0])
                .is_err()
        );
        let lasting = [None, Some(1_000 + 10 * MAX_AGENT_GRANT_LIFETIME_NS)];
        for expiry in lasting {
            let (_, key) = make_server_key(expiry);
            verify_device_cert_at(&key, 1_000 + 5 * MAX_AGENT_GRANT_LIFETIME_NS).unwrap();
            let bytes = postcard::to_allocvec(&(&key, [9u8; 32])).unwrap();
            let (back, _): (DeviceCert, [u8; 32]) = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(back, key);
            assert!(postcard::from_bytes::<(v1::DeviceCert, [u8; 32])>(&bytes).is_err());
            let json = serde_json::to_string(&key).unwrap();
            assert_eq!(serde_json::from_str::<DeviceCert>(&json).unwrap(), key);
            assert!(serde_json::from_str::<v1::DeviceCert>(&json).is_err());
            // The marker is signed: the same signature on a plain grant fails.
            let mut plain = key.clone();
            plain.capabilities = DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces);
            plain.expires_at_ns = Some(2_000);
            assert_eq!(
                verify_device_cert(&plain),
                Err(DeviceCertError::BadSignature)
            );
        }
        let (_, chosen) = make_server_key(Some(5_000));
        verify_device_cert_at(&chosen, 4_999).unwrap();
        assert_eq!(
            verify_device_cert_at(&chosen, 5_000),
            Err(DeviceCertError::Expired)
        );

        // Only a grant reaching all workspaces is a server key, and a grant
        // without the marker (an agent's, through consent) still expires.
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        for capabilities in [
            DeviceCapabilities {
                server_key: true,
                ..DeviceCapabilities::agent_grant(AgentScope::Workspace([7; 16]))
            },
            DeviceCapabilities {
                agent: None,
                ..DeviceCapabilities::server_key_grant()
            },
            DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
        ] {
            let mut cert = chosen.clone();
            cert.capabilities = capabilities;
            cert.expires_at_ns = None;
            assert_eq!(
                sign_device_cert(&mut cert, &root),
                Err(DeviceCertError::InvalidAgentGrant)
            );
            assert_eq!(
                verify_device_cert(&forced(cert, &root)),
                Err(DeviceCertError::InvalidAgentGrant)
            );
        }
    }

    /// A server key that never expires mints keys for one workspace that
    /// expire within the bound, and revoking it ends them.
    #[test]
    fn a_server_key_mints_keys_that_expire_and_end_with_it() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let (key, server) = make_server_key(None);
        let mint = |expires_at_ns| {
            prepare_minted_grant(&server, [12; 32], [7; 16], "Jane", 2_000, expires_at_ns)
        };
        let mut minted = mint(2_000 + MAX_AGENT_GRANT_LIFETIME_NS);
        sign_device_cert(&mut minted, &key).unwrap();
        verify_device_cert_at(&minted, 3_000).unwrap();
        assert_eq!(
            minted.capabilities.agent,
            Some(AgentScope::Workspace([7; 16]))
        );
        assert!(!minted.capabilities.server_key);
        let mut forever = mint(3_000);
        forever.expires_at_ns = None;
        let mut longer = mint(2_001 + MAX_AGENT_GRANT_LIFETIME_NS);
        for mut refused in [forever, longer.clone()] {
            assert_eq!(
                sign_device_cert(&mut refused, &key),
                Err(DeviceCertError::InvalidAgentGrant)
            );
        }
        longer.capabilities.server_key = true;
        assert!(sign_device_cert(&mut longer, &key).is_err());

        let mut entry = crate::RevocationEntry {
            user_public_key: server.user_public_key,
            revoked_device_public_key: server.device_public_key,
            revoked_at_ns: 5_000,
            reason: crate::RevocationReason::Retired,
            signature: [0; 64],
        };
        crate::sign_revocation_entry(&mut entry, &root).unwrap();
        let mut revocations = crate::RevocationList::new();
        revocations.insert(entry).unwrap();
        assert!(revocations.refuses(&server));
        assert!(revocations.refuses_at(&minted, 2_500));
    }

    /// Revoking a stand-in refuses what it issues from then on and keeps
    /// what it issued before.
    #[test]
    fn revoking_a_stand_in_keeps_what_it_issued_before() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let (key, stand_in) = make_stand_in(1);
        let mut entry = crate::RevocationEntry {
            user_public_key: stand_in.user_public_key,
            revoked_device_public_key: stand_in.device_public_key,
            revoked_at_ns: 5_000,
            reason: crate::RevocationReason::Retired,
            signature: [0; 64],
        };
        crate::sign_revocation_entry(&mut entry, &root).unwrap();
        let mut revocations = crate::RevocationList::new();
        revocations.insert(entry).unwrap();

        let mut before = issued_by(&stand_in, 4_999);
        sign_device_cert(&mut before, &key).unwrap();
        let mut after = issued_by(&stand_in, 5_000);
        sign_device_cert(&mut after, &key).unwrap();
        verify_device_cert(&before).unwrap();
        verify_device_cert(&after).unwrap();
        assert!(!revocations.refuses(&before));
        assert!(!revocations.refuses_at(&before, 10_000));
        assert!(revocations.refuses(&after));
        assert!(revocations.refuses_at(&after, 10_000));
        assert!(revocations.refuses(&stand_in));
        assert!(!revocations.refuses_at(&stand_in, 4_999));
        // The device's own revocation still counts.
        let mut device = crate::RevocationEntry {
            user_public_key: stand_in.user_public_key,
            revoked_device_public_key: before.device_public_key,
            revoked_at_ns: 20_000,
            reason: crate::RevocationReason::Lost,
            signature: [0; 64],
        };
        crate::sign_revocation_entry(&mut device, &root).unwrap();
        revocations.insert(device).unwrap();
        assert!(revocations.refuses(&before));
        assert!(!revocations.refuses_at(&before, 19_999));
    }

    #[test]
    fn verifiers_that_predate_the_marker_reject_foreign_certificates() {
        let native = golden_cert(false);
        let foreign = golden_cert(true);
        let nonce = [9u8; 32];

        let old: v1::DeviceCert =
            serde_json::from_str(&serde_json::to_string(&native).unwrap()).unwrap();
        assert_eq!(serde_json::to_string(&old).unwrap(), GOLDEN_V1_JSON);
        let old_wire = postcard::to_allocvec(&(&old, nonce)).unwrap();
        assert_eq!(postcard::to_allocvec(&(&native, nonce)).unwrap(), old_wire);
        let (_, old_nonce): (DeviceCert, [u8; 32]) = postcard::from_bytes(&old_wire).unwrap();
        assert_eq!(old_nonce, nonce);

        let foreign_json = serde_json::to_string(&foreign).unwrap();
        let json_error = serde_json::from_str::<v1::DeviceCert>(&foreign_json).unwrap_err();
        assert!(json_error.to_string().contains("unknown field `foreign`"));
        // Embedded in a larger message, the old decoder stops at the marker
        // instead of misreading the fields that follow.
        let foreign_wire = postcard::to_allocvec(&(&foreign, nonce)).unwrap();
        assert!(postcard::from_bytes::<(v1::DeviceCert, [u8; 32])>(&foreign_wire).is_err());
    }

    /// Administration signs under its own tag, round-trips in both encodings
    /// (alone and together with the foreign marker), survives no flip of the
    /// flag, and is refused by every verifier that predates it.
    #[test]
    fn administration_certificates_sign_under_their_own_tag_and_old_verifiers_refuse_them() {
        let native = golden_cert(false);
        let nonce = [9u8; 32];
        for foreign in [false, true] {
            let admin = golden_cert_with(DeviceCapabilities {
                foreign,
                ..DeviceCapabilities::administrator()
            });
            verify_device_cert(&admin).unwrap();
            assert_eq!(
                device_cert_signing_digest(&admin).unwrap(),
                signing_digest(DEVCERT_V3_PREIMAGE_TAG, &admin.view()).unwrap()
            );
            let json = serde_json::to_string(&admin).unwrap();
            assert!(json.contains(r#""can_administer_devices":true"#));
            assert_eq!(serde_json::from_str::<DeviceCert>(&json).unwrap(), admin);
            let bytes = crate::canonical::canonical_bytes(&admin).unwrap();
            let marker = 32 + 32 + 1 + "golden".len();
            assert_eq!(bytes[marker], EXTENDED_BINARY_MARKER);
            assert_eq!(
                crate::canonical::from_canonical_bytes::<DeviceCert>(&bytes).unwrap(),
                admin
            );

            let mut stripped = admin.clone();
            stripped.capabilities.can_administer_devices = false;
            assert_eq!(
                verify_device_cert(&stripped).unwrap_err(),
                DeviceCertError::BadSignature
            );

            let json_error = serde_json::from_str::<v1::DeviceCert>(&json).unwrap_err();
            assert!(
                json_error.to_string().contains("unknown field"),
                "{json_error}"
            );
            let wire = postcard::to_allocvec(&(&admin, nonce)).unwrap();
            assert!(postcard::from_bytes::<(v1::DeviceCert, [u8; 32])>(&wire).is_err());
        }
        let mut granted = native;
        granted.capabilities.can_administer_devices = true;
        assert_eq!(
            verify_device_cert(&granted).unwrap_err(),
            DeviceCertError::BadSignature
        );
    }

    #[test]
    fn extended_capabilities_refuse_bytes_that_do_not_grant_administration() {
        let admin = golden_cert_with(DeviceCapabilities::administrator());
        let bytes = crate::canonical::canonical_bytes(&admin).unwrap();
        let marker = 32 + 32 + 1 + "golden".len();
        assert_eq!(bytes[marker + 1], EXTENDED_ADMINISTER);
        for extended in [0, EXTENDED_FOREIGN, 4, 0x80] {
            let mut changed = bytes.clone();
            changed[marker + 1] = extended;
            assert!(
                crate::canonical::from_canonical_bytes::<DeviceCert>(&changed).is_err(),
                "extended byte {extended}"
            );
        }
    }

    #[test]
    fn binary_capabilities_refuse_flag_bytes_other_than_zero_or_one() {
        let mut bytes = crate::canonical::canonical_bytes(&golden_cert(true)).unwrap();
        let marker = 32 + 32 + 1 + "golden".len();
        assert_eq!(bytes[marker], FOREIGN_BINARY_MARKER);
        bytes[marker + 1] = 3;
        assert!(crate::canonical::from_canonical_bytes::<DeviceCert>(&bytes).is_err());
    }

    #[test]
    fn remembered_verification_never_accepts_a_changed_certificate() {
        let (_root, cert) = fresh_cert(DeviceCapabilities::primary());
        // Verify twice: the second check is answered by the remembered triple.
        verify_device_cert(&cert).unwrap();
        verify_device_cert(&cert).unwrap();
        for tamper in [0usize, 1, 2, 3] {
            let mut changed = cert.clone();
            match tamper {
                0 => changed.signature[5] ^= 0x01,
                1 => changed.device_public_key[3] ^= 0x01,
                2 => changed.issued_at_ns ^= 1,
                _ => changed.user_public_key[7] ^= 0x01,
            }
            assert!(verify_device_cert(&changed).is_err(), "tamper {tamper}");
        }
        verify_device_cert(&cert).unwrap();
    }
}

#[cfg(test)]
mod device_name_tests {
    use super::{UNNAMED_PRIMARY_DEVICE, UNNAMED_SHARED_DEVICE, device_name};

    #[test]
    fn a_name_the_person_gave_is_the_name() {
        assert_eq!(device_name("  Studio phone "), Some("Studio phone"));
        assert_eq!(device_name("Laptop"), Some("Laptop"));
    }

    #[test]
    fn the_stand_in_provisioning_fills_in_is_no_name() {
        assert_eq!(device_name(UNNAMED_PRIMARY_DEVICE), None);
        assert_eq!(device_name(UNNAMED_SHARED_DEVICE), None);
        assert_eq!(device_name("   "), None);
        assert_eq!(device_name(""), None);
    }
}
