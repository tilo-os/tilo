//! A change a host made for a caller on another device carries the invocation
//! that caller signed, and every device that takes the change in checks it.
use super::*;
use automerge::transaction::CommitOptions;
use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget, SignedCall};
use tilo_actions::{ActionAddress, ActionContext, Did, EditOp, FieldEdit, InvocationId, PathStep};
use tilo_server_protocol::invocation::sign_invocation_now;
use tilo_user_identity::{
    RevocationEntry, RevocationList, RevocationReason, sign_revocation_entry,
};

const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const APP: &str = "test.app";
const RECORD: &str = "tab:1";
const KIND: &str = "test";

fn now_ns(now: SystemTime) -> u64 {
    u64::try_from(
        now.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap()
}

/// The fixture's workspace, its devices' keys and certificates, and its clock:
/// the copy returned is device `a`'s.
type Seeded = (
    StatePeer,
    SigningKey,
    DeviceCert,
    SigningKey,
    DeviceCert,
    SystemTime,
);

fn seeded_with(revocations: RevocationList) -> Seeded {
    let (a, ac, b, bc, now) = fixtures();
    let mut peer = StatePeer::new(
        WORKSPACE,
        ac.clone(),
        vec![ac.clone(), bc.clone()],
        revocations,
        now,
    )
    .unwrap();
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "value", "seed").unwrap();
    doc.commit_edit("fixture", "value");
    peer.install_seed(
        SeedState {
            workspace_changes: seed_workspace(WORKSPACE, &ac, &[ac.clone(), bc.clone()]).unwrap(),
            documents: vec![SeedDocument {
                app_id: APP.into(),
                document_id: RECORD.into(),
                kind: KIND.into(),
                bytes: doc.save(),
            }],
        },
        &a,
        now,
    )
    .unwrap();
    (peer, a, ac, b, bc, now)
}

/// The invoker a change names for the caller whose device is `cert`: the
/// caller's person, on the device that asked.
fn caller_invoker(cert: &DeviceCert) -> tilo_actions::ActionInvoker {
    tilo_actions::ActionInvoker::person(hex::encode(cert.user_public_key)).from_device(Some(
        tilo_actions::InvokerDevice::certified(cert.device_public_key, "the caller's device"),
    ))
}

/// The history entry a host stamps on what it ran for `invoker`, as `cert`'s
/// device signed it.
fn caller_context(
    invoker: tilo_actions::ActionInvoker,
    cert: &DeviceCert,
    key: &SigningKey,
    now: SystemTime,
) -> ActionContext {
    let invocation = InvocationId::new();
    let mut request = InvokeRequest {
        invocation,
        target: RemoteActionTarget {
            provider: "so.tilo.test".into(),
            address: ActionAddress::new(RECORD, "field-edit"),
        },
        argument: InvokeArgument::Text("from the caller".into()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    request.signed = Some(Box::new(
        sign_invocation_now(WORKSPACE, &request, cert, key, now_ns(now)).unwrap(),
    ));
    ActionContext {
        what: Some(Did::Edit(FieldEdit {
            ty: KIND.into(),
            field: "value".into(),
            path: vec![PathStep::Field("value".into())],
            op: EditOp::Set,
            old: None,
            new: None,
            args: std::collections::BTreeMap::new(),
        })),
        invoker: Some(invoker),
        source_invocation: Some(invocation),
        signed: SignedCall::of(&request).map(Box::new),
        ..ActionContext::for_resource(RECORD, KIND, "Test")
    }
}

/// The write of `context` by `signer`'s device, as a peer sends it.
fn written(
    copy: &StatePeer,
    signer: &SigningKey,
    context: &ActionContext,
    value: &str,
    now: SystemTime,
) -> NeighbourMsg {
    committed(copy, signer, &context.change_message().unwrap(), value, now)
}

/// The write of the change `message` by `signer`'s device, as a peer sends it.
fn committed(
    copy: &StatePeer,
    signer: &SigningKey,
    message: &str,
    value: &str,
    now: SystemTime,
) -> NeighbourMsg {
    let mut envelope = unsigned_commit(copy, signer, message, value, now);
    collab_types::sign_op(&mut envelope, signer).unwrap();
    document_message(envelope)
}

/// The write of the change `message`, signed without the producer's own check
/// that every change names its invoker: a misbehaving device sends such a
/// change, and what admission does with it is the receiver's to decide.
fn committed_unclaimed(
    copy: &StatePeer,
    signer: &SigningKey,
    message: &str,
    value: &str,
    now: SystemTime,
) -> NeighbourMsg {
    let mut envelope = unsigned_commit(copy, signer, message, value, now);
    let bytes = envelope.signing_bytes().unwrap();
    let signature = collab_types::signer::sign_now(signer, bytes).unwrap();
    envelope.signature = collab_types::Signature(signature);
    document_message(envelope)
}

/// The unsigned write of the change `message` by `signer`'s device.
fn unsigned_commit(
    copy: &StatePeer,
    signer: &SigningKey,
    message: &str,
    value: &str,
    now: SystemTime,
) -> collab_types::SignedOp {
    let mut doc = AutoCommit::load(&copy.document(APP, RECORD).unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        signer.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    doc.put(ROOT, "value", value).unwrap();
    doc.commit_with(CommitOptions::default().with_message(message));
    let author = collab_types::Ed25519PublicKey::from_bytes(signer.verifying_key().to_bytes());
    collab_types::SignedOp::new_unsigned(
        collab_types::DocumentMutation::Incremental(doc.save_after(&heads))
            .encode()
            .unwrap(),
        collab_types::document_artifact_id(APP, WORKSPACE, RECORD),
        author,
        collab_types::document_attestation_id(APP, WORKSPACE, RECORD),
        collab_types::OpKind::Change,
        now,
        1,
    )
}

/// `envelope` as the document frame a peer sends.
fn document_message(envelope: collab_types::SignedOp) -> NeighbourMsg {
    NeighbourMsg::AppDocument {
        peer: crate::peer_handle(envelope.author.as_bytes()),
        app_id: APP.into(),
        cluster_id: WORKSPACE.into(),
        document_id: RECORD.into(),
        envelope: Box::new(envelope),
        readers: None,
    }
}

/// `receiver` takes `message` in: `Ok` admitted, `Err` refused.
#[expect(
    clippy::result_large_err,
    clippy::panic_in_result_fn,
    reason = "a test helper: the refusal is the whole answer, and a wrong shape fails the test"
)]
fn take(
    receiver: &mut StatePeer,
    message: NeighbourMsg,
    now: SystemTime,
) -> Result<(), crate::ChangeRefused> {
    let source = match &message {
        NeighbourMsg::AppDocument { envelope, .. } => *envelope.author.as_bytes(),
        _ => panic!("a write"),
    };
    receiver
        .receive_from(TransportOrigin::ServerDelta(source), message, now)
        .map(|_| ())
        .map_err(|error| match error {
            crate::AdmissionError::ChangeRefused(refused) => *refused,
            other => panic!("the change is refused as a change: {other:?}"),
        })
}

fn value(peer: &StatePeer) -> String {
    AutoCommit::load(&peer.document(APP, RECORD).unwrap())
        .unwrap()
        .get(ROOT, "value")
        .unwrap()
        .unwrap()
        .0
        .to_string()
}

/// A device of a person who is no member: its root's key, device key and cert.
fn stranger() -> (SigningKey, SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&[41; 32]);
    let key = SigningKey::from_bytes(&[42; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "stranger".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, &root).unwrap();
    (root, key, cert)
}

fn revoking(root: &SigningKey, cert: &DeviceCert) -> RevocationList {
    let mut entry = RevocationEntry {
        user_public_key: cert.user_public_key,
        revoked_device_public_key: cert.device_public_key,
        revoked_at_ns: 500,
        reason: RevocationReason::Compromised,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, root).unwrap();
    let mut revocations = RevocationList::new();
    revocations.insert(entry).unwrap();
    revocations
}

tilo_test_env::scenario! {
    fn a_received_change_made_for_a_caller_proves_it_was_asked_for;

    phase a_change_the_caller_signed_is_admitted;
    {
        let (mut receiver, a, ac, b, _bc, now) = seeded_with(RevocationList::new());
        let context = caller_context(caller_invoker(&ac), &ac, &a, now);
        let message = written(&receiver, &b, &context, "asked for", now);
        take(&mut receiver, message, now)
            .expect("the caller signed the invocation the change keeps");
        assert!(value(&receiver).contains("asked for"));
    }

    phase a_change_made_for_a_caller_without_a_signature_is_refused;
    {
        let (mut receiver, a, ac, b, _bc, now) = seeded_with(RevocationList::new());
        let mut context = caller_context(caller_invoker(&ac), &ac, &a, now);
        context.signed = None;
        let message = written(&receiver, &b, &context, "unsigned", now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(
            refused.reason.contains("carries no invocation") && refused.reason.contains("caller"),
            "{}",
            refused.reason
        );
        assert!(value(&receiver).contains("seed"), "never applied: {}", value(&receiver));
    }

    phase another_invocations_id_arguments_or_invoker_are_refused;
    {
        let (mut receiver, a, ac, b, _bc, now) = seeded_with(RevocationList::new());
        let mut moved = caller_context(caller_invoker(&ac), &ac, &a, now);
        moved.source_invocation = Some(InvocationId::new());
        let message = written(&receiver, &b, &moved, "moved", now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("another invocation"), "{}", refused.reason);

        let mut reargued = caller_context(caller_invoker(&ac), &ac, &a, now);
        reargued.signed.as_mut().unwrap().argument = Some(InvokeArgument::Text("other".into()));
        let message = written(&receiver, &b, &reargued, "reargued", now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("arguments"), "{}", refused.reason);

        let mut misnamed = caller_context(caller_invoker(&ac), &ac, &a, now);
        misnamed.invoker = Some(tilo_actions::ActionInvoker::person(hex::encode([9; 32])));
        let message = written(&receiver, &b, &misnamed, "misnamed", now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("someone it does not name"), "{}", refused.reason);

        let mut unnamed = caller_context(caller_invoker(&ac), &ac, &a, now);
        unnamed.invoker = None;
        unnamed.signed = None;
        let message = written(&receiver, &b, &unnamed, "unnamed", now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("names no invoker"), "{}", refused.reason);
        assert!(value(&receiver).contains("seed"), "never applied: {}", value(&receiver));
    }

    phase a_revoked_signer_or_a_non_member_or_a_removed_member_is_refused;
    {
        let (root, key, cert) = stranger();
        let (mut receiver, _a, _ac, b, _bc, now) = seeded_with(revoking(&root, &cert));
        let context = caller_context(caller_invoker(&cert), &cert, &key, now);
        let message = written(&receiver, &b, &context, "revoked", now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("revoked"), "{}", refused.reason);

        let (_root, key, cert) = stranger();
        let (mut receiver, _a, _ac, b, _bc, now) = seeded_with(RevocationList::new());
        let context = caller_context(caller_invoker(&cert), &cert, &key, now);
        let message = written(&receiver, &b, &context, "stranger", now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("not granted"), "{}", refused.reason);

        let (mut receiver, a, _ac, b, _bc, now) = seeded_with(RevocationList::new());
        let (key, cert, person) = add_member_with(&mut receiver, &a, now, 31, 32);
        let context = caller_context(caller_invoker(&cert), &cert, &key, now);
        receiver.governance_mut().remove_member(person, &a, now).unwrap();
        let message = written(&receiver, &b, &context, "removed", now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("not granted"), "{}", refused.reason);
        assert!(value(&receiver).contains("seed"), "never applied: {}", value(&receiver));
    }

    phase an_action_write_with_an_empty_stamp_is_refused;
    {
        let (mut receiver, _a, _ac, b, _bc, now) = seeded_with(RevocationList::new());
        let empty = collab_types::Stamp {
            verb: "set".into(),
            invocation: "i1".into(),
            ..collab_types::Stamp::default()
        };
        let message = committed_unclaimed(&receiver, &b, &empty.message(), "stampless", now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(
            refused.reason.contains("names no invoker"),
            "{}",
            refused.reason
        );
        assert!(value(&receiver).contains("seed"), "never applied: {}", value(&receiver));
    }

    phase a_platform_write_stamped_as_the_devices_person_is_admitted;
    {
        let (mut receiver, _a, _ac, b, bc, now) = seeded_with(RevocationList::new());
        // The device writes `$create`, `$settle` and `$settle-block` itself,
        // with no action anyone invoked: its person is their invoker, so the
        // change names who made it and admission keeps it.
        let platform = collab_types::Stamp {
            verb: "$create".into(),
            invoker: hex::encode(bc.user_public_key),
            invocation: "i1".into(),
            ..collab_types::Stamp::default()
        };
        let message = committed(&receiver, &b, &platform.message(), "founded", now);
        take(&mut receiver, message, now).expect("the platform write names the device's person");
        assert!(value(&receiver).contains("founded"), "{}", value(&receiver));
    }

    phase a_refused_change_is_a_verdict_its_author_reads;
    {
        let (mut receiver, a, ac, b, _bc, now) = seeded_with(RevocationList::new());
        let mut context = caller_context(caller_invoker(&ac), &ac, &a, now);
        context.signed = None;
        let message = written(&receiver, &b, &context, "unsigned", now);
        let source = b.verifying_key().to_bytes();
        let error = receiver
            .receive_from(TransportOrigin::ServerDelta(source), message.clone(), now)
            .unwrap_err();
        let crate::AdmissionError::ChangeRefused(refused) = &error else {
            panic!("the change is refused as a change: {error:?}");
        };
        assert_eq!((refused.app_id.as_str(), refused.document_id.as_str()), (APP, RECORD));
        assert!(receiver.workspace_mut().refuse_write(&ac, &message, &error, &a, now).unwrap());
        let held = receiver.workspace().write_refusals();
        assert_eq!(held.len(), 1, "{held:?}");
        assert_eq!(held[0].author, b.verifying_key().to_bytes());
        assert!(held[0].detail.contains("invocation"));
        assert_eq!(held[0].changes, refused.changes);
    }
}
