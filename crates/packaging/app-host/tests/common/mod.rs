//! Test helper: build a .tapp-tar in memory so integration tests and
//! benchmarks can drive `install::install_tapp` with a real signed package.
//!
//! Mirrors `tilo-app-package-cli::write_tapp`, kept local so this test crate does
//! not depend on the `tilo-app-package-cli` binary crate.

#![allow(dead_code)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_app_package::{Manifest, PackageId, SigningIdentity, pack};

/// Same TOML shape as the pkg crate's fixtures.
pub const SAMPLE_APP_MANIFEST_TEMPLATE: &str = r#"
[package]
id          = "so.tilo.editor"
version     = "1.4.2"
kind        = "app"
runtime     = "tilo-runtime/2025.06"

[publisher]
identity    = "ed25519:<HEX>"
signature   = ""

[[trust.attested_by]]
attestor    = "tilo-curated/2025.06"
claim       = "deadbeef"

[sandbox]
namespaces  = ["user", "pid", "uts", "ipc", "mount", "net"]
seccomp     = "tilo-default"
landlock    = "strict"

[sandbox.net]
mode = "off"

[sandbox.portals]
file-chooser = "via-host"

[sandbox.services]
collab      = "required@1"
tilo-fs      = "required@1"
spellcheck  = "optional@1"

[entrypoint]
bin             = "bin/editor"
splash-segments = ["bin/editor[0x0:0x300000]", "share/splash.png"]
splash-budget   = 100

[providers]
mime = ["application/vnd.oasis.opendocument.text"]
ext  = ["odt"]
"#;

pub fn render_template(template: &str, identity_hex: &str) -> String {
    template.replace("<HEX>", identity_hex)
}

pub fn write_app_payload(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir.join("bin"))?;
    std::fs::create_dir_all(dir.join("share"))?;
    std::fs::write(dir.join("bin/editor"), b"#!/notreal")?;
    std::fs::write(dir.join("share/splash.png"), b"PNG\x00\x00\x00")?;
    Ok(())
}

pub fn write_app_payload_with_bulk(dir: &std::path::Path, bulk_kib: usize) -> std::io::Result<()> {
    write_app_payload(dir)?;
    // Make the payload measurably-sized without stalling on IO —
    // fill a single "data" blob deterministically. Content is
    // non-repeating (so blob-store dedup can't skip the bytes).
    let bulk_bytes = bulk_kib.saturating_mul(1024);
    let mut bulk = Vec::with_capacity(bulk_bytes);
    for i in 0..bulk_bytes {
        // `i % 256` is always in 0..256, so this never falls back.
        let low_byte = u8::try_from(i % 256).unwrap_or(0);
        bulk.push(low_byte.wrapping_mul(31));
    }
    std::fs::write(dir.join("share/data.bin"), bulk)?;
    Ok(())
}

pub fn build_tapp(payload_kib: usize) -> (Vec<u8>, PackageId) {
    build_tapp_for("so.tilo.editor", payload_kib)
}

/// A package of app `app_id` whose payload carries a unique `payload_kib`
/// KiB blob (seeded by the app id, so two apps share no bulk bytes).
pub fn build_tapp_for(app_id: &str, payload_kib: usize) -> (Vec<u8>, PackageId) {
    let mut rng = UnwrapErr(SysRng);
    let sk = SigningKey::generate(&mut rng);
    let pub_hex = hex::encode(sk.verifying_key().to_bytes());
    let src = render_template(SAMPLE_APP_MANIFEST_TEMPLATE, &pub_hex)
        .replace("\"so.tilo.editor\"", &format!("{app_id:?}"));
    let manifest = Manifest::from_toml_str(&src).expect("decode manifest");

    let payload_dir = tempfile::tempdir().unwrap();
    write_app_payload_with_bulk(payload_dir.path(), payload_kib).unwrap();
    std::fs::write(payload_dir.path().join("share/owner.txt"), app_id).unwrap();
    let seed = app_id.bytes().fold(0u8, u8::wrapping_add);
    let bulk: Vec<u8> = std::fs::read(payload_dir.path().join("share/data.bin"))
        .unwrap()
        .into_iter()
        .map(|byte| byte ^ seed)
        .collect();
    std::fs::write(payload_dir.path().join("share/data.bin"), bulk).unwrap();

    let identity = SigningIdentity::from_signing_key(sk);
    let packed = pack(payload_dir.path(), &manifest, &identity).expect("pack");
    let bytes = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).expect("write tapp");
    (bytes, packed.id())
}
