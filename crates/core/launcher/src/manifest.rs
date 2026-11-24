//! The launcher's side of the package manifest.
//!
//! Decoding lives in [`tilo_app_package::view`]: one decoder, built on the canonical
//! schema, whose identity is the signed `[package].id`.

pub use tilo_app_package::continuity_schema;
pub use tilo_app_package::view::{
    ContinuityContract, ContinuityFacet, ContinuitySemanticSchema, Entrypoint, EntrypointType,
    KIND_APP, KIND_SERVICE, ManifestReadError, ManifestView, Net, PortalPolicy, Sandbox,
    ServiceReq, read_manifest,
};

/// The variable that tells a runtime's binary where the package it runs is (see
/// [`ManifestView::entrypoint`]'s `runtime`): where the sandbox mounts it on Linux, its installed
/// payload on macOS.
pub const APP_MODULE_ENV: &str = "TILO_APP_MODULE";

/// The one binary of the tilolang runtime, in its package: a tilolang app's window, the host of
/// its instances, and a page window, each in the role it is started for.
pub const TILOLANG_BIN: &str = "bin/tilolang-process";

/// Whether `runtime` (`<name>/<version>`, an entrypoint's runtime) is a tilolang runtime: a
/// package naming one is a tilolang app, whose instances a tilolang host runs.
#[must_use]
pub fn is_tilolang(runtime: &str) -> bool {
    runtime
        .split_once('/')
        .is_some_and(|(name, version)| name == "tilolang" && !version.is_empty())
}

/// A service requirement written as a manifest would (`"required@1"`).
#[cfg(test)]
pub(crate) fn service_req(raw: &str) -> ServiceReq {
    ServiceReq::parse("", raw).expect("valid service requirement")
}

/// A canonical manifest for tests: `package` is the body of `[package]`, `tail` the tables after the
/// sandbox.
#[cfg(test)]
pub(crate) fn test_manifest(package: &str, tail: &str) -> ManifestView {
    let net = if tail.contains("[sandbox.net]") {
        ""
    } else {
        "[sandbox.net]\nmode = \"off\"\n"
    };
    let source = format!(
        "[package]\n{package}\n[publisher]\nidentity = \"ed25519:{}\"\n\
         [sandbox]\nnamespaces = []\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n\
         {net}{tail}\n",
        "00".repeat(32)
    );
    ManifestView::from_toml_str(&source).unwrap_or_else(|e| panic!("{e}\n{source}"))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    fn desktop_fixture(
        dir: &Path,
        platform: &str,
        arch: &str,
    ) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let authored = tilo_app_package::Manifest::from_toml_str(&format!(
            r#"
[package]
id = "so.tilo.contract"
version = "0.1.0"
kind = "app"
runtime = "tilo-runtime/2099.01"
[publisher]
identity = "ed25519:{}"
signature = ""
[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount", "net"]
seccomp = "tilo-default"
landlock = "strict"
[sandbox.net]
mode = "off"
[entrypoint]
bin = "bin/app"
"#,
            "00".repeat(32)
        ))?;
        let native = tilo_app_package::desktop::desktop_manifest(
            &authored, platform, arch, arch, "bin/app",
        )?;
        let path = dir.join("manifest.toml");
        std::fs::write(&path, native.to_toml_string()?)?;
        Ok(path)
    }

    tilo_test_env::scenario! {
        fn the_desktop_contract_binds_the_linked_runtime_and_its_host;

        phase desktop_contract_uses_linked_runtime_without_override;
        {
            let dir = tempfile::tempdir().unwrap();
            let path =
                desktop_fixture(dir.path(), std::env::consts::OS, std::env::consts::ARCH).unwrap();
            let manifest = read_manifest(&path).unwrap();
            assert!(manifest.runtime.is_none());
            assert!(
                crate::run::resolve_runtime(&manifest, &crate::run::LaunchOptions::default())
                    .unwrap()
                    .is_none()
            );
            let mut canonical =
                tilo_app_package::Manifest::from_toml_str(&std::fs::read_to_string(&path).unwrap())
                    .unwrap();
            // A signed target cannot excuse a mismatched linked toolkit release.
            canonical.package.runtime = Some("tilo-runtime/2099.02".into());
            std::fs::write(&path, canonical.to_toml_string().unwrap()).unwrap();
            assert!(read_manifest(&path).is_err());
            canonical.package.runtime = Some("tilo-runtime/2099.01".into());
            canonical.target = None;
            std::fs::write(&path, canonical.to_toml_string().unwrap()).unwrap();
            let thin = read_manifest(&path).unwrap();
            assert_eq!(thin.runtime.as_deref(), Some("tilo-runtime/2099.01"));
            assert!(matches!(
                crate::run::resolve_runtime(&thin, &crate::run::LaunchOptions::default()),
                Err(crate::run::LaunchError::RuntimeBundleMissing { .. })
            ));
        }

        phase desktop_contract_refuses_foreign_host;
        {
            let dir = tempfile::tempdir().unwrap();
            let platform = if std::env::consts::OS == "linux" {
                "windows"
            } else {
                "linux"
            };
            let path = desktop_fixture(dir.path(), platform, std::env::consts::ARCH).unwrap();
            assert!(read_manifest(&path).is_err());
            let arch = if std::env::consts::ARCH == "x86_64" {
                "aarch64"
            } else {
                "x86_64"
            };
            let path = desktop_fixture(dir.path(), std::env::consts::OS, arch).unwrap();
            assert!(read_manifest(&path).is_err());
        }
    }

    fn repo() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("workspace root above crates/core/launcher")
            .to_path_buf()
    }

    tilo_test_env::scenario! {
        fn manifests_declare_what_they_open;

        phase shipped_manifests_declare_what_they_need_to_open;
        {
            {
                let manifest = read_manifest(
                    &repo().join("crates/engines/chromium-host/browser.tilo-app.toml"),
                )
                .expect("the browser's built manifest parses through installed-app schema");
                assert!(
                    manifest
                        .required_runtimes
                        .iter()
                        .any(|runtime| runtime.contains("chromium")),
                    "the browser names the engine it renders with"
                );
            }
            {
                let manifest = read_manifest(&repo().join("apps/settings/tilo-app.toml"))
                    .expect("shipped Settings manifest parses through installed-app schema");

                for service in ["models", "model-gateway"] {
                    let requirement = manifest
                        .sandbox
                        .services
                        .get(service)
                        .unwrap_or_else(|| panic!("Settings declares the {service} integration"));
                    assert!(
                        !requirement.is_required()
                            && (runtime_portal::OLDEST_RUNTIME_INTERFACE_VERSION
                                ..=runtime_portal::RUNTIME_INTERFACE_VERSION)
                                .contains(&requirement.version),
                        "Settings must open when {service} is not running"
                    );
                }
            }
        }
    }
}
