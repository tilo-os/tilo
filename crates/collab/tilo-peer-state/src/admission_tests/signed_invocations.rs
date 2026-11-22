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

/// The fixture's workspace, its two devices' keys and certificates, and its
/// clock: the copy returned is device `a`'s.
fn seeded_with(
    revocations: RevocationList,
) -> (StatePeer, SigningKey, DeviceCert, SigningKey, DeviceCert, SystemTime) {
    let (a, ac, b, bc, now) = fixtures();
    let mut peer = StatePeer::new(
        WORKSPACE,
        ac.clone(),
        vec![ac.clone(), bc.clone()],
        revocations,
        now,
    )
    .unwrap();
    let mut document = AutoCommit::new();
    document.put(ROOT, "value", "seed").unwrap();
    document.commit_edit("fixture", "value");
    peer.install_seed(
        SeedState {
            workspace_changes: seed_workspace(WORKSPACE, &ac, &[ac.clone(), bc.clone()]).unwrap(),
            documents: vec![SeedDocument {
                app_id: APP.into(),
                document_id: RECORD.into(),
                kind: KIND.into(),
                bytes: document.save(),
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

/// The history entry a host stamps on what it ran for `invoker`: the
/// invocation it admitted, and the invocation as `cert`'s device signed it.
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
) -> NeighbourMsg {
    let mut doc = AutoCommit::load(&copy.document(APP, RECORD).unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        signer.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    doc.put(ROOT, "value", value).unwrap();
    doc.commit_with(CommitOptions::default().with_message(context.change_message().unwrap()));
    let mutation = collab_types::DocumentMutation::Incremental(doc.save_after(&heads));
    let author = collab_types::Ed25519PublicKey::from_bytes(signer.verifying_key().to_bytes());
    let mut envelope = collab_types::SignedOp::new_unsigned(
        mutation.encode().unwrap(),
        collab_types::document_artifact_id(APP, WORKSPACE, RECORD),
        author,
        collab_types::document_attestation_id(APP, WORKSPACE, RECORD),
        collab_types::OpKind::Change,
        SystemTime::UNIX_EPOCH,
        1,
    );
    collab_types::sign_op(&mut envelope, signer).unwrap();
    NeighbourMsg::AppDocument {
        peer: crate::peer_handle(author.as_bytes()),
        app_id: APP.into(),
        cluster_id: WORKSPACE.into(),
        document_id: RECORD.into(),
        envelope: Box::new(envelope),
        own: None,
        readers: None,
    }
}

/// The refusal `receiver` answers for `message`, as its author's session reads
/// it.
fn refused(receiver: &mut StatePeer, message: NeighbourMsg, now: SystemTime) -> crate::ChangeRefused {
    let source = match &message {
        NeighbourMsg::AppDocument { envelope, .. } => *envelope.author.as_bytes(),
        _ => panic!("a write"),
    };
    match receiver.receive_from(TransportOrigin::ServerDelta(source), message, now) {
        Err(crate::AdmissionError::ChangeRefused(refused)) => *refused,
        other => panic!("the change is refused: {other:?}"),
    }
}

/// The refusal names the caller and the invocation, so its author's session
/// reads why.
fn says_the_invocation(refused: &crate::ChangeRefused) {
    assert!(
        refused.reason.contains("invocation") && refused.reason.contains("caller"),
        "{}",
        refused.reason
    );
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

/// A device of a person who is no member of the workspace: its root's key, its
/// device key and its certificate.
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

/// A revocation list refusing `cert`, signed by its person's root.
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
        let message = written(&receiver, &b, &context, "asked for");
        receiver
            .receive_from(
                TransportOrigin::ServerDelta(b.verifying_key().to_bytes()),
                message,
                now,
            )
            .expect("the caller signed the invocation the change keeps");
        assert_eq!(value(&receiver), "asked for");
    }

    phase a_change_made_for_a_caller_without_a_signature_is_refused;
    {
        let (mut receiver, a, ac, b, _bc, now) = seeded_with(RevocationList::new());
        let mut context = caller_context(caller_invoker(&ac), &ac, &a, now);
        context.signed = None;
        let message = written(&receiver, &b, &context, "unsigned");
        let refused = refused(&mut receiver, message, now);
        says_the_invocation(&refused);
        assert_eq!(value(&receiver), "seed", "never applied");
    }

    phase another_invocations_id_is_refused;
    {
        let (mut receiver, a, ac, b, _bc, now) = seeded_with(RevocationList::new());
        let mut context = caller_context(caller_invoker(&ac), &ac, &a, now);
        context.source_invocation = Some(InvocationId::new());
        let message = written(&receiver, &b, &context, "moved");
        let refused = refused(&mut receiver, message, now);
        says_the_invocation(&refused);
        assert_eq!(value(&receiver), "seed", "never applied");
    }

    phase arguments_that_are_not_the_signed_ones_are_refused;
    {
        let (mut receiver, a, ac, b, _bc, now) = seeded_with(RevocationList::new());
        let mut context = caller_context(caller_invoker(&ac), &ac, &a, now);
        context.signed.as_mut().unwrap().argument = Some(InvokeArgument::Text("other".into()));
        let message = written(&receiver, &b, &context, "reargued");
        let refused = refused(&mut receiver, message, now);
        says_the_invocation(&refused);
        assert_eq!(value(&receiver), "seed", "never applied");
    }

    phase a_signature_from_someone_else_than_the_invoker_is_refused;
    {
        let (mut receiver, a, ac, b, _bc, now) = seeded_with(RevocationList::new());
        let mut context = caller_context(caller_invoker(&ac), &ac, &a, now);
        context.invoker = Some(
            tilo_actions::ActionInvoker::person(hex::encode([9; 32])).from_device(Some(
                tilo_actions::InvokerDevice::certified([8; 32], "elsewhere"),
            )),
        );
        let message = written(&receiver, &b, &context, "misnamed");
        let refused = refused(&mut receiver, message, now);
        says_the_invocation(&refused);
        assert_eq!(value(&receiver), "seed", "never applied");
    }

    phase a_revoked_signers_invocation_is_refused;
    {
        let (root, key, cert) = stranger();
        let (mut receiver, _a, _ac, b, _bc, now) = seeded_with(revoking(&root, &cert));
        let context = caller_context(caller_invoker(&cert), &cert, &key, now);
        let message = written(&receiver, &b, &context, "revoked");
        let refused = refused(&mut receiver, message, now);
        says_the_invocation(&refused);
        assert_eq!(value(&receiver), "seed", "never applied");
    }

    phase a_caller_who_is_no_member_is_refused;
    {
        let (root, key, cert) = stranger();
        let _ = root;
        let (mut receiver, _a, _ac, b, _bc, now) = seeded_with(RevocationList::new());
        let context = caller_context(caller_invoker(&cert), &cert, &key, now);
        let message = written(&receiver, &b, &context, "stranger");
        let refused = refused(&mut receiver, message, now);
        assert!(
            refused.reason.contains("not granted"),
            "{}",
            refused.reason
        );
        assert_eq!(value(&receiver), "seed", "never applied");
    }

    phase a_caller_its_person_removed_is_refused;
    {
        let (mut receiver, a, _ac, b, _bc, now) = seeded_with(RevocationList::new());
        let (key, cert, person) = add_member_with(&mut receiver, &a, now, 31, 32);
        let context = caller_context(caller_invoker(&cert), &cert, &key, now);
        receiver
            .governance_mut()
            .remove_member(person, &a, now)
            .unwrap();
        let message = written(&receiver, &b, &context, "removed");
        let refused = refused(&mut receiver, message, now);
        assert!(
            refused.reason.contains("not granted"),
            "{}",
            refused.reason
        );
        assert_eq!(value(&receiver), "seed", "never applied");
    }

    phase a_refused_change_is_a_verdict_its_author_reads;
    {
        let (mut receiver, a, ac, b, _bc, now) = seeded_with(RevocationList::new());
        let mut context = caller_context(caller_invoker(&ac), &ac, &a, now);
        context.signed = None;
        let message = written(&receiver, &b, &context, "unsigned");
        let refused = refused(&mut receiver, message.clone(), now);
        assert_eq!(refused.app_id, APP);
        assert_eq!(refused.document_id, RECORD);
        assert!(!refused.changes.is_empty(), "it names what it refused");
        assert!(
            receiver
                .workspace_mut()
                .refuse_write(&ac, &message, &refused.reason, &a, now)
                .unwrap()
        );
        let held = receiver.workspace().write_refusals();
        assert_eq!(held.len(), 1, "{held:?}");
        assert_eq!(held[0].author, b.verifying_key().to_bytes());
        assert!(held[0].detail.contains("invocation"));
        assert_eq!(held[0].changes, refused.changes);
    }
}
