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
                document_id: RECORD.into(),
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
        ..ActionContext::for_resource("tab:1", KIND, "Test")
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
        own: None,
        readers: None,
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
            own,
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
            own,
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
                own,
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
                own,
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
                own: None,
                readers: None,
            },
        )
        .expect("an editor names the newer surface");
    }

    phase a_persons_change_showing_another_name_is_refused;
    // A person's device that writes a change showing another name, as only a
    // bot allowed it may, is refused by every receiver.
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

    phase a_bot_allowed_personas_may_show_another_name_and_no_other_bot_may;
    // The bot device whose installation allows Personas may show another
    // name; the same bot installed with its own appearance only may not.
    {
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
            let invoker = |appearance: Option<tilo_actions::Appearance>| {
                tilo_actions::ActionInvoker::bot(crate::state::bot_members_tests::BOT)
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
        document_id: RECORD.into(),
        envelope: Box::new(envelope),
        own: None,
        readers: None,
    }
}

/// A server of another person, named among the workspace's key-holding
/// holders: its device key, its certificate.
fn named_server(peer: &mut StatePeer, owner: &SigningKey) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[92; 32]);
    let server = certificate(&SigningKey::from_bytes(&[91; 32]), &key, "Household server");
    let hint = crate::owner::testing::governor::holding_hint(
        &server,
        &key,
        tilo_server_protocol::WorkspaceId::parse(WORKSPACE).unwrap(),
        iroh_base::EndpointAddr::new(
            iroh_base::EndpointId::from_bytes(&server.device_public_key).unwrap(),
        ),
    );
    assert!(
        peer.governance_mut()
            .author_keeping_device(&hint, owner, now())
            .unwrap()
    );
    (key, server)
}

/// The member's delegation of `app`'s undoable work to `server`, signed by
/// their own device `key`.
fn delegating(
    key: &SigningKey,
    person: [u8; 32],
    server: [u8; 32],
    app: &str,
) -> collab_types::work_delegation::WorkDelegation {
    use tilo_action_policy::{Answer, Grant, Grantee, KindSet, TreePath};
    let member = Grantee::person(hex::encode(person));
    let grant = Grant::new(
        "delegated-work",
        member.clone(),
        TreePath::workspace(WORKSPACE).app(app),
        KindSet::READ_UNDOABLE,
        Answer::Allow,
        member,
    );
    let at = u64::try_from(
        now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap();
    collab_types::work_delegation::WorkDelegation::sign(
        key,
        person,
        server,
        WORKSPACE,
        None,
        vec![grant],
        Vec::new(),
        (at.saturating_sub(1), at.saturating_add(10_000_000_000)),
    )
    .unwrap()
}

/// The server's signed write of the document's value as `invoker`, its `seq`th.
fn server_write(
    peer: &StatePeer,
    key: &SigningKey,
    seq: u64,
    value: &str,
    invoker: tilo_actions::ActionInvoker,
) -> NeighbourMsg {
    server_write_stamped(peer, key, seq, value, invoker, now())
}

/// [`server_write`] naming `at` as its time.
fn server_write_stamped(
    peer: &StatePeer,
    key: &SigningKey,
    seq: u64,
    value: &str,
    invoker: tilo_actions::ActionInvoker,
    at: std::time::SystemTime,
) -> NeighbourMsg {
    let mut doc = AutoCommit::load(&peer.document(APP, "tab:1").unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    doc.put(ROOT, "value", value).unwrap();
    let context = ActionContext {
        what: Some(edit_value()),
        ..ActionContext::for_resource("tab:1", KIND, "Test")
    }
    .with_invoker(invoker);
    doc.commit_with(CommitOptions::default().with_message(context.change_message().unwrap()));
    signed_write_stamped(&mut doc, &heads, key, seq, at)
}

/// The invoker history files for a server's write of the member's work.
fn the_servers_invoker(member: &str, server: [u8; 32]) -> tilo_actions::ActionInvoker {
    tilo_actions::ActionInvoker::person(member.to_owned()).from_device(Some(
        tilo_actions::InvokerDevice::certified(server, "Household server"),
    ))
}

/// The history entry the change `server` wrote carries.
fn history_invoker(peer: &StatePeer, server: [u8; 32]) -> tilo_actions::ActionInvoker {
    let mut doc = AutoCommit::load(&peer.document(APP, "tab:1").unwrap()).unwrap();
    let actor = automerge::ActorId::from(server.to_vec());
    doc.get_changes(&[])
        .into_iter()
        .rev()
        .filter(|change| change.actor_id() == &actor)
        .find_map(|change| {
            change
                .message()
                .and_then(ActionContext::from_change_message)
        })
        .expect("the server's change names what it did")
        .invoker
        .unwrap()
}

/// What `write` changes on `peer`, as a receiver takes the changes in.
fn workspace_changes(peer: &mut StatePeer, write: impl FnOnce(&mut StatePeer)) -> Vec<Vec<u8>> {
    let heads = peer.admitted_workspace().unwrap().clone().get_heads();
    write(peer);
    peer.admitted_workspace()
        .unwrap()
        .clone()
        .get_changes(&heads)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect()
}

/// A member delegates their work to a server of another person: every holder
/// takes the delegation in from the workspace, the server's work is admitted
/// as that member's — history naming their actor and the server it ran on —
/// another member's work is refused, and a revocation stops it at once.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario: the delegation, the work under it, its refusals and its revocation"
)]
fn a_server_takes_a_members_work_only_under_their_delegation() {
    use tilo_actions::{ActionInvoker, InvokerDevice};

    let (mut peer, guest_key, guest) = workspace_with(Role::Editor);
    let owner_key = SigningKey::from_bytes(&[2; 32]);
    let (server_key, server) = named_server(&mut peer, &owner_key);
    let guest_person = hex::encode(guest.user_public_key);
    let owner_person = hex::encode(peer.owner());
    let delegation = delegating(
        &guest_key,
        guest.user_public_key,
        server.device_public_key,
        APP,
    );

    // The member's device signs the delegation into the workspace, and a
    // holder that did not write it takes it in like any other change.
    let mut receiver = peer.clone();
    let delegation_changes = workspace_changes(&mut peer, |copy| {
        assert!(
            copy.workspace_mut()
                .author_work_delegation(&delegation, &guest_key, now())
                .unwrap()
        );
        assert_eq!(
            copy.workspace().work_delegations(),
            vec![delegation.clone()]
        );
    });
    receiver
        .receive_from(
            TransportOrigin::ServerDelta(guest.device_public_key),
            NeighbourMsg::SyncResp {
                changes: delegation_changes,
                workspace: WORKSPACE.into(),
            },
            now(),
        )
        .unwrap();
    assert_eq!(
        receiver.workspace().work_delegations(),
        vec![delegation.clone()],
        "every member reads the delegation beside who holds the workspace"
    );

    // The server takes the member's work: the holder admits it as theirs, and
    // history names their actor and the server it ran on.
    let invoker = the_servers_invoker(&guest_person, server.device_public_key);
    let work = server_write(
        &receiver,
        &server_key,
        1,
        "set by the server",
        invoker.clone(),
    );
    receive(&mut receiver, &server, work).expect("the server takes the member's work");
    let named = history_invoker(&receiver, server.device_public_key);
    assert_eq!(
        named.acting.by,
        tilo_action_policy::Actor::Member(
            tilo_action_policy::MemberId::parsed_person(&guest_person).unwrap()
        )
    );
    assert_eq!(named.acting.people(), vec![guest_person.as_str()]);
    assert_eq!(
        named.device.as_ref().map(|device| device.id.clone()),
        Some(hex::encode(server.device_public_key)),
        "history names the server it was done on"
    );

    // Another member's work is refused: nobody delegated it to this server.
    let for_another = server_write(
        &receiver,
        &server_key,
        2,
        "set for someone else",
        the_servers_invoker(&owner_person, server.device_public_key),
    );
    let refused = receive(&mut receiver.clone(), &server, for_another).unwrap_err();
    assert!(
        refused.contains("delegated it and no one else"),
        "{refused}"
    );

    // Nor may the server name the member and act on another device.
    let elsewhere = server_write(
        &receiver,
        &server_key,
        2,
        "set under another device",
        ActionInvoker::person(guest_person)
            .from_device(Some(InvokerDevice::certified([7; 32], "Somewhere else"))),
    );
    let refused = receive(&mut receiver.clone(), &server, elsewhere).unwrap_err();
    assert!(refused.contains("server it ran on"), "{refused}");

    // The member revokes it; every holder that takes the revocation in
    // refuses the server's work from then on.
    let revocation = workspace_changes(&mut peer, |copy| {
        assert!(
            copy.workspace_mut()
                .revoke_work_delegation(
                    &server.device_public_key,
                    &guest.user_public_key,
                    &guest_key,
                    now()
                )
                .unwrap()
        );
    });
    receiver
        .receive_from(
            TransportOrigin::ServerDelta(guest.device_public_key),
            NeighbourMsg::SyncResp {
                changes: revocation,
                workspace: WORKSPACE.into(),
            },
            now(),
        )
        .unwrap();
    assert!(
        receiver
            .workspace()
            .work_delegation(&server.device_public_key, &guest.user_public_key, now())
            .is_none(),
        "a revocation stops the delegation at once"
    );
    assert!(
        receiver.workspace().work_delegations()[0]
            .revoked_at_ns
            .is_some(),
        "and stays read by every holder that took it"
    );
    let after_revocation = server_write(
        &receiver,
        &server_key,
        2,
        "set after the revocation",
        invoker,
    );
    let refused = receive(&mut receiver.clone(), &server, after_revocation).unwrap_err();
    assert!(
        refused.contains("absent from governed roster") || refused.contains("certified identity"),
        "{refused}"
    );
}

/// A member takes their delegation back as a typed command from their own
/// device; another member's command takes none of their work back, and a
/// revocation already written is no change.
#[test]
fn a_member_revokes_their_delegation_from_their_own_device() {
    use crate::workspace_membership::WorkspaceMembershipCommand;

    let (mut peer, guest_key, guest) = workspace_with(Role::Editor);
    let owner_key = SigningKey::from_bytes(&[2; 32]);
    let owner = certificate(&SigningKey::from_bytes(&[1; 32]), &owner_key, "owner");
    let (_, server) = named_server(&mut peer, &owner_key);
    let delegation = delegating(
        &guest_key,
        guest.user_public_key,
        server.device_public_key,
        APP,
    );
    assert!(
        peer.workspace_mut()
            .author_work_delegation(&delegation, &guest_key, now())
            .unwrap()
    );

    // Another member's command names their own work, so it takes nothing of
    // the guest's delegation back.
    let by_another = WorkspaceMembershipCommand::RevokeWorkDelegation {
        server: server.device_public_key,
    };
    assert!(
        !peer
            .workspace_mut()
            .apply_workspace_membership_command(&owner, &by_another, &owner_key, now())
            .unwrap()
    );
    assert!(
        peer.workspace()
            .work_delegation(&server.device_public_key, &guest.user_public_key, now())
            .is_some(),
        "the guest's delegation is theirs alone to take back"
    );

    // The member's own device revokes it: every holder refuses the server's
    // work for them from then on, and the row says it was revoked.
    assert!(
        peer.workspace_mut()
            .apply_workspace_membership_command(&guest, &by_another, &guest_key, now())
            .unwrap()
    );
    assert!(
        peer.workspace()
            .work_delegation(&server.device_public_key, &guest.user_public_key, now())
            .is_none()
    );
    assert!(
        peer.workspace().work_delegations()[0]
            .revoked_at_ns
            .is_some()
    );
    // Saying it again is no change.
    assert!(
        !peer
            .workspace_mut()
            .apply_workspace_membership_command(&guest, &by_another, &guest_key, now())
            .unwrap()
    );
}

/// The changes by which the device `writer` puts `form` straight into its row
/// of the workspace's delegations, over whatever `peer` holds there: no verb
/// of the workspace's is asked, as any writer of that document can.
fn delegation_written_straight(
    peer: &StatePeer,
    writer: [u8; 32],
    form: &collab_types::work_delegation::WorkDelegation,
) -> Vec<Vec<u8>> {
    let mut doc = peer.admitted_workspace().unwrap().clone();
    doc.set_actor(peer.workspace_actor_for(&writer));
    let heads = doc.get_heads();
    let (_, rows) = doc
        .get(ROOT, collab_types::work_delegation::WORK_DELEGATIONS_KEY)
        .unwrap()
        .expect("a workspace is founded with its delegations");
    doc.put(&rows, form.row_key(), serde_json::to_string(form).unwrap())
        .unwrap();
    doc.commit();
    doc.get_changes(&heads)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect()
}

/// `changes` to the workspace's document arriving at `holder` from `device`.
fn take_workspace_changes(
    holder: &mut StatePeer,
    device: [u8; 32],
    changes: Vec<Vec<u8>>,
) -> Result<(), String> {
    holder
        .receive_from(
            TransportOrigin::ServerDelta(device),
            NeighbourMsg::SyncResp {
                changes,
                workspace: WORKSPACE.into(),
            },
            now(),
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// A member's withdrawal of their delegation stays. The delegation as it was
/// first signed still verifies, and whoever writes the workspace's document
/// can put that first form back into its row: no holder reads it live again,
/// the server takes none of the member's work under it, and every holder
/// still reads that it was withdrawn.
#[test]
fn a_withdrawn_delegation_stays_withdrawn_whoever_writes_its_first_form_back() {
    let (mut peer, guest_key, guest) = workspace_with(Role::Editor);
    let owner_key = SigningKey::from_bytes(&[2; 32]);
    let (server_key, server) = named_server(&mut peer, &owner_key);
    let guest_person = hex::encode(guest.user_public_key);
    let first = delegating(
        &guest_key,
        guest.user_public_key,
        server.device_public_key,
        APP,
    );

    // The member delegates and then withdraws; a holder takes both in.
    let mut holder = peer.clone();
    for step in ["delegates", "withdraws"] {
        let changes = workspace_changes(&mut peer, |copy| {
            let written = if step == "delegates" {
                copy.workspace_mut()
                    .author_work_delegation(&first, &guest_key, now())
            } else {
                copy.workspace_mut().revoke_work_delegation(
                    &server.device_public_key,
                    &guest.user_public_key,
                    &guest_key,
                    now(),
                )
            };
            assert_eq!(written, Ok(true), "the member {step}");
        });
        take_workspace_changes(&mut holder, guest.device_public_key, changes).unwrap();
    }
    let live = |holder: &StatePeer| {
        holder
            .workspace()
            .work_delegation(&server.device_public_key, &guest.user_public_key, now())
            .is_some()
    };
    assert!(!live(&holder), "the withdrawal stops the delegation");

    // Each device that writes the workspace's document puts the first form
    // straight back: the member's own, and the device of the workspace's
    // owner, which governs it.
    let invoker = the_servers_invoker(&guest_person, server.device_public_key);
    let mut broken = Vec::new();
    for (who, writer) in [
        ("the member's own device", guest.device_public_key),
        ("the owner's device", owner_key.verifying_key().to_bytes()),
    ] {
        let mut holder = holder.clone();
        let changes = delegation_written_straight(&holder, writer, &first);
        let taken = take_workspace_changes(&mut holder, writer, changes);
        // The batch reached the holder and is refused for what it does: a
        // test that passed because nothing was delivered would say nothing.
        assert!(
            taken
                .as_ref()
                .is_err_and(|refusal| refusal.contains("moved back")),
            "written back by {who}: {taken:?}"
        );
        if live(&holder) {
            broken.push(format!(
                "written back by {who} ({taken:?}): the withdrawn delegation is read live again"
            ));
        }
        let work = server_write(
            &holder,
            &server_key,
            1,
            "set after the withdrawal",
            invoker.clone(),
        );
        if receive(&mut holder.clone(), &server, work).is_ok() {
            broken.push(format!(
                "written back by {who} ({taken:?}): the server takes the member's work again"
            ));
        }
        let withdrawal_read = holder
            .workspace()
            .work_delegations()
            .iter()
            .any(|kept| kept.row_key() == first.row_key() && kept.revoked_at_ns.is_some());
        if !withdrawal_read {
            broken.push(format!(
                "written back by {who} ({taken:?}): no holder reads the withdrawal any more"
            ));
        }
    }
    assert!(broken.is_empty(), "{broken:#?}");
}

/// A member's delegation of their work to a server, withdrawn again, as the
/// copies of the workspace hold it.
struct Withdrawn {
    /// The owner's copy while the delegation was live.
    before: StatePeer,
    /// The owner's copy with the withdrawal.
    after: StatePeer,
    /// A holder that took the delegation and then its withdrawal in.
    holder: StatePeer,
    guest_key: SigningKey,
    guest: DeviceCert,
    owner_key: SigningKey,
    server_key: SigningKey,
    server: DeviceCert,
    /// The delegation as the member first signed it.
    first: collab_types::work_delegation::WorkDelegation,
}

impl Withdrawn {
    fn made() -> Self {
        let (mut peer, guest_key, guest) = workspace_with(Role::Editor);
        let owner_key = SigningKey::from_bytes(&[2; 32]);
        let (server_key, server) = named_server(&mut peer, &owner_key);
        let first = delegating(
            &guest_key,
            guest.user_public_key,
            server.device_public_key,
            APP,
        );
        let mut holder = peer.clone();
        let delegated = workspace_changes(&mut peer, |copy| {
            let written = copy
                .workspace_mut()
                .author_work_delegation(&first, &guest_key, now());
            assert_eq!(written, Ok(true), "the member delegates");
        });
        take_workspace_changes(&mut holder, guest.device_public_key, delegated).unwrap();
        let before = peer.clone();
        let withdrawn = workspace_changes(&mut peer, |copy| {
            let written = copy.workspace_mut().revoke_work_delegation(
                &server.device_public_key,
                &guest.user_public_key,
                &guest_key,
                now(),
            );
            assert_eq!(written, Ok(true), "the member withdraws");
        });
        take_workspace_changes(&mut holder, guest.device_public_key, withdrawn).unwrap();
        Self {
            before,
            after: peer,
            holder,
            guest_key,
            guest,
            owner_key,
            server_key,
            server,
            first,
        }
    }

    /// What the member's work on the server is on `copy` now.
    fn work(&self, copy: &StatePeer) -> crate::DelegatedWork {
        copy.workspace().delegated_work(
            &self.server.device_public_key,
            &self.guest.user_public_key,
            now(),
        )
    }

    /// Whether `copy` takes the server's write of the member's work.
    fn server_works(&self, copy: &StatePeer) -> bool {
        let invoker = the_servers_invoker(
            &hex::encode(self.guest.user_public_key),
            self.server.device_public_key,
        );
        let work = server_write(copy, &self.server_key, 1, "set by the server", invoker);
        receive(&mut copy.clone(), &self.server, work).is_ok()
    }

    fn owner_device(&self) -> [u8; 32] {
        self.owner_key.verifying_key().to_bytes()
    }
}

/// Every change of `peer`'s workspace document: a base and what was written
/// on it.
fn whole_workspace(peer: &StatePeer) -> Vec<Vec<u8>> {
    peer.admitted_workspace()
        .unwrap()
        .clone()
        .get_changes(&[])
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect()
}

/// A withdrawal stays where its first form arrives beside it, written by a
/// device that had not seen it, and where the governor sends a base made
/// before it; a base made with it is taken as any base is.
#[test]
fn a_withdrawal_stays_beside_its_first_form_and_under_a_base() {
    use crate::DelegatedWork;
    let case = Withdrawn::made();
    assert!(
        case.server_works(&case.before),
        "the server works while it is live"
    );
    assert!(matches!(
        case.work(&case.holder),
        DelegatedWork::Withdrawn { .. }
    ));

    // Written on a copy that had not taken the withdrawal in, the first form
    // is no move back: it is taken in beside the withdrawal and changes
    // nothing.
    let mut holder = case.holder.clone();
    let beside = delegation_written_straight(&case.before, case.owner_device(), &case.first);
    assert_eq!(
        take_workspace_changes(&mut holder, case.owner_device(), beside),
        Ok(())
    );
    assert!(
        matches!(case.work(&holder), DelegatedWork::Withdrawn { .. }),
        "the withdrawal stands over the form beside it"
    );
    assert!(
        !case.server_works(&holder),
        "the server takes none of the member's work"
    );

    // The governor's base, made of its copy before the withdrawal, moves the
    // row back for a holder of the withdrawal: refused whole.
    let mut stale = case.before.clone();
    assert_eq!(stale.rebase_workspace(), Ok(true));
    let mut holder = case.holder.clone();
    let taken = take_workspace_changes(&mut holder, case.owner_device(), whole_workspace(&stale));
    assert!(
        taken
            .as_ref()
            .is_err_and(|refusal| refusal.contains("moved back")),
        "{taken:?}"
    );
    assert!(matches!(
        case.work(&holder),
        DelegatedWork::Withdrawn { .. }
    ));

    // Its base made with the withdrawal is taken, and the withdrawal with it.
    let mut current = case.after.clone();
    assert_eq!(current.rebase_workspace(), Ok(true));
    let mut holder = case.holder.clone();
    assert_eq!(
        take_workspace_changes(&mut holder, case.owner_device(), whole_workspace(&current)),
        Ok(())
    );
    assert!(matches!(
        case.work(&holder),
        DelegatedWork::Withdrawn { .. }
    ));
    assert!(!case.server_works(&holder));
}

/// Only the member's own devices say what their delegation is: another
/// person's device cannot withdraw it in their name; their own device signs
/// it again to live on, and never once it was withdrawn; and one that came to
/// its end is told apart from one withdrawn and from work never given.
#[test]
fn only_the_members_own_devices_say_what_their_delegation_is() {
    use crate::DelegatedWork;
    let case = Withdrawn::made();
    let (server, person) = (case.server.device_public_key, case.guest.user_public_key);

    // The owner's device signs a withdrawal in the member's name and writes it
    // over their live delegation. It is no form of the member's, so the row
    // would hold none of theirs: refused.
    let mut live = case.before.clone();
    let forged = delegating(&case.owner_key, person, server, APP)
        .revoked(&case.owner_key, case.first.issued_at_ns.saturating_add(1))
        .unwrap();
    let changes = delegation_written_straight(&live, case.owner_device(), &forged);
    let taken = take_workspace_changes(&mut live, case.owner_device(), changes);
    assert!(
        taken
            .as_ref()
            .is_err_and(|refusal| refusal.contains("cannot be erased")),
        "{taken:?}"
    );
    assert!(matches!(case.work(&live), DelegatedWork::Live(_)));

    // The member's device signs the live delegation again: the same giving,
    // seven days from now.
    let renew = |copy: &mut StatePeer| {
        copy.workspace_mut()
            .renew_work_delegations(&case.guest_key, now())
    };
    assert_eq!(renew(&mut live), Ok(1));
    let renewed = live
        .workspace()
        .work_delegation(&server, &person, now())
        .expect("a renewed delegation is live");
    assert_eq!(
        renewed.giving, case.first.giving,
        "a renewal is no new giving"
    );
    assert_eq!(
        renewed.expires_at_ns.saturating_sub(renewed.issued_at_ns),
        collab_types::work_delegation::MAX_LIFETIME_NS
    );
    assert_eq!(
        renew(&mut live),
        Ok(0),
        "with more than half its life left it stays"
    );
    let later = now()
        .checked_add(std::time::Duration::from_secs(8 * 24 * 60 * 60))
        .unwrap();
    assert_eq!(
        live.workspace().delegated_work(&server, &person, later),
        DelegatedWork::Ended {
            at_ns: renewed.expires_at_ns
        }
    );

    // Withdrawn, it is never signed again, and a renewal signed without the
    // withdrawal is refused where it is written.
    let mut holder = case.holder.clone();
    assert_eq!(renew(&mut holder), Ok(0));
    let written = holder
        .workspace_mut()
        .author_work_delegation(&renewed, &case.guest_key, now());
    assert!(
        written
            .as_ref()
            .is_err_and(|refusal| refusal.contains("a later form")),
        "{written:?}"
    );
    assert!(matches!(
        case.work(&holder),
        DelegatedWork::Withdrawn { .. }
    ));
    assert_eq!(
        holder.workspace().delegated_work(&[5; 32], &person, now()),
        DelegatedWork::NotGiven
    );
}

/// Work a member gives again after they withdrew it stands only as the giving
/// that follows the withdrawal. The form a device signs without the
/// withdrawal in hand, a first giving, is refused where it is written; the
/// giving that follows the withdrawal is live on the copy that wrote it and
/// on every holder of the withdrawal that takes it in.
#[test]
fn work_given_again_follows_its_withdrawal() {
    use crate::DelegatedWork;
    let case = Withdrawn::made();
    let (server, person) = (case.server.device_public_key, case.guest.user_public_key);
    let mut copy = case.after.clone();

    let anew = delegating(&case.guest_key, person, server, APP);
    assert_eq!(anew.giving, 1);
    let written = copy
        .workspace_mut()
        .author_work_delegation(&anew, &case.guest_key, now());
    assert!(
        written
            .as_ref()
            .is_err_and(|refusal| refusal.contains("a later form")),
        "{written:?}"
    );
    assert!(matches!(case.work(&copy), DelegatedWork::Withdrawn { .. }));

    let held = copy
        .workspace()
        .work_delegations()
        .into_iter()
        .find(|held| held.row_key() == anew.row_key())
        .expect("the withdrawal is held");
    assert!(held.revoked_at_ns.is_some());
    let again = anew.given_after(&case.guest_key, &held).unwrap();
    assert_eq!(again.giving, 2, "the giving after the one withdrawn");
    let given = workspace_changes(&mut copy, |copy| {
        let written = copy
            .workspace_mut()
            .author_work_delegation(&again, &case.guest_key, now());
        assert_eq!(written, Ok(true), "the member gives the work again");
    });
    assert!(matches!(case.work(&copy), DelegatedWork::Live(_)));
    assert!(case.server_works(&copy));

    let mut holder = case.holder.clone();
    assert_eq!(
        take_workspace_changes(&mut holder, case.guest.device_public_key, given),
        Ok(())
    );
    assert!(matches!(case.work(&holder), DelegatedWork::Live(_)));
    assert!(case.server_works(&holder));

    // The withdrawal of the giving before, written straight back over it,
    // moves the row back as the first form would: refused, and still live.
    let changes = delegation_written_straight(&holder, case.owner_device(), &held);
    let taken = take_workspace_changes(&mut holder, case.owner_device(), changes);
    assert!(
        taken
            .as_ref()
            .is_err_and(|refusal| refusal.contains("moved back")),
        "{taken:?}"
    );
    assert!(matches!(case.work(&holder), DelegatedWork::Live(_)));
}

/// What `peer` holds of the workspace's document past what `other` holds.
fn workspace_past(peer: &StatePeer, other: &StatePeer) -> Vec<Vec<u8>> {
    let heads = other.admitted_workspace().unwrap().clone().get_heads();
    peer.admitted_workspace()
        .unwrap()
        .clone()
        .get_changes(&heads)
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect()
}

/// The race of a withdrawal with the document starting over, end to end. A
/// member takes their work back on a device that has not synced; someone is
/// admitted meanwhile, so the governor starts the document over from a base
/// that still holds the delegation live. The member's device adopts that
/// base and writes its withdrawal back in the same step: it follows the
/// workspace, never reads the work as given, and every copy that takes its
/// change in reads the work withdrawn and refuses the server's.
#[test]
fn a_withdrawal_the_governors_base_lacks_is_kept_and_written_back() {
    use crate::DelegatedWork;
    let case = Withdrawn::made();
    let mut governor = case.before.clone();
    assert_eq!(governor.rebase_workspace(), Ok(true));
    assert!(matches!(case.work(&governor), DelegatedWork::Live(_)));
    assert!(
        case.server_works(&governor),
        "the governor has not seen the withdrawal"
    );

    // The member's device holds the withdrawal and knows which device it is.
    let mut device = case.holder.clone();
    device.write_as_device(case.guest.device_public_key);
    assert_eq!(
        take_workspace_changes(&mut device, case.owner_device(), whole_workspace(&governor)),
        Ok(()),
        "the base is adopted"
    );
    let generation =
        |peer: &StatePeer| crate::workspace::base::generation(peer.admitted_workspace().unwrap());
    assert_eq!(
        generation(&device),
        generation(&governor),
        "the device follows its workspace"
    );
    assert!(
        matches!(case.work(&device), DelegatedWork::Withdrawn { .. }),
        "it keeps what it saw"
    );
    assert!(!case.server_works(&device));

    // What it wrote back reaches the governor, and a copy that follows the
    // governor and never saw the withdrawal.
    let back = workspace_past(&device, &governor);
    assert!(!back.is_empty(), "the form is written back as a change");
    let mut other = governor.clone();
    for copy in [&mut governor, &mut other] {
        assert_eq!(
            take_workspace_changes(copy, case.guest.device_public_key, back.clone()),
            Ok(())
        );
        assert!(
            matches!(case.work(copy), DelegatedWork::Withdrawn { .. }),
            "every copy reads the work withdrawn"
        );
        assert!(
            !case.server_works(copy),
            "and refuses the server's work for the member"
        );
    }
}

/// A base cannot make a copy that saw a withdrawal take the work. A copy that
/// was told no device, or a device that is no member's, refuses a base that
/// lacks the withdrawal and stays on what it holds; a copy that knows its
/// member's device adopts it with the withdrawal written back. On none of
/// them does the server's work for the member pass.
#[test]
fn a_base_cannot_make_a_copy_that_saw_a_withdrawal_take_the_work() {
    use crate::DelegatedWork;
    let case = Withdrawn::made();
    let mut stale = case.before.clone();
    assert_eq!(stale.rebase_workspace(), Ok(true));
    let base = whole_workspace(&stale);

    let told_none = case.holder.clone();
    let mut told_the_server = case.holder.clone();
    told_the_server.write_as_device(case.server.device_public_key);
    for (which, mut copy) in [
        ("told no device", told_none),
        ("told a device that is no member's", told_the_server),
    ] {
        let taken = take_workspace_changes(&mut copy, case.owner_device(), base.clone());
        assert!(
            taken
                .as_ref()
                .is_err_and(|refusal| refusal.contains("moved back")),
            "{which}: {taken:?}"
        );
        assert!(
            matches!(case.work(&copy), DelegatedWork::Withdrawn { .. }),
            "{which}"
        );
        assert!(!case.server_works(&copy), "{which}");
    }

    let mut told_the_member = case.holder.clone();
    told_the_member.write_as_device(case.guest.device_public_key);
    assert_eq!(
        take_workspace_changes(&mut told_the_member, case.owner_device(), base),
        Ok(())
    );
    assert!(matches!(
        case.work(&told_the_member),
        DelegatedWork::Withdrawn { .. }
    ));
    assert!(!case.server_works(&told_the_member));
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

/// Whether `copy` holds `text` as the document's value once `work` is handed
/// over as `origin` while the copy's clock reads `at`, and what it answered.
fn handed(
    copy: &StatePeer,
    origin: TransportOrigin,
    work: &NeighbourMsg,
    at: std::time::SystemTime,
    text: &str,
) -> (bool, String) {
    let mut copy = copy.clone();
    let answered = copy
        .receive_from(origin, work.clone(), at)
        .map_or_else(|error| error.to_string(), |_| "taken in".to_owned());
    (value(&copy).contains(text), answered)
}

/// A member's withdrawal stops their work on a server at once, whatever time
/// a write names. What the member's device held of the server's work when it
/// withdrew is history: a copy that joins late replays it. A write the server
/// stamps before the withdrawal and hands over after it is taken by no copy
/// that holds the withdrawal: not as kept history, and not by a copy whose
/// clock still reads before the withdrawal.
#[test]
fn a_withdrawal_stops_the_servers_work_whatever_time_a_write_names() {
    let second = std::time::Duration::from_secs(1);
    let (mut peer, guest_key, guest) = workspace_with(Role::Editor);
    let owner_key = SigningKey::from_bytes(&[2; 32]);
    let (server_key, server) = named_server(&mut peer, &owner_key);
    let device = server.device_public_key;
    let invoker = || the_servers_invoker(&hex::encode(guest.user_public_key), device);
    let first = delegating(&guest_key, guest.user_public_key, device, APP);
    let given = peer
        .workspace_mut()
        .author_work_delegation(&first, &guest_key, now());
    assert_eq!(given, Ok(true), "the member delegates");
    let joining = peer.clone();

    // The server works for the member, and the member's device holds that
    // work when it takes the work back a second later.
    let stood = server_write(&peer, &server_key, 1, "before the withdrawal", invoker());
    receive(&mut peer, &server, stood.clone()).unwrap();
    assert_eq!(value(&peer), "\"before the withdrawal\"");
    let withdrawn_at = now() + second;
    let withdrawal = workspace_changes(&mut peer, |copy| {
        let written = copy.workspace_mut().revoke_work_delegation(
            &device,
            &guest.user_public_key,
            &guest_key,
            withdrawn_at,
        );
        assert_eq!(written, Ok(true), "the member withdraws");
    });
    let after = withdrawn_at + second;
    let kept = || TransportOrigin::RetainedServerDelta(device);
    let live = || TransportOrigin::ServerDelta(device);

    // The setup tells a refusal from an admission: work the server stamps
    // after the withdrawal is refused.
    let later = server_write_stamped(&peer, &server_key, 2, "later", invoker(), after);
    let (taken, answer) = handed(&peer, live(), &later, after, "later");
    assert!(!taken, "work after the withdrawal: {answer}");

    // A copy that joins late takes the withdrawal in, then the history the
    // workspace keeps.
    let mut wrong = Vec::new();
    let mut late = joining;
    take_workspace_changes(&mut late, guest.device_public_key, withdrawal).unwrap();
    let replayed = late
        .receive_from(kept(), stood, after)
        .map_or_else(|error| error.to_string(), |_| "taken in".to_owned());
    if value(&late) != "\"before the withdrawal\"" {
        wrong.push(format!(
            "work that stood before the withdrawal does not replay on a late joiner: {replayed}"
        ));
    }

    // After the withdrawal the server writes on and names a time before it.
    let stamped = server_write_stamped(&peer, &server_key, 2, "stamped", invoker(), now());
    for (which, copy, origin, at) in [
        ("the member's device, as kept history", &peer, kept(), after),
        ("a late joiner, as kept history", &late, kept(), after),
        ("a copy whose clock reads before it", &peer, live(), now()),
    ] {
        let (taken, answer) = handed(copy, origin, &stamped, at, "stamped");
        if taken {
            wrong.push(format!(
                "{which} takes work the server stamps before the withdrawal: {answer}"
            ));
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

/// The server's second write of `value`, naming `at` as its time, written under
/// the governance whose heads are `governance`.
fn server_write_under(
    peer: &StatePeer,
    key: &SigningKey,
    value: &str,
    invoker: tilo_actions::ActionInvoker,
    at: std::time::SystemTime,
    governance: &[[u8; 32]],
) -> NeighbourMsg {
    let mut message = server_write_stamped(peer, key, 2, value, invoker, at);
    let NeighbourMsg::AppDocument { envelope, .. } = &mut message else {
        panic!("the server's write is a document write")
    };
    envelope.governance = governance
        .iter()
        .map(collab_types::governance_head)
        .collect();
    collab_types::sign_op(envelope, key).unwrap();
    message
}

/// A withdrawn server's write that names governance its receiver does not
/// hold is held: what the receiver has not heard of may be what lets its
/// writer. Nothing of it is taken in meanwhile. Once that governance is there
/// the write is decided against all of it and refused, as the same work is at
/// once where it names no governance: the withdrawal stands, also where the
/// governance that came raised the member.
#[test]
fn a_withdrawn_servers_write_under_governance_its_receiver_lacks_is_held_then_refused() {
    let second = std::time::Duration::from_secs(1);
    let (mut peer, guest_key, guest) = workspace_with(Role::Editor);
    let (owner_key, owner) = owner();
    let (server_key, server) = named_server(&mut peer, &owner_key);
    let device = server.device_public_key;
    let invoker = || the_servers_invoker(&hex::encode(guest.user_public_key), device);
    let first = delegating(&guest_key, guest.user_public_key, device, APP);
    let given = peer
        .workspace_mut()
        .author_work_delegation(&first, &guest_key, now());
    assert_eq!(given, Ok(true), "the member delegates");
    let stood = server_write(&peer, &server_key, 1, "before the withdrawal", invoker());
    receive(&mut peer, &server, stood).unwrap();
    assert_eq!(value(&peer), "\"before the withdrawal\"");
    let withdrawn_at = now() + second;
    let withdrawn = peer.workspace_mut().revoke_work_delegation(
        &device,
        &guest.user_public_key,
        &guest_key,
        withdrawn_at,
    );
    assert_eq!(withdrawn, Ok(true), "the member withdraws");
    let after = withdrawn_at + second;
    let live = || TransportOrigin::ServerDelta(device);

    // The governor raises the member where the copy has not heard of it:
    // governance the copy lacks, and none that takes anything from the member.
    let mut governor = peer.clone();
    governor
        .governance_mut()
        .set_member_role(guest.user_public_key, Role::Moderator, &owner_key, now())
        .unwrap();
    let lacked = governor.governance().heads();

    // The setup tells a refusal from a hold: the server's work after the
    // withdrawal that names no governance is refused at once.
    let plain = server_write_stamped(&peer, &server_key, 2, "plain", invoker(), after);
    let (taken, answer) = handed(&peer, live(), &plain, after, "plain");
    assert!(
        !taken && answer != "taken in",
        "work after the withdrawal is refused at once: {answer}"
    );

    // The same work under governance the copy lacks is held, and nothing of
    // it is taken in.
    let named = server_write_under(&peer, &server_key, "named", invoker(), after, &lacked);
    let mut copy = peer.clone();
    let before = value(&copy);
    let held = copy
        .receive_from(live(), named, after)
        .expect("held, not refused, while its governance is not there");
    assert!(
        held.deferred,
        "it waits for the governance it names: {held:?}"
    );
    assert_eq!(value(&copy), before, "nothing of it is taken in");

    // The governance arrives: the held write is decided against all of it.
    let arrived = copy
        .receive_from(
            TransportOrigin::ServerDelta(owner.device_public_key),
            governor.workspace_message(),
            after,
        )
        .expect("the governor's change is taken in");
    assert_eq!(
        (arrived.rejected, arrived.deferred),
        (1, false),
        "the held write is refused once its governance is there and waits no longer: {arrived:?}"
    );
    assert_eq!(
        value(&copy),
        before,
        "the withdrawn server's work is not taken in under the governance that came"
    );
}

/// A member's work on a server, taken back: the member delegated, the server
/// wrote once while the delegation stood, and the member's device took the
/// work back a second later.
struct TakenBack {
    /// The member's device, which signed the withdrawal.
    peer: StatePeer,
    /// A copy as it stood when the work was given: it holds the delegation
    /// and nothing that came after.
    joining: StatePeer,
    guest: DeviceCert,
    owner_key: SigningKey,
    server_key: SigningKey,
    server: DeviceCert,
    /// The server's write while the delegation stood, its first.
    stood: NeighbourMsg,
    /// The changes of the workspace's document that carry the withdrawal.
    withdrawal: Vec<Vec<u8>>,
    withdrawn_at: std::time::SystemTime,
}

impl TakenBack {
    /// The story; `held` says whether the member's device had taken the
    /// server's write in when it took the work back.
    fn made(held: bool) -> Self {
        let (mut peer, guest_key, guest) = workspace_with(Role::Editor);
        let owner_key = SigningKey::from_bytes(&[2; 32]);
        let (server_key, server) = named_server(&mut peer, &owner_key);
        let device = server.device_public_key;
        let first = delegating(&guest_key, guest.user_public_key, device, APP);
        let given = peer
            .workspace_mut()
            .author_work_delegation(&first, &guest_key, now());
        assert_eq!(given, Ok(true), "the member delegates");
        let joining = peer.clone();
        let invoker = the_servers_invoker(&hex::encode(guest.user_public_key), device);
        let stood = server_write(&peer, &server_key, 1, "before the withdrawal", invoker);
        if held {
            receive(&mut peer, &server, stood.clone()).unwrap();
            assert_eq!(value(&peer), "\"before the withdrawal\"");
        }
        let withdrawn_at = now()
            .checked_add(std::time::Duration::from_secs(1))
            .unwrap();
        let withdrawal = workspace_changes(&mut peer, |copy| {
            let written = copy.workspace_mut().revoke_work_delegation(
                &device,
                &guest.user_public_key,
                &guest_key,
                withdrawn_at,
            );
            assert_eq!(written, Ok(true), "the member withdraws");
        });
        Self {
            peer,
            joining,
            guest,
            owner_key,
            server_key,
            server,
            stood,
            withdrawal,
            withdrawn_at,
        }
    }

    fn device(&self) -> [u8; 32] {
        self.server.device_public_key
    }

    /// A second after the withdrawal.
    fn after(&self) -> std::time::SystemTime {
        self.withdrawn_at
            .checked_add(std::time::Duration::from_secs(1))
            .unwrap()
    }

    /// The server's work as a copy is handed what the workspace kept of it.
    fn kept(&self) -> TransportOrigin {
        TransportOrigin::RetainedServerDelta(self.device())
    }

    /// The server's work as it arrives fresh.
    fn live(&self) -> TransportOrigin {
        TransportOrigin::ServerDelta(self.device())
    }

    /// What the server writes after the withdrawal, its second write, naming
    /// a time before the withdrawal.
    fn stamped(&self) -> NeighbourMsg {
        let invoker = the_servers_invoker(&hex::encode(self.guest.user_public_key), self.device());
        server_write_stamped(&self.peer, &self.server_key, 2, "stamped", invoker, now())
    }

    /// `copy` once the withdrawal reached it.
    fn withdrawn(&self, mut copy: StatePeer) -> StatePeer {
        take_workspace_changes(
            &mut copy,
            self.guest.device_public_key,
            self.withdrawal.clone(),
        )
        .unwrap();
        copy
    }
}

/// The heads of the test's document on `copy`: two copies that hold the same
/// document read the same ones.
fn document_heads(copy: &StatePeer) -> Vec<automerge::ChangeHash> {
    copy.document(APP, "tab:1").map_or_else(Vec::new, |bytes| {
        AutoCommit::load(&bytes).unwrap().get_heads()
    })
}

/// Every copy that holds a withdrawal decides the server's work alike, from
/// what the member signed and from nothing else. Whether the withdrawal
/// reached a copy before the history the workspace keeps or between two kept
/// writes, the copy ends with the document the member's device held when it
/// took the work back.
#[test]
fn copies_that_hold_a_withdrawal_end_alike_whatever_order_they_met_it_in() {
    let story = TakenBack::made(true);
    let stamped = story.stamped();
    let hand = |copy: &mut StatePeer, work: &NeighbourMsg| {
        copy.receive_from(story.kept(), work.clone(), story.after())
            .map_or_else(|error| error.to_string(), |_| "taken in".to_owned())
    };

    // The withdrawal first, then the history the workspace keeps.
    let mut first = story.withdrawn(story.joining.clone());
    let stood_first = hand(&mut first, &story.stood);
    let stamped_first = hand(&mut first, &stamped);
    // The write that stood, then the withdrawal, then what the server wrote
    // after it.
    let mut between = story.joining.clone();
    let stood_between = hand(&mut between, &story.stood);
    let mut between = story.withdrawn(between);
    let stamped_between = hand(&mut between, &stamped);

    let mut wrong = Vec::new();
    for (which, copy, stood, stamped) in [
        ("the withdrawal first", &first, stood_first, stamped_first),
        (
            "the withdrawal between the two writes",
            &between,
            stood_between,
            stamped_between,
        ),
    ] {
        if value(copy) != "\"before the withdrawal\""
            || document_heads(copy) != document_heads(&story.peer)
        {
            wrong.push(format!(
                "{which}: the copy does not end with the document the member's device held \
                 ({}): what stood: {stood}; what the server stamped: {stamped}",
                value(copy)
            ));
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

/// A document the withdrawal names no cut for keeps nothing of the server's
/// work. The member's device took the work back before the server's write
/// reached it, so the form it signed names nothing there, and no copy that
/// holds the withdrawal takes that write, kept or fresh, although the server
/// made it while the delegation stood. That is the safe side: a copy cannot
/// tell work the member's device never held from work the server made after
/// the withdrawal and stamped before it, and taking either would let a server
/// write on for a member who took their work back.
#[test]
fn a_document_the_withdrawal_names_no_cut_for_takes_nothing_of_the_server() {
    let story = TakenBack::made(false);
    let late = story.withdrawn(story.joining.clone());
    let mut wrong = Vec::new();
    for (which, copy, origin, at) in [
        (
            "the member's device, as kept history",
            &story.peer,
            story.kept(),
            story.after(),
        ),
        (
            "the member's device, as a fresh write",
            &story.peer,
            story.live(),
            story.after(),
        ),
        (
            "a late joiner, as kept history",
            &late,
            story.kept(),
            story.after(),
        ),
        (
            "a copy whose clock reads before the withdrawal",
            &late,
            story.live(),
            now(),
        ),
    ] {
        let (taken, answer) = handed(copy, origin, &story.stood, at, "before the withdrawal");
        if taken {
            wrong.push(format!(
                "{which} takes the server's work in a document the withdrawal names no cut for: {answer}"
            ));
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

/// Nothing reads a withdrawal live. Asked about a time before the stamp its
/// member's device gave it, a copy that holds the withdrawal answers what it
/// answers for every other time: the work is withdrawn, and the server takes
/// none of it.
#[test]
fn a_withdrawal_is_read_live_at_no_time() {
    use crate::DelegatedWork;
    let story = TakenBack::made(true);
    let (server, person) = (story.device(), story.guest.user_public_key);
    let mut wrong = Vec::new();
    for (which, copy) in [
        ("the member's device", &story.peer),
        ("a late joiner", &story.withdrawn(story.joining.clone())),
    ] {
        for (when, at) in [
            ("before the withdrawal's stamp", now()),
            ("at its stamp", story.withdrawn_at),
            ("after it", story.after()),
        ] {
            let workspace = copy.workspace();
            if workspace.work_delegation(&server, &person, at).is_some() {
                wrong.push(format!("{which} reads the delegation live {when}"));
            }
            if copy
                .workspace()
                .delegation_for_server(&server, at)
                .is_some()
            {
                wrong.push(format!("{which} lets the server take work {when}"));
            }
            let work = copy.workspace().delegated_work(&server, &person, at);
            if !matches!(work, DelegatedWork::Withdrawn { .. }) {
                wrong.push(format!("{which} does not say withdrawn {when}: {work:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

/// Only a device of the member signs the cut of their withdrawal. The
/// member's device signs what it held of the server's work. A form that keeps
/// more, signed in the member's name by the server or by another member's
/// device, is no form of theirs, and the member's own form with a larger
/// number put into it no longer verifies: whoever writes one into the row,
/// the row keeps the cut its member signed, and the server's work above it
/// stays refused.
#[test]
fn only_a_device_of_the_member_signs_the_cut_of_their_withdrawal() {
    use collab_types::work_delegation::HeldWork;
    let story = TakenBack::made(true);
    let (device, person) = (story.device(), story.guest.user_public_key);
    let stamped = story.stamped();
    let route = crate::coverage::route_key(APP, "tab:1");
    let signed = story.peer.workspace().work_delegations();
    assert_eq!(signed.len(), 1);
    assert_eq!(
        signed[0].held,
        vec![HeldWork {
            route: route.clone(),
            last: 1,
            compactions: Vec::new(),
        }],
        "the member's device signs what it held of the server's work"
    );
    let at = signed[0]
        .revoked_at_ns
        .expect("the row holds the withdrawal");
    let more = || {
        vec![HeldWork {
            route: route.clone(),
            last: 2,
            compactions: Vec::new(),
        }]
    };
    let owner = story.owner_key.verifying_key().to_bytes();
    let by_the_server = delegating(&story.server_key, person, device, APP)
        .withdrawn(&story.server_key, at, more())
        .unwrap();
    let by_another = delegating(&story.owner_key, person, device, APP)
        .withdrawn(&story.owner_key, at, more())
        .unwrap();
    let mut raised = signed[0].clone();
    raised.held = more();
    for (who, writer, form) in [
        ("the server", device, &by_the_server),
        ("another member's device", owner, &by_another),
        ("the member's own form with a larger number", owner, &raised),
    ] {
        let mut copy = story.peer.clone();
        let changes = delegation_written_straight(&copy, writer, form);
        let taken = take_workspace_changes(&mut copy, writer, changes);
        assert!(taken.is_err(), "{who} writes a cut into the row: {taken:?}");
        assert_eq!(
            copy.workspace().work_delegations(),
            signed,
            "{who}: the row keeps the cut its member signed"
        );
        let (taken, answer) = handed(&copy, story.kept(), &stamped, story.after(), "stamped");
        assert!(!taken, "{who} raised the cut: {answer}");
    }
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
