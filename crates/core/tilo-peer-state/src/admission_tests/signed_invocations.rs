//! A change a host made for a caller on another device carries the invocation
//! that caller signed, and every device that takes the change in checks it.
use super::*;
use automerge::transaction::CommitOptions;
use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget, SignedCall};
use tilo_actions::{
    Acting, ActionAddress, ActionContext, Did, EditOp, FieldEdit, InvocationId, PathStep,
};
use tilo_server_protocol::invocation::sign_invocation_now;
use tilo_user_identity::{
    RevocationEntry, RevocationList, RevocationReason, sign_revocation_entry,
};

const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const APP: &str = "test.app";
const RECORD: &str = "tab:1";
const KIND: &str = "test";
/// The app that keeps the workspace's record of its agents, which its host writes for the
/// person who asks.
const AGENTS_APP: &str = tilo_actions::remote::WORKSPACE_AGENTS_APP;

/// The workspace's record of its agents: its (app, record).
fn agents() -> (&'static str, String) {
    (
        AGENTS_APP,
        collab_types::workspace_document_id(tilo_actions::remote::WORKSPACE_AGENTS_KIND),
    )
}

use crate::fixture::ns as now_ns;

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
    seeded_holding(revocations, false)
}

/// [`seeded_with`], the workspace's record of its agents seeded beside the record when
/// `with_agents`.
fn seeded_holding(revocations: RevocationList, with_agents: bool) -> Seeded {
    seeded_records(revocations, with_agents, &[])
}

/// [`seeded_holding`], with the records `empty` of the app kept as well, empty yet.
fn seeded_records(revocations: RevocationList, with_agents: bool, empty: &[&str]) -> Seeded {
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
    doc.commit_edit("fixture", "value", &ac.user_public_key);
    peer.install_seed(
        SeedState {
            workspace_changes: seed_workspace(WORKSPACE, &ac, &[ac.clone(), bc.clone()]).unwrap(),
            documents: [
                Some(SeedDocument {
                    app_id: APP.into(),
                    document_id: RECORD.into(),
                    kind: KIND.into(),
                    bytes: doc.save(),
                }),
                with_agents.then(|| SeedDocument {
                    app_id: AGENTS_APP.into(),
                    document_id: agents().1,
                    kind: tilo_actions::remote::WORKSPACE_AGENTS_KIND.into(),
                    bytes: doc.save(),
                }),
            ]
            .into_iter()
            .flatten()
            .chain(empty.iter().map(|record| SeedDocument {
                app_id: APP.into(),
                document_id: (*record).into(),
                kind: KIND.into(),
                bytes: AutoCommit::new().save(),
            }))
            .collect(),
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
        asks: false,
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

/// The step `verb` a host takes in the workspace's record of its agents for the
/// `workspace/agents#create` that `cert`'s person signed on their device: the history entry it
/// writes there.
fn agents_step(cert: &DeviceCert, key: &SigningKey, verb: &str, now: SystemTime) -> ActionContext {
    let kind = tilo_actions::remote::WORKSPACE_AGENTS_KIND;
    let invocation = InvocationId::new();
    let mut request = InvokeRequest {
        asks: false,
        invocation,
        target: RemoteActionTarget::workspace_governance(
            WORKSPACE,
            tilo_actions::remote::WORKSPACE_AGENT_CREATE,
        ),
        argument: InvokeArgument::None,
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
        what: Some(Did::Verb(tilo_actions::VerbCall::new(
            tilo_actions::ActionKey::new(AGENTS_APP, kind, verb),
            [],
        ))),
        invoker: Some(caller_invoker(cert)),
        source_invocation: Some(invocation),
        signed: SignedCall::of(&request).map(Box::new),
        ..ActionContext::for_resource(agents().1, kind, "Agents")
    }
}

/// The write of `context` to the workspace's record of its agents by `signer`'s device, as a
/// peer sends it.
fn agents_written(
    copy: &StatePeer,
    signer: &SigningKey,
    context: &ActionContext,
    now: SystemTime,
) -> NeighbourMsg {
    let message = context.change_message().unwrap();
    let (app, record) = agents();
    let mut envelope = unsigned_commit_at(copy, (app, &record), signer, &message, "made", now);
    collab_types::sign_op(&mut envelope, signer).unwrap();
    document_message_at((app, &record), envelope)
}

/// [`agents_written`], as a resource store writes it: the history entry kept in the change's
/// stamp.
fn agents_stamped(
    copy: &StatePeer,
    signer: &SigningKey,
    context: &ActionContext,
    now: SystemTime,
) -> NeighbourMsg {
    let stamp = collab_types::Stamp::of_message(&context.change_message().unwrap()).unwrap();
    let (app, record) = agents();
    let mut envelope = unsigned_commit_at(
        copy,
        (app, &record),
        signer,
        &stamp.message(),
        "stamped",
        now,
    );
    collab_types::sign_op(&mut envelope, signer).unwrap();
    document_message_at((app, &record), envelope)
}

/// One write of the changes `messages` to the workspace's record of its agents by `signer`'s
/// device, in order, as a resource store writes several changes of one call there. Signed
/// without the producer's own check that each change names its call when not `checked`: a
/// misbehaving device sends such a write.
fn agents_stamped_together(
    copy: &StatePeer,
    signer: &SigningKey,
    (messages, checked): (&[String], bool),
    now: SystemTime,
) -> NeighbourMsg {
    let (app, record) = agents();
    let mut doc = AutoCommit::load(&copy.document(app, &record).unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        signer.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    for (at, message) in messages.iter().enumerate() {
        doc.put(ROOT, "value", format!("stamped {at}")).unwrap();
        doc.commit_with(CommitOptions::default().with_message(message.as_str()));
    }
    let author = collab_types::Ed25519PublicKey::from_bytes(signer.verifying_key().to_bytes());
    let mut envelope = collab_types::SignedOp::new_unsigned(
        collab_types::DocumentMutation::Incremental(doc.save_after(&heads))
            .encode()
            .unwrap(),
        collab_types::document_artifact_id(app, WORKSPACE, &record),
        author,
        collab_types::document_attestation_id(app, WORKSPACE, &record),
        collab_types::OpKind::Change,
        now,
        1,
    );
    if checked {
        collab_types::sign_op(&mut envelope, signer).unwrap();
    } else {
        let bytes = envelope.signing_bytes().unwrap();
        let signature = collab_types::signer::sign_now(signer, bytes).unwrap();
        envelope.signature = collab_types::Signature(signature);
    }
    document_message_at((app, &record), envelope)
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
    committed_unclaimed_at(copy, (APP, RECORD), signer, message, value, now)
}

/// [`committed_unclaimed`] to `record` of `app`.
fn committed_unclaimed_at(
    copy: &StatePeer,
    (app, record): (&str, &str),
    signer: &SigningKey,
    message: &str,
    value: &str,
    now: SystemTime,
) -> NeighbourMsg {
    let mut envelope = unsigned_commit_at(copy, (app, record), signer, message, value, now);
    let bytes = envelope.signing_bytes().unwrap();
    let signature = collab_types::signer::sign_now(signer, bytes).unwrap();
    envelope.signature = collab_types::Signature(signature);
    document_message_at((app, record), envelope)
}

/// The unsigned write of the change `message` by `signer`'s device.
fn unsigned_commit(
    copy: &StatePeer,
    signer: &SigningKey,
    message: &str,
    value: &str,
    now: SystemTime,
) -> collab_types::SignedOp {
    unsigned_commit_at(copy, (APP, RECORD), signer, message, value, now)
}

/// The unsigned write of the change `message` to `record` of `app` by `signer`'s device.
fn unsigned_commit_at(
    copy: &StatePeer,
    (app, record): (&str, &str),
    signer: &SigningKey,
    message: &str,
    value: &str,
    now: SystemTime,
) -> collab_types::SignedOp {
    let mut doc = AutoCommit::load(&copy.document(app, record).unwrap()).unwrap();
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
        collab_types::document_artifact_id(app, WORKSPACE, record),
        author,
        collab_types::document_attestation_id(app, WORKSPACE, record),
        collab_types::OpKind::Change,
        now,
        1,
    )
}

/// `envelope` as the document frame a peer sends.
fn document_message(envelope: collab_types::SignedOp) -> NeighbourMsg {
    document_message_at((APP, RECORD), envelope)
}

/// `envelope`, a write of `record` of `app`, as the document frame a peer sends.
fn document_message_at(
    (app, record): (&str, &str),
    envelope: collab_types::SignedOp,
) -> NeighbourMsg {
    NeighbourMsg::AppDocument {
        peer: crate::peer_handle(envelope.author.as_bytes()),
        app_id: app.into(),
        cluster_id: WORKSPACE.into(),
        document_id: record.into(),
        envelope: Box::new(envelope),
        readers: None,
    }
}

/// The write of the change `message` by `signer`'s device as a device that
/// keeps no record keys sends it: its claims carry the call the change was
/// made for, outside the ciphertext, and its payload is a sealed frame this
/// receiver cannot open. The plaintext it seals is returned beside it, as a
/// reader that does hold the keys opens it.
fn sealed_commit(
    copy: &StatePeer,
    signer: &SigningKey,
    message: &str,
    value: &str,
    now: SystemTime,
) -> (collab_types::SignedOp, collab_types::DocumentMutation) {
    let mut envelope = unsigned_commit(copy, signer, message, value, now);
    let mutation =
        collab_types::DocumentMutation::decode(&envelope.payload).expect("an incremental write");
    let collab_types::DocumentMutation::Incremental(bytes) = &mutation else {
        panic!("an incremental write");
    };
    let changes = collab_admission::parse_changes(bytes).expect("the changes parse");
    envelope.claims =
        collab_types::claim::claims_of(&changes, |_| Vec::new()).expect("every change is claimed");
    envelope.encryption = Some(collab_types::EncryptionMetadata::content_key(
        1,
        collab_types::ContentRetention::Delta,
    ));
    envelope.payload = b"a sealed change this device cannot open".to_vec();
    collab_types::sign_op(&mut envelope, signer).expect("the device signs what it sends");
    (envelope, mutation)
}

/// The write of the change `message` by `signer`'s device, sealed, as the
/// document frame a key-less device keeps and forwards.
fn sealed(
    copy: &StatePeer,
    signer: &SigningKey,
    message: &str,
    value: &str,
    now: SystemTime,
) -> (NeighbourMsg, collab_types::DocumentMutation) {
    let (envelope, mutation) = sealed_commit(copy, signer, message, value, now);
    (document_message(envelope), mutation)
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
    value_at(peer, RECORD)
}

fn value_at(peer: &StatePeer, record: &str) -> String {
    AutoCommit::load(&peer.document(APP, record).unwrap())
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
    let cert = crate::fixture::cert(
        &root,
        &key,
        tilo_user_identity::DeviceCapabilities::member(),
    );
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

    phase a_change_naming_a_member_its_device_may_not_write_for_is_refused;
    {
        let (mut receiver, a, _ac, b, _bc, now) = seeded_with(RevocationList::new());
        let (key, cert, person) = add_member_with(&mut receiver, &a, now, 31, 32);
        // A device of the owner's writes a change naming another member, who
        // asked for nothing: the named member is not one that device runs.
        let mut context = caller_context(
            tilo_actions::ActionInvoker::person(hex::encode(person)),
            &cert,
            &key,
            now,
        );
        context.signed = None;
        let message = written(&receiver, &b, &context, "for another member", now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("not granted"), "{}", refused.reason);
        assert!(value(&receiver).contains("seed"), "never applied: {}", value(&receiver));
    }

    phase a_change_outside_the_callers_grants_is_refused;
    {
        let (mut receiver, a, _ac, b, _bc, now) = seeded_with(RevocationList::new());
        let (key, cert, person) = add_member_with(&mut receiver, &a, now, 31, 32);
        receiver
            .governance_mut()
            .set_member_role(person, crate::join::Role::Viewer, &a, now)
            .unwrap();
        // The caller signed the invocation, and reads the record: a field
        // edit is outside what they are granted.
        let context = caller_context(caller_invoker(&cert), &cert, &key, now);
        let message = written(&receiver, &b, &context, "outside the grants", now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("not granted"), "{}", refused.reason);
        assert!(value(&receiver).contains("seed"), "never applied: {}", value(&receiver));
    }

    phase a_carried_call_signed_by_another_key_is_refused;
    {
        let (mut receiver, a, _ac, b, _bc, now) = seeded_with(RevocationList::new());
        let (key, cert, _person) = add_member_with(&mut receiver, &a, now, 31, 32);
        // The change names the owner, and the invocation kept beside it is
        // another member's, signed with that member's key.
        let context = caller_context(
            tilo_actions::ActionInvoker::person(hex::encode(receiver.owner())).from_device(Some(
                tilo_actions::InvokerDevice::certified(cert.device_public_key, "their device"),
            )),
            &cert,
            &key,
            now,
        );
        let message = written(&receiver, &b, &context, "not the caller", now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("does not check"), "{}", refused.reason);
        assert!(value(&receiver).contains("seed"), "never applied: {}", value(&receiver));
    }

    phase a_change_wider_than_its_carried_call_is_refused;
    {
        let (mut receiver, a, _ac, b, _bc, now) = seeded_with(RevocationList::new());
        let (key, cert, _person) = add_member_with(&mut receiver, &a, now, 31, 32);
        // The call names one record, and the change writes another.
        let mut context = caller_context(caller_invoker(&cert), &cert, &key, now);
        context.resource = Some(tilo_actions::ActionResource {
            id: "tab:2".into(),
            kind: KIND.into(),
            label: String::new(),
        });
        let message = written(&receiver, &b, &context, "wider", now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("another action"), "{}", refused.reason);
        assert!(value(&receiver).contains("seed"), "never applied: {}", value(&receiver));
    }

    phase a_declared_step_counts_only_from_the_device_that_keeps_the_record;
    {
        let (mut receiver, a, _ac, b, _bc, now) = seeded_holding(RevocationList::new(), true);
        let (key, cert, _person) = add_member_with(&mut receiver, &a, now, 31, 32);
        // A member asked the workspace to make an agent, and the step `create` declares is
        // written in the workspace's record of its agents. Sent by a device that does not keep
        // the workspace's records, the same carried call is refused: a member's device takes no
        // step for a call but the call's own.
        let making = agents_step(&cert, &key, "create-agent", now);
        let message = agents_written(&receiver, &key, &making, now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("another action"), "{}", refused.reason);
        // From a device of the owner's, which keeps them, a step `create` does not declare is
        // refused too.
        let retiring = agents_step(&cert, &key, "retire-agent", now);
        let message = agents_written(&receiver, &b, &retiring, now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("another action"), "{}", refused.reason);
        // A resource store keeps the call in the change's stamp, and the change is held to the
        // same declaration: from the device that keeps the record, a step `create` does not
        // declare is refused, and the one it declares is admitted.
        let retiring = agents_step(&cert, &key, "retire-agent", now);
        let message = agents_stamped(&receiver, &b, &retiring, now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("another action"), "{}", refused.reason);
        let making = agents_step(&cert, &key, "create-agent", now);
        let message = agents_stamped(&receiver, &b, &making, now);
        take(&mut receiver, message, now).expect("the step the called action declares");
    }

    phase a_store_change_reads_the_call_it_names_from_its_write;
    {
        let (mut receiver, a, _ac, b, _bc, now) = seeded_holding(RevocationList::new(), true);
        let (key, cert, _person) = add_member_with(&mut receiver, &a, now, 31, 32);
        // A resource store's call that writes several changes of one record keeps itself in
        // the first, and the others name it by digest.
        let making = agents_step(&cert, &key, "create-agent", now);
        let mut keeping =
            collab_types::Stamp::of_message(&making.change_message().unwrap()).unwrap();
        if let Some(context) = keeping.context.as_mut() {
            context.signed = None;
        }
        let mut naming = keeping.clone();
        keeping.name_call(false);
        naming.name_call(true);
        assert!(naming.signed.is_none() && naming.call == keeping.call);
        // Sent without the change that keeps it, the call it names is missing: refused.
        let message = agents_stamped_together(&receiver, &b, (&[naming.message()], false), now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("not kept"), "{}", refused.reason);
        // A change that keeps another call than the one it names is refused.
        let mut forged = keeping.clone();
        forged.signed.as_mut().unwrap().argument = Some(InvokeArgument::Text("other".into()));
        let message = agents_stamped_together(&receiver, &b, (&[forged.message()], false), now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("not the one"), "{}", refused.reason);
        // With the change that keeps it, in the same write: both are the call's, and admitted.
        let message = agents_stamped_together(
            &receiver,
            &b,
            (&[keeping.message(), naming.message()], true),
            now,
        );
        take(&mut receiver, message, now).expect("the call the write keeps");
    }

    phase an_undo_outside_what_its_call_declares_is_refused;
    {
        let (mut receiver, a, ac, b, _bc, now) = seeded_with(RevocationList::new());
        // A person asked to take changes back as moderation does, which declares an undo in the
        // chat's record only. The device that keeps the records writes that undo in another
        // app's record: it is outside what the call declares, refused, and the record keeps
        // what it held.
        let mut undoing = caller_context(caller_invoker(&ac), &ac, &a, now);
        let invocation = InvocationId::new();
        let mut request = InvokeRequest {
            asks: false,
            invocation,
            target: RemoteActionTarget {
                provider: tilo_actions::remote::MODERATION_PROVIDER.into(),
                address: ActionAddress::new(
                    tilo_actions::remote::MODERATION_HISTORY,
                    tilo_actions::remote::MODERATION_UNDO,
                ),
            },
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        request.signed = Some(Box::new(
            sign_invocation_now(WORKSPACE, &request, &ac, &a, now_ns(now)).unwrap(),
        ));
        undoing.source_invocation = Some(invocation);
        undoing.signed = SignedCall::of(&request).map(Box::new);
        undoing.what = Some(Did::History(tilo_actions::HistoryStep {
            op: tilo_actions::HistoryOp::Undo,
            steps: 1,
            of: None,
            undoes: Some("a post".into()),
            verb: "send".into(),
        }));
        let message = written(&receiver, &b, &undoing, "taken back", now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("another action"), "{}", refused.reason);
        assert!(value(&receiver).contains("seed"), "never applied: {}", value(&receiver));
    }

    phase an_agent_its_device_runs_is_admitted;
    {
        let (mut receiver, _a, _ac, b, bc, now) = seeded_with(RevocationList::new());
        let owner = hex::encode(bc.user_public_key);
        // The owner's device ran the agent it handed the session to, and
        // signed what it changed: the member is one the device runs, so it
        // needs no carried call.
        let mut context = caller_context(
            tilo_actions::ActionInvoker::admitted(
                &tilo_member_types::Acting::handed_agent("scribe", owner),
            ),
            &bc,
            &b,
            now,
        );
        context.signed = None;
        let message = written(&receiver, &b, &context, "her agent's", now);
        take(&mut receiver, message, now).expect("the device writes for the agent it runs");
        assert!(value(&receiver).contains("her agent's"), "{}", value(&receiver));
    }

    phase an_agents_store_write_is_admitted_on_how_it_says_the_agent_acted;
    {
        let (mut receiver, _a, _ac, b, bc, now) = seeded_with(RevocationList::new());
        let owner = hex::encode(bc.user_public_key);
        let scribe = tilo_member_types::Actor::Member(tilo_member_types::MemberId::agent("scribe", owner.as_str()));
        let stamp = |acting: Option<tilo_member_types::Acting>| collab_types::Stamp {
            verb: "set".into(),
            invoker: collab_types::Stamp::by(&scribe),
            acted_for: Some(owner.clone()),
            invocation: InvocationId::new().to_string(),
            context: acting.map(|acting| {
                Box::new(ActionContext {
                    invoker: Some(tilo_actions::ActionInvoker::admitted(&acting)),
                    ..ActionContext::default()
                })
            }),
            ..collab_types::Stamp::default()
        };
        // A store write naming an agent beside its person, keeping no call, that records nothing
        // of how the agent acted: nothing shows the agent asked, so it is refused.
        let unsaid = committed(&receiver, &b, &stamp(None).message(), "unsaid", now);
        let refused = take(&mut receiver, unsaid, now).unwrap_err();
        assert!(refused.reason.contains("own certified key"), "{}", refused.reason);
        // One recording an agent with a key of its own, keeping no call it signed: refused.
        let own_key = tilo_member_types::Acting::agent("scribe", owner.as_str())
            .acting_for(tilo_member_types::Acting::person(owner.as_str()));
        let unsigned = committed(&receiver, &b, &stamp(Some(own_key)).message(), "unsigned", now);
        let refused = take(&mut receiver, unsigned, now).unwrap_err();
        assert!(refused.reason.contains("own certified key"), "{}", refused.reason);
        assert!(value(&receiver).contains("seed"), "never applied: {}", value(&receiver));
        // One recording the agent its person's device handed its session to: the device answers
        // for it, and it is admitted.
        let handed = tilo_member_types::Acting::handed_agent("scribe", owner.clone());
        let message = committed(&receiver, &b, &stamp(Some(handed)).message(), "handed", now);
        take(&mut receiver, message, now).expect("the device writes for the agent it runs");
        assert!(value(&receiver).contains("handed"), "{}", value(&receiver));
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
            invoker: collab_types::Stamp::by(&tilo_member_types::Actor::Member(
                tilo_member_types::MemberId::person(hex::encode(bc.user_public_key)),
            )),
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

    phase a_keyless_device_admits_a_sealed_change_made_for_a_caller;
    {
        // A device that keeps the sealed op without opening it cannot see the
        // call the change keeps in its ciphertext: the claim carries it too.
        let (mut receiver, a, _ac, b, _bc, now) = seeded_with(RevocationList::new());
        let (key, cert, _person) = add_member_with(&mut receiver, &a, now, 31, 32);
        let context = caller_context(caller_invoker(&cert), &cert, &key, now);
        let (message, _) = sealed(
            &receiver,
            &b,
            &context.change_message().unwrap(),
            "asked for",
            now,
        );
        take(&mut receiver, message, now).expect("the carried call proves who asked");
    }

    phase a_sealed_change_whose_carried_call_another_key_signed_is_refused;
    {
        let (mut receiver, a, _ac, b, _bc, now) = seeded_with(RevocationList::new());
        let (key, cert, _person) = add_member_with(&mut receiver, &a, now, 31, 32);
        // The change names the owner, and the call carried beside it is
        // another member's, signed with that member's key.
        let context = caller_context(
            tilo_actions::ActionInvoker::person(hex::encode(receiver.owner())).from_device(Some(
                tilo_actions::InvokerDevice::certified(cert.device_public_key, "their device"),
            )),
            &cert,
            &key,
            now,
        );
        let (message, _) = sealed(
            &receiver,
            &b,
            &context.change_message().unwrap(),
            "not the caller",
            now,
        );
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("does not check"), "{}", refused.reason);
    }

    phase a_sealed_change_naming_another_member_without_a_call_is_refused;
    {
        let (mut receiver, a, _ac, b, _bc, now) = seeded_with(RevocationList::new());
        let (key, cert, _person) = add_member_with(&mut receiver, &a, now, 31, 32);
        let mut context = caller_context(caller_invoker(&cert), &cert, &key, now);
        context.signed = None;
        let (message, _) = sealed(
            &receiver,
            &b,
            &context.change_message().unwrap(),
            "unproven",
            now,
        );
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(
            refused.reason.contains("carries no invocation"),
            "{}",
            refused.reason
        );
    }

    phase placed_work_syncs_through_a_device_without_the_records_keys;
    {
        // A host device runs a caller's action and writes the change; a server
        // that holds none of the record's keys keeps and forwards it; a device
        // that holds them opens it and finds the call the claim carried.
        let (mut server, a, _ac, b, _bc, now) = seeded_with(RevocationList::new());
        let (key, cert, _person) = add_member_with(&mut server, &a, now, 31, 32);
        let context = caller_context(caller_invoker(&cert), &cert, &key, now);
        let (envelope, mutation) = sealed_commit(
            &server,
            &b,
            &context.change_message().unwrap(),
            "placed",
            now,
        );
        take(
            &mut server,
            document_message(envelope.clone()),
            now,
        )
        .expect("a server without the keys keeps the sealed write");

        let (mut reader, reader_authority, _reader_cert, _b, _bc, now) =
            seeded_with(RevocationList::new());
        add_member_with(&mut reader, &reader_authority, now, 31, 32);
        take(&mut reader, document_message(envelope.clone()), now)
            .expect("a reader takes the sealed write in");
        collab_admission::check_opened_mutation(&envelope, &mutation)
            .expect("the call opened to is the one the claim carried");
    }
}

/// A change that keeps no history entry, no stamp and no framework name says nothing of who made
/// it, and is refused as every change that names no one. So is a window's direct edit that names
/// no one; one naming its maker is theirs.
#[test]
fn a_change_naming_nothing_is_refused_and_a_native_edit_names_its_maker() {
    let (mut receiver, _a, _ac, b, bc, now) = seeded_with(RevocationList::new());
    let plain = committed_unclaimed(&receiver, &b, "an edit", "plain", now);
    let refused = take(&mut receiver, plain, now).unwrap_err();
    assert!(
        refused.reason.contains("names no invoker"),
        "{}",
        refused.reason
    );
    assert_eq!(value(&receiver), "\"seed\"", "never applied");

    let native = committed_unclaimed(
        &receiver,
        &b,
        &tilo_actions::FrameworkChange::Native.change_message(),
        "native",
        now,
    );
    let refused = take(&mut receiver, native, now).unwrap_err();
    assert!(
        refused.reason.contains("names no invoker"),
        "{}",
        refused.reason
    );
    assert_eq!(value(&receiver), "\"seed\"", "never applied");

    // The same edit naming who made it is theirs.
    let named = ActionContext::whole_edit_by(&Acting::person(hex::encode(bc.user_public_key)));
    let message = committed_unclaimed(&receiver, &b, &named, "named", now);
    take(&mut receiver, message, now).expect("an edit naming its maker is theirs");
    assert_eq!(value(&receiver), "\"named\"");
}

/// A record's step to its storage version is one every holder derives alike: it names no one,
/// and is taken in as the step its kind and id derive. A change that names nothing else is
/// still refused.
#[test]
fn a_records_derived_version_step_names_no_one_and_is_taken_in() {
    let (mut receiver, _a, _ac, b, _bc, now) = seeded_with(RevocationList::new());
    let step = ActionContext::for_resource(RECORD, KIND, "Seed").with_described(
        tilo_actions::Described::new(tilo_actions::Did::history(
            tilo_actions::HistoryOp::Migrate {
                to: 1,
                rewrite: false,
            },
            0,
            None,
        )),
    );
    let message = committed_unclaimed(
        &receiver,
        &b,
        &step.change_message().unwrap(),
        "stepped",
        now,
    );
    take(&mut receiver, message, now).expect("a derived version step is every holder's");
    assert_eq!(value(&receiver), "\"stepped\"");
}

/// A change named the record's genesis that is not the founding every holder derives names no
/// one, so it is decided as its signer's person: a record's first content taken from an editor,
/// refused from a member the workspace lets only read.
#[test]
fn a_genesis_that_is_not_the_derived_founding_is_decided_as_its_signers_person() {
    let (mut receiver, a, _ac, b, _bc, now) =
        seeded_records(RevocationList::new(), false, &["tab:2", "tab:3"]);
    let genesis = tilo_actions::FrameworkChange::Genesis.change_message();
    let edited =
        committed_unclaimed_at(&receiver, (APP, "tab:2"), &b, &genesis, "by an editor", now);
    take(&mut receiver, edited, now).expect("an editor's first content is theirs");
    assert_eq!(value_at(&receiver, "tab:2"), "\"by an editor\"");

    let (key, _cert, person) = add_member_with(&mut receiver, &a, now, 41, 42);
    assert!(
        receiver
            .governance_mut()
            .set_member_role(person, crate::join::Role::Viewer, &a, now)
            .unwrap()
    );
    let dressed = committed_unclaimed_at(
        &receiver,
        (APP, "tab:3"),
        &key,
        &genesis,
        "by a viewer",
        now,
    );
    let refused = take(&mut receiver, dressed, now).unwrap_err();
    assert!(refused.reason.contains("not granted"), "{}", refused.reason);
    assert!(
        AutoCommit::load(&receiver.document(APP, "tab:3").unwrap())
            .unwrap()
            .get_heads()
            .is_empty(),
        "never applied"
    );
}

/// A record's genesis is its first change. Named on a later edit of a record that has what
/// someone made, it would be decided by the grant to create the record rather than against the
/// edits the record's app publishes, and let any writer rewrite the record whole: refused, from
/// an editor too, while the record's first content and a later edit of it named as an edit are
/// taken.
#[test]
fn a_genesis_is_taken_only_as_a_records_first_change() {
    let (mut receiver, _a, _ac, b, bc, now) =
        seeded_records(RevocationList::new(), false, &["tab:2"]);
    let genesis = tilo_actions::FrameworkChange::Genesis.change_message();
    let rewrite = committed_unclaimed(&receiver, &b, &genesis, "rewritten", now);
    let refused = take(&mut receiver, rewrite, now).unwrap_err();
    assert!(
        refused.reason.contains("genesis is its first change"),
        "{}",
        refused.reason
    );
    assert_eq!(value(&receiver), "\"seed\"", "never applied");

    let first = committed_unclaimed_at(&receiver, (APP, "tab:2"), &b, &genesis, "first", now);
    take(&mut receiver, first, now).expect("a record's first content may be its genesis");
    assert_eq!(value_at(&receiver, "tab:2"), "\"first\"");

    let again = committed_unclaimed_at(&receiver, (APP, "tab:2"), &b, &genesis, "again", now);
    let refused = take(&mut receiver, again, now).unwrap_err();
    assert!(
        refused.reason.contains("genesis is its first change"),
        "{}",
        refused.reason
    );
    assert_eq!(value_at(&receiver, "tab:2"), "\"first\"", "never applied");

    let native = ActionContext::whole_edit_by(&Acting::person(hex::encode(bc.user_public_key)));
    let mut edited = unsigned_commit_at(&receiver, (APP, "tab:2"), &b, &native, "edited", now);
    edited.seq = 2;
    collab_types::sign_op(&mut edited, &b).unwrap();
    take(
        &mut receiver,
        document_message_at((APP, "tab:2"), edited),
        now,
    )
    .expect("a later edit named as one is taken");
    assert_eq!(value_at(&receiver, "tab:2"), "\"edited\"");
}

/// A write of `signer`'s device at `seq` whose change names no one: a resource store's stamp with
/// no invoker, as a history kept before every change named who made it holds.
fn nameless_at(
    copy: &StatePeer,
    signer: &SigningKey,
    value: &str,
    seq: u64,
    now: SystemTime,
) -> NeighbourMsg {
    let nameless = collab_types::Stamp {
        verb: "set".into(),
        invocation: format!("i{seq}"),
        ..collab_types::Stamp::default()
    };
    let mut envelope = unsigned_commit(copy, signer, &nameless.message(), value, now);
    envelope.seq = seq;
    let bytes = envelope.signing_bytes().unwrap();
    envelope.signature =
        collab_types::Signature(collab_types::signer::sign_now(signer, bytes).unwrap());
    document_message(envelope)
}

/// A history one of the person's own devices kept before every change named who made it comes
/// in with one import step: what it made naming no one is the person's, the step that said so is
/// kept in the workspace's governance, and a later write naming no one is refused as any is.
#[test]
fn an_imported_historys_nameless_changes_are_the_persons_and_a_later_one_is_refused() {
    let (mut receiver, a, ac, b, bc, now) = seeded_with(RevocationList::new());
    let person = ac.user_public_key;
    let record = neighbour_wire::app_document_group(APP, RECORD);
    let through = |seq| {
        vec![crate::Imported {
            device: bc.device_public_key,
            record,
            seq,
        }]
    };
    let kept = nameless_at(&receiver, &b, "kept", 1, now);
    let refused = take(&mut receiver, kept, now).unwrap_err();
    assert!(
        refused.reason.contains("names no invoker"),
        "{}",
        refused.reason
    );

    // Device a imports what device b kept, through b's third write.
    let step = crate::ImportedHistory::signed(WORKSPACE, person, through(3), &a);
    assert!(
        receiver
            .governance_mut()
            .author_imported_history(&step, &a, now)
            .unwrap()
    );
    assert_eq!(
        receiver.governance().imported_histories(),
        vec![step.clone()]
    );
    assert!(
        !receiver
            .governance_mut()
            .author_imported_history(&step, &a, now)
            .unwrap(),
        "one import is one step"
    );

    let imported = nameless_at(&receiver, &b, "imported", 2, now);
    take(&mut receiver, imported, now).unwrap();
    assert_eq!(value(&receiver), "\"imported\"");
    assert_eq!(
        receiver
            .governance()
            .imported_as(&bc.device_public_key, &record, 2),
        Some(person),
        "what the imported write made naming no one is the person's"
    );

    // A later write that names no one is no part of the import.
    let later = nameless_at(&receiver, &b, "later", 4, now);
    let refused = take(&mut receiver, later, now).unwrap_err();
    assert!(
        refused.reason.contains("names no invoker"),
        "{}",
        refused.reason
    );
    assert_eq!(value(&receiver), "\"imported\"");

    // Only a device of the person signs a step for them.
    let (_root, key, _cert) = stranger();
    let forged = crate::ImportedHistory::signed(WORKSPACE, person, through(9), &key);
    assert!(
        receiver
            .governance_mut()
            .author_imported_history(&forged, &a, now)
            .is_err()
    );
    let mut widened = step;
    widened.through = through(9);
    assert!(
        receiver
            .governance_mut()
            .author_imported_history(&widened, &a, now)
            .is_err(),
        "a step changed after it was signed is not one"
    );
}

/// The change of the record's value to `value`, committed with `message` (none when `None`), as
/// the bytes a device's own writer is handed.
fn edit_of(copy: &StatePeer, message: Option<&str>, value: &str) -> Vec<u8> {
    let mut doc = AutoCommit::load(&copy.document(APP, RECORD).unwrap()).unwrap();
    let heads = doc.get_heads();
    doc.put(ROOT, "value", value).unwrap();
    let options = CommitOptions::default();
    doc.commit_with(match message {
        Some(message) => options.with_message(message),
        None => options,
    });
    doc.save_after(&heads)
}

/// The keys the workspace's members seal its records under.
static KEYS: std::sync::LazyLock<key_group::ContentKeys> =
    std::sync::LazyLock::new(|| crate::sealed_records::members_keys(WORKSPACE));

/// The record's value as a member reads it, its sealed writes opened with [`KEYS`].
fn opened_value(peer: &StatePeer) -> String {
    let record = crate::sealed_records::opened(peer, &KEYS, (APP, RECORD)).unwrap();
    AutoCommit::load(&record)
        .unwrap()
        .get(ROOT, "value")
        .unwrap()
        .unwrap()
        .0
        .to_string()
}

/// `changes` as the device `cert` writes them through its own writer, sealed under the members'
/// [`KEYS`] and signed by its key.
fn upkeep(
    copy: &StatePeer,
    key: &SigningKey,
    cert: &DeviceCert,
    changes: &[u8],
    now: SystemTime,
) -> Result<NeighbourMsg, crate::owner::WriteRefusal> {
    let (envelopes, _) = crate::owner::draft_local_write(
        copy,
        WORKSPACE,
        &cert.device_public_key,
        crate::owner::LocalWrite {
            app: APP,
            document: RECORD,
            readers: None,
            changes,
            founding: false,
            snapshot: false,
            sealed: Some(&KEYS),
            commit: None,
            now,
        },
    )?;
    let [mut envelope] = <[_; 1]>::try_from(envelopes).expect("one frame");
    collab_types::sign_op(&mut envelope, key).unwrap();
    Ok(document_message(envelope))
}

/// A server keeping a workspace writes its upkeep as the person whose device it is, signed by the
/// device's key: its own writer never signs a change that names no one, what it signs is decided
/// with that person's role, and a change that names the bare device key as who made it is refused,
/// for a device is no member.
#[test]
fn a_servers_upkeep_is_its_persons_and_signed_by_its_device() {
    let (mut receiver, a, _ac, _b, _bc, now) = seeded_with(RevocationList::new());
    let (server_key, server_cert, person) = add_member_with(&mut receiver, &a, now, 51, 52);
    // A device that keeps a record's sealed writes keeps the exact evidence it took them on.
    receiver.select_exact_admissions().unwrap();

    let refused = upkeep(
        &receiver,
        &server_key,
        &server_cert,
        &edit_of(&receiver, None, "bare"),
        now,
    )
    .expect_err("the writer signs no change that names no one");
    assert!(
        matches!(&refused, crate::owner::WriteRefusal::Refused(why) if why.contains("names no one")),
        "{refused:?}"
    );

    let native = ActionContext::whole_edit_by(&Acting::person(hex::encode(person)));
    let kept = upkeep(
        &receiver,
        &server_key,
        &server_cert,
        &edit_of(&receiver, Some(&native), "kept"),
        now,
    )
    .expect("the server's upkeep is drafted");
    take(&mut receiver, kept, now).expect("the upkeep is its person's, an editor");
    assert_eq!(opened_value(&receiver), "\"kept\"");

    // Named as the bare device key, the change names no member at all.
    let as_device = ActionContext {
        invoker: Some(tilo_actions::ActionInvoker::person(hex::encode(
            server_cert.device_public_key,
        ))),
        ..ActionContext::for_resource(RECORD, KIND, "Test")
    };
    // Its writer refuses to sign it, and a receiver handed it anyway refuses it.
    let named_device = upkeep(
        &receiver,
        &server_key,
        &server_cert,
        &edit_of(
            &receiver,
            Some(&as_device.change_message().unwrap()),
            "as the device",
        ),
        now,
    );
    let refused = match named_device {
        Ok(message) => take(&mut receiver, message, now).unwrap_err().reason,
        Err(refusal) => format!("{refusal:?}"),
    };
    assert!(
        refused.contains("not granted"),
        "a device is no member: {refused}"
    );
    assert_eq!(opened_value(&receiver), "\"kept\"", "never applied");

    // The person reads only now: the same upkeep is decided with the person's role.
    assert!(
        receiver
            .governance_mut()
            .set_member_role(person, crate::join::Role::Viewer, &a, now)
            .unwrap()
    );
    let read_only = upkeep(
        &receiver,
        &server_key,
        &server_cert,
        &edit_of(&receiver, Some(&native), "after"),
        now,
    );
    let refused = match read_only {
        Ok(message) => take(&mut receiver, message, now).unwrap_err().reason,
        Err(refusal) => format!("{refusal:?}"),
    };
    assert!(refused.contains("not granted"), "{refused}");
    assert_eq!(opened_value(&receiver), "\"kept\"", "never applied");
}

/// The app whose records are the workspace's folder, and the record of its tree.
const FILES: &str = tilo_app_id::apps::FILE_EXPLORER;
const FILES_RECORD: &str = "files-tree";

/// The fixture's workspace, its folder's tree kept beside the test record.
fn seeded_with_files() -> Seeded {
    seeded_with_files_holding(&[])
}

/// [`seeded_with_files`], with `more` devices on the workspace's roster too.
fn seeded_with_files_holding(more: &[DeviceCert]) -> Seeded {
    let (a, ac, b, bc, now) = fixtures();
    let roster: Vec<_> = [ac.clone(), bc.clone()]
        .into_iter()
        .chain(more.iter().cloned())
        .collect();
    let mut peer = StatePeer::new(
        WORKSPACE,
        ac.clone(),
        roster.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "value", "seed").unwrap();
    doc.commit_edit("fixture", "value", &ac.user_public_key);
    crate::fixture::found(
        &mut peer,
        (&a, &ac),
        &roster,
        vec![crate::fixture::document(
            FILES,
            FILES_RECORD,
            collab_types::FILES_TREE_KIND,
            doc.save(),
        )],
        now,
    );
    (peer, a, ac, b, bc, now)
}

/// The workspace's `open` of Files given `argument`, as `cert`'s device signed it.
fn files_open(
    argument: &serde_json::Value,
    cert: &DeviceCert,
    key: &SigningKey,
    now: SystemTime,
) -> InvokeRequest {
    let mut request = InvokeRequest {
        asks: false,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: tilo_actions::remote::workspace_provider(WORKSPACE),
            address: ActionAddress::new(
                format!(
                    "{}{FILES}",
                    tilo_actions::remote::APPLICATION_RESOURCE_PREFIX
                ),
                tilo_actions::remote::APPLICATION_OPEN_ACTION,
            ),
        },
        argument: InvokeArgument::Json(argument.as_object().cloned().unwrap()),
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
    request
}

/// The change of the workspace's tree a host makes for `request`: the tree's verbs `verbs`,
/// done for `invoker`, keeping the request as its caller signed it.
fn tree_change(
    request: &InvokeRequest,
    invoker: tilo_actions::ActionInvoker,
    verbs: &[&'static str],
) -> ActionContext {
    let kind = collab_types::FILES_TREE_KIND;
    let did = |verb: &str| {
        Did::Verb(tilo_actions::VerbCall::new(
            tilo_actions::ActionKey::new(FILES, kind, verb),
            [],
        ))
    };
    let what = match verbs {
        [verb] => did(verb),
        verbs => Did::Group(tilo_actions::GroupOf {
            members: verbs.iter().map(|verb| did(verb)).collect(),
            count: u32::try_from(verbs.len()).unwrap(),
            distinct: verbs.iter().map(|verb| did(verb)).collect(),
        }),
    };
    ActionContext {
        what: Some(what),
        invoker: Some(invoker),
        source_invocation: Some(request.invocation),
        signed: SignedCall::of(request).map(Box::new),
        ..ActionContext::for_resource(FILES_RECORD, kind, "Files")
    }
}

/// The write of the tree change `context` by `signer`'s device, as a peer sends it.
fn tree_written(
    copy: &StatePeer,
    signer: &SigningKey,
    context: &ActionContext,
    value: &str,
    now: SystemTime,
) -> NeighbourMsg {
    let mut envelope = unsigned_commit_at(
        copy,
        (FILES, FILES_RECORD),
        signer,
        &context.change_message().unwrap(),
        value,
        now,
    );
    collab_types::sign_op(&mut envelope, signer).unwrap();
    document_message_at((FILES, FILES_RECORD), envelope)
}

fn tree_value(peer: &StatePeer) -> String {
    AutoCommit::load(&peer.document(FILES, FILES_RECORD).unwrap())
        .unwrap()
        .get(ROOT, "value")
        .unwrap()
        .unwrap()
        .0
        .to_string()
}

/// The workspace's lasting grant letting the agent `id` of `person` write the folder's files,
/// written by the owner's device `authority`.
fn grant_agent_files(
    peer: &mut StatePeer,
    authority: &SigningKey,
    id: &str,
    person: &str,
    now: SystemTime,
) {
    peer.governance_mut()
        .put_member_grants(
            &[tilo_action_policy::Grant::new(
                format!("{id}-files"),
                tilo_action_policy::Grantee::agent(id, person),
                tilo_action_policy::TreePath::workspace(WORKSPACE).app(FILES),
                tilo_action_policy::KindSet::READ_UNDOABLE,
                tilo_action_policy::Answer::Allow,
                tilo_action_policy::Grantee::person(person),
            )],
            authority,
            now,
        )
        .unwrap();
}

tilo_test_env::scenario! {
    fn a_put_into_the_workspaces_folder_carries_the_open_its_caller_signed;

    phase the_tree_change_of_a_signed_open_given_content_is_admitted;
    {
        let (mut receiver, a, ac, b, _bc, now) = seeded_with_files();
        // The person asked their device to put two files into a new folder; the device that
        // keeps the tree made it as one change of the tree, and another replica takes it in.
        let request = files_open(
            &serde_json::json!({"files": [
                {"content": "tilo.content:aa", "name": "a.md", "dir": "Inbox"},
                {"content": "tilo.content:bb", "name": "b.md", "dir": "Inbox"},
            ]}),
            &ac,
            &a,
            now,
        );
        let context = tree_change(&request, caller_invoker(&ac), &["new-folder", "new-file"]);
        let message = tree_written(&receiver, &b, &context, "put", now);
        take(&mut receiver, message, now).expect("the person signed the open the change keeps");
        assert!(tree_value(&receiver).contains("put"), "{}", tree_value(&receiver));
    }

    phase a_tampered_argument_or_another_signer_is_refused;
    {
        let (mut receiver, a, ac, b, _bc, now) = seeded_with_files();
        let request = files_open(
            &serde_json::json!({"content": "tilo.content:aa", "name": "a.md"}),
            &ac,
            &a,
            now,
        );
        // The host kept other content than the person signed for.
        let mut tampered = tree_change(&request, caller_invoker(&ac), &["new-file"]);
        tampered.signed.as_mut().unwrap().argument = Some(InvokeArgument::Json(
            serde_json::json!({"content": "tilo.content:ff", "name": "a.md"})
                .as_object()
                .cloned()
                .unwrap(),
        ));
        let message = tree_written(&receiver, &b, &tampered, "tampered", now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("arguments"), "{}", refused.reason);

        // Another member signed the open, and the change names the owner as having asked.
        let (key, cert, _person) = add_member_with(&mut receiver, &a, now, 31, 32);
        let request = files_open(
            &serde_json::json!({"content": "tilo.content:aa", "name": "a.md"}),
            &cert,
            &key,
            now,
        );
        let misnamed = tree_change(&request, caller_invoker(&ac), &["new-file"]);
        let message = tree_written(&receiver, &b, &misnamed, "misnamed", now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("does not name"), "{}", refused.reason);
        assert!(tree_value(&receiver).contains("seed"), "never applied: {}", tree_value(&receiver));
    }

    phase an_open_naming_no_content_or_a_verb_no_put_takes_is_refused;
    {
        let (mut receiver, a, ac, b, _bc, now) = seeded_with_files();
        // A plain open of the app, which starts it, changes no file.
        let plain = files_open(&serde_json::json!({}), &ac, &a, now);
        let context = tree_change(&plain, caller_invoker(&ac), &["new-file"]);
        let message = tree_written(&receiver, &b, &context, "plain", now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("another action"), "{}", refused.reason);
        // A put moves nothing to the trash.
        let put = files_open(
            &serde_json::json!({"content": "tilo.content:aa", "name": "a.md"}),
            &ac,
            &a,
            now,
        );
        let context = tree_change(&put, caller_invoker(&ac), &["new-file", "trash"]);
        let message = tree_written(&receiver, &b, &context, "trashed", now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("another action"), "{}", refused.reason);
        assert!(tree_value(&receiver).contains("seed"), "never applied: {}", tree_value(&receiver));
    }

    phase an_agents_put_is_the_call_its_own_key_signed;
    {
        let (_, _, _, _, now) = fixtures();
        let root = SigningKey::from_bytes(&[1; 32]);
        let workspace_id = *crate::join::WorkspaceId::parse(WORKSPACE)
            .unwrap()
            .as_uuid()
            .as_bytes();
        // A key of the person's agent, certified by their root for that agent by its id.
        let agent_key = |seed: u8, id: &str| {
            let key = SigningKey::from_bytes(&[seed; 32]);
            let mut cert = DeviceCert {
                user_public_key: root.verifying_key().to_bytes(),
                device_public_key: key.verifying_key().to_bytes(),
                device_label: id.to_owned(),
                capabilities: tilo_user_identity::DeviceCapabilities::agent_grant(
                    tilo_user_identity::AgentScope::agent_in(workspace_id, id),
                ),
                issued_at_ns: now_ns(now) - 1,
                expires_at_ns: Some(now_ns(now) + 3_600_000_000_000),
                signature: [0; 64],
                issuer: None,
            };
            tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
            (key, cert)
        };
        let (scribe_key, scribe) = agent_key(41, "scribe");
        let (stray_key, stray) = agent_key(42, "stray");
        let (mut receiver, a, _ac, b, bc, now) =
            seeded_with_files_holding(&[scribe.clone(), stray.clone()]);
        let owner = hex::encode(bc.user_public_key);
        // The workspace lets the scribe write the folder's files, as it grants an agent any
        // action, and keeps the stray from them.
        grant_agent_files(&mut receiver, &a, "scribe", &owner, now);
        receiver
            .governance_mut()
            .put_member_grants(
                &[tilo_action_policy::Grant::new(
                    "stray-files",
                    tilo_action_policy::Grantee::agent("stray", owner.as_str()),
                    tilo_action_policy::TreePath::workspace(WORKSPACE).app(FILES),
                    tilo_action_policy::KindSet::UNDOABLE,
                    tilo_action_policy::Answer::Deny,
                    tilo_action_policy::Grantee::person(owner.as_str()),
                )],
                &a,
                now,
            )
            .unwrap();
        let agent = |id: &str| {
            tilo_actions::ActionInvoker::admitted(
                &tilo_member_types::Acting::agent(id, owner.as_str())
                    .acting_for(tilo_member_types::Acting::person(owner.as_str())),
            )
        };
        let put = |cert: &DeviceCert, key: &SigningKey| {
            files_open(
                &serde_json::json!({"content": "tilo.content:aa", "name": "a.md"}),
                cert,
                key,
                now,
            )
        };
        // The agent signed the open with its own key; the device that keeps the tree made the
        // change, and every replica decides it by that agent's grants.
        let own = tree_change(&put(&scribe, &scribe_key), agent("scribe"), &["new-file"]);
        let message = tree_written(&receiver, &b, &own, "own", now);
        take(&mut receiver, message, now).expect("the agent signed the open with its own key");
        assert!(tree_value(&receiver).contains("own"), "{}", tree_value(&receiver));
        // Another agent of the same person, which the workspace never let write its files, is
        // refused though it signed its own open.
        let ungranted = tree_change(&put(&stray, &stray_key), agent("stray"), &["new-file"]);
        let message = tree_written(&receiver, &b, &ungranted, "stray", now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("not granted"), "{}", refused.reason);
        // The other agent's key signs no call of the granted agent's.
        let borrowed = tree_change(&put(&stray, &stray_key), agent("scribe"), &["new-file"]);
        let message = tree_written(&receiver, &b, &borrowed, "borrowed", now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("does not name"), "{}", refused.reason);
        // The host kept other content than the agent signed for.
        let mut tampered = own;
        tampered.signed.as_mut().unwrap().argument = Some(InvokeArgument::Json(
            serde_json::json!({"content": "tilo.content:ff", "name": "a.md"})
                .as_object()
                .cloned()
                .unwrap(),
        ));
        let message = tree_written(&receiver, &b, &tampered, "tampered", now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("arguments"), "{}", refused.reason);
        // The device that keeps the records signs no agent's put for it: an agent asking under
        // no key of its own puts nothing.
        let mut carried = tree_change(&put(&bc, &b), agent("scribe"), &["new-file"]);
        carried.signed.as_mut().unwrap().argument = None;
        let message = tree_written(&receiver, &b, &carried, "carried", now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(
            refused.reason.contains("without its own certified key"),
            "{}",
            refused.reason
        );
        assert!(tree_value(&receiver).contains("own"), "{}", tree_value(&receiver));
    }

    phase a_put_outside_the_callers_grants_is_refused;
    {
        let (mut receiver, a, _ac, b, _bc, now) = seeded_with_files();
        let (key, cert, person) = add_member_with(&mut receiver, &a, now, 31, 32);
        receiver
            .governance_mut()
            .set_member_role(person, crate::join::Role::Viewer, &a, now)
            .unwrap();
        // A member who may only read the workspace signed the open: the change is decided by
        // their own grants on the tree's verbs, and they may make no file.
        let request = files_open(
            &serde_json::json!({"content": "tilo.content:aa", "name": "a.md"}),
            &cert,
            &key,
            now,
        );
        let context = tree_change(&request, caller_invoker(&cert), &["new-file"]);
        let message = tree_written(&receiver, &b, &context, "viewer's", now);
        let refused = take(&mut receiver, message, now).unwrap_err();
        assert!(refused.reason.contains("not granted"), "{}", refused.reason);
        assert!(tree_value(&receiver).contains("seed"), "never applied: {}", tree_value(&receiver));
    }
}
