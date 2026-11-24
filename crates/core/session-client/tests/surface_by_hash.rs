//! A page that holds a record's history written by an app version it does not
//! run asks its workspace for that version's surface by hash, on the same
//! content store it asks for a picture with, keeps it, and decides what waited
//! for it. The hold names the surface as the reason it waits, so a reader is
//! told what a record wants instead of being told it wants its keys.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; one test is one transcript between a native member and a page"
)]

use std::sync::Arc;
use std::time::SystemTime;
use tilo_session_client::{Link, Outbox};

use automerge::ReadDoc as _;
use automerge::transaction::Transactable as _;
use collab_types::{
    DocumentMutation, Ed25519PublicKey, OpKind, SignedOp, document_artifact_id,
    document_attestation_id,
};
use tilo_server::workspaces::WorkspaceContent;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use crate::native::NativeDevice;
use crate::support::{
    WORKSPACE, commit_change, enrol, ready, start_host, start_peer, wait_for, workspace,
};

/// The surface the laptop's write names: an app's own `surface.toml` in the
/// form it is kept by, a version of it this page does not run.
fn older_surface() -> Vec<u8> {
    let file = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/surface.toml"
    ))
    .unwrap();
    collab_types::surface::SurfaceFile::kept(&file)
        .expect("the app's surface parses")
        .1
        .into_bytes()
}

/// A host that keeps app surfaces, as one that decided changes against them
/// does, answering only once this test lets it: the page's hold on the write,
/// and the request it makes for the surface, are the same while the host has
/// not answered.
struct Keeps {
    bytes: Vec<u8>,
    answers: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

impl Keeps {
    /// Let the host answer what it was asked.
    fn answer(&self) {
        let (open, answered) = &*self.answers;
        *open.lock().unwrap() = true;
        answered.notify_all();
    }
}

impl WorkspaceContent for Keeps {
    fn read(&self, _workspace: &str, app: &str, hash: &str) -> Option<Vec<u8>> {
        let (open, answered) = &*self.answers;
        let mut open = open.lock().unwrap();
        while !*open {
            open = answered.wait(open).unwrap();
        }
        (app == "so.tilo.tasks" && hash == blake3::hash(&self.bytes).to_hex().as_str())
            .then(|| self.bytes.clone())
    }
}

/// The tracker's issues record this test's history is written to.
fn issues_route() -> tilo_peer_state::document_frame::DocumentRoute {
    tilo_peer_state::document_frame::DocumentRoute {
        app_id: "so.tilo.tasks".into(),
        ..crate::support::route()
    }
}

/// Whether the page says it holds something for the note, and why.
fn held(peer: &Link) -> Option<String> {
    let note = issues_route();
    let held = format!("held {}/{}:", note.app_id, note.document);
    peer.status()
        .key_state
        .lines()
        .find(|line| line.starts_with(&held))
        .map(str::to_owned)
}

/// Commit the pending change as the tasks app's executor commits an edit of
/// `issue`, one item of the issues collection.
fn commit_issue_edit(document: &mut automerge::AutoCommit, issue: &str) {
    use tilo_actions::{ActionContext, Did, EditOp, FieldEdit, PathStep};
    let kind = "tasks.issues";
    let context = ActionContext {
        what: Some(Did::Edit(FieldEdit {
            ty: kind.to_owned(),
            field: "issues".to_owned(),
            path: vec![
                PathStep::Field("issues".to_owned()),
                PathStep::Item(issue.to_owned()),
            ],
            op: EditOp::Set,
            old: None,
            new: None,
            args: std::collections::BTreeMap::new(),
        })),
        ..ActionContext::for_resource(kind, kind, kind)
    };
    let mut options = automerge::transaction::CommitOptions::default();
    if let Some(message) = context.change_message() {
        options.set_message(message);
    }
    document.commit_with(options).unwrap();
}

/// Write `note`'s history from the laptop as the tasks app starts one: its issues a unit each,
/// then three issue edits naming `surface`, each with the claim its executor makes, so every
/// edit is decided against that surface. Every write is sealed with the note's content key.
/// The document the history makes.
fn write_history(
    laptop: &NativeDevice,
    note: &tilo_peer_state::document_frame::DocumentRoute,
    surface: &[u8],
) -> automerge::AutoCommit {
    let artifact = document_artifact_id(&note.app_id, &note.cluster_id, &note.document);
    let laptop_device = laptop.device.key.verifying_key().to_bytes();
    let mut document = automerge::AutoCommit::new();
    let units = document
        .put_object(
            automerge::ROOT,
            collab_types::units::UNIT_INDEX,
            automerge::ObjType::List,
        )
        .unwrap();
    document.insert(&units, 0, "issues/*").unwrap();
    let issues = document
        .put_object(automerge::ROOT, "issues", automerge::ObjType::Map)
        .unwrap();
    commit_change(&mut document);
    let mut writes = vec![(
        DocumentMutation::Snapshot(document.save()),
        None,
        Vec::new(),
    )];
    for issue_number in 1..=3 {
        let before = document.clone();
        let issue = format!("issue-{issue_number}");
        document
            .put(&issues, issue.clone(), format!("issue {issue_number}"))
            .unwrap();
        commit_issue_edit(&mut document, &issue);
        let mutation = DocumentMutation::Incremental(document.save_incremental());
        let claims = collab_types::parse_changes(mutation.data())
            .unwrap()
            .iter()
            .map(|change| {
                let units = collab_types::units::written_units(&before, change).unwrap();
                collab_types::ChangeClaim::of_change(change.hash().0, change.message(), units)
                    .unwrap()
            })
            .collect();
        writes.push((mutation, Some(*blake3::hash(surface).as_bytes()), claims));
    }
    for (seq, (mutation, named, claims)) in (1..).zip(writes) {
        let frame = mutation.encode().unwrap();
        let (metadata, sealed) = laptop.seal(note, &frame);
        let op = SignedOp::new_unsigned_with_encryption(
            sealed,
            artifact,
            Ed25519PublicKey::from_bytes(laptop_device),
            document_attestation_id(&note.app_id, &note.cluster_id, &note.document),
            OpKind::Change,
            SystemTime::now(),
            seq,
            metadata,
        )
        .with_claims(claims)
        .with_surface(named);
        laptop.write(note, op);
    }

    document
}

/// A workspace whose laptop joined, on a server whose content keeps `older_surface`, answering
/// once the test lets it.
struct Started {
    root: ed25519_dalek::SigningKey,
    server_device: crate::support::Device,
    server: crate::support::TestHost,
    surface: Vec<u8>,
    keeps: Arc<Keeps>,
    _state: tempfile::TempDir,
}

async fn started() -> Started {
    // The page uses no app of its own: every change it receives names a
    // surface it must fetch, the one this test's server keeps.
    tilo_peer_state::decide_with_surfaces(tilo_session_client::surfaces::surfaces(), |_| None);

    let (root, _) = UserIdentity::generate("Ada", 1);
    let server_device = enrol(&root, "home-server", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start_host(
        &root,
        &server_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let surface = older_surface();
    let keeps = Arc::new(Keeps {
        bytes: surface.clone(),
        answers: Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new())),
    });
    server
        .server
        .workspaces()
        .unwrap()
        .serve_content(keeps.clone());
    Started {
        root,
        server_device,
        server,
        surface,
        keeps,
        _state: state,
    }
}

/// `laptop` joined to the workspace's group, as its owner.
async fn joined(started: &Started, laptop: crate::support::Device) -> NativeDevice {
    crate::support::place(&started.server.server, &laptop.cert).await;
    let mut laptop = NativeDevice::start(
        &started.server.server,
        started.root.verifying_key().to_bytes(),
        laptop,
        workspace(),
    )
    .await;
    laptop.join().await;
    laptop
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_fetches_the_surface_the_history_it_holds_was_written_with_and_decides_it() {
    let started = started().await;
    let Started {
        root,
        server_device,
        server,
        surface,
        keeps,
        ..
    } = &started;
    let laptop = enrol(root, "laptop", DeviceCapabilities::primary());
    let browser = enrol(root, "browser", DeviceCapabilities::member());

    // The laptop joins the group the host founded, announces itself and
    // writes the record's history, sealed and naming the other version's
    // surface.
    let note = issues_route();
    let laptop = Box::pin(joined(&started, laptop)).await;
    let document = write_history(&laptop, &note, surface);
    let issues = document.get(automerge::ROOT, "issues").unwrap().unwrap().1;

    // The page joins: it asks for the workspace's keys though it holds the
    // note's first write, undecided, and the host admits it.
    let page = start_peer(
        server.iroh(),
        browser.cert.clone(),
        server_device.cert.clone(),
        browser.key.clone(),
    );
    page.join_placed(WORKSPACE, "Workspace").unwrap();
    ready(&[&page]).await;
    crate::support::admitted(&page).await;

    // The page holds the write for the surface it does not run, and says so:
    // it waits for the app surface alone, not for keys or governance.
    wait_for("the write held for its app surface", || {
        let held = held(&page);
        async move {
            held.is_some_and(|held| held.contains("for its app surface"))
                .then_some(())
        }
    })
    .await;
    let opened = crate::support::resource_of(&page, &note).expect("the record's start is opened");
    assert_eq!(
        opened.length(&issues),
        0,
        "no issue edit is opened before its surface arrives"
    );

    // The page asked its workspace for that surface. The host answers, and
    // what the page waited for is decided: it is the bytes hashed to the
    // address asked for that decide it, which the page keeps by that hash.
    keeps.answer();
    crate::support::holds(&page, &note, &document).await;
    wait_for("nothing held for the note", || async {
        held(&page).is_none().then_some(())
    })
    .await;
    assert_eq!(
        tilo_session_client::surfaces::read("so.tilo.tasks", &blake3::hash(surface).to_hex()),
        Some(surface.clone()),
        "the page keeps the surface it fetched, by its hash"
    );
}

/// The server's agents join a workspace as the server's own device, with nothing kept from
/// before it last started. A note an older version of its app wrote is decided once that device
/// has the version's surface, which the server's content keeps and answers its own device with.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_server_s_own_device_decides_what_an_older_version_wrote_from_its_server_s_surfaces() {
    let started = started().await;
    let laptop = enrol(&started.root, "laptop", DeviceCapabilities::primary());
    let laptop = Box::pin(joined(&started, laptop)).await;
    let notes: Vec<_> = ["note-1", "note-2"]
        .into_iter()
        .map(|document| tilo_peer_state::document_frame::DocumentRoute {
            document: document.into(),
            ..issues_route()
        })
        .collect();
    let written: Vec<_> = notes
        .iter()
        .map(|note| write_history(&laptop, note, &started.surface))
        .collect();

    // The server's own device joins with an empty replica, handed the workspace's keys, as
    // the server's agents are.
    let device = &started.server_device;
    let agents = start_peer(
        started.server.iroh(),
        device.cert.clone(),
        device.cert.clone(),
        device.key.clone(),
    );
    agents
        .engine()
        .hand_keys(workspace(), laptop.peer.content_keys().unwrap());
    agents.join_placed(WORKSPACE, "Workspace").unwrap();
    ready(&[&agents]).await;

    // Both notes wait for the one surface before the server answers.
    wait_for("both notes held for their app surface", || {
        let held = agents.status().key_state;
        let waiting = notes.iter().all(|note| {
            held.lines().any(|line| {
                line.starts_with(&format!("held {}/{}:", note.app_id, note.document))
                    && line.contains("for its app surface")
            })
        });
        async move { waiting.then_some(()) }
    })
    .await;
    started.keeps.answer();
    for (note, document) in notes.iter().zip(&written) {
        crate::support::holds(&agents, note, document).await;
    }
    wait_for("nothing held for either note", || {
        let held = agents.status().key_state;
        async move { (!held.contains("for its app surface")).then_some(()) }
    })
    .await;
}
