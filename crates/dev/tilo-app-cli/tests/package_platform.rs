#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]
//! `tilo-app package --platform linux|macos|windows` builds one static executable
//! for the platform's triple and packs it under a signed desktop `[target]`;
//! `build`, `run` and `dev` take the same platforms and refuse what the host
//! cannot run.

use std::path::Path;
use std::process::Command;

/// Stands in for Cargo: resolves the app with no dependencies, records its
/// build arguments and reports one fake executable for the requested `--bin`
/// and `--target`; with `FAKE_CARGO_FAIL` set, fails after recording them.
const FAKE_CARGO: &str = r#"#!/bin/sh
if [ "$1" = metadata ]; then
  printf '{"packages":[],"resolve":{"root":"notes","nodes":[{"id":"notes","deps":[]}]}}\n'
  exit 0
fi
printf '%s\n' "$@" > "$FAKE_CARGO_ARGS"
[ -z "${FAKE_CARGO_FAIL:-}" ] || exit 101
while [ $# -gt 0 ]; do
  case "$1" in
    --manifest-path) manifest="$(cd "$(dirname "$2")" && pwd -P)/Cargo.toml"; shift ;;
    --bin) bin="$2"; shift ;;
    --target) target="$2"; shift ;;
  esac
  shift
done
case "$target" in *windows*) suffix=.exe ;; *) suffix= ;; esac
out="$(dirname "$manifest")/target/$target/release/$bin$suffix"
mkdir -p "$(dirname "$out")"
printf 'fake executable' > "$out"
chmod +x "$out"
printf '{"reason":"compiler-artifact","manifest_path":"%s","target":{"name":"%s","kind":["bin"]},"executable":"%s"}\n' "$manifest" "$bin" "$out"
"#;

fn tilo_app(dir: &Path, cargo: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tilo-app"))
        .args(args)
        .current_dir(dir)
        .env_remove("TILO_UI_PATH")
        .env("CARGO", cargo)
        .env("FAKE_CARGO_ARGS", dir.join("cargo-args"))
        .output()
        .expect("run tilo-app")
}

#[test]
fn desktop_packages_ship_one_executable_under_a_signed_target() {
    let dir = tempfile::tempdir().unwrap();
    let cargo = dir.path().join("fake-cargo");
    std::fs::write(&cargo, FAKE_CARGO).unwrap();
    let mut permissions = std::fs::metadata(&cargo).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&cargo, permissions).unwrap();
    let created = tilo_app(
        dir.path(),
        &cargo,
        &["new", "notes", "--template", "starter"],
    );
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let app = dir.path().join("notes");

    for (platform, arch, triple, module) in [
        ("macos", "aarch64", "aarch64-apple-darwin", "bin/notes"),
        ("linux", "x86_64", "x86_64-unknown-linux-gnu", "bin/notes"),
        (
            "windows",
            "x86_64",
            "x86_64-pc-windows-msvc",
            "bin/notes.exe",
        ),
    ] {
        let output = tilo_app(
            &app,
            &cargo,
            &["package", "--platform", platform, "--arch", arch, "--json"],
        );
        assert!(
            output.status.success(),
            "{platform}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let args = std::fs::read_to_string(app.join("cargo-args")).unwrap();
        let args: Vec<&str> = args.lines().collect();
        let after = |flag: &str| {
            args.iter()
                .position(|arg| *arg == flag)
                .map(|index| args[index + 1])
        };
        assert_eq!(after("--target"), Some(triple), "{args:?}");
        assert_eq!(after("--profile"), Some("release"), "{args:?}");
        assert_eq!(after("--bin"), Some("notes"), "{args:?}");
        assert!(
            !args.iter().any(|arg| arg.contains("thin")),
            "a desktop build links the toolkit in: {args:?}"
        );

        let tapp = app.join(format!("dist/notes-{platform}-{arch}.tapp"));
        let reader = std::io::BufReader::new(std::fs::File::open(&tapp).unwrap());
        let read =
            tilo_app_package::tapp::read(reader, &mut tilo_app_package::DiscardBlobs).unwrap();
        let manifest = &read.meta.manifest;
        let target = manifest.target.as_ref().expect("a desktop [target]");
        assert_eq!(target.platform, platform);
        assert_eq!(target.architecture, arch);
        assert_eq!(target.module, module);
        assert_eq!(
            Some(target.runtime.as_str()),
            manifest.package.runtime.as_deref()
        );
        assert_eq!(
            read.meta.collection.entries.keys().collect::<Vec<_>>(),
            [module]
        );
    }

    let output = tilo_app(
        &app,
        &cargo,
        &["package", "--static", "--arch", "aarch64", "--json"],
    );
    assert!(
        output.status.success(),
        "tilo: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reader = std::io::BufReader::new(std::fs::File::open(app.join("dist/notes.tapp")).unwrap());
    let read = tilo_app_package::tapp::read(reader, &mut tilo_app_package::DiscardBlobs).unwrap();
    let manifest = &read.meta.manifest;
    assert!(manifest.target.is_none());
    assert_eq!(
        manifest.package.isa,
        tilo_app_package::manifest::IsaRequirements {
            x86_64: None,
            aarch64: Some(tilo_app_package::manifest::AArch64Level::Armv8A),
        },
        "a Tilo OS package declares the one architecture it carries"
    );
}

/// A Tilo OS package links the OS toolkit, whose media stack needs
/// the development files of `GStreamer` where Cargo compiles. This machine lacking
/// them does not stop the build, which `cargo` may run elsewhere; when the
/// build fails, the error names the packages to install here.
#[test]
fn this_machines_libraries_are_named_only_when_the_build_fails() {
    let dir = tempfile::tempdir().unwrap();
    let cargo = dir.path().join("fake-cargo");
    std::fs::write(&cargo, FAKE_CARGO).unwrap();
    let mut permissions = std::fs::metadata(&cargo).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&cargo, permissions).unwrap();
    let created = tilo_app(
        dir.path(),
        &cargo,
        &["new", "notes", "--template", "starter"],
    );
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let app = dir.path().join("notes");
    let package = |fail: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tilo-app"));
        command
            .args(["package", "--force", "--json"])
            .current_dir(&app)
            .env_remove("TILO_UI_PATH")
            .env("CARGO", &cargo)
            .env("FAKE_CARGO_ARGS", app.join("cargo-args"))
            .env("PKG_CONFIG", dir.path().join("no-pkg-config"));
        if fail {
            command.env("FAKE_CARGO_FAIL", "1");
        }
        command.output().expect("run tilo-app")
    };

    let built = package(false);
    let stderr = String::from_utf8_lossy(&built.stderr);
    let args = std::fs::read_to_string(app.join("cargo-args")).unwrap();
    assert!(
        args.lines().any(|arg| arg.contains("thin")),
        "Cargo was not asked for a Tilo OS build: {args}\n{stderr}"
    );
    assert!(built.status.success(), "{stderr}");
    assert!(!stderr.contains("GStreamer"), "{stderr}");

    std::fs::remove_file(app.join("cargo-args")).unwrap();
    let failed = package(true);
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(!failed.status.success());
    assert!(app.join("cargo-args").exists(), "Cargo never ran: {stderr}");
    assert!(stderr.contains("links GStreamer"), "{stderr}");
    assert!(
        stderr.contains("sudo dnf install pkgconf-pkg-config gstreamer1-devel"),
        "{stderr}"
    );
}

#[test]
fn build_run_and_dev_take_the_package_platforms() {
    let dir = tempfile::tempdir().unwrap();
    let cargo = dir.path().join("fake-cargo");
    std::fs::write(&cargo, FAKE_CARGO).unwrap();
    let mut permissions = std::fs::metadata(&cargo).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    std::fs::set_permissions(&cargo, permissions).unwrap();
    let created = tilo_app(
        dir.path(),
        &cargo,
        &["new", "notes", "--template", "starter"],
    );
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let app = dir.path().join("notes");

    let output = tilo_app(
        &app,
        &cargo,
        &[
            "build",
            "--platform",
            "linux",
            "--arch",
            "aarch64",
            "--output",
            "out",
            "--json",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let args = std::fs::read_to_string(app.join("cargo-args")).unwrap();
    let args: Vec<&str> = args.lines().collect();
    let after = |flag: &str| {
        args.iter()
            .position(|arg| *arg == flag)
            .map(|index| args[index + 1])
    };
    assert_eq!(
        after("--target"),
        Some("aarch64-unknown-linux-gnu"),
        "{args:?}"
    );
    assert_eq!(after("--profile"), Some("release"), "{args:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["result"]["platform"], "linux");
    assert_eq!(report["result"]["target"], "aarch64-unknown-linux-gnu");
    assert_eq!(report["result"]["executables"][0]["bin"], "notes");
    assert_eq!(
        std::fs::read(app.join("out/notes")).unwrap(),
        b"fake executable"
    );

    let other_os = if cfg!(target_os = "macos") {
        "windows"
    } else {
        "macos"
    };
    let other_arch = if cfg!(target_arch = "aarch64") {
        "x86_64"
    } else {
        "aarch64"
    };
    for (command, platform, arch, expected) in [
        ("run", other_os, None, "does not run on"),
        ("dev", other_os, None, "does not run on"),
        ("run", "linux", Some(other_arch), "cannot run"),
        ("dev", "tilo", Some(other_arch), "cannot run"),
        (
            "run",
            "tilo",
            Some("wasm32"),
            "--arch wasm32 is for --platform web",
        ),
        (
            "build",
            "web",
            Some("x86_64"),
            "--platform web builds for wasm32",
        ),
    ] {
        let mut args = vec![command, "--platform", platform];
        if let Some(arch) = arch {
            args.extend(["--arch", arch]);
        }
        let output = tilo_app(&app, &cargo, &args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{args:?} should refuse");
        assert!(stderr.contains(expected), "{args:?}: {stderr}");
    }
}
