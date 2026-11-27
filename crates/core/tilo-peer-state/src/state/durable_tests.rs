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
const KEPT_LAYOUT: &str = "f74cd1f27104cb01a72342627a8d9669e25075842d0002f6f1386a556f2be13a";

/// The durable layout version the kept bytes were written at. A layout change
/// writes new kept bytes: bump [`DURABLE_VERSION`] and this together, so a
/// stored state's shape never moves under the version that names it.
const KEPT_VERSION: u32 = 19;

/// [`kept_state`] without an exact index or a sealed history, as `postcard`
/// wrote it, hex.
const KEPT: &str = "132433303264353965302d663538362d343265312d613539652d33616136313366306638363601010101010101010101010101010101010101010101010101010101010101010104856f4a830110736f2e74696c6f2e73657474696e677329757569643a36353033313630642d366161312d383139382d623736362d31303731303165396137346104856f4a83010303030303030303030303030303030303030303030303030303030303030303040000000001010101010101010101010101010101010101010101010101010101010101010104040404040404040404040404040404040404040404040404040404040404040c4672616d65776f726b2031360354010909090909090909090909090909090901000000e80701d00f4005050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505010101010101010101010101010101010101010101010101010101010101010106060606060606060606060606060606060606060606060606060606060606061774686520706572736f6e2773206f776e206465766963650101000084070040070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070101010101010101010101010101010101010101010101010101010101010101010404040404040404040404040404040404040404040404040404040404040404b817024008080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808010707070707070707070707070707070707070707070707070707070707070707080105010c0000";

/// [`kept_state`] with an exact index and a sealed history, as `postcard`
/// wrote it, hex.
const KEPT_SEALED: &str = "132433303264353965302d663538362d343265312d613539652d33616136313366306638363601010101010101010101010101010101010101010101010101010101010101010104856f4a830110736f2e74696c6f2e73657474696e677329757569643a36353033313630642d366161312d383139382d623736362d31303731303165396137346104856f4a83010303030303030303030303030303030303030303030303030303030303030303040000000001010101010101010101010101010101010101010101010101010101010101010104040404040404040404040404040404040404040404040404040404040404040c4672616d65776f726b2031360354010909090909090909090909090909090901000000e80701d00f4005050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505050505010101010101010101010101010101010101010101010101010101010101010106060606060606060606060606060606060606060606060606060606060606061774686520706572736f6e2773206f776e206465766963650101000084070040070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070707070101010101010101010101010101010101010101010101010101010101010101010404040404040404040404040404040404040404040404040404040404040404b817024008080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808080808010707070707070707070707070707070707070707070707070707070707070707080105010c017d00012433303264353965302d663538362d343265312d613539652d3361613631336630663836360c736f2e74696c6f2e63686174066c6f756e6765030303030303030303030303030303030303030303030303030303030303030304050505050505050505050505050505050505050505050505050505050505050500010c736f2e74696c6f2e63686174066c6f756e676501067365616c656410000000000000000000000000000000070303030303030303030303030303030303030303030303030303030303030303100000000000000000000000000000000800000004000000004000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";

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

    phase states_of_the_versions_before_come_up_relabelled;
    // The versions before this build's have this build's layout: they differ
    // in the version number, in the messages a waiting write may be (content
    // kept or let go of, and before that asked for and answered many at a
    // time), in what certificates they may
    // name (a placed device's records, a placement grant) and, before that, in
    // what governance rows mark, so bringing any of them up relabels it and
    // changes no other byte.
    {
        let bytes = postcard::to_stdvec(&kept_state(None, Vec::new())).expect("the layout writes");
        for version in super::durable::RELABELLED..=super::durable::PREVIOUS {
            let mut earlier = bytes.clone();
            earlier[0] = u8::try_from(version).expect("an earlier version is small");
            let brought = brought_up(&earlier).expect("the version comes up");
            assert_eq!(brought.as_ref(), bytes.as_slice(), "version {version} is relabelled");
            let (value, tail) =
                postcard::take_from_bytes::<Durable<'_>>(brought.as_ref()).expect("it reads");
            assert!(tail.is_empty());
            assert_eq!(value.version, DURABLE_VERSION);
        }
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

    phase a_write_whose_migration_step_was_written_without_its_bool_is_refused_by_its_record;
    {
        // A build in the window between the migration step and its `rewrite` being written whole
        // kept writes whose claim names the step as a history move: a postcard reader takes the
        // units prefix after it as the absent bool, and the signature covers the bytes it reads
        // as. The state is refused by the record, waiting write or sealed, never read with a
        // write it would serve as another claim.
        let change = [9_u8; 32];
        let op = || {
            let mut op = collab_types::SignedOp::new_unsigned(
                vec![7_u8; 64],
                collab_types::ArtifactId::new(),
                collab_types::Ed25519PublicKey([3; 32]),
                collab_types::AttestationId::new(),
                collab_types::OpKind::Change,
                std::time::SystemTime::UNIX_EPOCH,
                4,
            );
            op.claims = vec![collab_types::claim::ChangeClaim {
                change,
                action: collab_types::claim::ClaimedAction::History(
                    tilo_actions::HistoryOp::Migrate { to: 3, rewrite: true },
                ),
                units: Vec::new(),
                invoker: None,
                call: None,
            }];
            op
        };
        let waiting = NeighbourMsg::AppDocument {
            peer: 0,
            app_id: "so.tilo.chat".into(),
            cluster_id: "workspace".into(),
            document_id: "lounge".into(),
            envelope: Box::new(op()),
            readers: None,
        };
        let mut state = kept_state(None, Vec::new());
        state.pending = VecDeque::from([([7; 32], waiting.clone(), false)]);
        state.pending_bytes = crate::postcard_sink::encoded_len(&waiting, usize::MAX).unwrap();
        let bytes = postcard::to_stdvec(&state).unwrap();
        let Err(error) = Durable::read(&bytes, KEPT_WORKSPACE) else {
            panic!("a state holding the old-shape write reads");
        };
        assert!(
            error.contains("so.tilo.chat/lounge, a waiting write"),
            "the waiting write is refused by its record: {error}"
        );

        state.pending = VecDeque::new();
        state.pending_bytes = 0;
        state.sealed = vec![(
            Route {
                app: "so.tilo.chat".into(),
                record: "lounge".into(),
            },
            vec![op()],
        )];
        let bytes = postcard::to_stdvec(&state).unwrap();
        let Err(error) = Durable::read(&bytes, KEPT_WORKSPACE) else {
            panic!("a state holding the old-shape sealed write reads");
        };
        assert!(
            error.contains("so.tilo.chat/lounge, a sealed write"),
            "the sealed write is refused by its record: {error}"
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
/// invoker text, and no call.
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

    let acting = tilo_member_types::Acting {
        by: tilo_member_types::Actor::member(tilo_member_types::MemberId::person("ana")),
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

/// A state of layout `version` (12 or 13) holding `write` for a prerequisite and `op` as its one
/// record's sealed write, with the exact index `exact`, as those layouts wrote it.
fn written_at_13(version: u32, write: &[u8], op: &OpAt13, exact: &[u8]) -> Vec<u8> {
    written_with(version, vec![kept_document()], None, write, op, exact)
}

/// [`written_at_13`] of `documents`, each followed by `own` when it is given: the member's own
/// record layouts 10 and 11 kept with each document.
fn written_with(
    version: u32,
    documents: Vec<DurableDocument>,
    own: Option<&[u8]>,
    write: &[u8],
    op: &OpAt13,
    exact: &[u8],
) -> Vec<u8> {
    let certificates = vec![kept_certificate()];
    let revocations = vec![kept_revocation()];
    let receipts: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let held = Durable {
        version,
        workspace: KEPT_WORKSPACE.into(),
        owner: [1; 32],
        workspace_bytes: Some(b"\x85\x6f\x4a\x83".to_vec()),
        documents,
        receipts: receipts.clone(),
        certificates: certificates.clone(),
        revocations: revocations.clone(),
        pending: VecDeque::new(),
        pending_bytes: 0,
        exact: Some(exact),
        sealed: Vec::new(),
    };
    let mut bytes = postcard::to_stdvec(&(
        version,
        &held.workspace,
        &held.owner,
        &held.workspace_bytes,
        held.documents.len(),
    ))
    .unwrap();
    for document in &held.documents {
        bytes.extend(postcard::to_stdvec(document).unwrap());
        bytes.extend_from_slice(own.unwrap_or_default());
    }
    bytes.extend(postcard::to_stdvec(&(&receipts, &certificates, &revocations, 1_usize)).unwrap());
    bytes.extend_from_slice(&[7; 32]);
    bytes.extend_from_slice(write);
    bytes.push(1);
    bytes.extend(postcard::to_stdvec(&(write.len(), Some(exact))).unwrap());
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
    /// A state of layout 12 or 13, which kept its held and sealed writes with claims that name no
    /// call, comes up with every one of them: each verifies as its author signed it, and the
    /// exact index names the sealed write by its bytes as this build writes them.
    fn a_state_kept_before_claims_named_a_call_comes_up_with_every_write;

    phase its_held_and_sealed_writes_come_up_and_verify;
    {
        for version in [12_u32, 13] {
            let key = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
            let (write, op, _) = write_at_13(&key);
            let kept_id = *blake3::hash(&postcard::to_stdvec(&op).unwrap()).as_bytes();
            let key_of = |sequence| crate::admission::AdmissionKey {
                workspace: KEPT_WORKSPACE.into(),
                app: "so.tilo.chat".into(),
                record: "lounge".into(),
                author: key.verifying_key().to_bytes(),
                sequence,
            };
            let mut index = crate::admission::ExactAdmissions::default();
            index
                .insert(&crate::admission::DocumentAdmission {
                    key: key_of(4),
                    operation_id: kept_id,
                })
                .unwrap();
            let exact = index.encode().unwrap();
            let kept = written_at_13(version, &write, &op, &exact);
            let brought = brought_up(&kept).expect("the state comes up");
            let read = Durable::read(&brought, KEPT_WORKSPACE).expect("it reads as this build's layout");
            assert_eq!(read.pending.len(), 1, "layout {version}: the held write comes up");
            let NeighbourMsg::AppDocument { envelope, document_id, .. } = &read.pending[0].1 else {
                panic!("layout {version}: the held write is a document write");
            };
            assert_eq!(document_id, "uuid:1c2f");
            collab_types::verify_op(envelope).expect("the held write verifies as its author signed it");
            let measured = crate::postcard_sink::encoded_len(&read.pending[0].1, usize::MAX).unwrap();
            assert_eq!(read.pending_bytes, measured, "the held bytes are counted as this build writes them");
            assert_eq!(read.sealed.len(), 1, "layout {version}: the sealed history comes up");
            let sealed = &read.sealed[0].1;
            assert_eq!(sealed.len(), 1);
            collab_types::verify_op(&sealed[0]).expect("the sealed write verifies as its author signed it");
            assert_eq!(sealed[0].signature.0, op.signature.0, "its signature is its author's");
            let index = crate::admission::ExactAdmissions::decode(read.exact.unwrap()).unwrap();
            let now = crate::admission::DocumentAdmission::from_op(
                KEPT_WORKSPACE,
                "so.tilo.chat",
                "lounge",
                &sealed[0],
            )
            .unwrap();
            assert_ne!(now.operation_id, kept_id, "its bytes are this build's");
            assert_eq!(
                index.get(&key_of(4)),
                Some(&now.operation_id),
                "the exact index names the write by its bytes as this build writes them"
            );
            assert_eq!(read.documents.len(), 1, "the documents come up as they were");
            assert_eq!(read.certificates.len(), 1);
            assert_eq!(read.revocations.len(), 1);
            let mut forged = sealed[0].clone();
            forged.claims[0].units = vec![vec![tilo_actions::PathStep::Key("other".into())]];
            assert!(collab_types::verify_op(&forged).is_err(), "a changed claim does not verify");
        }
    }
}

tilo_test_env::scenario! {
    fn a_kept_member_list_with_a_superseded_waiting_change_restores_without_it;

    phase it_restores_and_is_kept_again_whole;
    // An earlier layout kept the workspace's member list on a base saved with the changes then
    // waiting beside it. A change of a history a base replaced waited there, and the document
    // dropped it once its actor numbered a change of the current history the same: the kept
    // state held both, and the host holding it no longer started.
    {
        use automerge::transaction::Transactable as _;
        let (peer, _, _, _, now) = crate::admission_tests::seeded();
        let workspace = peer.workspace_id().to_owned();
        let owner = peer.owner_certificates()[0].clone();
        let restore = |bytes: &[u8]| {
            StatePeer::restore(
                bytes,
                &workspace,
                owner.clone(),
                vec![owner.clone()],
                RevocationList::new(),
                now,
            )
        };
        let kept = peer.durable().unwrap();
        let mut durable = Durable::read(&kept, &workspace).unwrap();
        let mut doc =
            automerge::AutoCommit::load(durable.workspace_bytes.as_deref().unwrap()).unwrap();
        // The actor's second change of the replaced history, built on a first this document
        // never gets.
        let actor = automerge::ActorId::from(b"holder".as_slice());
        let mut replaced = doc.fork().with_actor(actor.clone());
        replaced.put(automerge::ROOT, "replaced", 1).unwrap();
        replaced.commit();
        let missing = replaced.get_heads();
        replaced.put(automerge::ROOT, "replaced", 2).unwrap();
        replaced.commit();
        let superseded = replaced.get_changes(&missing);
        tilo_automerge::ApplyIncoming::apply_incoming(&mut doc, superseded).unwrap();
        assert_eq!(doc.committed_document().queued_changes().count(), 1);
        let base = doc.save();
        let heads = doc.get_heads();
        // The actor writes on the current history: its second change takes the waiting one's
        // number.
        doc.set_actor(actor);
        for n in 0..2 {
            doc.put(automerge::ROOT, "written", n).unwrap();
            doc.commit();
        }
        durable.workspace_bytes = Some([base, doc.save_after(&heads)].concat());
        let broken = postcard::to_allocvec(&durable).unwrap();
        assert!(
            Durable::read(&broken, &workspace)
                .ok()
                .and_then(|durable| automerge::AutoCommit::load(
                    durable.workspace_bytes.as_deref().unwrap()
                )
                .err())
                .is_some_and(|error| error.to_string().contains("duplicate seq")),
            "the kept member list is one automerge refuses whole"
        );

        let restored = restore(&broken).expect("the state restores without the superseded change");
        assert_eq!(restored.workspace_heads(), Some(doc.get_heads()));
        // Kept again, the state holds what it applied, and loads whole.
        let rewritten = restored.durable().unwrap();
        let members = Durable::read(&rewritten, &workspace)
            .unwrap()
            .workspace_bytes
            .unwrap();
        let mut reloaded = automerge::AutoCommit::load(&members).expect("it loads whole");
        assert_eq!(reloaded.get_heads(), doc.get_heads());
        assert_eq!(reloaded.committed_document().queued_changes().count(), 0);
        let again = restore(&rewritten).unwrap();
        assert_eq!(again.workspace_heads(), Some(doc.get_heads()));
    }
}

/// Bytes kept as hex in a fixture, as a test reads them: whitespace between them is the file's.
fn fixture(hex: &str) -> Vec<u8> {
    hex::decode(hex.split_whitespace().collect::<String>()).unwrap()
}

/// The released build's [`KEPT`], layout 10, as its own code wrote it: one document kept with the
/// member's own record it is, one heartbeat waiting, a certificate with its issuer and a
/// revocation.
const KEPT_AT_10: &str = include_str!("fixtures/durable-10.hex");

/// The released build's [`KEPT_SEALED`], layout 10, as its own code wrote it: [`KEPT_AT_10`] with
/// an exact index and one record's sealed write.
const KEPT_SEALED_AT_10: &str = include_str!("fixtures/durable-10-sealed.hex");

/// A state the released build's own code wrote, layout 10: [`KEPT_AT_10`]'s document,
/// certificate and revocation, a signed document write waiting to be sent that names the
/// member's own record it is and claims one change by a person, a heartbeat waiting after it, the
/// same signed write as record `so.tilo.chat`/`lounge`'s one sealed write, and an exact index
/// naming that write by its bytes as the released build wrote them.
const WRITES_AT_10: &str = include_str!("fixtures/durable-10-writes.hex");

/// The record [`KEPT_AT_10`]'s document is: a member's own record of kind `settings.root`, named
/// as the released build named it.
const KEPT_AT_10_RECORD: &str = "uuid:6503160d-6aa1-8198-b766-107101e9a74a";

tilo_test_env::scenario! {
    /// What the released build kept of a workspace, layout 10, comes up as this build's layout:
    /// every document, certificate, revocation, waiting write, exact admission and sealed write
    /// it held, only the member's own record each document and write named dropped.
    fn the_released_builds_kept_states_come_up_with_everything_they_held;

    phase its_kept_state_comes_up;
    {
        let kept = fixture(KEPT_AT_10);
        assert_eq!(postcard::take_from_bytes::<u32>(&kept).unwrap().0, 10);
        let brought = brought_up(&kept).expect("layout 10 comes up");
        let read = Durable::read(&brought, KEPT_WORKSPACE).expect("it reads as this build's layout");
        assert_eq!(read.version, DURABLE_VERSION);
        assert_eq!(read.owner, [1; 32]);
        assert_eq!(read.workspace_bytes.as_deref(), Some(b"\x85\x6f\x4a\x83".as_slice()));
        assert_eq!(read.documents.len(), 1);
        let document = &read.documents[0];
        assert_eq!(
            (document.route.app.as_str(), document.route.record.as_str()),
            ("so.tilo.settings", KEPT_AT_10_RECORD)
        );
        assert_eq!(document.bytes, b"\x85\x6f\x4a\x83");
        assert_eq!(document.watermarks, vec![([3; 32], 4)]);
        assert_eq!(read.certificates.len(), 1);
        let certificate = &read.certificates[0];
        assert_eq!(certificate.device_public_key, [4; 32]);
        assert_eq!(certificate.device_label, "Framework 16");
        assert!(certificate.capabilities.can_own_workspaces);
        assert!(certificate.capabilities.browser_pages);
        assert_eq!(
            certificate.capabilities.agent,
            Some(tilo_user_identity::AgentScope::Workspace([9; 16]))
        );
        assert_eq!(certificate.signature, [5; 64]);
        let issuer = certificate.issuer.as_deref().expect("its issuer");
        assert_eq!(issuer.device_public_key, [6; 32]);
        assert!(issuer.capabilities.can_admit_devices);
        assert_eq!(read.revocations.len(), 1);
        assert_eq!(read.revocations[0].reason, tilo_user_identity::RevocationReason::Retired);
        assert_eq!(read.revocations[0].revoked_device_public_key, [4; 32]);
        assert_eq!(read.pending, kept_pending(), "the waiting write and its flag");
        let measured = crate::postcard_sink::encoded_len(&read.pending[0].1, usize::MAX).unwrap();
        assert_eq!(read.pending_bytes, measured, "the waiting bytes are counted as this build writes them");
        assert!(read.exact.is_none());
        assert!(read.sealed.is_empty());
        // What this build keeps of it is what it would write of the same state.
        assert_eq!(
            brought.as_ref(),
            postcard::to_stdvec(&Durable {
                version: DURABLE_VERSION,
                workspace: KEPT_WORKSPACE.into(),
                owner: [1; 32],
                workspace_bytes: Some(b"\x85\x6f\x4a\x83".to_vec()),
                documents: vec![DurableDocument {
                    route: Route {
                        app: "so.tilo.settings".into(),
                        record: KEPT_AT_10_RECORD.into(),
                    },
                    ..kept_document()
                }],
                receipts: BTreeMap::new(),
                certificates: read.certificates.clone(),
                revocations: read.revocations.clone(),
                pending: kept_pending(),
                pending_bytes: measured,
                exact: None,
                sealed: Vec::new(),
            })
            .unwrap()
        );
    }

    phase its_kept_state_with_a_sealed_history_comes_up;
    {
        let kept = fixture(KEPT_SEALED_AT_10);
        let brought = brought_up(&kept).expect("layout 10 comes up");
        let read = Durable::read(&brought, KEPT_WORKSPACE).expect("it reads as this build's layout");
        assert_eq!(read.documents.len(), 1);
        assert_eq!(read.certificates.len(), 1);
        assert_eq!(read.revocations.len(), 1);
        assert_eq!(read.pending, kept_pending());
        let index = crate::admission::ExactAdmissions::decode(read.exact.unwrap()).unwrap();
        assert_eq!(index.len(), 1, "the exact index comes up");
        assert_eq!(read.sealed.len(), 1, "the sealed history comes up");
        let (route, writes) = &read.sealed[0];
        assert_eq!((route.app.as_str(), route.record.as_str()), ("so.tilo.chat", "lounge"));
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].payload, b"sealed");
        assert_eq!(writes[0].author.as_bytes(), &[3; 32]);
        assert_eq!(writes[0].seq, 4);
    }

    phase its_writes_come_up_and_verify_as_their_authors_signed_them;
    {
        let key = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
        let kept = fixture(WRITES_AT_10);
        let brought = brought_up(&kept).expect("layout 10 comes up");
        let read = Durable::read(&brought, KEPT_WORKSPACE).expect("it reads as this build's layout");
        assert_eq!(read.pending.len(), 2, "every waiting write");
        let ([7, ..], NeighbourMsg::AppDocument { app_id, document_id, envelope, readers, .. }, false) =
            &read.pending[0]
        else {
            panic!("the first waiting write is the document write, not yet sent");
        };
        assert_eq!((app_id.as_str(), document_id.as_str()), ("so.tilo.notes", "uuid:1c2f"));
        assert_eq!(*readers, None, "a write kept before reader sets names none");
        assert_eq!(envelope.author.as_bytes(), &key.verifying_key().to_bytes());
        assert_eq!(envelope.claims.len(), 1);
        assert!(envelope.claims[0].invoker.is_some(), "its claim names who made it");
        collab_types::verify_op(envelope).expect("the waiting write verifies as its author signed it");
        assert_eq!(read.pending[1], ([8; 32], NeighbourMsg::Heartbeat { peer: 1, ts: 5 }, true));
        let measured: usize = read
            .pending
            .iter()
            .map(|(_, message, _)| crate::postcard_sink::encoded_len(message, usize::MAX).unwrap())
            .sum();
        assert_eq!(read.pending_bytes, measured, "the waiting bytes are counted as this build writes them");
        assert_eq!(read.sealed.len(), 1);
        let sealed = &read.sealed[0].1;
        assert_eq!(sealed.len(), 1);
        collab_types::verify_op(&sealed[0]).expect("the sealed write verifies as its author signed it");
        assert_eq!(sealed[0].signature, envelope.signature, "its signature is its author's");
        let index = crate::admission::ExactAdmissions::decode(read.exact.unwrap()).unwrap();
        let now = crate::admission::DocumentAdmission::from_op(
            KEPT_WORKSPACE,
            "so.tilo.chat",
            "lounge",
            &sealed[0],
        )
        .unwrap();
        assert_eq!(
            index.get(&now.key),
            Some(&now.operation_id),
            "the exact index names the write by its bytes as this build writes them"
        );
    }

    phase it_is_split_as_it_was_written;
    // A journal the released build appended beside such a state is a delta of the stretches it
    // split the state into, so a state of layout 10 is split as it was written and replays
    // against them.
    {
        let kept = fixture(KEPT_AT_10);
        let parts = DurableParts::read(&kept).expect("layout 10 splits as it was written");
        assert_eq!(parts.to_vec(), kept, "its stretches are its bytes");
        assert_eq!(parts.documents.len(), 1);
        let route = Route {
            app: "so.tilo.settings".into(),
            record: KEPT_AT_10_RECORD.into(),
        };
        let document = parts.documents.get(&route).expect("its document's stretch");
        let own = postcard::to_stdvec(&Some(("settings.root".to_owned(), [2_u8; 32]))).unwrap();
        assert!(
            document.document.concat().ends_with(&own),
            "a document's stretch holds the own record it was written with"
        );
    }
}

tilo_test_env::scenario! {
    /// A state of layout 11, or of layout 10 in either of its layouts, comes up as layout 12 of
    /// the same state does: only the member's own record each document and waiting write named
    /// goes.
    fn a_state_kept_with_own_records_comes_up_as_it_would_without_them;

    phase every_layout_comes_up_as_layout_12;
    {
        let key = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
        let (write, op, _) = write_at_13(&key);
        let exact = kept_exact();
        // Two documents, in the order of their routes as every layout writes them: one of them
        // the record the sealed write is of.
        let documents = || {
            vec![
                DurableDocument {
                    route: Route {
                        app: "so.tilo.chat".into(),
                        record: "lounge".into(),
                    },
                    ..kept_document()
                },
                kept_document(),
            ]
        };
        let at_12 = written_with(12, documents(), None, &write, &op, &exact);
        let today = brought_up(&at_12).expect("layout 12 comes up").into_owned();
        let own = postcard::to_stdvec(&Some(("notes.page".to_owned(), [2_u8; 32]))).unwrap();
        // The write ends with the reader set it names: none.
        let (without_readers, readers) = write.split_at(write.len() - 1);
        assert_eq!(readers, [0]);
        let with_own_then_readers = [without_readers, &own, readers].concat();
        let with_own = [without_readers, &own].concat();
        for (version, kept_write) in [(11_u32, &with_own_then_readers), (10, &with_own_then_readers), (10, &with_own)] {
            let kept = written_with(version, documents(), Some(&own), kept_write, &op, &exact);
            let brought = brought_up(&kept).expect("the state comes up");
            assert_eq!(brought.as_ref(), today.as_slice(), "layout {version} comes up as layout 12 does");
            let parts = DurableParts::read(&kept).expect("it splits as it was written");
            assert_eq!(parts.to_vec(), kept, "layout {version}: its stretches are its bytes");
            assert_eq!(parts.documents.len(), 2);
        }
    }

    phase a_device_state_kept_at_layout_10_restores_as_it_was;
    {
        let (peer, _, _, _, now) = crate::admission_tests::seeded();
        let workspace = peer.workspace_id().to_owned();
        let owner = peer.owner_certificates()[0].clone();
        let restore = |bytes: &[u8]| {
            StatePeer::restore(
                bytes,
                &workspace,
                owner.clone(),
                vec![owner.clone()],
                RevocationList::new(),
                now,
            )
        };
        let today = peer.durable().unwrap();
        let durable = Durable::read(&today, &workspace).unwrap();
        assert!(durable.pending.is_empty() && durable.sealed.is_empty(), "a state of documents alone");
        // As layout 10 wrote it: each document named the own record it was, here none.
        let mut kept = postcard::to_stdvec(&(
            10_u32,
            &durable.workspace,
            &durable.owner,
            &durable.workspace_bytes,
            durable.documents.len(),
        ))
        .unwrap();
        for document in &durable.documents {
            kept.extend(postcard::to_stdvec(document).unwrap());
            kept.extend(postcard::to_stdvec(&None::<(String, [u8; 32])>).unwrap());
        }
        kept.extend(
            postcard::to_stdvec(&(
                &durable.receipts,
                &durable.certificates,
                &durable.revocations,
                0_usize,
                0_usize,
                durable.exact,
                &durable.sealed,
            ))
            .unwrap(),
        );
        let restored = restore(&kept).expect("a state kept at layout 10 restores");
        assert_eq!(restored.durable().unwrap(), today, "it keeps what it held");
        assert_eq!(restored.workspace_heads(), peer.workspace_heads());
    }
}
