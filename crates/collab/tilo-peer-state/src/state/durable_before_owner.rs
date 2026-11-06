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
        };
        tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
        cert
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
            room: "302d59e0-f586-42e1-a59e-3aa613f0f866".into(),
            authority: authority.clone(),
            room_bytes: None,
            documents: Vec::new(),
            receipts: BTreeMap::new(),
            certificates: Vec::new(),
            revocations: Vec::new(),
        };
        let current = durable.into_current(authority.user_public_key);
        assert_eq!(current.version, DURABLE_BASE_VERSION);
        assert_eq!(current.workspace, "302d59e0-f586-42e1-a59e-3aa613f0f866");
        assert_eq!(current.owner, root.verifying_key().to_bytes());
    }

    /// A version 4 envelope this module reads comes back as this build's
    /// base with its pending queue, and its own version first.
    #[test]
    fn an_older_version_four_state_reads_with_its_pending() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let authority = cert(&root, 2);
        let bytes = postcard::to_stdvec(&DurableV4BeforeOwner {
            version: 4,
            base: DurableV3 {
                version: 3,
                room: "302d59e0-f586-42e1-a59e-3aa613f0f866".into(),
                authority: authority.clone(),
                room_bytes: Some(b"\x85\x6f\x4a\x83".to_vec()),
                documents: Vec::new(),
                receipts: BTreeMap::new(),
                certificates: Vec::new(),
                revocations: Vec::new(),
            },
            pending: VecDeque::from([(
                [7; 32],
                neighbour_wire::NeighbourMsg::Heartbeat { peer: 1, ts: 5 },
                true,
            )]),
            pending_bytes: 12,
        })
        .unwrap();
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
}

/// Whether a durable state's base predates [`DURABLE_BASE_VERSION`], read
/// from the three integers both layouts begin with.
pub fn durable_base_is_before_owner(bytes: &[u8]) -> Result<bool, String> {
    let mut rest = bytes;
    for _ in 0..2 {
        let (_, tail) =
            postcard::take_from_bytes::<u32>(rest).map_err(|error| error.to_string())?;
        rest = tail;
    }
    let (base, _) = postcard::take_from_bytes::<u32>(rest).map_err(|error| error.to_string())?;
    Ok(base != DURABLE_BASE_VERSION)
}

/// The durable base layout. Version 4 names the workspace's owner by their root
/// key; version 3, which a build before it wrote, named the governing device
/// itself ([`durable_before_owner`]).
pub const DURABLE_BASE_VERSION: u32 = 4;

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
