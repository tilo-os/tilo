//! Disk-free actual signed OSTree/GPG capsule qualification; missing tools fail.
use super::*;
use std::collections::BTreeMap;
use std::process::Command;

fn command(program: &str, args: &[&str]) -> Vec<u8> {
    let output = Command::new("/usr/bin/timeout")
        .args(["--signal=KILL", "30s", program])
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "native fixture tool {program} {args:?} failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

type SourceSnapshot = BTreeMap<PathBuf, (Vec<u8>, u32, u32, u32)>;

fn source_snapshot(root: &Path) -> SourceSnapshot {
    fn collect(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, (Vec<u8>, u32, u32, u32)>) {
        for entry in std::fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            let info = std::fs::symlink_metadata(&path).unwrap();
            assert!(
                !info.file_type().is_symlink(),
                "fixture inventory requires regular source"
            );
            let bytes = if info.is_dir() {
                Vec::new()
            } else {
                std::fs::read(&path).unwrap()
            };
            out.insert(
                path.strip_prefix(root).unwrap().into(),
                (bytes, info.uid(), info.gid(), info.mode()),
            );
            if info.is_dir() {
                collect(root, &path, out);
            }
        }
    }
    let mut found = BTreeMap::new();
    collect(root, root, &mut found);
    found
}

fn key(root: &Path, name: &str) -> (PathBuf, String) {
    let home = root.join(name);
    std::fs::create_dir(&home).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let at = home.to_str().unwrap();
    command(
        "/usr/bin/gpg",
        &[
            "--homedir",
            at,
            "--batch",
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--quick-generate-key",
            "Tilo Synthetic Qualification",
            "ed25519",
            "sign",
            "0",
        ],
    );
    let listed = command(
        "/usr/bin/gpg",
        &["--homedir", at, "--batch", "--with-colons", "--list-keys"],
    );
    let text = String::from_utf8(listed).unwrap();
    let fingerprints: Vec<_> = text
        .lines()
        .filter(|line| line.starts_with("fpr:"))
        .map(|line| line.split(':').nth(9).unwrap().to_owned())
        .collect();
    assert_eq!(fingerprints.len(), 1);
    (
        home,
        fingerprints
            .first()
            .expect("one generated fingerprint")
            .clone(),
    )
}

fn populate_source(root: &Path, source: &Path) {
    let (approved, signer) = key(root, "synthetic-approved-key");
    let (wrong, other) = key(root, "synthetic-wrong-key");
    assert!(signer != other, "synthetic signing keys must be distinct");
    for (name, fingerprint) in [("approved-key-id", &signer), ("wrong-key-id", &other)] {
        assert_eq!(fingerprint.len(), 40);
        assert!(fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()));
        std::fs::write(
            source.join(name),
            fingerprint
                .get(24..)
                .expect("fingerprint has a key-id suffix"),
        )
        .unwrap();
    }
    let tree = root.join("commit-tree");
    std::fs::create_dir(&tree).unwrap();
    std::fs::write(tree.join("document"), b"signed fixture document").unwrap();
    let repo = source.join("repo");
    let repo_arg = format!("--repo={}", repo.display());
    command("/usr/bin/ostree", &[&repo_arg, "init", "--mode=archive"]);
    let tree_arg = format!("--tree=dir={}", tree.display());
    let sign_arg = format!("--gpg-sign={signer}");
    let home_arg = format!("--gpg-homedir={}", approved.display());
    let commit = command(
        "/usr/bin/ostree",
        &[
            &repo_arg,
            "commit",
            "--branch=fixture",
            "--subject=synthetic",
            &tree_arg,
            &sign_arg,
            &home_arg,
        ],
    );
    assert_eq!(String::from_utf8_lossy(&commit).trim().len(), 64);
    std::fs::write(source.join("commit"), commit).unwrap();
    for (name, home, fingerprint) in [
        ("approved.asc", &approved, &signer),
        ("wrong.asc", &wrong, &other),
    ] {
        let public = command(
            "/usr/bin/gpg",
            &[
                "--homedir",
                home.to_str().unwrap(),
                "--batch",
                "--armor",
                "--export",
                fingerprint,
            ],
        );
        assert!(!public.is_empty());
        std::fs::write(source.join(name), public).unwrap();
    }
    // Synthetic private keys never cross the capsule boundary. Reap their agents.
    for home in [&approved, &wrong] {
        command(
            "/usr/bin/gpgconf",
            &["--homedir", home.to_str().unwrap(), "--kill", "all"],
        );
    }
}

fn runtime(root: &Path) -> (PathBuf, String) {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let packaging = repo.join("apps/imager/packaging");
    let probe = root.join("tilo-capture-openpgp-probe");
    let flags = command("/usr/bin/pkg-config", &["--cflags", "--libs", "gpgme"]);
    let flags = String::from_utf8(flags).unwrap();
    let source = packaging.join("capture-openpgp-engine.c");
    let mut compile = vec![
        "-Wall",
        "-Wextra",
        "-Werror",
        source.to_str().unwrap(),
        "-o",
        probe.to_str().unwrap(),
    ];
    compile.extend(flags.split_whitespace());
    command("/usr/bin/cc", &compile);
    let table = root.join("tilo-capture-openpgp.json");
    let sources = root.join("openpgp-sources");
    command(
        "/usr/bin/python3",
        &[
            packaging.join("capture_openpgp.py").to_str().unwrap(),
            probe.to_str().unwrap(),
            table.to_str().unwrap(),
            sources.to_str().unwrap(),
        ],
    );
    let launcher = root.join("tilo-launcher");
    std::fs::copy(crate::support::launcher_binary(), &launcher).unwrap();
    command(
        "/usr/bin/strip",
        &["--strip-debug", launcher.to_str().unwrap()],
    );
    let mut binaries = vec![
        launcher,
        PathBuf::from("/usr/bin/bwrap"),
        PathBuf::from("/usr/bin/tar"),
    ];
    binaries.extend(
        std::fs::read_to_string(sources)
            .unwrap()
            .lines()
            .map(PathBuf::from),
    );
    let mut libraries = vec![table];
    for binary in &binaries {
        let output = command("/usr/bin/ldd", &[binary.to_str().unwrap()]);
        let text = String::from_utf8(output).unwrap();
        assert!(!text.contains("not found"));
        for line in text.lines() {
            if let Some(path) = line.split_whitespace().find(|item| item.starts_with('/')) {
                libraries.push(path.into());
            }
        }
    }
    libraries.sort();
    libraries.dedup();
    let identity = runtime_bundle::Identity::generate();
    let key = root.join("synthetic-runtime-key.json");
    std::fs::write(&key, identity.to_json_bytes()).unwrap();
    let runtime = root.join("signed-openpgp-runtime");
    runtime_bundle::build_bundle_with(
        &runtime,
        "2026.10.1",
        &runtime_bundle::BuildOptions {
            identity: Some(&key),
            extra_bins: &binaries,
            extra_libs: &libraries,
            ..Default::default()
        },
    )
    .unwrap();
    (runtime, identity.public_key_hex())
}

#[test]
fn signed_original_openpgp_capsule_accepts_approved_commit_and_refuses_wrong_key() {
    const CHILD: &str = "TILO_OPENPGP_CAPSULE_CHILD";
    tilo_test_env::require_capability!(
        Chown,
        tilo_test_env::switch_identity(65534),
        "the OpenPGP capsule runs as root in a mount namespace of its own"
    );
    if std::env::var_os(CHILD).is_none() {
        let result = Command::new("/usr/bin/unshare").args(["--mount", "--propagation", "private"]).arg(std::env::current_exe().unwrap()).args(["--exact", "capture_service::openpgp_native::signed_original_openpgp_capsule_accepts_approved_commit_and_refuses_wrong_key", "--nocapture"]).env(CHILD, "1").status().unwrap();
        assert!(
            result.success(),
            "native OpenPGP capsule qualification unavailable or failed"
        );
        return;
    }
    assert_eq!(
        rustix::process::geteuid().as_raw(),
        0,
        "isolated root qualifier required"
    );
    // Compile/cache finite fixture before taking the environment lock.
    let payload = super::super::support::capture_worker_payload();
    let temporary = tempfile::Builder::new()
        .prefix("tilo-openpgp-capsule-")
        .tempdir_in("/run")
        .unwrap();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("TMPDIR", temporary.path());
    let mut home = tilo_test_env::TestHome::new().unwrap();
    home.env().set(launcher::run::DEBUG_STDERR_ENV, "1");
    let root = home.root();
    let (source, mounted) = readonly_source(root);
    let before = source_snapshot(&source);
    let (runtime, publisher) = runtime(root);
    let installed = installed_worker(root, payload);
    let admission = CaptureAdmission::verify_original_source(
        &runtime,
        &publisher,
        File::open(&source).unwrap(),
        1000,
        1001,
        &[1002],
        [7; 32],
    )
    .unwrap()
    .with_archive_helper()
    .unwrap()
    .with_original_ostree_helper()
    .unwrap()
    .with_installation(installed.clone(), &installed.publisher_key_hex)
    .unwrap();
    let data = root.join("openpgp-worker");
    std::fs::create_dir(&data).unwrap();
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut launch = original_reader_launch(&installed, &data, &root.join("source-secret"));
    let LaunchMode::Service { passthrough_argv } = &mut launch.options.mode else {
        panic!("canonical service launch required")
    };
    passthrough_argv[0] = "--original-openpgp".into();
    let key = ServiceLeaseKey::default();
    let result = returning_callback_retains_original_custody(
        launch,
        admission,
        File::open(&source).unwrap(),
        key.clone(),
        false,
    )
    .unwrap();
    assert!(result.matches(&key, &data, PACKAGE));
    assert_eq!(source_snapshot(&source), before);
    mounted.release();
}

fn readonly_source(root: &Path) -> (PathBuf, Mounted) {
    let source = root.join("source");
    std::fs::create_dir(&source).unwrap();
    populate_source(root, &source);
    // Existing fixture supplies group-read + owner-write assertions and readonly mount.
    std::fs::rename(&source, root.join("repository-source")).unwrap();
    let (source, mount_path) = original_reader_source(root);
    assert_eq!(unsafe { libc::umount(mount_path.as_ptr()) }, 0);
    for entry in std::fs::read_dir(root.join("repository-source")).unwrap() {
        let entry = entry.unwrap();
        std::fs::rename(entry.path(), source.join(entry.file_name())).unwrap();
    }
    assert_eq!(
        unsafe {
            libc::mount(
                mount_path.as_ptr(),
                mount_path.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null(),
            )
        },
        0
    );
    assert_eq!(
        unsafe {
            libc::mount(
                std::ptr::null(),
                mount_path.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
                std::ptr::null(),
            )
        },
        0
    );
    (source, Mounted(mount_path))
}

struct Mounted(std::ffi::CString);
impl Mounted {
    fn release(self) {
        assert_eq!(
            unsafe { libc::umount(self.0.as_ptr()) },
            0,
            "owned fixture unmount failed"
        );
        std::mem::forget(self);
    }
}
impl Drop for Mounted {
    fn drop(&mut self) {
        // Panic cleanup remains confined to this child-only private mount namespace.
        let _ = unsafe { libc::umount2(self.0.as_ptr(), libc::MNT_DETACH) };
    }
}
