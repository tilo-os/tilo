//! The runtime builds a test's phone carries in itself, declared as the APK's signed manifest
//! declares them: the exact Tilo runtime build the app links and the tilolang runtime it runs
//! tilolang apps on. It lives beside the phone's own tests, not in their shared support: the
//! session's tests include that support, and they carry no package of an app's.

/// The exact Tilo runtime build a test's phone links: what packaging stamps into a packaged
/// phone's manifest, its digest the build's own.
const TILO_RUNTIME: &str = "tilo-runtime/2026.07+native.0123456789abcdef0123456789abcdef";

/// Declare the runtime builds this test's phone carries in itself. A phone's manifest is stamped
/// by packaging; this build has no APK, so the test states the same manifest and signs it.
pub fn declare() {
    let identity = tilo_app_package::pack::SigningIdentity::from_signing_key(
        ed25519_dalek::SigningKey::from_bytes(&[7; 32]),
    );
    let mut manifest = tilo_app_package::Manifest::from_toml_str(&format!(
        r#"
[package]
id = "so.tilo.phone"
name = "Tilo"
category = "System"
version = "0.1.0"
kind = "app"
runtime = "{tapp}"
optional-runtimes = ["{tilolang}"]

[package.isa]
x86_64 = "v3"
aarch64 = "aarch64"

[publisher]
identity = "ed25519:{publisher}"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "host"

[entrypoint]
bin = "bin/tilo-phone"
type = "gui"
"#,
        tapp = TILO_RUNTIME,
        tilolang = tilolang_tilo::runtime(),
        publisher = hex::encode(identity.public_key_bytes()),
    ))
    .expect("the test's phone's manifest reads");
    manifest
        .sign(&identity)
        .expect("the test's phone signs its own manifest");
    manifest
        .verify_signature()
        .expect("the test's phone's manifest verifies");
    tilo_runtime::device::packages::carry_runtimes(&manifest)
        .expect("the phone declares the runtimes it carries");
}
