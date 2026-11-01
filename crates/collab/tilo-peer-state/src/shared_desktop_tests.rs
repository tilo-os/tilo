//! A member's write of the room's shared desktop is decided by what it does
//! to what someone else made: closing, moving or renaming a window another
//! person opened needs the room's moderation grant.
use automerge::AutoCommit;
use ed25519_dalek::SigningKey;
use tilo_desktop_model::layout::{Strip, Tab, TileContent, WindowGroup};
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_KIND, DESKTOP_RECORD, SharedDesktop};
use tilo_desktop_model::{
    DesktopTree, StripId, WindowWidth, Workspace, WorkspaceId, shared_record,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use web_time::{Duration, SystemTime};

use crate::join::{InviteGrant, JoinChallenge, MemberRole, issue_invitation, sign_join_proof};
use crate::{NeighbourMsg, SeedDocument, SeedState, StatePeer, TransportOrigin, seed_room};

const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaab";

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

fn tab(id: u64) -> Tab {
    Tab {
        id,
        title: format!("tab {id}"),
        tile: TileContent::LocalApp {
            app_id: "so.tilo.notes".into(),
            label: "Notes".into(),
            note: String::new(),
        },
        app_title: None,
    }
}

/// A desktop of one strip holding a window per `(tab, who)`, each in a
/// group of its own.
fn desktop(windows: &[(u64, &str)]) -> SharedDesktop {
    let mut workspace = Workspace::new(WorkspaceId(1), "Room");
    let mut strip = Strip::new(StripId(10), "Room");
    for (id, _) in windows {
        strip.window_groups.push(WindowGroup {
            id: id.wrapping_add(100),
            tabs: vec![tab(*id)],
            active_tab: *id,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
    }
    workspace.strips.push(strip);
    SharedDesktop {
        tree: DesktopTree {
            active: workspace.id,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        },
        records: Vec::new(),
        authors: windows
            .iter()
            .map(|(id, who)| (*id, (*who).to_owned()))
            .collect(),
    }
}

/// A room whose desktop holds the owner's window 1, and a member of another
/// person holding `role` in it, with their device key and certificate.
fn room_with(role: MemberRole) -> (StatePeer, String, SigningKey, DeviceCert) {
    let owner_root = SigningKey::from_bytes(&[1; 32]);
    let owner_key = SigningKey::from_bytes(&[2; 32]);
    let owner = certificate(&owner_root, &owner_key, "owner");
    let owner_person = hex::encode(owner.user_public_key);
    let mut peer = StatePeer::new(
        ROOM,
        owner.clone(),
        vec![owner.clone()],
        RevocationList::new(),
        now(),
    )
    .unwrap();
    let mut record = AutoCommit::new();
    shared_record::write(&mut record, &desktop(&[(1, &owner_person)])).unwrap();
    record.commit();
    peer.install_seed(
        SeedState {
            room_changes: seed_room(ROOM, &owner, std::slice::from_ref(&owner)).unwrap(),
            documents: vec![SeedDocument {
                app_id: DESKTOP_APP.into(),
                record_id: DESKTOP_RECORD.into(),
                kind: DESKTOP_KIND.into(),
                bytes: record.save(),
            }],
        },
        &owner_key,
        now(),
    )
    .unwrap();

    let now_ns: u64 = 2000 * 1_000_000_000;
    let guest_key = SigningKey::from_bytes(&[42; 32]);
    let guest = certificate(&SigningKey::from_bytes(&[41; 32]), &guest_key, "guest");
    let signed = issue_invitation(
        InviteGrant {
            workspace_id: 7,
            room: ROOM.into(),
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
    (peer, owner_person, guest_key, guest)
}

/// The guest's signed write of the room's desktop as `desktop`.
fn write(
    peer: &StatePeer,
    key: &SigningKey,
    sequence: u64,
    desktop: &SharedDesktop,
) -> NeighbourMsg {
    let mut doc = AutoCommit::load(&peer.document(DESKTOP_APP, DESKTOP_RECORD).unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    shared_record::write(&mut doc, desktop).unwrap();
    let person = hex::encode(key.verifying_key().to_bytes());
    doc.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(shared_record::change_message(Some(&person))),
    );
    let mutation = collab_types::DocumentMutation::Incremental(doc.save_after(&heads));
    let author = collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
    let mut envelope = collab_types::SignedOp::new_unsigned(
        mutation.encode().unwrap(),
        collab_types::document_artifact_id(DESKTOP_APP, ROOM, DESKTOP_RECORD),
        author,
        collab_types::document_attestation_id(DESKTOP_APP, ROOM, DESKTOP_RECORD),
        collab_types::OpKind::Change,
        now(),
        sequence,
    );
    collab_types::sign_op(&mut envelope, key).unwrap();
    NeighbourMsg::AppDocument {
        peer: crate::peer_handle(author.as_bytes()),
        app_id: DESKTOP_APP.into(),
        cluster_id: ROOM.into(),
        record_id: DESKTOP_RECORD.into(),
        envelope: Box::new(envelope),
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

fn windows(peer: &StatePeer) -> Vec<u64> {
    let doc = AutoCommit::load(&peer.document(DESKTOP_APP, DESKTOP_RECORD).unwrap()).unwrap();
    shared_record::read(&doc, None)
        .unwrap()
        .tree
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .map(|tab| tab.id)
        .collect()
}

/// An editor opens and closes their own window, and may not close the
/// owner's.
#[test]
fn an_editor_closes_their_own_window_but_not_someone_elses() {
    let (mut peer, owner, key, guest) = room_with(MemberRole::Editor);
    let person = hex::encode(guest.user_public_key);
    let opened = write(&peer, &key, 1, &desktop(&[(1, &owner), (2, &person)]));
    receive(&mut peer, &guest, opened).expect("an editor opens a window");
    assert_eq!(windows(&peer), [1, 2]);

    let closes_owners = write(&peer, &key, 2, &desktop(&[(2, &person)]));
    let refused = receive(&mut peer, &guest, closes_owners).unwrap_err();
    assert!(refused.contains("may not close 1"), "{refused}");
    assert_eq!(windows(&peer), [1, 2], "refused, the desktop is as it was");

    let closes_own = write(&peer, &key, 3, &desktop(&[(1, &owner)]));
    receive(&mut peer, &guest, closes_own).expect("an editor closes their own window");
    assert_eq!(windows(&peer), [1]);
}

/// A moderator closes anyone's window.
#[test]
fn a_moderator_closes_someone_elses_window() {
    let (mut peer, _, key, guest) = room_with(MemberRole::Moderator);
    let closes_owners = write(&peer, &key, 1, &desktop(&[]));
    receive(&mut peer, &guest, closes_owners).expect("a moderator closes any window");
    assert!(windows(&peer).is_empty());
}
