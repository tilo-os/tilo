//! A peer decides every received change by its claim against the record
//! app's surface: the action must be granted to its author, and the change
//! must write only units that action may write.
use std::sync::Arc;

use automerge::transaction::{CommitOptions, Transactable};
use automerge::{AutoCommit, ObjType, ROOT};
use collab_types::claim::ClaimedAction;
use collab_types::{ReceiptSurface, SurfaceSource, Surfaces, UnitPath, WrittenDocument};
use ed25519_dalek::SigningKey;
use tilo_action_policy::ActionKind;
use tilo_actions::{
    ActionContext, ActionItem, ActionKey, Did, EditOp, FieldEdit, PathStep, VerbCall,
};
use tilo_user_identity::{DeviceCert, RevocationList};

use crate::join::WorkspaceId;
use crate::join::{InviteGrant, JoinChallenge, Role, issue_invitation, sign_join_proof};
use crate::owner::testing::member_certificate as certificate;
use crate::owner::testing::{now, receive};
use crate::{NeighbourMsg, SeedDocument, SeedState, StatePeer, TransportOrigin, seed_workspace};

const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const APP: &str = "test.app";
const RECORD: &str = "tab:1";
const KIND: &str = "test";
/// The surface the test record's identity names.
const PINNED: [u8; 32] = [7; 32];

/// The test app's surface: `comment` sits under the comments, writes the
/// record and the comment it names; every field offers edits, an edit of
/// the notes writing the notes' unit alone and any other the record.
struct App;

impl ReceiptSurface for App {
    fn kind(&self) -> &str {
        KIND
    }
    fn verb(&self, action: &str) -> Option<(ActionKind, Option<String>)> {
        match action {
            "comment" => Some((ActionKind::Undoable, Some("comments".into()))),
            // Moderation is the workspace's, not an editor's, as in every app
            // whose surface fields its verbs under `moderation`.
            "moderate" => Some((ActionKind::Undoable, Some("moderation".into()))),
            _ => None,
        }
    }
    fn editable(&self, _: &[PathStep]) -> bool {
        true
    }
    fn footprint(&self, action: &ClaimedAction) -> Vec<UnitPath> {
        match action {
            ClaimedAction::Verb {
                item: Some(item), ..
            } => vec![
                Vec::new(),
                vec![
                    PathStep::Field("comments".into()),
                    PathStep::Item(item.clone()),
                ],
            ],
            ClaimedAction::Edit { path, .. }
                if path.first() == Some(&PathStep::Field("notes".into())) =>
            {
                vec![vec![PathStep::Field("notes".into())]]
            }
            _ => vec![Vec::new()],
        }
    }
}

/// Finds the test app's surface, or none at all (it has not arrived).
struct Source(bool);

impl SurfaceSource for Source {
    fn surface(
        &self,
        _: &WrittenDocument<'_>,
        _: Option<&str>,
        _: Option<&[u8; 32]>,
    ) -> Option<Arc<dyn ReceiptSurface>> {
        self.0.then(|| Arc::new(App) as Arc<dyn ReceiptSurface>)
    }
}

/// A surface like a mailbox's: a send queues work for a device in the
/// outbox, and a device of the workspace settles the entries it carries out.
struct Mailbox;

impl ReceiptSurface for Mailbox {
    fn kind(&self) -> &str {
        KIND
    }
    fn verb(&self, action: &str) -> Option<(ActionKind, Option<String>)> {
        (action == "send").then_some((ActionKind::Outside, None))
    }
    fn editable(&self, _: &[PathStep]) -> bool {
        true
    }
    fn footprint(&self, _: &ClaimedAction) -> Vec<UnitPath> {
        vec![Vec::new()]
    }
    fn outboxes(&self) -> Vec<UnitPath> {
        vec![vec![PathStep::Field("outbox".into())]]
    }
}

/// Finds the mailbox's surface.
struct MailboxSource;

impl SurfaceSource for MailboxSource {
    fn surface(
        &self,
        _: &WrittenDocument<'_>,
        _: Option<&str>,
        _: Option<&[u8; 32]>,
    ) -> Option<Arc<dyn ReceiptSurface>> {
        Some(Arc::new(Mailbox))
    }
}

/// A workspace whose record keeps its comments as units, and a member of
/// another person holding `role` in it.
fn workspace_with(role: Role) -> (StatePeer, SigningKey, DeviceCert) {
    workspace_keeping(role, &["comments/*"])
}

/// A workspace whose record keeps the units `units` (an index only when there
/// are any), and a member of another person holding `role` in it.
fn workspace_keeping(role: Role, units: &[&str]) -> (StatePeer, SigningKey, DeviceCert) {
    let owner_key = SigningKey::from_bytes(&[2; 32]);
    let owner = certificate(&SigningKey::from_bytes(&[1; 32]), &owner_key, "owner");
    let mut peer = StatePeer::new(
        WORKSPACE,
        owner.clone(),
        vec![owner.clone()],
        RevocationList::new(),
        now(),
    )
    .unwrap();
    let mut body = AutoCommit::new();
    if !units.is_empty() {
        let index = body
            .put_object(ROOT, collab_types::units::UNIT_INDEX, ObjType::List)
            .unwrap();
        for (at, unit) in units.iter().enumerate() {
            body.insert(&index, at, *unit).unwrap();
        }
    }
    body.put_object(ROOT, "comments", ObjType::Map).unwrap();
    if units.contains(&"outbox") {
        // A resource that queues work for a device: one entry waiting to be
        // carried out.
        let outbox = body.put_object(ROOT, "outbox", ObjType::Map).unwrap();
        let entry = body.put_object(&outbox, "m1", ObjType::Map).unwrap();
        let value = body.put_object(&entry, "v", ObjType::Map).unwrap();
        body.put(&value, "to", "ada@example.test").unwrap();
    }
    body.put(ROOT, "value", "seed").unwrap();
    let meta = body.put_object(ROOT, "tilo:record", ObjType::Map).unwrap();
    body.put(&meta, "surface", hex::encode(PINNED)).unwrap();
    body.commit_with(
        CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Genesis.change_message()),
    );
    peer.install_seed(
        SeedState {
            workspace_changes: seed_workspace(WORKSPACE, &owner, std::slice::from_ref(&owner))
                .unwrap(),
            documents: vec![SeedDocument {
                app_id: APP.into(),
                document_id: RECORD.into(),
                kind: KIND.into(),
                bytes: body.save(),
            }],
        },
        &owner_key,
        now(),
    )
    .unwrap();

    let now_ns: u64 = 2000 * 1_000_000_000;
    let guest_root = SigningKey::from_bytes(&[41; 32]);
    let guest_key = SigningKey::from_bytes(&[42; 32]);
    let guest = certificate(&guest_root, &guest_key, "guest");
    let signed = issue_invitation(
        InviteGrant {
            workspace_id: WorkspaceId::parse(WORKSPACE).unwrap(),
            issuer: owner,
            role,
            nonce: [43; 16],
            recipient_user_public_key: Some(guest.user_public_key),
            invitee_name: Some("Guest".into()),
            expires_at_ns: now_ns + 10_000_000_000,
            transport: vec![],
        },
        &owner_key,
    )
    .unwrap();
    let join = crate::join::decode_and_verify_invitation(
        &signed,
        Some(guest.user_public_key),
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    let challenge = JoinChallenge::new(
        &join,
        &guest,
        [44; 32],
        [45; 32],
        now_ns + 1_000_000_000,
        now_ns,
    )
    .unwrap();
    let proof = sign_join_proof(challenge.clone(), &guest_key).unwrap();
    let redemption =
        crate::join::verify_join_proof(&proof, &challenge, &guest, &RevocationList::new(), now_ns)
            .unwrap();
    assert!(
        peer.governance_mut()
            .admit_invited_member(&redemption, &owner_key, now())
            .unwrap()
    );
    peer.set_surfaces(Surfaces::from_source(Arc::new(Source(true))));
    (peer, guest_key, guest)
}

/// Commit what `write` does as the step `what` on the item `item`, made by
/// the person whose root key is `person`.
fn step(doc: &mut AutoCommit, what: Did, item: Option<&str>, person: [u8; 32]) {
    let context = ActionContext {
        what: Some(what),
        item: item.map(|id| ActionItem {
            id: id.into(),
            label: id.into(),
        }),
        ..ActionContext::for_resource("tab:1", KIND, "Test")
    }
    .with_invoker(tilo_actions::ActionInvoker::person(
        tilo_actions::PersonId::from_root(person),
    ));
    doc.commit_with(CommitOptions::default().with_message(context.change_message().unwrap()));
}

fn edit_value() -> Did {
    Did::Edit(FieldEdit {
        ty: KIND.into(),
        field: "value".into(),
        path: vec![PathStep::Field("value".into())],
        op: EditOp::Set,
        old: None,
        new: None,
        args: std::collections::BTreeMap::new(),
    })
}

fn comment() -> Did {
    Did::Verb(VerbCall::new(ActionKey::new(APP, KIND, "comment"), []))
}

/// The person whose device `key` is, as `peer`'s workspace certifies it.
fn person_of(peer: &StatePeer, key: &SigningKey) -> [u8; 32] {
    let device = key.verifying_key().to_bytes();
    peer.certificates()
        .into_iter()
        .find(|cert| cert.device_public_key == device)
        .expect("the workspace certifies the writer's device")
        .user_public_key
}

/// The guest's signed write: what `write` does to the record, committed as
/// `what` on `item`.
fn guest_write(
    peer: &StatePeer,
    key: &SigningKey,
    write: impl FnOnce(&mut AutoCommit),
    what: Did,
    item: Option<&str>,
) -> NeighbourMsg {
    guest_write_at(peer, key, write, what, item, 1)
}

/// [`guest_write`] as the guest's `seq`th write of the record.
fn guest_write_at(
    peer: &StatePeer,
    key: &SigningKey,
    write: impl FnOnce(&mut AutoCommit),
    what: Did,
    item: Option<&str>,
    seq: u64,
) -> NeighbourMsg {
    let mut doc = AutoCommit::load(&peer.document(APP, RECORD).unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    write(&mut doc);
    step(&mut doc, what, item, person_of(peer, key));
    signed_write_at(&mut doc, &heads, key, seq)
}

/// The heads of the record as `peer` holds it now.
/// The value the fixture's document holds on `peer`.
fn value(peer: &StatePeer) -> String {
    AutoCommit::load(&peer.document(APP, RECORD).unwrap())
        .unwrap()
        .get(ROOT, "value")
        .unwrap()
        .unwrap()
        .0
        .to_string()
}

fn recorded_heads(peer: &StatePeer) -> Vec<automerge::ChangeHash> {
    AutoCommit::load(&peer.document(APP, RECORD).unwrap())
        .unwrap()
        .get_heads()
}

/// [`guest_write_at`] of `write` made on the record as it stood at `heads`:
/// a page that continued the writer from a copy that went another way writes
/// the writer's sequence a second time.
fn guest_write_from(
    peer: &StatePeer,
    key: &SigningKey,
    heads: &[automerge::ChangeHash],
    write: impl FnOnce(&mut AutoCommit),
    what: Did,
    item: Option<&str>,
    seq: u64,
) -> NeighbourMsg {
    let mut doc = AutoCommit::load(&peer.document(APP, RECORD).unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let actor = doc.get_actor().clone();
    let mut forked = doc.fork_at(heads).unwrap();
    forked.set_actor(actor);
    write(&mut forked);
    step(&mut forked, what, item, person_of(peer, key));
    signed_write_at(&mut forked, heads, key, seq)
}

/// The guest's signed write, committed as `what` by `invoker`.
fn guest_write_by(
    peer: &StatePeer,
    key: &SigningKey,
    write: impl FnOnce(&mut AutoCommit),
    what: Did,
    invoker: tilo_actions::ActionInvoker,
) -> NeighbourMsg {
    let mut doc = AutoCommit::load(&peer.document(APP, RECORD).unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    write(&mut doc);
    let context = ActionContext {
        what: Some(what),
        ..ActionContext::for_resource("tab:1", KIND, "Test")
    }
    .with_invoker(invoker);
    doc.commit_with(CommitOptions::default().with_message(context.change_message().unwrap()));
    signed_write(&mut doc, &heads, key)
}

/// What `doc` changed since `heads`, signed as the author's device signs
/// it, which a receiver does not trust to have checked its own grants.
fn signed_write(
    doc: &mut AutoCommit,
    heads: &[automerge::ChangeHash],
    key: &SigningKey,
) -> NeighbourMsg {
    signed_write_at(doc, heads, key, 1)
}

/// [`signed_write`] as the author's `seq`th write of the record.
fn signed_write_at(
    doc: &mut AutoCommit,
    heads: &[automerge::ChangeHash],
    key: &SigningKey,
    seq: u64,
) -> NeighbourMsg {
    signed_write_stamped(doc, heads, key, seq, now())
}

/// [`signed_write_at`] naming `at` as its time: a write's time is whatever
/// its author stamps.
fn signed_write_stamped(
    doc: &mut AutoCommit,
    heads: &[automerge::ChangeHash],
    key: &SigningKey,
    seq: u64,
    at: std::time::SystemTime,
) -> NeighbourMsg {
    let mutation = collab_types::DocumentMutation::Incremental(doc.save_after(heads));
    let author = collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
    let mut envelope = collab_types::SignedOp::new_unsigned(
        mutation.encode().unwrap(),
        collab_types::document_artifact_id(APP, WORKSPACE, RECORD),
        author,
        collab_types::document_attestation_id(APP, WORKSPACE, RECORD),
        collab_types::OpKind::Change,
        at,
        seq,
    )
    .with_surface(Some(PINNED));
    collab_types::sign_op(&mut envelope, key).unwrap();
    NeighbourMsg::AppDocument {
        peer: crate::peer_handle(author.as_bytes()),
        app_id: APP.into(),
        cluster_id: WORKSPACE.into(),
        document_id: RECORD.into(),
        envelope: Box::new(envelope),
        readers: None,
    }
}

/// The write `message` carries, sealed: its claims name `units`, what its
/// action may write as the app's surface declares it, and no receiver reads
/// its changes, as a receiver that holds no key for them decides it.
fn sealed(message: NeighbourMsg, key: &SigningKey, units: &[UnitPath]) -> NeighbourMsg {
    let NeighbourMsg::AppDocument {
        peer,
        app_id,
        cluster_id,
        document_id,
        mut envelope,
        readers,
    } = message
    else {
        panic!("a document write")
    };
    let payload = envelope.payload.clone();
    let changes = collab_types::parse_changes(
        collab_types::DocumentMutation::decode(&payload)
            .unwrap()
            .data(),
    )
    .unwrap();
    envelope.claims = collab_types::claim::claims_of(&changes, |_| units.to_vec()).unwrap();
    envelope.encryption = Some(collab_types::op::EncryptionMetadata::content_key(
        1,
        collab_types::ContentRetention::of_frame(&payload),
    ));
    envelope.payload = vec![0; 16];
    // Signed by hand: a debug build will not sign such an op.
    let signature = ed25519_dalek::Signer::sign(key, &envelope.signing_bytes().unwrap());
    envelope.signature = collab_types::Signature::from_bytes(signature.to_bytes());
    NeighbourMsg::AppDocument {
        peer,
        app_id,
        cluster_id,
        document_id,
        envelope,
        readers,
    }
}

/// What a device writes settling the entry it carried out: its state, inside
/// the entry.
fn settled_entry() -> impl FnOnce(&mut AutoCommit) {
    |doc| {
        let Some((_, outbox)) = doc.get(ROOT, "outbox").unwrap() else {
            panic!("the resource queues work")
        };
        let Some((_, entry)) = doc.get(&outbox, "m1").unwrap() else {
            panic!("the outbox holds m1")
        };
        let Some((_, value)) = doc.get(&entry, "v").unwrap() else {
            panic!("m1 keeps its value")
        };
        doc.put(&value, "settled", "done").unwrap();
    }
}

/// The edit [`settled_entry`] commits as.
fn settle_entry() -> Did {
    Did::Edit(FieldEdit {
        ty: KIND.into(),
        field: "outbox".into(),
        path: vec![
            PathStep::Field("outbox".into()),
            PathStep::Item("m1".into()),
        ],
        op: EditOp::Settle,
        old: None,
        new: None,
        args: std::collections::BTreeMap::new(),
    })
}

fn comment_on(id: &'static str) -> impl FnOnce(&mut AutoCommit) {
    move |doc| {
        let Some((_, comments)) = doc.get(ROOT, "comments").unwrap() else {
            panic!("the record keeps comments")
        };
        let entry = doc.put_object(&comments, id, ObjType::Map).unwrap();
        let value = doc.put_object(&entry, "v", ObjType::Map).unwrap();
        doc.put(&value, "text", "looks good").unwrap();
    }
}

use automerge::ReadDoc as _;

fn moderation() -> Did {
    Did::Verb(VerbCall::new(ActionKey::new(APP, KIND, "moderate"), []))
}

tilo_test_env::scenario! {
    fn a_change_is_decided_by_the_units_its_author_may_write;

    phase a_refused_change_leaves_the_rest_of_the_record;
    // One record carries a writer's repeated sequence and a change the
    // workspace refuses. Both are dropped on their own: the writer's other
    // changes, before and after them, are read. A page
    // ([`StatePeer::decide_relayed_document`]) and a host
    // ([`StatePeer::receive_from`]) refuse the same change the same way.
    {
        let (mut peer, key, guest) = workspace_with(Role::Editor);
        let first = guest_write_at(&peer, &key, comment_on("c1"), comment(), Some("c1"), 1);
        receive(&mut peer, &guest, first).expect("an editor may comment");
        let after_first = recorded_heads(&peer);
        let kept = guest_write_at(&peer, &key, comment_on("c9"), comment(), Some("c9"), 2);
        receive(&mut peer, &guest, kept).expect("the second change of the sequence");
        // A page that continued the writer from the record as it stood before
        // that change writes the same sequence again.
        let repeated = guest_write_from(
            &peer,
            &key,
            &after_first,
            comment_on("c8"),
            comment(),
            Some("c8"),
            3,
        );
        let moderation_write = guest_write_at(
            &peer,
            &key,
            |doc| {
                let moderation = doc.put_object(ROOT, "moderation", ObjType::Map).unwrap();
                doc.put(&moderation, "category", "rules").unwrap();
            },
            moderation(),
            None,
            4,
        );
        let later = guest_write_at(&peer, &key, comment_on("c2"), comment(), Some("c2"), 5);
        let page = peer.clone();
        // A record holds one change under each writer's sequence, so the one the
        // page wrote under a sequence the record holds is refused.
        let refused = receive(&mut peer, &guest, repeated).unwrap_err();
        assert!(refused.contains("duplicate seq"), "{refused}");
        let refused = page
            .decide_relayed_document(
                TransportOrigin::ServerDelta(guest.device_public_key),
                &moderation_write,
                now(),
            )
            .unwrap_err();
        assert!(refused.contains("not granted"), "{refused}");
        let refused = receive(&mut peer, &guest, moderation_write).unwrap_err();
        assert!(refused.contains("not granted"), "{refused}");
        page.decide_relayed_document(
            TransportOrigin::ServerDelta(guest.device_public_key),
            &later,
            now(),
        )
        .expect("a page reads the record on past the refused changes");
        receive(&mut peer, &guest, later)
            .expect("the host reads the record on past the refused changes");

        let doc = AutoCommit::load(&peer.document(APP, RECORD).unwrap()).unwrap();
        let Some((_, comments)) = doc.get(ROOT, "comments").unwrap() else {
            panic!("the record keeps its comments");
        };
        for id in ["c1", "c9", "c2"] {
            assert!(doc.get(&comments, id).unwrap().is_some(), "{id} is read");
        }
        assert!(
            doc.get(&comments, "c8").unwrap().is_none(),
            "the change under a sequence the record holds is refused"
        );
        assert!(
            doc.get(ROOT, "moderation").unwrap().is_none(),
            "the refused change wrote nothing"
        );
    }

    phase a_change_is_refused_when_its_action_is_not_granted_to_its_author;
    // A commenter's comment is granted; their edit of the record is not.
    {
        let (mut peer, key, guest) = workspace_with(Role::Commenter);
        let commented = guest_write(&peer, &key, comment_on("c1"), comment(), Some("c1"));
        receive(&mut peer, &guest, commented).expect("a commenter may comment");
        let edited = guest_write(
            &peer,
            &key,
            |doc| doc.put(ROOT, "value", "x").unwrap(),
            edit_value(),
            None,
        );
        let refused = receive(&mut peer, &guest, edited).unwrap_err();
        assert!(refused.contains("not granted"), "{refused}");

        let (mut peer, key, guest) = workspace_with(Role::Editor);
        let edited = guest_write(
            &peer,
            &key,
            |doc| doc.put(ROOT, "value", "x").unwrap(),
            edit_value(),
            None,
        );
        receive(&mut peer, &guest, edited).expect("an editor may edit");
    }

    phase a_granted_action_writing_another_unit_is_refused;
    // A granted action that writes a unit it may not write is refused: a
    // comment on one item that writes another, whether the claim names the
    // unit it wrote or hides it.
    {
        let (mut peer, key, guest) = workspace_with(Role::Commenter);
        // The change itself says what it wrote: c2, which commenting on c1 may
        // not write.
        let wrote_other = guest_write(&peer, &key, comment_on("c2"), comment(), Some("c1"));
        let refused = receive(&mut peer.clone(), &guest, wrote_other.clone()).unwrap_err();
        assert!(refused.contains("may not write"), "{refused}");

        // A cleartext op that brings its own claims (naming c1) is refused: its
        // changes are the one account of what it did.
        let NeighbourMsg::AppDocument {
            peer: handle,
            app_id,
            cluster_id,
            document_id,
            mut envelope,
            readers,
        } = wrote_other
        else {
            panic!("a document write")
        };
        let changes = collab_types::parse_changes(
            collab_types::DocumentMutation::decode(&envelope.payload)
                .unwrap()
                .data(),
        )
        .unwrap();
        envelope.claims = collab_types::claims_of(&changes, |_| {
            vec![
                Vec::new(),
                vec![
                    PathStep::Field("comments".into()),
                    PathStep::Item("c1".into()),
                ],
            ]
        })
        .unwrap();
        // Signed by hand: a debug build will not sign such an op.
        let signature = ed25519_dalek::Signer::sign(&key, &envelope.signing_bytes().unwrap());
        envelope.signature = collab_types::Signature::from_bytes(signature.to_bytes());
        let lying = NeighbourMsg::AppDocument {
            peer: handle,
            app_id,
            cluster_id,
            document_id,
            envelope,
            readers,
        };
        let refused = receive(&mut peer, &guest, lying).unwrap_err();
        assert!(refused.contains("do not name exactly"), "{refused}");
    }

    phase changes_after_the_first_unit_index_are_read_by_it;
    // A record that had no units until a change gave it its index (its app
    // gained a unit after it was made) has what the changes after it wrote
    // read by that index: an edit of the notes that follows writes the notes'
    // unit, not the whole record its claim could not name.
    {
        let (mut peer, key, guest) = workspace_keeping(Role::Editor, &[]);
        let mut doc = AutoCommit::load(&peer.document(APP, RECORD).unwrap()).unwrap();
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        let heads = doc.get_heads();
        let index = doc
            .put_object(ROOT, collab_types::units::UNIT_INDEX, ObjType::List)
            .unwrap();
        doc.insert(&index, 0, "notes").unwrap();
        doc.put(ROOT, "value", "indexed").unwrap();
        step(&mut doc, edit_value(), None, guest.user_public_key);
        let notes = doc.put_object(ROOT, "notes", ObjType::Map).unwrap();
        doc.put(&notes, "n1", "first").unwrap();
        step(
            &mut doc,
            Did::Edit(FieldEdit {
                ty: KIND.into(),
                field: "notes".into(),
                path: vec![PathStep::Field("notes".into())],
                op: EditOp::Set,
                old: None,
                new: None,
                args: std::collections::BTreeMap::new(),
            }),
            None,
            guest.user_public_key,
        );
        let written = signed_write(&mut doc, &heads, &key);
        receive(&mut peer, &guest, written).expect("the notes edit wrote the notes alone");
    }

    phase a_settled_outbox_entry_is_no_reach_outside;
    // A resource the workspace owns that queues work for a device: only what
    // a change queued reaches outside it. A device settling the entry it
    // carried out writes inside it, not a new one, so its edit is what its
    // author may make of the resource, not a reach outside.
    {
        let (mut peer, key, guest) = workspace_keeping(Role::Editor, &["outbox"]);
        peer.set_surfaces(Surfaces::from_source(Arc::new(MailboxSource)));
        let recorded = guest_write(&peer, &key, settled_entry(), settle_entry(), None);
        receive(&mut peer, &guest, recorded).expect("settling an entry queues nothing");
    }

    phase a_sealed_settled_outbox_entry_is_no_reach_outside;
    // A receiver that holds no key for the change decides it by its claim, which
    // names what its action may write: the outbox is one unit, so a claim
    // naming it cannot tell an entry settled from one put in. The edit names
    // where it wrote, inside the entry, and that queued nothing.
    {
        let (mut peer, key, guest) = workspace_keeping(Role::Editor, &["outbox"]);
        peer.set_surfaces(Surfaces::from_source(Arc::new(MailboxSource)));
        let recorded = sealed(
            guest_write(&peer, &key, settled_entry(), settle_entry(), None),
            &key,
            &[vec![PathStep::Field("outbox".into())]],
        );
        receive(&mut peer, &guest, recorded).expect("a settled entry is no reach outside");
    }

    phase an_item_stored_as_its_order_key_writes_its_own_unit;
    // An item kept as its order key alone (a list of plain ids, rows of a
    // grid) is the scalar under its id: writing it writes that item's unit,
    // exactly as writing an entry map does.
    {
        let order_key = |id: &'static str| {
            move |doc: &mut AutoCommit| {
                let Some((_, comments)) = doc.get(ROOT, "comments").unwrap() else {
                    panic!("the record keeps comments")
                };
                doc.put(&comments, id, "a0").unwrap();
            }
        };
        let (mut peer, key, guest) = workspace_with(Role::Commenter);
        let own = guest_write(&peer, &key, order_key("c1"), comment(), Some("c1"));
        receive(&mut peer, &guest, own).expect("commenting on c1 may write c1");
        let other = guest_write(&peer, &key, order_key("c2"), comment(), Some("c1"));
        let refused = receive(&mut peer, &guest, other).unwrap_err();
        assert!(refused.contains("may not write"), "{refused}");
    }
}

tilo_test_env::scenario! {
    fn a_change_is_held_or_refused_by_its_surface_its_version_and_the_name_it_shows;

    phase a_change_whose_app_surface_has_not_arrived_is_held_until_it_has;
    // Without the app's surface a change is held, never merged on trust, and
    // decided once the surface is here.
    {
        let (mut peer, key, guest) = workspace_with(Role::Editor);
        peer.set_surfaces(Surfaces::from_source(Arc::new(Source(false))));
        let edited = guest_write(
            &peer,
            &key,
            |doc| doc.put(ROOT, "value", "x").unwrap(),
            edit_value(),
            None,
        );
        let value = |peer: &StatePeer| {
            AutoCommit::load(&peer.document(APP, RECORD).unwrap())
                .unwrap()
                .get(ROOT, "value")
                .unwrap()
                .unwrap()
                .0
                .to_string()
        };
        let held = peer
            .receive_from(
                TransportOrigin::ServerDelta(guest.device_public_key),
                edited.clone(),
                now(),
            )
            .unwrap();
        assert!(held.deferred, "kept for later");
        assert_eq!(value(&peer), "\"seed\"", "nothing merged");
        peer.set_surfaces(Surfaces::from_source(Arc::new(Source(true))));
        receive(&mut peer, &guest, edited).expect("decided once the surface is here");
        assert_eq!(value(&peer), "\"x\"");
    }

    phase an_op_naming_another_surface_than_its_record_is_refused;
    // An op naming another surface than its record's identity is refused, but
    // the change naming a newer one for the record is decided as an upgrade.
    {
        let (mut peer, key, guest) = workspace_with(Role::Editor);
        let resurfaced = |message: NeighbourMsg| {
            let NeighbourMsg::AppDocument {
                peer: handle,
                app_id,
                cluster_id,
                document_id,
                mut envelope,
                readers,
            } = message
            else {
                panic!("a document write")
            };
            envelope.surface = Some([8; 32]);
            collab_types::sign_op(&mut envelope, &key).unwrap();
            NeighbourMsg::AppDocument {
                peer: handle,
                app_id,
                cluster_id,
                document_id,
                envelope,
                readers,
            }
        };
        let edited = guest_write(
            &peer,
            &key,
            |doc| doc.put(ROOT, "value", "x").unwrap(),
            edit_value(),
            None,
        );
        let refused = receive(&mut peer.clone(), &guest, resurfaced(edited)).unwrap_err();
        assert!(refused.contains("another app surface"), "{refused}");

        let mut doc = AutoCommit::load(&peer.document(APP, RECORD).unwrap()).unwrap();
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        let heads = doc.get_heads();
        let meta = doc.get(ROOT, "tilo:record").unwrap().unwrap().1;
        doc.put(&meta, "surface", hex::encode([8; 32])).unwrap();
        doc.commit_with(
            CommitOptions::default()
                .with_message(tilo_actions::FrameworkChange::Upgrade.change_message()),
        );
        let mutation = collab_types::DocumentMutation::Incremental(doc.save_after(&heads));
        let author = collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
        let mut envelope = collab_types::SignedOp::new_unsigned(
            mutation.encode().unwrap(),
            collab_types::document_artifact_id(APP, WORKSPACE, RECORD),
            author,
            collab_types::document_attestation_id(APP, WORKSPACE, RECORD),
            collab_types::OpKind::Change,
            now(),
            2,
        )
        .with_surface(Some([8; 32]));
        collab_types::sign_op(&mut envelope, &key).unwrap();
        receive(
            &mut peer,
            &guest,
            NeighbourMsg::AppDocument {
                peer: crate::peer_handle(author.as_bytes()),
                app_id: APP.into(),
                cluster_id: WORKSPACE.into(),
                document_id: RECORD.into(),
                envelope: Box::new(envelope),
                readers: None,
            },
        )
        .expect("an editor names the newer surface");
    }

    phase a_persons_change_showing_another_name_is_refused;
    // A device that writes a change showing another name is refused by every
    // receiver: no author may show one.
    {
        let (mut peer, key, guest) = workspace_with(Role::Editor);
        // The change names the person whose device signed it: an invoker this
        // one verified, which is what lets a receiver decide the change as
        // that person's.
        let guest_person = hex::encode(guest.user_public_key);
        let impostor = tilo_actions::ActionInvoker::person(guest_person.clone()).with_appearance(
            Some(tilo_actions::Appearance::new("Owner", None, None).unwrap()),
        );
        let edited = guest_write_by(
            &peer,
            &key,
            |doc| doc.put(ROOT, "value", "x").unwrap(),
            edit_value(),
            impostor,
        );
        let refused = receive(&mut peer, &guest, edited).unwrap_err();
        assert!(refused.contains("appearance"), "{refused}");
        let plain = guest_write_by(
            &peer,
            &key,
            |doc| doc.put(ROOT, "value", "y").unwrap(),
            edit_value(),
            tilo_actions::ActionInvoker::person(guest_person),
        );
        receive(&mut peer, &guest, plain).expect("an editor's own edit is admitted");
    }

    phase a_change_made_with_another_version_than_the_workspace_runs_waits_for_its_author_to_update;
    // The workspace runs one version of an app: a change made with another is held,
    // not merged, until its author runs the workspace's version and sends the same
    // changes naming that version's surface, with the record upgraded to it.
    {
        use crate::workspace_membership::{AppRelease, WorkspaceMembershipCommand};
        const NEWER: [u8; 32] = [8; 32];
        let (mut peer, key, guest) = workspace_with(Role::Editor);
        let owner_key = SigningKey::from_bytes(&[2; 32]);
        let owner = certificate(&SigningKey::from_bytes(&[1; 32]), &owner_key, "owner");
        let release = AppRelease {
            app: APP.into(),
            version: "2.0.0-dev.1".into(),
            surface: hex::encode(NEWER),
            package: hex::encode([9; 32]),
            publisher: hex::encode([10; 32]),
            file: None,
            dev: true,
            pinned: false,
            payload: crate::workspace_membership::AppPayload::Tapp,
        };
        let install = WorkspaceMembershipCommand::InstallApp(release.clone());
        let refused = peer
            .clone()
            .workspace_mut()
            .apply_workspace_membership_command(&guest, &install, &owner_key, now())
            .unwrap_err();
        assert!(
            refused.contains("Not allowed"),
            "an editor installs no app: {refused}"
        );
        assert!(
            peer.workspace_mut()
                .apply_workspace_membership_command(&owner, &install, &owner_key, now())
                .unwrap()
        );
        let installed = peer
            .governance()
            .app_version(APP)
            .expect("the workspace runs v2");
        assert_eq!(installed.release, release);
        assert_eq!(installed.installed_by, hex::encode(owner.user_public_key));

        // The guest still runs v1: its edit names v1's surface.
        let mut doc = AutoCommit::load(&peer.document(APP, RECORD).unwrap()).unwrap();
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        let heads = doc.get_heads();
        doc.put(ROOT, "value", "x").unwrap();
        step(&mut doc, edit_value(), None, guest.user_public_key);
        let old = signed_write(&mut doc, &heads, &key);
        let held = peer
            .receive_from(
                TransportOrigin::ServerDelta(guest.device_public_key),
                old,
                now(),
            )
            .unwrap();
        assert!(held.deferred, "held, not refused");
        assert_eq!(value(&peer), "\"seed\"", "nothing merged");

        // Its desktop switches to v2: the record names v2's surface, and the
        // same edit goes out again under it.
        let upgraded = upgraded_write(&mut doc, &heads, &key, NEWER);
        receive(&mut peer, &guest, upgraded).expect("the updated guest's edit is decided against v2");
        assert_eq!(value(&peer), "\"x\"", "the guest's edit is kept");

        // Taking the choice back lets every version write again.
        assert!(
            peer.workspace_mut()
                .apply_workspace_membership_command(
                    &owner,
                    &WorkspaceMembershipCommand::UninstallApp { app: APP.into() },
                    &owner_key,
                    now(),
                )
                .unwrap()
        );
        assert!(peer.governance().app_versions().is_empty());
    }
}

/// The changes since `heads`, sent again after the record is upgraded to the
/// version whose surface hashes to `surface`, naming that surface.
fn upgraded_write(
    doc: &mut AutoCommit,
    heads: &[automerge::ChangeHash],
    key: &SigningKey,
    surface: [u8; 32],
) -> NeighbourMsg {
    let meta = doc.get(ROOT, "tilo:record").unwrap().unwrap().1;
    doc.put(&meta, "surface", hex::encode(surface)).unwrap();
    doc.commit_with(
        CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Upgrade.change_message()),
    );
    let mutation = collab_types::DocumentMutation::Incremental(doc.save_after(heads));
    let author = collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
    let mut envelope = collab_types::SignedOp::new_unsigned(
        mutation.encode().unwrap(),
        collab_types::document_artifact_id(APP, WORKSPACE, RECORD),
        author,
        collab_types::document_attestation_id(APP, WORKSPACE, RECORD),
        collab_types::OpKind::Change,
        now(),
        2,
    )
    .with_surface(Some(surface));
    collab_types::sign_op(&mut envelope, key).unwrap();
    NeighbourMsg::AppDocument {
        peer: crate::peer_handle(author.as_bytes()),
        app_id: APP.into(),
        cluster_id: WORKSPACE.into(),
        document_id: RECORD.into(),
        envelope: Box::new(envelope),
        readers: None,
    }
}

/// A write this device drafted, not signed yet, under a write the same device
/// sent meanwhile on another of its connections: numbered past that write to
/// be signed then, never refused for the signature it does not carry yet.
#[test]
fn an_unsigned_draft_under_a_write_its_device_sent_meanwhile_is_numbered_past_it() {
    let (mut peer, guest_key, guest) = workspace_with(Role::Editor);
    let drafted = guest_write(
        &peer,
        &guest_key,
        |doc| doc.put(ROOT, "value", "drafted").unwrap(),
        edit_value(),
        None,
    );
    let sent = guest_write(
        &peer,
        &guest_key,
        |doc| doc.put(ROOT, "value", "sent").unwrap(),
        edit_value(),
        None,
    );
    receive(&mut peer, &guest, sent).expect("the other connection's write");
    let NeighbourMsg::AppDocument {
        app_id,
        document_id,
        mut envelope,
        ..
    } = drafted
    else {
        panic!("a document write")
    };
    envelope.signature = collab_types::Signature::from_bytes([0; 64]);
    let (numbered, again) = peer
        .sequenced_local_document(&app_id, &document_id, *envelope, now())
        .expect("numbered past, not refused");
    assert!(again, "signed again under its new number");
    assert!(numbered.seq > 1, "past the write sent meanwhile");
}

/// The guest's write of `value`, written under the governance whose heads
/// are `governance`, as its page drafts it.
fn guest_write_under(
    peer: &StatePeer,
    key: &SigningKey,
    value: &str,
    governance: &[[u8; 32]],
) -> NeighbourMsg {
    let mut message = guest_write(
        peer,
        key,
        |doc| doc.put(ROOT, "value", value).unwrap(),
        edit_value(),
        None,
    );
    let NeighbourMsg::AppDocument { envelope, .. } = &mut message else {
        panic!("the guest's write is a document write")
    };
    envelope.governance = governance
        .iter()
        .map(collab_types::governance_head)
        .collect();
    collab_types::sign_op(envelope, key).unwrap();
    message
}

/// The owner's device, which governs the test workspace.
fn owner() -> (SigningKey, DeviceCert) {
    let owner_key = SigningKey::from_bytes(&[2; 32]);
    let owner = certificate(&SigningKey::from_bytes(&[1; 32]), &owner_key, "owner");
    (owner_key, owner)
}

/// A viewer made an editor a moment ago writes at once: a peer that has not
/// heard of the role yet holds the write rather than refusing it, and takes
/// it in once the role arrives.
/// A viewer's write names the viewer who made it, and their role refuses it: what keeps a
/// viewer from writing is the grant, never a missing name. An editor's same write is taken in.
#[test]
fn a_viewers_write_naming_them_is_refused_by_their_role_and_an_editors_is_taken() {
    for (role, taken) in [(Role::Viewer, false), (Role::Editor, true)] {
        let (mut peer, key, guest) = workspace_with(role);
        let before = value(&peer);
        let write = guest_write(
            &peer,
            &key,
            |doc| doc.put(ROOT, "value", "written").unwrap(),
            edit_value(),
            None,
        );
        let received = receive(&mut peer, &guest, write);
        if taken {
            received.expect("an editor writes the record");
            assert!(value(&peer).contains("written"));
        } else {
            let refused = received.unwrap_err();
            assert!(refused.contains("not granted"), "{refused}");
            assert_eq!(value(&peer), before, "nothing of it is taken in");
        }
    }
}

#[test]
fn a_write_under_a_role_given_a_moment_ago_waits_for_it_then_applies() {
    let (mut peer, guest_key, guest) = workspace_with(Role::Viewer);
    let (owner_key, owner) = owner();
    let mut governor = peer.clone();
    governor
        .governance_mut()
        .set_member_role(guest.user_public_key, Role::Editor, &owner_key, now())
        .unwrap();
    let before = value(&peer);
    let write = guest_write_under(
        &governor,
        &guest_key,
        "edited",
        &governor.governance().heads(),
    );
    receive(&mut peer, &guest, write).expect("held, not refused");
    assert_eq!(value(&peer), before, "nothing of it is taken in yet");

    peer.receive_from(
        TransportOrigin::ServerDelta(owner.device_public_key),
        governor.workspace_message(),
        now(),
    )
    .unwrap();
    assert_ne!(value(&peer), before, "taken in once the role arrived");
    assert!(value(&peer).contains("edited"));
}

/// A write naming governance its receiver holds is decided at once against
/// all of it: an editor removed after the governance its write names (while
/// it wrote) is refused, the earlier heads bringing back nothing it lost.
#[test]
fn a_write_citing_governance_from_before_its_writers_removal_is_refused() {
    let (mut peer, guest_key, guest) = workspace_with(Role::Editor);
    let (owner_key, _) = owner();
    let before_removal = peer.governance().heads();
    let write = guest_write_under(&peer, &guest_key, "edited", &before_removal);
    peer.governance_mut()
        .remove_member(guest.user_public_key, &owner_key, now())
        .unwrap();
    assert!(
        receive(&mut peer, &guest, write).is_err(),
        "a removed writer's write is refused, whatever governance it names"
    );
}

/// A replay reaching a peer ahead of the role change its writes were made
/// under (a server's session that just started, caught up by its server)
/// holds a writer's first writes and lets the rest go: none of them is
/// refused, and once the role arrives the held ones apply and the server is
/// asked to replay the ones let go, which apply then too.
#[test]
fn writes_ahead_of_their_role_change_are_asked_for_again_once_it_arrives() {
    let (mut peer, guest_key, guest) = workspace_with(Role::Viewer);
    let (owner_key, owner) = owner();
    let mut governor = peer.clone();
    governor
        .governance_mut()
        .set_member_role(guest.user_public_key, Role::Editor, &owner_key, now())
        .unwrap();
    let named: Vec<[u8; 4]> = governor
        .governance()
        .heads()
        .iter()
        .map(collab_types::governance_head)
        .collect();
    let count = u64::try_from(crate::MAX_HELD_FOR_GOVERNANCE_PER_WRITER).unwrap() + 4;
    // One after the other, as a page writes them: the governor, which holds
    // the role, takes each in, and the next is written on top of it.
    let writes: Vec<NeighbourMsg> = (1..=count)
        .map(|seq| {
            let edit = format!("edit {seq}");
            let mut message = guest_write_at(
                &governor,
                &guest_key,
                |doc| doc.put(ROOT, "value", edit).unwrap(),
                edit_value(),
                None,
                seq,
            );
            let NeighbourMsg::AppDocument { envelope, .. } = &mut message else {
                panic!("a document write")
            };
            envelope.governance.clone_from(&named);
            collab_types::sign_op(envelope, &guest_key).unwrap();
            receive(&mut governor, &guest, message.clone()).expect("the governor takes it in");
            message
        })
        .collect();
    let origin = TransportOrigin::ServerDelta(guest.device_public_key);
    let mut let_go = Vec::new();
    for write in &writes {
        let report = peer
            .receive_from(origin, write.clone(), now())
            .expect("a write ahead of its role change is not refused");
        for dropped in &report.dropped {
            assert!(
                !matches!(dropped.reason, crate::DropReason::Refused(_)),
                "let go to be asked for again, never refused: {dropped}"
            );
        }
        let_go.extend(report.dropped);
    }
    assert!(
        !let_go.is_empty(),
        "past its share of the queue a write is let go"
    );

    let arrived = peer
        .receive_from(
            TransportOrigin::ServerDelta(owner.device_public_key),
            governor.workspace_message(),
            now(),
        )
        .unwrap();
    assert!(
        arrived.replay_wanted,
        "the role arrived: what was let go waiting for it is asked for again"
    );
    for dropped in &let_go {
        let replayed = &writes[usize::try_from(dropped.sequence).unwrap() - 1];
        let report = peer.receive_from(origin, replayed.clone(), now()).unwrap();
        assert!(
            !report.deferred && report.dropped.is_empty(),
            "write {} applies once replayed",
            dropped.sequence
        );
    }
    assert_eq!(
        value(&peer),
        format!("\"edit {count}\""),
        "every write of the replay is taken in"
    );
}

#[path = "written_under_tests.rs"]
mod written_under;

/// What the machine copied from the guest's writes: the record as it stood when the disk was
/// copied, changed under the copy's own Automerge writer, and signed with the key both machines
/// carry as the `seq`th write of the record, the number the original gave a write of its own.
fn copied_machine_write(
    copied_from: &StatePeer,
    key: &SigningKey,
    write: impl FnOnce(&mut AutoCommit),
    item: &str,
    seq: u64,
) -> NeighbourMsg {
    let mut doc = AutoCommit::load(&copied_from.document(APP, RECORD).unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(b"the copied machine".to_vec()));
    let heads = doc.get_heads();
    write(&mut doc);
    step(&mut doc, comment(), Some(item), person_of(copied_from, key));
    signed_write_at(&mut doc, &heads, key, seq)
}

/// One device key on two machines: each signs a change of its own under the same number. The
/// workspace takes the first, refuses the second naming the device, and takes nothing more under
/// the key while its other members write on. Once the person re-adds one machine with a ticket,
/// a device of theirs certified after the clash, the original's writes are taken again.
#[test]
fn a_device_running_on_two_machines_is_refused_by_name_until_one_is_readded() {
    let (mut peer, key, guest) = workspace_with(Role::Editor);
    peer.select_exact_admissions().unwrap();
    let origin = TransportOrigin::ServerDelta(guest.device_public_key);
    let copied_from = peer.clone();

    let original = guest_write_at(&peer, &key, comment_on("c1"), comment(), Some("c1"), 1);
    peer.receive_from_report(origin, original, now())
        .expect("the original machine's write");
    let clash = copied_machine_write(&copied_from, &key, comment_on("c2"), "c2", 1);
    let refused = peer
        .receive_from_report(origin, clash, now())
        .expect_err("two changes under one number");
    assert!(
        matches!(&refused, crate::AdmissionError::CopiedDevice(copied)
            if copied.device == guest.device_public_key && copied.name == "guest"),
        "{refused:?}"
    );
    assert_eq!(
        refused.to_string(),
        "guest is running on two machines; its changes clash. Re-add one of them with a ticket"
    );
    assert_eq!(refused.reason(), neighbour_wire::RefusalReason::Conflict);
    assert_eq!(peer.copied_devices().len(), 1);

    // Nothing more under the key, from either machine.
    let next = guest_write_at(&peer, &key, comment_on("c3"), comment(), Some("c3"), 2);
    assert!(matches!(
        peer.receive_from_report(origin, next.clone(), now()),
        Err(crate::AdmissionError::CopiedDevice(_))
    ));
    assert!(!value_has_comment(&peer, "c3"));

    // The owner writes on meanwhile.
    let owner_key = SigningKey::from_bytes(&[2; 32]);
    let owner_origin = TransportOrigin::ServerDelta(owner_key.verifying_key().to_bytes());
    let mut doc = AutoCommit::load(&peer.document(APP, RECORD).unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(b"the owner".to_vec()));
    let heads = doc.get_heads();
    comment_on("o1")(&mut doc);
    step(&mut doc, comment(), Some("o1"), person_of(&peer, &owner_key));
    let owners = signed_write_at(&mut doc, &heads, &owner_key, 1);
    peer.receive_from_report(owner_origin, owners, now())
        .expect("another device's write is taken");
    assert!(value_has_comment(&peer, "o1"));

    // The person re-adds the copy with a ticket: it is certified under a key of its own.
    let later = now() + std::time::Duration::from_secs(1);
    let readded_key = SigningKey::from_bytes(&[46; 32]);
    let mut readded = certificate(&SigningKey::from_bytes(&[41; 32]), &readded_key, "guest");
    readded.issued_at_ns = u64::try_from(
        later
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap();
    tilo_user_identity::sign_device_cert(&mut readded, &SigningKey::from_bytes(&[41; 32]))
        .unwrap();
    assert!(
        peer.governance_mut()
            .admit_member_device(&readded, &owner_key, later)
            .unwrap()
    );
    peer.receive_from_report(origin, next, later)
        .expect("the original's write is taken once the copy was re-added");
    assert!(value_has_comment(&peer, "c3"));
    assert!(peer.copied_devices().is_empty());
}

/// Whether the record on `peer` has the comment `id`.
fn value_has_comment(peer: &StatePeer, id: &str) -> bool {
    let doc = AutoCommit::load(&peer.document(APP, RECORD).unwrap()).unwrap();
    let Some((_, comments)) = doc.get(ROOT, "comments").unwrap() else {
        return false;
    };
    doc.get(&comments, id).unwrap().is_some()
}
