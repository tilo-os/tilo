//! The durable peer base a build before the owner-root layout wrote: it
//! names the workspace's governing device by its certificate where this build
//! names the owner's root key. Read here, never written: a reader of such a
//! state takes the certificate's root key as the owner, which is whom the
//! certificate certified the device to act for, and the state opens as the
//! one this build keeps.
//!
//! The envelopes around it (4, 5 and 6) did not change, so each is read with
//! this base where the current shape names [`Durable`].

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};

use super::DurableDocument;
use crate::state::SealedHistory;
use neighbour_wire::NeighbourMsg;
use tilo_user_identity::DeviceCert;

/// A state a build before [`DURABLE_BASE_VERSION`] wrote: the same base, the
/// governing device where this build names the owner's root key. Its
/// envelopes (v4, v5 and v6) are otherwise the ones above, so each is read
/// through the same shape around this base.
#[derive(Serialize, Deserialize)]
pub(super) struct DurableV3 {
    pub(super) version: u32,
    pub(super) room: String,
    pub(super) authority: DeviceCert,
    pub(super) room_bytes: Option<Vec<u8>>,
    pub(super) documents: Vec<DurableDocument>,
    pub(super) receipts: BTreeMap<String, Vec<u8>>,
    pub(super) certificates: Vec<DeviceCert>,
    pub(super) revocations: Vec<tilo_user_identity::RevocationEntry>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct DurableV4BeforeOwner {
    pub(super) version: u32,
    pub(super) base: DurableV3,
    pub(super) pending: VecDeque<([u8; 32], NeighbourMsg, bool)>,
    pub(super) pending_bytes: usize,
}

#[derive(Serialize, Deserialize)]
pub(super) struct DurableV5BeforeOwner<'a> {
    pub(super) version: u32,
    pub(super) base: DurableV4BeforeOwner,
    #[serde(borrow)]
    pub(super) exact: &'a [u8],
}

#[derive(Serialize, Deserialize)]
pub(super) struct DurableV6BeforeOwner<'a> {
    pub(super) version: u32,
    pub(super) base: DurableV4BeforeOwner,
    #[serde(borrow)]
    pub(super) exact: &'a [u8],
    pub(super) sealed: SealedHistory,
}

impl DurableV3 {
    /// This base as the layout this build writes, `owner` being the root key
    /// of the device that governed it.
    pub(super) fn into_current(self, owner: [u8; 32]) -> Durable {
        Durable {
            version: DURABLE_BASE_VERSION,
            workspace: self.room,
            owner,
            workspace_bytes: self.room_bytes,
            documents: self.documents,
            receipts: self.receipts,
            certificates: self.certificates,
            revocations: self.revocations,
        }
    }
}

/// What reading an earlier build's state answers: the base this build keeps,
/// its pending queue and that queue's size, its exact evidence and its sealed
/// history.
type Opened = (
    Durable,
    VecDeque<([u8; 32], NeighbourMsg, bool)>,
    usize,
    super::ExactAdmissions,
    SealedHistory,
);

/// A version 4 state an earlier build wrote, read as the base this build
/// keeps: its pending queue, and no exact evidence or sealed history, which
/// those envelopes did not have.
pub(super) fn v4(bytes: &[u8]) -> Result<Opened, String> {
    let value: DurableV4BeforeOwner =
        postcard::from_bytes(bytes).map_err(|error| error.to_string())?;
    if value.base.version != 3 {
        return Err(format!(
            "unsupported durable base version {}",
            value.base.version
        ));
    }
    let owner = value.base.authority.user_public_key;
    Ok((
        value.base.into_current(owner),
        value.pending,
        value.pending_bytes,
        super::ExactAdmissions::default(),
        Vec::new(),
    ))
}

/// A version 5 state an earlier build wrote, with its exact evidence.
pub(super) fn v5(bytes: &[u8]) -> Result<Opened, String> {
    let (value, tail): (DurableV5BeforeOwner<'_>, _) =
        postcard::take_from_bytes(bytes).map_err(|error| error.to_string())?;
    if !tail.is_empty()
        || value.version != 5
        || value.base.version != 4
        || value.base.base.version != 3
    {
        return Err("invalid native v5 durable envelope".into());
    }
    let exact = super::ExactAdmissions::decode(value.exact).map_err(|error| error.to_string())?;
    let owner = value.base.base.authority.user_public_key;
    Ok((
        value.base.base.into_current(owner),
        value.base.pending,
        value.base.pending_bytes,
        exact,
        Vec::new(),
    ))
}

/// A version 6 state an earlier build wrote, with its exact evidence and the
/// sealed writes of each encrypted record.
pub(super) fn v6(bytes: &[u8]) -> Result<Opened, String> {
    let (value, tail): (DurableV6BeforeOwner<'_>, _) =
        postcard::take_from_bytes(bytes).map_err(|error| error.to_string())?;
    if !tail.is_empty()
        || value.version != 6
        || value.base.version != 4
        || value.base.base.version != 3
        || value.sealed.is_empty()
    {
        return Err("invalid native v6 durable envelope".into());
    }
    let exact = super::ExactAdmissions::decode(value.exact).map_err(|error| error.to_string())?;
    let owner = value.base.base.authority.user_public_key;
    Ok((
        value.base.base.into_current(owner),
        value.base.pending,
        value.base.pending_bytes,
        exact,
        value.sealed,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_files::native::migration::assert_golden_bytes;
    use tilo_user_identity::DeviceCapabilities;

    /// A device certificate, signed by `root`.
    fn cert(root: &ed25519_dalek::SigningKey, seed: u8) -> DeviceCert {
        let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "lounge room host".into(),
            capabilities: DeviceCapabilities::host(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    /// The workspace the kept states are of.
    const KEPT_WORKSPACE: &str = "302d59e0-f586-42e1-a59e-3aa613f0f866";

    /// The state this build writes for the kept workspace, hex, kept so the
    /// layout cannot change under the version devices already hold. It is
    /// [`kept_state`] as `postcard` wrote it.
    const KEPT: &str = "04052433303264353965302d663538362d343265312d613539652d33616136313366306638363601010101010101010101010101010101010101010101010101010101010101010104856f4a83000000000107070707070707070707070707070707070707070707070707070707070707070a0105010c";

    /// [`KEPT`] as a build before the compact governance layout wrote it:
    /// base version 4, read as this build's base with its governance
    /// brought up ([`crate::workspace::layout`]).
    const KEPT_LAYOUT_BEFORE: &str = "04042433303264353965302d663538362d343265312d613539652d33616136313366306638363601010101010101010101010101010101010101010101010101010101010101010104856f4a83000000000107070707070707070707070707070707070707070707070707070707070707070a0105010c";

    /// The same state as a build before the owner root wrote it, hex: the
    /// governing device's certificate where this build names the owner's key,
    /// with that certificate's root as the owner.
    const KEPT_BEFORE_OWNER: &str = "04032433303264353965302d663538362d343265312d613539652d3361613631336630663836368a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394106c6f756e676520726f6f6d20686f73740000010101004048a78b5536b7a22f7a66cc88614c4d63ed508deb3bb8096ba692949dda1b96f1efe9f37b768745a6f4b4f5cfb1ecb302821d4e28fca992957661212609627a060104856f4a83000000000107070707070707070707070707070707070707070707070707070707070707070a0105010c";

    /// Bytes kept as hex, as a test reads them.
    fn kept(hex: &str) -> Vec<u8> {
        hex::decode(hex).unwrap()
    }

    /// The durable state this build writes with an exact index and a sealed
    /// history, hex, kept so the layout cannot change under the version
    /// devices already hold. It is [`kept_v6`] as `postcard` wrote it.
    const KEPT_SEALED: &str = "0604052433303264353965302d663538362d343265312d613539652d33616136313366306638363601010101010101010101010101010101010101010101010101010101010101010104856f4a83000000000107070707070707070707070707070707070707070707070707070707070707070a0105010c7d00012433303264353965302d663538362d343265312d613539652d3361613631336630663836360c736f2e74696c6f2e63686174066c6f756e6765030303030303030303030303030303030303030303030303030303030303030304050505050505050505050505050505050505050505050505050505050505050500010c736f2e74696c6f2e63686174066c6f756e676501067365616c6564100000000000000000000000000000000703030303030303030303030303030303030303030303030303030303030303031000000000000000000000000000000008000000040000004000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";

    /// One admission the peer settled exactly.
    fn kept_exact() -> Vec<u8> {
        let mut index = crate::admission::ExactAdmissions::default();
        index
            .insert(&crate::admission::DocumentAdmission {
                key: crate::admission::AdmissionKey {
                    workspace: KEPT_WORKSPACE.into(),
                    app: "so.tilo.chat".into(),
                    record: "lounge".into(),
                    author: [3; 32],
                    sequence: 4,
                },
                operation_id: [5; 32],
            })
            .unwrap();
        index.encode().unwrap()
    }

    /// One sealed write of one record.
    fn kept_sealed() -> SealedHistory {
        vec![(
            super::super::Route {
                app: "so.tilo.chat".into(),
                record: "lounge".into(),
            },
            vec![collab_types::SignedOp::new_unsigned(
                b"sealed".to_vec(),
                collab_types::ArtifactId(uuid::Uuid::from_u128(7)),
                collab_types::Ed25519PublicKey([3; 32]),
                collab_types::AttestationId(uuid::Uuid::from_u128(8)),
                collab_types::OpKind::Change,
                std::time::SystemTime::UNIX_EPOCH,
                4,
            )],
        )]
    }

    fn kept_v6(exact: &[u8]) -> Vec<u8> {
        postcard::to_stdvec(&DurableV6 {
            version: 6,
            base: kept_state(),
            exact,
            sealed: kept_sealed(),
        })
        .unwrap()
    }

    #[test]
    fn a_durable_state_with_a_sealed_history_kept_here_reads_and_writes_as_it_is() {
        let exact = kept_exact();
        assert_golden_bytes(
            "a peer's durable state with a sealed history",
            &kept(KEPT_SEALED),
            |bytes| {
                let (value, tail): (DurableV6<'_>, _) =
                    postcard::take_from_bytes(bytes).expect("the kept state reads");
                assert!(tail.is_empty(), "the kept state is all of the file");
                assert_eq!(value.version, 6);
                assert_eq!(value.base.base.workspace, KEPT_WORKSPACE);
                let index = crate::admission::ExactAdmissions::decode(value.exact).unwrap();
                assert_eq!(index.len(), 1);
                assert_eq!(value.sealed.len(), 1);
                assert_eq!(value.sealed[0].0.app, "so.tilo.chat");
                assert_eq!(value.sealed[0].1.len(), 1);
            },
            || kept_v6(&exact),
        );
    }

    /// One write waiting to be sent, in both kept states.
    fn kept_pending() -> VecDeque<([u8; 32], NeighbourMsg, bool)> {
        VecDeque::from([(
            [7; 32],
            neighbour_wire::NeighbourMsg::Heartbeat { peer: 1, ts: 5 },
            true,
        )])
    }

    /// The state this build writes for the kept workspace: nothing published
    /// yet, one write waiting.
    fn kept_state() -> DurableV4 {
        DurableV4 {
            version: 4,
            base: Durable {
                version: DURABLE_BASE_VERSION,
                workspace: KEPT_WORKSPACE.into(),
                owner: [1; 32],
                workspace_bytes: Some(b"\x85\x6f\x4a\x83".to_vec()),
                documents: Vec::new(),
                receipts: BTreeMap::new(),
                certificates: Vec::new(),
                revocations: Vec::new(),
            },
            pending: kept_pending(),
            pending_bytes: 12,
        }
    }

    /// The same state as a build before the owner root wrote it.
    fn kept_before_owner() -> DurableV4BeforeOwner {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        DurableV4BeforeOwner {
            version: 4,
            base: DurableV3 {
                version: 3,
                room: KEPT_WORKSPACE.into(),
                authority: cert(&root, 2),
                room_bytes: Some(b"\x85\x6f\x4a\x83".to_vec()),
                documents: Vec::new(),
                receipts: BTreeMap::new(),
                certificates: Vec::new(),
                revocations: Vec::new(),
            },
            pending: kept_pending(),
            pending_bytes: 12,
        }
    }

    /// A state this build writes reads back the way the bytes kept beside it
    /// do, and writes them again exactly: a field of the base or of an
    /// envelope added, removed or reordered changes these bytes, and every
    /// device's stored state would stop opening.
    #[test]
    fn the_state_kept_here_reads_and_writes_as_it_is() {
        assert_golden_bytes(
            "the durable admission state",
            &kept(KEPT),
            |bytes| {
                assert_ne!(durable_base_version(bytes).unwrap(), BASE_BEFORE_OWNER);
                let (value, tail): (DurableV4, _) = postcard::take_from_bytes(bytes).unwrap();
                assert!(tail.is_empty());
                assert_eq!(value.base.version, DURABLE_BASE_VERSION);
                assert_eq!(value.base.workspace, KEPT_WORKSPACE);
                assert_eq!(value.base.owner, [1; 32]);
                assert_eq!(value.pending_bytes, 12);
            },
            || postcard::to_stdvec(&kept_state()).unwrap(),
        );
    }

    /// A state a build before the compact governance layout wrote names that
    /// base, and reads in this build's base fields: the restore path brings
    /// its governance up.
    #[test]
    fn the_state_kept_before_the_compact_governance_reads_as_this_base() {
        let bytes = kept(KEPT_LAYOUT_BEFORE);
        assert_eq!(
            durable_base_version(&bytes).unwrap(),
            GOVERNANCE_LAYOUT_BEFORE
        );
        let (value, tail): (DurableV4, _) = postcard::take_from_bytes(&bytes).unwrap();
        assert!(tail.is_empty());
        assert_eq!(value.base.version, GOVERNANCE_LAYOUT_BEFORE);
        assert_eq!(value.base.workspace, KEPT_WORKSPACE);
        assert_eq!(value.pending_bytes, 12);
    }

    /// The state a build before the owner root wrote reads as this build's,
    /// through the reader the restore path uses, with the certificate's root
    /// as the owner.
    #[test]
    fn the_state_kept_from_before_the_owner_root_names_its_owner() {
        assert_golden_bytes(
            "the durable admission state before the owner root",
            &kept(KEPT_BEFORE_OWNER),
            |bytes| {
                assert_eq!(durable_base_version(bytes).unwrap(), BASE_BEFORE_OWNER);
                let (durable, pending, pending_bytes, _exact, sealed) = v4(bytes).unwrap();
                assert_eq!(durable.version, DURABLE_BASE_VERSION);
                assert_eq!(
                    durable.owner,
                    ed25519_dalek::SigningKey::from_bytes(&[1; 32])
                        .verifying_key()
                        .to_bytes()
                );
                assert_eq!(durable.workspace, KEPT_WORKSPACE);
                assert_eq!(pending.len(), 1);
                assert_eq!(pending_bytes, 12);
                assert!(sealed.is_empty());
            },
            || postcard::to_stdvec(&kept_before_owner()).unwrap(),
        );
    }

    /// A version 3 base names the governing device by its certificate; the
    /// reader takes that certificate's root as the owner, which is whom the
    /// certificate certified the device to act for.
    #[test]
    fn an_older_base_reads_as_the_owner_this_build_names() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let authority = cert(&root, 2);
        let durable = DurableV3 {
            version: 3,
            room: KEPT_WORKSPACE.into(),
            authority: authority.clone(),
            room_bytes: None,
            documents: Vec::new(),
            receipts: BTreeMap::new(),
            certificates: Vec::new(),
            revocations: Vec::new(),
        };
        let current = durable.into_current(authority.user_public_key);
        assert_eq!(current.version, DURABLE_BASE_VERSION);
        assert_eq!(current.workspace, KEPT_WORKSPACE);
        assert_eq!(current.owner, root.verifying_key().to_bytes());
    }

    /// A version 4 envelope this module reads comes back as this build's
    /// base with its pending queue, and its own version first.
    #[test]
    fn an_older_version_four_state_reads_with_its_pending() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let bytes = postcard::to_stdvec(&kept_before_owner()).unwrap();
        let (durable, pending, pending_bytes, _exact, sealed) = v4(&bytes).unwrap();
        assert_eq!(durable.version, DURABLE_BASE_VERSION);
        assert_eq!(durable.owner, root.verifying_key().to_bytes());
        assert_eq!(
            durable.workspace_bytes.as_deref(),
            Some(&b"\x85\x6f\x4a\x83"[..])
        );
        assert_eq!(pending.len(), 1);
        assert_eq!(pending_bytes, 12);
        assert!(sealed.is_empty());
    }

    /// The base of a state this build wrote is this build's, in every
    /// envelope: a version 4 envelope names it next, and a wrapping one
    /// after its own version. What follows a version 4 base is its
    /// workspace's name, whose length is no version.
    #[test]
    fn a_state_this_build_wrote_is_not_one_from_before_the_owner_root() {
        let v4 = kept_state();
        let v5 = DurableV5 {
            version: 5,
            base: DurableV4 {
                version: 4,
                base: kept_state().base,
                pending: VecDeque::new(),
                pending_bytes: 0,
            },
            exact: &[],
        };
        assert_ne!(
            durable_base_version(&postcard::to_stdvec(&v4).unwrap()).unwrap(),
            BASE_BEFORE_OWNER
        );
        assert_ne!(
            durable_base_version(&postcard::to_stdvec(&v5).unwrap()).unwrap(),
            BASE_BEFORE_OWNER
        );
    }

    /// A state a build before the owner root wrote is read as that layout,
    /// in the version 4 envelope and in the ones that wrap it.
    #[test]
    fn a_state_from_before_the_owner_root_is_read_as_that_layout() {
        let v5 = DurableV5BeforeOwner {
            version: 5,
            base: kept_before_owner(),
            exact: &[],
        };
        assert_eq!(
            durable_base_version(&postcard::to_stdvec(&kept_before_owner()).unwrap()).unwrap(),
            BASE_BEFORE_OWNER
        );
        assert_eq!(
            durable_base_version(&postcard::to_stdvec(&v5).unwrap()).unwrap(),
            BASE_BEFORE_OWNER
        );
    }
}

/// The version of a durable state's base, read from the versions that lead
/// the bytes, never by decoding a layout and retrying another.
///
/// How many versions lead the base's depends on the envelope: version 4
/// names its base's version second, and versions 5 and 6 wrap a version 4
/// envelope, which names it after its own.
pub fn durable_base_version(bytes: &[u8]) -> Result<u32, String> {
    let (envelope, rest) = version(bytes)?;
    let rest = match envelope {
        4 => rest,
        5 | 6 => version(rest)?.1,
        _ => return Err("unsupported durable admission version".into()),
    };
    Ok(version(rest)?.0)
}

/// The version of a durable state's base, read from the versions that lead
/// it.
fn version(bytes: &[u8]) -> Result<(u32, &[u8]), String> {
    postcard::take_from_bytes::<u32>(bytes).map_err(|error| error.to_string())
}

/// The base a build before the owner-root layout wrote ([`DurableV3`]).
pub(crate) const BASE_BEFORE_OWNER: u32 = 3;

/// The base whose governance holds device rows as JSON and each receipt
/// with its whole invitation: the same fields as [`DURABLE_BASE_VERSION`],
/// its governance brought up as it is read ([`crate::workspace::layout`]).
pub const GOVERNANCE_LAYOUT_BEFORE: u32 = 4;

/// The durable base layout. Version 5 keeps the governance's device rows
/// and receipts compact ([`crate::workspace::layout`]); version 4 is the same
/// base around the earlier governance layout, and version 3, which a build
/// before both wrote, named the governing device itself rather than the
/// owner's root key ([`durable_before_owner`]).
pub const DURABLE_BASE_VERSION: u32 = 5;

#[derive(Serialize, Deserialize)]
pub struct Durable {
    pub(super) version: u32,
    pub(super) workspace: String,
    pub(super) owner: [u8; 32],
    pub(super) workspace_bytes: Option<Vec<u8>>,
    pub(super) documents: Vec<DurableDocument>,
    pub(super) receipts: BTreeMap<String, Vec<u8>>,
    pub(super) certificates: Vec<DeviceCert>,
    pub(super) revocations: Vec<tilo_user_identity::RevocationEntry>,
}
#[derive(Serialize, Deserialize)]
pub struct DurableV4 {
    pub(super) version: u32,
    pub(super) base: Durable,
    pub(super) pending: VecDeque<([u8; 32], NeighbourMsg, bool)>,
    pub(super) pending_bytes: usize,
}

// The v4 shape stays frozen. Exact evidence is an explicitly selected addition.
#[derive(Serialize, Deserialize)]
pub struct DurableV5<'a> {
    pub(super) version: u32,
    pub(super) base: DurableV4,
    #[serde(borrow)]
    pub(super) exact: &'a [u8],
}

/// V5 and the sealed history of each encrypted record. Written only while a
/// record holds sealed writes, so every other state keeps its v5 bytes.
#[derive(Serialize, Deserialize)]
pub struct DurableV6<'a> {
    pub(super) version: u32,
    pub(super) base: DurableV4,
    #[serde(borrow)]
    pub(super) exact: &'a [u8],
    pub(super) sealed: SealedHistory,
}
