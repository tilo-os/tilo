//! `tilo-vmd build` end to end with a stand-in `cargo`: the synced tree's
//! own manifest installer lays the component out, the layer lands in the
//! store without travelling back to the host, and the build dir is
//! recorded so `gc` can evict it.
#![allow(clippy::unwrap_used, clippy::panic, reason = "test scaffolding")]

use std::io::{BufReader, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};

use tilo_vm_protocol::{
    BuildJob, BuildTarget, Client, Gc, JobLine, JobOutput, LayerList, Owner, Provenance, Status,
    code,
};

const FAKE_CARGO: &str = r#"#!/bin/sh
case "$1" in
  metadata) echo '{"packages":[{"name":"shell","features":{"dev-overrides":[]}}]}' ;;
  build)
    echo "   Compiling shell v0.1.0 ($*)" >&2
    mkdir -p target/release
    printf '#!/bin/sh\necho built\n' > target/release/shell
    chmod 755 target/release/shell ;;
esac
"#;

fn repo() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.."))
}

fn setup() -> (tempfile::TempDir, String) {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("cargo"), FAKE_CARGO).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(bin.join("cargo"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    let src = root.path().join("state/src/wt-1");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("Cargo.toml"), "[workspace]\n").unwrap();
    // The manifest, its installer, the checked-in inputs the shell
    // component copies from beside the manifest (`@manifest/...`), and the
    // font catalog a source shell swap stages its verified fonts from.
    for file in [
        "os/components.toml",
        "os/scripts/install-components.py",
        "os/usr/libexec/tilo-session",
        "os/usr/share/tilo/system-interfaces.toml",
        "tools/font-catalog.py",
    ] {
        let to = src.join(file);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(repo().join(file), to).unwrap();
    }
    copy_tree(
        &repo().join("crates/sdk/ui-core/assets/fonts"),
        &src.join("crates/sdk/ui-core/assets/fonts"),
    );
    // No sccache from the developer's PATH: the stand-in cargo ignores it.
    let path = format!("{}:/usr/bin:/bin", bin.display());
    (root, path)
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn job(target: BuildTarget) -> BuildJob {
    let mut provenance = Provenance::local("shell");
    provenance.source_commit = Some("abc".into());
    BuildJob {
        worktree: "wt-1".into(),
        target,
        provenance,
        owner: Owner::default(),
        color: false,
    }
}

fn run(root: &Path, path: &str, job: &BuildJob) -> (JobLine, String) {
    let mut child = Command::new(tilo_test_env::package_binary!("tilo-vmd"))
        .args([
            "build",
            "--apply",
            "record",
            "--base",
            "/nonexistent",
            "--root",
        ])
        .arg(root.join("state"))
        .env("PATH", path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(&serde_json::to_vec(job).unwrap()).unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let line = JobLine::find(&stdout).unwrap_or_else(|| panic!("no job line in {stdout:?}"));
    assert_eq!(
        stdout.lines().count(),
        1,
        "stdout carries only the job line: {stdout:?}"
    );
    (line, String::from_utf8(output.stderr).unwrap())
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn in_vm_builds_import_layers_and_report_errors_with_hints() {
    // Phase `build_errors_come_back_with_hints`.
    {
        let (root, path) = setup();
        let (line, _) = run(
            root.path(),
            &path,
            &job(BuildTarget::Component {
                name: "nope".into(),
                profile: None,
            }),
        );
        let error = line.error.unwrap();
        assert_eq!(error.code, code::NOT_FOUND);
        assert!(error.hint.contains("shell"), "{}", error.hint);

        let (line, _) = run(
            root.path(),
            &path,
            &job(BuildTarget::App {
                dir: "missing-app".into(),
                slot: "a".into(),
            }),
        );
        assert!(line.error.unwrap().message.contains("apps/missing-app"));

        let mut unsynced = job(BuildTarget::Component {
            name: "shell".into(),
            profile: None,
        });
        unsynced.worktree = "never-synced".into();
        let (line, _) = run(root.path(), &path, &unsynced);
        let error = line.error.unwrap();
        assert!(error.hint.contains("--builder vm"), "{}", error.hint);
    }
    // Phase `in_vm_build_imports_a_layer_and_records_its_build_dir`.
    {
        let (root, path) = setup();
        let shell = || {
            job(BuildTarget::Component {
                name: "shell".into(),
                profile: None,
            })
        };
        let (line, stderr) = run(root.path(), &path, &shell());
        assert!(line.error.is_none(), "{:?}\n{stderr}", line.error);
        assert!(
            stderr.contains("Compiling shell"),
            "build output streams: {stderr}"
        );
        assert!(
            stderr.contains("shell/dev-overrides"),
            "dev overrides are enabled: {stderr}"
        );
        let Some(JobOutput::Layer {
            hash,
            existed,
            provenance,
            phases,
            ..
        }) = line.result
        else {
            panic!("expected a layer");
        };
        assert!(!existed);
        assert_eq!(provenance.builder.as_deref(), Some("vm"));
        assert_eq!(provenance.source_commit.as_deref(), Some("abc"));
        assert_eq!(provenance.component, "shell");
        assert!(phases.contains_key("build") && phases.contains_key("import"));
        let layer = root.path().join("state/layers").join(&hash);
        assert!(layer.join("usr/bin/shell").is_file());
        assert_eq!(
            std::fs::read(layer.join("usr/libexec/tilo-session")).unwrap(),
            std::fs::read(repo().join("os/usr/libexec/tilo-session")).unwrap(),
            "the imported shell layer carries this source checkout's startup wrapper"
        );

        let record: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.path().join("state/src/wt-1.json")).unwrap(),
        )
        .unwrap();
        assert!(record["bytes"].as_u64().unwrap() > 0);

        let (again, _) = run(root.path(), &path, &shell());
        assert!(matches!(
            again.result,
            Some(JobOutput::Layer { existed: true, .. })
        ));

        // gc sees the build dir and evicts it when told to keep none.
        let mut rpc = Command::new(tilo_test_env::package_binary!("tilo-vmd"))
            .args(["rpc", "--apply", "record", "--root"])
            .arg(root.path().join("state"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut client = Client::new(
            BufReader::new(rpc.stdout.take().unwrap()),
            rpc.stdin.take().unwrap(),
        );
        client.hello("test").unwrap();
        assert_eq!(client.call(LayerList {}).unwrap().len(), 1);
        let status = client.call(Status { slot: None }).unwrap();
        assert!(status.storage.targets_bytes > 0);
        let gc = client
            .call(Gc {
                keep: 5,
                dry_run: false,
                keep_targets: Some(0),
                free_goal_bytes: None,
                min_age_seconds: None,
            })
            .unwrap();
        assert_eq!(gc.removed_targets, ["wt-1"]);
        assert!(gc.freed_target_bytes > 0);
        assert!(!root.path().join("state/src/wt-1/target").exists());
        assert!(root.path().join("state/src/wt-1/Cargo.toml").exists());
        drop(client);
        let _ = rpc.wait();
    }
}
