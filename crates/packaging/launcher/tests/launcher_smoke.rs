//! Launcher smoke tests.
//!
//! These tests invoke the *library* surface of `tilo-launcher` against ad-hoc fixtures.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use launcher::capabilities::CapabilityPlan;
use launcher::manifest::{ManifestView, ServiceReq, read_manifest};
use launcher::run::{RunError, RunOptions, run, run_with, run_with_captured};
use launcher::services::EXIT_REQUIRED_SERVICE_MISSING;
use tilo_test_env::{ScopedEnv, TestHome};

use crate::support::{bwrap, hello_payload, isolated_options, manifest};

// helpers

/// Point `$XDG_RUNTIME_DIR`, where session services are looked up, at `dir`.
fn runtime_dir_env(dir: &Path) -> ScopedEnv {
    let mut env = ScopedEnv::new();
    env.set("XDG_RUNTIME_DIR", dir);
    env
}

// `with_glibc_binds` is gone: those loader paths are in the launcher's system read profile now, so
// every app gets them without declaring.

fn service_req(raw: &str) -> ServiceReq {
    ServiceReq::parse("", raw).expect("valid service requirement")
}

fn hello_manifest(services: BTreeMap<String, ServiceReq>) -> ManifestView {
    let mut manifest = manifest("com.example.hello", "bin/hello", "off");
    manifest.sandbox.services = services;
    manifest
}

// tests

#[test]
fn manifest_errors_fail_before_exec_and_manifests_round_trip() {
    {
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
    {
        // Manifest points at a runtime whose bundle doesn't exist anywhere on the search path: expect
        // a RuntimeBundleMissing error naming the paths looked at.
        let home = runtime_home();
        let mut m = hello_manifest(BTreeMap::new());
        m.runtime = Some("tilo-runtime/nope-9.9.9".to_string());
        let opts = offline_options();
        let err = run_with(&m, home.root(), &opts).unwrap_err();
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
    {
        // Smoke: write a manifest, read it back, assert key fields.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest.toml");
        std::fs::write(
            &path,
            r#"
    [package]
    id = "com.example.demo"
    version = "1.2.3"
    kind = "app"

    [publisher]
    identity = "ed25519:0000000000000000000000000000000000000000000000000000000000000000"

    [entrypoint]
    bin = "bin/demo"

    [sandbox]
    namespaces = []
    seccomp = "tilo-default"
    landlock = "strict"

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
        assert_eq!(m.version, "1.2.3");
        assert_eq!(m.entrypoint.bin, "bin/demo");
        let collab = &m.sandbox.services["collab"];
        assert!(collab.is_required() && collab.version == 1);
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn apps_launch_unless_a_required_service_is_missing() {
    {
        // Manifest says `collab = "required@1"` but no socket exists.
        let dir = tempfile::tempdir().unwrap();
        let _env = runtime_dir_env(dir.path());

        let mut svcs = BTreeMap::new();
        svcs.insert("collab".to_string(), service_req("required@1"));
        let m = hello_manifest(svcs);
        let opts = RunOptions {
            no_services: false,
            landlock_enforce: true,
            ..isolated_options(dir.path())
        };
        let err = run_with(&m, dir.path(), &opts).unwrap_err();
        assert_eq!(err.to_exit_code(), EXIT_REQUIRED_SERVICE_MISSING);
        let msg = format!("{err}");
        assert!(msg.contains("collab"), "error must name service: {msg}");
    }
    {
        // Same manifest, but --no-services: required failure becomes a skip + warn.
        let _dir = tempfile::tempdir().unwrap();
        let mut svcs = BTreeMap::new();
        svcs.insert("collab".to_string(), service_req("required@1"));
        let resolved =
            launcher::services::resolve_service_binds(&svcs, true).expect("no-services infallible");
        assert!(resolved.is_empty());
    }
    {
        let bwrap = bwrap();
        let payload = hello_payload();
        let data = tempfile::tempdir().unwrap();
        let mut svcs = BTreeMap::new();
        svcs.insert("collab".to_string(), service_req("required@1"));
        let m = hello_manifest(svcs);
        let opts = gui_options(data.path(), bwrap, true, false);
        let status = run_with(&m, payload, &opts).expect("launch");
        assert!(status.success());
    }
    {
        let bwrap = bwrap();
        let dir = tempfile::tempdir().unwrap();
        let _env = runtime_dir_env(dir.path());

        let payload = hello_payload();

        let mut svcs = BTreeMap::new();
        svcs.insert("collab".to_string(), service_req("optional@1"));
        let m = hello_manifest(svcs);
        let opts = gui_options(dir.path(), bwrap, false, false);
        let status = run_with(&m, payload, &opts).expect("launch");
        assert!(status.success(), "optional missing should still run");
    }
    {
        let bwrap = bwrap();
        let payload = hello_payload();
        let data = tempfile::tempdir().unwrap();

        let m = hello_manifest(BTreeMap::new());
        let opts = gui_options(data.path(), bwrap, false, true);
        let (status, stdout) = run_with_captured(&m, payload, &opts).expect("launch");
        assert!(status.success(), "child exited non-zero: {status:?}");
        let stdout = String::from_utf8_lossy(&stdout);
        assert!(
            stdout.contains("hello"),
            "expected 'hello' in stdout, got {stdout:?}"
        );
    }
    {
        let bwrap = bwrap();
        let identity = tilo_app_package::SigningIdentity::generate();
        let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
            r#"
    [package]
    id = "com.example.hello"
    version = "1.0.0"
    kind = "app"

    [publisher]
    identity = "ed25519:{}"
    signature = ""

    [sandbox]
    namespaces = ["user"]
    seccomp = "tilo-default"
    landlock = "strict"

    [sandbox.net]
    mode = "off"

    [entrypoint]
    bin = "bin/hello"
    type = "gui"
    "#,
            hex::encode(identity.public_key_bytes())
        ))
        .unwrap();
        let packed = tilo_app_package::pack(hello_payload(), &manifest, &identity).unwrap();
        let tapp = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();

        let store_root = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let store = package_store::PackageStore::at(store_root.path());
        store
            .install(
                tapp.as_slice(),
                |_| Ok(package_store::Admission::default()),
                None,
            )
            .expect("install into the package store");
        let row = package_store::Catalog::at(store_root.path(), None)
            .find("com.example.hello")
            .unwrap()
            .expect("the catalogue lists the installed package");
        assert_eq!(row.payload_root, store.layout().payload(packed.id()));

        let m = read_manifest(&row.manifest_path).expect("the launcher reads the signed manifest");
        let data = tempfile::tempdir().unwrap();
        let opts = gui_options(data.path(), bwrap, false, true);
        let (status, stdout) = run_with_captured(&m, &row.payload_root, &opts).expect("launch");
        assert!(status.success(), "child exited non-zero: {status:?}");
        let stdout = String::from_utf8_lossy(&stdout);
        assert!(
            stdout.contains("hello"),
            "expected 'hello' in stdout, got {stdout:?}"
        );
    }
}

// Superseded by `tests/workspace_confinement.rs`, which starts the real thing under bwrap and has it
// try the reads/writes — the assertion is on a syscall's answer, not an argv or an exit code that was
// never at risk.

/// Options that resolve without services and without enforcing the sandbox.
fn offline_options() -> RunOptions {
    RunOptions {
        no_services: true,
        landlock_enforce: false,
        ..Default::default()
    }
}

/// Options for a GUI launch confined with `bwrap` in `dir`, with the sandbox not enforced and `shim`
/// naming the launcher's own shim.
fn gui_options(
    dir: &std::path::Path,
    bwrap: std::path::PathBuf,
    no_services: bool,
    shim: bool,
) -> RunOptions {
    RunOptions {
        no_services,
        runtime_root: None,
        bwrap_bin: Some(bwrap),
        sandbox_shim_bin: shim.then(|| tilo_test_env::package_binary!("tilo-launcher")),
        minimal_libs: false,
        landlock_enforce: false,
        mode: launcher::run::LaunchMode::Gui,
        ..isolated_options(dir)
    }
}

/// A fresh home for the runtime-bundle lookup, which is resolved from the environment:
/// `$XDG_DATA_HOME/tilo-runtime` unless [`runtime_bundle::RUNTIME_DIR_ENV`] names another root, and a
/// trust list named by `TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE`.
fn runtime_home() -> TestHome {
    let mut home = TestHome::new().unwrap();
    home.env()
        .remove(runtime_bundle::RUNTIME_DIR_ENV)
        .remove("TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE");
    home
}

/// Build a valid unsigned runtime bundle under `<xdg_data_home>/tilo-runtime/<version>/` from a fake
/// host rootfs and return its resolved path.
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
            extra_bins: &[],
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
            extra_bins: &[],
        },
    )
    .expect("build signed bundle");
    (dest, identity)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn runtime_bundles_and_the_dbus_proxy_shape_the_launch_argv() {
    {
        use launcher::sandbox::build_bwrap_argv;

        // Set up a real signed bundle so resolve_runtime + its integrity check are happy.
        let home = runtime_home();
        let (bundle_dir, _identity) = install_signed_bundle(home.data(), "smoke-1.0.0");

        let mut m = manifest("com.example.hello", "bin/hello", "off");
        m.runtime = Some("tilo-runtime/smoke-1.0.0".to_string());
        // Assert the sandbox argv contains the runtime bind + LD_LIBRARY_PATH for a manually-supplied
        // runtime tuple (the pure-function surface).
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

        // And when both minimal-libs and a resolvable runtime bundle are requested, resolve_runtime
        // picks the bundle — exercised directly since argv isn't readable from `run_with` without
        // spawning bwrap.
        let opts = RunOptions {
            minimal_libs: true,
            ..offline_options()
        };
        let resolved = launcher::run::resolve_runtime_test_helper(&m, &opts).expect("resolve ok");
        assert!(resolved.is_some(), "runtime should resolve");
        let (path, version) = resolved.unwrap();
        assert_eq!(version, "smoke-1.0.0");
        assert_eq!(path, bundle_dir);

        // And no-runtime manifest resolves to None (no error).
        m.runtime = None;
        let resolved =
            launcher::run::resolve_runtime_test_helper(&m, &opts).expect("no runtime = ok");
        assert!(resolved.is_none());
    }
    {
        use launcher::sandbox::build_bwrap_argv;

        let home = runtime_home();

        // Build a signed bundle folding in stand-ins for the two shared objects a thin app links (Rust
        // std + Tilo runtime dylib), exercising the same `extra_libs` path the real bake uses.
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

        let bundle_dir = home.data().join("tilo-runtime").join(version);
        runtime_bundle::build_bundle_with(
            &bundle_dir,
            version,
            &runtime_bundle::BuildOptions {
                from: Some(host.path()),
                arch: Some("x86_64"),
                identity: Some(&id_path),
                extra_libs: &[libstd, libruntime],
                extra_bins: &[],
            },
        )
        .expect("build thin bundle");

        // The shared libs the thin app links live at the top of the bundle's lib/ — exactly the dir
        // the launcher turns into LD_LIBRARY_PATH.
        assert!(bundle_dir.join("lib/libstd-abc123.so").exists());
        assert!(bundle_dir.join("lib/libruntime_dylib.so").exists());

        let mut m = manifest("so.tilo.thin", "bin/thin", "off");
        m.version = "0.1.0".to_string();
        m.runtime = Some(format!("tilo-runtime/{version}"));
        m.entrypoint.r#type = launcher::manifest::EntrypointType::Cli;
        m.cli_commands = vec!["thin".to_string()];

        // The manifest's `runtime` field resolves to the signed bundle (signature verified inside
        // resolve_runtime).
        let opts = offline_options();
        let (resolved_path, resolved_ver) = launcher::run::resolve_runtime_test_helper(&m, &opts)
            .expect("resolve ok")
            .expect("bundle resolves");
        assert_eq!(resolved_ver, version);
        assert_eq!(resolved_path, bundle_dir);

        // The argv mounts the bundle and points LD_LIBRARY_PATH at the dir holding libstd + libruntime
        // — so the thin app finds them by soname.
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

        let payload = tempfile::tempdir().unwrap();
        let entry = payload.path().join("bin/thin");
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::fs::write(
            &entry,
            launcher::loader_preflight::elf_fixture(
                &["libruntime_dylib.so", "libstd-55999f89fc36b51f.so"],
                None,
            ),
        )
        .unwrap();
        let data_home = tempfile::tempdir().unwrap();
        let opts = RunOptions {
            no_services: true,
            landlock_enforce: false,
            ..isolated_options(data_home.path())
        };
        let err = run_with(&m, payload.path(), &opts).unwrap_err();
        match &err {
            RunError::RuntimeLibraryMissing {
                package,
                runtime,
                library,
                searched,
            } => {
                assert_eq!(package, "so.tilo.thin");
                assert_eq!(runtime, &format!("tilo-runtime/{version}"));
                assert_eq!(library, "libstd-55999f89fc36b51f.so");
                assert_eq!(searched.first(), Some(&bundle_dir.join("lib")));
            }
            other => panic!("expected RuntimeLibraryMissing, got {other:?}"),
        }
        let message = err.to_string();
        for needle in [
            "built for a different Tilo runtime",
            "libstd-55999f89fc36b51f.so",
            "tilo-runtime/thin-smoke-1",
            "`app update so.tilo.thin`",
            "`app install so.tilo.thin`",
            "`app remove so.tilo.thin`",
        ] {
            assert!(
                message.contains(needle),
                "{needle:?} missing from: {message}"
            );
        }
        assert!(
            !message.contains('\n'),
            "one stderr line for the shell: {message}"
        );
    }
    {
        let mut home = runtime_home();
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
        let mut manifest = hello_manifest(BTreeMap::new());
        manifest.runtime = Some(format!("tilo-runtime/{version}"));
        // An installed bundle of the same version under the data home must lose.
        install_signed_bundle(home.data(), version);
        home.env()
            .set(runtime_bundle::RUNTIME_DIR_ENV, overlay.path());
        let resolved =
            launcher::run::resolve_runtime_test_helper(&manifest, &RunOptions::default());
        assert_eq!(resolved.unwrap().unwrap().0, bundle);
    }
    {
        use launcher::dbus_proxy::PROXY_BIN_ENV;
        use launcher::sandbox::build_bwrap_argv;

        // Point the launcher's proxy binary at a checked-in mock.
        let tmp = tempfile::tempdir().unwrap();
        let mock = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock-xdg-dbus-proxy");
        let mut env = ScopedEnv::new();
        env.set(PROXY_BIN_ENV, &mock)
            .set("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1234/bus");

        let mut m = hello_manifest(BTreeMap::new());
        m.sandbox.portals.insert(
            "notifications".to_string(),
            launcher::manifest::PortalPolicy::ViaHost,
        );

        // Drive the proxy path via the launcher's public helper.
        let sock = tmp.path().join("proxy.sock");
        let bin = launcher::dbus_proxy::find_proxy_binary().expect("PROXY_BIN_ENV was set above");
        // The launch's own readiness bound is sized for the real proxy on an idle machine; this
        // asserts the wiring, not the startup latency of a shell script on a loaded test host.
        let guard = launcher::dbus_proxy::spawn_dbus_proxy_within(
            &bin,
            "unix:path=/run/user/1234/bus",
            &sock,
            tilo_test_env::hang_bound(),
        )
        .expect("spawn mock proxy");
        let plan = launcher::sandbox::DBusBindPlan {
            host_socket: guard.socket_path().to_path_buf(),
            sandbox_path: launcher::dbus_proxy::sandbox_bus_path(1234),
            atspi: None,
        };

        let pid = guard.pid().expect("proxy pid");
        assert!(std::path::Path::new(&format!("/proc/{pid}")).exists());

        // The bwrap argv binds the proxy socket at /run/user/1234/bus, NOT the raw session bus
        // address.
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
        let deadline = tilo_test_env::hang_deadline();
        while std::path::Path::new(&format!("/proc/{pid}")).exists()
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "proxy pid {pid} should be reaped after Drop"
        );
        drop(env);
    }
}

#[test]
fn required_runtimes_mount_when_installed_and_trusted_and_refuse_the_launch_otherwise() {
    let mut home = runtime_home();
    let (installed, identity) = install_signed_bundle(home.data(), "2026.10.0-engine");
    let _unsigned = install_unsigned_bundle(home.data(), "2026.10.0-unsigned");
    let mut m = hello_manifest(BTreeMap::new());
    m.required_runtimes = vec!["tilo-runtime/2026.10.0-engine".to_string()];
    let trust_file = home.root().join("trust.txt");
    std::fs::write(&trust_file, format!("{}\n", identity.public_key_hex())).unwrap();
    home.env()
        .set("TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE", &trust_file);
    let opts = offline_options();

    let (argv, mounts) = launcher::run::required_runtimes_test_helper(&m, &opts).unwrap();
    assert_eq!(
        argv,
        [
            "--ro-bind".to_string(),
            installed.display().to_string(),
            "/run/tilo-runtime/2026.10.0-engine".to_string(),
            "entry".to_string(),
        ]
    );
    assert_eq!(mounts, ["/run/tilo-runtime/2026.10.0-engine"]);

    m.required_runtimes
        .push("tilo-runtime/2026.10.0-missing".to_string());
    let refused = launcher::run::required_runtimes_test_helper(&m, &opts).unwrap_err();
    assert!(
        matches!(refused, launcher::run::RunError::RequiredRuntimeMissing { ref runtime, .. }
            if runtime == "tilo-runtime/2026.10.0-missing"),
        "{refused}"
    );

    m.required_runtimes = vec!["tilo-runtime/2026.10.0-unsigned".to_string()];
    launcher::run::required_runtimes_test_helper(&m, &opts).unwrap_err();

    m.required_runtimes = vec!["tilo-runtime/2026.10.0-engine".to_string()];
    let other = runtime_bundle::Identity::generate();
    std::fs::write(&trust_file, format!("{}\n", other.public_key_hex())).unwrap();
    let refused = launcher::run::required_runtimes_test_helper(&m, &opts).unwrap_err();
    assert!(
        matches!(
            refused,
            launcher::run::RunError::RuntimeBundleSignature { .. }
        ),
        "{refused}"
    );

    m.required_runtimes = vec!["tilo-runtime/../2026.07".to_string()];
    let refused = launcher::run::required_runtimes_test_helper(&m, &opts).unwrap_err();
    assert!(
        matches!(
            refused,
            launcher::run::RunError::RuntimeReferenceMalformed(_)
        ),
        "{refused}"
    );
}

#[test]
fn an_entrypoint_runtime_is_mounted_and_required() {
    let mut home = runtime_home();
    let (installed, identity) = install_signed_bundle(home.data(), "0.1.0-tilolang");
    let mut m = hello_manifest(BTreeMap::new());
    m.required_runtimes = vec!["tilolang/0.1.0-tilolang".to_string()];
    m.entrypoint.runtime = Some("tilolang/0.1.0-tilolang".to_string());
    let trust_file = home.root().join("trust.txt");
    std::fs::write(&trust_file, format!("{}\n", identity.public_key_hex())).unwrap();
    home.env()
        .set("TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE", &trust_file);
    let opts = offline_options();

    let (argv, mounts) = launcher::run::required_runtimes_test_helper(&m, &opts).unwrap();
    assert_eq!(
        argv,
        [
            "--ro-bind".to_string(),
            installed.display().to_string(),
            "/run/tilo-runtime/0.1.0-tilolang".to_string(),
            "entry".to_string(),
        ]
    );
    assert_eq!(mounts, ["/run/tilo-runtime/0.1.0-tilolang"]);

    m.required_runtimes = vec!["tilolang/0.2.0-missing".to_string()];
    m.entrypoint.runtime = Some("tilolang/0.2.0-missing".to_string());
    let refused = launcher::run::required_runtimes_test_helper(&m, &opts).unwrap_err();
    assert!(
        matches!(refused, launcher::run::RunError::RequiredRuntimeMissing { ref runtime, .. }
            if runtime == "tilolang/0.2.0-missing"),
        "{refused}"
    );

    m.required_runtimes = vec!["tilolang/0.1.0-tilolang".to_string()];
    m.entrypoint.runtime = Some("tilolang/0.1.0-tilolang".to_string());
    let other = runtime_bundle::Identity::generate();
    std::fs::write(&trust_file, format!("{}\n", other.public_key_hex())).unwrap();
    let refused = launcher::run::required_runtimes_test_helper(&m, &opts).unwrap_err();
    assert!(
        matches!(
            refused,
            launcher::run::RunError::RuntimeBundleSignature { .. }
        ),
        "{refused}"
    );
}

#[test]
fn bundle_signatures_gate_the_runtime_mount() {
    {
        let home = runtime_home();
        let _bundle = install_unsigned_bundle(home.data(), "sig-refuse-1");

        let mut m = hello_manifest(BTreeMap::new());
        m.runtime = Some("tilo-runtime/sig-refuse-1".to_string());

        let opts = offline_options();
        let err = launcher::run::resolve_runtime_test_helper(&m, &opts).unwrap_err();
        match &err {
            RunError::RuntimeBundleUnsigned { .. } => {}
            other => panic!("expected RuntimeBundleUnsigned, got {other:?}"),
        }

        let opts_ok = RunOptions {
            allow_unsigned_runtime: true,
            ..offline_options()
        };
        let resolved =
            launcher::run::resolve_runtime_test_helper(&m, &opts_ok).expect("opt-in accepts");
        let (_p, v) = resolved.expect("bundle resolves");
        assert_eq!(v, "sig-refuse-1");
    }
    {
        let mut home = runtime_home();
        let (_bundle, identity) = install_signed_bundle(home.data(), "trust-file-1");

        let mut m = hello_manifest(BTreeMap::new());
        m.runtime = Some("tilo-runtime/trust-file-1".to_string());
        let opts = offline_options();

        // 1.
        let trust_file = home.root().join("trust.txt");
        std::fs::write(
            &trust_file,
            format!(
                "# managed by test\n{}\n\n# blank + comments are ignored\n",
                identity.public_key_hex()
            ),
        )
        .unwrap();
        home.env()
            .set("TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE", &trust_file);
        launcher::run::resolve_runtime_test_helper(&m, &opts).expect("trusted pubkey ok");

        // 2.
        let other = runtime_bundle::Identity::generate();
        std::fs::write(&trust_file, format!("{}\n", other.public_key_hex())).unwrap();
        let err = launcher::run::resolve_runtime_test_helper(&m, &opts).unwrap_err();
        match &err {
            RunError::RuntimeBundleSignature { .. } => {}
            RunError::RuntimeBundleUnsigned { .. } => panic!("bundle IS signed"),
            other => panic!("expected RuntimeBundleSignature, got {other:?}"),
        }

        // 3.
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

        // 4.
        home.env().remove("TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE");
        launcher::run::resolve_runtime_test_helper(&m, &opts).expect("no trust file = any signed");
    }
    {
        let home = runtime_home();
        let (_bundle, identity) = install_signed_bundle(home.data(), "sig-pin-1");

        let mut m = hello_manifest(BTreeMap::new());
        m.runtime = Some("tilo-runtime/sig-pin-1".to_string());

        // Default: accepted.
        let opts = offline_options();
        launcher::run::resolve_runtime_test_helper(&m, &opts).expect("accept signed");

        // Pin to the bundle's publisher: accepted.
        let opts_pinned = RunOptions {
            require_runtime_publisher: Some(identity.public_key_hex()),
            ..offline_options()
        };
        launcher::run::resolve_runtime_test_helper(&m, &opts_pinned).expect("pin matches");

        // Pin to a different publisher: refused.
        let other = runtime_bundle::Identity::generate();
        let opts_wrong = RunOptions {
            require_runtime_publisher: Some(other.public_key_hex()),
            ..offline_options()
        };
        let err = launcher::run::resolve_runtime_test_helper(&m, &opts_wrong).unwrap_err();
        match &err {
            RunError::RuntimeBundleSignature { .. } => {}
            other => panic!("expected RuntimeBundleSignature, got {other:?}"),
        }
    }
}

// workspace capabilities

#[test]
fn capability_files_on_disk_become_binds_or_nothing() {
    {
        use collab_types::caps::{Access, CapOverride, Scope, Subject};

        let dir = tempfile::tempdir().unwrap();
        let caps_file = dir.path().join("caps.json");
        let caps = vec![
            CapOverride::grant(
                Subject::app("com.example.hello"),
                Scope::FsPath("/var/workspace/books".to_string()),
                Access::Read,
            ),
            // Addressed to a different app: must not survive the crossing, even though it sits in the
            // same file.
            CapOverride::grant(
                Subject::app("com.example.other"),
                Scope::FsPath("/var/workspace/secrets".to_string()),
                Access::Write,
            ),
        ];
        std::fs::write(
            &caps_file,
            serde_json::to_vec(&collab_types::caps::LaunchGrants {
                authors: Vec::new(),
                sandbox: caps,
            })
            .unwrap(),
        )
        .unwrap();

        let m = hello_manifest(BTreeMap::new());
        let opts = RunOptions {
            capabilities_path: Some(caps_file),
            ..offline_options()
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
    {
        let dir = tempfile::tempdir().unwrap();
        let m = hello_manifest(BTreeMap::new());

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
}
