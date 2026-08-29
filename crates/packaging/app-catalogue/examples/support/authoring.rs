//! Authoring the `.qpkg` a local test registry serves.
//!
//! Shared by the `serve-test-registry` example and the test that holds
//! it to its contract. Lives under `examples/support/` (a dev-only
//! dependency set) so the packer doesn't ship inside `qos-catalogue`.
//!
//! The manifest decides real launch behaviour — `[sandbox.services]`
//! (which service descriptors the launcher hands the app; no `collab`
//! entry means collaboration silently doesn't work), the runtime
//! bundle, sandbox namespaces, the entrypoint binary. A registry
//! serving a *synthesised* manifest therefore serves a different app
//! than the one it points at, with no visible symptom beyond a peer's
//! caret never arriving.
//!
//! So the rule: **publish the manifest the payload was built with**
//! (a payload root with `manifest.toml` beside `bin/`, as
//! `qos-install` writes). Synthesising a minimal manifest is the
//! fallback for a payload with none at all, not the default.

use std::path::{Path, PathBuf};

use pkg::pack::{SigningIdentity, pack};
use pkg::{Manifest, MemoryBlobStore};

/// Where the manifest that got packed came from, printed so a log says which app was actually published.
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
    pub qpkg: Vec<u8>,
    /// The version to advertise, read off the manifest rather than chosen, so the registry entry and package agree.
    pub version: String,
    pub source: ManifestSource,
}

/// The `manifest.toml` belonging to `payload_bin`, if part of an installed package.
///
/// `qos-install` unpacks as `<payload-root>/manifest.toml` beside
/// `<payload-root>/bin/<binary>`, so the manifest is the binary's
/// grandparent's `manifest.toml`; two levels up is also checked, for
/// a nested entrypoint like `libexec/helpers/thing`.
pub fn manifest_beside(payload_bin: &Path) -> Option<PathBuf> {
    let mut dir = payload_bin.parent();
    for _ in 0..3 {
        let Some(d) = dir else { return None };
        let candidate = d.join("manifest.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = d.parent();
    }
    None
}

/// Pack `payload_bin` into a real, signed `.qpkg`.
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
) -> Authored {
    let pubhex = hex::encode(identity.public_key_bytes());
    let found = manifest_path
        .map(Path::to_path_buf)
        .or_else(|| manifest_beside(payload_bin));

    let (mut manifest, source) = match &found {
        Some(path) => {
            let toml = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("read the package manifest at {path:?}: {e}"));
            let manifest = Manifest::from_toml_str(&toml)
                .unwrap_or_else(|e| panic!("parse the package manifest at {path:?}: {e}"));
            (manifest, ManifestSource::Package(path.clone()))
        }
        None => (
            Manifest::from_toml_str(&synthesised_toml(package_id, name, &pubhex))
                .expect("the synthesised manifest parses"),
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

    let dir = tempfile::tempdir().expect("tempdir");
    let payload = dir.path().join("payload");
    let dest = payload.join(&entry_rel);
    std::fs::create_dir_all(dest.parent().expect("the entrypoint has a directory"))
        .expect("create the payload tree");
    std::fs::copy(payload_bin, &dest)
        .unwrap_or_else(|e| panic!("copy {payload_bin:?} into the payload at {dest:?}: {e}"));
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dest).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dest, perms).unwrap();
    }

    let version = manifest.package.version.clone();
    let mut store = MemoryBlobStore::new();
    let id = pack(&payload, &manifest, identity, &mut store).expect("pack");
    let qpkg = app_registry::qpkg::write_bytes(id, &store).expect("write qpkg bytes");
    Authored {
        qpkg,
        version,
        source,
    }
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
seccomp = "qos-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/app"
type = "gui"
"#
    )
}
