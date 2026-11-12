//! A member's write of the workspace's shared desktop is decided by what it does
//! to what someone else made: closing, moving or renaming a window another
//! person opened needs the workspace's moderation grant. The same write is what
//! the workspace's other devices then draw: a page one device opens arrives over
//! the peer replication and stands on another device's desk, with what the first
//! device changed about it afterwards.
use std::collections::HashMap;

use automerge::AutoCommit;
use ed25519_dalek::SigningKey;
use tilo_desktop_model::layout::{Strip, Tab, TileContent, WindowGroup};
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_KIND, DESKTOP_RECORD, SharedDesktop};
use tilo_desktop_model::{
    DesktopTree, StripId, WindowWidth, Workspace, WorkspaceSlot, shared_record,
};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource};
use tilo_user_identity::{DeviceCert, RevocationList};

use crate::join::{InviteGrant, JoinChallenge, MemberRole, issue_invitation, sign_join_proof};
use crate::owner::testing::member_certificate as certificate;
use crate::owner::testing::{now, receive};
use crate::{NeighbourMsg, SeedDocument, SeedState, StatePeer, seed_workspace};

const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaab";

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
        view: String::new(),
        pinned: false,
    }
}

/// A desktop of one strip holding a window per `(tab, who)`, each in a
/// group of its own.
fn desktop(windows: &[(u64, &str)]) -> SharedDesktop {
    let mut workspace = Workspace::new(WorkspaceSlot(1), "Workspace");
    let mut strip = Strip::new(StripId(10), "Workspace");
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
            active: workspace.slot,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        },
        records: Vec::new(),
        authors: windows
            .iter()
            .map(|(id, who)| (*id, (*who).to_owned()))
            .collect(),
        opened_by: std::collections::BTreeMap::new(),
    }
}

/// A workspace whose desktop holds the owner's window 1, and a member of another
/// person holding `role` in it, with their device key and certificate.
fn workspace_with(role: MemberRole) -> (StatePeer, String, SigningKey, DeviceCert) {
    let owner_root = SigningKey::from_bytes(&[1; 32]);
    let owner_key = SigningKey::from_bytes(&[2; 32]);
    let owner = certificate(&owner_root, &owner_key, "owner");
    let owner_person = hex::encode(owner.user_public_key);
    let mut peer = StatePeer::new(
        WORKSPACE,
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
            workspace_changes: seed_workspace(WORKSPACE, &owner, std::slice::from_ref(&owner))
                .unwrap(),
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
            room: WORKSPACE.into(),
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

/// The guest's signed write of the workspace's desktop as `desktop`.
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
        collab_types::document_artifact_id(DESKTOP_APP, WORKSPACE, DESKTOP_RECORD),
        author,
        collab_types::document_attestation_id(DESKTOP_APP, WORKSPACE, DESKTOP_RECORD),
        collab_types::OpKind::Change,
        now(),
        sequence,
    );
    collab_types::sign_op(&mut envelope, key).unwrap();
    NeighbourMsg::AppDocument {
        peer: crate::peer_handle(author.as_bytes()),
        app_id: DESKTOP_APP.into(),
        cluster_id: WORKSPACE.into(),
        record_id: DESKTOP_RECORD.into(),
        envelope: Box::new(envelope),
    }
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
    let (mut peer, owner, key, guest) = workspace_with(MemberRole::Editor);
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
    let (mut peer, _, key, guest) = workspace_with(MemberRole::Moderator);
    let closes_owners = write(&peer, &key, 1, &desktop(&[]));
    receive(&mut peer, &guest, closes_owners).expect("a moderator closes any window");
    assert!(windows(&peer).is_empty());
}

/// A desktop holding the owner's window 1, and one window that is `person`'s
/// page: the workspace's record `record`, shown under the name `title`.
fn desktop_with_page(owner: &str, person: &str, title: &str, record: &str) -> SharedDesktop {
    let mut desktop = desktop(&[(1, owner), (2, person)]);
    let page = &mut desktop.tree.workspaces[0].strips[0].window_groups[1].tabs[0];
    page.title = title.to_owned();
    desktop.records.push((page.id, record.to_owned()));
    desktop
}

/// A desk that presents nothing: no window here is laid out or shown, so this
/// source is never asked for a tile, and it answers the tab's own name when it
/// is, as a host with no app to open would.
struct Notices;

impl TileSource for Notices {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> ui::widget::TileHost {
        ui::widget::TileHost::new(ui::widget::TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }
}

/// One window as a desk draws it: the strip it stands in, its tab, the name on
/// it, the app it runs, and the record it is a page of when it is one.
#[derive(Debug, PartialEq, Eq)]
struct Drawn {
    strip: u64,
    tab: u64,
    title: String,
    app: String,
    record: Option<String>,
}

/// The windows `desk` draws once it is given the desktop `peer` replicated, the
/// way a mount gives its desk one that arrived — through the surface's own
/// reconcile of the tree, with the record each window is a page of beside it.
fn drawn(desk: &DesktopHandle, peer: &StatePeer) -> Vec<Drawn> {
    let doc = AutoCommit::load(&peer.document(DESKTOP_APP, DESKTOP_RECORD).unwrap()).unwrap();
    let Some(shared) = shared_record::read(&doc, None) else {
        return Vec::new();
    };
    let records: HashMap<u64, String> = shared.records.into_iter().collect();
    desk.replace_shared_tree(shared.tree, &[]);
    desk.read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| {
                strip
                    .window_groups
                    .iter()
                    .flat_map(|group| &group.tabs)
                    .map(|tab| Drawn {
                        strip: strip.id.0,
                        tab: tab.id,
                        title: tab.title.clone(),
                        app: match &tab.tile {
                            TileContent::LocalApp { app_id, .. } => app_id.clone(),
                            other => format!("{other:?}"),
                        },
                        record: records.get(&tab.id).cloned(),
                    })
            })
            .collect()
    })
}

/// A second device of the workspace draws the page the first one opened: the
/// workspace's desktop record the first device publishes reaches it through the
/// replication, and its desk shows that window. When the first device renames
/// the page, the window on the desk carries the new name.
///
/// The desk is the one that stands for the whole time, as a session's does: it
/// is the arriving desktop that gives it the page, and the next arrival that
/// changes it.
#[test]
fn another_devices_page_reaches_the_desk_and_its_rename_follows() {
    let (mut peer, owner, key, guest) = workspace_with(MemberRole::Editor);
    let person = hex::encode(guest.user_public_key);
    let (_surface, desk) = DesktopSurface::new(DesktopTree::default(), 7, Box::new(Notices));

    let opened = write(
        &peer,
        &key,
        1,
        &desktop_with_page(&owner, &person, "Draft", "draft"),
    );
    receive(&mut peer, &guest, opened).expect("an editor opens a page");
    assert_eq!(
        drawn(&desk, &peer),
        [
            Drawn {
                strip: 10,
                tab: 1,
                title: "tab 1".into(),
                app: "so.tilo.notes".into(),
                record: None,
            },
            Drawn {
                strip: 10,
                tab: 2,
                title: "Draft".into(),
                app: "so.tilo.notes".into(),
                record: Some("draft".into()),
            },
        ]
    );

    let renamed = write(
        &peer,
        &key,
        2,
        &desktop_with_page(&owner, &person, "Draft, for review", "draft"),
    );
    receive(&mut peer, &guest, renamed).expect("an editor renames their own page");
    assert_eq!(
        drawn(&desk, &peer),
        [
            Drawn {
                strip: 10,
                tab: 1,
                title: "tab 1".into(),
                app: "so.tilo.notes".into(),
                record: None,
            },
            Drawn {
                strip: 10,
                tab: 2,
                title: "Draft, for review".into(),
                app: "so.tilo.notes".into(),
                record: Some("draft".into()),
            },
        ],
        "the desk shows the page under its new name, on the record it was opened on"
    );
}
