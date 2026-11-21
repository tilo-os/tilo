use super::*;
use tilo_app_package::{Manifest, SigningIdentity, pack, tapp};

const APP: &str = "so.tilo.lang.fixture";
const WORKSPACE: &str = "6f1c2a52-8a2b-4c39-9d6b-1f3b0e0c7a10";
/// What the package's module holds: found again in the `.tapp` to tamper with it.
const MODULE: &[u8] = b"\0asm the hosted fixture's compiled module";

/// A tilolang package `id` as `tilolang build` signs it: its entrypoint runs the tilolang runtime,
/// its payload is the module that runtime runs.
fn tilolang_package(id: &str, identity: &SigningIdentity) -> Vec<u8> {
    let scratch = tempfile::tempdir().unwrap();
    let payload = scratch.path().join("payload");
    std::fs::create_dir_all(&payload).unwrap();
    std::fs::write(payload.join("app.wasm"), MODULE).unwrap();
    std::fs::write(payload.join("app.ids"), b"# fixture ids\n").unwrap();
    let manifest = Manifest::from_toml_str(&format!(
        r#"
[package]
id = "{id}"
kind = "app"
version = "0.1.0"
runtime = "tilo-runtime/2026.11"
optional-runtimes = ["tilolang/0.1.0+tilolang"]

[publisher]
identity = "ed25519:{}"
signature = ""

[sandbox]
namespaces = ["user", "pid", "mount"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
runtime = "tilolang/0.1.0+tilolang"
bin = "bin/tilolang-app"
type = "gui"
"#,
        hex::encode(identity.public_key_bytes()),
    ))
    .unwrap();
    let packed = pack(&payload, &manifest, identity).unwrap();
    let source: &dyn tilo_app_package::BlobSource = &packed;
    tapp::to_bytes(&packed.meta, Some(source)).unwrap()
}

/// A server's hosted packages kept in a fresh directory, and a publisher's key.
fn hosted() -> (tempfile::TempDir, HostedPackages, SigningIdentity) {
    let root = tempfile::tempdir().unwrap();
    let packages = HostedPackages::new(root.path().to_path_buf());
    (root, packages, SigningIdentity::generate())
}

tilo_test_env::scenario! {
    fn a_hosted_workspace_runs_only_the_package_its_governance_installed_as_verified;

    phase a_governed_package_is_installed_and_verified_as_a_launch_verifies_it;
    {
        // The package the workspace installed runs as the tilolang app its signature vouches for,
        // its module read from the verified payload.
        let (root, packages, identity) = hosted();
        let verified = packages
            .verified(WORKSPACE, APP, &tilolang_package(APP, &identity))
            .expect("the governed package verifies");
        assert_eq!(verified.runtime, "tilolang/0.1.0+tilolang");
        assert_eq!(verified.app_runtime, "tilo-runtime/2026.11");
        assert_eq!(verified.publisher, identity.public_key_bytes());
        assert_eq!(verified.module.wasm, MODULE);
        assert!(
            root.path().join(WORKSPACE).is_dir(),
            "the workspace's packages are kept in a store of its own"
        );
    }

    phase a_tampered_package_is_refused;
    {
        // A byte of the module changed after signing: the package no longer holds together, and
        // nothing of it runs.
        let (_root, packages, identity) = hosted();
        let mut tampered = tilolang_package(APP, &identity);
        let at = tampered
            .windows(MODULE.len())
            .position(|window| window == MODULE)
            .expect("the package carries its module");
        tampered[at + 1] ^= 0x20;
        let refused = packages
            .verified(WORKSPACE, APP, &tampered)
            .expect_err("a tampered package is refused");
        assert!(refused.contains("was refused"), "{refused}");
    }

    phase a_package_that_is_not_the_installed_app_is_refused;
    {
        let (_root, packages, identity) = hosted();
        let other = tilolang_package("so.tilo.lang.other", &identity);
        let refused = packages
            .verified(WORKSPACE, APP, &other)
            .expect_err("another app's package does not run as this one");
        assert!(refused.contains("not so.tilo.lang.fixture"), "{refused}");
    }

    phase a_workspace_is_named_by_its_id_only;
    {
        let (_root, packages, identity) = hosted();
        let refused = packages
            .verified("../elsewhere", APP, &tilolang_package(APP, &identity))
            .expect_err("a path is no workspace");
        assert!(refused.contains("no workspace id"), "{refused}");
    }
}

tilo_test_env::scenario! {
    fn a_server_runs_apps_only_through_its_launcher;

    phase a_server_without_a_launcher_runs_no_app;
    {
        let root = tempfile::tempdir().unwrap();
        let missing = [root.path().join("tilo-launcher")];
        let refused = tilo_launcher(&missing).expect_err("no launcher, no app");
        assert!(refused.contains("no tilo-launcher is installed"), "{refused}");
        assert!(
            refused.contains(&missing[0].display().to_string()),
            "it says where it looked: {refused}"
        );
    }

    phase the_first_installed_launcher_is_the_one;
    {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("tilo-launcher");
        let installed = root.path().join("installed");
        std::fs::write(&installed, b"").unwrap();
        let candidates = [missing, installed.clone()];
        assert_eq!(tilo_launcher(&candidates).unwrap(), installed);
    }
}
