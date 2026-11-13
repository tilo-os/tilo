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
    reason = "test fixtures: a failed step is the assertion, and the session \
              lives until the mounts it serves are done"
)]

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

use crate::common::personal::{self, Other, Personal};

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
    let socket = laptop.socket().to_path_buf();
    let mut session = personal::session(&socket, "Ada", &cluster, state.path(), FILES, true);
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
    change_tree(&here, |expected, what| {
        wait_for_tree(&mut session, &there, expected, what);
    });

    // A device that mounts the workspace later is given the tree whole.
    let later = namespace("ada-tablet");
    let _tablet_mount = mount(&later, &cluster, &tablet.paths);
    let expected = contents(&here);
    wait_for_tree(
        &mut session,
        &later,
        &expected.iter().map(String::as_str).collect::<Vec<_>>(),
        "the tablet to be given the tree whole",
    );

    // A device of someone else's is refused the person's own tree.
    let someone = crate::common::people::Person::new("Bob");
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
    let (site, ada, laptop, phone) = crate::common::people::two_devices_of_one_person_with_files();
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

    change_tree(&here, |expected, what| {
        wait_held(&site, &[&laptop, &phone], &there, expected, what);
    });

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

/// A shared workspace's tree is the workspace's own Files record: what one
/// member's mount changes reaches another member's mount, a person joining the
/// workspace later is given the tree whole, and a member the workspace removes
/// stops receiving it.
#[test]
fn a_shared_workspaces_tree_reaches_each_member() {
    let (site, _ada, bob, ada_laptop, bob_laptop) =
        crate::common::people::two_people_at_a_site_with_files();
    let cluster = site.workspace.clone();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime for the mounts");
    let _in_runtime = runtime.enter();

    // Ada's mount holds the workspace's tree first: the record's founder.
    let here = namespace("ada-laptop");
    here.lock_recover()
        .create_dir(ROOT_INO, "Notes", 1_000)
        .expect("a folder");
    here.lock_recover()
        .create_file(ROOT_INO, "readme.txt", None, 5, 1_000)
        .expect("a file");
    let _ada_mount = mount_on(&ada_laptop, &here, &cluster);

    // Bob's mount holds nothing yet: it is given the tree Ada's mount holds.
    let there = namespace("bob-laptop");
    let _bob_mount = mount_on(&bob_laptop, &there, &cluster);
    wait_held(
        &site,
        &[&ada_laptop, &bob_laptop],
        &there,
        &["Notes", "readme.txt"],
        "Bob to hold the tree Ada's mount holds",
    );

    // What one member's mount changes reaches the other's as it happens.
    change_tree(&here, |expected, what| {
        wait_held(&site, &[&ada_laptop, &bob_laptop], &there, expected, what);
    });
    there
        .lock_recover()
        .create_file(ROOT_INO, "from-bob.md", None, 0, 5_000)
        .expect("a file");
    wait_held(
        &site,
        &[&ada_laptop, &bob_laptop],
        &here,
        &["Notes", "readme.txt", "from-bob.md"],
        "Ada to hold the file Bob's mount made",
    );

    // A person joining the workspace later is given the tree whole.
    let cyd = crate::common::people::Person::new("Cyd");
    let cyd_laptop = crate::common::people::joined_carrying(
        &site,
        &cyd,
        "Cyd's laptop",
        &crate::common::people::Device::install_files,
    );
    site.wait_for_member(cyd.public);
    let later = namespace("cyd-laptop");
    let _cyd_mount = mount_on(&cyd_laptop, &later, &cluster);
    let expected = contents(&here);
    let devices = [&ada_laptop, &bob_laptop, &cyd_laptop];
    wait_held(
        &site,
        &devices,
        &later,
        &expected.iter().map(String::as_str).collect::<Vec<_>>(),
        "Cyd to be given the tree whole",
    );

    // A member the workspace removes reads nothing more of its tree, and
    // writes nothing into it, while the members it keeps carry on.
    site.remove(bob.public);
    site.wait_for_removal(bob.public);
    here.lock_recover()
        .create_file(ROOT_INO, "after-removal.md", None, 0, 9_000)
        .expect("a file");
    let expected = contents(&here);
    wait_held(
        &site,
        &[&ada_laptop, &cyd_laptop],
        &later,
        &expected.iter().map(String::as_str).collect::<Vec<_>>(),
        "Cyd to hold the file Ada's mount made after Bob was removed",
    );
    there
        .lock_recover()
        .create_file(ROOT_INO, "from-removed.md", None, 0, 10_000)
        .expect("a file");
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < until {
        let (bob, ada, cyd) = (contents(&there), contents(&here), contents(&later));
        assert!(
            !bob.iter().any(|path| path == "after-removal.md")
                && !ada.iter().any(|path| path == "from-removed.md")
                && !cyd.iter().any(|path| path == "from-removed.md"),
            "the removed member reads nothing the workspace writes after his removal, and writes \
             nothing into it: Bob's mount holds {bob:?}, Ada's {ada:?}, Cyd's {cyd:?}\n{}",
            bob_laptop.log(),
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // A sealed workspace's record never leaves a device in the clear: no
    // session refused an op of the tree's channel, not even the first write
    // that founded it.
    for device in [&ada_laptop, &bob_laptop, &cyd_laptop] {
        let warnings = device.warnings();
        assert!(
            !warnings.contains("a document op its channel's gate does not take"),
            "{} refused a plaintext op of the sealed tree:\n{warnings}",
            device.label,
        );
    }
}

/// What a mount changes in the tree it holds — a file added, moved into
/// `Notes`, renamed there, and deleted — each held by `held` as it happens.
fn change_tree(here: &Mounted, mut held: impl FnMut(&[&str], &str)) {
    here.lock_recover()
        .create_file(ROOT_INO, "todo.md", None, 0, 2_000)
        .expect("a file");
    held(&["Notes", "readme.txt", "todo.md"], "the file added");
    let notes = here
        .lock_recover()
        .resolve_path_ino("Notes")
        .expect("the folder made");
    assert!(
        here.lock_recover()
            .rename(ROOT_INO, "todo.md", notes, "todo.md", 3_000),
        "the file moves into the folder"
    );
    held(&["Notes", "Notes/todo.md", "readme.txt"], "the move");
    assert!(
        here.lock_recover()
            .rename(notes, "todo.md", notes, "todo-1.md", 4_000),
        "the file is renamed"
    );
    held(&["Notes", "Notes/todo-1.md", "readme.txt"], "the rename");
    let moved = here
        .lock_recover()
        .resolve_path_ino("Notes/todo-1.md")
        .expect("the renamed file");
    assert!(here.lock_recover().remove(moved), "the file is deleted");
    held(&["Notes", "readme.txt"], "the deletion");
}

/// Wait until `namespace` holds exactly `expected`, naming what was waited for.
fn wait_held(
    site: &crate::common::people::Site,
    devices: &[&crate::common::people::Device],
    namespace: &Mounted,
    expected: &[&str],
    what: &str,
) {
    let mut expected: Vec<String> = expected.iter().map(|path| (*path).to_owned()).collect();
    expected.sort();
    crate::common::people::waited(what, site, devices, || {
        let held = contents(namespace);
        held == expected
    });
}

/// Every path `namespace`'s tree holds as the record reads it — the projection
/// the Files app renders — sorted, each folder's contents by its folder's path.
fn contents(namespace: &Mounted) -> Vec<String> {
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
    let bytes = namespace.lock_recover().save();
    let document = automerge::AutoCommit::load(&bytes).expect("the tree's document loads");
    let tree = FilesTree::read(&document).expect("the tree reads");
    let mut found = Vec::new();
    walk(&tree, ROOT_ID, "", &mut found);
    found.sort();
    found
}

/// A tree document this device mounts, as the daemon serving the mount holds it.
type Mounted = Arc<Mutex<Namespace>>;

/// A tree document nothing has mounted yet, as a device mounts a workspace it
/// holds no tree of.
fn namespace(author: &str) -> Mounted {
    Arc::new(Mutex::new(Namespace::new(author).expect("a tree document")))
}

/// Publish `namespace` on `cluster` as `paths`'s device, as the files daemon
/// serving the mount does.
fn mount(namespace: &Mounted, cluster: &str, paths: &IdentityPaths) -> tokio::task::JoinHandle<()> {
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
        tilo_paths::xdg::service_socket("identity"),
    )
}

/// Publish `namespace` on `cluster` as `device`'s mount does: the daemon of
/// that device, on the collaboration socket its session serves.
fn mount_on(
    device: &crate::common::people::Device,
    namespace: &Mounted,
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
        device
            .directory()
            .join("runtime")
            .join("tilo/services/identity.sock"),
    )
}

/// Run `session` until this device's tree holds exactly `expected`, failing
/// with the tree it holds instead.
fn wait_for_tree(session: &mut Session, namespace: &Mounted, expected: &[&str], what: &str) {
    let mut expected: Vec<String> = expected.iter().map(|path| (*path).to_owned()).collect();
    expected.sort();
    let held = crate::common::wait_for(session, what, |session| {
        session.settle_services();
        session.step(Duration::from_millis(5)).expect("step");
        let held = contents(namespace);
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
    crate::common::wait_for(session, "the session to answer the join", |session| {
        session.settle_services();
        session.step(Duration::from_millis(5)).expect("step");
        told.try_recv().ok()
    })
}
