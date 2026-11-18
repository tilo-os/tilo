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
        document_id: RECORD.into(),
        envelope: Box::new(envelope),
        own: None,
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
    signed_write_at(&mut doc, &heads, key, seq)
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
