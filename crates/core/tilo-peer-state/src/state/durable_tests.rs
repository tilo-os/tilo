//! The bytes the durable encoding writes are kept here, so a field added to,
//! removed from or moved in the layout fails a test instead of leaving every
//! device's stored state unreadable.
use std::collections::{BTreeMap, VecDeque};

use super::*;
use ed25519_dalek::Verifier as _;
use tilo_files::native::migration::assert_golden_bytes;
use tilo_user_identity::DeviceCapabilities;

/// The workspace the kept states are of.
const KEPT_WORKSPACE: &str = "302d59e0-f586-42e1-a59e-3aa613f0f866";

/// The digest of the kept state's bytes: the layout, as it is written.
const KEPT_LAYOUT: &str = "1bada46cb4c3f45c038444b0af3ecf37002fe6c2464a39175cfe3516e8034827";

/// The durable layout version the kept bytes were written at. A layout change
/// writes new kept bytes: bump [`DURABLE_VERSION`] and this together, so a
/// stored state's shape never moves under the version that names it.
const KEPT_VERSION: u32 = 15;

/// [`kept_state`] without an exact index or a sealed history, as `postcard`
/// wrote it, hex.
const KEPT: &str = "0f2433303264353965302d663538362d343265312d613539652d33616136313366306638363601010101010101010101010101010101010101010101010101010101010101010104856f4a830110736f2e74696c6f2e73657474696e677329757569643a36353033313630642d366161312d383139382d623736362d31303731303165396137346104856f4a83010303030303030303030303030303030303030303030303030303030303030303040000000001010101010101010101010101010101010101010101010101010101010101010104040404040404040404040404040404040404040404040404040404040404040c4672616d65776f726b2031360354010909090909090909090909090909090901000000e80701d00f4005050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505010101010101010101010101010101010101010101010101010101010101010106060606060606060606060606060606060606060606060606060606060606061774686520706572736f6e2773206f776e206465766963650101000084070040070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070101010101010101010101010101010101010101010101010101010101010101010404040404040404040404040404040404040404040404040404040404040404b817024008080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808010707070707070707070707070707070707070707070707070707070707070707080105010c0000";

/// [`kept_state`] with an exact index and a sealed history, as `postcard`
/// wrote it, hex.
const KEPT_SEALED: &str = "0f2433303264353965302d663538362d343265312d613539652d33616136313366306638363601010101010101010101010101010101010101010101010101010101010101010104856f4a830110736f2e74696c6f2e73657474696e677329757569643a36353033313630642d366161312d383139382d623736362d31303731303165396137346104856f4a83010303030303030303030303030303030303030303030303030303030303030303040000000001010101010101010101010101010101010101010101010101010101010101010104040404040404040404040404040404040404040404040404040404040404040c4672616d65776f726b2031360354010909090909090909090909090909090901000000e80701d00f4005050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505010101010101010101010101010101010101010101010101010101010101010106060606060606060606060606060606060606060606060606060606060606061774686520706572736f6e2773206f776e206465766963650101000084070040070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070101010101010101010101010101010101010101010101010101010101010101010404040404040404040404040404040404040404040404040404040404040404b817024008080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808010707070707070707070707070707070707070707070707070707070707070707080105010c017d00012433303264353965302d663538362d343265312d613539652d3361613631336630663836360c736f2e74696c6f2e63686174066c6f756e6765030303030303030303030303030303030303030303030303030303030303030304050505050505050505050505050505050505050505050505050505050505050500010c736f2e74696c6f2e63686174066c6f756e676501067365616c656410000000000000000000000000000000070303030303030303030303030303030303030303030303030303030303030303100000000000000000000000000000000800000004000000004000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";

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

    phase a_state_of_the_version_before_comes_up_relabelled;
    // The version before this build's has this build's layout: it differs in
    // the version number and in what its governance rows mark, so bringing it
    // up relabels it and changes no other byte.
    {
        let bytes = postcard::to_stdvec(&kept_state(None, Vec::new())).expect("the layout writes");
        let mut previous = bytes.clone();
        previous[0] = u8::try_from(super::durable::PREVIOUS).expect("the version before is small");
        let brought = brought_up(&previous).expect("the version before comes up");
        assert_eq!(brought.as_ref(), bytes.as_slice());
        let (value, tail) =
            postcard::take_from_bytes::<Durable<'_>>(brought.as_ref()).expect("it reads");
        assert!(tail.is_empty());
        assert_eq!(value.version, DURABLE_VERSION);
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
                assert_eq!(value.documents[0].route.app, "so.tilo.settings");
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

/// One record of the workspace.
fn kept_document() -> DurableDocument {
    DurableDocument {
        route: Route {
            app: "so.tilo.settings".into(),
            record: "uuid:6503160d-6aa1-8198-b766-107101e9a74a".into(),
        },
        bytes: b"\x85\x6f\x4a\x83".to_vec(),
        watermarks: vec![([3; 32], 4)],
        encryption_required: false,
        covered: BTreeMap::new(),
        covered_snapshot: None,
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
            retired: tilo_user_identity::RetiredFlags::default(),
            can_own_workspaces: true,
            can_admit_devices: false,
            foreign: false,
            agent: Some(tilo_user_identity::AgentScope::Workspace([9; 16])),
            stand_in: false,
            server_key: false,
            browser_pages: true,
            workspace_service: false,
            workspace: None,
            records: None,
            placing: None,
        },
        issued_at_ns: 1_000,
        expires_at_ns: Some(2_000),
        signature: [5; 64],
        issuer: Some(Box::new(DeviceCert {
            user_public_key: [1; 32],
            device_public_key: [6; 32],
            device_label: "the person's own device".into(),
            capabilities: DeviceCapabilities {
                retired: tilo_user_identity::RetiredFlags::default(),
                can_own_workspaces: true,
                can_admit_devices: true,
                foreign: false,
                agent: None,
                stand_in: false,
                server_key: false,
                browser_pages: false,
                workspace_service: false,
                workspace: None,
                records: None,
                placing: None,
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

/// A signed op as version 13 wrote it: its claims name each run's changes, action, units and
/// invoker text, and no call. The state drops it as it comes up ([`brought_up`]).
#[derive(Clone, serde::Serialize)]
struct OpAt13 {
    #[serde(with = "serde_bytes")]
    payload: Vec<u8>,
    artifact: collab_types::ArtifactId,
    author: collab_types::Ed25519PublicKey,
    attestation_id: collab_types::AttestationId,
    kind: collab_types::OpKind,
    timestamp: SystemTime,
    seq: u64,
    encryption: Option<collab_types::EncryptionMetadata>,
    claims: Vec<RunAt13>,
    surface: Option<[u8; 32]>,
    governance: Vec<[u8; 4]>,
    signature: collab_types::Signature,
}

#[derive(Clone, serde::Serialize)]
struct RunAt13 {
    changes: Vec<[u8; 32]>,
    action: collab_types::ClaimedAction,
    units: Vec<collab_types::UnitPath>,
    invoker: Option<String>,
}

/// What version 13's build signed: the op's fields, each claim run's invoker text, and no call.
#[derive(serde::Serialize)]
struct SigningAt13 {
    #[serde(with = "serde_bytes")]
    payload: Vec<u8>,
    artifact: collab_types::ArtifactId,
    author: collab_types::Ed25519PublicKey,
    attestation_id: collab_types::AttestationId,
    kind: collab_types::OpKind,
    timestamp: SystemTime,
    seq: u64,
    encryption: Option<collab_types::EncryptionMetadata>,
    claims: Vec<ClaimAt13>,
    surface: Option<[u8; 32]>,
    governance: Vec<[u8; 4]>,
}

#[derive(serde::Serialize)]
struct ClaimAt13 {
    change: [u8; 32],
    action: collab_types::ClaimedAction,
    units: Vec<collab_types::UnitPath>,
    invoker: Option<String>,
}

/// One document write as version 13 kept it.
#[derive(serde::Serialize)]
struct WriteAt13 {
    peer: neighbour_wire::PeerId,
    app_id: String,
    cluster_id: String,
    document_id: String,
    envelope: OpAt13,
    readers: Option<String>,
}

/// `key`'s version 13 write with one claim, its op, and the bytes its signature covers: built
/// from version 13's layout, not from this build's types.
fn write_at_13(key: &ed25519_dalek::SigningKey) -> (Vec<u8>, OpAt13, Vec<u8>) {
    use ed25519_dalek::Signer as _;

    let acting = tilo_action_policy::Acting {
        by: tilo_action_policy::Actor::member(tilo_action_policy::MemberId::person("ana")),
        app: None,
        agent_session: None,
        cause: None,
        foreign_device: false,
        handed: false,
        acted_for: Vec::new(),
    };
    let command = serde_json::to_string(&acting).unwrap();
    let change = [9_u8; 32];
    let action = collab_types::ClaimedAction::whole_resource_edit(String::new());
    let units = vec![Vec::new()];
    let payload = vec![7_u8; 64];
    let artifact = collab_types::ArtifactId::new();
    let author = collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
    let attestation_id = collab_types::AttestationId::new();
    let timestamp = SystemTime::UNIX_EPOCH;
    let encryption = Some(collab_types::EncryptionMetadata::content_key(
        1,
        collab_types::ContentRetention::Delta,
    ));
    let signed = postcard::to_stdvec(&SigningAt13 {
        payload: payload.clone(),
        artifact,
        author,
        attestation_id,
        kind: collab_types::OpKind::Change,
        timestamp,
        seq: 4,
        encryption,
        claims: vec![ClaimAt13 {
            change,
            action: action.clone(),
            units: units.clone(),
            invoker: Some(command.clone()),
        }],
        surface: None,
        governance: Vec::new(),
    })
    .unwrap();
    let signature = collab_types::Signature::from_bytes(key.sign(&signed).to_bytes());
    let op = OpAt13 {
        payload,
        artifact,
        author,
        attestation_id,
        kind: collab_types::OpKind::Change,
        timestamp,
        seq: 4,
        encryption,
        claims: vec![RunAt13 {
            changes: vec![change],
            action,
            units,
            invoker: Some(command),
        }],
        surface: None,
        governance: Vec::new(),
        signature,
    };
    let mut write = vec![NeighbourMsg::APP_DOCUMENT_TAG];
    write.extend(
        postcard::to_stdvec(&WriteAt13 {
            peer: 3,
            app_id: "so.tilo.notes".into(),
            cluster_id: KEPT_WORKSPACE.into(),
            document_id: "uuid:1c2f".into(),
            envelope: op.clone(),
            readers: None,
        })
        .unwrap(),
    );
    (write, op, signed)
}

/// A version 13 state holding `write` for a prerequisite and `op` as its one record's sealed
/// write, as version 13 wrote it.
fn written_at_13(write: &[u8], op: &OpAt13) -> Vec<u8> {
    let document = kept_document();
    let certificates = vec![kept_certificate()];
    let revocations = vec![kept_revocation()];
    let receipts: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let exact = kept_exact();
    let held = Durable {
        version: 13,
        workspace: KEPT_WORKSPACE.into(),
        owner: [1; 32],
        workspace_bytes: Some(b"\x85\x6f\x4a\x83".to_vec()),
        documents: vec![document],
        receipts: receipts.clone(),
        certificates: certificates.clone(),
        revocations: revocations.clone(),
        pending: VecDeque::new(),
        pending_bytes: 0,
        exact: Some(&exact),
        sealed: Vec::new(),
    };
    let mut bytes = postcard::to_stdvec(&(
        13_u32,
        &held.workspace,
        &held.owner,
        &held.workspace_bytes,
        held.documents.len(),
    ))
    .unwrap();
    for document in &held.documents {
        bytes.extend(postcard::to_stdvec(document).unwrap());
    }
    bytes.extend(postcard::to_stdvec(&(&receipts, &certificates, &revocations, 1_usize)).unwrap());
    bytes.extend_from_slice(&[7; 32]);
    bytes.extend_from_slice(write);
    bytes.push(1);
    bytes.extend(postcard::to_stdvec(&(write.len(), Some(&exact))).unwrap());
    bytes.extend(
        postcard::to_stdvec(&vec![(
            Route {
                app: "so.tilo.chat".into(),
                record: "lounge".into(),
            },
            vec![op.clone()],
        )])
        .unwrap(),
    );
    bytes
}

tilo_test_env::scenario! {
    fn a_version_13_state_comes_up_without_the_writes_it_can_no_longer_verify;

    phase its_held_and_sealed_writes_are_dropped_and_the_rest_comes_up;
    {
        let key = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
        let (write, op, signed) = write_at_13(&key);
        assert!(
            key.verifying_key()
                .verify(
                    &signed,
                    &ed25519_dalek::Signature::from_bytes(&op.signature.0),
                )
                .is_ok(),
            "a version 13 write's claim signature verifies over version 13's bytes"
        );
        let exact = kept_exact();
        let at_13 = written_at_13(&write, &op);
        let brought = brought_up(&at_13).expect("version 13 comes up");
        assert_eq!(
            postcard::take_from_bytes::<u32>(&brought).unwrap().0,
            DURABLE_VERSION
        );
        let read = Durable::read(&brought, KEPT_WORKSPACE).expect("it reads as this build's layout");
        assert!(read.pending.is_empty(), "the held writes are dropped");
        assert_eq!(read.pending_bytes, 0, "and the bytes they took are not counted");
        assert!(read.sealed.is_empty(), "the sealed writes are dropped");
        assert_eq!(read.workspace, KEPT_WORKSPACE);
        assert_eq!(read.owner, [1; 32]);
        assert_eq!(read.workspace_bytes.as_deref(), Some(b"\x85\x6f\x4a\x83".as_slice()));
        assert_eq!(read.documents.len(), 1);
        assert_eq!(read.documents[0].route.app, "so.tilo.settings");
        assert_eq!(read.certificates.len(), 1);
        assert_eq!(read.certificates[0].device_public_key, [4; 32]);
        assert_eq!(read.revocations.len(), 1);
        assert_eq!(read.exact, Some(exact.as_slice()), "the exact index comes up as it was");
    }
}
