//! The bytes the durable encoding writes are kept here, so a field added to,
//! removed from or moved in the layout fails a test instead of leaving every
//! device's stored state unreadable.
use std::collections::{BTreeMap, VecDeque};

use super::*;
use tilo_files::native::migration::assert_golden_bytes;

/// The workspace the kept states are of.
const KEPT_WORKSPACE: &str = "302d59e0-f586-42e1-a59e-3aa613f0f866";

/// [`kept_state`] without an exact index or a sealed history, as `postcard`
/// wrote it, hex.
const KEPT: &str = "082433303264353965302d663538362d343265312d613539652d33616136313366306638363601010101010101010101010101010101010101010101010101010101010101010104856f4a830110736f2e74696c6f2e73657474696e677329757569643a36353033313630642d366161312d383139382d623736362d31303731303165396137346104856f4a8301030303030303030303030303030303030303030303030303030303030303030304000000010d73657474696e67732e726f6f7402020202020202020202020202020202020202020202020202020202020202020000000107070707070707070707070707070707070707070707070707070707070707070a0105010c0000";

/// [`kept_state`] with an exact index and a sealed history, as `postcard`
/// wrote it, hex.
const KEPT_SEALED: &str = "082433303264353965302d663538362d343265312d613539652d33616136313366306638363601010101010101010101010101010101010101010101010101010101010101010104856f4a830110736f2e74696c6f2e73657474696e677329757569643a36353033313630642d366161312d383139382d623736362d31303731303165396137346104856f4a8301030303030303030303030303030303030303030303030303030303030303030304000000010d73657474696e67732e726f6f7402020202020202020202020202020202020202020202020202020202020202020000000107070707070707070707070707070707070707070707070707070707070707070a0105010c017d00012433303264353965302d663538362d343265312d613539652d3361613631336630663836360c736f2e74696c6f2e63686174066c6f756e6765030303030303030303030303030303030303030303030303030303030303030304050505050505050505050505050505050505050505050505050505050505050500010c736f2e74696c6f2e63686174066c6f756e676501067365616c6564100000000000000000000000000000000703030303030303030303030303030303030303030303030303030303030303031000000000000000000000000000000008000000040000004000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";

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

tilo_test_env::scenario! {
    fn kept_durable_states_read_and_write_as_they_are;

    phase a_durable_state_with_a_sealed_history_kept_here_reads_and_writes_as_it_is;
    {
        let exact = kept_exact();
        assert_golden_bytes(
            "a peer's durable state with a sealed history",
            &kept(KEPT_SEALED),
            |bytes| {
                let (value, tail): (Durable<'_>, _) =
                    postcard::take_from_bytes(bytes).expect("the kept state reads");
                assert!(tail.is_empty(), "the kept state is all of the file");
                assert_eq!(value.version, DURABLE_VERSION);
                assert_eq!(value.workspace, KEPT_WORKSPACE);
                let index = crate::admission::ExactAdmissions::decode(value.exact.unwrap()).unwrap();
                assert_eq!(index.len(), 1);
                assert_eq!(value.sealed.len(), 1);
                assert_eq!(value.sealed[0].0.app, "so.tilo.chat");
                assert_eq!(value.sealed[0].1.len(), 1);
            },
            || postcard::to_stdvec(&kept_state(Some(&exact), kept_sealed())).unwrap(),
        );
    }

    phase the_state_kept_here_reads_and_writes_as_it_is;
    {
        assert_golden_bytes(
            "the durable admission state",
            &kept(KEPT),
            |bytes| {
                let (value, tail): (Durable<'_>, _) = postcard::take_from_bytes(bytes).unwrap();
                assert!(tail.is_empty());
                assert_eq!(value.version, DURABLE_VERSION);
                assert_eq!(value.workspace, KEPT_WORKSPACE);
                assert_eq!(value.owner, [1; 32]);
                assert_eq!(value.pending_bytes, 12);
                assert!(value.exact.is_none());
                assert_eq!(
                    value.documents[0].own.as_ref().map(|own| own.owner),
                    Some([2; 32]),
                    "a member's own record is kept with what it is"
                );
            },
            || postcard::to_stdvec(&kept_state(None, Vec::new())).unwrap(),
        );
    }
}

/// One write waiting to be sent, in both kept states.
fn kept_pending() -> VecDeque<([u8; 32], NeighbourMsg, bool)> {
    VecDeque::from([(
        [7; 32],
        neighbour_wire::NeighbourMsg::Heartbeat { peer: 1, ts: 5 },
        true,
    )])
}

/// One member's own record, kept with what it is.
fn kept_document() -> DurableDocument {
    let own = collab_types::OwnRecord::new("settings.root", [2; 32]).unwrap();
    DurableDocument {
        route: Route {
            app: "so.tilo.settings".into(),
            record: own.record_id(),
        },
        bytes: b"\x85\x6f\x4a\x83".to_vec(),
        watermarks: vec![([3; 32], 4)],
        encryption_required: false,
        covered: BTreeMap::new(),
        covered_snapshot: None,
        own: Some(own),
    }
}

/// The state this build writes for the kept workspace: one member's own
/// record, one write waiting.
fn kept_state(exact: Option<&[u8]>, sealed: SealedHistory) -> Durable<'_> {
    Durable {
        version: DURABLE_VERSION,
        workspace: KEPT_WORKSPACE.into(),
        owner: [1; 32],
        workspace_bytes: Some(b"\x85\x6f\x4a\x83".to_vec()),
        documents: vec![kept_document()],
        receipts: BTreeMap::new(),
        certificates: Vec::new(),
        revocations: Vec::new(),
        pending: kept_pending(),
        pending_bytes: 12,
        exact,
        sealed,
    }
}
