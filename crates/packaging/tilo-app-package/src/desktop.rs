//! Linux, Windows, macOS and Android builds: the authored manifest around one
//! self-contained executable, with a signed `[target]` naming its platform and
//! architecture.

use std::path::PathBuf;

use crate::id::Kind;
use crate::manifest::{Entrypoint, EntrypointType, IsaRequirements, Manifest, Target};
use crate::validate::{ManifestError, validate_manifest};

/// The manifest a `platform` (`linux`, `windows`, `macos` or `android`) build
/// of `authored` is packed with: `module` is the executable's payload path (an
/// `.apk` or `.aab` for `android`) and `isa` the native feature level it was
/// compiled for. The toolkit release linked in is
/// `[package].runtime`, which a desktop build must declare.
///
/// # Errors
///
/// Returns the [`ManifestError`] the resulting manifest fails validation
/// with, or [`ManifestError::TargetKindUnsupported`] for a non-app package.
pub fn desktop_manifest(
    authored: &Manifest,
    platform: &str,
    architecture: &str,
    isa: &str,
    module: &str,
) -> Result<Manifest, ManifestError> {
    if authored.package.kind != Kind::App {
        return Err(ManifestError::TargetKindUnsupported);
    }
    let mut manifest = authored.clone();
    manifest.package.isa = IsaRequirements::default();
    manifest.cli = None;
    manifest.entrypoint = Some(Entrypoint {
        bin: PathBuf::from(module),
        r#type: EntrypointType::Gui,
        gui_bin: None,
        cli_bin: None,
        splash_segments: Vec::new(),
        splash_budget_ms: authored
            .entrypoint
            .as_ref()
            .map_or(100, |entry| entry.splash_budget_ms),
    });
    manifest.target = Some(Target {
        platform: platform.to_owned(),
        architecture: architecture.to_owned(),
        isa: isa.to_owned(),
        runtime: authored.package.runtime.clone().unwrap_or_default(),
        module: module.to_owned(),
    });
    validate_manifest(&manifest)?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authored() -> Manifest {
        crate::web::tests::authored("app", Some("tilo-runtime/2026.07"))
    }

    #[test]
    fn a_desktop_build_names_its_executable_and_linked_release() {
        let manifest = desktop_manifest(
            &authored(),
            registry_types::WINDOWS_PLATFORM,
            "x86_64",
            "x86_64-v2",
            "bin/books.exe",
        )
        .unwrap();
        let target = manifest.target.as_ref().unwrap();
        assert_eq!(target.platform, "windows");
        assert_eq!(target.runtime, "tilo-runtime/2026.07");
        assert_eq!(target.module, "bin/books.exe");
        assert_eq!(
            manifest
                .entrypoint
                .as_ref()
                .and_then(Entrypoint::gui_bin_path),
            Some(&PathBuf::from("bin/books.exe"))
        );
        assert_eq!(manifest.package.isa, IsaRequirements::default());

        let mut moved = manifest.clone();
        if let Some(target) = moved.target.as_mut() {
            target.module = "bin/other.exe".into();
        }
        assert!(matches!(
            validate_manifest(&moved),
            Err(ManifestError::TargetEntrypointMismatch(_))
        ));
        let mut other_release = manifest;
        other_release.package.runtime = Some("tilo-runtime/2026.08".into());
        assert!(matches!(
            validate_manifest(&other_release),
            Err(ManifestError::TargetRuntimeMismatch { .. })
        ));
    }

    #[test]
    fn a_desktop_build_refuses_what_it_cannot_describe() {
        let mut unversioned = authored();
        unversioned.package.runtime = None;
        assert!(matches!(
            desktop_manifest(&unversioned, "macos", "aarch64", "aarch64", "bin/books"),
            Err(ManifestError::TargetRuntimeMismatch { .. })
        ));
        assert!(matches!(
            desktop_manifest(&authored(), "macos", "wasm32", "aarch64", "bin/books"),
            Err(ManifestError::BadTarget(_))
        ));
        assert!(matches!(
            desktop_manifest(&authored(), "freebsd", "x86_64", "x86_64", "bin/books"),
            Err(ManifestError::BadTarget(_))
        ));
        assert!(matches!(
            desktop_manifest(&authored(), "macos", "aarch64", "aarch64", "../books"),
            Err(ManifestError::BadTargetModule(_))
        ));
        for apk in ["books.apk", "bin/books.aab"] {
            let android = desktop_manifest(
                &authored(),
                registry_types::ANDROID_PLATFORM,
                "aarch64",
                "aarch64",
                apk,
            )
            .unwrap();
            assert_eq!(
                android
                    .target
                    .as_ref()
                    .map(|target| target.platform.as_str()),
                Some("android")
            );
        }
        for not_a_package in ["bin/books", "books.exe", "books.apk.zip", "../books.apk"] {
            assert!(matches!(
                desktop_manifest(
                    &authored(),
                    registry_types::ANDROID_PLATFORM,
                    "x86_64",
                    "x86_64",
                    not_a_package
                ),
                Err(ManifestError::BadTargetModule(_))
            ));
        }
        assert!(matches!(
            desktop_manifest(&authored(), "android", "armv7", "armv7", "books.apk"),
            Err(ManifestError::BadTarget(_))
        ));
        let mut service = authored();
        service.package.kind = Kind::Service;
        assert_eq!(
            desktop_manifest(&service, "macos", "aarch64", "aarch64", "bin/books"),
            Err(ManifestError::TargetKindUnsupported)
        );
    }
}
