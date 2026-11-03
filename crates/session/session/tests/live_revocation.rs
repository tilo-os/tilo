//! The running desk's file access list, as the Files daemon enforces it: the
//! session publishes it as it starts and again when it changes, so taking a
//! folder away reaches the app at its next open instead of waiting for the
//! next login.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion"
)]

mod common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::Duration;

/// How many times the stand-in Files daemon was asked to publish.
fn publications(seen: &Path) -> u64 {
    std::fs::read_to_string(seen.join("count"))
        .ok()
        .and_then(|count| count.trim().parse().ok())
        .unwrap_or(0)
}

#[test]
fn the_session_publishes_its_file_access_list_to_the_files_daemon() {
    let root = common::private_home();
    let seen = root.path().join("seen");
    std::fs::create_dir_all(&seen).unwrap();
    // A stand-in for `fs publish-authority`: it records each publication and
    // keeps the list it was handed, which is exactly what the daemon signs.
    let script = root.path().join("fs");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             set -e\n\
             test \"$1\" = publish-authority\n\
             test \"$2\" = --acl\n\
             n=$(cat {seen}/count 2>/dev/null || echo 0)\n\
             n=$((n+1))\n\
             echo \"$n\" > {seen}/count\n\
             cp \"$3\" {seen}/$n.json\n",
            seen = seen.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(root.path().join("runtime")).unwrap();
    // SAFETY: this binary runs the one test below, so nothing else reads the
    // environment while it changes.
    unsafe {
        std::env::set_var("HOME", root.path());
        std::env::set_var("XDG_CONFIG_HOME", root.path().join("config"));
        std::env::set_var("XDG_RUNTIME_DIR", root.path().join("runtime"));
        std::env::set_var("TILO_IDENTITY_ROOT", root.path().join("identity"));
        std::env::set_var("TILO_AUTHORITY_SOCKET_DIR", root.path().join("sockets"));
        std::env::set_var("TILO_FS_AUTHORITY_PUBLISHER", &script);
    }
    let acl = root.path().join("config/tilo/fs-capabilities.json");
    let mut session = common::seeded_session(root.path());
    common::until(&mut session, "the file access list", |_| {
        acl.is_file() && publications(&seen) == 1
    });
    let published: collab_types::FsAuthority =
        serde_json::from_slice(&std::fs::read(&acl).unwrap()).unwrap();
    assert!(
        !published.workspace_members.is_empty(),
        "every workspace says who may read it: {published:?}"
    );
    // A desk change that grants nothing new is not published again: the
    // daemon signs a list, not every frame.
    common::press(&mut session, "Frames");
    for _ in 0..5 {
        session.step(Duration::from_millis(1)).unwrap();
    }
    assert_eq!(
        publications(&seen),
        1,
        "an unchanged list is not republished"
    );
    let kept: collab_types::FsAuthority =
        serde_json::from_slice(&std::fs::read(&acl).unwrap()).unwrap();
    assert_eq!(kept.capabilities, published.capabilities);
}
