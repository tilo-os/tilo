//! Core-WebAssembly ABI between the Tilo format host and format-provider
//! guests, plus the Rust types both sides speak.
//!
//! A provider is a plain core WebAssembly module (built for
//! `wasm32-unknown-unknown`). Every call runs in a fresh instance, so guests
//! never free anything: the host discards the whole linear memory afterwards.
//!
//! # Exports (ABI version 1)
//!
//! | Export                | Signature            | Meaning                                  |
//! | --------------------- | -------------------- | ---------------------------------------- |
//! | `memory`              | memory               | The guest's linear memory.               |
//! | `tilo_fp_abi_version` | `() -> i32`          | Returns [`ABI_VERSION`].                 |
//! | `tilo_fp_alloc`       | `(len: i32) -> i32`  | Reserves `len` bytes for host input.     |
//! | `tilo_fp_detect`      | `(ptr, len) -> i64`  | Sniffs a file head.                      |
//! | `tilo_fp_import`      | `(ptr, len) -> i64`  | Native format bytes to canonical bytes.  |
//! | `tilo_fp_export`      | `(ptr, len) -> i64`  | Canonical bytes to native format bytes.  |
//!
//! The host calls `tilo_fp_alloc`, writes its input there, then calls one of
//! the three entry points with that region. Each entry point returns a
//! response region packed as `ptr | len << 32` (see [`pack_region`]); the
//! response bytes are encoded by [`encode_detected`] or [`encode_result`].
//!
//! # Imports
//!
//! The only import a provider may declare is `tilo_fp.fill_random(ptr: i32,
//! len: i32)`, which fills guest memory with host entropy. Rust guests get it
//! through the `entropy` feature. Everything else is refused at load time, so
//! a provider has no file, network, clock, or environment access.
//!
//! # Guests
//!
//! Implement [`Provider`] and invoke `export_provider!`; the macro emits the
//! exports above.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

#[cfg(target_arch = "wasm32")]
#[doc(hidden)]
pub mod guest;

/// Version returned by the guest's `tilo_fp_abi_version` export.
pub const ABI_VERSION: i32 = 1;

/// Export names of ABI version 1.
pub mod exports {
    pub const MEMORY: &str = "memory";
    pub const ABI_VERSION: &str = "tilo_fp_abi_version";
    pub const ALLOC: &str = "tilo_fp_alloc";
    pub const DETECT: &str = "tilo_fp_detect";
    pub const IMPORT: &str = "tilo_fp_import";
    pub const EXPORT: &str = "tilo_fp_export";
}

/// Import module and names a provider may use.
pub mod imports {
    pub const MODULE: &str = "tilo_fp";
    pub const FILL_RANDOM: &str = "fill_random";
}

/// Provider confidence tier — a coarse trust bucket that the host uses to pick
/// among multiple providers claiming the same MIME type. `Builtin` providers are
/// vendored with the OS; `Installed` are user-installed third-party providers;
/// `Ephemeral` are providers attached transiently (e.g. by a packaged app).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Ephemeral = 0,
    Installed = 1,
    Builtin = 2,
}

impl Tier {
    /// Numeric value carried in [`Confidence::tier`].
    pub fn as_u32(self) -> u32 {
        self as u32
    }

    /// Parse a `priority = "…"` value from the registry TOML.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "ephemeral" => Some(Tier::Ephemeral),
            "installed" => Some(Tier::Installed),
            "builtin" => Some(Tier::Builtin),
            _ => None,
        }
    }
}

/// How sure a provider is that it understands a file.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Confidence {
    pub tier: u32,
    pub score: f32,
}

/// Result of `detect`.
#[derive(Clone, Debug, PartialEq)]
pub struct Detected {
    pub confidence: Confidence,
    pub mime: Option<String>,
}

/// Categorised codec failure. `kind` lets the host (and a calling client)
/// distinguish parse vs. export vs. resource failures without parsing
/// free-form messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecError {
    pub kind: String,
    pub message: String,
}

impl CodecError {
    pub fn parse(message: impl Into<String>) -> Self {
        Self::new("parse", message)
    }

    pub fn export(message: impl Into<String>) -> Self {
        Self::new("export", message)
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new("unsupported", message)
    }

    pub fn resource_limit(message: impl Into<String>) -> Self {
        Self::new("resource_limit", message)
    }

    pub fn trap(message: impl Into<String>) -> Self {
        Self::new("trap", message)
    }

    fn new(kind: &str, message: impl Into<String>) -> Self {
        Self {
            kind: kind.to_owned(),
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

/// The three operations a guest implements.
pub trait Provider {
    fn detect(head: &[u8]) -> Detected;
    /// Convert native format bytes into canonical document bytes.
    ///
    /// # Errors
    ///
    /// Returns a [`CodecError`] when the input cannot be parsed.
    fn import_doc(bytes: &[u8]) -> Result<Vec<u8>, CodecError>;
    /// Convert canonical document bytes into native format bytes.
    ///
    /// # Errors
    ///
    /// Returns a [`CodecError`] when the canonical bytes are invalid or the
    /// document cannot be expressed in the native format.
    fn export_doc(canonical: &[u8]) -> Result<Vec<u8>, CodecError>;
}

/// Pack a guest memory region into an entry point's `i64` return value.
pub fn pack_region(ptr: u32, len: u32) -> u64 {
    u64::from(ptr) | (u64::from(len) << 32)
}

/// Split an entry point's return value into `(ptr, len)`.
pub fn unpack_region(packed: u64) -> (u32, u32) {
    let [p0, p1, p2, p3, l0, l1, l2, l3] = packed.to_le_bytes();
    (
        u32::from_le_bytes([p0, p1, p2, p3]),
        u32::from_le_bytes([l0, l1, l2, l3]),
    )
}

/// Why a response could not be decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeError(&'static str);

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "malformed provider response: {}", self.0)
    }
}

impl std::error::Error for DecodeError {}

const STATUS_OK: u8 = 0;
const STATUS_ERR: u8 = 1;

/// Encode a `detect` response: `tier: u32 LE | score: f32 LE | mime UTF-8`.
/// An empty MIME means "none".
pub fn encode_detected(d: &Detected) -> Vec<u8> {
    let mime = d.mime.as_deref().unwrap_or_default();
    let mut out = Vec::with_capacity(8_usize.saturating_add(mime.len()));
    out.extend_from_slice(&d.confidence.tier.to_le_bytes());
    out.extend_from_slice(&d.confidence.score.to_le_bytes());
    out.extend_from_slice(mime.as_bytes());
    out
}

/// Decode a response produced by [`encode_detected`].
///
/// # Errors
///
/// Returns a [`DecodeError`] if the response is truncated or the MIME is not
/// UTF-8.
pub fn decode_detected(bytes: &[u8]) -> Result<Detected, DecodeError> {
    let (tier, rest) = take_u32(bytes).ok_or(DecodeError("truncated detect tier"))?;
    let (score, mime) = take_u32(rest).ok_or(DecodeError("truncated detect score"))?;
    let mime = std::str::from_utf8(mime).map_err(|_| DecodeError("mime is not UTF-8"))?;
    Ok(Detected {
        confidence: Confidence {
            tier,
            score: f32::from_bits(score),
        },
        mime: (!mime.is_empty()).then(|| mime.to_owned()),
    })
}

/// Encode an import/export response: `0 | payload` on success,
/// `1 | kind_len: u32 LE | kind | message` on failure.
pub fn encode_result(r: &Result<Vec<u8>, CodecError>) -> Vec<u8> {
    match r {
        Ok(payload) => {
            let mut out = Vec::with_capacity(payload.len().saturating_add(1));
            out.push(STATUS_OK);
            out.extend_from_slice(payload);
            out
        }
        Err(e) => {
            let kind_len = u32::try_from(e.kind.len()).unwrap_or(u32::MAX);
            let mut out = Vec::with_capacity(
                5_usize
                    .saturating_add(e.kind.len())
                    .saturating_add(e.message.len()),
            );
            out.push(STATUS_ERR);
            out.extend_from_slice(&kind_len.to_le_bytes());
            out.extend_from_slice(e.kind.as_bytes());
            out.extend_from_slice(e.message.as_bytes());
            out
        }
    }
}

/// Decode a response produced by [`encode_result`], consuming the buffer so a
/// successful payload is returned without another copy.
///
/// # Errors
///
/// Returns a [`DecodeError`] if the response is empty, carries an unknown
/// status, or has a truncated or non-UTF-8 error record.
pub fn decode_result(mut bytes: Vec<u8>) -> Result<Result<Vec<u8>, CodecError>, DecodeError> {
    let Some(&status) = bytes.first() else {
        return Err(DecodeError("empty response"));
    };
    match status {
        STATUS_OK => {
            bytes.remove(0);
            Ok(Ok(bytes))
        }
        STATUS_ERR => {
            let rest = bytes.get(1..).unwrap_or_default();
            let (kind_len, rest) = take_u32(rest).ok_or(DecodeError("truncated error kind"))?;
            let kind_len = usize::try_from(kind_len).map_err(|_| DecodeError("error kind"))?;
            if rest.len() < kind_len {
                return Err(DecodeError("truncated error kind"));
            }
            let (kind, message) = rest.split_at(kind_len);
            let text = |b: &[u8]| {
                std::str::from_utf8(b)
                    .map(str::to_owned)
                    .map_err(|_| DecodeError("error record is not UTF-8"))
            };
            Ok(Err(CodecError {
                kind: text(kind)?,
                message: text(message)?,
            }))
        }
        _ => Err(DecodeError("unknown status")),
    }
}

fn take_u32(bytes: &[u8]) -> Option<(u32, &[u8])> {
    let (head, rest) = bytes.split_first_chunk::<4>()?;
    Some((u32::from_le_bytes(*head), rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_and_error_kinds_have_stable_names() {
        for (text, want) in [
            ("builtin", Some(Tier::Builtin)),
            ("Installed", Some(Tier::Installed)),
            ("ephemeral", Some(Tier::Ephemeral)),
            ("nope", None),
        ] {
            assert_eq!(Tier::parse(text), want, "case {text}");
        }
        assert!(Tier::Builtin > Tier::Installed);
        assert!(Tier::Installed > Tier::Ephemeral);

        for (err, kind) in [
            (CodecError::parse("x"), "parse"),
            (CodecError::export("x"), "export"),
            (CodecError::unsupported("x"), "unsupported"),
            (CodecError::resource_limit("x"), "resource_limit"),
            (CodecError::trap("x"), "trap"),
        ] {
            assert_eq!(err.kind, kind);
        }
    }

    #[test]
    fn abi_payloads_round_trip_and_reject_malformed_bytes() {
        let packed = pack_region(0x1234_5678, 0x9abc_def0);
        assert_eq!(unpack_region(packed), (0x1234_5678, 0x9abc_def0));

        for d in [
            Detected {
                confidence: Confidence {
                    tier: 2,
                    score: 0.9,
                },
                mime: Some("text/markdown".into()),
            },
            Detected {
                confidence: Confidence {
                    tier: 0,
                    score: 0.0,
                },
                mime: None,
            },
        ] {
            assert_eq!(decode_detected(&encode_detected(&d)).unwrap(), d);
        }
        assert!(decode_detected(&[1, 2, 3]).is_err());

        for result in [
            Ok(b"canonical".to_vec()),
            Err(CodecError::parse("bad heading")),
            Ok(Vec::new()),
        ] {
            assert_eq!(decode_result(encode_result(&result)).unwrap(), result);
        }
        for malformed in [
            Vec::new(),
            vec![7],
            vec![STATUS_ERR, 9, 0, 0, 0, b'x'],
            vec![STATUS_ERR, 1, 0, 0, 0, 0xff],
        ] {
            assert!(decode_result(malformed.clone()).is_err(), "{malformed:?}");
        }
    }
}
