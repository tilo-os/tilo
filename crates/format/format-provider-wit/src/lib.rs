//! WIT interface + shared Rust mirror types for qOS format providers.
//!
//! The canonical `format-provider` interface is defined in [`wit/format-provider.wit`].
//! Guest crates compiled to `wasm32-wasip2` use `wit-bindgen` to generate Rust
//! bindings from that WIT file; the host loads the resulting components via
//! `wasmtime`'s component model. The Rust types below mirror the WIT records so
//! the host can speak about them without depending on generated bindings (which
//! live in the host's own build).
//!
//! The on-disk WIT package is also exposed as a path constant so the host's
//! integration tests can find it during cargo runs without environment plumbing.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a test that unwraps is stating an invariant"
    )
)]

use serde::{Deserialize, Serialize};

/// Path to this crate's `wit/` directory, made available to consumers at compile
/// time. Used by the host crate to locate the canonical WIT package when
/// building component bindings via `wasmtime::component::bindgen!` macros.
pub const WIT_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/wit");

/// Provider confidence tier — a coarse trust bucket that the host uses to pick
/// among multiple providers claiming the same MIME type. `Builtin` providers are
/// vendored with the OS; `Installed` are user-installed third-party providers;
/// `Ephemeral` are providers attached transiently (e.g. by a packaged app).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Ephemeral = 0,
    Installed = 1,
    Builtin = 2,
}

impl Tier {
    /// Numeric value used in the WIT `confidence.tier` field.
    pub fn as_u32(self) -> u32 {
        self as u32
    }

    /// Parse a `tier = "…"` value from the registry TOML.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "ephemeral" => Some(Tier::Ephemeral),
            "installed" => Some(Tier::Installed),
            "builtin" => Some(Tier::Builtin),
            _ => None,
        }
    }
}

/// Mirror of the WIT `confidence` record.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Confidence {
    pub tier: u32,
    pub score: f32,
}

/// Mirror of the WIT `detected` record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Detected {
    pub confidence: Confidence,
    pub mime: Option<String>,
}

/// Mirror of the WIT `codec-error` record. Categorised by `kind` so the host
/// (and a calling client) can distinguish parse vs. export vs. resource failures
/// without parsing free-form messages.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CodecError {
    pub kind: String,
    pub message: String,
}

impl CodecError {
    pub fn parse(message: impl Into<String>) -> Self {
        Self {
            kind: "parse".into(),
            message: message.into(),
        }
    }

    pub fn export(message: impl Into<String>) -> Self {
        Self {
            kind: "export".into(),
            message: message.into(),
        }
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self {
            kind: "unsupported".into(),
            message: message.into(),
        }
    }

    pub fn resource_limit(message: impl Into<String>) -> Self {
        Self {
            kind: "resource_limit".into(),
            message: message.into(),
        }
    }

    pub fn trap(message: impl Into<String>) -> Self {
        Self {
            kind: "trap".into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for CodecError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wit_dir_exists() {
        let dir = std::path::Path::new(WIT_DIR);
        assert!(
            dir.is_dir(),
            "WIT_DIR must point at a real directory: {dir:?}"
        );
        let wit = dir.join("format-provider.wit");
        assert!(
            wit.is_file(),
            "format-provider.wit ships with the crate: {wit:?}"
        );
    }

    #[test]
    fn tier_parses_and_orders() {
        assert_eq!(Tier::parse("builtin"), Some(Tier::Builtin));
        assert_eq!(Tier::parse("Installed"), Some(Tier::Installed));
        assert_eq!(Tier::parse("ephemeral"), Some(Tier::Ephemeral));
        assert_eq!(Tier::parse("nope"), None);
        assert!(Tier::Builtin > Tier::Installed);
        assert!(Tier::Installed > Tier::Ephemeral);
    }

    #[test]
    fn codec_error_kinds_distinct() {
        assert_eq!(CodecError::parse("x").kind, "parse");
        assert_eq!(CodecError::export("x").kind, "export");
        assert_eq!(CodecError::unsupported("x").kind, "unsupported");
        assert_eq!(CodecError::resource_limit("x").kind, "resource_limit");
        assert_eq!(CodecError::trap("x").kind, "trap");
    }
}
