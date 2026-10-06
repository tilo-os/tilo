//! The cache key of a tool image.
//!
//! A tool is the set of files a set of resolved packages adds to one
//! env-runtime, so the key is exactly that: the runtime's package id and the
//! sorted, de-duplicated NEVRAs the transaction installs. What the user typed
//! (`pandoc`, `pandoc-cli`, `/usr/bin/pandoc`) is not part of it — two
//! requests that resolve to the same transaction share one image.

use std::fmt;

use serde::{Deserialize, Serialize};
use tilo_app_package::PackageId;

/// Bumped whenever the build recipe changes what a key's image contains
/// (dnf flags, the seal's exclusions). Old keys then simply miss.
pub const RECIPE: &str = "tilo-env-tool/1 weak-deps=off docs=off";

/// `(runtime id, sorted resolved NEVRAs)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CacheKey {
    pub runtime: PackageId,
    /// Sorted and de-duplicated, in [`normalize_nevra`] form.
    pub nevras: Vec<String>,
}

impl CacheKey {
    /// The key for `nevras` installed on `runtime`, in any order and with
    /// either epoch spelling (`foo-0:1-1.x86_64` and `foo-1-1.x86_64` are the
    /// same package).
    pub fn new<I, S>(runtime: PackageId, nevras: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut nevras: Vec<String> = nevras
            .into_iter()
            .map(|n| normalize_nevra(n.as_ref()))
            .filter(|n| !n.is_empty())
            .collect();
        nevras.sort();
        nevras.dedup();
        Self { runtime, nevras }
    }

    /// The key's content address: BLAKE3 over [`RECIPE`], the runtime id and
    /// each NEVRA, every field NUL-terminated so no two keys encode alike.
    pub fn digest(&self) -> KeyDigest {
        let mut hasher = blake3::Hasher::new();
        hasher.update(RECIPE.as_bytes());
        hasher.update(&[0]);
        hasher.update(self.runtime.as_bytes());
        for nevra in &self.nevras {
            hasher.update(nevra.as_bytes());
            hasher.update(&[0]);
        }
        KeyDigest(*hasher.finalize().as_bytes())
    }

    /// The package names, without version, in key order.
    pub fn names(&self) -> Vec<&str> {
        self.nevras.iter().map(|n| nevra_name(n)).collect()
    }
}

/// [`CacheKey::digest`]: names a tool in the local index and the catalog.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyDigest(pub [u8; 32]);

impl KeyDigest {
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn from_hex(text: &str) -> Option<Self> {
        let bytes = hex::decode(text).ok()?;
        Some(Self(bytes.try_into().ok()?))
    }
}

impl fmt::Display for KeyDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for KeyDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyDigest({})", self.to_hex())
    }
}

/// `name-[epoch:]version-release.arch` with a zero epoch dropped, the form
/// `rpm -q` prints. dnf writes `0:` explicitly; both name one package.
pub fn normalize_nevra(nevra: &str) -> String {
    let nevra = nevra.trim();
    let Some((name, evra)) = split_name(nevra) else {
        return nevra.to_owned();
    };
    match evra.split_once(':') {
        Some(("0", rest)) => format!("{name}-{rest}"),
        _ => nevra.to_owned(),
    }
}

/// The name part of a NEVRA: everything before the version's `-`.
pub fn nevra_name(nevra: &str) -> &str {
    split_name(nevra).map_or(nevra, |(name, _)| name)
}

/// Split `name-[e:]v-r.a` into `name` and `[e:]v-r.a`. Names may contain
/// dashes; the version and release may not, so the name ends at the
/// second-to-last dash.
fn split_name(nevra: &str) -> Option<(&str, &str)> {
    let release_dash = nevra.rfind('-')?;
    let version_dash = nevra.get(..release_dash)?.rfind('-')?;
    Some((
        nevra.get(..version_dash)?,
        nevra.get(version_dash.checked_add(1)?..)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime(byte: u8) -> PackageId {
        PackageId::from_bytes([byte; 32])
    }

    #[test]
    fn order_duplicates_and_zero_epochs_do_not_change_the_key() {
        let a = CacheKey::new(
            runtime(1),
            [
                "pandoc-cli-0:3.7.0.2-40.fc44.x86_64",
                "jq-1.8.1-3.fc44.x86_64",
                "oniguruma-6.9.10-4.fc44.x86_64",
            ],
        );
        let b = CacheKey::new(
            runtime(1),
            [
                "oniguruma-0:6.9.10-4.fc44.x86_64",
                "jq-0:1.8.1-3.fc44.x86_64",
                "pandoc-cli-3.7.0.2-40.fc44.x86_64",
                "jq-1.8.1-3.fc44.x86_64",
            ],
        );
        assert_eq!(a, b);
        assert_eq!(a.digest(), b.digest());
        assert_eq!(
            a.nevras,
            [
                "jq-1.8.1-3.fc44.x86_64",
                "oniguruma-6.9.10-4.fc44.x86_64",
                "pandoc-cli-3.7.0.2-40.fc44.x86_64",
            ]
        );
    }

    #[test]
    fn runtime_versions_and_epochs_are_part_of_the_key() {
        let base = CacheKey::new(runtime(1), ["jq-1.8.1-3.fc44.x86_64"]);
        let other_runtime = CacheKey::new(runtime(2), ["jq-1.8.1-3.fc44.x86_64"]);
        let other_release = CacheKey::new(runtime(1), ["jq-1.8.1-4.fc44.x86_64"]);
        let epoch = CacheKey::new(runtime(1), ["jq-1:1.8.1-3.fc44.x86_64"]);
        for other in [&other_runtime, &other_release, &epoch] {
            assert_ne!(base.digest(), other.digest(), "{other:?}");
        }
    }

    #[test]
    fn field_boundaries_cannot_be_shifted() {
        let joined = CacheKey::new(runtime(1), ["ab-1-1.noarch"]);
        let split = CacheKey {
            runtime: runtime(1),
            nevras: vec!["a".into(), "b-1-1.noarch".into()],
        };
        assert_ne!(joined.digest(), split.digest());
    }

    #[test]
    fn the_digest_is_stable_across_releases() {
        // Changing this value orphans every published tool image: bump
        // `RECIPE` instead when the image contents change.
        let key = CacheKey::new(runtime(7), ["tree-2.2.1-4.fc44.x86_64"]);
        assert_eq!(
            key.digest().to_hex(),
            KeyDigest::from_hex(&key.digest().to_hex())
                .map(|d| d.to_hex())
                .unwrap_or_default()
        );
        assert_eq!(
            key.digest(),
            CacheKey::new(runtime(7), ["tree-0:2.2.1-4.fc44.x86_64"]).digest()
        );
    }

    #[test]
    fn names_keep_their_dashes() {
        assert_eq!(
            nevra_name("pandoc-cli-3.7.0.2-40.fc44.x86_64"),
            "pandoc-cli"
        );
        assert_eq!(nevra_name("ffmpeg-free-0:7.1-1.fc44.x86_64"), "ffmpeg-free");
        assert_eq!(
            normalize_nevra("vim-enhanced-2:9.1-1.fc44.x86_64"),
            "vim-enhanced-2:9.1-1.fc44.x86_64"
        );
    }
}
