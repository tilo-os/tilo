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
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use web_time::{Duration, SystemTime};

use crate::join::{InviteGrant, JoinChallenge, MemberRole, issue_invitation, sign_join_proof};
use crate::{NeighbourMsg, SeedDocument, SeedState, StatePeer, TransportOrigin, seed_room};

const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const APP: &str = "test.app";
const RECORD: &str = "tab:1";
const KIND: &str = "test";

/// The test app's surface: `comment` sits under the comments, writes the
/// record and the comment it names; every field offers edits, each
/// writing its own unit.
struct App;

impl ReceiptSurface for App {
    fn kind(&self) -> &str {
        KIND
    }
    fn verb(&self, action: &str) -> Option<(ActionKind, Option<String>)> {
        (action == "comment").then(|| (ActionKind::Undoable, Some("comments".into())))
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
            _ => vec![Vec::new()],
        }
    }
}

/// Finds the test app's surface, or none at all (it has not arrived).
struct Source(bool);

impl SurfaceSource for Source {
    fn surface(&self, _: &Record<'_>, _: Option<&str>) -> Option<Arc<dyn ReceiptSurface>> {
        self.0.then(|| Arc::new(App) as Arc<dyn ReceiptSurface>)
    }
}

fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(2000))
        .unwrap()
}

fn certificate(root: &SigningKey, key: &SigningKey, label: &str) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

/// A room whose record keeps its comments as units, and a member of
/// another person holding `role` in it.
fn room_with(role: MemberRole) -> (StatePeer, SigningKey, DeviceCert) {
    let owner_key = SigningKey::from_bytes(&[2; 32]);
    let owner = certificate(&SigningKey::from_bytes(&[1; 32]), &owner_key, "owner");
    let mut peer = StatePeer::new(
        ROOM,
        owner.clone(),
        vec![owner.clone()],
        RevocationList::new(),
        now(),
    )
    .unwrap();
    let mut record = AutoCommit::new();
    let index = record
        .put_object(ROOT, collab_types::units::UNIT_INDEX, ObjType::List)
        .unwrap();
    record.insert(&index, 0, "comments/*").unwrap();
    record.put_object(ROOT, "comments", ObjType::Map).unwrap();
    record.put(ROOT, "value", "seed").unwrap();
    record.commit_with(
        CommitOptions::default()
            .with_message(tilo_actions::FrameworkChange::Genesis.change_message()),
    );
    peer.install_seed(
        SeedState {
            room_changes: seed_room(ROOM, &owner, std::slice::from_ref(&owner)).unwrap(),
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
            workspace_id: 7,
            room: ROOM.into(),
            authority: owner.clone(),
            issuer: owner,
            delegation: None,
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
        peer.admit_invited_member(&redemption, &owner_key, now())
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
    let mut doc = AutoCommit::load(&peer.document(APP, RECORD).unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    write(&mut doc);
    step(&mut doc, what, item);
    // Signed as the author's device signs it, which a receiver does not
    // trust to have checked its own grants.
    let mutation = collab_types::DocumentMutation::Incremental(doc.save_after(&heads));
    let author = collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
    let mut envelope = collab_types::SignedOp::new_unsigned(
        mutation.encode().unwrap(),
        collab_types::document_artifact_id(APP, ROOM, RECORD),
        author,
        collab_types::document_attestation_id(APP, ROOM, RECORD),
        collab_types::OpKind::Change,
        now(),
        1,
    );
    collab_types::sign_op(&mut envelope, key).unwrap();
    NeighbourMsg::AppDocument {
        peer: crate::peer_handle(author.as_bytes()),
        app_id: APP.into(),
        cluster_id: ROOM.into(),
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

fn receive(peer: &mut StatePeer, guest: &DeviceCert, message: NeighbourMsg) -> Result<(), String> {
    peer.receive_from(
        TransportOrigin::RelayDelta(guest.device_public_key),
        message,
        now(),
    )
    .map(|_| ())
}

use automerge::ReadDoc as _;

/// A commenter's comment is granted; their edit of the record is not.
#[test]
fn a_change_is_refused_when_its_action_is_not_granted_to_its_author() {
    let (mut peer, key, guest) = room_with(MemberRole::Commenter);
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

    let (mut peer, key, guest) = room_with(MemberRole::Editor);
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
    let (mut peer, key, guest) = room_with(MemberRole::Commenter);
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

/// Without the app's surface a change is held, never merged on trust.
#[test]
fn a_change_whose_app_surface_has_not_arrived_is_held() {
    let (mut peer, key, guest) = room_with(MemberRole::Editor);
    peer.set_surfaces(Surfaces::from_source(Arc::new(Source(false))));
    let edited = guest_write(
        &peer,
        &key,
        |doc| doc.put(ROOT, "value", "x").unwrap(),
        edit_value(),
        None,
    );
    let held = receive(&mut peer, &guest, edited).unwrap_err();
    assert!(held.contains("held"), "{held}");
    assert_eq!(
        AutoCommit::load(&peer.document(APP, RECORD).unwrap())
            .unwrap()
            .get(ROOT, "value")
            .unwrap()
            .unwrap()
            .0
            .to_string(),
        "\"seed\"",
        "nothing merged"
    );
}
