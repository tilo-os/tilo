//! A workspace's files are the Files app's own record: each device's mount
//! publishes the tree it holds on the record's route, and what one device's
//! mount changes reaches the person's other devices — a device that joins
//! later is given the tree whole, and a device of someone else is refused.
//!
//! The session here is the record's plane: the mounts reach it as two
//! certified devices of one person reach the workspace their records route on,
//! and the tree is read back through `fs_protocol::tree`, the projection the
//! Files app renders.
//!
//! A session admits a change it journals as a local publication only from the
//! device it runs on ("native local publication signer differs from selected
//! device"): what another of the person's devices writes reaches it over the
//! person's store link (`Session`'s `PersonalSync` door), which a test cannot
//! reach from outside. So the mounts here are one device's, writing and
//! reading the one record: what the holder changes reaches a mount given the
//! tree, which is the same `receive` a change that arrived from the link takes.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    reason = "test fixtures: a failed step is the assertion, and the session \
              lives until the mounts it serves are done"
)]

// A host admits its members' records through an Automerge worker: this binary is that worker, as it
// is the session's own.
tilo_automerge_isolate::test_worker!();

mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fs::namespace::{Namespace, ROOT_INO};
use fs_protocol::files::FileNodeKind;
use fs_protocol::tree::{FilesTree, ROOT_ID, Tree};
use tilo_lock::MutexRecover as _;
use tilo_resource::Resource as _;
use tilo_session::Session;
use tilo_user_identity::IdentityPaths;

use common::personal::{self, Other, Personal};

/// The app whose person record a workspace's tree is.
const FILES: &str = "so.tilo.file-explorer";
/// The kind of record it keeps.
const KIND: &str = "files.tree";

/// What Files declares of the person's records, as its package ships: the
/// surface the session admits a join of this record against.
const SURFACE: &[u8] = include_bytes!("../../../../apps/file-explorer/surface.toml");

#[test]
fn a_tree_changed_here_reaches_the_persons_other_devices() {
    let laptop = Personal::new("Ada");
    let phone = laptop.second("Ada's phone");
    let tablet = laptop.second("Ada's tablet");
    let cluster = laptop.cluster();
    tilo_action_broker::register_package_surface(FILES, SURFACE);
    let state = tempfile::tempdir().expect("a state for the session");
    personal::hold_workspace(
        state.path(),
        &laptop.device,
        &laptop.cert,
        &cluster,
        FILES,
        KIND,
    );
    let socket = laptop.socket().to_path_buf();
    let mut session =
        personal::session(&socket, &laptop, "Ada", &cluster, state.path(), FILES, true);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime for the mounts");
    let _in_runtime = runtime.enter();

    // The laptop mounts a tree of its own: it is the record's first holder.
    let here = namespace("ada-laptop");
    here.lock_recover()
        .create_dir(ROOT_INO, "Notes", 1_000)
        .expect("a folder");
    here.lock_recover()
        .create_file(ROOT_INO, "readme.txt", None, 5, 1_000)
        .expect("a file");
    let _laptop_mount = mount(&here, &cluster, &laptop.paths);

    // The phone mounts nothing yet: it is given the tree the laptop holds.
    let there = namespace("ada-phone");
    let _phone_mount = mount(&there, &cluster, &phone.paths);
    wait_for_tree(
        &mut session,
        &there,
        &["Notes", "readme.txt"],
        "the phone to hold the tree the laptop mounts",
    );

    // What the laptop's mount changes reaches the phone as the mount makes it.
    here.lock_recover()
        .create_file(ROOT_INO, "todo.md", None, 0, 2_000)
        .expect("a file");
    wait_for_tree(
        &mut session,
        &there,
        &["Notes", "readme.txt", "todo.md"],
        "the phone to hold the file the laptop added",
    );
    let notes = here
        .lock_recover()
        .resolve_path_ino("Notes")
        .expect("the folder the laptop made");
    assert!(
        here.lock_recover()
            .rename(ROOT_INO, "todo.md", notes, "todo.md", 3_000),
        "the laptop moves its file into the folder"
    );
    wait_for_tree(
        &mut session,
        &there,
        &["Notes", "Notes/todo.md", "readme.txt"],
        "the phone to hold the move the laptop made",
    );
    assert!(
        here.lock_recover()
            .rename(notes, "todo.md", notes, "todo-1.md", 4_000),
        "the laptop renames the file it moved"
    );
    wait_for_tree(
        &mut session,
        &there,
        &["Notes", "Notes/todo-1.md", "readme.txt"],
        "the phone to hold the rename the laptop made",
    );
    let moved = here
        .lock_recover()
        .resolve_path_ino("Notes/todo-1.md")
        .expect("the renamed file");
    assert!(here.lock_recover().remove(moved), "the laptop deletes it");
    wait_for_tree(
        &mut session,
        &there,
        &["Notes", "readme.txt"],
        "the phone to hold the deletion the laptop made",
    );

    // A device that mounts the workspace later is given the tree whole.
    let later = namespace("ada-tablet");
    let _tablet_mount = mount(&later, &cluster, &tablet.paths);
    let held = tree(&here);
    let expected: Vec<String> = contents(&held);
    wait_for_tree(
        &mut session,
        &later,
        &expected.iter().map(String::as_str).collect::<Vec<_>>(),
        "the tablet to be given the tree whole",
    );

    // A device of someone else's is refused the person's own tree.
    let someone = common::people::Person::new("Bob");
    let stranger = Other::new(&someone, "Bob's laptop");
    let refused = joins(
        &mut session,
        &socket,
        &stranger.device,
        &stranger.cert,
        &cluster,
    );
    assert!(
        refused.is_err(),
        "a device of another person joins the person's own tree: {refused:?}"
    );
}

/// The tree a person's mount holds reaches their other device over the
/// record route, as two real sessions of theirs serve them.
#[test]
fn a_tree_changed_on_one_device_reaches_the_persons_other_device() {
    let (site, ada, laptop, phone) = common::people::two_devices_of_one_person_with_files();
    let installed = package_store::Catalog::at(laptop.store(), None);
    assert_eq!(
        app_catalogue::apps::signed_surface_of(&installed, FILES)
            .expect("the store reads")
            .as_deref(),
        Some(SURFACE),
        "the surface Files' installed package carries"
    );
    let cluster = collab_types::personal_cluster(&ada.public).to_string();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime for the mounts");
    let _in_runtime = runtime.enter();

    let here = namespace("ada-laptop");
    here.lock_recover()
        .create_dir(ROOT_INO, "Notes", 1_000)
        .expect("a folder");
    here.lock_recover()
        .create_file(ROOT_INO, "readme.txt", None, 5, 1_000)
        .expect("a file");
    let _laptop_mount = mount_on(&laptop, &here, &cluster);

    let there = namespace("ada-phone");
    let _phone_mount = mount_on(&phone, &there, &cluster);
    wait_held(
        &site,
        &[&laptop, &phone],
        &there,
        &["Notes", "readme.txt"],
        "the tree the laptop holds",
    );

    here.lock_recover()
        .create_file(ROOT_INO, "todo.md", None, 0, 2_000)
        .expect("a file");
    wait_held(
        &site,
        &[&laptop, &phone],
        &there,
        &["Notes", "readme.txt", "todo.md"],
        "the file the laptop added",
    );
    let notes = here
        .lock_recover()
        .resolve_path_ino("Notes")
        .expect("the folder the laptop made");
    assert!(
        here.lock_recover()
            .rename(ROOT_INO, "todo.md", notes, "todo.md", 3_000),
        "the laptop moves its file into the folder"
    );
    wait_held(
        &site,
        &[&laptop, &phone],
        &there,
        &["Notes", "Notes/todo.md", "readme.txt"],
        "the move the laptop made",
    );
    assert!(
        here.lock_recover()
            .rename(notes, "todo.md", notes, "todo-1.md", 4_000),
        "the laptop renames the file it moved"
    );
    wait_held(
        &site,
        &[&laptop, &phone],
        &there,
        &["Notes", "Notes/todo-1.md", "readme.txt"],
        "the rename the laptop made",
    );
    let moved = here
        .lock_recover()
        .resolve_path_ino("Notes/todo-1.md")
        .expect("the renamed file");
    assert!(here.lock_recover().remove(moved), "the laptop deletes it");
    wait_held(
        &site,
        &[&laptop, &phone],
        &there,
        &["Notes", "readme.txt"],
        "the deletion the laptop made",
    );

    // What the other device changes reaches the first one the same way.
    there
        .lock_recover()
        .create_file(ROOT_INO, "from-phone.md", None, 0, 5_000)
        .expect("a file");
    wait_held(
        &site,
        &[&laptop, &phone],
        &here,
        &["Notes", "readme.txt", "from-phone.md"],
        "the file the phone made",
    );
}

/// Wait until `namespace` holds exactly `expected`, naming what was waited for.
fn wait_held(
    site: &common::people::Site,
    devices: &[&common::people::Device],
    namespace: &Arc<Mutex<Namespace>>,
    expected: &[&str],
    what: &str,
) {
    let mut expected: Vec<String> = expected.iter().map(|path| (*path).to_owned()).collect();
    expected.sort();
    common::people::waited(what, site, devices, || {
        let mut held = contents(&tree(namespace));
        held.sort();
        held == expected
    });
}

/// `namespace`'s tree as the record reads it: the projection the Files app
/// renders.
fn tree(namespace: &Arc<Mutex<Namespace>>) -> Tree {
    let bytes = namespace.lock_recover().save();
    let document = automerge::AutoCommit::load(&bytes).expect("the tree's document loads");
    FilesTree::read(&document).expect("the tree reads")
}

/// Every path in `tree`, the folders' contents by their folder's path.
fn contents(tree: &Tree) -> Vec<String> {
    fn walk(tree: &Tree, parent: &str, prefix: &str, found: &mut Vec<String>) {
        for (id, entry) in tree.children(parent) {
            let name = String::from_utf8_lossy(entry.name.as_bytes()).into_owned();
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            found.push(path.clone());
            if entry.kind == FileNodeKind::Directory {
                walk(tree, id, &path, found);
            }
        }
    }
    let mut found = Vec::new();
    walk(tree, ROOT_ID, "", &mut found);
    found
}

/// A tree document nothing has mounted yet, as a device mounts a workspace it
/// holds no tree of.
fn namespace(author: &str) -> Arc<Mutex<Namespace>> {
    Arc::new(Mutex::new(Namespace::new(author).expect("a tree document")))
}

/// Publish `namespace` on `cluster` as `paths`'s device, as the files daemon
/// serving the mount does.
fn mount(
    namespace: &Arc<Mutex<Namespace>>,
    cluster: &str,
    paths: &IdentityPaths,
) -> tokio::task::JoinHandle<()> {
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock reads after the epoch")
        .as_nanos();
    let credentials = fs::identity::load_certified_mount_identity(
        paths,
        u64::try_from(now_ns).expect("the clock reads a time a certificate holds"),
    )
    .expect("this device's certificate is valid")
    .into_parts()
    .1;
    fs::tree_record::spawn(
        Arc::clone(namespace),
        cluster.to_owned(),
        credentials,
        tilo_paths::xdg::service_socket("collab"),
    )
}

/// Publish `namespace` on `cluster` as `device`'s mount does: the daemon of
/// that device, on the collaboration socket its session serves.
fn mount_on(
    device: &common::people::Device,
    namespace: &Arc<Mutex<Namespace>>,
    cluster: &str,
) -> tokio::task::JoinHandle<()> {
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock reads after the epoch")
        .as_nanos();
    let credentials = fs::identity::load_certified_mount_identity(
        &device.identity(),
        u64::try_from(now_ns).expect("the clock reads a time a certificate holds"),
    )
    .expect("this device's certificate is valid")
    .into_parts()
    .1;
    fs::tree_record::spawn(
        Arc::clone(namespace),
        cluster.to_owned(),
        credentials,
        device
            .directory()
            .join("runtime")
            .join("tilo/services/collab.sock"),
    )
}

/// Run `session` until this device's tree holds exactly `expected`, failing
/// with the tree it holds instead.
fn wait_for_tree(
    session: &mut Session,
    namespace: &Arc<Mutex<Namespace>>,
    expected: &[&str],
    what: &str,
) {
    let mut expected: Vec<String> = expected.iter().map(|path| (*path).to_owned()).collect();
    expected.sort();
    let held = common::wait_for(session, what, |session| {
        session.settle_services();
        session.step(Duration::from_millis(5)).expect("step");
        let mut held = contents(&tree(namespace));
        held.sort();
        (held == expected).then_some(held)
    });
    assert_eq!(held, expected, "{what}");
}

/// What the session answers a device of `cert`, presenting `device`'s proof
/// that it is that certificate's, joining the person's own tree.
fn joins(
    session: &mut Session,
    socket: &Path,
    device: &ed25519_dalek::SigningKey,
    cert: &tilo_user_identity::DeviceCert,
    cluster: &str,
) -> Result<(), String> {
    let channel = personal::channel(FILES, cluster, KIND);
    let presentation = personal::presentation(device, cert, cluster);
    let client = personal::connect(socket, FILES, cluster);
    let (tell, told) = std::sync::mpsc::channel();
    tokio::spawn(async move {
        let _ = tell.send(
            client
                .join_channel(&channel, &presentation)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string()),
        );
    });
    common::wait_for(session, "the session to answer the join", |session| {
        session.settle_services();
        session.step(Duration::from_millis(5)).expect("step");
        told.try_recv().ok()
    })
}
