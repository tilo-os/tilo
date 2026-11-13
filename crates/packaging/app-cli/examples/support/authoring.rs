//! Authoring the `.tapp` a local test registry serves.

use std::path::{Path, PathBuf};

use tilo_app_package::Manifest;
use tilo_app_package::pack::{SigningIdentity, pack};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestSource {
    Package(PathBuf),
    /// No manifest found next to the payload; a minimal one was
    /// synthesised — the app can install and start, but declares no
    /// services so it cannot collaborate.
    Synthesised,
}

pub struct Authored {
    pub tapp: Vec<u8>,
    /// The version to advertise, read off the manifest rather than chosen, so the registry entry and package agree.
    pub version: String,
    pub source: ManifestSource,
}

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

    manifest.package.id = package_id.to_string();
    manifest.package.name = Some(name.to_string());
    manifest.publisher.identity = format!("ed25519:{pubhex}");
    manifest.publisher.signature = String::new();
    manifest.trust.attested_by.clear();

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
    let packed =
        pack(&payload, &manifest, identity).map_err(|e| AuthorError::Pack(e.to_string()))?;
    let tapp = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed))
        .map_err(|e| AuthorError::WriteTapp(e.to_string()))?;
    Ok(Authored {
        tapp,
        version,
        source,
    })
}

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
