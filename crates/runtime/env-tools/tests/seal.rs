//! The seal on a hand-made upper: no sandbox needed.

use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::Path;

use env_tools::seal;

fn write(root: &Path, rel: &str, body: &str, mode: u32) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn the_seal_keeps_package_files_and_drops_package_manager_churn() {
    let dir = tempfile::tempdir().unwrap();
    let (upper, lower, out) = (
        dir.path().join("upper"),
        dir.path().join("lower"),
        dir.path().join("out"),
    );
    write(&lower, "etc/shells", "/bin/sh\n", 0o644);
    write(&lower, "usr/lib/sysimage/rpm/rpmdb.sqlite", "db", 0o644);

    write(&upper, "usr/bin/jq", "#!jq", 0o4755);
    write(&upper, "usr/lib64/libonig.so.5.4.0", "lib", 0o600);
    std::os::unix::fs::symlink("libonig.so.5.4.0", upper.join("usr/lib64/libonig.so.5")).unwrap();
    write(&upper, "usr/share/doc/jq/README", "", 0o644);
    write(&upper, "etc/shells", "/bin/sh\n/usr/bin/jq\n", 0o644);
    write(&upper, "etc/jq.conf.rpmnew", "x", 0o644);
    write(&upper, "etc/resolv.conf", "", 0o644);
    write(&upper, "etc/ld.so.cache", "c", 0o644);
    write(&upper, "usr/lib/sysimage/rpm/rpmdb.sqlite", "db2", 0o644);
    write(&upper, "usr/lib/sysimage/libdnf5/history", "h", 0o644);
    std::fs::create_dir_all(upper.join("usr/share/empty/deeper")).unwrap();
    // Copied-up directories carry the runtime's 0555.
    std::fs::set_permissions(
        upper.join("usr/lib64"),
        std::fs::Permissions::from_mode(0o555),
    )
    .unwrap();

    let report = seal::seal(&upper, &lower, &out).unwrap();

    assert_eq!(report.files, 4, "{report:?}");
    assert_eq!(report.symlinks, 1);
    assert_eq!(
        report.dropped,
        [
            "etc/jq.conf.rpmnew",
            "etc/ld.so.cache",
            "etc/resolv.conf",
            "usr/lib/sysimage/libdnf5",
            "usr/lib/sysimage/rpm",
        ]
    );
    assert_eq!(report.shadows, ["etc/shells"]);
    assert!(report.whiteouts.is_empty());

    let mode = |rel: &str| std::fs::symlink_metadata(out.join(rel)).unwrap().mode() & 0o7777;
    assert_eq!(mode("usr/bin/jq"), 0o755, "setuid is stripped, exec kept");
    assert_eq!(mode("usr/lib64/libonig.so.5.4.0"), 0o644);
    assert_eq!(mode("usr/lib64"), 0o755);
    assert_eq!(
        std::fs::read_link(out.join("usr/lib64/libonig.so.5")).unwrap(),
        Path::new("libonig.so.5.4.0")
    );
    assert!(
        !out.join("usr/share/empty").exists(),
        "empty dirs are pruned"
    );
    assert!(!out.join("usr/lib/sysimage").exists());
    assert!(!out.join("usr/lib").exists());
    for rel in [
        "usr",
        "usr/bin",
        "usr/bin/jq",
        "usr/lib64/libonig.so.5",
        "etc/shells",
    ] {
        assert_eq!(
            std::fs::symlink_metadata(out.join(rel)).unwrap().mtime(),
            seal::EPOCH,
            "{rel}"
        );
    }
}

#[test]
fn the_seal_refuses_an_existing_destination() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("upper/usr")).unwrap();
    std::fs::create_dir_all(dir.path().join("out")).unwrap();
    assert!(
        seal::seal(
            &dir.path().join("upper"),
            dir.path(),
            &dir.path().join("out")
        )
        .is_err()
    );
}
