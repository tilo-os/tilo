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

use crate::capabilities::Capabilities;
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
    /// `[cli]` block — commands the package publishes on the user's
    /// PATH via the launcher's shim binary. Only meaningful when the
    /// entrypoint declares `type = "cli"` or `type = "both"`.
    #[serde(default)]
    pub cli: Option<Cli>,
    /// `[capabilities]` block — privileged system operations the package
    /// declares it will ask the trusted broker to perform. A ceiling, not
    /// a grant: absent (the default) means an ordinary app that touches no
    /// privileged operation. See [`crate::capabilities`].
    #[serde(default, skip_serializing_if = "Capabilities::is_empty")]
    pub capabilities: Capabilities,
}

/// `[package]` — the identifying metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    /// Reverse-DNS identifier — `dev.qos.editor`. Stable across
    /// versions; pinned in CLI / loader / sandbox profile names.
    pub id: String,
    /// Human-facing name — `"API Tester"`, `"On-Screen Keyboard"`.
    /// Absent means "derive one from the id's trailing component",
    /// which is what every consumer did before this field existed;
    /// it is here because that derivation cannot produce `API` from
    /// `api-tester` or `E-Book Reader` from `ebook`.
    #[serde(default)]
    pub name: Option<String>,
    /// Freedesktop-style menu category — `Office`, `Development`,
    /// `Network`, `System`, `Utility`, `Multimedia`, `Accessibility`.
    /// Surfaced by the shell's Applications search provider so a
    /// category word finds the apps in it.
    #[serde(default)]
    pub category: Option<String>,
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
    /// Minimum CPU feature-level per architecture this build's
    /// binaries require. Publishers ship one entry per architecture
    /// they support:
    ///
    /// ```toml
    /// [package.isa]
    /// x86_64 = "v3"
    /// aarch64 = "v8.2-a"
    /// ```
    ///
    /// A missing entry for the local host's arch means "no build
    /// available for this arch"; `qos-install` refuses the install with
    /// a clear error. A missing entry that IS provided at the widest
    /// value (`baseline` for x86, `armv8-a` for aarch64) is the widest
    /// possible floor.
    #[serde(default)]
    pub isa: IsaRequirements,
}

/// Per-arch minimum-feature-level requirements. Every field is
/// optional — a publisher who only ships x86_64 leaves aarch64
/// unset.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsaRequirements {
    #[serde(default)]
    pub x86_64: Option<X86_64Level>,
    #[serde(default)]
    pub aarch64: Option<AArch64Level>,
}

impl IsaRequirements {
    /// The requirement for the given architecture. `None` = no build
    /// declared for this arch; caller refuses the install.
    pub fn for_arch(&self, arch: Arch) -> Option<IsaLevel> {
        match arch {
            Arch::X86_64 => self.x86_64.map(IsaLevel::X86_64),
            Arch::AArch64 => self.aarch64.map(IsaLevel::AArch64),
        }
    }
}

/// Architectures qOS distributes packages for. Deliberately narrow —
/// broader targets (riscv64, ppc64le) join as they clear the
/// "publisher pipeline supports it + we have a bootc image + we can
/// test it" gate.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    X86_64,
    AArch64,
}

/// Per-arch level, arch-tagged so mismatched compares are impossible
/// at the type level (a v3 x86 tag can't accidentally be compared
/// with an armv8.2 tag).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum IsaLevel {
    X86_64(X86_64Level),
    AArch64(AArch64Level),
}

impl IsaLevel {
    pub fn arch(self) -> Arch {
        match self {
            IsaLevel::X86_64(_) => Arch::X86_64,
            IsaLevel::AArch64(_) => Arch::AArch64,
        }
    }

    /// Machine-readable label used in `.qpkg` filenames — e.g.
    /// `dev.qos.editor-0.1.0-x86_64-v3.qpkg`.
    pub fn wire_label(self) -> &'static str {
        match self {
            IsaLevel::X86_64(l) => l.wire_label(),
            IsaLevel::AArch64(l) => l.wire_label(),
        }
    }

    /// Ranks within an arch — cross-arch compare panics because it's
    /// a caller bug (comparison between arm and x86 has no semantic
    /// meaning).
    pub fn rank(self) -> u8 {
        match self {
            IsaLevel::X86_64(l) => l.rank(),
            IsaLevel::AArch64(l) => l.rank(),
        }
    }
}

/// x86-64 microarchitecture feature levels per the SysV psABI x86-64
/// supplement. Each level is a strict superset of the previous.
#[allow(non_camel_case_types)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum X86_64Level {
    /// Baseline x86-64 (K8, 2003+). SSE2 + CMPXCHG16B + generic AMD64.
    #[serde(alias = "x86-64")]
    #[serde(alias = "baseline")]
    Baseline,
    /// v2 — Nehalem (Intel 2008) + Bulldozer (AMD 2011). Adds
    /// SSE3/SSSE3/SSE4.1/SSE4.2/POPCNT/CMPXCHG16B.
    #[serde(rename = "v2")]
    #[serde(alias = "x86-64-v2")]
    V2,
    /// v3 — Haswell (Intel 2013) + Excavator (AMD 2015). Adds AVX/AVX2
    /// + BMI1/BMI2/FMA/F16C/LZCNT/MOVBE/OSXSAVE. **qOS default floor.**
    #[serde(rename = "v3")]
    #[serde(alias = "x86-64-v3")]
    V3,
    /// v4 — Skylake-X (Intel 2017) + Zen 4 (AMD 2022, opt-in). Adds
    /// AVX-512F/BW/CD/DQ/VL. Server + top-tier laptop only.
    #[serde(rename = "v4")]
    #[serde(alias = "x86-64-v4")]
    V4,
}

impl X86_64Level {
    pub fn wire_label(self) -> &'static str {
        match self {
            X86_64Level::Baseline => "x86_64",
            X86_64Level::V2 => "x86_64-v2",
            X86_64Level::V3 => "x86_64-v3",
            X86_64Level::V4 => "x86_64-v4",
        }
    }
    pub fn rank(self) -> u8 {
        match self {
            X86_64Level::Baseline => 0,
            X86_64Level::V2 => 1,
            X86_64Level::V3 => 2,
            X86_64Level::V4 => 3,
        }
    }
}

/// ARMv8/v9-A profile levels covering the practical publishing tiers.
/// Modelled on the ARM Architecture Reference Manual's profile
/// designators, condensed to the four that map cleanly to shipping
/// silicon families.
///
/// Deliberately excluded from the level dimension: SVE vector width
/// (SVE2 code is width-agnostic — the same binary runs on 128/256/
/// 512-bit implementations); vendor-specific extras (Apple AMX,
/// Qualcomm MSA). Those become opt-in `target-cpu` selections per app,
/// not a tier bump.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AArch64Level {
    /// ARMv8-A baseline (2013+). NEON + FP + AES/SHA1/SHA2 crypto
    /// (mandatory in v8-A). Every 64-bit Android + iPhone 5S+.
    #[serde(rename = "armv8-a")]
    #[serde(alias = "v8-a")]
    #[serde(alias = "aarch64")]
    #[serde(alias = "baseline")]
    Armv8A,
    /// ARMv8.2-A (Cortex-A75/A55, 2017+, Apple A11+, Snapdragon 845+).
    /// Adds FP16, dot-product (SDOT/UDOT).
    #[serde(rename = "armv8.2-a")]
    #[serde(alias = "v8.2-a")]
    Armv8_2A,
    /// ARMv8.4-A (Cortex-A76+, 2019+, Apple A12+, Neoverse N1). Adds
    /// i8mm, ARMv8.2-SHA3.
    #[serde(rename = "armv8.4-a")]
    #[serde(alias = "v8.4-a")]
    Armv8_4A,
    /// ARMv9-A (Cortex-X2/A710+, 2021+, Apple M2+, Neoverse V2).
    /// Adds SVE2 (width-agnostic — code runs on any SVE2 impl).
    #[serde(rename = "armv9-a")]
    #[serde(alias = "v9-a")]
    Armv9A,
}

impl AArch64Level {
    pub fn wire_label(self) -> &'static str {
        match self {
            AArch64Level::Armv8A => "aarch64",
            AArch64Level::Armv8_2A => "aarch64-v8.2",
            AArch64Level::Armv8_4A => "aarch64-v8.4",
            AArch64Level::Armv9A => "aarch64-v9",
        }
    }
    pub fn rank(self) -> u8 {
        match self {
            AArch64Level::Armv8A => 0,
            AArch64Level::Armv8_2A => 1,
            AArch64Level::Armv8_4A => 2,
            AArch64Level::Armv9A => 3,
        }
    }
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
    /// Attestor's Ed25519 signature over the canonical attestation
    /// preimage — see [`crate::trust::attestation_preimage`]. Hex
    /// encoded (128 chars for a 64-byte signature). Empty string
    /// if no signature is attached (legacy claim-only rows).
    #[serde(default, rename = "signature")]
    pub signature_hex: String,
}

impl TrustAttestation {
    /// Parse the signature hex into raw bytes. `Ok(None)` when
    /// the `signature` field is empty.
    pub fn signature_bytes(&self) -> Result<Option<[u8; 64]>, IdentityParseError> {
        if self.signature_hex.is_empty() {
            return Ok(None);
        }
        let bytes = hex::decode(&self.signature_hex).map_err(IdentityParseError::Hex)?;
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
    /// `[sandbox.fs]` block — **retired**, and absent from every
    /// manifest that validates.
    ///
    /// Decoded only so a manifest that still carries one can be refused
    /// by name (`validate::ManifestError::FsWriteDeclared` /
    /// `FsReadDeclared`) rather than by a serde "unknown field" error
    /// that says nothing about why. See `crate::validate` for what
    /// replaced it.
    #[serde(default, skip_serializing_if = "SandboxFs::is_empty")]
    pub fs: SandboxFs,
    /// `[sandbox.net]` block — egress policy.
    pub net: SandboxNet,
    /// `[sandbox.portals]` table — portal policy per portal name.
    ///
    /// The map keys are freedesktop portal names (`file-chooser`,
    /// `screencast`, `notifications`, `secret-service`, `open-uri`).
    /// Values are [`PortalPolicy`] — today either `via-host` (the
    /// launcher plumbs the session D-Bus socket in and the app can
    /// use `ashpd`) or `deny` (the portal is explicitly disabled).
    /// An entry MUST exist for every portal the app expects to use;
    /// absence == the launcher will NOT proxy the bus in for that
    /// portal, so the app's `ashpd` call surfaces `NotAvailable`.
    #[serde(default)]
    pub portals: std::collections::BTreeMap<String, PortalPolicy>,
    /// `[sandbox.services]` table — required/optional services.
    /// Each value parses as `"required@N"` / `"optional@N"`.
    #[serde(default)]
    pub services: std::collections::BTreeMap<String, ServiceReq>,
}

/// A path pattern in the retired `[sandbox.fs]` block. Stored as a raw
/// string — it is never expanded into a path any more, only quoted back
/// in the refusal that names it.
pub type PathPattern = String;

/// `[sandbox.fs]` — **retired**.
///
/// It used to be declarative file access: the packager listed paths and
/// the launcher bound them, source and destination, read-write for every
/// `write` entry. That made the sandbox enforce a policy written by the
/// thing it was sandboxing — `write = ["/"]` was a valid manifest and
/// produced a valid launch.
///
/// The fields survive so that a package still carrying them is refused
/// with its own words quoted back. Nothing reads them to build a
/// sandbox.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxFs {
    /// Read-only path patterns. Any entry is a validation failure.
    #[serde(default)]
    pub read: Vec<PathPattern>,
    /// Read-write path patterns. Any entry is a validation failure.
    #[serde(default)]
    pub write: Vec<PathPattern>,
}

impl SandboxFs {
    /// Whether the package declared no paths — the only shape that
    /// validates, and therefore the only one re-encoded.
    pub fn is_empty(&self) -> bool {
        self.read.is_empty() && self.write.is_empty()
    }
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

/// How the launcher should route a portal request.
///
/// Historically the manifest wrote `"via-host"` as a bare string.
/// Callers now parse into this enum so misspellings surface at
/// manifest-decode time instead of silently downgrading to a no-op.
///
/// The wire form is a plain kebab-case string; TOML round-trips
/// via the `#[serde(rename_all = "kebab-case")]` attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PortalPolicy {
    /// Route through the freedesktop portal on the host session bus.
    /// The launcher bind-mounts the session D-Bus socket into the
    /// sandbox and sets `DBUS_SESSION_BUS_ADDRESS` — the app then
    /// uses `runtime::portal_xdg` (or `ashpd` directly).
    ViaHost,
    /// The portal is explicitly denied. The launcher does NOT plumb
    /// the bus in and any `ashpd` call from the app surfaces
    /// `PortalError::NotAvailable`. Same on-wire behaviour as
    /// omitting the entry, but signals intent for auditors.
    Deny,
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
///
/// Apps can ship both a GUI and a CLI entrypoint. The `type` field
/// selects which flavour is meant:
///
/// - `gui` (default) — `bin` is a GUI app; use the whole [`Entrypoint`]
///   as today.
/// - `cli` — `bin` is a CLI app; the launcher inherits stdio and
///   does not set up a Wayland surface. Publishes commands from the
///   [`Cli`] block onto the user's PATH via installed shims.
/// - `both` — the package ships two binaries. `gui_bin` is used
///   when launched through the app grid / graphical launcher; `cli_bin`
///   is what the PATH shim execvs. Both must be under the payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entrypoint {
    /// Path inside the payload to the binary to exec. For
    /// `type = "both"` the `gui_bin` / `cli_bin` fields take
    /// precedence; `bin` is kept for schema back-compat and used
    /// only when the split fields are absent.
    #[serde(default)]
    pub bin: PathBuf,
    /// Kind of entrypoint. Defaults to `gui` when omitted so
    /// existing manifests stay valid.
    #[serde(default, rename = "type")]
    pub r#type: EntrypointType,
    /// GUI-flavour binary path inside the payload. Only meaningful
    /// when `type = "both"`.
    #[serde(default, rename = "gui-bin")]
    pub gui_bin: Option<PathBuf>,
    /// CLI-flavour binary path inside the payload. Only meaningful
    /// when `type = "both"`.
    #[serde(default, rename = "cli-bin")]
    pub cli_bin: Option<PathBuf>,
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

impl Entrypoint {
    /// The binary path the *GUI* launcher should exec. Falls back
    /// to `bin` when the split fields aren't in the manifest.
    pub fn gui_bin_path(&self) -> Option<&PathBuf> {
        match self.r#type {
            EntrypointType::Gui => Some(&self.bin),
            EntrypointType::Cli => None,
            EntrypointType::Both => self.gui_bin.as_ref().or(Some(&self.bin)),
        }
    }

    /// The binary path a CLI command should exec. Falls back to
    /// `bin` when the split fields aren't in the manifest.
    pub fn cli_bin_path(&self) -> Option<&PathBuf> {
        match self.r#type {
            EntrypointType::Gui => None,
            EntrypointType::Cli => Some(&self.bin),
            EntrypointType::Both => self.cli_bin.as_ref().or(Some(&self.bin)),
        }
    }
}

/// Which flavour of entrypoint the package exposes. Wire form is
/// lowercase — `type = "gui"` / `"cli"` / `"both"`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntrypointType {
    /// Windowed / graphical app. The default so back-compat is silent.
    #[default]
    Gui,
    /// Terminal / stdio-only app.
    Cli,
    /// Ships both flavours; the [`Entrypoint::gui_bin`] / `cli_bin`
    /// paths pick the concrete binary per launch mode.
    Both,
}

/// `[cli]` — commands the package publishes on the user's PATH.
///
/// Each string in `commands` becomes a shim in
/// `~/.local/share/qos-apps/bin/<cmd>` at install time; the shim
/// re-execs through the launcher's `run-installed` subcommand.
///
/// Callers writing manifests should keep the names short and
/// namespace-clean — the shim install layer treats them as raw
/// filesystem paths.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cli {
    /// PATH-visible command names (e.g. `["notes", "notes-import"]`).
    /// Empty vec == the package ships no PATH commands.
    #[serde(default)]
    pub commands: Vec<String>,
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
