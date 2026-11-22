//! A workspace's files are its Files resources: each device's mount syncs its tree through
//! the device's workspace service, which the device's session serves the tree of, and what one
//! mount changes reaches the other devices of the workspace. A device that joins later is given
//! the tree whole, and a member the workspace removes reads and writes nothing more of it.
//!
//! The mounts here are the tests' own, each reaching one device's session as that device's
//! files daemon does, and the tree is read back through `fs_protocol::tree`, the projection the
//! Files app renders.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              lives until the mounts it serves are done"
)]

use fs::namespace::ROOT_INO;
use tilo_lock::MutexRecover as _;

use crate::common::files::{Mounted, contents, mount_on};

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
    let ada_mount = mount_on(&ada_laptop, &cluster);
    let here = ada_mount.namespace();
    here.lock_recover()
        .create_dir(ROOT_INO, "Notes", 1_000)
        .expect("a folder");
    here.lock_recover()
        .create_file(ROOT_INO, "readme.txt", None, 5, 1_000)
        .expect("a file");

    // Bob's mount holds nothing yet: it is given the tree Ada's mount holds.
    let bob_mount = mount_on(&bob_laptop, &cluster);
    let there = bob_mount.namespace();
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
    let cyd_mount = mount_on(&cyd_laptop, &cluster);
    let later = cyd_mount.namespace();
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
            !warnings.contains("a document op its channel's check does not take"),
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
