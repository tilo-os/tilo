//! Runtime packages: the runtime build a thin app links travels as a
//! `kind = "runtime"` package, and installing it installs the signed bundle
//! where the launcher looks. An app naming a build this device lacks is
//! refused until that build is installed, and a runtime package is held to
//! the launcher's own bundle checks.

use std::path::{Path, PathBuf};
use std::process::Command;

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
