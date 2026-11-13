//! Execution targets a registry version may bind, and their canonical spellings: a target is signed
//! verbatim, so two publishers describing one contract must produce the same bytes.
//!
//! - `tilo-os` — a native thin app; `runtime` is the exact build it links, since Rust dylibs have no
//!   stable ABI and only the build digest says whether a device can load it.
//! - `linux`, `windows`, `macos` and `android` — the same app as a self-contained executable or
//!   package for another desktop. Tilo OS installs only `tilo-os` builds.

use crate::entry::RegistryTarget;

pub const TILO_OS_PLATFORM: &str = "tilo-os";
pub const LINUX_PLATFORM: &str = "linux";
pub const WINDOWS_PLATFORM: &str = "windows";
pub const MACOS_PLATFORM: &str = "macos";
pub const ANDROID_PLATFORM: &str = "android";

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

/// Native families outside Tilo OS: the executable links the toolkit in, and its signed manifest
/// names the exact version.
pub const DESKTOP_PLATFORMS: [&str; 4] = [
    LINUX_PLATFORM,
    WINDOWS_PLATFORM,
    MACOS_PLATFORM,
    ANDROID_PLATFORM,
];
pub const MAX_TARGET_ISA_BYTES: usize = 256;
pub const MAX_TARGET_RUNTIME_BYTES: usize = 256;

const MAX_RUNTIME_VERSION_LEN: usize = 64;

/// Separates a native runtime's release from its ABI digest:
/// `tilo-runtime/2026.07+native.<32 hex>`.
pub const NATIVE_RUNTIME_MARKER: &str = "+native.";

/// Hex digits of the ABI digest after [`NATIVE_RUNTIME_MARKER`]: 128 bits of the bundle's content
/// digest.
pub const NATIVE_RUNTIME_ID_HEX_LEN: usize = 32;

/// The registry package offering `tilo-runtime` builds for Tilo OS. Each of its versions provides one
/// build, so a device fetches the one an app names.
pub const NATIVE_RUNTIME_PACKAGE_ID: &str = "so.tilo.runtime";

/// Whether a registry package is a runtime rather than an app: the runtime thin apps link
/// ([`NATIVE_RUNTIME_PACKAGE_ID`]) or one an app mounts, such as a browser engine. A version of one
/// records the runtime it provides in `target.runtime`; no store lists them as apps.
#[must_use]
pub fn is_runtime_package_id(package_id: &str) -> bool {
    package_id == NATIVE_RUNTIME_PACKAGE_ID
        || package_id
            .strip_prefix(NATIVE_RUNTIME_PACKAGE_ID)
            .is_some_and(|rest| rest.starts_with('.') && rest.len() > 1)
}

/// The package id prefix of a language pack: one locale's catalogues for every app
/// (`so.tilo.language-pack.de-de`), as `tilo_app_package::LANGUAGE_PACK_ID_PREFIX` has it.
pub const LANGUAGE_PACK_PACKAGE_PREFIX: &str = "so.tilo.language-pack.";

#[must_use]
pub fn is_language_pack_package_id(package_id: &str) -> bool {
    package_id
        .strip_prefix(LANGUAGE_PACK_PACKAGE_PREFIX)
        .is_some_and(|locale| !locale.is_empty())
}

/// The architecture of a build that is data for every architecture: a language pack's, only on
/// `tilo-os` targets.
pub const ANY_ARCHITECTURE: &str = "any";

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
    #[error("interface requirements must be sorted, distinct `<name>@<version>` entries: {0}")]
    Interfaces(String),
    #[error("native runtime {0:?} is not `<name>/<version>+native.<32 hex>`")]
    NativeRuntime(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeRuntime<'a> {
    /// The whole id, as a manifest's `package.runtime` and the registry target carry it.
    pub id: &'a str,
    pub name: &'a str,
    pub release: &'a str,
    pub digest: &'a str,
}

impl NativeRuntime<'_> {
    /// The bundle's version: its directory name under `/usr/lib/tilo-runtime` and the per-user root,
    /// and the `version` its signed `bundle.toml` records.
    pub fn bundle_version(&self) -> &str {
        self.id
            .split_once('/')
            .map_or(self.id, |(_, version)| version)
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
    if !name_ok || !valid_runtime_version(release) || !digest_ok {
        return Err(invalid());
    }
    Ok(NativeRuntime {
        id: runtime,
        name,
        release,
        digest,
    })
}

fn valid_runtime_version(version: &str) -> bool {
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
    pub fn validate(&self) -> Result<(), TargetError> {
        self.validate_interfaces()?;
        if self.platform == TILO_OS_PLATFORM
            && self.architecture == ANY_ARCHITECTURE
            && self.isa == ANY_ARCHITECTURE
            && self.runtime.is_none()
        {
            return Ok(());
        }
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

    /// Whether the build is data for every architecture ([`ANY_ARCHITECTURE`]).
    #[must_use]
    pub fn is_any_architecture(&self) -> bool {
        self.architecture == ANY_ARCHITECTURE
    }

    /// The exact native runtime build a `tilo-os` target binds, when it names one in canonical form.
    /// A `tilo-os` version without one cannot say which runtime it links, so a device cannot know
    /// whether it can load it.
    pub fn native_runtime(&self) -> Option<NativeRuntime<'_>> {
        if self.platform != TILO_OS_PLATFORM {
            return None;
        }
        self.runtime
            .as_deref()
            .and_then(|runtime| parse_native_runtime(runtime).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "4b1d0c7e9a2f3b5c6d7e8f9012345678";

    #[test]
    fn a_build_for_every_architecture_is_tilo_os_data() {
        let target =
            |platform: &str, architecture: &str, isa: &str, runtime: Option<&str>| RegistryTarget {
                platform: platform.into(),
                architecture: architecture.into(),
                isa: isa.into(),
                runtime: runtime.map(Into::into),
                interfaces: Vec::new(),
            };
        let any = target(TILO_OS_PLATFORM, ANY_ARCHITECTURE, ANY_ARCHITECTURE, None);
        assert!(any.validate().is_ok());
        assert!(any.is_any_architecture());
        assert!(
            target(LINUX_PLATFORM, ANY_ARCHITECTURE, ANY_ARCHITECTURE, None)
                .validate()
                .is_err()
        );
        assert!(
            target(
                TILO_OS_PLATFORM,
                ANY_ARCHITECTURE,
                ANY_ARCHITECTURE,
                Some("tilo-runtime/1.0")
            )
            .validate()
            .is_err()
        );
        assert!(is_language_pack_package_id("so.tilo.language-pack.de-de"));
        assert!(!is_language_pack_package_id("so.tilo.language-pack."));
        assert!(!is_language_pack_package_id("so.tilo.settings"));
    }

    #[test]
    fn native_runtime_binds_release_and_build() {
        let id = format!("tilo-runtime/2026.07+native.{DIGEST}");
        let runtime = parse_native_runtime(&id).unwrap();
        assert_eq!(runtime.id, id);
        assert_eq!(runtime.name, "tilo-runtime");
        assert_eq!(runtime.release, "2026.07");
        assert_eq!(runtime.digest, DIGEST);
        assert_eq!(runtime.bundle_version(), format!("2026.07+native.{DIGEST}"));
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
        let mut wasm_on_native = native.clone();
        wasm_on_native.architecture = "wasm32".into();
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
            wasm.architecture = "wasm32".into();
            assert_eq!(
                wasm.validate(),
                Err(TargetError::Architecture {
                    platform,
                    architecture: "wasm32".into()
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
        }
        for architecture in ["armv7", "arm64-v8a", "x86", "wasm32"] {
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
