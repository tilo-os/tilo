//! Launcher smoke tests.
//!
//! These tests invoke the *library* surface of `tilo-launcher` against
//! ad-hoc fixtures. Bubblewrap is part of the product security boundary, so
//! absence is a broken build environment rather than a reason to skip.
//! Landlock enforcement is covered by its policy tests; these smoke cases
//! isolate process launch and service-resolution semantics so they also run
//! on builder kernels older than Landlock ABI v5.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use launcher::capabilities::CapabilityPlan;
use launcher::manifest::{Entrypoint, Fs, MinimalManifest, Net, Sandbox, read_manifest};
use launcher::run::{RunError, RunOptions, run, run_with, run_with_captured};
use launcher::services::EXIT_REQUIRED_SERVICE_MISSING;

/// The runtime-bundle lookup tests all mutate `HOME` / `XDG_DATA_HOME`
/// to a per-test tempdir; since env vars are process-global, run those
/// tests serially or concurrent tempdir paths bleed across each other.
static RUNTIME_ENV_MUTEX: Mutex<()> = Mutex::new(());

// helpers

/// rustc-compile the in-tree `hello` fixture into `out_dir/bin/hello`
/// rather than `cargo build`, since the fixture lives outside the
/// workspace and this wants a single-shot build that doesn't dirty the
/// workspace target dir.
fn build_hello(out_dir: &Path) -> PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hello/src/main.rs");
    let bin = out_dir.join("bin").join("hello");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    let status = std::process::Command::new("rustc")
        .arg("--edition=2024")
        .arg("-O")
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .status()
        .expect("invoke rustc");
    assert!(status.success(), "rustc failed to build hello fixture");
    bin
}

// `with_glibc_binds` is gone: those loader paths are in the launcher's
// system read profile now, so every app gets them without declaring.

fn hello_manifest(payload_root: &Path, services: BTreeMap<String, String>) -> MinimalManifest {
    let _ = payload_root;
    MinimalManifest {
        package_id: "com.example.hello".to_string(),
        name: None,
        category: None,
        icon: None,
        version: "0.0.0".to_string(),
        kind: "app".to_string(),
        runtime: None,
        entrypoint: Entrypoint {
            bin: "bin/hello".to_string(),
            r#type: launcher::manifest::EntrypointType::Gui,
            gui_bin: None,
            cli_bin: None,
        },
        sandbox: Sandbox {
            net: Net {
                mode: "off".to_string(),
                ..Net::default()
            },
            fs: Fs::default(),
            services,
            portals: BTreeMap::new(),
            ..Sandbox::default()
        },
        cli_commands: Vec::new(),
        opens_ext: Vec::new(),
        opens_mime: Vec::new(),
        opens_schemes: Vec::new(),
        continuity: launcher::manifest::ContinuityContract::default(),
    }
}

/// Locate the product sandbox executable. A test unable to observe it fails.
fn bwrap_path() -> PathBuf {
    let p = PathBuf::from("/usr/bin/bwrap");
    assert!(p.exists(), "bwrap is required to verify tapp launching");
    p
}

// tests

#[test]
fn manifest_malformed_fails_before_exec() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("manifest.toml");
    std::fs::write(&path, "this is { not valid toml = ").unwrap();
    let opts = RunOptions::default();
    let err = run(&path, dir.path(), &opts).unwrap_err();
    match err {
        RunError::Manifest(_) => {} // expected: parse error
        other => panic!("expected ManifestError, got {other:?}"),
    }
}

#[test]
#[serial_test::serial(process_env)]
fn required_service_missing_fails() {
    // Manifest says `collab = "required@1"` but no socket exists.
    let dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("XDG_RUNTIME_DIR", dir.path()) };

    let mut svcs = BTreeMap::new();
    svcs.insert("collab".to_string(), "required@1".to_string());
    let m = hello_manifest(dir.path(), svcs);
    let opts = RunOptions {
        no_services: false,
        landlock_enforce: true,
        ..Default::default()
    };
    let err = run_with(&m, dir.path(), &opts).unwrap_err();
    assert_eq!(err.to_exit_code(), EXIT_REQUIRED_SERVICE_MISSING);
    let msg = format!("{err}");
    assert!(msg.contains("collab"), "error must name service: {msg}");
}

#[test]
fn no_services_flag_skips_required_resolution() {
    // Same manifest, but --no-services: required failure becomes a skip +
    // warn. Can't exec without bwrap, so this just asserts the resolution
    // step succeeds; the spawn step is exercised by the live tests.
    let _dir = tempfile::tempdir().unwrap();
    let mut svcs = BTreeMap::new();
    svcs.insert("collab".to_string(), "required@1".to_string());
    let resolved =
        launcher::services::resolve_service_binds(&svcs, true).expect("no-services infallible");
    assert!(resolved.is_empty());
}

#[test]
fn hello_world_runs() {
    let bwrap = bwrap_path();
    let payload = tempfile::tempdir().unwrap();
    let _bin = build_hello(payload.path());

    let m = hello_manifest(payload.path(), BTreeMap::new());
    let opts = RunOptions {
        no_services: false,
        runtime_root: None,
        bwrap_bin: Some(bwrap),
        sandbox_shim_bin: Some(PathBuf::from(env!("CARGO_BIN_EXE_tilo-launcher"))),
        minimal_libs: false,
        landlock_enforce: false,
        mode: launcher::run::LaunchMode::Gui,
        ..Default::default()
    };
    let (status, stdout) = run_with_captured(&m, payload.path(), &opts).expect("launch");
    assert!(status.success(), "child exited non-zero: {status:?}");
    let stdout = String::from_utf8_lossy(&stdout);
    assert!(
        stdout.contains("hello"),
        "expected 'hello' in stdout, got {stdout:?}"
    );
}

#[test]
fn no_services_flag_skips_required() {
    let bwrap = bwrap_path();
    let payload = tempfile::tempdir().unwrap();
    let _bin = build_hello(payload.path());
    let mut svcs = BTreeMap::new();
    svcs.insert("collab".to_string(), "required@1".to_string());
    let m = hello_manifest(payload.path(), svcs);
    let opts = RunOptions {
        no_services: true,
        runtime_root: None,
        bwrap_bin: Some(bwrap),
        minimal_libs: false,
        landlock_enforce: false,
        mode: launcher::run::LaunchMode::Gui,
        ..Default::default()
    };
    let status = run_with(&m, payload.path(), &opts).expect("launch");
    assert!(status.success());
}

#[test]
#[serial_test::serial(process_env)]
fn optional_service_missing_warns_and_runs() {
    let bwrap = bwrap_path();
    let dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("XDG_RUNTIME_DIR", dir.path()) };

    let payload = tempfile::tempdir().unwrap();
    let _bin = build_hello(payload.path());

    let mut svcs = BTreeMap::new();
    svcs.insert("collab".to_string(), "optional@1".to_string());
    let m = hello_manifest(payload.path(), svcs);
    let opts = RunOptions {
        no_services: false,
        runtime_root: None,
        bwrap_bin: Some(bwrap),
        minimal_libs: false,
        landlock_enforce: false,
        mode: launcher::run::LaunchMode::Gui,
        ..Default::default()
    };
    let status = run_with(&m, payload.path(), &opts).expect("launch");
    assert!(status.success(), "optional missing should still run");
}

// Superseded by `tests/room_confinement.rs`, which starts the real
// thing under bwrap and has it try the reads/writes — the assertion is
// on a syscall's answer, not an argv or an exit code that was never at
// risk.

#[test]
#[serial_test::serial(process_env)]
fn manifest_declared_runtime_missing_bundle_fails() {
    let _guard = RUNTIME_ENV_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Manifest points at a runtime whose bundle doesn't exist anywhere on
    // the search path: expect a RuntimeBundleMissing error naming the
    // paths looked at.
    let dir = tempfile::tempdir().unwrap();
    let user_home = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("XDG_DATA_HOME", user_home.path());
        std::env::set_var("HOME", user_home.path());
        std::env::set_var("XDG_RUNTIME_DIR", dir.path());
    }
    let mut m = hello_manifest(dir.path(), BTreeMap::new());
    m.runtime = Some("tilo-runtime/nope-9.9.9".to_string());
    let opts = RunOptions {
        no_services: true,
        landlock_enforce: false,
        ..Default::default()
    };
    let err = run_with(&m, dir.path(), &opts).unwrap_err();
    match &err {
        RunError::RuntimeBundleMissing { runtime, paths } => {
            assert!(runtime.contains("nope-9.9.9"));
            assert!(!paths.is_empty(), "error should list checked paths");
        }
        other => panic!("expected RuntimeBundleMissing, got {other:?}"),
    }
    let msg = format!("{err}");
    assert!(
        msg.contains("nope-9.9.9"),
        "error text must name runtime: {msg}"
    );
}

/// Build a valid unsigned runtime bundle under `<xdg_data_home>/tilo-runtime/<version>/`
/// from a fake host rootfs and return its resolved path.
fn install_unsigned_bundle(xdg_data_home: &Path, version: &str) -> PathBuf {
    let host = tempfile::tempdir().unwrap();
    for rel in ["lib64/ld-linux-x86-64.so.2", "lib64/libc.so.6"] {
        let p = host.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, format!("stub {rel}")).unwrap();
    }
    let dest = xdg_data_home.join("tilo-runtime").join(version);
    runtime_bundle::build_bundle_with(
        &dest,
        version,
        &runtime_bundle::BuildOptions {
            from: Some(host.path()),
            arch: Some("x86_64"),
            identity: None,
            extra_libs: &[],
        },
    )
    .expect("build unsigned bundle");
    dest
}

/// Same but signed with the returned identity.
fn install_signed_bundle(
    xdg_data_home: &Path,
    version: &str,
) -> (PathBuf, runtime_bundle::Identity) {
    let host = tempfile::tempdir().unwrap();
    for rel in ["lib64/ld-linux-x86-64.so.2", "lib64/libc.so.6"] {
        let p = host.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, format!("stub {rel}")).unwrap();
    }
    let identity = runtime_bundle::Identity::generate();
    let id_path = host.path().join("id.json");
    std::fs::write(&id_path, identity.to_json_bytes()).unwrap();

    let dest = xdg_data_home.join("tilo-runtime").join(version);
    runtime_bundle::build_bundle_with(
        &dest,
        version,
        &runtime_bundle::BuildOptions {
            from: Some(host.path()),
            arch: Some("x86_64"),
            identity: Some(&id_path),
            extra_libs: &[],
        },
    )
    .expect("build signed bundle");
    (dest, identity)
}

/// When an app manifest declares a runtime bundle, the launcher's argv
/// should: bind the bundle at /run/tilo-runtime/<v>/, set
/// `LD_LIBRARY_PATH` to <mount>/lib, and NOT include the --minimal-libs
/// loader binds even if the flag is on.
#[test]
#[serial_test::serial(process_env)]
fn runtime_bundle_wins_over_minimal_libs_in_argv() {
    use launcher::manifest::{Entrypoint, Fs, Net, Sandbox};
    use launcher::sandbox::build_bwrap_argv;

    let _guard = RUNTIME_ENV_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Set up a real signed bundle so resolve_runtime + its integrity
    // check are happy.
    let user_home = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("XDG_DATA_HOME", user_home.path());
        std::env::set_var("HOME", user_home.path());
    }
    let (bundle_dir, _identity) = install_signed_bundle(user_home.path(), "smoke-1.0.0");

    let mut m = MinimalManifest {
        package_id: "com.example.hello".to_string(),
        name: None,
        category: None,
        icon: None,
        version: "0.0.0".to_string(),
        kind: "app".to_string(),
        runtime: Some("tilo-runtime/smoke-1.0.0".to_string()),
        entrypoint: Entrypoint {
            bin: "bin/hello".to_string(),
            r#type: launcher::manifest::EntrypointType::Gui,
            gui_bin: None,
            cli_bin: None,
        },
        sandbox: Sandbox {
            net: Net {
                mode: "off".to_string(),
                ..Net::default()
            },
            fs: Fs::default(),
            services: BTreeMap::new(),
            portals: BTreeMap::new(),
            ..Sandbox::default()
        },
        cli_commands: Vec::new(),
        opens_ext: Vec::new(),
        opens_mime: Vec::new(),
        opens_schemes: Vec::new(),
        continuity: launcher::manifest::ContinuityContract::default(),
    };
    // Assert the sandbox argv contains the runtime bind + LD_LIBRARY_PATH
    // for a manually-supplied runtime tuple (the pure-function surface).
    let argv = build_bwrap_argv(
        &m,
        Path::new("/host/payload"),
        Some((bundle_dir.as_path(), "smoke-1.0.0")),
        &[],
        None,
        None,
        &CapabilityPlan::empty(),
    );
    assert!(
        argv.windows(3).any(|w| w[0] == "--ro-bind"
            && w[1] == bundle_dir.display().to_string()
            && w[2] == "/run/tilo-runtime/smoke-1.0.0"),
        "runtime bind not in argv: {argv:?}"
    );
    assert!(
        argv.windows(3).any(|w| w[0] == "--setenv"
            && w[1] == "LD_LIBRARY_PATH"
            && w[2] == "/run/tilo-runtime/smoke-1.0.0/lib"),
        "LD_LIBRARY_PATH setenv not in argv: {argv:?}"
    );

    // And when both minimal-libs and a resolvable runtime bundle are
    // requested, resolve_runtime picks the bundle — exercised directly
    // since argv isn't readable from `run_with` without spawning bwrap.
    let opts = RunOptions {
        no_services: true,
        landlock_enforce: false,
        minimal_libs: true,
        ..Default::default()
    };
    let resolved = launcher::run::resolve_runtime_test_helper(&m, &opts).expect("resolve ok");
    assert!(resolved.is_some(), "runtime should resolve");
    let (path, version) = resolved.unwrap();
    assert_eq!(version, "smoke-1.0.0");
    assert_eq!(path, bundle_dir);

    // And no-runtime manifest resolves to None (no error).
    m.runtime = None;
    let resolved = launcher::run::resolve_runtime_test_helper(&m, &opts).expect("no runtime = ok");
    assert!(resolved.is_none());
}

/// A {libstd, libruntime} runtime bundle mounts so a thin
/// (`-C prefer-dynamic`) app resolves its shared SDK + std libraries
/// from the bundle: the shared `.so`s live at the TOP of `<bundle>/lib`
/// and the launcher sets `LD_LIBRARY_PATH=<mount>/lib`, so the loader
/// finds them by soname. Signature integrity is enforced along the way.
#[test]
#[serial_test::serial(process_env)]
fn thin_app_bundle_exposes_shared_libs_on_ld_library_path() {
    use launcher::manifest::{Entrypoint, Fs, Net, Sandbox};
    use launcher::sandbox::build_bwrap_argv;

    let _guard = RUNTIME_ENV_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let user_home = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("XDG_DATA_HOME", user_home.path());
        std::env::set_var("HOME", user_home.path());
    }

    // Build a signed bundle folding in stand-ins for the two shared
    // objects a thin app links (Rust std + Tilo runtime dylib), exercising
    // the same `extra_libs` path the real bake uses.
    let version = "thin-smoke-1";
    let host = tempfile::tempdir().unwrap();
    for rel in ["lib64/ld-linux-x86-64.so.2", "lib64/libc.so.6"] {
        let p = host.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, format!("stub {rel}")).unwrap();
    }
    let libstd = host.path().join("libstd-abc123.so");
    let libruntime = host.path().join("libruntime_dylib.so");
    std::fs::write(&libstd, b"stub libstd").unwrap();
    std::fs::write(&libruntime, b"stub libruntime").unwrap();
    let identity = runtime_bundle::Identity::generate();
    let id_path = host.path().join("id.json");
    std::fs::write(&id_path, identity.to_json_bytes()).unwrap();

    let bundle_dir = user_home.path().join("tilo-runtime").join(version);
    runtime_bundle::build_bundle_with(
        &bundle_dir,
        version,
        &runtime_bundle::BuildOptions {
            from: Some(host.path()),
            arch: Some("x86_64"),
            identity: Some(&id_path),
            extra_libs: &[libstd, libruntime],
        },
    )
    .expect("build thin bundle");

    // The shared libs the thin app links live at the top of the bundle's
    // lib/ — exactly the dir the launcher turns into LD_LIBRARY_PATH.
    assert!(bundle_dir.join("lib/libstd-abc123.so").exists());
    assert!(bundle_dir.join("lib/libruntime_dylib.so").exists());

    let m = MinimalManifest {
        package_id: "so.tilo.thin".to_string(),
        name: None,
        category: None,
        icon: None,
        version: "0.1.0".to_string(),
        kind: "app".to_string(),
        runtime: Some(format!("tilo-runtime/{version}")),
        entrypoint: Entrypoint {
            bin: "bin/thin".to_string(),
            r#type: launcher::manifest::EntrypointType::Cli,
            gui_bin: None,
            cli_bin: None,
        },
        sandbox: Sandbox {
            net: Net {
                mode: "off".to_string(),
                ..Net::default()
            },
            fs: Fs::default(),
            services: BTreeMap::new(),
            portals: BTreeMap::new(),
            ..Sandbox::default()
        },
        cli_commands: vec!["thin".to_string()],
        opens_ext: Vec::new(),
        opens_mime: Vec::new(),
        opens_schemes: Vec::new(),
        continuity: launcher::manifest::ContinuityContract::default(),
    };

    // The manifest's `runtime` field resolves to the signed bundle
    // (signature verified inside resolve_runtime).
    let opts = RunOptions {
        no_services: true,
        landlock_enforce: false,
        ..Default::default()
    };
    let (resolved_path, resolved_ver) = launcher::run::resolve_runtime_test_helper(&m, &opts)
        .expect("resolve ok")
        .expect("bundle resolves");
    assert_eq!(resolved_ver, version);
    assert_eq!(resolved_path, bundle_dir);

    // The argv mounts the bundle and points LD_LIBRARY_PATH at the dir
    // holding libstd + libruntime — so the thin app finds them by soname.
    let argv = build_bwrap_argv(
        &m,
        Path::new("/host/payload"),
        Some((resolved_path.as_path(), resolved_ver.as_str())),
        &[],
        None,
        None,
        &CapabilityPlan::empty(),
    );
    let mount = format!("/run/tilo-runtime/{version}");
    assert!(
        argv.windows(3).any(|w| w[0] == "--ro-bind"
            && w[1] == resolved_path.display().to_string()
            && w[2] == mount),
        "runtime bind not in argv: {argv:?}"
    );
    assert!(
        argv.windows(3).any(|w| w[0] == "--setenv"
            && w[1] == "LD_LIBRARY_PATH"
            && w[2] == format!("{mount}/lib")),
        "LD_LIBRARY_PATH not pointed at the shared-lib dir: {argv:?}"
    );
}

#[test]
#[serial_test::serial(process_env)]
fn configured_runtime_root_wins_over_the_installed_fallback() {
    let _guard = RUNTIME_ENV_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let overlay = tempfile::tempdir().unwrap();
    let version = "developer-overlay-smoke";
    let bundle = overlay.path().join(version);
    let host = tempfile::tempdir().unwrap();
    for rel in ["lib64/ld-linux-x86-64.so.2", "lib64/libc.so.6"] {
        let path = host.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, rel).unwrap();
    }
    let identity = runtime_bundle::Identity::generate();
    let identity_path = host.path().join("identity.json");
    std::fs::write(&identity_path, identity.to_json_bytes()).unwrap();
    runtime_bundle::build_bundle_with(
        &bundle,
        version,
        &runtime_bundle::BuildOptions {
            from: Some(host.path()),
            arch: Some("x86_64"),
            identity: Some(&identity_path),
            ..runtime_bundle::BuildOptions::default()
        },
    )
    .unwrap();
    let mut manifest = hello_manifest(Path::new("/payload"), BTreeMap::new());
    manifest.runtime = Some(format!("tilo-runtime/{version}"));
    let old = std::env::var_os(runtime_bundle::RUNTIME_DIR_ENV);
    unsafe { std::env::set_var(runtime_bundle::RUNTIME_DIR_ENV, overlay.path()) };
    let resolved = launcher::run::resolve_runtime_test_helper(&manifest, &RunOptions::default());
    unsafe {
        match old {
            Some(value) => std::env::set_var(runtime_bundle::RUNTIME_DIR_ENV, value),
            None => std::env::remove_var(runtime_bundle::RUNTIME_DIR_ENV),
        }
    }
    assert_eq!(resolved.unwrap().unwrap().0, bundle);
}

/// End-to-end: a manifest with `[sandbox.portals] notifications =
/// "via-host"` and a working `xdg-dbus-proxy` (mocked) results in the
/// launcher spawning the proxy, binding the *proxy* socket (not the
/// raw bus), and reaping the proxy on teardown. argv isn't observable
/// from `run_with` directly, so this drives `resolve_dbus_bind` via the
/// same entry path and asserts on the resulting plan + guard.
#[test]
#[serial_test::serial(process_env)]
fn dbus_proxy_wires_up_when_manifest_declares_via_host_portal() {
    use launcher::dbus_proxy::PROXY_BIN_ENV;
    use launcher::sandbox::build_bwrap_argv;
    use std::os::unix::fs::PermissionsExt;

    // Point the launcher's proxy binary at a shell-script mock.
    let tmp = tempfile::tempdir().unwrap();
    let mock = tmp.path().join("mock-xdg-dbus-proxy");
    std::fs::write(
        &mock,
        r#"#!/bin/sh
: >"$2"
# exec, not a backgrounded child: the guard signals and reaps this pid,
# and a child of a shell it killed would be orphaned, never reaped.
if [ -x /usr/bin/sleep ]; then
    exec /usr/bin/sleep infinity
fi
if [ -x /bin/sleep ]; then
    exec /bin/sleep infinity
fi
exit 1
"#,
    )
    .unwrap();
    let mut perms = std::fs::metadata(&mock).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&mock, perms).unwrap();

    // SAFETY: setting env for the test process. Test intentionally
    // gates ordering by not running in parallel with any other proxy
    // test — nothing else touches this var.
    unsafe {
        std::env::set_var(PROXY_BIN_ENV, &mock);
        std::env::set_var("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1234/bus");
    }

    // Build a manifest declaring `notifications = via-host`.
    let mut m = hello_manifest(tmp.path(), BTreeMap::new());
    m.sandbox.portals.insert(
        "notifications".to_string(),
        launcher::manifest::PortalPolicy::ViaHost,
    );

    // Drive the proxy path via the launcher's public helper. In-test uid
    // is fictitious (1234), so use a tempdir socket instead of the
    // canonical /run/user/1234/... path a real launch would use.
    let sock = tmp.path().join("proxy.sock");
    let bin = launcher::dbus_proxy::find_proxy_binary().expect("PROXY_BIN_ENV was set above");
    let guard = launcher::dbus_proxy::spawn_dbus_proxy(&bin, "unix:path=/run/user/1234/bus", &sock)
        .expect("spawn mock proxy");
    let plan = launcher::sandbox::DBusBindPlan {
        host_socket: guard.socket_path().to_path_buf(),
        sandbox_path: launcher::dbus_proxy::sandbox_bus_path(1234),
    };

    // The proxy child exists.
    let pid = guard.pid().expect("proxy pid");
    assert!(std::path::Path::new(&format!("/proc/{pid}")).exists());

    // The bwrap argv binds the proxy socket at /run/user/1234/bus,
    // NOT the raw session bus address.
    let argv = build_bwrap_argv(
        &m,
        std::path::Path::new("/host/pkg"),
        None,
        &[],
        Some(&plan),
        None,
        &CapabilityPlan::empty(),
    );
    // The host source must be the proxy socket, NOT /run/user/1234/bus itself.
    let proxy_sock = plan.host_socket.display().to_string();
    assert!(
        argv.windows(3)
            .any(|w| w[0] == "--ro-bind" && w[1] == proxy_sock && w[2] == "/run/user/1234/bus"),
        "expected `--ro-bind {proxy_sock} /run/user/1234/bus`, got {argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a == "/run/user/1234/bus"
            && argv.iter().position(|x| x == a) != argv.iter().rposition(|x| x == a)
            && a != &proxy_sock),
        "raw bus path must not appear as a host source in argv"
    );

    // Drop the guard: the proxy is SIGTERMed + reaped.
    drop(guard);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::path::Path::new(&format!("/proc/{pid}")).exists()
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        !std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "proxy pid {pid} should be reaped after Drop"
    );

    unsafe {
        std::env::remove_var(PROXY_BIN_ENV);
        std::env::remove_var("DBUS_SESSION_BUS_ADDRESS");
    }
}

#[test]
fn read_manifest_round_trips() {
    // Smoke: write a manifest, read it back, assert key fields.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("manifest.toml");
    std::fs::write(
        &path,
        r#"
package_id = "com.example.demo"
version = "1.2.3"

[entrypoint]
bin = "bin/demo"

[sandbox.net]
mode = "off"

[sandbox.services]
collab = "required@1"
fs = "optional@2"
"#,
    )
    .unwrap();
    let m = read_manifest(&path).expect("parse");
    assert_eq!(m.package_id, "com.example.demo");
    assert_eq!(m.entrypoint.bin, "bin/demo");
    assert_eq!(m.sandbox.services.get("collab").unwrap(), "required@1");
}

/// A runtime bundle that exists but is unsigned is refused by default;
/// --allow-unsigned-runtime makes the resolve succeed.
#[test]
#[serial_test::serial(process_env)]
fn unsigned_bundle_is_refused_unless_opted_in() {
    let _guard = RUNTIME_ENV_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().unwrap();
    let user_home = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("XDG_DATA_HOME", user_home.path());
        std::env::set_var("HOME", user_home.path());
        std::env::set_var("XDG_RUNTIME_DIR", dir.path());
    }
    let _bundle = install_unsigned_bundle(user_home.path(), "sig-refuse-1");

    let mut m = hello_manifest(dir.path(), BTreeMap::new());
    m.runtime = Some("tilo-runtime/sig-refuse-1".to_string());

    let opts = RunOptions {
        no_services: true,
        landlock_enforce: false,
        ..Default::default()
    };
    let err = launcher::run::resolve_runtime_test_helper(&m, &opts).unwrap_err();
    match &err {
        RunError::RuntimeBundleUnsigned { .. } => {}
        other => panic!("expected RuntimeBundleUnsigned, got {other:?}"),
    }

    let opts_ok = RunOptions {
        no_services: true,
        landlock_enforce: false,
        allow_unsigned_runtime: true,
        ..Default::default()
    };
    let resolved =
        launcher::run::resolve_runtime_test_helper(&m, &opts_ok).expect("opt-in accepts");
    let (_p, v) = resolved.expect("bundle resolves");
    assert_eq!(v, "sig-refuse-1");
}

/// The trusted-publisher file pins which signing keys are acceptable
/// for runtime bundles at mount time: missing means any signed
/// publisher works; a matching key accepts; a mismatched key refuses.
#[test]
#[serial_test::serial(process_env)]
fn trusted_publisher_file_gates_mount() {
    let _guard = RUNTIME_ENV_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().unwrap();
    let user_home = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("XDG_DATA_HOME", user_home.path());
        std::env::set_var("HOME", user_home.path());
        std::env::set_var("XDG_RUNTIME_DIR", dir.path());
    }
    let (_bundle, identity) = install_signed_bundle(user_home.path(), "trust-file-1");

    let mut m = hello_manifest(dir.path(), BTreeMap::new());
    m.runtime = Some("tilo-runtime/trust-file-1".to_string());
    let opts = RunOptions {
        no_services: true,
        landlock_enforce: false,
        ..Default::default()
    };

    // 1. Trust file lists this publisher: accepted.
    let trust_file = user_home.path().join("trust.txt");
    std::fs::write(
        &trust_file,
        format!(
            "# managed by test\n{}\n\n# blank + comments are ignored\n",
            identity.public_key_hex()
        ),
    )
    .unwrap();
    unsafe {
        std::env::set_var("TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE", &trust_file);
    }
    launcher::run::resolve_runtime_test_helper(&m, &opts).expect("trusted pubkey ok");

    // 2. Trust file lists a different publisher only: refused.
    let other = runtime_bundle::Identity::generate();
    std::fs::write(&trust_file, format!("{}\n", other.public_key_hex())).unwrap();
    let err = launcher::run::resolve_runtime_test_helper(&m, &opts).unwrap_err();
    match &err {
        RunError::RuntimeBundleSignature { .. } => {}
        RunError::RuntimeBundleUnsigned { .. } => panic!("bundle IS signed"),
        other => panic!("expected RuntimeBundleSignature, got {other:?}"),
    }

    // 3. Trust file lists BOTH the actual publisher and a random extra —
    //    the multi-entry branch of enforce_bundle_policy.
    std::fs::write(
        &trust_file,
        format!(
            "{}\n{}\n",
            identity.public_key_hex(),
            other.public_key_hex()
        ),
    )
    .unwrap();
    launcher::run::resolve_runtime_test_helper(&m, &opts).expect("multi-entry list accepts");

    // 4. No trust file: any signed publisher works (verifies the env
    //    override above, since the default /etc file isn't readable here).
    unsafe {
        std::env::remove_var("TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE");
    }
    launcher::run::resolve_runtime_test_helper(&m, &opts).expect("no trust file = any signed");
}

/// A signed bundle is accepted by default. Pinning the publisher to
/// its actual key still accepts; pinning to a different key refuses.
#[test]
#[serial_test::serial(process_env)]
fn signed_bundle_publisher_pin_enforced() {
    let _guard = RUNTIME_ENV_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().unwrap();
    let user_home = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("XDG_DATA_HOME", user_home.path());
        std::env::set_var("HOME", user_home.path());
        std::env::set_var("XDG_RUNTIME_DIR", dir.path());
    }
    let (_bundle, identity) = install_signed_bundle(user_home.path(), "sig-pin-1");

    let mut m = hello_manifest(dir.path(), BTreeMap::new());
    m.runtime = Some("tilo-runtime/sig-pin-1".to_string());

    // Default: accepted.
    let opts = RunOptions {
        no_services: true,
        landlock_enforce: false,
        ..Default::default()
    };
    launcher::run::resolve_runtime_test_helper(&m, &opts).expect("accept signed");

    // Pin to the bundle's publisher: accepted.
    let opts_pinned = RunOptions {
        no_services: true,
        landlock_enforce: false,
        require_runtime_publisher: Some(identity.public_key_hex()),
        ..Default::default()
    };
    launcher::run::resolve_runtime_test_helper(&m, &opts_pinned).expect("pin matches");

    // Pin to a different publisher: refused.
    let other = runtime_bundle::Identity::generate();
    let opts_wrong = RunOptions {
        no_services: true,
        landlock_enforce: false,
        require_runtime_publisher: Some(other.public_key_hex()),
        ..Default::default()
    };
    let err = launcher::run::resolve_runtime_test_helper(&m, &opts_wrong).unwrap_err();
    match &err {
        RunError::RuntimeBundleSignature { .. } => {}
        other => panic!("expected RuntimeBundleSignature, got {other:?}"),
    }
}

// workspace capabilities

/// A capability file on disk becomes a bind in the argv, through the
/// exact pair `run_with` uses: `resolve_capabilities` reads it,
/// `build_bwrap_argv` renders the result.
#[test]
fn a_capability_file_on_disk_becomes_a_bind_in_the_argv() {
    use collab_types::caps::{CapOverride, Scope, Subject, Verb};

    let dir = tempfile::tempdir().unwrap();
    let caps_file = dir.path().join("caps.json");
    let caps = vec![
        CapOverride::grant(
            Subject::app("com.example.hello"),
            Scope::FsPath("/var/workspace/books".to_string()),
            Verb::Read,
        ),
        // Addressed to a different app: must not survive the crossing,
        // even though it sits in the same file.
        CapOverride::grant(
            Subject::app("com.example.other"),
            Scope::FsPath("/var/workspace/secrets".to_string()),
            Verb::Write,
        ),
    ];
    std::fs::write(&caps_file, serde_json::to_vec(&caps).unwrap()).unwrap();

    let m = hello_manifest(dir.path(), BTreeMap::new());
    let opts = RunOptions {
        no_services: true,
        landlock_enforce: false,
        capabilities_path: Some(caps_file),
        ..Default::default()
    };
    let plan = launcher::run::resolve_capabilities(&m, &opts);
    let argv = launcher::sandbox::build_bwrap_argv(
        &m,
        Path::new("/host/pkg"),
        None,
        &[],
        None,
        None,
        &plan,
    );

    assert!(
        argv.windows(3).any(|w| w[0] == "--ro-bind-try"
            && w[1] == "/var/workspace/books"
            && w[2] == "/var/workspace/books"),
        "the granted path must be bound read-only: {argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a == "/var/workspace/secrets"),
        "another app's grant in the same file must not be bound: {argv:?}"
    );
}

/// Deny by default at the file boundary: an unparseable capability file
/// grants nothing, same as no file at all — neither is a launch
/// failure, both are just the manifest's sandbox.
#[test]
fn an_unparseable_or_absent_capability_file_grants_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let m = hello_manifest(dir.path(), BTreeMap::new());

    let malformed = dir.path().join("caps.json");
    std::fs::write(&malformed, b"not json at all").unwrap();
    let opts = RunOptions {
        no_services: true,
        capabilities_path: Some(malformed),
        ..Default::default()
    };
    assert_eq!(
        launcher::run::resolve_capabilities(&m, &opts),
        CapabilityPlan::empty(),
        "a malformed capability file must not fall open"
    );

    let opts_missing = RunOptions {
        no_services: true,
        capabilities_path: Some(dir.path().join("does-not-exist.json")),
        ..Default::default()
    };
    assert_eq!(
        launcher::run::resolve_capabilities(&m, &opts_missing),
        CapabilityPlan::empty(),
        "a missing capability file must not fall open"
    );

    let opts_none = RunOptions {
        no_services: true,
        ..Default::default()
    };
    assert_eq!(
        launcher::run::resolve_capabilities(&m, &opts_none),
        CapabilityPlan::empty()
    );
}
