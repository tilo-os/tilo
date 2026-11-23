//! A device's own workspace service holds a workspace a server hosts as a member copy, the way
//! it holds one another device serves: it proves its membership at the address the server signs
//! for the workspace, and the copy syncs with the server from then on, across a restart of the
//! device's service too: what either seals, the other opens, with the keys the device's key
//! service takes part in the workspace's group for over the member link.

use std::sync::Arc;
use std::time::Duration;

use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};
use tilo_peer_state::workspace_documents::{DeviceDocuments as _, DeviceWrites as _, DocumentRead};
use tilo_server::WorkspaceId;
use tilo_server::workspaces::Workspaces;
use tilo_user_identity::DeviceCapabilities;
use tilo_workspace_service::device_documents::{ServiceDocuments, ServiceWrites};
use tilo_workspace_service::own_device;

use crate::common::{TestDevice, TestUser, device_endpoint, spawn_server};

const WORKSPACE: WorkspaceId = WorkspaceId::from_u128(0x6d65_6d62_6572);

/// The service `laptop` keeps under `state`, opened as a device opens its own: joining only,
/// with the device's key service beside it, which takes part in the groups of the workspaces the
/// service holds.
async fn own_service(
    state: &std::path::Path,
    laptop: &TestDevice,
) -> (
    tilo_server::service::WorkspaceService,
    own_device::KeyService,
) {
    let signer: Arc<dyn collab_types::signer::DeviceSigner> = Arc::new(laptop.sk.clone());
    let keys = own_device::KeyService::open(
        "laptop-groups",
        &state.join("groups"),
        key_group::VaultKey::new([3; 32]),
        Arc::clone(&signer),
        Arc::new(tilo_workspace_keys::NoSealChecks),
    )
    .expect("the laptop's key service opens");
    let service = own_device::open(
        state.join("service"),
        own_device::Device {
            cert: &laptop.cert,
            signer: &signer,
            itself: None,
        },
        device_endpoint(laptop).await,
        keys.groups(),
        false,
        |config| config,
    )
    .await
    .expect("the laptop's own service opens");
    keys.attach(service.workspaces());
    (service, keys)
}

/// The workspace's layout, a record every member reads and its group seals.
fn layout_route() -> tilo_peer_state::document_frame::DocumentRoute {
    tilo_peer_state::document_frame::DocumentRoute {
        app_id: DESKTOP_APP.into(),
        cluster_id: WORKSPACE.to_string(),
        document: DESKTOP_DOCUMENT.into(),
    }
}

/// The layout as `workspaces` reads it now, when it reads.
fn layout(workspaces: &Workspaces) -> Option<automerge::AutoCommit> {
    let read = ServiceDocuments::of(workspaces.clone())
        .read(&WORKSPACE.to_string(), DESKTOP_APP, DESKTOP_DOCUMENT, None)
        .ok()?;
    let DocumentRead::Whole { bytes, .. } = read else {
        return None;
    };
    automerge::AutoCommit::load(&bytes).ok()
}

/// The layout's front, as `layout` holds it.
fn front(layout: &automerge::AutoCommit) -> Option<String> {
    use automerge::ReadDoc as _;
    match layout.get(automerge::ROOT, "front").ok()? {
        Some((automerge::Value::Scalar(value), _)) => value.as_str().map(str::to_owned),
        _ => None,
    }
}

/// Wait until `workspaces` reads `want` as the layout's front.
async fn reads_front(workspaces: &Workspaces, what: &str, want: &str) {
    tilo_test_env::within(what, async {
        loop {
            let read = workspaces.clone();
            let now = tokio::task::spawn_blocking(move || layout(&read).as_ref().and_then(front))
                .await
                .unwrap();
            if now.as_deref() == Some(want) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
}

/// Write `front` as the layout's front on `workspaces`, sealed with the keys its device's key
/// service holds for the layout's group: the layout's founding write where none is held yet.
async fn write_front(workspaces: &Workspaces, what: &str, want: &str) {
    let sealed = tilo_test_env::within(what, async {
        loop {
            if let Some(Ok(keys)) = workspaces.own_route_keys(&layout_route(), true).await {
                return keys;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    let read = workspaces.clone();
    let held = tokio::task::spawn_blocking(move || layout(&read))
        .await
        .unwrap();
    let founding = held.is_none();
    let mut document = held.unwrap_or_default();
    let heads = document.get_heads();
    automerge::transaction::Transactable::put(&mut document, automerge::ROOT, "front", want)
        .unwrap();
    let changes = if founding {
        document.save()
    } else {
        document.save_after(&heads)
    };
    ServiceWrites::of(workspaces.clone())
        .write(
            &WORKSPACE.to_string(),
            &changes,
            tilo_peer_state::workspace_documents::DocumentWriting {
                app: DESKTOP_APP,
                document: DESKTOP_DOCUMENT,
                readers: None,
                founding,
                snapshot: founding,
                sealed: Some(&sealed),
            },
        )
        .await
        .unwrap_or_else(|refusal| panic!("{what}: {refusal:?}"));
}

/// Wait until `workspaces` reads `name` as the workspace's name.
async fn named(workspaces: &Workspaces, name: &str) {
    let workspace = WORKSPACE.to_string();
    tilo_test_env::within("the member copy reads the server's rename", async {
        loop {
            if workspaces
                .summary(&workspace)
                .await
                .is_ok_and(|summary| summary.name == name)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
}

/// Wait until the server reads the lease `id` held by `device`.
async fn leased(server: &Workspaces, id: &str, device: [u8; 32]) {
    let workspace = WORKSPACE.to_string();
    tilo_test_env::within("the server reads the member copy's write", async {
        loop {
            let holder = server
                .state(&workspace)
                .await
                .and_then(|state| state.workspace().lease(id, tilo_clock::wall()).holder);
            if holder.is_some_and(|claim| claim.device == device) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
}

/// Write the lease `id` on the member copy, as `copies`' device.
async fn lease(copies: &Workspaces, id: &str) {
    let workspace = WORKSPACE.to_string();
    let write = copies
        .member_membership_write(
            &workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Lease {
                id: id.into(),
                ttl_ms: 60_000,
                rank: tilo_peer_state::unranked(),
            },
        )
        .await
        .expect("the member copy writes the lease");
    copies
        .commit_member_document(&workspace, write)
        .await
        .expect("the member copy keeps the lease");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_devices_own_service_holds_a_workspace_a_server_hosts_as_a_member_copy() {
    let ada = TestUser::fresh("Ada");
    let server_device = ada.enrol("home-server", DeviceCapabilities::member());
    let laptop = ada.enrol("laptop", DeviceCapabilities::primary());
    let server_key = server_device.device_pubkey();
    let server = spawn_server(&ada, server_device, &[WORKSPACE], &[&laptop.cert]).await;
    let hosted = server
        .server
        .workspaces()
        .expect("the server holds workspaces");
    let workspace = WORKSPACE.to_string();
    let hint = hosted
        .hosts(&workspace)
        .await
        .expect("the server signs where it is reached")
        .into_iter()
        .find(|hint| hint.host.device_public_key == server_key)
        .expect("the server names its own address");

    let state = tempfile::tempdir().unwrap();
    let (service, keys) = own_service(state.path(), &laptop).await;
    let copies = service.workspaces().clone();
    assert_eq!(
        tilo_test_env::within(
            "the laptop holds the workspace as a member copy",
            copies.hold_as_member(WORKSPACE, &hint),
        )
        .await
        .expect("the server admits the laptop's membership"),
        WORKSPACE
    );
    assert!(copies.is_member(&workspace).await.unwrap());

    // Both ways: the laptop's write reaches the server, and the server's reaches the laptop.
    lease(&copies, "laptop").await;
    leased(hosted, "laptop", laptop.device_pubkey()).await;
    hosted
        .rename(&workspace, "Studio")
        .await
        .expect("the server renames the workspace");
    named(&copies, "Studio").await;

    // A sealed record both ways: the laptop's key service takes part in the workspace's group over
    // its member link, so what it seals the server opens, and what the server seals it opens.
    write_front(&copies, "the laptop seals the layout", "Main").await;
    reads_front(hosted, "the server opens the laptop's layout", "Main").await;
    write_front(hosted, "the server seals a change of the layout", "Moved").await;
    reads_front(&copies, "the laptop opens the server's change", "Moved").await;

    // The laptop's service opens again on the copy it kept, and syncs with the server from it.
    drop(copies);
    service.shutdown().await;
    drop(keys);
    let (service, keys) = own_service(state.path(), &laptop).await;
    let copies = service.workspaces().clone();
    assert!(
        copies.is_member(&workspace).await.unwrap(),
        "the copy is held as a member copy after a restart"
    );
    named(&copies, "Studio").await;
    reads_front(&copies, "the laptop opens the layout it kept", "Moved").await;
    write_front(
        &copies,
        "the laptop seals a change after its restart",
        "Again",
    )
    .await;
    reads_front(hosted, "the server opens the change", "Again").await;
    hosted
        .rename(&workspace, "Workshop")
        .await
        .expect("the server renames the workspace again");
    named(&copies, "Workshop").await;
    lease(&copies, "laptop-again").await;
    leased(hosted, "laptop-again", laptop.device_pubkey()).await;
    drop(copies);
    service.shutdown().await;
    drop(keys);
}

/// The notes app, whose records the tests below write and read.
const NOTES: &str = "so.tilo.notes";

/// Notes Ada's laptop wrote, by record, with the title each was last given.
const NOTES_WRITTEN: [(&str, &str); 3] = [
    ("groceries", "Groceries"),
    ("ideas", "Ideas"),
    ("trip", "Trip"),
];

fn note_route(note: &str) -> tilo_peer_state::document_frame::DocumentRoute {
    tilo_peer_state::document_frame::DocumentRoute {
        app_id: NOTES.into(),
        cluster_id: WORKSPACE.to_string(),
        document: note.into(),
    }
}

/// The note's title as `workspaces` reads it now, when it reads.
fn note_title(workspaces: &Workspaces, note: &str) -> Option<String> {
    use automerge::ReadDoc as _;
    let read = ServiceDocuments::of(workspaces.clone())
        .read(&WORKSPACE.to_string(), NOTES, note, None)
        .ok()?;
    let DocumentRead::Whole { bytes, .. } = read else {
        return None;
    };
    let note = automerge::AutoCommit::load(&bytes).ok()?;
    match note.get(automerge::ROOT, "title").ok()? {
        Some((automerge::Value::Scalar(value), _)) => value.as_str().map(str::to_owned),
        _ => None,
    }
}

/// Write the note `note` on `workspaces`, sealed with the keys its device's key service holds
/// for the workspace's group, as someone typing it writes it: a founding write, then edits after
/// it, the last of them titling it `title`.
async fn write_note(workspaces: &Workspaces, note: &str, title: &str) {
    let sealed = tilo_test_env::within("the note's keys", async {
        loop {
            if let Some(Ok(keys)) = workspaces.own_route_keys(&note_route(note), true).await {
                return keys;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    let mut document = automerge::AutoCommit::new();
    let mut founding = true;
    for typed in ["", "Untitled", title] {
        let heads = document.get_heads();
        automerge::transaction::Transactable::put(&mut document, automerge::ROOT, "title", typed)
            .unwrap();
        let changes = if founding {
            document.save()
        } else {
            document.save_after(&heads)
        };
        ServiceWrites::of(workspaces.clone())
            .write(
                &WORKSPACE.to_string(),
                &changes,
                tilo_peer_state::workspace_documents::DocumentWriting {
                    app: NOTES,
                    document: note,
                    readers: None,
                    founding,
                    snapshot: founding,
                    sealed: Some(&sealed),
                },
            )
            .await
            .unwrap_or_else(|refusal| panic!("the note {note}: {refusal:?}"));
        founding = false;
    }
}

/// Wait until `workspaces` reads every note Ada's laptop wrote, each with its last title.
async fn reads_every_note(workspaces: &Workspaces, what: &str) {
    tilo_test_env::within(what, async {
        loop {
            let read = workspaces.clone();
            let all = tokio::task::spawn_blocking(move || {
                NOTES_WRITTEN
                    .iter()
                    .all(|(note, title)| note_title(&read, note).as_deref() == Some(*title))
            })
            .await
            .unwrap();
            if all {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
}

/// The runtime a test that holds its process's wall clock runs its devices on, in the process
/// of its own the hold needs.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("the test's runtime")
}

/// A server holding the workspace, with notes Ada's laptop wrote there well before any other
/// device arrived, and nothing written since: the wall clock of the whole process is held 40
/// minutes back while the server starts and the notes are written, and the laptop's service
/// stops before the clock moves on, so every record is long past the window a live write is
/// taken in by the time another device arrives. Answers the server, its device and where it is
/// reached.
async fn notes_written_long_ago(
    ada: &TestUser,
    process: tilo_clock::testing::OwnProcess,
) -> (
    crate::common::TestServer,
    TestDevice,
    tilo_peer_state::join::TransportHint,
) {
    let long_ago = tilo_clock::wall()
        .checked_sub(Duration::from_secs(40 * 60))
        .expect("a wall time 40 minutes back");
    let held = tilo_clock::testing::hold_wall_everywhere(process, long_ago);
    let server_device = ada.enrol("home-server", DeviceCapabilities::member());
    let issuer = TestDevice {
        sk: server_device.sk.clone(),
        cert: server_device.cert.clone(),
    };
    let laptop = ada.enrol("laptop", DeviceCapabilities::primary());
    let server = spawn_server(ada, server_device, &[WORKSPACE], &[&laptop.cert]).await;
    let hosted = server
        .server
        .workspaces()
        .expect("the server holds workspaces")
        .clone();
    let hint = hosted
        .hosts(&WORKSPACE.to_string())
        .await
        .expect("the server signs where it is reached")
        .into_iter()
        .find(|hint| hint.host.device_public_key == issuer.device_pubkey())
        .expect("the server names its own address");

    let state = tempfile::tempdir().unwrap();
    let (service, keys) = own_service(state.path(), &laptop).await;
    let copies = service.workspaces().clone();
    tilo_test_env::within(
        "the laptop holds the workspace as a member copy",
        copies.hold_as_member(WORKSPACE, &hint),
    )
    .await
    .expect("the server admits the laptop's membership");
    for (note, title) in NOTES_WRITTEN {
        write_note(&copies, note, title).await;
    }
    reads_every_note(&hosted, "the server opens the laptop's notes").await;
    // The laptop's sync stops, and time passes.
    drop(copies);
    service.shutdown().await;
    drop(keys);
    drop(held);
    (server, issuer, hint)
}

/// A further device of the person, such as a computer restored from their root, holds the
/// workspace from the server that lists it for them and reads every note written there long
/// before it arrived, though nothing was written since: it only reads, and its key service
/// takes part in the workspace's group as it holds the copy.
#[test]
fn a_further_device_reads_every_note_written_long_before_it_arrived() {
    tilo_test_env::in_own_process(
        module_path!(),
        "a_further_device_reads_every_note_written_long_before_it_arrived",
        || {
            let process = tilo_clock::testing::OwnProcess::of_a_test_in_its_own_process();
            runtime().block_on(a_further_device_reads(process));
        },
    );
}

async fn a_further_device_reads(process: tilo_clock::testing::OwnProcess) {
    let ada = TestUser::fresh("Ada");
    let (_server, _issuer, hint) = notes_written_long_ago(&ada, process).await;

    let restored = ada.enrol("restored computer", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let (service, keys) = own_service(state.path(), &restored).await;
    let copies = service.workspaces().clone();
    tilo_test_env::within(
        "the restored computer holds the workspace as a member copy",
        copies.hold_as_member(WORKSPACE, &hint),
    )
    .await
    .expect("the server admits a further device of the person");
    reads_every_note(
        &copies,
        "the restored computer reads every note written before it arrived",
    )
    .await;
    drop(copies);
    service.shutdown().await;
    drop(keys);
}

/// Someone invited to the workspace reads every note written there long before they joined,
/// though nothing was written since.
#[test]
fn an_invited_device_reads_every_note_written_long_before_it_joined() {
    tilo_test_env::in_own_process(
        module_path!(),
        "an_invited_device_reads_every_note_written_long_before_it_joined",
        || {
            let process = tilo_clock::testing::OwnProcess::of_a_test_in_its_own_process();
            runtime().block_on(an_invited_device_reads(process));
        },
    );
}

async fn an_invited_device_reads(process: tilo_clock::testing::OwnProcess) {
    let ada = TestUser::fresh("Ada");
    let (server, issuer, _hint) = notes_written_long_ago(&ada, process).await;
    let hosted = server
        .server
        .workspaces()
        .expect("the server holds workspaces");

    let bob = TestUser::fresh("Bob");
    let desktop = bob.enrol("desktop", DeviceCapabilities::primary());
    let invited = crate::common::invitation(
        hosted,
        &issuer,
        &WORKSPACE.to_string(),
        tilo_peer_state::join::Role::Editor,
        &desktop,
    )
    .await;
    let state = tempfile::tempdir().unwrap();
    let (service, keys) = own_service(state.path(), &desktop).await;
    let copies = service.workspaces().clone();
    assert_eq!(
        tilo_test_env::within(
            "the invited device joins",
            copies.join_member(&tilo_peer_state::join::encode_invitation_text(
                &invited.signed_invitation
            )),
        )
        .await
        .expect("the server admits the invitation"),
        WORKSPACE
    );
    reads_every_note(
        &copies,
        "the invited device reads every note written before it joined",
    )
    .await;
    drop(copies);
    service.shutdown().await;
    drop(keys);
}
