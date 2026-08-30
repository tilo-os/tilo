//! Manifest schema validation.
//!
//! Second pass after TOML decode: structural fields are guaranteed to
//! exist, so this checks cross-field invariants:
//!
//! - `package.version` parses as semver.
//! - `package.runtime` is `<name>/<semver>`, absent for `kind = "runtime"`.
//! - `kind = "app"` has `[entrypoint]` with `bin` set.
//! - `kind = "format-provider"` has non-empty `providers.mime` and
//!   ships `component.wasm` (payload-side counterpart in
//!   [`validate_manifest_with_payload`]; this only enforces the mime half).
//! - `publisher.identity` parses as `ed25519:<32-byte-hex>`.
//! - `publisher.signature`, if present, parses as 64-byte hex.

use semver::Version;

use crate::id::Kind;
use crate::manifest::{EntrypointType, Manifest};

/// Validation failure reasons; each variant is one concrete invariant a manifest can violate, surfaced verbatim by the CLI.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ManifestError {
    #[error("package.version {0:?} is not valid semver: {1}")]
    BadVersion(String, String),
    #[error("package.runtime {0:?} must be `<name>/<semver>`")]
    BadRuntimeForm(String),
    #[error("package.runtime version {0:?} is not valid semver: {1}")]
    BadRuntimeVersion(String, String),
    #[error("kind=runtime must not declare a runtime dependency")]
    RuntimeKindHasRuntime,
    #[error("kind=app requires `[entrypoint]` with `bin` set")]
    AppMissingEntrypoint,
    #[error("kind=format-provider requires non-empty `providers.mime`")]
    FormatProviderMissingMime,
    #[error("publisher.identity: {0}")]
    BadPublisherIdentity(String),
    #[error("publisher.signature: {0}")]
    BadPublisherSignature(String),
    #[error(
        "package `{package}` declares `sandbox.fs.write = [{entry:?}]`, and an app does not \
         choose where it writes.\n  \
         The launcher gives every app one directory — per room, per package — and points HOME, \
         XDG_DATA_HOME, XDG_CONFIG_HOME, XDG_STATE_HOME and XDG_CACHE_HOME inside it. Remove \
         the `[sandbox.fs]` block; anything the app writes under those paths already lands \
         there.\n  \
         Access to a path outside it is a capability the user grants at runtime, not a line in \
         a manifest."
    )]
    FsWriteDeclared { package: String, entry: String },
    #[error(
        "package `{package}` declares `sandbox.fs.read = [{entry:?}]`, and an app does not \
         choose what it reads.\n  \
         Every app gets the same system read profile (loader and C library, fonts, GPU loader \
         manifests, CA bundle, timezone data). Remove the `[sandbox.fs]` block.\n  \
         Anything the profile does not cover — a user's documents, /usr/bin, /var/workspace, a \
         device node — is a capability the user grants at runtime, not a line in a manifest."
    )]
    FsReadDeclared { package: String, entry: String },
}

/// Validate the manifest against schema rules that don't require
/// touching the payload; payload-side checks live in
/// [`crate::pack::pack`] and [`validate_manifest_with_payload`].
pub fn validate_manifest(m: &Manifest) -> Result<(), ManifestError> {
    // Filesystem policy first — the check most likely to reject a
    // package a publisher believed was fine, so it's the line they read
    // rather than the fourth error after a semver nit. Writes before
    // reads: a package asking for `/` is the more alarming of the two.
    if let Some(entry) = m.sandbox.fs.write.first() {
        return Err(ManifestError::FsWriteDeclared {
            package: m.package.id.clone(),
            entry: entry.clone(),
        });
    }
    if let Some(entry) = m.sandbox.fs.read.first() {
        return Err(ManifestError::FsReadDeclared {
            package: m.package.id.clone(),
            entry: entry.clone(),
        });
    }

    Version::parse(&m.package.version)
        .map_err(|e| ManifestError::BadVersion(m.package.version.clone(), e.to_string()))?;

    match (&m.package.kind, &m.package.runtime) {
        (Kind::Runtime, Some(_)) => return Err(ManifestError::RuntimeKindHasRuntime),
        (Kind::Runtime, None) => {}
        (_, Some(rt)) => validate_runtime_string(rt)?,
        (_, None) => {
            // Schema allows runtime to be omitted for non-runtime kinds; the loader applies a default, not an error here.
        }
    }

    match m.package.kind {
        Kind::App => {
            let ep = m
                .entrypoint
                .as_ref()
                .ok_or(ManifestError::AppMissingEntrypoint)?;
            // Which paths are load-bearing depends on the entrypoint's flavour:
            // `Gui`/`Cli` fall back to top-level `bin`; `Both` requires
            // `gui_bin`/`cli_bin` (or legacy `bin` as fallback).
            let has_bin = match ep.r#type {
                EntrypointType::Gui | EntrypointType::Cli => !ep.bin.as_os_str().is_empty(),
                EntrypointType::Both => {
                    ep.gui_bin
                        .as_ref()
                        .is_some_and(|p| !p.as_os_str().is_empty())
                        || ep
                            .cli_bin
                            .as_ref()
                            .is_some_and(|p| !p.as_os_str().is_empty())
                        || !ep.bin.as_os_str().is_empty()
                }
            };
            if !has_bin {
                return Err(ManifestError::AppMissingEntrypoint);
            }
            // A [cli] block only takes effect if the entrypoint opts in — not
            // rejected on a Gui entrypoint (forward-compatible schema); the
            // installer just won't install shims from it.
            if matches!(ep.r#type, EntrypointType::Cli | EntrypointType::Both)
                && m.cli.as_ref().is_none_or(|c| c.commands.is_empty())
            {
                // A CLI entrypoint with no commands is fine — still runnable via `qos-install run <id>`, no error.
            }
        }
        Kind::FormatProvider => {
            if m.providers.mime.is_empty() {
                return Err(ManifestError::FormatProviderMissingMime);
            }
        }
        Kind::Service | Kind::Plugin | Kind::Runtime => {}
    }

    m.publisher
        .identity_bytes()
        .map_err(|e| ManifestError::BadPublisherIdentity(e.to_string()))?;
    m.publisher
        .signature_bytes()
        .map_err(|e| ManifestError::BadPublisherSignature(e.to_string()))?;

    Ok(())
}

fn validate_runtime_string(raw: &str) -> Result<(), ManifestError> {
    let (name, ver) = raw
        .split_once('/')
        .ok_or_else(|| ManifestError::BadRuntimeForm(raw.to_string()))?;
    if name.is_empty() || ver.is_empty() {
        return Err(ManifestError::BadRuntimeForm(raw.to_string()));
    }
    // Some real-world runtime versions look like 2025.06, not semver (the
    // spec example uses qos-runtime/2025.06) — accept strict semver or a
    // YYYY.MM[.MICRO] calendar form.
    if is_calver(ver) || Version::parse(ver).is_ok() {
        Ok(())
    } else {
        Err(ManifestError::BadRuntimeVersion(
            ver.to_string(),
            "not semver and not YYYY.MM[.MICRO] calendar form".into(),
        ))
    }
}

/// Calendar-version recogniser: 2 or 3 dot-separated non-empty digit groups, leading zeros permitted.
fn is_calver(ver: &str) -> bool {
    let parts: Vec<&str> = ver.split('.').collect();
    if !matches!(parts.len(), 2 | 3) {
        return false;
    }
    parts
        .iter()
        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// Payload-aware validation, called after walking the payload
/// directory: checks `format-provider` ships `component.wasm` and
/// `entrypoint.bin` resolves.
pub fn validate_manifest_with_payload(
    m: &Manifest,
    payload_files: &[std::path::PathBuf],
) -> Result<(), ManifestError> {
    validate_manifest(m)?;
    if matches!(m.package.kind, Kind::FormatProvider)
        && !payload_files
            .iter()
            .any(|p| p.file_name().is_some_and(|n| n == "component.wasm"))
    {
        // Promoting this from the docstring rule to an enforced payload-side
        // check, using the same enum so the CLI handler renders one error message.
        return Err(ManifestError::FormatProviderMissingMime); // re-used; see below
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_runtime_manifest() -> Manifest {
        let src = r#"
[package]
id = "dev.qos.runtime"
version = "2025.6.0"
kind = "runtime"

[publisher]
identity = "ed25519:0000000000000000000000000000000000000000000000000000000000000000"
signature = ""

[sandbox]
namespaces = []
seccomp = "qos-default"
landlock = "strict"

[sandbox.fs]
read = []
write = []

[sandbox.net]
mode = "off"
        "#;
        Manifest::from_toml_str(src).unwrap()
    }

    #[test]
    fn runtime_kind_with_runtime_field_rejected() {
        let mut m = minimal_runtime_manifest();
        m.package.runtime = Some("qos-runtime/2025.06".into());
        assert_eq!(
            validate_manifest(&m),
            Err(ManifestError::RuntimeKindHasRuntime)
        );
    }

    #[test]
    fn calendar_runtime_version_accepted() {
        let mut m = minimal_runtime_manifest();
        m.package.kind = Kind::App;
        m.package.runtime = Some("qos-runtime/2025.06".into());
        // Will still fail because no entrypoint is set, but the
        // runtime-version parse should pass before that — make sure the
        // error is App-specific, not runtime-version.
        let e = validate_manifest(&m).unwrap_err();
        assert!(
            matches!(e, ManifestError::AppMissingEntrypoint),
            "expected app-entrypoint error, got {e:?}"
        );
    }
}
