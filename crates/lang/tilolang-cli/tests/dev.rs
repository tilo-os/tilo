//! A saved build is staged for the session's daemon as the app's manifest and payload, unsigned:
//! the device marks it, packs it, signs it with its own key and runs it under the id its marking
//! derives.

use tilo_test_env::SpawnUnderEnvLock as _;
use tilolang_cli::package::{self, BuildOptions};

use crate::support::{copy_package, packages, with_used};

#[test]
fn a_saved_build_is_staged_unsigned_for_the_device() {
    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("counter");
    copy_package(&packages().join("counter"), &source);
    let options = BuildOptions {
        dir: source,
        search: Vec::new(),
        out: scratch.path().join("out"),
        tilo_runtime: package::DEFAULT_TILO_RUNTIME.into(),
        tilolang_runtime: package::default_tilolang_runtime(),
        version: "0.1.0".into(),
        locked: false,
    };
    let staged = package::stage_dev(&options, &[]).unwrap().pop().unwrap();
    let manifest = tilo_app_package::manifest::Manifest::from_toml_str(
        &std::fs::read_to_string(&staged.manifest).unwrap(),
    )
    .unwrap();
    assert_eq!(
        manifest.package.id, staged.app,
        "the app's own id, unsigned"
    );
    assert_eq!(manifest.publisher.identity, "", "no publisher signs it");
    assert!(manifest.publisher.signature.is_empty());
    assert!(
        manifest.package.dev.is_none(),
        "the device marks it, not the tool"
    );
    // The device reads the manifest apart from the payload it packs.
    assert!(
        staged
            .payload
            .iter()
            .all(|(path, _)| path != "tilo-app.toml"),
        "{:?}",
        staged.payload
    );
    assert!(
        staged
            .payload
            .iter()
            .any(|(path, _)| path == tilolang_tilo::bundle::WASM_FILE),
        "the build's module is in the payload: {:?}",
        staged.payload
    );
    assert!(
        staged.payload.iter().all(|(_, source)| source.is_file()),
        "every payload file is on disk where the manifest names it"
    );
    // Nothing is packed: the device packs the `.tapp` it signs.
    let mut pending = vec![staged.dir.clone()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                pending.push(path);
            } else {
                assert_ne!(
                    path.extension().and_then(|extension| extension.to_str()),
                    Some("tapp"),
                    "{} is a package the tool would have signed",
                    path.display()
                );
            }
        }
    }
    // A save is compared by its digest: the same sources stage to the same one.
    let again = package::stage_dev(&options, &[]).unwrap().pop().unwrap();
    assert_eq!(again.digest, staged.digest);
}

/// A build for the device is compiled as the id the device's marking derives, while its manifest
/// names the app it builds: the device derives the very id the module runs under, and the build's
/// records and kinds are its own.
#[test]
fn a_build_for_the_device_is_compiled_as_the_id_it_runs_under() {
    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("counter");
    copy_package(&packages().join("counter"), &source);
    let options = BuildOptions {
        dir: source,
        search: Vec::new(),
        out: scratch.path().join("out"),
        tilo_runtime: package::DEFAULT_TILO_RUNTIME.into(),
        tilolang_runtime: package::default_tilolang_runtime(),
        version: "0.1.0".into(),
        locked: false,
    };
    let of = "so.tilo.lang.counter";
    let dev = "so.tilo.lang.counter.dev.0123456789abcdef";
    let compile_as = [(of.to_owned(), dev.to_owned())];
    let staged = package::stage_dev(&options, &compile_as)
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(staged.dev, dev, "the staging names the id it runs under");
    assert_eq!(
        staged.meta.app, dev,
        "the module is the device's own app, not the one its sources declare"
    );
    assert_ne!(staged.app, dev, "the manifest names the app it builds");
    let manifest = tilo_app_package::manifest::Manifest::from_toml_str(
        &std::fs::read_to_string(&staged.manifest).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest.package.id, staged.app);
    assert!(
        manifest
            .resources
            .kinds
            .iter()
            .any(|kind| kind.starts_with(&format!("tl.{dev}."))),
        "the build declares its own kinds: {:?}",
        manifest.resources.kinds
    );
}

/// A package that carries an `app.toml` names in it the app its sources declare, not the device's
/// development copy of it: a development build accepts the declaration and still compiles the
/// module as the id it runs under there.
#[test]
fn a_development_build_accepts_the_app_its_app_toml_names() {
    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("counter");
    copy_package(&packages().join("counter"), &source);
    std::fs::write(
        source.join("app.toml"),
        "[package]\nid = \"so.tilo.lang.counter\"\nkind = \"app\"\n",
    )
    .unwrap();
    let options = BuildOptions {
        dir: source,
        search: Vec::new(),
        out: scratch.path().join("out"),
        tilo_runtime: package::DEFAULT_TILO_RUNTIME.into(),
        tilolang_runtime: package::default_tilolang_runtime(),
        version: "0.1.0".into(),
        locked: false,
    };
    let of = "so.tilo.lang.counter";
    let dev = "so.tilo.lang.counter.dev.0123456789abcdef";
    let compile_as = [(of.to_owned(), dev.to_owned())];
    let staged = package::stage_dev(&options, &compile_as)
        .unwrap_or_else(|error| panic!("{error}"))
        .pop()
        .unwrap();
    assert_eq!(staged.app, of, "the manifest names the app it builds");
    assert_eq!(staged.meta.app, dev, "the module is the device's own copy");
    // A declaration of an app those sources do not declare is still refused.
    std::fs::write(
        options.dir.join("app.toml"),
        "[package]\nid = \"so.tilo.lang.other\"\n",
    )
    .unwrap();
    let Err(refused) = package::stage_dev(&options, &compile_as) else {
        panic!("another app is refused");
    };
    assert!(
        refused.contains("so.tilo.lang.other") && refused.contains(of),
        "{refused}"
    );
}

/// A development build of an app that imports another tilolang app resolves that import, which is
/// staged as a development build too: the importer names the id the imported package is installed
/// under, while its `use` names the id the sources declare.
#[test]
fn a_development_build_resolves_imports_under_their_development_ids() {
    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("tl");
    for name in with_used(&["tube"]) {
        copy_package(&packages().join(&name), &source.join(&name));
    }
    let dir = source.join("tube");
    let options = BuildOptions {
        dir: dir.clone(),
        search: Vec::new(),
        out: scratch.path().join("out"),
        tilo_runtime: package::DEFAULT_TILO_RUNTIME.into(),
        tilolang_runtime: package::default_tilolang_runtime(),
        version: "0.1.0".into(),
        locked: false,
    };
    // What `tilolang build --dev` asks the device: the id it installs each package's development
    // build under.
    let compile_as: Vec<(String, String)> = tilolang_cli::package_dirs(&dir, &[])
        .unwrap()
        .iter()
        .filter_map(|package| {
            let source = tilolang::source::PackageSource::read_dir(package).ok()?;
            let app = source.header().0?;
            Some((app.clone(), format!("{app}.dev.0123456789abcdef")))
        })
        .collect();
    assert!(
        compile_as.iter().any(|(app, _)| app == "so.tilo.playback"),
        "tube imports playback: {compile_as:?}"
    );
    let staged =
        package::stage_dev(&options, &compile_as).unwrap_or_else(|error| panic!("{error}"));
    let tube = staged
        .iter()
        .find(|app| app.app == "so.tilo.tube")
        .expect("tube is staged");
    assert_eq!(tube.dev, "so.tilo.tube.dev.0123456789abcdef");
    assert!(
        tube.meta
            .uses
            .iter()
            .any(|used| used == "so.tilo.playback.dev.0123456789abcdef"),
        "tube's use of playback names the id playback is staged under: {:?}",
        tube.meta.uses
    );
}

/// A `tilolang dev` run again attaches to the session this login already runs: one command does
/// not mean a new session each time.
#[cfg(unix)]
#[test]
fn a_second_dev_attaches_to_the_running_session() {
    use std::os::unix::fs::PermissionsExt as _;

    let scratch = tempfile::tempdir().unwrap();
    let repo = scratch.path().join("repo");
    std::fs::create_dir_all(repo.join("dist/dev")).unwrap();
    // The session script records that it ran: this dev must not run it.
    let started = scratch.path().join("started");
    let script = repo.join("dist/dev/nested-session.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\necho started >> {}\nexit 1\n", started.display()),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    // A running session of this login: its state, its control socket, the API socket it
    // answers on, and the runtimes it baked.
    let state = scratch.path().join("state");
    std::fs::create_dir_all(state.join("apps/runtimes")).unwrap();
    std::fs::write(state.join("apps/runtimes/.version"), "2026.07\n").unwrap();
    std::fs::write(state.join("apps/runtimes/.tilolang-version"), "0.1.0\n").unwrap();
    let runtime = scratch.path().join("runtime");
    let digest = {
        use sha2::Digest as _;
        sha2::Sha256::digest(state.as_os_str().as_encoded_bytes())
    };
    let mut hash = hex::encode(digest);
    hash.truncate(12);
    let session = runtime.join(format!("tilo-nested-{hash}"));
    let login = session.join(tilo_paths::login::SESSION_DIR);
    std::fs::create_dir_all(&login).unwrap();
    let _control = std::os::unix::net::UnixListener::bind(session.join("control.sock")).unwrap();
    let _api =
        std::os::unix::net::UnixListener::bind(login.join(tilo_paths::login::API_SOCKET_FILE))
            .unwrap();
    std::fs::write(
        session.join("session.json"),
        serde_json::json!({
            "control": session.join("control.sock"),
            "runtime": session,
            "state": state,
        })
        .to_string(),
    )
    .unwrap();

    // The session's tools: the installer this test lets fail after the attach is seen.
    let bin = repo.join("target/nested-session/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let install = bin.join("tilo-install");
    std::fs::write(&install, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&install, std::fs::Permissions::from_mode(0o755)).unwrap();

    let package = scratch.path().join("counter");
    copy_package(&packages().join("counter"), &package);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_tilolang"))
        .args(["dev"])
        .arg(&package)
        .args(["--search"])
        .arg(packages())
        .args(["--repo"])
        .arg(&repo)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", scratch.path().join("xdg-state"))
        .env("TILO_REPO", &repo)
        .output_locked()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("attaching to the running session"),
        "{stderr}"
    );
    assert!(
        !started.exists(),
        "no session was started: {}",
        std::fs::read_to_string(&started).unwrap_or_default()
    );
}

/// A session may run on a device store other than its state's (`TILO_APPS_DATA_DIR`, which the
/// walk shares and a session started from a shell inherits). A development build's app is
/// installed there with the runtime it pins, and the installer finds that runtime: the store the
/// launcher reads the build's runtime from is the session's, never a directory this tool assumed.
#[cfg(unix)]
#[test]
fn a_dev_build_installs_into_the_store_its_session_runs_on() {
    let scratch = tempfile::tempdir().unwrap();
    let repo = scratch.path().join("repo");
    std::fs::create_dir_all(repo.join("dist/dev")).unwrap();
    let started = scratch.path().join("started");
    let script = repo.join("dist/dev/nested-session.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\necho started >> {}\nexit 1\n", started.display()),
    )
    .unwrap();
    executable(&script);

    // The store the session runs on, as the session script named it: the runtime bundles it baked
    // and no app yet.
    let state = scratch.path().join("state");
    let store = scratch.path().join("shared-store");
    let tilolang = "0.1.0+tilolang.0123456789abcdef";
    let runtimes = store.join("runtimes");
    std::fs::create_dir_all(runtimes.join(tilolang)).unwrap();
    std::fs::write(
        runtimes.join(tilolang).join("bundle.toml"),
        format!("version = \"{tilolang}\"\n"),
    )
    .unwrap();
    std::fs::write(runtimes.join(".version"), "2026.07\n").unwrap();
    std::fs::write(runtimes.join(".tilolang-version"), format!("{tilolang}\n")).unwrap();

    // A root this tool must not read: its state's own, with the stamps an earlier layout left and
    // no bundle.
    let unused = state.join("apps/runtimes");
    std::fs::create_dir_all(&unused).unwrap();
    std::fs::write(
        unused.join(".tilolang-version"),
        "0.1.0+tilolang.ffffffffffffffff\n",
    )
    .unwrap();

    let runtime = scratch.path().join("runtime");
    write_session(&state, &runtime, &store);
    // The session's installer: it refuses as the real one does when the runtime a build pins is
    // not in the store the launcher reads, and records the store it was pointed at.
    let bin = repo.join("target/nested-session/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let calls = scratch.path().join("installs");
    write_installer(&bin.join("tilo-install"), &calls, tilolang);

    let package = scratch.path().join("counter");
    copy_package(&packages().join("counter"), &package);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_tilolang"))
        .args(["dev"])
        .arg(&package)
        .args(["--search"])
        .arg(packages())
        .args(["--repo"])
        .arg(&repo)
        .args(["--session"])
        .arg(&state)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", scratch.path().join("xdg-state"))
        .env("TILO_REPO", &repo)
        .output_locked()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("does not have it"),
        "the installer found no runtime in the store the session runs on: {stderr}"
    );
    assert!(
        stderr.contains("installed"),
        "the build reached the session's installer: {stderr}"
    );
    assert!(
        !started.exists(),
        "no session was started: {}",
        std::fs::read_to_string(&started).unwrap_or_default()
    );
    // Every call went to the session's store: its apps, and the runtimes its launcher reads.
    let recorded = std::fs::read_to_string(&calls).unwrap();
    for line in recorded.lines() {
        assert!(
            line.contains(&store.display().to_string()),
            "the installer was pointed at {}: {line}",
            store.display()
        );
    }
    assert!(
        recorded.lines().any(|line| line.starts_with("install ")),
        "the build was installed there: {recorded}"
    );
    assert!(runtimes.join(tilolang).join("bundle.toml").is_file());
}

#[cfg(unix)]
fn executable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A running session of this login on `state`, whose device store is `store`; answers the runtime
/// directory it keeps its file in.
#[cfg(unix)]
fn write_session(
    state: &std::path::Path,
    runtime: &std::path::Path,
    store: &std::path::Path,
) -> std::path::PathBuf {
    let digest = {
        use sha2::Digest as _;
        sha2::Sha256::digest(state.as_os_str().as_encoded_bytes())
    };
    let mut hash = hex::encode(digest);
    hash.truncate(12);
    let session = runtime.join(format!("tilo-nested-{hash}"));
    let login = session.join(tilo_paths::login::SESSION_DIR);
    std::fs::create_dir_all(&login).unwrap();
    let _control = std::os::unix::net::UnixListener::bind(session.join("control.sock")).unwrap();
    let _api =
        std::os::unix::net::UnixListener::bind(login.join(tilo_paths::login::API_SOCKET_FILE))
            .unwrap();
    std::fs::write(
        session.join("session.json"),
        serde_json::json!({
            "control": session.join("control.sock"),
            "runtime": session,
            "state": state,
            "store": store,
        })
        .to_string(),
    )
    .unwrap();
    session
}

/// The session's `tilo-install`: it refuses the build when the runtime it pins (`tilolang`) is not
/// in the store the launcher reads, records every call's stores in `calls`, and answers the ids a
/// development build of the counter runs under.
#[cfg(unix)]
fn write_installer(install: &std::path::Path, calls: &std::path::Path, tilolang: &str) {
    std::fs::write(
        install,
        format!(
            "#!/bin/sh\n\
             echo \"$1 $TILO_APPS_DATA_DIR $TILO_RUNTIME_BUNDLE_DIR\" >> {}\n\
             case \"$1\" in\n\
             dev-id) echo so.tilo.lang.counter.dev.0123456789abcdef ;;\n\
             install)\n\
             if [ ! -f \"$TILO_RUNTIME_BUNDLE_DIR/{tilolang}/bundle.toml\" ]; then\n\
             echo \"tilolang: pins runtime tilolang/{tilolang} and this device does not have it\" >&2\n\
             exit 1\n\
             fi\n\
             while [ $# -gt 0 ]; do\n\
             if [ \"$1\" = --id-file ]; then printf '%s\\n' so.tilo.lang.counter.dev.0123456789abcdef > \"$2\"; fi\n\
             shift\n\
             done ;;\n\
             *) exit 0 ;;\n\
             esac\n",
            calls.display()
        ),
    )
    .unwrap();
    executable(install);
}
