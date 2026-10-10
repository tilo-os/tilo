//! Test fixtures shared across the integration tests.
//!
//! The TOML here mirrors the schema in the crate docs verbatim
//! so a future spec edit is one place away.

#![allow(dead_code)] // each test file pulls a different subset
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::Path;

/// The canonical example manifest used in the crate-level docs.
///
/// `<HEX>` is patched per-test with a real 64-char hex pubkey
/// before parsing.
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

/// A CLI-flavour app manifest — declares an `[entrypoint]` with
/// `type = "cli"` plus a `[cli]` block that publishes a single
/// command on PATH.
pub const CLI_APP_MANIFEST_TEMPLATE: &str = r#"
[package]
id          = "so.tilo.notes-cli"
version     = "0.1.0"
kind        = "app"

[publisher]
identity    = "ed25519:<HEX>"
signature   = ""

[sandbox]
namespaces  = ["user", "pid", "uts", "ipc", "mount", "net"]
seccomp     = "tilo-default"
landlock    = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin  = "bin/notes"
type = "cli"

[cli]
commands = ["notes"]
"#;

/// A `type = "both"` manifest — ships separate `gui-bin` and
/// `cli-bin` under the payload.
pub const BOTH_APP_MANIFEST_TEMPLATE: &str = r#"
[package]
id          = "so.tilo.notes"
version     = "0.2.0"
kind        = "app"

[publisher]
identity    = "ed25519:<HEX>"
signature   = ""

[sandbox]
namespaces  = ["user", "pid", "uts", "ipc", "mount", "net"]
seccomp     = "tilo-default"
landlock    = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
type    = "both"
gui-bin = "bin/notes-gui"
cli-bin = "bin/notes"

[cli]
commands = ["notes"]
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
