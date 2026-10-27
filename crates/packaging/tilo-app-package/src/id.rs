//! Package identity: [`PackageId`] (the BLAKE3 root hash) and
//! [`Kind`] (app / service / format-provider / plugin / runtime /
//! env-runtime / tool).
//!
//! The package id is intentionally the same shape across every
//! package kind — same `pack` flow, same on-disk format, same
//! verifier. The `Kind` discriminates *what* the package is, not
//! *how* it is packed.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Content-addressed package identity. The 32 bytes are the BLAKE3
/// hash of the signed-root blob (the manifest + signature blob).
///
/// `Display` formats as `tapp:<hex>` for human/log/UI surfaces;
/// `FromStr` accepts that form back. The raw bytes form is used
/// on the wire and in the blob store.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PackageId(#[serde(with = "serde_id_bytes")] pub [u8; 32]);

impl PackageId {
    /// Wrap an explicit 32-byte hash.
    pub fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }

    /// Borrow the raw 32 bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Hex form without the `tapp:` prefix.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Display for PackageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tapp:{}", self.to_hex())
    }
}

impl fmt::Debug for PackageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Short form for logs: first 8 hex chars are unique enough.
        let h = self.to_hex();
        write!(f, "PackageId(tapp:{}…)", h.get(..8).unwrap_or(&h))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ParsePackageIdError {
    #[error("expected `tapp:<hex>` form, got {0:?}")]
    BadPrefix(String),
    #[error("hex decode failed: {0}")]
    Hex(#[from] hex::FromHexError),
    #[error("expected 32 bytes, got {0}")]
    WrongLength(usize),
}

impl FromStr for PackageId {
    type Err = ParsePackageIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let rest = s
            .strip_prefix("tapp:")
            .ok_or_else(|| ParsePackageIdError::BadPrefix(s.to_string()))?;
        let bytes = hex::decode(rest)?;
        if bytes.len() != 32 {
            return Err(ParsePackageIdError::WrongLength(bytes.len()));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        Ok(Self(out))
    }
}

mod serde_id_bytes {
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        // serialise as hex string so the manifest's `package.id`
        // field is human-readable. The wire form (blob payloads)
        // uses the raw 32 bytes directly.
        s.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s: String = Deserialize::deserialize(d)?;
        let v = hex::decode(&s).map_err(D::Error::custom)?;
        if v.len() != 32 {
            return Err(D::Error::custom(format!(
                "expected 32 bytes, got {}",
                v.len()
            )));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&v);
        Ok(out)
    }
}

/// What the package is. Same packaging machinery handles all kinds;
/// the loader picks the kind-specific runtime hook.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    App,
    Service,
    FormatProvider,
    Plugin,
    Runtime,
    /// A minimal userland (shell, coreutils, glibc, dnf) an environment's
    /// `/usr` and `/etc` start from. An image, never launched.
    EnvRuntime,
    /// The files a set of packages adds on top of one exact
    /// [`Kind::EnvRuntime`] package, named by `[image].runtime`. An image,
    /// never launched.
    Tool,
    /// One language's catalogues for every app and the toolkit: the files
    /// `<locale>/<catalogue>.ftl`, data for every architecture, which the
    /// runtime reads when a person's language is that locale. Its id is
    /// [`LANGUAGE_PACK_ID_PREFIX`] and the locale, lowercased. Never
    /// launched.
    LanguagePack,
}

/// The package id of a language pack is this and its locale in lower case
/// (`so.tilo.language-pack.de-de`).
pub const LANGUAGE_PACK_ID_PREFIX: &str = "so.tilo.language-pack.";

/// The locale a language pack's package id names, lowercased (`de-de`), or
/// `None` for an id that names none.
#[must_use]
pub fn language_pack_locale(package_id: &str) -> Option<&str> {
    package_id
        .strip_prefix(LANGUAGE_PACK_ID_PREFIX)
        .filter(|locale| {
            !locale.is_empty()
                && locale.len() <= 35
                && locale
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                && !locale.starts_with('-')
                && !locale.ends_with('-')
        })
}

impl Kind {
    /// String form used in the manifest (kebab-case).
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::App => "app",
            Kind::Service => "service",
            Kind::FormatProvider => "format-provider",
            Kind::Plugin => "plugin",
            Kind::Runtime => "runtime",
            Kind::EnvRuntime => "env-runtime",
            Kind::Tool => "tool",
            Kind::LanguagePack => "language-pack",
        }
    }

    /// Whether packages of this kind are environment images: read-only
    /// directory trees stacked under an environment's `/usr` and `/etc`,
    /// which the store keeps apart from applications and nothing launches.
    pub fn is_image(&self) -> bool {
        matches!(self, Kind::EnvRuntime | Kind::Tool)
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
