//! Authoring the `.tapp` a local test registry serves.
//!
//! Shared by the `serve-test-registry` example and the test that holds
//! it to its contract. Lives under `examples/support/` (a dev-only
//! dependency set) so the packer doesn't ship inside `app`.
//!
//! The manifest decides launch behaviour — `[sandbox.services]`
//! (which service descriptors the launcher hands the app; no `collab`
//! entry means collaboration silently doesn't work), the runtime
//! bundle, sandbox namespaces, the entrypoint binary. A registry
//! serving a *synthesised* manifest therefore serves a different app
//! than the one it points at, with no visible symptom beyond a peer's
//! caret never arriving.
//!
//! So the rule: **publish the manifest the payload was built with**
//! (a payload root with `manifest.toml` beside `bin/`, as
//! `tilo-install` writes). Synthesising a minimal manifest is the
//! fallback for a payload with none at all, not the default.

use std::path::{Path, PathBuf};

use tilo_app_package::pack::{SigningIdentity, pack};
use tilo_app_package::{Manifest, MemoryBlobStore};

/// Where the manifest that got packed came from, so a log identifies the published app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestSource {
    /// Taken from the package the payload belongs to.
    Package(PathBuf),
    /// No manifest found next to the payload; a minimal one was
    /// synthesised — the app can install and start, but declares no
    /// services so it cannot collaborate.
    Synthesised,
}

/// One authored package: the bytes to serve, plus what they say about themselves.
pub struct Authored {
    pub tapp: Vec<u8>,
    /// The version to advertise, read off the manifest rather than chosen, so the registry entry and package agree.
    pub version: String,
    pub source: ManifestSource,
}

/// Failure authoring a `.tapp` for the local test registry — every
/// variant names the step that failed.
#[derive(Debug, thiserror::Error)]
pub enum AuthorError {
    #[error("read the package manifest at {}: {detail}", path.display())]
    ReadManifest { path: PathBuf, detail: String },
    #[error("parse the package manifest at {}: {detail}", path.display())]
    ParseManifest { path: PathBuf, detail: String },
    #[error("the synthesised manifest failed to parse: {0}")]
    SynthesisedManifest(String),
    #[error("create a staging tempdir: {0}")]
    TempDir(String),
    #[error("the entrypoint {} has no parent directory", .0.display())]
    EntrypointHasNoParent(PathBuf),
    #[error("create the payload tree: {0}")]
    CreatePayloadTree(String),
    #[error("copy {} into the payload at {}: {detail}", from.display(), to.display())]
    CopyPayload {
        from: PathBuf,
        to: PathBuf,
        detail: String,
    },
    #[error("read metadata for {}: {detail}", path.display())]
    ReadMetadata { path: PathBuf, detail: String },
    #[error("set permissions on {}: {detail}", path.display())]
    SetPermissions { path: PathBuf, detail: String },
    #[error("pack the payload: {0}")]
    Pack(String),
    #[error("write tapp bytes: {0}")]
    WriteTapp(String),
}

/// The `manifest.toml` belonging to `payload_bin`, if part of an installed package.
///
/// `tilo-install` unpacks as `<payload-root>/manifest.toml` beside
/// `<payload-root>/bin/<binary>`, so the manifest is the binary's
/// grandparent's `manifest.toml`; two levels up is also checked, for
/// a nested entrypoint like `libexec/helpers/thing`.
pub fn manifest_beside(payload_bin: &Path) -> Option<PathBuf> {
    let mut dir = payload_bin.parent();
    for _ in 0..3 {
        let d = dir?;
        let candidate = d.join("manifest.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = d.parent();
    }
    None
}

/// Pack `payload_bin` into a signed `.tapp`.
///
/// `manifest_path` overrides the search; `None` looks beside the
/// payload via [`manifest_beside`] and synthesises only if that finds nothing.
///
/// Whatever manifest is used is re-stamped with `identity` as
/// publisher, since it's signed here and now — the original build's
/// publisher would fail `pack`'s own identity check. Third-party
/// attestations are dropped too: a local test registry can't mint
/// one, and an empty-signature row only warns at install.
///
/// # Errors
///
/// Returns an error if the manifest can't be read or parsed, the
/// staging tempdir or payload tree can't be built, the payload can't
/// be copied/chmod'd into it, or packing/serialising the `.tapp` fails.
pub fn author(
    package_id: &str,
    name: &str,
    payload_bin: &Path,
    manifest_path: Option<&Path>,
    identity: &SigningIdentity,
) -> Result<Authored, AuthorError> {
    let pubhex = hex::encode(identity.public_key_bytes());
    let found = manifest_path
        .map(Path::to_path_buf)
        .or_else(|| manifest_beside(payload_bin));

    let (mut manifest, source) = match &found {
        Some(path) => {
            let toml = std::fs::read_to_string(path).map_err(|e| AuthorError::ReadManifest {
                path: path.clone(),
                detail: e.to_string(),
            })?;
            let manifest =
                Manifest::from_toml_str(&toml).map_err(|e| AuthorError::ParseManifest {
                    path: path.clone(),
                    detail: e.to_string(),
                })?;
            (manifest, ManifestSource::Package(path.clone()))
        }
        None => (
            Manifest::from_toml_str(&synthesised_toml(package_id, name, &pubhex))
                .map_err(|e| AuthorError::SynthesisedManifest(e.to_string()))?,
            ManifestSource::Synthesised,
        ),
    };

    // The registry advertises this package under the id/name the caller
    // gave, so the manifest must agree or the installed app answers to a
    // different name than the row that installed it.
    manifest.package.id = package_id.to_string();
    manifest.package.name = Some(name.to_string());
    manifest.publisher.identity = format!("ed25519:{pubhex}");
    manifest.publisher.signature = String::new();
    manifest.trust.attested_by.clear();

    // The entrypoint the manifest declares is where the payload has to
    // land — otherwise the package installs and then fails to exec.
    let entry_rel: PathBuf = manifest
        .entrypoint
        .as_ref()
        .map(|e| e.bin.clone())
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("bin/app"));

    let dir = tempfile::tempdir().map_err(|e| AuthorError::TempDir(e.to_string()))?;
    let payload = dir.path().join("payload");
    let dest = payload.join(&entry_rel);
    let dest_parent = dest
        .parent()
        .ok_or_else(|| AuthorError::EntrypointHasNoParent(entry_rel.clone()))?;
    std::fs::create_dir_all(dest_parent)
        .map_err(|e| AuthorError::CreatePayloadTree(e.to_string()))?;
    std::fs::copy(payload_bin, &dest).map_err(|e| AuthorError::CopyPayload {
        from: payload_bin.to_path_buf(),
        to: dest.clone(),
        detail: e.to_string(),
    })?;
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dest)
            .map_err(|e| AuthorError::ReadMetadata {
                path: dest.clone(),
                detail: e.to_string(),
            })?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dest, perms).map_err(|e| AuthorError::SetPermissions {
            path: dest.clone(),
            detail: e.to_string(),
        })?;
    }

    let version = manifest.package.version.clone();
    let mut store = MemoryBlobStore::new();
    let id = pack(&payload, &manifest, identity, &mut store)
        .map_err(|e| AuthorError::Pack(e.to_string()))?;
    let tapp = app_registry::tapp::write_bytes(id, &store)
        .map_err(|e| AuthorError::WriteTapp(e.to_string()))?;
    Ok(Authored {
        tapp,
        version,
        source,
    })
}

/// The minimum that installs and starts, for a payload with no
/// package. Declares no services on purpose — pretending otherwise
/// would hand the launcher a descriptor the app was never built to read.
fn synthesised_toml(package_id: &str, name: &str, pubhex: &str) -> String {
    format!(
        r#"
[package]
id = "{package_id}"
version = "1.4.0"
kind = "app"
name = "{name}"
category = "Office"

[publisher]
identity = "ed25519:{pubhex}"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/app"
type = "gui"
"#
    )
}
