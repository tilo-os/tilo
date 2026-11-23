//! A member's write of the workspace's shared desktop is decided by what it does
//! to what someone else made: the workspace's windows are every member's to close
//! and move, and closing or renaming a strip someone else made needs the
//! workspace's moderation grant. The same write is what
//! the workspace's other devices then draw: a page one device opens arrives over
//! the peer replication and stands on another device's shell, with what the first
//! device changed about it afterwards.
use std::collections::HashMap;

use automerge::AutoCommit;
use ed25519_dalek::SigningKey;
use tilo_desktop_model::layout::{Strip, Tab, TileContent, WindowGroup};
use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT, DESKTOP_KIND, SharedDesktop};
use tilo_desktop_model::{Layout, StripId, WindowWidth, Workspace, WorkspaceId, shared_resource};
use tilo_session_core::desktop::state::DesktopHandle;
use tilo_user_identity::{DeviceCert, RevocationList};

use crate::join::{InviteGrant, JoinChallenge, Role, issue_invitation, sign_join_proof};
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
    let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Workspace");
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
        tree: Layout {
            active: workspace.id(),
            workspaces: vec![workspace],
            ..Layout::default()
        },
        records: Vec::new(),
        items: Vec::new(),
        authors: windows
            .iter()
            .map(|(id, who)| (*id, (*who).to_owned()))
            .collect(),
        opened_by: std::collections::BTreeMap::new(),
    }
}

/// A workspace whose desktop holds the owner's window 1, and a member of another
/// person holding `role` in it, with their device key and certificate.
fn workspace_with(role: Role) -> (StatePeer, String, SigningKey, DeviceCert) {
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
    shared_resource::write(&mut record, &desktop(&[(1, &owner_person)])).unwrap();
    record.commit();
    peer.install_seed(
        SeedState {
            workspace_changes: seed_workspace(WORKSPACE, &owner, std::slice::from_ref(&owner))
                .unwrap(),
            documents: vec![SeedDocument {
                app_id: DESKTOP_APP.into(),
                document_id: DESKTOP_DOCUMENT.into(),
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
    (peer, owner_person, guest_key, guest)
}

/// The guest's signed write of the workspace's desktop as `desktop`, made as
/// `person`.
fn write(
    peer: &StatePeer,
    key: &SigningKey,
    person: &str,
    sequence: u64,
    desktop: &SharedDesktop,
) -> NeighbourMsg {
    write_of(peer, key, person, sequence, DESKTOP_DOCUMENT, |doc| {
        shared_resource::write(doc, desktop).unwrap();
    })
}

/// `key`'s signed write of `record` in the workspace, as `person` makes it with
/// `change`: a document nobody wrote yet is founded by the whole of it.
fn write_of(
    peer: &StatePeer,
    key: &SigningKey,
    person: &str,
    sequence: u64,
    record: &str,
    change: impl FnOnce(&mut AutoCommit),
) -> NeighbourMsg {
    let held = peer.document(DESKTOP_APP, record);
    let mut doc = match &held {
        Some(bytes) => AutoCommit::load(bytes).unwrap(),
        None => AutoCommit::new(),
    };
    doc.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = doc.get_heads();
    change(&mut doc);
    doc.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(shared_resource::change_message(Some(person))),
    );
    let mutation = if held.is_some() {
        collab_types::DocumentMutation::Incremental(doc.save_after(&heads))
    } else {
        collab_types::DocumentMutation::Snapshot(doc.save())
    };
    let author = collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
    let mut envelope = collab_types::SignedOp::new_unsigned(
        mutation.encode().unwrap(),
        collab_types::document_artifact_id(DESKTOP_APP, WORKSPACE, record),
        author,
        collab_types::document_attestation_id(DESKTOP_APP, WORKSPACE, record),
        collab_types::OpKind::Change,
        now(),
        sequence,
    );
    collab_types::sign_op(&mut envelope, key).unwrap();
    NeighbourMsg::AppDocument {
        peer: crate::peer_handle(author.as_bytes()),
        app_id: DESKTOP_APP.into(),
        cluster_id: WORKSPACE.into(),
        document_id: record.into(),
        envelope: Box::new(envelope),
        readers: None,
    }
}

tilo_test_env::scenario! {
    fn the_shared_desktop_is_written_by_whom_the_workspace_lets;

    phase a_view_of_the_layout_is_its_persons_to_write;
    // A person's view of the workspace's layout is theirs to write: another member reads it and may
    // not write it, whatever the workspace lets them do to the layout itself. The workspace's
    // policy says so, by a grant it gives each of its people on their own view.
    {
        use tilo_desktop_model::person_view::{PersonView, resource_of};

        let (mut peer, owner, key, guest) = workspace_with(Role::Editor);
        let view = PersonView {
            front_strip: Some(StripId(10)),
            ..PersonView::default()
        };
        let theirs = resource_of(&owner);
        let person = hex::encode(guest.user_public_key);
        let intrudes = write_of(&peer, &key, &person, 1, &theirs, |doc| {
            view.write(doc).unwrap();
        });
        let refused = receive(&mut peer, &guest, intrudes).unwrap_err();
        assert!(refused.contains("document change forbidden"), "{refused}");
        assert!(
            peer.document(DESKTOP_APP, &theirs).is_none(),
            "nothing of the refused write is taken in"
        );

        let own = resource_of(&person);
        let looks = write_of(&peer, &key, &person, 1, &own, |doc| {
            view.write(doc).unwrap();
        });
        let taken = peer
            .receive_from(
                crate::TransportOrigin::ServerDelta(guest.device_public_key),
                looks,
                now(),
            )
            .expect("the guest writes their own view");
        assert_eq!(taken.rejected, 0, "the guest's own view is theirs to write");
    }

    phase an_editor_closes_any_window_but_renames_no_strip_someone_else_made;
    // The workspace's windows are every member's: an editor opens a window, closes their own and
    // the owner's. The strip is not theirs, so they may not rename it.
    {
        let (mut peer, owner, key, guest) = workspace_with(Role::Editor);
        let person = hex::encode(guest.user_public_key);
        let opened = write(&peer, &key, &person, 1, &desktop(&[(1, &owner), (2, &person)]));
        receive(&mut peer, &guest, opened).expect("an editor opens a window");
        assert_eq!(windows(&peer), [1, 2]);

        let mut renames = desktop(&[(1, &owner), (2, &person)]);
        renames.tree.workspaces[0].strips[0].name = "Mine".into();
        let renames = write(&peer, &key, &person, 2, &renames);
        let refused = receive(&mut peer, &guest, renames).unwrap_err();
        assert!(refused.contains("may not rename 10"), "{refused}");
        assert_eq!(strip_names(&peer), ["Workspace"], "refused, the desktop is as it was");

        let closes_own = write(&peer, &key, &person, 3, &desktop(&[(1, &owner)]));
        receive(&mut peer, &guest, closes_own).expect("an editor closes their own window");
        assert_eq!(windows(&peer), [1]);

        let closes_owners = write(&peer, &key, &person, 4, &desktop(&[]));
        receive(&mut peer, &guest, closes_owners).expect("an editor closes the owner's window");
        assert!(windows(&peer).is_empty());
    }

    phase a_moderator_closes_someone_elses_window;
    // A moderator closes anyone's window and renames a strip someone else made.
    {
        let (mut peer, _, key, guest) = workspace_with(Role::Moderator);
        let person = hex::encode(guest.user_public_key);
        let mut clears = desktop(&[]);
        clears.tree.workspaces[0].strips[0].name = "Renamed".into();
        let clears = write(&peer, &key, &person, 1, &clears);
        receive(&mut peer, &guest, clears).expect("a moderator closes any window");
        assert!(windows(&peer).is_empty());
        assert_eq!(strip_names(&peer), ["Renamed"]);
    }

    phase another_devices_page_reaches_the_shell_and_its_rename_follows;
    // A second device of the workspace draws the page the first one opened: the
    // workspace's desktop record the first device publishes reaches it through the
    // replication, and its shell shows that window. When the first device renames
    // the page, the window on the shell carries the new name.
    //
    // The shell is the one that stands for the whole time, as a session's does: it
    // is the arriving desktop that gives it the page, and the next arrival that
    // changes it.
    {
        let (mut peer, owner, key, guest) = workspace_with(Role::Editor);
        let person = hex::encode(guest.user_public_key);
        let shell = DesktopHandle::new(Layout::default(), 7);

        let opened = write(
            &peer,
            &key,
            &person,
            1,
            &desktop_with_page(&owner, &person, "Draft", "draft"),
        );
        receive(&mut peer, &guest, opened).expect("an editor opens a page");
        assert_eq!(
            drawn(&shell, &peer),
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
            &person,
            2,
            &desktop_with_page(&owner, &person, "Draft, for review", "draft"),
        );
        receive(&mut peer, &guest, renamed).expect("an editor renames their own page");
        assert_eq!(
            drawn(&shell, &peer),
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
            "the shell shows the page under its new name, on the record it was opened on"
        );
    }
}

fn windows(peer: &StatePeer) -> Vec<u64> {
    let doc = AutoCommit::load(&peer.document(DESKTOP_APP, DESKTOP_DOCUMENT).unwrap()).unwrap();
    shared_resource::read(&doc, None)
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

fn strip_names(peer: &StatePeer) -> Vec<String> {
    let doc = AutoCommit::load(&peer.document(DESKTOP_APP, DESKTOP_DOCUMENT).unwrap()).unwrap();
    shared_resource::read(&doc, None)
        .unwrap()
        .tree
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .map(|strip| strip.name.clone())
        .collect()
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

/// One window as a shell draws it: the strip it stands in, its tab, the name on
/// it, the app it runs, and the record it is a page of when it is one.
#[derive(Debug, PartialEq, Eq)]
struct Drawn {
    strip: u64,
    tab: u64,
    title: String,
    app: String,
    record: Option<String>,
}

/// The windows `shell` draws once it is given the desktop `peer` replicated, the
/// way a mount gives its shell one that arrived — through the desktop's own
/// reconcile of the tree, with the record each window is a page of beside it.
fn drawn(shell: &DesktopHandle, peer: &StatePeer) -> Vec<Drawn> {
    let doc = AutoCommit::load(&peer.document(DESKTOP_APP, DESKTOP_DOCUMENT).unwrap()).unwrap();
    let Some(shared) = shared_resource::read(&doc, None) else {
        return Vec::new();
    };
    let records: HashMap<u64, String> = shared.records.into_iter().collect();
    shell.replace_shared_tree(shared.tree, &[]);
    shell.read(|tree| {
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
