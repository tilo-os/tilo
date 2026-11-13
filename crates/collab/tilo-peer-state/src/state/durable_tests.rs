//! The bytes the durable encoding writes are kept here, so a field added to,
//! removed from or moved in the base or an envelope fails a test instead of
//! leaving every device's stored state unreadable.
use std::collections::{BTreeMap, VecDeque};

use super::*;
use tilo_files::native::migration::assert_golden_bytes;

/// The workspace the kept states are of.
const KEPT_WORKSPACE: &str = "302d59e0-f586-42e1-a59e-3aa613f0f866";

/// The state this build writes for the kept workspace, hex, kept so the
/// layout cannot change under the version devices already hold. It is
/// [`kept_state`] as `postcard` wrote it.
const KEPT: &str = "04052433303264353965302d663538362d343265312d613539652d33616136313366306638363601010101010101010101010101010101010101010101010101010101010101010104856f4a83000000000107070707070707070707070707070707070707070707070707070707070707070a0105010c";

/// [`KEPT`] as a build before the compact governance layout wrote it:
/// base version 4, read as this build's base with its governance brought up
/// ([`crate::workspace::layout`]).
const KEPT_LAYOUT_BEFORE: &str = "04042433303264353965302d663538362d343265312d613539652d33616136313366306638363601010101010101010101010101010101010101010101010101010101010101010104856f4a83000000000107070707070707070707070707070707070707070707070707070707070707070a0105010c";

/// The durable state this build writes with an exact index and a sealed
/// history, hex, kept so the layout cannot change under the version devices
/// already hold. It is [`kept_v6`] as `postcard` wrote it.
const KEPT_SEALED: &str = "0604052433303264353965302d663538362d343265312d613539652d33616136313366306638363601010101010101010101010101010101010101010101010101010101010101010104856f4a83000000000107070707070707070707070707070707070707070707070707070707070707070a0105010c7d00012433303264353965302d663538362d343265312d613539652d3361613631336630663836360c736f2e74696c6f2e63686174066c6f756e6765030303030303030303030303030303030303030303030303030303030303030304050505050505050505050505050505050505050505050505050505050505050500010c736f2e74696c6f2e63686174066c6f756e676501067365616c6564100000000000000000000000000000000703030303030303030303030303030303030303030303030303030303030303031000000000000000000000000000000008000000040000004000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";

/// Bytes kept as hex, as a test reads them.
fn kept(hex: &str) -> Vec<u8> {
    hex::decode(hex).unwrap()
}

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
        Route {
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
