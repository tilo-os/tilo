//! Execution targets a registry version may bind, and their canonical
//! spellings.
//!
//! A [`RegistryTarget`] is signed verbatim, so every accepted spelling must
//! be canonical: two publishers describing the same contract must produce the
//! same bytes, and a client must be able to decide support from the string
//! alone.
//!
//! Six platform families are accepted:
//!
//! - `tilo-os` — native thin apps; `architecture` is `x86_64` or `aarch64`,
//!   `isa` is the native feature level and `runtime`, when present, is the
//!   exact runtime build the app links against ([`parse_native_runtime`]).
//!   A thin app links Rust dylibs, which have no stable ABI, so the release
//!   name alone (`tilo-runtime/2026.07`) says nothing about whether a device's
//!   runtime can load it; only the build digest does.
//! - `linux`, `windows` and `macos` — the same app as a self-contained native
//!   executable for another desktop, under the same architecture and `isa`
//!   rules. A `linux` build is for Linux desktops other than Tilo OS, which
//!   installs only `tilo-os` builds.
//! - `android` — the same app as an Android package (an APK, or an AAB for
//!   store upload) for `aarch64` or `x86_64` devices, under the same
//!   architecture and `isa` rules. Tilo OS never installs it.
//! - `web` — WebAssembly side modules loaded by the browser toolkit core;
//!   `architecture` is `wasm32`, `isa` is a canonical feature-set string
//!   ([`parse_wasm_isa`]) and `runtime` is required and is the exact core
//!   build ABI id ([`parse_web_runtime`]).

use crate::entry::RegistryTarget;

pub const TILO_OS_PLATFORM: &str = "tilo-os";
pub const LINUX_PLATFORM: &str = "linux";
pub const WINDOWS_PLATFORM: &str = "windows";
pub const MACOS_PLATFORM: &str = "macos";
pub const ANDROID_PLATFORM: &str = "android";
pub const WEB_PLATFORM: &str = "web";

/// File extensions an `android` build's `module` may carry: an installable
/// APK, or an Android App Bundle.
pub const ANDROID_PACKAGE_EXTENSIONS: [&str; 2] = ["apk", "aab"];

/// Families whose versions are native executables for `x86_64` or `aarch64`.
pub const NATIVE_PLATFORMS: [&str; 5] = [
    TILO_OS_PLATFORM,
    LINUX_PLATFORM,
    WINDOWS_PLATFORM,
    MACOS_PLATFORM,
    ANDROID_PLATFORM,
];

/// Native families outside Tilo OS — the other desktops and Android: the
/// executable links the toolkit in, and its signed manifest carries a
/// `[target]` naming the version exactly.
pub const DESKTOP_PLATFORMS: [&str; 4] = [
    LINUX_PLATFORM,
    WINDOWS_PLATFORM,
    MACOS_PLATFORM,
    ANDROID_PLATFORM,
];
pub const WASM32_ARCHITECTURE: &str = "wasm32";

/// Every `isa` of the `web` platform starts with this baseline: the
/// WebAssembly 1.0 (MVP) instruction set on a 32-bit memory.
pub const WASM_ISA_BASELINE: &str = "wasm32-mvp";

/// The runtime family a web module links against.
pub const WEB_RUNTIME_NAME: &str = "tilo-runtime";

/// Post-MVP WebAssembly features a web `isa` may require, spelled as LLVM's
/// `target_features` section and `-C target-feature` spell them, in canonical
/// (byte-wise ascending) order.
pub const WASM_FEATURES: &[&str] = &[
    "atomics",
    "bulk-memory",
    "bulk-memory-opt",
    "call-indirect-overlong",
    "exception-handling",
    "extended-const",
    "multimemory",
    "multivalue",
    "mutable-globals",
    "nontrapping-fptoint",
    "reference-types",
    "relaxed-simd",
    "sign-ext",
    "simd128",
    "tail-call",
];

/// Longest accepted `isa`: the baseline plus every known feature.
pub const MAX_TARGET_ISA_BYTES: usize = 256;
/// Longest accepted `runtime`.
pub const MAX_TARGET_RUNTIME_BYTES: usize = 256;

/// Hex digits of the build id after `+wasm.`: 128 bits of the core build's
/// ABI digest.
pub const WEB_RUNTIME_ID_HEX_LEN: usize = 32;
const MAX_RUNTIME_VERSION_LEN: usize = 64;

/// Separates a native runtime's release from its ABI digest:
/// `tilo-runtime/2026.07+native.<32 hex>`.
pub const NATIVE_RUNTIME_MARKER: &str = "+native.";

/// Hex digits of the ABI digest after [`NATIVE_RUNTIME_MARKER`]: 128 bits of
/// the runtime bundle's content digest.
pub const NATIVE_RUNTIME_ID_HEX_LEN: usize = 32;

/// The registry package that offers builds of the `tilo-runtime` family for
/// Tilo OS. An app's `runtime` names the exact build; the versions of this
/// package each provide one, so a device that lacks the build an app needs
/// fetches it from the same registry before the app.
pub const NATIVE_RUNTIME_PACKAGE_ID: &str = "so.tilo.runtime";

/// Whether a registry package is a runtime rather than an app: the runtime
/// thin apps link ([`NATIVE_RUNTIME_PACKAGE_ID`]) or a further one an app
/// mounts, such as a browser engine (`so.tilo.runtime.<name>`). A version of
/// one records in `target.runtime` the runtime it provides; a store lists
/// none of them as apps and installs them for the apps that need them.
#[must_use]
pub fn is_runtime_package_id(package_id: &str) -> bool {
    package_id == NATIVE_RUNTIME_PACKAGE_ID
        || package_id
            .strip_prefix(NATIVE_RUNTIME_PACKAGE_ID)
            .is_some_and(|rest| rest.starts_with('.') && rest.len() > 1)
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum TargetError {
    #[error("unsupported platform {0:?}")]
    Platform(String),
    #[error("platform {platform} does not support architecture {architecture:?}")]
    Architecture {
        platform: &'static str,
        architecture: String,
    },
    #[error("isa must not be empty")]
    EmptyIsa,
    #[error(
        "web isa {0:?} is not canonical: `wasm32-mvp` followed by `+<feature>` for known \
         features in ascending order"
    )]
    WasmIsa(String),
    #[error("interface requirements must be sorted, distinct `<name>@<version>` entries: {0}")]
    Interfaces(String),
    #[error("web targets require a runtime")]
    MissingRuntime,
    #[error("web runtime {0:?} is not `tilo-runtime/<version>+wasm.<32 hex>`")]
    WebRuntime(String),
    #[error("native runtime {0:?} is not `<name>/<version>+native.<32 hex>`")]
    NativeRuntime(String),
}

/// A validated web `isa`: the post-MVP features the module requires.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmIsa<'a> {
    features: Vec<&'a str>,
}

impl<'a> WasmIsa<'a> {
    /// Required features in canonical order.
    pub fn features(&self) -> &[&'a str] {
        &self.features
    }

    /// Whether a host offering `supported` can run the module.
    pub fn is_satisfied_by(&self, supported: &[&str]) -> bool {
        self.features
            .iter()
            .all(|feature| supported.contains(feature))
    }
}

/// Parse a canonical web `isa`:
///
/// ```text
/// isa     = "wasm32-mvp" *( "+" feature )
/// feature = one of WASM_FEATURES
/// ```
///
/// Features appear at most once, in byte-wise ascending order, so a feature
/// set has exactly one spelling, e.g.
/// `wasm32-mvp+bulk-memory+multivalue+mutable-globals+nontrapping-fptoint+reference-types+sign-ext`.
///
/// # Errors
///
/// Returns [`TargetError::WasmIsa`] for any other spelling.
pub fn parse_wasm_isa(isa: &str) -> Result<WasmIsa<'_>, TargetError> {
    let invalid = || TargetError::WasmIsa(isa.to_string());
    if isa.len() > MAX_TARGET_ISA_BYTES {
        return Err(invalid());
    }
    let mut parts = isa.split('+');
    if parts.next() != Some(WASM_ISA_BASELINE) {
        return Err(invalid());
    }
    let mut features: Vec<&str> = Vec::new();
    for feature in parts {
        if !WASM_FEATURES.contains(&feature) || features.last().is_some_and(|last| *last >= feature)
        {
            return Err(invalid());
        }
        features.push(feature);
    }
    Ok(WasmIsa { features })
}

/// The canonical `isa` for a set of feature names; unknown names and
/// duplicates are refused rather than dropped, because the result is signed.
///
/// # Errors
///
/// Returns [`TargetError::WasmIsa`] naming the offending feature.
pub fn canonical_wasm_isa<'a>(
    features: impl IntoIterator<Item = &'a str>,
) -> Result<String, TargetError> {
    let mut sorted: Vec<&str> = Vec::new();
    for feature in features {
        if !WASM_FEATURES.contains(&feature) || sorted.contains(&feature) {
            return Err(TargetError::WasmIsa(feature.to_string()));
        }
        sorted.push(feature);
    }
    sorted.sort_unstable();
    let mut isa = WASM_ISA_BASELINE.to_string();
    for feature in sorted {
        isa.push('+');
        isa.push_str(feature);
    }
    Ok(isa)
}

/// A validated web runtime ABI id: the toolkit core release and build a
/// module links against (or, for the core itself, provides).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WebRuntime<'a> {
    /// The whole id, exactly the bytes of a module's `tilo.runtime_abi`
    /// custom section.
    pub abi: &'a str,
    /// Release, e.g. `2026.07`.
    pub version: &'a str,
    /// The build digest after `+wasm.`.
    pub build: &'a str,
}

impl WebRuntime<'_> {
    /// `tilo-runtime/<version>`, the native-style runtime requirement the
    /// authored manifest declares.
    pub fn release(&self) -> String {
        format!("{WEB_RUNTIME_NAME}/{}", self.version)
    }
}

/// Parse a web runtime ABI id:
///
/// ```text
/// runtime = "tilo-runtime/" version "+wasm." build
/// version = 1*DIGIT 1*2( "." 1*DIGIT ) [ "-" pre ]   ; CalVer or SemVer core
/// pre     = ( ALPHA / DIGIT ) *( ALPHA / DIGIT / "." / "-" )
/// build   = 32( DIGIT / %x61-66 )                    ; lower-case hex
/// ```
///
/// e.g. `tilo-runtime/2026.07+wasm.fe8836616de68a65d59c02eb73caa005`, the
/// string the core and every app module carry in their `tilo.runtime_abi`
/// section. `SemVer` build metadata is not allowed in `version`: `+`
/// introduces the build.
///
/// # Errors
///
/// Returns [`TargetError::WebRuntime`] for any other spelling.
pub fn parse_web_runtime(runtime: &str) -> Result<WebRuntime<'_>, TargetError> {
    let invalid = || TargetError::WebRuntime(runtime.to_string());
    if runtime.len() > MAX_TARGET_RUNTIME_BYTES {
        return Err(invalid());
    }
    let rest = runtime
        .strip_prefix(WEB_RUNTIME_NAME)
        .and_then(|rest| rest.strip_prefix('/'))
        .ok_or_else(invalid)?;
    let (version, build) = rest.split_once("+wasm.").ok_or_else(invalid)?;
    let build_ok = build.len() == WEB_RUNTIME_ID_HEX_LEN
        && build
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !valid_web_runtime_version(version) || !build_ok {
        return Err(invalid());
    }
    Ok(WebRuntime {
        abi: runtime,
        version,
        build,
    })
}

/// A validated native runtime id: the runtime release and the digest of the
/// exact build (`tilo-runtime/<version>+native.<32 hex>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeRuntime<'a> {
    /// The whole id, exactly as an app manifest's `package.runtime` and the
    /// registry target carry it.
    pub id: &'a str,
    /// Runtime family, e.g. `tilo-runtime`.
    pub name: &'a str,
    /// Release, e.g. `2026.07`.
    pub release: &'a str,
    /// ABI digest after [`NATIVE_RUNTIME_MARKER`].
    pub digest: &'a str,
}

impl NativeRuntime<'_> {
    /// The runtime bundle's version: its directory name under
    /// `/usr/lib/tilo-runtime` and the per-user runtime root, and the
    /// `version` its signed `bundle.toml` records.
    pub fn bundle_version(&self) -> &str {
        self.id
            .split_once('/')
            .map_or(self.id, |(_, version)| version)
    }

    /// `<name>/<release>`, the runtime an app's authored manifest declares.
    pub fn release_ref(&self) -> String {
        format!("{}/{}", self.name, self.release)
    }
}

/// Parse a native runtime id:
///
/// ```text
/// runtime = name "/" version "+native." digest
/// name    = 1*( ALPHA / DIGIT / "-" / "_" / "." )
/// version = 1*DIGIT 1*2( "." 1*DIGIT ) [ "-" pre ]   ; CalVer or SemVer core
/// digest  = 32( DIGIT / %x61-66 )                    ; lower-case hex
/// ```
///
/// e.g. `tilo-runtime/2026.07+native.4b1d0c7e9a2f3b5c6d7e8f9012345678`.
///
/// # Errors
///
/// Returns [`TargetError::NativeRuntime`] for any other spelling.
pub fn parse_native_runtime(runtime: &str) -> Result<NativeRuntime<'_>, TargetError> {
    let invalid = || TargetError::NativeRuntime(runtime.to_string());
    if runtime.len() > MAX_TARGET_RUNTIME_BYTES {
        return Err(invalid());
    }
    let (name, rest) = runtime.split_once('/').ok_or_else(invalid)?;
    let name_ok = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    let (release, digest) = rest.split_once(NATIVE_RUNTIME_MARKER).ok_or_else(invalid)?;
    let digest_ok = digest.len() == NATIVE_RUNTIME_ID_HEX_LEN
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !name_ok || !valid_web_runtime_version(release) || !digest_ok {
        return Err(invalid());
    }
    Ok(NativeRuntime {
        id: runtime,
        name,
        release,
        digest,
    })
}

fn valid_web_runtime_version(version: &str) -> bool {
    if version.is_empty() || version.len() > MAX_RUNTIME_VERSION_LEN {
        return false;
    }
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    };
    let groups: Vec<&str> = core.split('.').collect();
    matches!(groups.len(), 2 | 3)
        && groups
            .iter()
            .all(|group| !group.is_empty() && group.bytes().all(|b| b.is_ascii_digit()))
        && pre.is_none_or(|pre| {
            pre.bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphanumeric())
                && pre
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        })
}

impl RegistryTarget {
    /// Check the target is one this registry schema accepts, with canonical
    /// spellings for the web platform.
    ///
    /// # Errors
    ///
    /// Returns the first [`TargetError`] found.
    pub fn validate(&self) -> Result<(), TargetError> {
        self.validate_interfaces()?;
        match self.platform.as_str() {
            platform @ (TILO_OS_PLATFORM | LINUX_PLATFORM | WINDOWS_PLATFORM | MACOS_PLATFORM
            | ANDROID_PLATFORM) => {
                if !matches!(self.architecture.as_str(), "x86_64" | "aarch64") {
                    return Err(TargetError::Architecture {
                        platform: NATIVE_PLATFORMS
                            .into_iter()
                            .find(|known| *known == platform)
                            .unwrap_or(TILO_OS_PLATFORM),
                        architecture: self.architecture.clone(),
                    });
                }
                if self.isa.is_empty() {
                    return Err(TargetError::EmptyIsa);
                }
                Ok(())
            }
            WEB_PLATFORM => {
                if self.architecture != WASM32_ARCHITECTURE {
                    return Err(TargetError::Architecture {
                        platform: WEB_PLATFORM,
                        architecture: self.architecture.clone(),
                    });
                }
                parse_wasm_isa(&self.isa)?;
                parse_web_runtime(self.runtime.as_deref().ok_or(TargetError::MissingRuntime)?)?;
                Ok(())
            }
            other => Err(TargetError::Platform(other.to_string())),
        }
    }

    fn validate_interfaces(&self) -> Result<(), TargetError> {
        let mut names = std::collections::BTreeSet::new();
        let mut previous: Option<&String> = None;
        for raw in &self.interfaces {
            let requirement = raw
                .parse::<crate::InterfaceRequirement>()
                .map_err(|error| TargetError::Interfaces(error.to_string()))?;
            if requirement.to_string() != *raw
                || !names.insert(requirement.name)
                || previous.is_some_and(|previous| previous >= raw)
            {
                return Err(TargetError::Interfaces(raw.clone()));
            }
            previous = Some(raw);
        }
        Ok(())
    }

    pub fn is_web(&self) -> bool {
        self.platform == WEB_PLATFORM
    }

    /// The exact native runtime build a `tilo-os` target binds, when it names
    /// one in canonical form. A `tilo-os` version without one cannot say
    /// which runtime it links, so a device cannot know whether it can load
    /// it.
    pub fn native_runtime(&self) -> Option<NativeRuntime<'_>> {
        if self.platform != TILO_OS_PLATFORM {
            return None;
        }
        self.runtime
            .as_deref()
            .and_then(|runtime| parse_native_runtime(runtime).ok())
    }

    /// The web runtime this target binds, when it is a valid web target.
    pub fn web_runtime(&self) -> Option<WebRuntime<'_>> {
        if !self.is_web() {
            return None;
        }
        self.runtime
            .as_deref()
            .and_then(|runtime| parse_web_runtime(runtime).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUST_DEFAULT: &str = "wasm32-mvp+bulk-memory+bulk-memory-opt+call-indirect-overlong+multivalue+mutable-globals+nontrapping-fptoint+reference-types+sign-ext";

    fn web(isa: &str, runtime: Option<&str>) -> RegistryTarget {
        RegistryTarget {
            platform: WEB_PLATFORM.into(),
            architecture: WASM32_ARCHITECTURE.into(),
            isa: isa.into(),
            runtime: runtime.map(Into::into),
            interfaces: Vec::new(),
        }
    }

    #[test]
    fn feature_table_is_in_canonical_order() {
        assert!(WASM_FEATURES.windows(2).all(|pair| pair[0] < pair[1]));
        let every = canonical_wasm_isa(WASM_FEATURES.iter().copied()).unwrap();
        assert!(every.len() <= MAX_TARGET_ISA_BYTES, "{}", every.len());
    }

    #[test]
    fn web_isa_has_exactly_one_spelling() {
        assert_eq!(
            parse_wasm_isa("wasm32-mvp").unwrap().features(),
            [] as [&str; 0]
        );
        let isa = parse_wasm_isa(RUST_DEFAULT).unwrap();
        assert!(isa.features().contains(&"sign-ext"));
        assert_eq!(
            canonical_wasm_isa(isa.features().iter().rev().copied()).unwrap(),
            RUST_DEFAULT
        );
        for bad in [
            "",
            "wasm32",
            "wasm64-mvp",
            "wasm32-mvp+",
            "wasm32-mvp+sign-ext+bulk-memory",
            "wasm32-mvp+sign-ext+sign-ext",
            "wasm32-mvp+Sign-Ext",
            "wasm32-mvp+made-up",
            "wasm32-mvp +sign-ext",
            "x86_64-v3",
        ] {
            assert!(parse_wasm_isa(bad).is_err(), "{bad:?} must be refused");
        }
        assert!(canonical_wasm_isa(["sign-ext", "sign-ext"]).is_err());
        assert!(canonical_wasm_isa(["made-up"]).is_err());
    }

    #[test]
    fn host_support_is_a_superset_check() {
        let isa = parse_wasm_isa("wasm32-mvp+bulk-memory+sign-ext").unwrap();
        assert!(isa.is_satisfied_by(&["sign-ext", "bulk-memory", "simd128"]));
        assert!(!isa.is_satisfied_by(&["sign-ext"]));
        assert!(parse_wasm_isa("wasm32-mvp").unwrap().is_satisfied_by(&[]));
    }

    const BUILD: &str = "fe8836616de68a65d59c02eb73caa005";

    #[test]
    fn web_runtime_binds_release_and_build() {
        let id = format!("tilo-runtime/2026.07+wasm.{BUILD}");
        let runtime = parse_web_runtime(&id).unwrap();
        assert_eq!(runtime.abi, id);
        assert_eq!(runtime.version, "2026.07");
        assert_eq!(runtime.build, BUILD);
        assert_eq!(runtime.release(), "tilo-runtime/2026.07");
        let full = format!("tilo-runtime/2026.7.0-browser.1+wasm.{BUILD}");
        assert_eq!(
            parse_web_runtime(&full).unwrap().version,
            "2026.7.0-browser.1"
        );
        for bad in [
            "tilo-runtime/2026.07".to_string(),
            "tilo-runtime/2026.07+wasm.".to_string(),
            format!("tilo-runtime/2026.07+abi.{BUILD}"),
            format!("tilo-runtime/2026.07+wasm.{}", &BUILD[1..]),
            format!("tilo-runtime/2026.07+wasm.{BUILD}0"),
            format!("tilo-runtime/2026.07+wasm.{}", BUILD.to_uppercase()),
            format!("tilo-runtime/2026.07+wasm.{}g", &BUILD[1..]),
            format!("other-runtime/2026.07+wasm.{BUILD}"),
            format!("tilo-runtime/2026+wasm.{BUILD}"),
            format!("tilo-runtime/1.2.3.4+wasm.{BUILD}"),
            format!("tilo-runtime/1.2.3-+wasm.{BUILD}"),
            format!("tilo-runtime/1.2.3+build+wasm.{BUILD}"),
            format!("tilo-runtime/+wasm.{BUILD}"),
        ] {
            assert!(parse_web_runtime(&bad).is_err(), "{bad:?} must be refused");
        }
    }

    const DIGEST: &str = "4b1d0c7e9a2f3b5c6d7e8f9012345678";

    #[test]
    fn native_runtime_binds_release_and_build() {
        let id = format!("tilo-runtime/2026.07+native.{DIGEST}");
        let runtime = parse_native_runtime(&id).unwrap();
        assert_eq!(runtime.id, id);
        assert_eq!(runtime.name, "tilo-runtime");
        assert_eq!(runtime.release, "2026.07");
        assert_eq!(runtime.digest, DIGEST);
        assert_eq!(runtime.bundle_version(), format!("2026.07+native.{DIGEST}"));
        assert_eq!(runtime.release_ref(), "tilo-runtime/2026.07");
        for bad in [
            "tilo-runtime/2026.07".to_string(),
            "tilo-runtime/2026.07+native.".to_string(),
            format!("tilo-runtime/2026.07+wasm.{DIGEST}"),
            format!("tilo-runtime/2026.07+native.{}", &DIGEST[1..]),
            format!("tilo-runtime/2026.07+native.{DIGEST}0"),
            format!("tilo-runtime/2026.07+native.{}", DIGEST.to_uppercase()),
            format!("/2026.07+native.{DIGEST}"),
            format!("tilo runtime/2026.07+native.{DIGEST}"),
            format!("tilo-runtime/2026+native.{DIGEST}"),
            format!("tilo-runtime/+native.{DIGEST}"),
        ] {
            assert!(
                parse_native_runtime(&bad).is_err(),
                "{bad:?} must be refused"
            );
        }
        let target = RegistryTarget {
            platform: TILO_OS_PLATFORM.into(),
            architecture: "aarch64".into(),
            isa: "aarch64".into(),
            runtime: Some(id.clone()),
            interfaces: Vec::new(),
        };
        assert_eq!(target.native_runtime().map(|r| r.digest), Some(DIGEST));
        let release_only = RegistryTarget {
            runtime: Some("tilo-runtime/2026.07".into()),
            ..target
        };
        assert_eq!(release_only.native_runtime(), None);
        assert!(is_runtime_package_id("so.tilo.runtime"));
        assert!(is_runtime_package_id("so.tilo.runtime.chromium"));
        assert!(!is_runtime_package_id("so.tilo.runtimes"));
        assert!(!is_runtime_package_id("so.tilo.runtime."));
        assert!(!is_runtime_package_id("so.tilo.notes"));
    }

    #[test]
    fn web_targets_require_wasm32_canonical_isa_and_runtime() {
        let runtime = &format!("tilo-runtime/2026.07+wasm.{BUILD}");
        assert_eq!(web(RUST_DEFAULT, Some(runtime)).validate(), Ok(()));
        assert_eq!(
            web(RUST_DEFAULT, Some(runtime))
                .web_runtime()
                .map(|r| r.build),
            Some(BUILD)
        );
        assert_eq!(
            web(RUST_DEFAULT, None).validate(),
            Err(TargetError::MissingRuntime)
        );
        assert!(matches!(
            web("wasm32-mvp+sign-ext+bulk-memory", Some(runtime)).validate(),
            Err(TargetError::WasmIsa(_))
        ));
        let mut wrong_arch = web(RUST_DEFAULT, Some(runtime));
        wrong_arch.architecture = "x86_64".into();
        assert!(matches!(
            wrong_arch.validate(),
            Err(TargetError::Architecture { .. })
        ));
    }

    #[test]
    fn interface_requirements_have_one_canonical_list() {
        let with = |interfaces: &[&str]| RegistryTarget {
            platform: TILO_OS_PLATFORM.into(),
            architecture: "x86_64".into(),
            isa: "x86_64-v3".into(),
            runtime: None,
            interfaces: interfaces.iter().map(|s| (*s).to_string()).collect(),
        };
        assert_eq!(with(&[]).validate(), Ok(()));
        assert_eq!(with(&["collab@1", "system@1"]).validate(), Ok(()));
        for bad in [
            &["system@1", "collab@1"][..],
            &["system@1", "system@2"][..],
            &["system@1", "system@1"][..],
            &["system"][..],
            &["system@01"][..],
        ] {
            assert!(
                matches!(with(bad).validate(), Err(TargetError::Interfaces(_))),
                "{bad:?} validated"
            );
        }
        // An empty list is absent from the wire, so entries signed before
        // the field existed keep their bytes.
        let json = serde_json::to_string(&with(&[])).unwrap();
        assert!(!json.contains("interfaces"));
    }

    #[test]
    fn tilo_os_targets_keep_their_rules() {
        let native = RegistryTarget {
            platform: TILO_OS_PLATFORM.into(),
            architecture: "aarch64".into(),
            isa: "aarch64-v8.2".into(),
            runtime: Some("tilo-runtime/2026.07".into()),
            interfaces: Vec::new(),
        };
        assert_eq!(native.validate(), Ok(()));
        assert_eq!(native.web_runtime(), None);
        let mut wasm_on_native = native.clone();
        wasm_on_native.architecture = WASM32_ARCHITECTURE.into();
        assert!(wasm_on_native.validate().is_err());
        let mut empty = native;
        empty.isa.clear();
        assert_eq!(empty.validate(), Err(TargetError::EmptyIsa));
        for platform in DESKTOP_PLATFORMS {
            let desktop = RegistryTarget {
                platform: platform.into(),
                architecture: "x86_64".into(),
                isa: "x86_64-v2".into(),
                runtime: None,
                interfaces: Vec::new(),
            };
            assert_eq!(desktop.validate(), Ok(()));
            let mut wasm = desktop.clone();
            wasm.architecture = WASM32_ARCHITECTURE.into();
            assert_eq!(
                wasm.validate(),
                Err(TargetError::Architecture {
                    platform,
                    architecture: WASM32_ARCHITECTURE.into()
                })
            );
            let mut bare = desktop;
            bare.isa.clear();
            assert_eq!(bare.validate(), Err(TargetError::EmptyIsa));
        }
        assert!(matches!(
            RegistryTarget {
                platform: "Android".into(),
                architecture: "aarch64".into(),
                isa: "aarch64".into(),
                runtime: None,
                interfaces: Vec::new(),
            }
            .validate(),
            Err(TargetError::Platform(_))
        ));
        assert!(matches!(
            RegistryTarget {
                platform: "freebsd".into(),
                architecture: "x86_64".into(),
                isa: "x86_64".into(),
                runtime: None,
                interfaces: Vec::new(),
            }
            .validate(),
            Err(TargetError::Platform(_))
        ));
    }

    #[test]
    fn android_targets_are_native_arm_or_intel_builds() {
        assert!(NATIVE_PLATFORMS.contains(&ANDROID_PLATFORM));
        assert!(DESKTOP_PLATFORMS.contains(&ANDROID_PLATFORM));
        for (architecture, isa) in [("aarch64", "aarch64-v8.2"), ("x86_64", "x86_64")] {
            let android = RegistryTarget {
                platform: ANDROID_PLATFORM.into(),
                architecture: architecture.into(),
                isa: isa.into(),
                runtime: Some("tilo-runtime/2026.07".into()),
                interfaces: Vec::new(),
            };
            assert_eq!(android.validate(), Ok(()));
            assert!(!android.is_web());
            assert_eq!(android.web_runtime(), None);
        }
        for architecture in ["armv7", "arm64-v8a", "x86", WASM32_ARCHITECTURE] {
            assert_eq!(
                RegistryTarget {
                    platform: ANDROID_PLATFORM.into(),
                    architecture: architecture.into(),
                    isa: "aarch64".into(),
                    runtime: None,
                    interfaces: Vec::new(),
                }
                .validate(),
                Err(TargetError::Architecture {
                    platform: ANDROID_PLATFORM,
                    architecture: architecture.into(),
                })
            );
        }
        assert_eq!(
            RegistryTarget {
                platform: ANDROID_PLATFORM.into(),
                architecture: "aarch64".into(),
                isa: String::new(),
                runtime: None,
                interfaces: Vec::new(),
            }
            .validate(),
            Err(TargetError::EmptyIsa)
        );
    }
}
