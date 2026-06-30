//! Test fixtures shared across the integration tests.
//!
//! The TOML here mirrors the schema in the crate docs verbatim
//! so a future spec edit is one place away.

#![allow(dead_code)] // each test file pulls a different subset

use std::path::Path;

/// The canonical example manifest used in the crate-level docs.
///
/// `<HEX>` is patched per-test with a real 64-char hex pubkey
/// before parsing.
pub const SAMPLE_APP_MANIFEST_TEMPLATE: &str = r#"
[package]
id          = "dev.qos.editor"
version     = "1.4.2"
kind        = "app"
runtime     = "qos-runtime/2025.06"

[publisher]
identity    = "ed25519:<HEX>"
signature   = ""

[[trust.attested_by]]
attestor    = "qos-curated/2025.06"
claim       = "deadbeef"

[sandbox]
namespaces  = ["user", "pid", "uts", "ipc", "mount", "net"]
seccomp     = "qos-default"
landlock    = "strict"

[sandbox.fs]
read  = ["~/.local/share/dev.qos.editor", "/var/workspace/<scope=current>"]
write = ["~/.local/share/dev.qos.editor"]

[sandbox.net]
mode = "off"

[sandbox.portals]
file-chooser = "via-host"

[sandbox.services]
collab      = "required@1"
qos-fs      = "required@1"
spellcheck  = "optional@1"

[entrypoint]
bin             = "bin/editor"
splash-segments = ["bin/editor[0x0:0x300000]", "share/splash.png"]
splash-budget   = 100

[providers]
mime = ["application/vnd.oasis.opendocument.text"]
ext  = ["odt"]
"#;

/// Plug a 32-byte pubkey hex into the template.
pub fn render_template(template: &str, identity_hex: &str) -> String {
    template.replace("<HEX>", identity_hex)
}

/// Lay out a small valid app payload under `dir`:
///
/// ```text
/// bin/editor          (~10 bytes)
/// share/splash.png    (~6 bytes)
/// ```
pub fn write_app_payload(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir.join("bin"))?;
    std::fs::create_dir_all(dir.join("share"))?;
    std::fs::write(dir.join("bin/editor"), b"#!/notreal")?;
    std::fs::write(dir.join("share/splash.png"), b"PNG\x00\x00\x00")?;
    Ok(())
}

/// Lay out a valid format-provider payload (must contain
/// `component.wasm` somewhere — we put it at the root).
pub fn write_format_provider_payload(dir: &Path) -> std::io::Result<()> {
    std::fs::write(dir.join("component.wasm"), b"\x00asm")?;
    Ok(())
}
