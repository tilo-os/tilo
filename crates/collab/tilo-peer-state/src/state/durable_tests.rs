//! The bytes the durable encoding writes are kept here, so a field added to,
//! removed from or moved in the layout fails a test instead of leaving every
//! device's stored state unreadable.
use std::collections::{BTreeMap, VecDeque};

use super::*;
use tilo_files::native::migration::assert_golden_bytes;
use tilo_user_identity::DeviceCapabilities;

/// The workspace the kept states are of.
const KEPT_WORKSPACE: &str = "302d59e0-f586-42e1-a59e-3aa613f0f866";

/// The digest of the kept state's bytes: the layout, as it is written.
const KEPT_LAYOUT: &str = "79af4b55e009696d71b98abfd9529f7c8e299f4f89f15a01beeaaba660df0664";

/// The durable layout version the kept bytes were written at. A layout change
/// writes new kept bytes: bump [`DURABLE_VERSION`] and this together, so a
/// stored state's shape never moves under the version that names it.
const KEPT_VERSION: u32 = 10;

/// [`kept_state`] without an exact index or a sealed history, as `postcard`
/// wrote it, hex.
const KEPT: &str = "0a2433303264353965302d663538362d343265312d613539652d33616136313366306638363601010101010101010101010101010101010101010101010101010101010101010104856f4a830110736f2e74696c6f2e73657474696e677329757569643a36353033313630642d366161312d383139382d623736362d31303731303165396137346104856f4a8301030303030303030303030303030303030303030303030303030303030303030304000000010d73657474696e67732e726f6f7402020202020202020202020202020202020202020202020202020202020202020001010101010101010101010101010101010101010101010101010101010101010104040404040404040404040404040404040404040404040404040404040404040c4672616d65776f726b2031360354010909090909090909090909090909090901000000e80701d00f4005050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505010101010101010101010101010101010101010101010101010101010101010106060606060606060606060606060606060606060606060606060606060606061774686520706572736f6e2773206f776e2064657669636503020101000084070040070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070101010101010101010101010101010101010101010101010101010101010101010404040404040404040404040404040404040404040404040404040404040404b817024008080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808010707070707070707070707070707070707070707070707070707070707070707080105010c0000";

/// [`kept_state`] with an exact index and a sealed history, as `postcard`
/// wrote it, hex.
const KEPT_SEALED: &str = "0a2433303264353965302d663538362d343265312d613539652d33616136313366306638363601010101010101010101010101010101010101010101010101010101010101010104856f4a830110736f2e74696c6f2e73657474696e677329757569643a36353033313630642d366161312d383139382d623736362d31303731303165396137346104856f4a8301030303030303030303030303030303030303030303030303030303030303030304000000010d73657474696e67732e726f6f7402020202020202020202020202020202020202020202020202020202020202020001010101010101010101010101010101010101010101010101010101010101010104040404040404040404040404040404040404040404040404040404040404040c4672616d65776f726b2031360354010909090909090909090909090909090901000000e80701d00f4005050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505010101010101010101010101010101010101010101010101010101010101010106060606060606060606060606060606060606060606060606060606060606061774686520706572736f6e2773206f776e2064657669636503020101000084070040070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070101010101010101010101010101010101010101010101010101010101010101010404040404040404040404040404040404040404040404040404040404040404b817024008080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808010707070707070707070707070707070707070707070707070707070707070707080105010c017d00012433303264353965302d663538362d343265312d613539652d3361613631336630663836360c736f2e74696c6f2e63686174066c6f756e6765030303030303030303030303030303030303030303030303030303030303030304050505050505050505050505050505050505050505050505050505050505050500010c736f2e74696c6f2e63686174066c6f756e676501067365616c656410000000000000000000000000000000070303030303030303030303030303030303030303030303030303030303030303100000000000000000000000000000000800000004000000004000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";

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

    phase the_kept_layout_and_its_version_move_together;
    // A field added, removed or moved writes other kept bytes: that is a new
    // layout, and its version moves with it. This fails before anyone ships a
    // layout change under the last version, which would leave stored states
    // unreadable.
    {
        let bytes = postcard::to_stdvec(&kept_state(None, Vec::new())).expect("the layout writes");
        let (version, _) = postcard::take_from_bytes::<u32>(&bytes).expect("a version first");
        assert_eq!(KEPT_VERSION, DURABLE_VERSION, "a new layout takes a new version");
        assert_eq!(version, DURABLE_VERSION);
        assert_eq!(
            blake3::hash(&bytes).to_hex().as_str(),
            KEPT_LAYOUT,
            "the durable layout changed: bump DURABLE_VERSION and write the kept states again \
             (KEPT and KEPT_SEALED), with this digest"
        );
    }

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
                assert_eq!(value.version, KEPT_VERSION);
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
                assert_eq!(value.version, KEPT_VERSION);
                assert_eq!(value.workspace, KEPT_WORKSPACE);
                assert_eq!(value.owner, [1; 32]);
                assert_eq!(value.pending_bytes, 12);
                assert!(value.exact.is_none());
                assert_eq!(
                    value.documents[0].own.as_ref().map(|own| own.owner),
                    Some([2; 32]),
                    "a member's own record is kept with what it is"
                );
                assert_eq!(
                    value.certificates[0].issuer.as_ref().map(|issuer| issuer.device_public_key),
                    Some([6; 32]),
                    "a certificate's issuer is an Option a stored state carries"
                );
                assert_eq!(
                    value.revocations[0].reason,
                    tilo_user_identity::RevocationReason::Retired
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
    let own = collab_types::OwnResource::new("settings.root", [2; 32]).unwrap();
    DurableDocument {
        route: Route {
            app: "so.tilo.settings".into(),
            record: own.document_id(),
        },
        bytes: b"\x85\x6f\x4a\x83".to_vec(),
        watermarks: vec![([3; 32], 4)],
        encryption_required: false,
        covered: BTreeMap::new(),
        covered_snapshot: None,
        own: Some(own),
    }
}

/// One certificate, with the issuer that signed it: an `Option` a stored
/// state carries.
fn kept_certificate() -> DeviceCert {
    DeviceCert {
        user_public_key: [1; 32],
        device_public_key: [4; 32],
        device_label: "Framework 16".into(),
        capabilities: DeviceCapabilities {
            can_own_workspaces: true,
            can_admit_devices: false,
            foreign: false,
            can_administer_devices: false,
            agent: Some(tilo_user_identity::AgentScope::Workspace([9; 16])),
            stand_in: false,
            server_key: false,
            browser_pages: true,
        },
        issued_at_ns: 1_000,
        expires_at_ns: Some(2_000),
        signature: [5; 64],
        issuer: Some(Box::new(DeviceCert {
            user_public_key: [1; 32],
            device_public_key: [6; 32],
            device_label: "the person's own device".into(),
            capabilities: DeviceCapabilities {
                can_own_workspaces: true,
                can_admit_devices: true,
                foreign: false,
                can_administer_devices: true,
                agent: None,
                stand_in: false,
                server_key: false,
                browser_pages: false,
            },
            issued_at_ns: 900,
            expires_at_ns: None,
            signature: [7; 64],
            issuer: None,
        })),
    }
}

/// One revocation a stored state keeps.
fn kept_revocation() -> tilo_user_identity::RevocationEntry {
    tilo_user_identity::RevocationEntry {
        user_public_key: [1; 32],
        revoked_device_public_key: [4; 32],
        revoked_at_ns: 3_000,
        reason: tilo_user_identity::RevocationReason::Retired,
        signature: [8; 64],
    }
}

/// The state this build writes for the kept workspace: one member's own
/// record, one write waiting.
fn kept_state(exact: Option<&[u8]>, sealed: SealedHistory) -> Durable<'_> {
    Durable {
        version: KEPT_VERSION,
        workspace: KEPT_WORKSPACE.into(),
        owner: [1; 32],
        workspace_bytes: Some(b"\x85\x6f\x4a\x83".to_vec()),
        documents: vec![kept_document()],
        receipts: BTreeMap::new(),
        // A certificate with an issuer, and a revocation: the shapes a stored
        // state carries beside its documents, kept under the same layout.
        certificates: vec![kept_certificate()],
        revocations: vec![kept_revocation()],
        pending: kept_pending(),
        pending_bytes: 12,
        exact,
        sealed,
    }
}
