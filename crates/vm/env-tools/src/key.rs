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
/// The cache key of a tool image: `(runtime id, sorted resolved NEVRAs)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CacheKey {
    pub runtime: PackageId,
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
