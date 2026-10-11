//! Runtime packages: the runtime build a thin app links travels as a
//! `kind = "runtime"` package, and installing it installs the signed bundle
//! where the launcher looks. An app naming a build this device lacks is
//! refused until that build is installed, and a runtime package is held to
//! the launcher's own bundle checks.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_app_package::{Manifest, SigningIdentity, pack};

fn key() -> SigningKey {
    SigningKey::generate(&mut UnwrapErr(SysRng))
}

fn hex_key(key: &SigningKey) -> String {
    hex::encode(key.verifying_key().to_bytes())
}

fn write_tapp(path: &Path, manifest: &str, key: &SigningKey, payload: &Path) {
    let manifest = Manifest::from_toml_str(manifest).unwrap();
    let packed = pack(
        payload,
        &manifest,
        &SigningIdentity::from_signing_key(key.clone()),
    )
    .expect("pack");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = std::fs::File::create(path).unwrap();
    tilo_app_package::tapp::write(file, &packed.meta, Some(&packed)).unwrap();
}

fn isa_line() -> &'static str {
    match runtime_bundle::host_arch() {
        "aarch64" => "aarch64 = \"aarch64\"",
        _ => "x86_64 = \"x86-64\"",
    }
}

struct Env {
    home: tempfile::TempDir,
    publisher: SigningKey,
}

impl Env {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
            publisher: key(),
        }
    }

    /// An environment whose home is under `parent`.
    fn in_dir(parent: &Path) -> Self {
        Self {
            home: tempfile::tempdir_in(parent).unwrap(),
            publisher: key(),
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.home.path().join(rel)
    }

    /// A signed runtime build of 2026.07 holding `dylib` as its toolkit,
    /// as the release pipeline bakes it; returns its version.
    fn bundle(&self, name: &str, dylib: &[u8], runtime_key: &Path) -> (String, PathBuf) {
        let host = self.path(&format!("host-{name}"));
        let lib = runtime_bundle::host_minimal_paths_for(runtime_bundle::host_arch())
            .unwrap()
            .first()
            .unwrap()
            .trim_start_matches('/');
        std::fs::create_dir_all(host.join(lib).parent().unwrap()).unwrap();
        std::fs::write(host.join(lib), b"loader").unwrap();
        let toolkit = self.path(&format!("toolkit-{name}/libruntime_dylib.so"));
        std::fs::create_dir_all(toolkit.parent().unwrap()).unwrap();
        std::fs::write(&toolkit, dylib).unwrap();
        let (manifest, dir) = runtime_bundle::build_abi_bundle(
            &self.path(&format!("bundles-{name}")),
            "2026.07",
            &runtime_bundle::BuildOptions {
                from: Some(&host),
                arch: None,
                identity: Some(runtime_key),
                extra_libs: &[toolkit],
            },
        )
        .unwrap();
        (manifest.version, dir)
    }

    /// The `so.tilo.runtime` package of a bundle, saying it provides
    /// `provides`.
    fn runtime_tapp(&self, name: &str, bundle: &Path, provides: &str) -> PathBuf {
        self.runtime_tapp_with_host(name, bundle, provides, None)
    }

    fn runtime_tapp_with_host(
        &self,
        name: &str,
        bundle: &Path,
        provides: &str,
        host: Option<&str>,
    ) -> PathBuf {
        use std::fmt::Write as _;
        let mut manifest = format!(
            r#"
[package]
id = "so.tilo.runtime"
version = "0.1.0"
kind = "runtime"
provides = "{provides}"

[package.isa]
{isa}

[publisher]
identity = "ed25519:{key}"
signature = ""

[sandbox]
namespaces = []
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"
"#,
            isa = isa_line(),
            key = hex_key(&self.publisher),
        );
        if let Some(host) = host {
            write!(
                manifest,
                "\n[entrypoint]\ntype = \"cli\"\nbin = \"{host}\"\n"
            )
            .unwrap();
        }
        let path = self.path(&format!("runtime-{name}.tapp"));
        write_tapp(&path, &manifest, &self.publisher, bundle);
        path
    }

    /// An app linked against the runtime build `runtime`.
    fn app_tapp(&self, runtime: &str) -> PathBuf {
        let payload = self.path("app-payload");
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        std::fs::write(payload.join("bin/app"), b"app").unwrap();
        let manifest = format!(
            r#"
[package]
id = "so.tilo.linked"
version = "1.0.0"
kind = "app"
runtime = "{runtime}"

[publisher]
identity = "ed25519:{key}"
signature = ""

[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount", "net"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/app"
"#,
            key = hex_key(&self.publisher),
        );
        let path = self.path("app.tapp");
        write_tapp(&path, &manifest, &self.publisher, &payload);
        path
    }

    /// The thin form of `runtime_tapp`: metadata only, its payload blobs
    /// served by a registry. Returns the path and the payload by hash.
    fn thin_runtime_tapp(
        &self,
        name: &str,
        bundle: &Path,
        provides: &str,
    ) -> (PathBuf, HashMap<String, Vec<u8>>) {
        let full = self.runtime_tapp(name, bundle, provides);
        let mut blobs = Collected::default();
        let tapp =
            tilo_app_package::tapp::read(std::fs::File::open(&full).unwrap(), &mut blobs).unwrap();
        let path = self.path(&format!("runtime-{name}-thin.tapp"));
        let file = std::fs::File::create(&path).unwrap();
        tilo_app_package::tapp::write(file, &tapp.meta, None).unwrap();
        (path, blobs.0)
    }

    fn trust(&self, runtime_keys: &[&str]) {
        let mut list = runtime_keys.join("\n");
        list.push('\n');
        std::fs::write(self.path("trusted-runtime-publishers.txt"), list).unwrap();
    }

    fn cmd(&self, args: &[&str]) -> (bool, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_tilo-install"))
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_DATA_HOME", self.path("data"))
            // Where `XDG_DATA_HOME` puts it on every platform, macOS included.
            .env(
                runtime_bundle::RUNTIME_DIR_ENV,
                self.path("data/tilo-runtime"),
            )
            .env(
                "TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE",
                self.path("trusted-runtime-publishers.txt"),
            )
            .args(args)
            .output()
            .expect("spawn tilo-install");
        (
            out.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    fn installed_bundle(&self, version: &str) -> PathBuf {
        self.path("data/tilo-runtime").join(version)
    }
}

/// Payload blobs by hex hash, as a registry stores them.
#[derive(Default)]
struct Collected(HashMap<String, Vec<u8>>);

impl tilo_app_package::BlobSink for Collected {
    fn accept(
        &mut self,
        hash: &tilo_app_package::BlobHash,
        len: u64,
        bytes: &mut dyn Read,
    ) -> std::io::Result<()> {
        let mut body = Vec::new();
        bytes.take(len).read_to_end(&mut body)?;
        self.0.insert(hash.to_hex(), body);
        Ok(())
    }
}

/// A registry serving `blobs` at `/blobs/<hex>` until the test ends; every
/// requested path is recorded.
fn registry(blobs: HashMap<String, Vec<u8>>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requested = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&requested);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut request = [0; 2048];
            let read = stream.read(&mut request).unwrap_or(0);
            let text = String::from_utf8_lossy(&request[..read]).to_string();
            let path = text.split_whitespace().nth(1).unwrap_or("").to_string();
            seen.lock().unwrap().push(path.clone());
            let body = path
                .strip_prefix("/blobs/")
                .and_then(|hash| blobs.get(hash));
            let mut reply = match body {
                Some(body) => format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .into_bytes(),
                None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_vec(),
            };
            reply.extend_from_slice(body.map_or(&[][..], Vec::as_slice));
            let _ = stream.write_all(&reply);
        }
    });
    (url, requested)
}

#[test]
fn runtime_install_reuses_verified_local_blobs() {
    let env = Env::new();
    let (runtime_key, runtime_identity) = identity_file(&env, "runtime-publisher");
    env.trust(&[&runtime_key]);
    // Build one is installed; build two shares its loader, not its toolkit.
    let (one, one_bundle) = env.bundle("one", b"toolkit build one", &runtime_identity);
    let package = env.runtime_tapp("one", &one_bundle, &format!("tilo-runtime/{one}"));
    let (ok, out) = env.cmd(&["install", package.to_str().unwrap(), "--allow-self-signed"]);
    assert!(ok, "{out}");
    let (two, two_bundle) = env.bundle("two", b"toolkit build two", &runtime_identity);
    let (thin, blobs) = env.thin_runtime_tapp("two", &two_bundle, &format!("tilo-runtime/{two}"));
    let shared = tilo_app_package::BlobHash::of(b"loader").to_hex();
    assert!(blobs.contains_key(&shared), "both builds carry the loader");
    let (url, requested) = registry(blobs);

    // Phase `only_bytes_this_device_lacks_are_fetched`.
    let (ok, out) = env.cmd(&[
        "install",
        thin.to_str().unwrap(),
        "--allow-self-signed",
        // A second build of the one runtime package is a replacement: the
        // catalogue holds one verified record per package id.
        "--force",
        "--payload-from",
        &url,
    ]);
    assert!(ok, "{out}");
    assert!(out.contains("already on this device"), "{out}");
    let requested = requested.lock().unwrap().clone();
    assert!(!requested.is_empty(), "the new toolkit is fetched");
    assert!(
        !requested.iter().any(|path| path.contains(&shared)),
        "the shared loader was fetched again: {requested:?}"
    );
    let installed = env.installed_bundle(&two);
    runtime_bundle::verify_bundle(&installed).expect("the reused bundle verifies");
    assert_eq!(
        std::fs::read(installed.join("lib/libruntime_dylib.so")).unwrap(),
        b"toolkit build two"
    );
}

#[test]
fn runtime_install_refuses_a_local_file_whose_hash_differs() {
    let env = Env::new();
    let (runtime_key, runtime_identity) = identity_file(&env, "runtime-publisher");
    env.trust(&[&runtime_key]);
    let (one, one_bundle) = env.bundle("one", b"toolkit build one", &runtime_identity);
    let package = env.runtime_tapp("one", &one_bundle, &format!("tilo-runtime/{one}"));
    let (ok, out) = env.cmd(&["install", package.to_str().unwrap(), "--allow-self-signed"]);
    assert!(ok, "{out}");
    // The installed loader no longer holds the bytes its manifest lists. A
    // runtime sealed with fs-verity cannot be written, but its directory can:
    // the loader is replaced, not edited.
    let shared = tilo_app_package::BlobHash::of(b"loader").to_hex();
    let loader = bundle_path_of(&env.installed_bundle(&one), &shared);
    let loader_path = env.installed_bundle(&one).join(&loader);
    std::fs::remove_file(&loader_path).unwrap();
    std::fs::write(&loader_path, b"loadeR").unwrap();
    let (two, two_bundle) = env.bundle("two", b"toolkit build two", &runtime_identity);
    let (thin, blobs) = env.thin_runtime_tapp("two", &two_bundle, &format!("tilo-runtime/{two}"));
    let (url, requested) = registry(blobs);
    let (ok, out) = env.cmd(&[
        "install",
        thin.to_str().unwrap(),
        "--allow-self-signed",
        // A second build of the one runtime package is a replacement: the
        // catalogue holds one verified record per package id.
        "--force",
        "--payload-from",
        &url,
    ]);
    assert!(ok, "{out}");
    assert!(
        requested
            .lock()
            .unwrap()
            .iter()
            .any(|path| path.contains(&shared)),
        "the altered local copy must not stand in for the signed loader"
    );
    let installed = env.installed_bundle(&two);
    runtime_bundle::verify_bundle(&installed).expect("the installed bundle verifies");
    assert_eq!(std::fs::read(installed.join(loader)).unwrap(), b"loader");
}

/// A temporary directory on a filesystem with fs-verity:
/// `TILO_FSVERITY_DIR`, else the target's temporary directory.
fn verity_dir() -> Result<tempfile::TempDir, String> {
    let base = std::env::var_os("TILO_FSVERITY_DIR")
        .filter(|dir| !dir.is_empty())
        .map_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")), PathBuf::from);
    std::fs::create_dir_all(&base).map_err(|e| format!("{}: {e}", base.display()))?;
    let dir = tempfile::tempdir_in(&base).map_err(|e| format!("{}: {e}", base.display()))?;
    let probe = dir.path().join("probe");
    std::fs::write(&probe, b"probe").map_err(|e| e.to_string())?;
    runtime_bundle::verity::enable(&probe).map_err(|e| format!("{}: {e}", base.display()))?;
    Ok(dir)
}

#[test]
fn an_installed_runtime_is_sealed_so_a_launch_reads_none_of_it() {
    let dir = tilo_test_env::require_capability!(
        FsVerity,
        verity_dir(),
        "no fs-verity filesystem to install a runtime on"
    );
    let env = Env::in_dir(dir.path());
    let (runtime_key, runtime_identity) = identity_file(&env, "runtime-publisher");
    env.trust(&[&runtime_key]);
    let (version, bundle) = env.bundle("one", b"toolkit build one", &runtime_identity);
    let package = env.runtime_tapp("one", &bundle, &format!("tilo-runtime/{version}"));
    let (ok, out) = env.cmd(&["install", package.to_str().unwrap(), "--allow-self-signed"]);
    assert!(ok, "{out}");
    let installed = env.installed_bundle(&version);
    let policy = runtime_bundle::VerifyOptions {
        require_publisher: Some(runtime_key),
        require_signature: true,
        require_arch: None,
    };
    let (manifest, check) = runtime_bundle::verify_bundle_verity_with(&installed, &policy).unwrap();
    let files = manifest.files.len();
    assert!(
        out.contains(&format!("sealed   : {files} file(s) with fs-verity")),
        "{out}"
    );
    assert_eq!(
        check,
        runtime_bundle::PayloadCheck {
            by_verity: files,
            hashed: 0,
            hashed_bytes: 0,
        }
    );
    // Sealed means immutable: the toolkit cannot be edited in place.
    let toolkit = installed.join("lib/libruntime_dylib.so");
    assert!(
        std::fs::OpenOptions::new()
            .write(true)
            .open(&toolkit)
            .is_err()
    );
}

/// The path `bundle`'s manifest lists for the file hashing to `hash`.
fn bundle_path_of(bundle: &Path, hash: &str) -> String {
    let text =
        std::fs::read_to_string(bundle.join(runtime_bundle::BUNDLE_MANIFEST_FILENAME)).unwrap();
    let manifest: runtime_bundle::BundleManifest = toml::from_str(&text).unwrap();
    manifest
        .files
        .into_iter()
        .find(|file| file.hash == hash)
        .unwrap()
        .path
}

/// A runtime-signing identity file; returns its public key and path.
fn identity_file(env: &Env, name: &str) -> (String, PathBuf) {
    let identity = runtime_bundle::Identity::generate();
    let path = env.path(&format!("{name}.json"));
    std::fs::write(&path, identity.to_json_bytes()).unwrap();
    (identity.public_key_hex(), path)
}

#[test]
fn an_app_installs_only_beside_the_runtime_build_it_links() {
    let env = Env::new();
    let (runtime_key, runtime_identity) = identity_file(&env, "runtime-publisher");
    env.trust(&[&runtime_key]);
    let (version, bundle) = env.bundle("one", b"toolkit build one", &runtime_identity);
    let runtime = format!("tilo-runtime/{version}");

    // Phase `an_app_naming_a_missing_build_is_refused_with_the_fix`.
    let app = env.app_tapp(&runtime);
    let (ok, out) = env.cmd(&["install", app.to_str().unwrap(), "--allow-self-signed"]);
    assert!(!ok, "{out}");
    assert!(out.contains(&format!("needs runtime `{runtime}`")), "{out}");
    assert!(out.contains("app install so.tilo.linked"), "{out}");

    // Phase `the_runtime_package_installs_the_bundle_where_the_launcher_looks`.
    let package = env.runtime_tapp("one", &bundle, &runtime);
    let (ok, out) = env.cmd(&["install", package.to_str().unwrap(), "--allow-self-signed"]);
    assert!(ok, "{out}");
    let installed = env.installed_bundle(&version);
    let verified = runtime_bundle::verify_bundle(&installed).expect("installed bundle verifies");
    assert_eq!(verified.version, version);
    assert_eq!(
        std::fs::read(installed.join("lib/libruntime_dylib.so")).unwrap(),
        b"toolkit build one"
    );
    // Installing it again is a no-op, not a failure: the Store asks for a
    // runtime only when it is missing, but a race must not break an install.
    let (ok, out) = env.cmd(&["install", package.to_str().unwrap(), "--allow-self-signed"]);
    assert!(ok && out.contains("already installed"), "{out}");

    // Phase `with_its_runtime_present_the_app_installs`.
    let (ok, out) = env.cmd(&["install", app.to_str().unwrap(), "--allow-self-signed"]);
    assert!(ok, "{out}");

    // Phase `a_runtime_from_an_untrusted_publisher_is_refused`.
    let (_, other_identity) = identity_file(&env, "other-publisher");
    let (other_version, other_bundle) = env.bundle("two", b"toolkit build two", &other_identity);
    let other = env.runtime_tapp(
        "two",
        &other_bundle,
        &format!("tilo-runtime/{other_version}"),
    );
    let (ok, out) = env.cmd(&["install", other.to_str().unwrap(), "--allow-self-signed"]);
    assert!(!ok, "{out}");
    assert!(out.contains("does not trust"), "{out}");
    assert!(!env.installed_bundle(&other_version).exists());

    // Phase `a_package_claiming_another_build_is_refused`.
    let forged = format!(
        "tilo-runtime/2026.07+native.{}",
        "0".repeat(runtime_bundle::ABI_DIGEST_HEX_LEN)
    );
    let lying = env.runtime_tapp("lying", &bundle, &forged);
    let (ok, out) = env.cmd(&["install", lying.to_str().unwrap(), "--allow-self-signed"]);
    assert!(!ok, "{out}");
    assert!(out.contains("provides"), "{out}");
    let (_, forged_version) = forged.split_once('/').unwrap();
    assert!(!env.installed_bundle(forged_version).exists());
}

#[cfg(unix)]
#[test]
fn signed_engine_runtime_preserves_executable_permissions_on_install() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    let (runtime_key, runtime_identity) = identity_file(&env, "engine-publisher");
    env.trust(&[&runtime_key]);
    let host = env.path("engine-host-root");
    let loader = runtime_bundle::host_minimal_paths_for(runtime_bundle::host_arch()).unwrap()[0]
        .trim_start_matches('/');
    std::fs::create_dir_all(host.join(loader).parent().unwrap()).unwrap();
    std::fs::write(host.join(loader), b"loader fixture").unwrap();
    let engine = env.path("chromium");
    std::fs::create_dir_all(&engine).unwrap();
    let executable = engine.join("tilo-chromium-host");
    std::fs::write(&executable, b"#!/bin/sh\nprintf 'engine started\\n'\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(engine.join("resources.pak"), b"engine resource").unwrap();
    let bundle = env.path("signed-engine-bundle");
    let version = "2026.10.2-engine-test";
    runtime_bundle::build_bundle_with(
        &bundle,
        version,
        &runtime_bundle::BuildOptions {
            from: Some(&host),
            arch: Some(runtime_bundle::host_arch()),
            identity: Some(&runtime_identity),
            extra_libs: &[engine],
        },
    )
    .unwrap();
    let tapp = env.runtime_tapp_with_host(
        "engine",
        &bundle,
        &format!("tilo-runtime/{version}"),
        Some("lib/chromium/tilo-chromium-host"),
    );
    let (ok, output) = env.cmd(&[
        "install",
        tapp.to_str().unwrap(),
        "--require-publisher",
        &hex_key(&env.publisher),
    ]);
    assert!(ok, "{output}");
    let installed = env.installed_bundle(version);
    runtime_bundle::verify_bundle_with(
        &installed,
        &runtime_bundle::VerifyOptions {
            require_publisher: Some(runtime_key),
            require_signature: true,
            require_arch: Some(runtime_bundle::host_arch().to_string()),
        },
    )
    .unwrap();
    let host = installed.join("lib/chromium/tilo-chromium-host");
    assert_ne!(
        std::fs::metadata(&host).unwrap().permissions().mode() & 0o111,
        0,
        "signed engine executable lost its execute bits"
    );
    assert_eq!(
        std::fs::metadata(installed.join("lib/chromium/resources.pak"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0,
        "resource acquired execute bits"
    );
    let process = Command::new(host)
        .output()
        .expect("installed signed engine must execute");
    assert!(process.status.success());
    assert_eq!(process.stdout, b"engine started\n");
}

/// Collecting the runtime builds nothing runs on, with this file's fixtures.
#[path = "gc.rs"]
mod gc;
