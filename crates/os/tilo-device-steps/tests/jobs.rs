//! The `build` step end to end with a stand-in `cargo`: the synced tree's
//! own manifest installer lays the component out, the layer lands in the
//! store without travelling back to the host, and the build dir is
//! recorded so `gc` can evict it.

use std::path::Path;

use tilo_device_protocol::{
    BaseInfo, Build, BuildTarget, Call, Gc, JobOutput, LayerList, Owner, Provenance, RpcError,
    Status, code,
};
use tilo_device_steps::apply::RecordingApply;

const FAKE_CARGO: &str = r#"#!/bin/sh
case "$1" in
  metadata) echo '{"packages":[{"name":"tilo-session","features":{"dev-overrides":[]}}]}' ;;
  build)
    echo "   Compiling tilo-session v0.1.0 ($*)" >&2
    mkdir -p target/release
    # Every `--bin` the component asks for, as a real build would leave them.
    while [ "$#" -gt 0 ]; do
      if [ "$1" = --bin ]; then
        shift
        printf '#!/bin/sh\necho built\n' > "target/release/$1"
        chmod 755 "target/release/$1"
      fi
      shift
    done ;;
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
    // The manifest, its installer, the checked-in inputs the session
    // component copies from beside the manifest (`@manifest/...`), and the
    // font catalog a source session swap stages its verified fonts from.
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

fn job(target: BuildTarget) -> Build {
    let mut provenance = Provenance::local("tilo-session");
    provenance.source_commit = Some("abc".into());
    Build {
        worktree: "wt-1".into(),
        target,
        provenance,
        owner: Owner::default(),
        color: false,
    }
}

/// Run step `C` against the state under `root` with `path` as `PATH`, answering its outcome and
/// its progress.
fn step<C: Call>(root: &Path, path: &str, args: &C) -> (Result<C::Output, RpcError>, String) {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("PATH", path);
    let mut daemon = tilo_device_steps::Daemon::new(
        &root.join("state"),
        Box::new(RecordingApply::default()),
        None,
        BaseInfo::default(),
    );
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("args.json"),
        serde_json::to_vec(args).unwrap(),
    )
    .unwrap();
    tilo_device_steps::run_step_on(&mut daemon, C::ACTION, dir.path());
    drop(env);
    let mut result: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&std::fs::read(dir.path().join("result.json")).unwrap()).unwrap();
    let outcome = match (result.remove("ok"), result.remove("err")) {
        (Some(ok), None) => Ok(serde_json::from_value(ok).unwrap()),
        (None, Some(err)) => Err(serde_json::from_value(err).unwrap()),
        other => panic!("a result is ok or err: {other:?}"),
    };
    let progress = std::fs::read_to_string(dir.path().join("progress")).unwrap_or_default();
    (outcome, progress)
}

fn run(root: &Path, path: &str, job: &Build) -> (Result<JobOutput, RpcError>, String) {
    step(root, path, job)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn in_vm_builds_import_layers_and_report_errors_with_hints() {
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
        let error = line.unwrap_err();
        assert_eq!(error.code, code::NOT_FOUND);
        assert!(error.hint.contains("tilo-session"), "{}", error.hint);

        let (line, _) = run(
            root.path(),
            &path,
            &job(BuildTarget::App {
                dir: "missing-app".into(),
                account: "a".into(),
            }),
        );
        assert!(line.unwrap_err().message.contains("apps/missing-app"));

        let mut unsynced = job(BuildTarget::Component {
            name: "tilo-session".into(),
            profile: None,
        });
        unsynced.worktree = "never-synced".into();
        let (line, _) = run(root.path(), &path, &unsynced);
        let error = line.unwrap_err();
        assert!(error.hint.contains("--builder vm"), "{}", error.hint);
    }
    {
        let (root, path) = setup();
        let session = || {
            job(BuildTarget::Component {
                name: "tilo-session".into(),
                profile: None,
            })
        };
        let (line, stderr) = run(root.path(), &path, &session());
        let line = line.unwrap_or_else(|error| panic!("{error:?}\n{stderr}"));
        assert!(
            stderr.contains("Compiling tilo-session"),
            "build output streams: {stderr}"
        );
        assert!(
            stderr.contains("tilo-session/dev-overrides"),
            "dev overrides are enabled: {stderr}"
        );
        let JobOutput::Layer {
            hash,
            existed,
            provenance,
            phases,
            ..
        } = line
        else {
            panic!("expected a layer");
        };
        assert!(!existed);
        assert_eq!(provenance.builder.as_deref(), Some("vm"));
        assert_eq!(provenance.source_commit.as_deref(), Some("abc"));
        assert_eq!(provenance.component, "tilo-session");
        assert!(phases.contains_key("build") && phases.contains_key("import"));
        let layer = root.path().join("state/layers").join(&hash);
        assert!(layer.join("usr/bin/tilo-session").is_file());
        assert_eq!(
            std::fs::read(layer.join("usr/libexec/tilo-session")).unwrap(),
            std::fs::read(repo().join("os/usr/libexec/tilo-session")).unwrap(),
            "the imported session layer carries this source checkout's startup wrapper"
        );

        let record: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.path().join("state/src/wt-1.json")).unwrap(),
        )
        .unwrap();
        assert!(record["bytes"].as_u64().unwrap() > 0);

        let (again, _) = run(root.path(), &path, &session());
        assert!(matches!(again, Ok(JobOutput::Layer { existed: true, .. })));

        // gc sees the build dir and evicts it when told to keep none.
        assert_eq!(step(root.path(), &path, &LayerList {}).0.unwrap().len(), 1);
        let status = step(root.path(), &path, &Status { account: None })
            .0
            .unwrap();
        assert!(status.storage.targets_bytes > 0);
        let gc = step(
            root.path(),
            &path,
            &Gc {
                keep: 5,
                dry_run: false,
                keep_targets: Some(0),
                free_goal_bytes: None,
                min_age_seconds: None,
            },
        )
        .0
        .unwrap();
        assert_eq!(gc.removed_targets, ["wt-1"]);
        assert!(gc.freed_target_bytes > 0);
        assert!(!root.path().join("state/src/wt-1/target").exists());
        assert!(root.path().join("state/src/wt-1/Cargo.toml").exists());
    }
}
