//! The Tilo package manifest — schema, TOML parsing, and its sub-types.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::id::{Kind, PackageId};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// `[package]` block — id, version, kind, runtimes.
    pub package: Package,
    /// `[publisher]` block — identity + signature (filled by `tilo-app-package sign`, empty at author time).
    pub publisher: Publisher,
    /// `[trust]` block — third-party attestations.
    #[serde(default)]
    pub trust: Trust,
    /// `[continuity]` block — the framework-owned state facets this app
    /// promises every handoff will retain. This is signed with the rest of
    /// the manifest, so a release cannot silently weaken its fidelity.
    #[serde(default)]
    pub continuity: ContinuityContract,
    /// Signed external accessibility acceptance evidence. This is deliberately
    /// package metadata rather than a source-only test list: the installed
    /// image check must drive the exact manifest/payload pair that was packed.
    #[serde(default, skip_serializing_if = "Accessibility::is_empty")]
    pub accessibility: Accessibility,
    /// `[actions]` block — what the app may do on the session's action API
    /// under its own identity, beyond the actions it publishes itself.
    #[serde(default, skip_serializing_if = "ActionAccess::is_empty")]
    pub actions: ActionAccess,
    /// `[resources]` block — the resource kinds this app's records hold,
    /// written by `tilolang package` from its `#[resource(kind)]`
    /// declarations. A host creates a missing record for the app only in a
    /// kind listed here; a manifest without the block declares none.
    #[serde(default, skip_serializing_if = "Resources::is_empty")]
    pub resources: Resources,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub offers: Vec<OfferedAction>,
    /// `[sandbox]` block — the security profile applied at load.
    pub sandbox: Sandbox,
    /// `[entrypoint]` block — what to launch. Required for `app`; optional/unset for other kinds.
    #[serde(default)]
    pub entrypoint: Option<Entrypoint>,
    /// `[providers]` block — resource claims for GUI apps and MIME scope for
    /// `format-provider` packages.
    #[serde(default)]
    pub providers: Providers,
    /// `[cli]` block — commands published on PATH via the launcher's
    /// shim. Only meaningful when the entrypoint is `cli` or `both`.
    #[serde(default)]
    pub cli: Option<Cli>,
    /// `[target]` block — the non-native execution target this build was
    /// packed for. Absent for native Tilo OS packages, whose target is
    /// `[package.isa]`. Written by the packer, never by the app author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
    /// `[image]` block — what an `env-runtime` or `tool` image is built on
    /// and holds. Required for those kinds, refused for every other one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<Image>,
    /// `[capabilities]` block — the host capabilities the app uses. Signed
    /// with the rest of the manifest so every host can check them before
    /// launch.
    #[serde(default, skip_serializing_if = "Capabilities::is_empty")]
    pub capabilities: Capabilities,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Platform {
    Linux,
    Macos,
    Web,
    /// Linux, and only under Tilo OS host services.
    TiloOs,
    /// Windows: a runtime whose binaries are Windows programs.
    Windows,
}

impl Platform {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Macos => "macos",
            Self::Web => "web",
            Self::TiloOs => "tilo-os",
            Self::Windows => "windows",
        }
    }
}

/// The targets of an app that does not declare `package.platforms`.
pub const DEFAULT_PLATFORMS: &[Platform] = &[Platform::Linux, Platform::Macos, Platform::Web];

/// The capability whose origin connector the session hands the workspace's account sign-ins.
pub const ACCOUNT_SIGN_INS: &str = "account-sign-ins";

/// The capability that lets an app open a file in the app that opens its kind (`File.open`),
/// beside Files, which opens one for the person. An app that reads the device's folders declares
/// it (`uses files`), and the session answers a document's apps and its open to an app only when
/// its signed manifest declares it: the manifest is the ceiling, never the app's request.
pub const FILES_OPEN: &str = "files.open";

/// The capability to carry an outside service's call into a workspace call: a bridge program
/// asks the device for a call lane, and only an app whose signed manifest declares this one is
/// given it. The workspace's grants still decide who may be in the call.
pub const CALLS: &str = "calls";

/// `[capabilities]` — host capabilities by name (`network.http`,
/// `notifications`, `files.save`, ...). A `required` capability that a host
/// cannot provide stops the launch; an `optional` one degrades.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub optional: Vec<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub reasons: std::collections::BTreeMap<String, String>,
}

/// Capabilities that lower the sandbox itself rather than reach a brokered
/// service, so a manifest must say why it declares one
/// ([`Capabilities::reasons`]). `io-uring` opens the kernel's `io_uring`
/// interface, which the app sandbox's syscall filter otherwise refuses.
pub const REASONED_CAPABILITIES: &[&str] = &["io-uring"];

impl Capabilities {
    fn is_empty(&self) -> bool {
        self.required.is_empty() && self.optional.is_empty() && self.reasons.is_empty()
    }

    /// Why the app declares `name`, if the manifest says.
    #[must_use]
    pub fn reason(&self, name: &str) -> Option<&str> {
        self.reasons.get(name).map(String::as_str)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub platform: String,
    pub architecture: String,
    pub isa: String,
    pub runtime: String,
    pub module: String,
}

/// `[image]` — an environment image's binding and contents.
///
/// ```toml
/// [image]
/// runtime = "9f3c…"   # tool only: the exact env-runtime package id (hex)
/// packages = ["pandoc-0:3.1.11.1-2.fc44.x86_64"]
/// ```
///
/// A `tool` holds the files its packages add on top of one exact
/// `env-runtime`; `runtime` names that package, and a tool is never stacked
/// on any other. An `env-runtime` names none.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<PackageId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accessibility {
    #[serde(default)]
    pub atspi_acceptance: Option<AtspiAcceptance>,
}

impl Accessibility {
    fn is_empty(&self) -> bool {
        self.atspi_acceptance.is_none()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtspiAcceptance {
    pub root: String,
    pub required: Vec<String>,
    pub action: String,
    pub after_action: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    pub id: String,
    /// The ids the app was published under before (`previously = ["so.tilo.lang.todo"]`):
    /// what it keeps of them (its records, grants and windows) is this app's. Signed with the
    /// manifest, and only honoured for an id the same publisher signed: installing it over an
    /// app of another publisher's is refused.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previously: Vec<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub names: std::collections::BTreeMap<String, String>,
    /// What else people call the app, keyed by BCP 47 tag:
    ///
    /// ```toml
    /// [package.keywords]
    /// en = ["youtube", "video", "videos"]
    /// de = ["filme"]
    /// ```
    ///
    /// Aliases, synonyms and the well-known products the app stands in for.
    /// Search matches every language's words whatever the desktop speaks,
    /// since a German desktop still gets typed "email". The words are
    /// authored in `apps/<app>/search/<tag>.ftl` and joined here when the
    /// app is packed (`tilo-app-package-cli pack`), never by hand.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub keywords: std::collections::BTreeMap<String, Vec<String>>,
    /// The app's tag line in each language, keyed by BCP 47 tag: one plain
    /// phrase saying what the app does, shown under its name in the Store and
    /// in search.
    ///
    /// ```toml
    /// [package.taglines]
    /// en = "Notes you write alone or together"
    /// de = "Notizen, allein oder gemeinsam geschrieben"
    /// ```
    ///
    /// Authored as the `app-tagline` message of `apps/<app>/search/<tag>.ftl`
    /// and joined here when the app is packed, like [`Self::keywords`].
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub taglines: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screenshots: Vec<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub version: String,
    pub kind: Kind,
    /// `<runtime-name>/<version>` of the Tilo runtime this binds to;
    /// `None` for `kind = "runtime"`. Version must parse as semver.
    ///
    /// A packed native app names the exact runtime build it was linked
    /// against, `tilo-runtime/<release>+native.<abi digest>`: its Rust
    /// dylibs have no stable ABI, so it loads on that build and no other.
    /// Packaging stamps the build over the authored release.
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provides: Option<String>,
    /// Further runtime bundles, `<runtime-name>/<version>` each, the app
    /// needs beside its `runtime`: the browser's web engine is one. They are
    /// part of the app: installing or updating it installs them in the same
    /// step or not at all, and the launcher mounts each read-only at
    /// `/run/tilo-runtime/<version>` and refuses to start the app without
    /// one.
    ///
    /// The manifest key is still `optional-runtimes`: renaming it would
    /// change the package format every device and registry reads.
    #[serde(
        default,
        rename = "optional-runtimes",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub required_runtimes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<Platform>>,
    /// One-line reason the app narrows `platforms`; required exactly when
    /// the list leaves out a default platform.
    #[serde(
        default,
        rename = "platform-reason",
        skip_serializing_if = "Option::is_none"
    )]
    pub platform_reason: Option<String>,
    /// Minimum CPU feature-level per architecture this build's binaries
    /// require, one entry per supported architecture:
    ///
    /// ```toml
    /// [package.isa]
    /// x86_64 = "v3"
    /// aarch64 = "v8.2-a"
    /// ```
    ///
    /// A missing entry for the host's arch means "no build available";
    /// `tilo-install` refuses the install with a clear error.
    #[serde(default)]
    pub isa: IsaRequirements,
    /// `[package.dev]` — this package is a development build: the device whose developer built
    /// and signed it, and the app it is a build of. It runs under an id of its own
    /// ([`dev_app_id`]), so it never takes an installed app's identity or grants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dev: Option<DevBuild>,
}

/// `[package.dev]` — what makes a package a development build: the key of the device that built
/// and signed it, and the app whose sources it was built from.
///
/// A dev build is installed only by the device whose key it names, which refuses one signed by
/// another device or not signed at all.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevBuild {
    /// The device that built it, as `ed25519:<hex>`: the key whose signature the package's root
    /// carries, and the only device that installs it.
    pub device: String,
    /// The id of the app this is a development build of.
    pub of: String,
}

impl DevBuild {
    /// The device that built this, as key bytes.
    ///
    /// # Errors
    ///
    /// See [`Publisher::identity_bytes`].
    pub fn device_bytes(&self) -> Result<[u8; 32], IdentityParseError> {
        parse_ed25519_hex(&self.device)
    }
}

/// The id a development build of `app` runs under on the device whose key is `device`: a name of
/// its own, marked as that device's build, so a dev build never takes an installed app's id or
/// the grants given to it.
#[must_use]
pub fn dev_app_id(app: &str, device: &[u8; 32]) -> String {
    format!("{app}.dev.{}", hex::encode(&device[..8]))
}

impl Package {
    /// Whether this package is a development build.
    #[must_use]
    pub fn is_dev(&self) -> bool {
        self.dev.is_some()
    }
}

/// The text `texts` (a package's names or tag lines, by language tag) shows a reader of
/// `locale`: its own tag, else another region of its language, else English; `None` when it has
/// none of them ([`tilo_paths::matching`]).
#[must_use]
pub fn text_in<'a>(
    texts: &'a std::collections::BTreeMap<String, String>,
    locale: &str,
) -> Option<&'a str> {
    tilo_paths::matching(locale, texts.keys().map(String::as_str))
        .or_else(|| texts.keys().position(|tag| tag.eq_ignore_ascii_case("en")))
        .and_then(|position| texts.values().nth(position))
        .map(String::as_str)
}

impl Package {
    /// The tag line to show in `locale` (BCP 47): its tag, else its language, else English
    /// ([`text_in`]); `None` when the app declares none.
    #[must_use]
    pub fn tagline_for(&self, locale: &str) -> Option<&str> {
        text_in(&self.taglines, locale).filter(|tagline| !tagline.trim().is_empty())
    }

    #[must_use]
    pub fn native_runtime_binding(&self) -> Option<&str> {
        if self.kind == Kind::Runtime {
            self.provides.as_deref()
        } else {
            self.runtime.as_deref()
        }
    }

    #[must_use]
    pub fn platforms(&self) -> &[Platform] {
        self.platforms.as_deref().unwrap_or(DEFAULT_PLATFORMS)
    }

    #[must_use]
    pub fn is_platform_narrowed(&self) -> bool {
        let platforms = self.platforms();
        DEFAULT_PLATFORMS
            .iter()
            .any(|platform| !platforms.contains(platform))
    }

    #[must_use]
    pub fn supports(&self, platform: Platform) -> bool {
        self.platforms().contains(&platform)
    }

    /// Whether a build of this app may be published for a registry platform
    /// (the registry's names are the manifest's). A Linux `.tapp` may be
    /// published for `tilo-os` whether the app declares `linux` or
    /// `tilo-os`; every other platform must be declared as named.
    #[must_use]
    pub fn publishable_as(&self, registry_platform: &str) -> bool {
        match registry_platform {
            "tilo-os" => self.supports(Platform::TiloOs) || self.supports(Platform::Linux),
            "linux" => self.supports(Platform::Linux),
            "macos" => self.supports(Platform::Macos),
            "web" => self.supports(Platform::Web),
            "windows" => self.supports(Platform::Windows),
            _ => false,
        }
    }
}

/// Per-arch minimum-feature-level requirements; every field is
/// optional (a publisher who only ships `x86_64` leaves aarch64 unset).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IsaRequirements {
    #[serde(default)]
    pub x86_64: Option<X86_64Level>,
    #[serde(default)]
    pub aarch64: Option<AArch64Level>,
}

impl IsaRequirements {
    /// The requirement for the given architecture; `None` = no build
    /// declared, caller refuses the install.
    pub fn for_arch(&self, arch: Arch) -> Option<IsaLevel> {
        match arch {
            Arch::X86_64 => self.x86_64.map(IsaLevel::X86_64),
            Arch::AArch64 => self.aarch64.map(IsaLevel::AArch64),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.x86_64.is_none() && self.aarch64.is_none()
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    X86_64,
    AArch64,
}

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

    pub fn wire_label(self) -> &'static str {
        match self {
            IsaLevel::X86_64(l) => l.wire_label(),
            IsaLevel::AArch64(l) => l.wire_label(),
        }
    }

    /// Ranks within an arch: comparing a level of one arch with another panics.
    pub fn rank(self) -> u8 {
        match self {
            IsaLevel::X86_64(l) => l.rank(),
            IsaLevel::AArch64(l) => l.rank(),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum X86_64Level {
    #[serde(alias = "x86-64")]
    #[serde(alias = "baseline")]
    Baseline,
    #[serde(rename = "v2")]
    #[serde(alias = "x86-64-v2")]
    V2,
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

/// ARMv8/v9-A profile levels, condensed from the ARM ARM's profile
/// designators to the four that map cleanly to shipping silicon.
///
/// Deliberately excluded: SVE vector width (SVE2 is width-agnostic)
/// and vendor extras (Apple AMX, Qualcomm MSA) — those are per-app
/// opt-in `target-cpu` selections, not a tier bump.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AArch64Level {
    #[serde(rename = "armv8-a")]
    #[serde(alias = "v8-a")]
    #[serde(alias = "aarch64")]
    #[serde(alias = "baseline")]
    Armv8A,
    #[serde(rename = "armv8.2-a")]
    #[serde(alias = "v8.2-a")]
    Armv8_2A,
    #[serde(rename = "armv8.4-a")]
    #[serde(alias = "v8.4-a")]
    Armv8_4A,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Publisher {
    /// The publisher's Ed25519 public key as `ed25519:<hex>` (canonical lower-case hex).
    pub identity: String,
    /// Detached signature over `signing_bytes(manifest, payload)`; empty until `tilo-app-package sign` fills it.
    #[serde(default)]
    pub signature: String,
}

impl Publisher {
    /// Parse the hex part out of an `ed25519:<hex>` identity string into raw key bytes.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityParseError::BadPrefix`] if `identity` isn't
    /// `ed25519:<hex>`, [`IdentityParseError::Hex`] if the hex is
    /// malformed, or [`IdentityParseError::WrongLength`] if it doesn't
    /// decode to exactly 32 bytes.
    pub fn identity_bytes(&self) -> Result<[u8; 32], IdentityParseError> {
        parse_ed25519_hex(&self.identity)
    }

    /// Parse the hex signature into 64 raw bytes; `Ok(None)` if unsigned.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityParseError::Hex`] if the signature is not valid
    /// hex, or [`IdentityParseError::WrongLength`] if it doesn't decode to
    /// exactly 64 bytes.
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

/// `[trust]` — optional third-party attestations; the loader checks
/// these before granting elevated capabilities.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trust {
    #[serde(default, rename = "attested_by")]
    pub attested_by: Vec<TrustAttestation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustAttestation {
    pub attestor: String,
    /// Opaque, hex-encoded claim bytes; format is attestor-specific.
    #[serde(rename = "claim")]
    pub claim_hex: String,
    /// Attestor's Ed25519 signature over the canonical attestation
    /// preimage (see [`crate::trust::attestation_preimage`]), hex-encoded;
    /// empty for legacy claim-only rows.
    #[serde(default, rename = "signature")]
    pub signature_hex: String,
}

/// `[continuity]` — minimum facets a continuation payload must contain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuityContract {
    #[serde(default = "default_continuity_facets")]
    pub required: Vec<ContinuityFacet>,
    #[serde(default, rename = "semantic-schema")]
    pub semantic_schema: Option<ContinuitySemanticSchema>,
}

impl Default for ContinuityContract {
    fn default() -> Self {
        Self {
            required: default_continuity_facets(),
            semantic_schema: None,
        }
    }
}

fn default_continuity_facets() -> Vec<ContinuityFacet> {
    vec![ContinuityFacet::Presentation]
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContinuityFacet {
    Presentation,
    /// App-owned resource identity and domain state, sanitized for roaming.
    Semantic,
}

/// Portable semantic projections understood by the platform runtime.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContinuitySemanticSchema {
    WebLocationV1,
    PublicationPositionV1,
    MediaPositionV1,
    RealtimeSessionV1,
    TextPositionV1,
}

impl TrustAttestation {
    /// Parse the signature hex into raw bytes; `Ok(None)` when empty.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityParseError::Hex`] if `signature_hex` is not valid
    /// hex, or [`IdentityParseError::WrongLength`] if it doesn't decode to
    /// exactly 64 bytes.
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

/// `[actions]` — what an application declared it may do on the session's action
/// API under its own identity.
///
/// The declaration is a ceiling, never a grant: invoking another app's action
/// still needs the workspace to grant it to the app.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionAccess {
    /// Read the catalogue of actions the person using the app is offered:
    /// labels, typed parameters and contracts, never their data.
    #[serde(default)]
    pub query: bool,
    /// Invoke actions other apps publish, as this app.
    #[serde(default)]
    pub invoke: bool,
}

impl ActionAccess {
    pub fn is_empty(&self) -> bool {
        !self.query && !self.invoke
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sandbox {
    pub namespaces: Vec<String>,
    pub seccomp: String,
    /// Landlock strictness — `strict` | `lax` | `off`.
    pub landlock: String,
    /// `[sandbox.net]` block — egress policy.
    pub net: SandboxNet,
    /// `[sandbox.portals]`: a [`PortalPolicy`] per name in [`PORTALS`]. The launcher's bus proxy
    /// refuses every portal the app does not allow here; notifications need no entry.
    #[serde(default)]
    pub portals: std::collections::BTreeMap<String, PortalPolicy>,
    #[serde(default)]
    pub services: std::collections::BTreeMap<String, ServiceReq>,
}

/// `[sandbox.net]` — egress policy. `mode` is one of
/// `off|host|dns-only|allowlist`; `hosts` carries allowed names only
/// when `mode = "allowlist"`, otherwise empty/ignored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxNet {
    pub mode: NetMode,
    /// Only meaningful when `mode = "allowlist"`.
    #[serde(default)]
    pub hosts: Vec<String>,
    /// `calls = true`: audio and video over UDP to any public internet
    /// address, carried by the egress proxy only while a call of this app
    /// is live. TCP stays the hosts above.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub calls: bool,
}

impl SandboxNet {
    /// Convenience: hosts only if mode is Allowlist, else `None`.
    pub fn allowlist_hosts(&self) -> Option<&[String]> {
        if matches!(self.mode, NetMode::Allowlist) {
            Some(&self.hosts)
        } else {
            None
        }
    }
}

/// Every name `[sandbox.portals]` accepts, with the `org.freedesktop.portal` interface a grant opens
/// on the session bus. `microphone` has no bus portal: the runtime captures the device for the app.
pub const PORTALS: &[(&str, Option<&str>)] = &[
    ("camera", Some("Camera")),
    ("file-chooser", Some("FileChooser")),
    ("location", Some("Location")),
    ("microphone", None),
    ("network-monitor", Some("NetworkMonitor")),
    ("open-uri", Some("OpenURI")),
    ("screencast", Some("ScreenCast")),
    ("secrets", Some("Secret")),
    ("wallpaper", Some("Wallpaper")),
];

/// Whether an app may reach a portal (`via-host`) or not (`deny`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PortalPolicy {
    ViaHost,
    /// The portal is explicitly denied — same on-wire behaviour as
    /// omitting the entry (`ashpd` surfaces `PortalError::NotAvailable`),
    /// but signals intent for auditors.
    Deny,
}

/// Egress modes; kebab-case wire form matches `mode = "off"/"host"/"dns-only"/"allowlist"` exactly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetMode {
    Off,
    /// Full host network (only for `runtime` / privileged apps).
    Host,
    /// DNS resolution only — for apps that surface URLs but rely on the host browser to follow them.
    DnsOnly,
    Allowlist,
}

impl NetMode {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Host => "host",
            Self::DnsOnly => "dns-only",
            Self::Allowlist => "allowlist",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceReq {
    pub protocol: String,
    pub requirement: Requirement,
    /// Protocol version pin; `1` if the manifest wrote no `@N`.
    pub version: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Requirement {
    Required,
    Optional,
}

impl ServiceReq {
    /// Parse `"required@1"`/`"optional@2"`/`"required"` (defaults version to 1); rejects anything else.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceReqParseError::BadVersion`] if the text after `@`
    /// isn't a valid integer, or [`ServiceReqParseError::BadSeverity`] if
    /// the severity is neither `required` nor `optional`.
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

    /// Whether the app cannot run without the service.
    #[must_use]
    pub fn is_required(&self) -> bool {
        self.requirement == Requirement::Required
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ServiceReqParseError {
    #[error("unknown severity {got:?} (expected `required` or `optional`)")]
    BadSeverity { got: String },
    #[error("bad version in {raw:?}, expected integer after `@`")]
    BadVersion { raw: String },
}

// TOML encodes ServiceReq as a string ("required@1"); to keep
// `services` deserialisable from a flat table, Serialize/Deserialize are implemented manually.
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entrypoint {
    #[serde(default)]
    pub bin: PathBuf,
    #[serde(default, rename = "type")]
    pub r#type: EntrypointType,
    /// GUI-flavour binary path; only meaningful when `type = "both"`.
    #[serde(default, rename = "gui-bin")]
    pub gui_bin: Option<PathBuf>,
    /// CLI-flavour binary path; only meaningful when `type = "both"`.
    #[serde(default, rename = "cli-bin")]
    pub cli_bin: Option<PathBuf>,
    /// Splash-segment hints: byte ranges the loader prefetches so the
    /// splash renders before the full binary is cached.
    #[serde(default, rename = "splash-segments")]
    pub splash_segments: Vec<SplashSegment>,
    /// Soft budget (ms) for the splash before the loader falls back to default display.
    #[serde(default = "default_splash_budget", rename = "splash-budget")]
    pub splash_budget_ms: u32,
    /// The runtime bundle (`<name>/<version>`) whose `bin` this entrypoint
    /// executes, instead of a binary in the package's own payload. The
    /// package then carries only what that runtime runs (a tilolang app's
    /// compiled module), and the launcher tells the runtime where it is
    /// (`TILO_APP_MODULE`). It must be one of `[package].optional-runtimes`,
    /// the app's required runtimes, so it installs with the app like every
    /// other one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// The program the runtime's binary runs, a file of the package's own payload: a JavaScript
    /// file for `bun`, a Python one for `python`. The launcher hands it to the binary as its first
    /// argument, at the payload's place in the sandbox. Only for an entrypoint that names a
    /// `runtime`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<PathBuf>,
    /// The outside services whose resources the app's types hold, by origin (`imap`): the
    /// system runs a connector process for each, in the app's sandbox, whether or not a window
    /// of the app is open. Only for an entrypoint that names a `runtime`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub origins: Vec<String>,
    /// The app keeps a long-lived connection to an outside service (a chat bridge's gateway, a
    /// mail account's fetching): exactly one device of each workspace runs its process with no
    /// window, one that has the app installed and gives it every capability it requires, and
    /// another takes it over when that device goes silent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub connection: bool,
    /// What a window of the app is to other devices ([`WindowKind`]). Left out, it is a view of
    /// records, which every device opens for itself.
    #[serde(default, skip_serializing_if = "WindowKind::is_view")]
    pub window: WindowKind,
}

impl Entrypoint {
    /// The binary the GUI launcher should exec; falls back to `bin` if unset.
    pub fn gui_bin_path(&self) -> Option<&PathBuf> {
        match self.r#type {
            EntrypointType::Gui => Some(&self.bin),
            EntrypointType::Cli => None,
            EntrypointType::Both => self.gui_bin.as_ref().or(Some(&self.bin)),
        }
    }

    /// The binary a CLI command should exec; falls back to `bin` if unset.
    pub fn cli_bin_path(&self) -> Option<&PathBuf> {
        match self.r#type {
            EntrypointType::Gui => None,
            EntrypointType::Cli => Some(&self.bin),
            EntrypointType::Both => self.cli_bin.as_ref().or(Some(&self.bin)),
        }
    }
}

/// What a window of an app is to the person's other devices and the workspace's other members;
/// wire form is lowercase (`view`/`device`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowKind {
    /// A view of records the workspace holds. A window one device opens is one every other
    /// device opens for itself, on the same records.
    #[default]
    View,
    /// What the window shows lives in the app's process on the device that opened it: a page in
    /// a web engine, a terminal's session, another machine's display. The window runs only on
    /// that device. Other devices list it as a window that runs there and start nothing for it;
    /// they see it when it is shared, and act on it through that device.
    Device,
}

impl WindowKind {
    /// Whether this is the kind a manifest need not say.
    #[must_use]
    pub fn is_view(&self) -> bool {
        matches!(self, Self::View)
    }
}

/// Which entrypoint flavour the package exposes; wire form is lowercase (`gui`/`cli`/`both`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntrypointType {
    #[default]
    Gui,
    /// Terminal / stdio-only app.
    Cli,
    Both,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cli {
    /// PATH-visible command names; empty vec means no PATH commands.
    #[serde(default)]
    pub commands: Vec<String>,
}

fn default_splash_budget() -> u32 {
    100
}

/// A path with optional byte range for `splash-segments`, e.g.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplashSegment {
    pub path: PathBuf,
    /// `(start, end)` inclusive-start, exclusive-end byte range; `None` means the whole file.
    pub byte_range: Option<(u64, u64)>,
}

impl SplashSegment {
    /// Parse the wire form.
    ///
    /// # Errors
    ///
    /// Returns [`SplashSegmentError::BadShape`] if `raw` isn't
    /// `path[start:end]` or a plain path, [`SplashSegmentError::BadInt`] if
    /// `start`/`end` aren't valid decimal or `0x`-prefixed hex integers, and
    /// [`SplashSegmentError::EmptyRange`] if `end < start`.
    pub fn parse(raw: &str) -> Result<Self, SplashSegmentError> {
        if let Some(open) = raw.rfind('[') {
            let close = raw
                .rfind(']')
                .ok_or_else(|| SplashSegmentError::BadShape(raw.to_string()))?;
            if close <= open {
                return Err(SplashSegmentError::BadShape(raw.to_string()));
            }
            let path_str = raw
                .get(..open)
                .ok_or_else(|| SplashSegmentError::BadShape(raw.to_string()))?;
            let path = PathBuf::from(path_str);
            let inner_start = open
                .checked_add(1)
                .ok_or_else(|| SplashSegmentError::BadShape(raw.to_string()))?;
            let inner = raw
                .get(inner_start..close)
                .ok_or_else(|| SplashSegmentError::BadShape(raw.to_string()))?;
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resources {
    #[serde(default)]
    pub kinds: Vec<String>,
    /// The kinds of [`Self::kinds`] of which each member of a workspace keeps one record that
    /// only they read and change, whatever their role: what they watched, what they are writing
    /// and have not sent. The workspace's policy decides them by that reader rule.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub private: Vec<String>,
}

impl Resources {
    fn is_empty(&self) -> bool {
        self.kinds.is_empty() && self.private.is_empty()
    }
}

/// One `[[offers]]` entry: an action the app publishes at a stable address,
/// offered while the app is not running.
///
/// ```toml
/// [[offers]]
/// resource = "calendar.calendar"
/// action = "show-tasks"
/// label = "Show tasks"
/// kind = "read"
/// operation = "view"
/// ```
///
/// `tilolang surface` writes them from what the app's code publishes on an
/// empty document: the actions people are offered with nothing chosen that
/// take nothing, or one line of typed text ([`Self::asks`]). The running app
/// publishes the full typed contract, and that is what admits an invocation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferedAction {
    pub resource: String,
    /// The kind of that resource; the resource itself when unset.
    #[serde(
        default,
        rename = "resource-kind",
        skip_serializing_if = "Option::is_none"
    )]
    pub resource_kind: Option<String>,
    pub action: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub kind: OfferKind,
    #[serde(default)]
    pub operation: OfferOperation,
    /// Whether a person confirms the action before it runs.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub confirm: bool,
    /// The values it asks for before it runs: one name for an action taking one line, or each
    /// value with the type and words a small form shows ([`Asked`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asks: Option<Asked>,
    /// The other words people use for it, which search ranks as it ranks the label.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// Its picture, by its Lucide name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

impl OfferedAction {
    #[must_use]
    pub fn resource_kind(&self) -> &str {
        self.resource_kind.as_deref().unwrap_or(&self.resource)
    }

    /// The values it asks for, in the order a form gives them.
    #[must_use]
    pub fn asked(&self) -> Vec<AskedParam> {
        match &self.asks {
            None => Vec::new(),
            Some(Asked::One(name)) => vec![AskedParam {
                name: name.clone(),
                ty: "text".to_owned(),
                description: None,
            }],
            Some(Asked::Many(params)) => params.clone(),
        }
    }
}

/// What an offered action asks of whoever runs it: a name for an action taking one line, or each
/// value's name, type and the words beside its field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Asked {
    One(String),
    Many(Vec<AskedParam>),
}

/// One value an offer asks for, as a small form shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskedParam {
    pub name: String,
    /// What it takes, as [`tilo_actions::ParamType`] spells it (`text`, `secret`, `integer`…).
    #[serde(rename = "type", default = "text_type")]
    pub ty: String,
    /// The words a person reads beside the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

fn text_type() -> String {
    "text".to_owned()
}

/// What an offered action is able to do: read, change only the document, or
/// reach outside it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OfferKind {
    Read,
    #[default]
    Undoable,
    Outside,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OfferOperation {
    Document,
    #[default]
    Effect,
    View,
}

/// `[providers]` — normalized resources an app can open, or codec MIME scope
/// for `kind = "format-provider"`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Providers {
    #[serde(default)]
    pub mime: Vec<String>,
    #[serde(default)]
    pub ext: Vec<String>,
    #[serde(default)]
    pub schemes: Vec<String>,
    /// The app is a tool someone picks for these resources (an editor that
    /// imports video) rather than where they open by default: it is offered
    /// under Open with, and another app claiming the same resource without
    /// this flag is the default. When nothing else claims a resource it is
    /// still the default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub secondary: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<String>,
}

impl Providers {
    fn normalize(&mut self) {
        self.resources = normalize_unique(&self.resources, |value| value.trim().to_owned());
        self.mime = normalize_unique(&self.mime, |value| value.trim().to_ascii_lowercase());
        self.ext = normalize_unique(&self.ext, |value| {
            value.trim().trim_start_matches('.').to_ascii_lowercase()
        });
        self.schemes = normalize_unique(&self.schemes, |value| {
            value.trim().trim_end_matches(':').to_ascii_lowercase()
        });
    }
}

fn normalize_unique(values: &[String], normalize: impl Fn(&str) -> String) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        let normalized = normalize(value);
        if !out.contains(&normalized) {
            out.push(normalized);
        }
    }
    out
}

impl Manifest {
    /// Whether a window of this app runs only on the device that opened it
    /// (`[entrypoint] window = "device"`, [`WindowKind::Device`]).
    #[must_use]
    pub fn window_runs_on_its_device(&self) -> bool {
        self.entrypoint
            .as_ref()
            .is_some_and(|entrypoint| entrypoint.window == WindowKind::Device)
    }

    /// Decode a manifest from TOML source.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestParseError::Toml`] if `s` is not valid TOML for
    /// this schema.
    pub fn from_toml_str(s: &str) -> Result<Self, ManifestParseError> {
        let m: Manifest = toml::from_str(s).map_err(ManifestParseError::Toml)?;
        Ok(m.normalized())
    }

    /// What every decode applies after serde: idempotent, so a manifest
    /// that already went through [`Manifest::from_toml_str`] is unchanged.
    pub(crate) fn normalized(mut self) -> Self {
        // TOML encodes [sandbox.services] as service-name → "required@1"
        // string; the generic decode loses the key, so patch each
        // ServiceReq's `protocol` field with the map key that named it.
        let services = std::mem::take(&mut self.sandbox.services);
        self.sandbox.services = services
            .into_iter()
            .map(|(name, mut req)| {
                req.protocol.clone_from(&name);
                (name, req)
            })
            .collect();
        self.providers.normalize();
        self
    }

    /// The system interfaces this package cannot work without: each
    /// `required@N` service, sorted by name. What a registry version records
    /// as its target's `interfaces`, and what a device checks against the
    /// versions its OS serves before installing or running the package.
    #[must_use]
    pub fn required_interfaces(&self) -> Vec<registry_types::InterfaceRequirement> {
        self.sandbox
            .services
            .iter()
            .filter(|(_, req)| req.requirement == Requirement::Required)
            .map(|(name, req)| registry_types::InterfaceRequirement {
                name: name.clone(),
                version: req.version,
            })
            .collect()
    }

    /// Every interface version this package speaks: each service it names,
    /// required or optional, sorted by name. An optional service may be
    /// absent where the package runs, but where it is present the package
    /// speaks only this version of it, so a device runs the package only
    /// when it serves every one of these exactly
    /// (docs/architecture/compatibility.md).
    #[must_use]
    pub fn interface_versions(&self) -> Vec<registry_types::InterfaceRequirement> {
        self.sandbox
            .services
            .iter()
            .map(|(name, req)| registry_types::InterfaceRequirement {
                name: name.clone(),
                version: req.version,
            })
            .collect()
    }

    /// [`Manifest::required_interfaces`] in the registry's wire form,
    /// `<name>@<version>`.
    #[must_use]
    pub fn required_interface_wire(&self) -> Vec<String> {
        self.required_interfaces()
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    /// Re-encode to TOML; used for round-trip tests and `tilo-app-package inspect`.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestParseError::Encode`] if serialisation fails.
    pub fn to_toml_string(&self) -> Result<String, ManifestParseError> {
        toml::to_string_pretty(self).map_err(ManifestParseError::Encode)
    }

    /// The bytes a manifest's `[publisher] signature` covers: the manifest as it reads with that
    /// signature cleared. A package signs it as part of its signed root; a host packaged without
    /// a payload (an APK) signs it alone, so what it says of itself — the runtime build it links —
    /// is its build's own and nothing rewritten in place.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestParseError::Encode`] if serialisation fails.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, ManifestParseError> {
        let mut unsigned = self.clone();
        unsigned.publisher.signature.clear();
        unsigned.to_toml_string().map(String::into_bytes)
    }

    /// Sign this manifest with `identity`'s key, which must be the publisher the manifest names,
    /// over [`Self::signing_bytes`].
    ///
    /// # Errors
    ///
    /// The manifest names another publisher, or cannot be serialised.
    pub fn sign(&mut self, identity: &crate::pack::SigningIdentity) -> Result<(), String> {
        let named = self
            .publisher
            .identity_bytes()
            .map_err(|error| format!("this manifest names no publisher: {error}"))?;
        if named != identity.public_key_bytes() {
            return Err("this manifest names another publisher than the key signing it".to_owned());
        }
        let bytes = self
            .signing_bytes()
            .map_err(|error| format!("this manifest does not encode: {error}"))?;
        self.publisher.signature = hex::encode(identity.sign_message(&bytes));
        Ok(())
    }

    /// Whether `[publisher] signature` is the publisher's over the manifest as it reads: a host
    /// trusts the runtime its own signed manifest names, and nothing else, for what it carries.
    ///
    /// # Errors
    ///
    /// The manifest is unsigned or names no usable publisher, or the signature is not its
    /// publisher's over these bytes.
    pub fn verify_signature(&self) -> Result<(), String> {
        let publisher = self
            .publisher
            .identity_bytes()
            .map_err(|error| format!("this manifest names no publisher: {error}"))?;
        let signature = self
            .publisher
            .signature_bytes()
            .map_err(|error| format!("this manifest's signature does not read: {error}"))?
            .ok_or("this manifest is unsigned")?;
        let bytes = self
            .signing_bytes()
            .map_err(|error| format!("this manifest does not encode: {error}"))?;
        let key = ed25519_dalek::VerifyingKey::from_bytes(&publisher)
            .map_err(|error| format!("this manifest names no publisher key: {error}"))?;
        key.verify_strict(&bytes, &ed25519_dalek::Signature::from_bytes(&signature))
            .map_err(|error| format!("this manifest is not its publisher's: {error}"))
    }
}

#[cfg(test)]
mod accessibility_tests {
    use super::*;

    /// The smallest manifest an app decodes from; the optional tables are
    /// appended to it.
    fn bare() -> &'static str {
        r#"
[package]
id = "so.tilo.example"
version = "0.1.0"
kind = "app"

[publisher]
identity = "ed25519:0000000000000000000000000000000000000000000000000000000000000000"
signature = ""

[sandbox]
namespaces = []
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"
"#
    }

    /// The manifest with `tail` appended, decoded and then decoded again from
    /// its own encoding: what the signer writes, the reader reads.
    fn round_trip(tail: &str) -> (Manifest, String) {
        let manifest = Manifest::from_toml_str(&format!("{}\n{tail}", bare())).unwrap();
        let encoded = manifest.to_toml_string().unwrap();
        let recovered = Manifest::from_toml_str(&encoded).unwrap();
        assert_eq!(recovered, manifest, "{tail}");
        (manifest, encoded)
    }

    tilo_test_env::scenario! {
        fn optional_declarations_survive_the_signed_manifest_round_trip;

        phase atspi_acceptance_survives_the_signed_manifest_round_trip;
        {
            let (manifest, encoded) = round_trip(
                "[entrypoint]\nbin = \"bin/example\"\ntype = \"gui\"\n\
                 [accessibility.atspi_acceptance]\nroot = \"Example\"\n\
                 required = [\"Open\"]\naction = \"Open\"\nafter_action = [\"Close\"]\n",
            );
            assert_eq!(
                manifest.accessibility,
                Accessibility {
                    atspi_acceptance: Some(AtspiAcceptance {
                        root: "Example".into(),
                        required: vec!["Open".into()],
                        action: "Open".into(),
                        after_action: vec!["Close".into()],
                    }),
                }
            );
            assert!(encoded.contains("[accessibility.atspi_acceptance]"));
        }

        phase declared_action_access_survives_the_signed_manifest_round_trip;
        {
            let bare_encoded = Manifest::from_toml_str(bare())
                .unwrap()
                .to_toml_string()
                .unwrap();
            assert!(!bare_encoded.contains("[actions]"), "{bare_encoded}");

            for tail in [
                "",
                "[actions]\n",
                "[actions]\nquery = false\ninvoke = false\n",
            ] {
                let (undeclared, encoded) = round_trip(tail);
                assert_eq!(
                    undeclared.actions,
                    ActionAccess {
                        query: false,
                        invoke: false,
                    },
                    "{tail}"
                );
                assert_eq!(undeclared.actions, ActionAccess::default(), "{tail}");
                assert_eq!(encoded, bare_encoded, "{tail}");
            }

            for (table, expected, written) in [
                (
                    "[actions]\nquery = true\n",
                    ActionAccess {
                        query: true,
                        invoke: false,
                    },
                    "\n[actions]\nquery = true\ninvoke = false\n\n",
                ),
                (
                    "[actions]\ninvoke = true\n",
                    ActionAccess {
                        query: false,
                        invoke: true,
                    },
                    "\n[actions]\nquery = false\ninvoke = true\n\n",
                ),
                (
                    "[actions]\nquery = true\ninvoke = true\n",
                    ActionAccess {
                        query: true,
                        invoke: true,
                    },
                    "\n[actions]\nquery = true\ninvoke = true\n\n",
                ),
            ] {
                let (declared, encoded) = round_trip(table);
                assert_eq!(declared.actions, expected, "{table}");
                assert!(encoded.contains(written), "{encoded}");
            }
        }

        phase declared_resource_kinds_survive_the_signed_manifest_round_trip;
        {
            // A manifest from before the field declares no kinds and encodes,
            // so signs, exactly as it did.
            let undeclared = Manifest::from_toml_str(bare()).unwrap();
            assert!(undeclared.resources.kinds.is_empty());
            assert!(!undeclared.to_toml_string().unwrap().contains("[resources]"));

            let (declared, _) =
                round_trip("[resources]\nkinds = [\"example.note\", \"example.todo\"]\n");
            assert_eq!(declared.resources.kinds, ["example.note", "example.todo"]);

            let (private, _) = round_trip(
                "[resources]\nkinds = [\"example.note\", \"example.draft\"]\n\
                 private = [\"example.draft\"]\n",
            );
            assert_eq!(private.resources.private, ["example.draft"]);
        }

        phase offered_actions_survive_the_signed_manifest_round_trip;
        {
            // A manifest from before the table offers nothing and encodes, so
            // signs, exactly as it did.
            let undeclared = Manifest::from_toml_str(bare()).unwrap();
            assert!(undeclared.offers.is_empty());
            assert!(!undeclared.to_toml_string().unwrap().contains("offers"));

            let (declared, _) = round_trip(
                "[[offers]]\nresource = \"example.list\"\naction = \"show-done\"\n\
                 label = \"Show done\"\nkind = \"read\"\noperation = \"view\"\n\
                 [[offers]]\nresource = \"example.list\"\nresource-kind = \"example.todo\"\n\
                 action = \"clear-done\"\nlabel = \"Clear done\"\nconfirm = true\n",
            );
            assert_eq!(
                declared.offers,
                [
                    OfferedAction {
                        resource: "example.list".into(),
                        resource_kind: None,
                        action: "show-done".into(),
                        label: "Show done".into(),
                        description: None,
                        kind: OfferKind::Read,
                        operation: OfferOperation::View,
                        confirm: false,
                        asks: None,
                        keywords: Vec::new(),
                        icon: None,
                    },
                    OfferedAction {
                        resource: "example.list".into(),
                        resource_kind: Some("example.todo".into()),
                        action: "clear-done".into(),
                        label: "Clear done".into(),
                        description: None,
                        kind: OfferKind::Undoable,
                        operation: OfferOperation::Effect,
                        confirm: true,
                        asks: None,
                        keywords: Vec::new(),
                        icon: None,
                    },
                ]
            );
            assert_eq!(declared.offers[0].resource_kind(), "example.list");
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestParseError {
    #[error("TOML decode failed: {0}")]
    Toml(toml::de::Error),
    #[error("TOML encode failed: {0}")]
    Encode(toml::ser::Error),
}
