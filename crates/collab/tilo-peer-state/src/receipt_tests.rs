//! A peer decides every received change by its claim against the record
//! app's surface: the action must be granted to its author, and the change
//! must write only units that action may write.
use std::sync::Arc;

use automerge::transaction::{CommitOptions, Transactable};
use automerge::{AutoCommit, ObjType, ROOT};
use collab_types::claim::ClaimedAction;
use collab_types::{ReceiptSurface, Record, SurfaceSource, Surfaces, UnitPath};
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
        _: &Record<'_>,
        _: Option<&str>,
        _: Option<&[u8; 32]>,
    ) -> Option<Arc<dyn ReceiptSurface>> {
        self.0.then(|| Arc::new(App) as Arc<dyn ReceiptSurface>)
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
    let mut record = AutoCommit::new();
    if !units.is_empty() {
        let index = record
            .put_object(ROOT, collab_types::units::UNIT_INDEX, ObjType::List)
            .unwrap();
        for (at, unit) in units.iter().enumerate() {
            record.insert(&index, at, *unit).unwrap();
        }
    }
    record.put_object(ROOT, "comments", ObjType::Map).unwrap();
    record.put(ROOT, "value", "seed").unwrap();
    let meta = record
        .put_object(ROOT, "tilo:record", ObjType::Map)
        .unwrap();
    record.put(&meta, "surface", hex::encode(PINNED)).unwrap();
    record.commit_with(
        CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Genesis.change_message()),
    );
    peer.install_seed(
        SeedState {
            workspace_changes: seed_workspace(WORKSPACE, &owner, std::slice::from_ref(&owner))
                .unwrap(),
            documents: vec![SeedDocument {
                app_id: APP.into(),
                record_id: RECORD.into(),
                kind: KIND.into(),
                bytes: record.save(),
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

/// Commit what `write` does as the step `what` on the item `item`.
fn step(doc: &mut AutoCommit, what: Did, item: Option<&str>) {
    let context = ActionContext {
        what: Some(what),
        item: item.map(|id| ActionItem {
            id: id.into(),
            label: id.into(),
        }),
        ..ActionContext::for_resource(RECORD, KIND, "Test")
    };
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
    step(&mut doc, what, item);
    signed_write_at(&mut doc, &heads, key, seq)
}

/// The heads of the record as `peer` holds it now.
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
    step(&mut forked, what, item);
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
        ..ActionContext::for_resource(RECORD, KIND, "Test")
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
    let mutation = collab_types::DocumentMutation::Incremental(doc.save_after(heads));
    let author = collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
    let mut envelope = collab_types::SignedOp::new_unsigned(
        mutation.encode().unwrap(),
        collab_types::document_artifact_id(APP, WORKSPACE, RECORD),
        author,
        collab_types::document_attestation_id(APP, WORKSPACE, RECORD),
        collab_types::OpKind::Change,
        now(),
        seq,
    )
    .with_surface(Some(PINNED));
    collab_types::sign_op(&mut envelope, key).unwrap();
    NeighbourMsg::AppDocument {
        peer: crate::peer_handle(author.as_bytes()),
        app_id: APP.into(),
        cluster_id: WORKSPACE.into(),
        record_id: RECORD.into(),
        envelope: Box::new(envelope),
    }
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

/// One record carries a writer's repeated sequence and a change the
/// workspace refuses. Both are dropped on their own: the writer's other
/// changes, before and after them, are read. A page
/// ([`StatePeer::decide_relayed_document`]) and a host
/// ([`StatePeer::receive_from`]) refuse the same change the same way.
#[test]
fn a_refused_change_leaves_the_rest_of_the_record() {
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
            TransportOrigin::RelayDelta(guest.device_public_key),
            &moderation_write,
            now(),
        )
        .unwrap_err();
    assert!(refused.contains("not granted"), "{refused}");
    let refused = receive(&mut peer, &guest, moderation_write).unwrap_err();
    assert!(refused.contains("not granted"), "{refused}");
    page.decide_relayed_document(
        TransportOrigin::RelayDelta(guest.device_public_key),
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

/// A commenter's comment is granted; their edit of the record is not.
#[test]
fn a_change_is_refused_when_its_action_is_not_granted_to_its_author() {
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

/// A granted action that writes a unit it may not write is refused: a
/// comment on one item that writes another, whether the claim names the
/// unit it wrote or hides it.
#[test]
fn a_granted_action_writing_another_unit_is_refused() {
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
        record_id,
        mut envelope,
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
        record_id,
        envelope,
    };
    let refused = receive(&mut peer, &guest, lying).unwrap_err();
    assert!(refused.contains("do not name exactly"), "{refused}");
}

/// A record that had no units until a change gave it its index (its app
/// gained a unit after it was made) has what the changes after it wrote
/// read by that index: an edit of the notes that follows writes the notes'
/// unit, not the whole record its claim could not name.
#[test]
fn changes_after_the_first_unit_index_are_read_by_it() {
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
    step(&mut doc, edit_value(), None);
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
    );
    let written = signed_write(&mut doc, &heads, &key);
    receive(&mut peer, &guest, written).expect("the notes edit wrote the notes alone");
}

/// An item kept as its order key alone (a list of plain ids, rows of a
/// grid) is the scalar under its id: writing it writes that item's unit,
/// exactly as writing an entry map does.
#[test]
fn an_item_stored_as_its_order_key_writes_its_own_unit() {
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

/// Without the app's surface a change is held, never merged on trust, and
/// decided once the surface is here.
#[test]
fn a_change_whose_app_surface_has_not_arrived_is_held_until_it_has() {
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
            TransportOrigin::RelayDelta(guest.device_public_key),
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

/// An op naming another surface than its record's identity is refused, but
/// the change naming a newer one for the record is decided as an upgrade.
#[test]
fn an_op_naming_another_surface_than_its_record_is_refused() {
    let (mut peer, key, guest) = workspace_with(Role::Editor);
    let resurfaced = |message: NeighbourMsg| {
        let NeighbourMsg::AppDocument {
            peer: handle,
            app_id,
            cluster_id,
            record_id,
            mut envelope,
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
            record_id,
            envelope,
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
            record_id: RECORD.into(),
            envelope: Box::new(envelope),
        },
    )
    .expect("an editor names the newer surface");
}

/// A person's device that writes a change showing another name, as only a
/// bot allowed it may, is refused by every receiver.
#[test]
fn a_persons_change_showing_another_name_is_refused() {
    let (mut peer, key, guest) = workspace_with(Role::Editor);
    let impostor = tilo_actions::ActionInvoker::actor("person:guest").with_appearance(Some(
        tilo_actions::Appearance::new("Owner", None, None).unwrap(),
    ));
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
        tilo_actions::ActionInvoker::actor("person:guest"),
    );
    receive(&mut peer, &guest, plain).expect("an editor's own edit is admitted");
}

/// The bot device whose installation allows Personas may show another
/// name; the same bot installed with its own appearance only may not.
#[test]
fn a_bot_allowed_personas_may_show_another_name_and_no_other_bot_may() {
    use tilo_action_policy::{Answer, KindSet, Level, Rule, TreePath};
    let edit = Rule::new(
        TreePath::root()
            .any(Level::Workspace)
            .app(APP)
            .kind(KIND)
            .resource(RECORD)
            .field("value")
            .action("field-edit"),
        KindSet::READ_UNDOABLE,
        Answer::Allow,
    );
    let program = crate::state::bot_members_tests::program_invoking(
        ActionKey::new(APP, KIND, "field-edit"),
        vec![edit],
    );
    for (policy, admitted) in [
        (crate::AppearancePolicy::Personas, true),
        (crate::AppearancePolicy::Own, false),
    ] {
        let (mut peer, _, _) = workspace_with(Role::Viewer);
        let owner_key = SigningKey::from_bytes(&[2; 32]);
        let owner = certificate(&SigningKey::from_bytes(&[1; 32]), &owner_key, "owner");
        let (key, device) = crate::state::bot_members_tests::installed_bot(
            &mut peer,
            &owner_key,
            &owner,
            &program,
            policy,
            now(),
        );
        let actor = format!("bot:{}", crate::state::bot_members_tests::BOT);
        let invoker = |appearance: Option<tilo_actions::Appearance>| {
            tilo_actions::ActionInvoker::actor(actor.clone())
                .with_kind(tilo_actions::ActorKind::Bot)
                .with_appearance(appearance)
        };
        let shown = guest_write_by(
            &peer,
            &key,
            |doc| doc.put(ROOT, "value", "hi").unwrap(),
            edit_value(),
            invoker(Some(
                tilo_actions::Appearance::new("alice", None, None).unwrap(),
            )),
        );
        assert_eq!(
            receive(&mut peer.clone(), &device, shown).is_ok(),
            admitted,
            "{policy:?}"
        );
        let plain = guest_write_by(
            &peer,
            &key,
            |doc| doc.put(ROOT, "value", "own name").unwrap(),
            edit_value(),
            invoker(None),
        );
        receive(&mut peer, &device, plain).expect("a bot writes under its own name");
    }
}

/// The workspace runs one version of an app: a change made with another is held,
/// not merged, until its author runs the workspace's version and sends the same
/// changes naming that version's surface, with the record upgraded to it.
#[test]
fn a_change_made_with_another_version_than_the_workspace_runs_waits_for_its_author_to_update() {
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
    step(&mut doc, edit_value(), None);
    let old = signed_write(&mut doc, &heads, &key);
    let held = peer
        .receive_from(
            TransportOrigin::RelayDelta(guest.device_public_key),
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

/// The record's `value` as the peer holds it.
fn value(peer: &StatePeer) -> String {
    AutoCommit::load(&peer.document(APP, RECORD).unwrap())
        .unwrap()
        .get(ROOT, "value")
        .unwrap()
        .unwrap()
        .0
        .to_string()
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
        record_id: RECORD.into(),
        envelope: Box::new(envelope),
    }
}
