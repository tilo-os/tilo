//! The launcher's side of the package manifest.
//!
//! Decoding lives in [`tilo_app_package::view`]: one decoder, built on the
//! canonical schema, whose identity is the signed `[package].id`. This
//! module re-exports that view under the names the sandbox code uses and
//! adds what only the launcher decides — which manifests it refuses to
//! sandbox.
//!
//! ### `[sandbox.fs]` is not part of the vocabulary
//!
//! `[sandbox.fs]` is rejected rather than interpreted: filesystem writes use
//! [`crate::appdir`], system reads use [`crate::profile`], and other access
//! requires an explicit [`crate::capabilities`] grant. This prevents a package
//! from defining the boundary meant to contain it; rejecting the field also
//! avoids silently running an app with less access than its manifest requested.

pub use tilo_app_package::continuity_schema;
pub use tilo_app_package::view::{
    ContinuityContract, ContinuityFacet, ContinuitySemanticSchema, Entrypoint, EntrypointType, Fs,
    KIND_APP, KIND_SERVICE, ManifestReadError, ManifestView, Net, PortalPolicy, Sandbox,
    ServiceReq, read_manifest,
};

/// A manifest asking for filesystem access it is not allowed to ask
/// for. One variant per field so the message can say what to do
/// instead — the two cases have different answers.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FsPolicyViolation {
    #[error(
        "package `{package}` declares `sandbox.fs.write = [{entry:?}]`, and an app does not \
         choose where it writes.\n  \
         The launcher gives every app one directory — per room, per package — binds it at \
         {app_dir} and points HOME, XDG_DATA_HOME, XDG_CONFIG_HOME, XDG_STATE_HOME and \
         XDG_CACHE_HOME inside it. Remove the `[sandbox.fs]` block; anything the app writes \
         under those paths already lands there.\n  \
         Access to a path outside it is a capability the user grants at runtime, not a line \
         in a manifest."
    )]
    Write {
        package: String,
        entry: String,
        app_dir: &'static str,
    },
    #[error(
        "package `{package}` declares `sandbox.fs.read = [{entry:?}]`, and an app does not \
         choose what it reads.\n  \
         Every app gets the same system read profile (loader and C library, fonts, GPU loader \
         manifests, CA bundle, timezone data). Remove the `[sandbox.fs]` block.\n  \
         Anything the profile does not cover — a user's documents, /usr/bin, /var/workspace, a \
         device node — is a capability the user grants at runtime, not a line in a manifest."
    )]
    Read { package: String, entry: String },
}

/// Refuse a manifest that names filesystem paths.
///
/// Called at launch and at install: an install-time check alone would
/// be bypassed by `tilo-launcher run --manifest` or a package installed
/// before this rule existed.
///
/// **No first-party exception.** Every manifest in the tree asked for
/// something plausible — that is how the original hole stayed
/// invisible. A bypass for OS-shipped packages would make this a rule
/// about strangers, not a boundary.
///
/// # Errors
///
/// Returns [`FsPolicyViolation::Write`] or [`FsPolicyViolation::Read`] if
/// `manifest`'s `[sandbox.fs]` names any path at all — the vocabulary was
/// removed; naming one is refused, not silently dropped.
pub fn enforce_fs_policy(manifest: &ManifestView) -> Result<(), FsPolicyViolation> {
    // Writes first: a package asking for `/` is the more alarming of the
    // two and should be the line the user reads.
    if let Some(entry) = manifest.sandbox.fs.write.first() {
        return Err(FsPolicyViolation::Write {
            package: manifest.package_id.clone(),
            entry: entry.clone(),
            app_dir: crate::appdir::SANDBOX_APP_DIR,
        });
    }
    if let Some(entry) = manifest.sandbox.fs.read.first() {
        return Err(FsPolicyViolation::Read {
            package: manifest.package_id.clone(),
            entry: entry.clone(),
        });
    }
    Ok(())
}

/// A service requirement written as a manifest would (`"required@1"`).
#[cfg(test)]
pub(crate) fn service_req(raw: &str) -> ServiceReq {
    ServiceReq::parse("", raw).expect("valid service requirement")
}

/// A canonical manifest for tests: `package` is the body of `[package]`,
/// `tail` the tables after the sandbox. `[sandbox.net]` is `off` unless
/// `tail` sets it.
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

    #[test]
    fn desktop_contract_uses_linked_runtime_without_override() {
        let dir = tempfile::tempdir().unwrap();
        let path =
            desktop_fixture(dir.path(), std::env::consts::OS, std::env::consts::ARCH).unwrap();
        let manifest = read_manifest(&path).unwrap();
        assert!(manifest.runtime.is_none());
        assert!(
            crate::run::resolve_runtime(&manifest, &crate::run::RunOptions::default())
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
            crate::run::resolve_runtime(&thin, &crate::run::RunOptions::default()),
            Err(crate::run::RunError::RuntimeBundleMissing { .. })
        ));
    }

    #[test]
    fn desktop_contract_refuses_foreign_host() {
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

    fn repo() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("workspace root above crates/packaging/launcher")
            .to_path_buf()
    }

    #[test]
    fn shipped_manifests_declare_what_they_need_to_open() {
        {
            let manifest = read_manifest(&repo().join("apps/browser/tilo-app.toml"))
                .expect("shipped browser manifest parses through installed-app schema");
            assert_eq!(
                manifest.continuity.required,
                vec![ContinuityFacet::Presentation, ContinuityFacet::Semantic]
            );
        }
        // Phase `shipped_settings_does_not_require_model_services_to_open`.
        // Settings is the recovery surface for networking, accounts, displays,
        // and the rest of the OS.  A device without the optional local-model
        // stack must still be able to open it; the Agent Models pane handles an
        // unavailable backend in-app.
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
                        && requirement.version == runtime_portal::RUNTIME_INTERFACE_VERSION,
                    "Settings must open when {service} is not running"
                );
            }
        }
    }

    fn manifest_with(block: &str) -> ManifestView {
        let src = format!(
            "[package]\nid = \"com.example.hostile\"\nversion = \"1.0.0\"\nkind = \"app\"\n\
             [publisher]\nidentity = \"ed25519:{}\"\n\
             [sandbox]\nnamespaces = []\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n\
             [sandbox.net]\nmode = \"off\"\n\
             [entrypoint]\nbin = \"bin/x\"\n{block}",
            "00".repeat(32)
        );
        ManifestView::from_toml_str(&src).expect("a manifest declaring fs paths still decodes")
    }

    #[test]
    fn manifests_may_name_no_filesystem_path() {
        // Phase `every_path_a_package_could_name_is_refused`.
        // The six shapes the original hole made possible, each refused by
        // naming the package and entry.
        {
            for entry in [
                "~",
                "/",
                "~/.ssh",
                "~/../../etc",
                "~/.local/share/so.tilo.totp",
                "$XDG_DATA_HOME/so.tilo.totp",
            ] {
                let m = manifest_with(&format!("[sandbox.fs]\nwrite = [{entry:?}]\n"));
                let Err(err) = enforce_fs_policy(&m) else {
                    panic!("write = [{entry:?}] must be refused");
                };
                let text = err.to_string();
                assert!(
                    text.contains("com.example.hostile") && text.contains(entry),
                    "the refusal must name the package and the entry: {text}"
                );
            }
        }
        // Phase `a_declared_read_is_refused_whether_or_not_the_profile_covers_it`.
        // A read is refused on the same terms — the read profile isn't a
        // filter to add to, an entry it already covers is refused too.
        {
            for entry in ["/etc/shadow", "/var/workspace", "/usr/share/fonts"] {
                let m = manifest_with(&format!("[sandbox.fs]\nread = [{entry:?}]\n"));
                let Err(err) = enforce_fs_policy(&m) else {
                    panic!("read = [{entry:?}] must be refused");
                };
                assert!(
                    err.to_string().contains(entry),
                    "the refusal must name the entry: {err}"
                );
            }
        }
        // Phase `a_manifest_that_names_no_path_passes`.
        // A manifest with no `[sandbox.fs]` block is the normal case and must
        // stay silent — an empty list is not a declaration.
        {
            let m = manifest_with("");
            assert!(enforce_fs_policy(&m).is_ok());
            let empty = manifest_with("[sandbox.fs]\nread = []\nwrite = []\n");
            assert!(
                enforce_fs_policy(&empty).is_ok(),
                "an explicitly empty block declares nothing"
            );
        }
        // Phase `no_shipped_manifest_names_a_path`.
        // Every manifest the OS ships goes through the same check as a
        // stranger's — no exemption.
        {
            let mut checked = 0usize;
            for entry in std::fs::read_dir(repo().join("apps")).expect("apps/ is readable") {
                let path = entry.expect("dir entry").path().join("tilo-app.toml");
                if !path.exists() {
                    continue;
                }
                let m = read_manifest(&path)
                    .unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()));
                enforce_fs_policy(&m)
                    .unwrap_or_else(|e| panic!("{} is refused by its own OS: {e}", path.display()));
                checked += 1;
            }
            assert!(
                checked >= 40,
                "expected the shipped app fleet, found {checked} manifests"
            );
        }
    }
}
