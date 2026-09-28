//! The signed registry entry that publishes one build of a package.
//!
//! Everything in the entry that describes the package — id, version,
//! category, target — comes from the signed package itself, so an entry
//! cannot describe a package as something it is not. The registry checks the
//! same things again; these checks only fail early, on the publisher's side.

use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use ed25519_dalek::SigningKey;
use serde::Deserialize;
use tilo_app_package::manifest::Arch;
use tilo_app_package::{DiscardBlobs, Manifest, PackageId, Verification};

/// The platform a build is for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Platform {
    /// Native Tilo OS thin app (`x86_64` or `aarch64`).
    #[default]
    TiloOs,
    /// WebAssembly side module for the browser toolkit core (`wasm32`).
    Web,
    /// Self-contained executable for Linux desktops other than Tilo OS
    /// (`x86_64` or `aarch64`).
    Linux,
    /// Self-contained Windows executable (`x86_64` or `aarch64`).
    Windows,
    /// Self-contained macOS executable (`x86_64` or `aarch64`).
    Macos,
}

impl Platform {
    /// The platform's name in a registry target.
    #[must_use]
    pub fn wire(self) -> &'static str {
        match self {
            Self::TiloOs => registry_types::TILO_OS_PLATFORM,
            Self::Web => registry_types::WEB_PLATFORM,
            Self::Linux => registry_types::LINUX_PLATFORM,
            Self::Windows => registry_types::WINDOWS_PLATFORM,
            Self::Macos => registry_types::MACOS_PLATFORM,
        }
    }

    /// The platform a package was built for: its signed `[target]`, or
    /// Tilo OS when it has none.
    #[must_use]
    pub fn of(manifest: &Manifest) -> Option<Self> {
        let Some(target) = &manifest.target else {
            return Some(Self::TiloOs);
        };
        [Self::Web, Self::Linux, Self::Windows, Self::Macos]
            .into_iter()
            .find(|platform| platform.wire() == target.platform)
    }
}

/// Publisher-authored, localizable store presentation. JSON uses the same
/// signed wire types as the registry entry, so no lossy conversion or locale
/// normalization happens between authoring and signing.
#[derive(Clone, Debug, Deserialize)]
pub struct EntryMetadata {
    pub name: registry_types::LocalizedText,
    pub description: registry_types::LocalizedText,
    pub tagline: registry_types::LocalizedText,
    #[serde(default)]
    pub images: registry_types::LocalizedImages,
    pub supported_languages: Vec<String>,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
}

/// A package that decoded, carries its whole payload within the limits and
/// is signed by the publisher.
pub struct Checked {
    pub id: PackageId,
    pub manifest: Manifest,
}

/// Decode `tapp_bytes` and check it the way the registry will.
///
/// # Errors
///
/// Why the registry would refuse the package.
pub fn check_package(tapp_bytes: &[u8], publisher: [u8; 32]) -> Result<Checked> {
    let tapp = tilo_app_package::tapp::read(tapp_bytes, &mut DiscardBlobs)
        .map_err(|error| anyhow!("decode .tapp: {error}"))?;
    let missing = tapp.missing();
    if !missing.is_empty() {
        bail!(
            "publish the full .tapp; {} payload blob(s) are not carried",
            missing.len()
        );
    }
    let manifest = &tapp.meta.manifest;
    let payload_paths: Vec<PathBuf> = tapp
        .meta
        .collection
        .entries
        .keys()
        .map(PathBuf::from)
        .collect();
    tilo_app_package::validate_manifest_with_payload(manifest, &payload_paths)
        .map_err(|error| anyhow!("manifest/payload validation: {error}"))?;
    tilo_app_package::validate_materialized_payload(manifest, &tapp.meta.collection, |hash| {
        tapp.carried.get(hash).copied()
    })
    .map_err(|error| anyhow!("{error}"))?;
    match tilo_app_package::verify(&tapp.meta, &[publisher]) {
        Verification::Trusted { .. } | Verification::AttestedBy { .. } => {}
        Verification::UntrustedPublisher { identity, .. } => bail!(
            "the package is signed by ed25519:{}, not by this publisher key ed25519:{}",
            hex::encode(identity),
            hex::encode(publisher)
        ),
        other => bail!("the package failed verification: {other:?}"),
    }
    if let Some(limit) = tilo_app_package::app_size_limit(manifest)
        && tapp_bytes.len() as u64 > limit
    {
        bail!(
            "application .tapp is {} bytes; limit is {limit} bytes",
            tapp_bytes.len()
        );
    }
    Ok(Checked {
        id: tapp.meta.id,
        manifest: manifest.clone(),
    })
}

/// Build the entry from the package's own manifest: id, name,
/// category, version come from inside the signed package, so a
/// registry row cannot describe a package as something it is not.
///
/// # Errors
///
/// When the package fails [`check_package`], does not match `platform` or
/// `arch`, or conflicts with `previous`.
#[allow(
    clippy::too_many_lines,
    reason = "registry entry assembly must bind package inspection, target selection, metadata, prior-entry continuity, validation, and signing in one auditable sequence"
)]
pub fn build_entry(
    tapp_bytes: &[u8],
    url: &str,
    signer: &SigningKey,
    platform: Platform,
    arch: Option<&str>,
    previous: Option<&registry_types::RegistryEntry>,
    metadata: Option<&EntryMetadata>,
) -> Result<registry_types::RegistryEntry> {
    let checked = check_package(tapp_bytes, signer.verifying_key().to_bytes())?;
    let binding = match platform {
        Platform::TiloOs => native_binding(tapp_bytes, &checked, arch)?,
        Platform::Web | Platform::Linux | Platform::Windows | Platform::Macos => {
            signed_target_binding(tapp_bytes, &checked, platform, arch)?
        }
    };
    let package = &checked.manifest.package;
    let now = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs())
        });
    let version = registry_types::RegistryVersion {
        version: package.version.clone(),
        tapp_url: url.to_string(),
        tapp_root_hash_hex: binding.package_id.to_hex(),
        tapp_size_bytes: tapp_bytes.len() as u64,
        released_at_unix: now,
        tapp_blake3_hex: binding.tapp_blake3_hex,
        target: binding.target,
    };
    let mut artifact_exists = false;
    if let Some(previous) = previous {
        if previous.package_id != package.id
            || !previous
                .publisher_pubkey_hex
                .eq_ignore_ascii_case(&hex::encode(signer.verifying_key().to_bytes()))
        {
            bail!("the previous entry does not belong to this package and publisher");
        }
        if let Some(existing) = previous
            .versions
            .iter()
            .find(|existing| same_version_identity(existing, &version))
        {
            if !same_artifact(existing, &version) {
                bail!(
                    "version {} for target {}/{} already exists with different package bytes or metadata",
                    version.version,
                    version.target.platform,
                    version.target.architecture,
                );
            }
            if metadata.is_none() {
                return Ok(previous.clone());
            }
            artifact_exists = true;
        }
        if candidate_is_older_than_published(previous, &version) {
            // A slower CI run may finish after a newer per-target release.
            // Preserve the already-published entry instead of allowing stale
            // package bytes or metadata to race the registry transition.
            return Ok(previous.clone());
        }
    }

    let mut versions = previous.map_or_else(Vec::new, |entry| entry.versions.clone());
    if !artifact_exists {
        versions.push(version);
    }
    versions.sort_by(compare_registry_versions);
    let display_name = package.name.clone().unwrap_or_else(|| package.id.clone());
    let mut entry = registry_types::RegistryEntry {
        package_id: package.id.clone(),
        name: previous.map_or_else(|| display_name.clone().into(), |entry| entry.name.clone()),
        description: previous.map_or_else(
            || match platform {
                Platform::TiloOs => {
                    format!("{display_name} is a native Tilo OS application.").into()
                }
                Platform::Web | Platform::Linux | Platform::Windows | Platform::Macos => {
                    format!("{display_name} is a Tilo application.").into()
                }
            },
            |entry| entry.description.clone(),
        ),
        tagline: previous.map_or_else(
            || match platform {
                Platform::TiloOs => format!("{display_name} for Tilo OS").into(),
                Platform::Web => format!("{display_name} on Tilo").into(),
                Platform::Linux => format!("{display_name} for Linux").into(),
                Platform::Windows => format!("{display_name} for Windows").into(),
                Platform::Macos => format!("{display_name} for macOS").into(),
            },
            |entry| entry.tagline.clone(),
        ),
        images: previous.map_or_else(Default::default, |entry| entry.images.clone()),
        supported_languages: previous.map_or_else(
            || vec!["en".into()],
            |entry| entry.supported_languages.clone(),
        ),
        categories: package.category.clone().into_iter().collect(),
        source_url: previous.and_then(|entry| entry.source_url.clone()),
        homepage: previous.and_then(|entry| entry.homepage.clone()),
        publisher_pubkey_hex: hex::encode(signer.verifying_key().to_bytes()),
        versions,
        updated_at_unix: previous.map_or(now, |entry| {
            now.max(entry.updated_at_unix.saturating_add(1))
        }),
        signature_hex: String::new(),
    };
    if let Some(metadata) = metadata {
        entry.name.clone_from(&metadata.name);
        entry.description.clone_from(&metadata.description);
        entry.tagline.clone_from(&metadata.tagline);
        entry.images.clone_from(&metadata.images);
        entry
            .supported_languages
            .clone_from(&metadata.supported_languages);
        entry.source_url.clone_from(&metadata.source_url);
        entry.homepage.clone_from(&metadata.homepage);
    }
    entry
        .validate()
        .map_err(|error| anyhow!("invalid registry metadata: {error}"))?;
    registry_types::sign_entry(&mut entry, signer);
    Ok(entry)
}

/// What a registry version binds from the final tapp bytes.
struct ArtifactBinding {
    package_id: tilo_app_package::PackageId,
    tapp_blake3_hex: String,
    target: registry_types::RegistryTarget,
}

fn native_binding(
    tapp_bytes: &[u8],
    checked: &Checked,
    arch: Option<&str>,
) -> Result<ArtifactBinding> {
    let isa = &checked.manifest.package.isa;
    let declared: Vec<Arch> = [Arch::X86_64, Arch::AArch64]
        .into_iter()
        .filter(|arch| isa.for_arch(*arch).is_some())
        .collect();
    let arch = match arch {
        None => match declared.as_slice() {
            [only] => *only,
            [] => bail!("the manifest declares no target architecture"),
            _ => {
                bail!("the manifest declares several architectures; pass the one this build is for")
            }
        },
        Some("x86_64") => Arch::X86_64,
        Some("aarch64") => Arch::AArch64,
        Some(other) => {
            bail!("unknown architecture {other:?} for tilo-os (expected x86_64 or aarch64)")
        }
    };
    let level = isa
        .for_arch(arch)
        .ok_or_else(|| anyhow!("the manifest declares no build for {}", arch_wire(arch)))?;
    Ok(ArtifactBinding {
        package_id: checked.id,
        tapp_blake3_hex: blake3::hash(tapp_bytes).to_hex().to_string(),
        target: registry_types::RegistryTarget {
            platform: registry_types::TILO_OS_PLATFORM.into(),
            architecture: arch_wire(arch).into(),
            isa: level.wire_label().to_string(),
            runtime: checked.manifest.package.runtime.clone(),
        },
    })
}

fn arch_wire(arch: Arch) -> &'static str {
    match arch {
        Arch::X86_64 => "x86_64",
        Arch::AArch64 => "aarch64",
    }
}

/// A web, Linux, Windows or macOS build binds the `[target]` its signed manifest
/// records; the tapp passes the same verification a native one does before
/// anything is signed.
fn signed_target_binding(
    tapp_bytes: &[u8],
    checked: &Checked,
    platform: Platform,
    arch: Option<&str>,
) -> Result<ArtifactBinding> {
    if platform == Platform::Web
        && arch.is_some_and(|arch| arch != registry_types::WASM32_ARCHITECTURE)
    {
        bail!("web builds target wasm32");
    }
    let target = checked.manifest.target.as_ref().ok_or_else(|| {
        anyhow!(
            "the tapp has no [target]; it is a Tilo OS build, not a {} build",
            platform.wire()
        )
    })?;
    let target = registry_types::RegistryTarget {
        platform: target.platform.clone(),
        architecture: target.architecture.clone(),
        isa: target.isa.clone(),
        runtime: Some(target.runtime.clone()),
    };
    if target.platform != platform.wire() {
        bail!(
            "the tapp's target is {}, not {}",
            target.platform,
            platform.wire()
        );
    }
    if let Some(arch) = arch
        && arch != target.architecture
    {
        bail!("the tapp was built for {}, not {arch}", target.architecture);
    }
    Ok(ArtifactBinding {
        package_id: checked.id,
        tapp_blake3_hex: blake3::hash(tapp_bytes).to_hex().to_string(),
        target,
    })
}

fn same_version_identity(
    left: &registry_types::RegistryVersion,
    right: &registry_types::RegistryVersion,
) -> bool {
    tilo_app_package::version::compare(&left.version, &right.version).is_eq()
        && left.target.platform == right.target.platform
        && left.target.architecture == right.target.architecture
}

fn same_artifact(
    left: &registry_types::RegistryVersion,
    right: &registry_types::RegistryVersion,
) -> bool {
    left.tapp_root_hash_hex == right.tapp_root_hash_hex
        && left.tapp_size_bytes == right.tapp_size_bytes
        && left.tapp_blake3_hex == right.tapp_blake3_hex
        && left.target == right.target
}

fn candidate_is_older_than_published(
    previous: &registry_types::RegistryEntry,
    candidate: &registry_types::RegistryVersion,
) -> bool {
    previous.versions.iter().any(|existing| {
        existing.target.platform == candidate.target.platform
            && existing.target.architecture == candidate.target.architecture
            && tilo_app_package::version::compare(&candidate.version, &existing.version).is_lt()
    })
}

fn compare_registry_versions(
    left: &registry_types::RegistryVersion,
    right: &registry_types::RegistryVersion,
) -> std::cmp::Ordering {
    let semantic = tilo_app_package::version::compare(&right.version, &left.version);
    semantic.then_with(|| left.target.architecture.cmp(&right.target.architecture))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(value: &str) -> registry_types::RegistryVersion {
        registry_types::RegistryVersion {
            version: value.into(),
            tapp_url: String::new(),
            tapp_root_hash_hex: String::new(),
            tapp_size_bytes: 0,
            released_at_unix: 0,
            tapp_blake3_hex: String::new(),
            target: registry_types::RegistryTarget {
                platform: registry_types::TILO_OS_PLATFORM.into(),
                architecture: "x86_64".into(),
                isa: "x86_64-v3".into(),
                runtime: None,
            },
        }
    }

    #[test]
    fn version_identity_is_unique_per_precedence_and_target() {
        // phase: source_hash_cannot_create_a_second_artifact_at_the_same_precedence
        {
            assert!(same_version_identity(
                &version("0.1.1-dev.10+g111"),
                &version("0.1.1-dev.10+g222")
            ));
            assert!(!same_version_identity(
                &version("0.1.1-dev.10+g111"),
                &version("0.1.1-dev.11+g000")
            ));
            assert!(
                compare_registry_versions(
                    &version("0.1.1-dev.10+g111"),
                    &version("0.1.1-dev.9+gzzz")
                )
                .is_lt()
            );
        }
        // phase: slower_release_is_stale_only_for_the_same_target
        {
            let current = version("0.1.42+gnew");
            let entry = registry_types::RegistryEntry {
                package_id: "so.tilo.browser".into(),
                name: "Browser".into(),
                description: "Browser".into(),
                tagline: "Browse".into(),
                images: registry_types::LocalizedImages::default(),
                supported_languages: vec!["en".into()],
                categories: Vec::new(),
                source_url: None,
                homepage: None,
                publisher_pubkey_hex: String::new(),
                versions: vec![current],
                updated_at_unix: 0,
                signature_hex: String::new(),
            };

            assert!(candidate_is_older_than_published(
                &entry,
                &version("0.1.41+gold")
            ));
            assert!(!candidate_is_older_than_published(
                &entry,
                &version("0.1.43+gnext")
            ));
            let mut arm = version("0.1.1+gold");
            arm.target.architecture = "aarch64".into();
            arm.target.isa = "aarch64-v8.2".into();
            assert!(!candidate_is_older_than_published(&entry, &arm));
        }
    }
}
