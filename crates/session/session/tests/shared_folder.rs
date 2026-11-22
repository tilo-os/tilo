//! A folder is shared as the workspace it lives in: the invitation the shell makes admits the
//! person it names at the part it grants, and what they hold and write is the workspace's own
//! tree. Removing them removes both what the workspace writes afterwards and what they write.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              lives until the mounts it serves are done"
)]

use crate::common::files::{Mounted, contents, mount_on};
use fs::namespace::ROOT_INO;
use tilo_lock::MutexRecover as _;

/// The whole story: a folder shared through the shell, the other person's edit reaching the
/// holder, and the workspace's removal taking both away again.
#[test]
fn a_folder_shared_through_the_shell_is_edited_then_taken_back() {
    let (site, _ada, bob, ada_laptop) = crate::common::people::a_site_with_files_for_one_laptop();
    let cluster = site.workspace.clone();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime for the mounts");
    let _in_runtime = runtime.enter();

    let ada_mount = mount_on(&ada_laptop, &cluster);
    let here = ada_mount.namespace();
    here.lock_recover()
        .create_dir(ROOT_INO, "Notes", 1_000)
        .expect("a folder");
    let held = wait_until(&site, &[&ada_laptop], &here, &["Notes"]);
    assert_eq!(
        held,
        vec!["Notes".to_owned()],
        "Ada's mount holds her folder"
    );

    let invitation = crate::common::people::shared_invitation(&ada_laptop, "editor", bob.public);
    let mut bob_laptop = bob.device("Bob's laptop");
    bob_laptop.install_files();
    bob_laptop.join(&invitation);
    bob_laptop.wait_holding(&site.workspace);
    site.wait_for_member(bob.public);

    let bob_mount = mount_on(&bob_laptop, &cluster);
    let there = bob_mount.namespace();
    let held = wait_until(&site, &[&ada_laptop, &bob_laptop], &there, &["Notes"]);
    assert_eq!(
        held,
        vec!["Notes".to_owned()],
        "Bob holds the shared folder"
    );

    // An editor's write reaches the person who shared the folder.
    there
        .lock_recover()
        .create_file(ROOT_INO, "from-bob.md", None, 0, 5_000)
        .expect("a file");
    let held = wait_until(
        &site,
        &[&ada_laptop, &bob_laptop],
        &here,
        &["Notes", "from-bob.md"],
    );
    assert_eq!(
        held,
        vec!["Notes".to_owned(), "from-bob.md".to_owned()],
        "Ada holds the edit Bob made"
    );

    // Removed from the workspace, Bob reads nothing more of it and writes nothing into it.
    site.remove(bob.public);
    site.wait_for_removal(bob.public);
    here.lock_recover()
        .create_file(ROOT_INO, "after-removal.md", None, 0, 9_000)
        .expect("a file");
    let held = wait_until(
        &site,
        &[&ada_laptop],
        &here,
        &["Notes", "from-bob.md", "after-removal.md"],
    );
    assert_eq!(
        held,
        vec![
            "Notes".to_owned(),
            "after-removal.md".to_owned(),
            "from-bob.md".to_owned()
        ],
        "Ada's mount keeps the workspace's files after the removal"
    );
    there
        .lock_recover()
        .create_file(ROOT_INO, "from-removed.md", None, 0, 10_000)
        .expect("a file");
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < until {
        let (his_mount, ada_mount) = (contents(&there), contents(&here));
        assert!(
            !his_mount.iter().any(|path| path == "after-removal.md")
                && !ada_mount.iter().any(|path| path == "from-removed.md"),
            "the removed member reads nothing the workspace writes after his removal, and writes \
             nothing into it: Bob's mount holds {his_mount:?}, Ada's {ada_mount:?}\n{}",
            bob_laptop.log(),
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn wait_until(
    site: &crate::common::people::Site,
    devices: &[&crate::common::people::Device],
    namespace: &Mounted,
    expected: &[&str],
) -> Vec<String> {
    let mut expected: Vec<String> = expected.iter().map(|path| (*path).to_owned()).collect();
    expected.sort();
    crate::common::people::waited("the tree", site, devices, || {
        contents(namespace) == expected
    });
    contents(namespace)
}
