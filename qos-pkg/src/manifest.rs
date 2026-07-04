//! The qOS package manifest — schema, TOML parsing, and the
//! sub-types it composes.
//!
//! The manifest is the human-authored side of a `.qpkg`. It is
//! deserialised from TOML, validated (see [`crate::validate`]),
//! then bound into the signed root via the canonical postcard
//! encoding so the on-wire bytes are stable across TOML formatter
//! whitespace differences.
//!
//! The shape mirrors the schema in the crate docs exactly; field
//! renames map kebab-case TOML keys onto Rust snake_case fields.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::id::Kind;

/// Top-level manifest. Every field except those marked `Option`
/// is required; missing required fields fail TOML decode with a
/// helpful path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// `[package]` block — id, version, kind, optional runtime.
    pub package: Package,
    /// `[publisher]` block — identity + signature (signature is
    /// filled by `qos-pkg sign`, empty at manifest-author time).
    pub publisher: Publisher,
    /// `[trust]` block — third-party attestations.
    #[serde(default)]
    pub trust: Trust,
    /// `[sandbox]` block — the security profile applied at load.
    pub sandbox: Sandbox,
    /// `[entrypoint]` block — what to launch. Required for `app`;
    /// optional for other kinds (services declare their own
    /// systemd unit; runtimes don't have entrypoints).
    #[serde(default)]
    pub entrypoint: Option<Entrypoint>,
    /// `[providers]` block — only meaningful for `format-provider`.
    #[serde(default)]
    pub providers: Providers,
}

/// `[package]` — the identifying metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    /// Reverse-DNS identifier — `dev.qos.editor`. Stable across
    /// versions; pinned in CLI / loader / sandbox profile names.
    pub id: String,
    /// Semver string. Validated by [`crate::validate`] via the
    /// `semver` crate; rejected if not parseable.
    pub version: String,
    /// One of the [`Kind`] variants.
    pub kind: Kind,
    /// `<runtime-name>/<version>` of the qOS runtime this package
    /// binds to. `None` for `kind = "runtime"` (which IS a
    /// runtime). The version side must parse as semver.
    #[serde(default)]
    pub runtime: Option<String>,
}

/// `[publisher]` — who signed it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Publisher {
    /// The publisher's Ed25519 public key, formatted as
    /// `ed25519:<hex>`. Hex is canonical lower-case; the verifier
    /// re-parses this into bytes.
    pub identity: String,
    /// Detached signature over `signing_bytes(manifest, payload)`.
    /// Empty string at author time; filled by `qos-pkg sign`.
    #[serde(default)]
    pub signature: String,
}

impl Publisher {
    /// Parse the hex part out of an `ed25519:<hex>` identity
    /// string. Returns just the raw key bytes.
    pub fn identity_bytes(&self) -> Result<[u8; 32], IdentityParseError> {
        parse_ed25519_hex(&self.identity)
    }

    /// Parse the hex signature into 64 raw bytes. `Ok(None)` if
    /// the signature is empty (the manifest is not yet signed).
    pub fn signature_bytes(&self) -> Result<Option<[u8; 64]>, IdentityParseError> {
        if self.signature.is_empty() {
            return Ok(None);
        }
        let bytes = hex::decode(&self.signature).map_err(IdentityParseError::Hex)?;
        if bytes.len() != 64 {
            return Err(IdentityParseError::WrongLength {
                expected: 64,
                got: bytes.len(),
            });
        }
        let mut out = [0u8; 64];
        out.copy_from_slice(&bytes);
        Ok(Some(out))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IdentityParseError {
    #[error("expected `ed25519:<hex>` form, got {0:?}")]
    BadPrefix(String),
    #[error("hex decode failed: {0}")]
    Hex(hex::FromHexError),
    #[error("expected {expected} bytes, got {got}")]
    WrongLength { expected: usize, got: usize },
}

fn parse_ed25519_hex(s: &str) -> Result<[u8; 32], IdentityParseError> {
    let rest = s
        .strip_prefix("ed25519:")
        .ok_or_else(|| IdentityParseError::BadPrefix(s.to_string()))?;
    let bytes = hex::decode(rest).map_err(IdentityParseError::Hex)?;
    if bytes.len() != 32 {
        return Err(IdentityParseError::WrongLength {
            expected: 32,
            got: bytes.len(),
        });
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// `[trust]` — optional third-party attestations. A curation
/// service or sandbox-policy reviewer can attach a claim that
/// the loader checks before granting elevated capabilities.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trust {
    /// `[[trust.attested_by]]` repeated array.
    #[serde(default, rename = "attested_by")]
    pub attested_by: Vec<TrustAttestation>,
}

/// One row from `[[trust.attested_by]]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustAttestation {
    /// Attestor's identity, e.g. `qos-curated/2025.06`.
    pub attestor: String,
    /// Opaque claim bytes — hex-encoded for the manifest. Format
    /// is attestor-specific (often itself a small signature
    /// over the package id).
    #[serde(rename = "claim")]
    pub claim_hex: String,
}

/// `[sandbox]` — the runtime confinement profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sandbox {
    /// Linux namespaces to unshare. Names match `unshare(2)`'s
    /// flags: `user`, `pid`, `uts`, `ipc`, `mount`, `net`.
    pub namespaces: Vec<String>,
    /// Named seccomp profile — `qos-default` is the baseline.
    pub seccomp: String,
    /// Landlock strictness — `strict` | `lax` | `off`.
    pub landlock: String,
    /// `[sandbox.fs]` block — read / write path patterns.
    pub fs: SandboxFs,
    /// `[sandbox.net]` block — egress policy.
    pub net: SandboxNet,
    /// `[sandbox.portals]` table — portal mode per portal name.
    #[serde(default)]
    pub portals: std::collections::BTreeMap<String, String>,
    /// `[sandbox.services]` table — required/optional services.
    /// Each value parses as `"required@N"` / `"optional@N"`.
    #[serde(default)]
    pub services: std::collections::BTreeMap<String, ServiceReq>,
}

/// A path pattern in `[sandbox.fs]`. Stored as a raw string so
/// loader-side expansion (`~`, `<scope=current>`, …) can run
/// without re-parsing.
pub type PathPattern = String;

/// `[sandbox.fs]` — declarative file access.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxFs {
    /// Read-only path patterns. Empty `Vec` if the package
    /// reads nothing outside its own payload.
    #[serde(default)]
    pub read: Vec<PathPattern>,
    /// Read-write path patterns.
    #[serde(default)]
    pub write: Vec<PathPattern>,
}

/// `[sandbox.net]` — egress policy.
///
/// `mode` is one of `off | host | dns-only | allowlist`. When
/// `mode = "allowlist"` the `hosts` field carries the allowed
/// host names. For the other modes `hosts` is empty / ignored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxNet {
    pub mode: NetMode,
    /// Only meaningful when `mode = "allowlist"`.
    #[serde(default)]
    pub hosts: Vec<String>,
}

impl SandboxNet {
    /// Convenience: hosts only if mode is Allowlist; `None` for
    /// every other mode.
    pub fn allowlist_hosts(&self) -> Option<&[String]> {
        if matches!(self.mode, NetMode::Allowlist) {
            Some(&self.hosts)
        } else {
            None
        }
    }
}

/// Egress modes. The kebab-case wire form matches the TOML
/// `mode = "off"` / `mode = "host"` / `mode = "dns-only"` /
/// `mode = "allowlist"` values exactly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetMode {
    /// No network access at all.
    Off,
    /// Full host network (only for `runtime` / privileged apps).
    Host,
    /// DNS resolution only — useful for apps that surface URLs
    /// but rely on the host browser to follow them.
    DnsOnly,
    /// Allowlist of hostnames the app may connect to. The
    /// concrete host names live in [`SandboxNet::hosts`].
    Allowlist,
}

/// `required@N` / `optional@N` — a declared dependency on a
/// service-protocol with a version pin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceReq {
    /// Protocol name as declared by the providing service (e.g.
    /// `collab`, `qos-fs`, `spellcheck`).
    pub protocol: String,
    /// Whether absence is a hard failure.
    pub requirement: Requirement,
    /// Protocol version pin. `1` if the manifest wrote just
    /// `"required"` / `"optional"` with no `@N`.
    pub version: u32,
}

/// Service-dep severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Requirement {
    Required,
    Optional,
}

impl ServiceReq {
    /// Parse `"required@1"` / `"optional@2"` / `"required"`
    /// (defaults version to `1`). Rejects anything else.
    pub fn parse(protocol: &str, raw: &str) -> Result<Self, ServiceReqParseError> {
        let (sev, version) = match raw.split_once('@') {
            Some((sev, ver)) => {
                let v: u32 = ver.parse().map_err(|_| ServiceReqParseError::BadVersion {
                    raw: raw.to_string(),
                })?;
                (sev, v)
            }
            None => (raw, 1),
        };
        let requirement = match sev {
            "required" => Requirement::Required,
            "optional" => Requirement::Optional,
            other => {
                return Err(ServiceReqParseError::BadSeverity {
                    got: other.to_string(),
                });
            }
        };
        Ok(Self {
            protocol: protocol.to_string(),
            requirement,
            version,
        })
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ServiceReqParseError {
    #[error("unknown severity {got:?} (expected `required` or `optional`)")]
    BadSeverity { got: String },
    #[error("bad version in {raw:?}, expected integer after `@`")]
    BadVersion { raw: String },
}

// The TOML side encodes ServiceReq as a string ("required@1"). To
// keep the `services` map deserialisable from a flat table we
// implement Serialize/Deserialize manually.
impl<'de> serde::Deserialize<'de> for ServiceReq {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw: String = serde::Deserialize::deserialize(d)?;
        ServiceReq::parse("", &raw).map_err(serde::de::Error::custom)
    }
}

impl serde::Serialize for ServiceReq {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let sev = match self.requirement {
            Requirement::Required => "required",
            Requirement::Optional => "optional",
        };
        s.serialize_str(&format!("{sev}@{}", self.version))
    }
}

/// `[entrypoint]` — the launch hook.
///
/// For `app` packages this is required. For `service` packages it
/// is optional (services typically install a systemd unit).
/// For `runtime` packages it is never set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entrypoint {
    /// Path inside the payload to the binary to exec.
    pub bin: PathBuf,
    /// Splash-segment hints: the loader prefetches these byte
    /// ranges before exec so the splash screen renders before the
    /// full binary is in cache.
    #[serde(default, rename = "splash-segments")]
    pub splash_segments: Vec<SplashSegment>,
    /// Soft budget (ms) for the splash to display before the
    /// real window is expected. Loader uses this to decide
    /// whether to keep the splash or fall back to a default.
    #[serde(default = "default_splash_budget", rename = "splash-budget")]
    pub splash_budget_ms: u32,
}

fn default_splash_budget() -> u32 {
    100
}

/// A path-with-optional-byte-range used in `splash-segments`.
///
/// Wire form: `"bin/editor[0x0:0x300000]"` (range) or
/// `"share/splash.png"` (whole file). Parsed eagerly so the
/// loader has a typed range instead of a string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplashSegment {
    pub path: PathBuf,
    /// `(start, end)` inclusive-start, exclusive-end byte
    /// range. `None` means the whole file.
    pub byte_range: Option<(u64, u64)>,
}

impl SplashSegment {
    /// Parse the wire form.
    pub fn parse(raw: &str) -> Result<Self, SplashSegmentError> {
        if let Some(open) = raw.rfind('[') {
            let close = raw
                .rfind(']')
                .ok_or_else(|| SplashSegmentError::BadShape(raw.to_string()))?;
            if close <= open {
                return Err(SplashSegmentError::BadShape(raw.to_string()));
            }
            let path = PathBuf::from(&raw[..open]);
            let inner = &raw[open + 1..close];
            let (a, b) = inner
                .split_once(':')
                .ok_or_else(|| SplashSegmentError::BadShape(raw.to_string()))?;
            let start = parse_int(a).ok_or_else(|| SplashSegmentError::BadInt(a.to_string()))?;
            let end = parse_int(b).ok_or_else(|| SplashSegmentError::BadInt(b.to_string()))?;
            if end < start {
                return Err(SplashSegmentError::EmptyRange);
            }
            Ok(Self {
                path,
                byte_range: Some((start, end)),
            })
        } else {
            Ok(Self {
                path: PathBuf::from(raw),
                byte_range: None,
            })
        }
    }
}

fn parse_int(s: &str) -> Option<u64> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        s.parse::<u64>().ok()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SplashSegmentError {
    #[error("malformed splash segment {0:?} (expected `path[start:end]` or `path`)")]
    BadShape(String),
    #[error("not a number: {0:?}")]
    BadInt(String),
    #[error("end < start")]
    EmptyRange,
}

impl<'de> serde::Deserialize<'de> for SplashSegment {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s: String = serde::Deserialize::deserialize(d)?;
        SplashSegment::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl serde::Serialize for SplashSegment {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let path_str = self
            .path
            .to_str()
            .ok_or_else(|| serde::ser::Error::custom("splash-segment path is not UTF-8"))?;
        match self.byte_range {
            None => s.serialize_str(path_str),
            Some((a, b)) => s.serialize_str(&format!("{path_str}[0x{a:x}:0x{b:x}]")),
        }
    }
}

/// `[providers]` — what file kinds this package can open.
/// Only meaningful for `kind = "format-provider"`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Providers {
    /// MIME types claimed (e.g. `application/vnd.oasis.opendocument.text`).
    #[serde(default)]
    pub mime: Vec<String>,
    /// File extensions claimed (without leading dot).
    #[serde(default)]
    pub ext: Vec<String>,
}

impl Manifest {
    /// Decode a manifest from TOML source.
    pub fn from_toml_str(s: &str) -> Result<Self, ManifestParseError> {
        // The TOML side encodes `[sandbox.services]` as a table of
        // service-name → string. The `ServiceReq` Deserialize sees
        // the value as a raw `"required@1"` string; the protocol
        // name we attach to it comes from the table key. The
        // generic decode loses that key, so we post-process.
        let mut m: Manifest = toml::from_str(s).map_err(ManifestParseError::Toml)?;
        // Patch the `protocol` field of each ServiceReq with the
        // map-key that named it.
        let services = std::mem::take(&mut m.sandbox.services);
        m.sandbox.services = services
            .into_iter()
            .map(|(name, mut req)| {
                req.protocol = name.clone();
                (name, req)
            })
            .collect();
        Ok(m)
    }

    /// Re-encode to TOML. Used for round-trip tests + the
    /// `qos-pkg inspect` CLI.
    pub fn to_toml_string(&self) -> Result<String, ManifestParseError> {
        toml::to_string_pretty(self).map_err(ManifestParseError::Encode)
    }
}

/// TOML decode / encode errors.
#[derive(Debug, thiserror::Error)]
pub enum ManifestParseError {
    #[error("TOML decode failed: {0}")]
    Toml(toml::de::Error),
    #[error("TOML encode failed: {0}")]
    Encode(toml::ser::Error),
}
