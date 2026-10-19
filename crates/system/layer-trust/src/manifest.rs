//! The signed layer manifest.
//!
//! ```json
//! {
//!   "type": "tilo.signed-layer",
//!   "schema": 1,
//!   "name": "<extension name; tilo-vm uses the content hash>",
//!   "component": "shell",
//!   "revision": "<source commit, +dirty:<hash> for uncommitted changes, or local>",
//!   "content_sha256": "<64 hex>",
//!   "generation": 107,
//!   "base_image_digest": "sha256:<64 hex>" | null,
//!   "extension_release": "ID=_any\nTILO_VM_COMPONENT=shell\n",
//!   "entries": [
//!     {"kind": "file", "path": "usr/bin/shell", "sha256": "<64 hex>", "verity": "<64 hex>" | null, "executable": true},
//!     {"kind": "link", "path": "usr/lib/libx.so", "target": "libx.so.1"}
//!   ]
//! }
//! ```
//!
//! `content_sha256` is the SHA-256 over the entries in their listed order,
//! each as its tag (`x` executable file, `f` file, `l` link), its path and its
//! content (the file's SHA-256 or the link's target), every field followed by
//! a NUL byte. That is tilo-vm's layer identity, so for a tilo-vm layer the
//! name, the content hash and the directory the layer lives in are one value,
//! and a signer that built the layer can check the manifest describes it
//! without reading the payload again.
//!
//! `verity` is the file's fs-verity digest (SHA-256, 4 KiB blocks, no salt):
//! the activation trusts a file whose kernel-measured digest equals it without
//! reading it, and hashes any other file. `generation` is the slot generation
//! the layer was endorsed for and `base_image_digest` the image it was built
//! on; both are records, not conditions.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{is_hex, is_layer_name};

pub const MANIFEST_TYPE: &str = "tilo.signed-layer";
pub const SCHEMA: u32 = 1;
/// A manifest larger than this is refused before it is parsed.
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;
/// A path longer than this is not a layer path.
const MAX_PATH: usize = 4096;
/// The top-level trees systemd-sysext merges.
pub const TREES: [&str; 2] = ["usr", "opt"];

/// What a layer carries at one path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Entry {
    File {
        path: String,
        sha256: String,
        verity: Option<String>,
        executable: bool,
    },
    Link {
        path: String,
        target: String,
    },
}

impl Entry {
    pub fn path(&self) -> &str {
        match self {
            Self::File { path, .. } | Self::Link { path, .. } => path,
        }
    }
}

/// A signed layer's manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayerManifest {
    #[serde(rename = "type")]
    pub kind: String,
    pub schema: u32,
    pub name: String,
    pub component: String,
    pub revision: String,
    pub content_sha256: String,
    pub generation: u64,
    pub base_image_digest: Option<String>,
    pub extension_release: String,
    pub entries: Vec<Entry>,
}

/// Why a manifest is not one this verifier accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestError(pub String);

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ManifestError {}

fn fail<T>(message: impl Into<String>) -> Result<T, ManifestError> {
    Err(ManifestError(message.into()))
}

/// Whether `path` is a relative, normal path inside one of [`TREES`].
fn is_layer_path(path: &str) -> bool {
    let mut parts = path.split('/');
    parts.next().is_some_and(|top| TREES.contains(&top))
        && path.len() <= MAX_PATH
        && !path.contains('\0')
        && path.contains('/')
        && parts.all(|part| !part.is_empty() && part != "." && part != "..")
}

/// `content_sha256` of `entries` in this order.
pub fn content_sha256(entries: &[Entry]) -> String {
    let mut hasher = Sha256::new();
    for entry in entries {
        let (tag, path, content) = match entry {
            Entry::File {
                path,
                sha256,
                executable,
                ..
            } => (if *executable { "x" } else { "f" }, path, sha256),
            Entry::Link { path, target } => ("l", path, target),
        };
        for part in [tag, path.as_str(), content.as_str()] {
            hasher.update(part.as_bytes());
            hasher.update([0]);
        }
    }
    hex::encode(hasher.finalize())
}

/// The `extension-release` tilo-vm writes into its layers: no OS pin (the
/// layer is checked against the image by tilo-vmd), and the component.
pub fn tilo_vm_extension_release(component: &str) -> String {
    format!("ID=_any\nTILO_VM_COMPONENT={component}\n")
}

impl LayerManifest {
    /// Parse and check manifest bytes.
    ///
    /// # Errors
    /// Malformed JSON, an unknown field, or anything [`Self::check`] refuses.
    pub fn parse(bytes: &[u8]) -> Result<Self, ManifestError> {
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|error| ManifestError(format!("the manifest is malformed: {error}")))?;
        manifest.check()?;
        Ok(manifest)
    }

    /// The bytes a signer signs and the activation reads.
    ///
    /// # Errors
    /// Serialisation failed.
    pub fn to_bytes(&self) -> Result<Vec<u8>, ManifestError> {
        let mut bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| ManifestError(format!("cannot write the manifest: {error}")))?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// Everything that can be checked without the payload.
    ///
    /// # Errors
    /// The first thing that is wrong.
    pub fn check(&self) -> Result<(), ManifestError> {
        if self.kind != MANIFEST_TYPE {
            return fail(format!(
                "the manifest is a {:?}, not a {MANIFEST_TYPE}",
                self.kind
            ));
        }
        if self.schema != SCHEMA {
            return fail(format!("manifest schema {} is not {SCHEMA}", self.schema));
        }
        if !is_layer_name(&self.name) {
            return fail(format!("{:?} is not a layer name", self.name));
        }
        if self.component.is_empty()
            || self.component.len() > 128
            || self.component.chars().any(char::is_control)
        {
            return fail("the component name is empty, too long or has control characters");
        }
        if self.revision.len() > 256 || self.revision.chars().any(char::is_control) {
            return fail("the revision is too long or has control characters");
        }
        if let Some(digest) = &self.base_image_digest
            && !digest
                .strip_prefix("sha256:")
                .is_some_and(|hex| is_hex(hex, 64))
        {
            return fail(format!("{digest:?} is not a sha256:<64 hex> image digest"));
        }
        if self.extension_release.len() > 4096 || self.extension_release.contains('\0') {
            return fail("the extension-release text is too long or has a NUL");
        }
        let mut seen = std::collections::BTreeSet::new();
        for entry in &self.entries {
            let path = entry.path();
            if !is_layer_path(path) {
                return fail(format!("{path:?} is not a path inside usr/ or opt/"));
            }
            if path == "usr/lib/extension-release.d"
                || path.starts_with("usr/lib/extension-release.d/")
            {
                return fail(format!("{path} is the activation's own extension-release"));
            }
            if !seen.insert(path) {
                return fail(format!("{path} is listed twice"));
            }
            match entry {
                Entry::File { sha256, verity, .. } => {
                    if !is_hex(sha256, 64) || verity.as_deref().is_some_and(|v| !is_hex(v, 64)) {
                        return fail(format!("{path}: a digest is not 64 lower-case hex"));
                    }
                }
                Entry::Link { target, .. } => {
                    if target.is_empty() || target.len() > MAX_PATH || target.contains('\0') {
                        return fail(format!("{path}: the link target is empty or too long"));
                    }
                }
            }
        }
        if !is_hex(&self.content_sha256, 64) {
            return fail("content_sha256 is not 64 lower-case hex");
        }
        let computed = content_sha256(&self.entries);
        if computed != self.content_sha256 {
            return fail(format!(
                "the entries hash to {computed}, not to the content_sha256 {}",
                self.content_sha256
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> LayerManifest {
        let entries = vec![
            Entry::File {
                path: "usr/bin/shell".into(),
                sha256: "a".repeat(64),
                verity: Some("b".repeat(64)),
                executable: true,
            },
            Entry::Link {
                path: "usr/lib/libx.so".into(),
                target: "libx.so.1".into(),
            },
        ];
        let content = content_sha256(&entries);
        LayerManifest {
            kind: MANIFEST_TYPE.into(),
            schema: SCHEMA,
            name: content.clone(),
            component: "shell".into(),
            revision: "1a2b3c".into(),
            content_sha256: content,
            generation: 3,
            base_image_digest: Some(format!("sha256:{}", "c".repeat(64))),
            extension_release: tilo_vm_extension_release("shell"),
            entries,
        }
    }

    #[test]
    fn manifests_round_trip_and_bind_their_entries() {
        let manifest = sample();
        let bytes = manifest.to_bytes().unwrap();
        assert_eq!(LayerManifest::parse(&bytes).unwrap(), manifest);

        let mut changed = manifest.clone();
        if let Entry::File { executable, .. } = &mut changed.entries[0] {
            *executable = false;
        }
        assert!(changed.check().unwrap_err().0.contains("hash to"));

        for (field, value) in [
            ("type", "\"tilo.system-extension.sysext-manifest\""),
            ("schema", "2"),
            ("name", "\"../etc\""),
            ("base_image_digest", "\"latest\""),
        ] {
            let mut json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            json[field] = serde_json::from_str(value).unwrap();
            let bytes = serde_json::to_vec(&json).unwrap();
            assert!(LayerManifest::parse(&bytes).is_err(), "{field} = {value}");
        }
        let mut json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        json["extra"] = serde_json::json!(1);
        assert!(LayerManifest::parse(&serde_json::to_vec(&json).unwrap()).is_err());
    }

    #[test]
    fn entries_stay_inside_the_merged_trees() {
        for bad in [
            "etc/passwd",
            "usr",
            "/usr/bin/x",
            "usr/../etc/x",
            "usr//x",
            "usr/lib/extension-release.d/extension-release.x",
        ] {
            let mut manifest = sample();
            manifest.entries = vec![Entry::Link {
                path: bad.into(),
                target: "x".into(),
            }];
            manifest.content_sha256 = content_sha256(&manifest.entries);
            assert!(manifest.check().is_err(), "{bad}");
        }
        let mut twice = sample();
        twice.entries.push(twice.entries[0].clone());
        twice.content_sha256 = content_sha256(&twice.entries);
        assert!(twice.check().unwrap_err().0.contains("twice"));
    }
}
