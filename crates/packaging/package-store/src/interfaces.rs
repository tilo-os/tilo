//! Which system interfaces this OS serves, and whether a package fits it.
//!
//! The image declares the one version it serves of each system interface in
//! [`SYSTEM_INTERFACES_FILE`]; a package names the version it speaks of each
//! through the services of its manifest (see
//! [`tilo_app_package::interfaces`]). A host without the declaration (a
//! development machine, another distribution) judges nothing: every package
//! fits it, and a missing service still fails at launch as it always did.

use std::path::{Path, PathBuf};

use tilo_app_package::Manifest;
use tilo_app_package::interfaces::{InterfaceRequirement, ProvidedInterfaces, Unmet};

use crate::dev_env::dev_override;

/// Where the image declares the system interfaces it serves.
pub const SYSTEM_INTERFACES_FILE: &str = "/usr/share/tilo/system-interfaces.toml";
/// Names another declaration instead of [`SYSTEM_INTERFACES_FILE`], in
/// builds that honour developer overrides ([`crate::dev_env::DEV_ENV_OVERRIDES`]).
pub const SYSTEM_INTERFACES_ENV: &str = "TILO_SYSTEM_INTERFACES";

#[derive(Debug, thiserror::Error)]
pub enum InterfacesError {
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is not a system-interface declaration: {detail}")]
    Parse { path: PathBuf, detail: String },
}

/// The declaration this session reads.
pub fn system_interfaces_path() -> PathBuf {
    dev_override(SYSTEM_INTERFACES_ENV)
        .map_or_else(|| PathBuf::from(SYSTEM_INTERFACES_FILE), PathBuf::from)
}

/// The declaration at `path`; `None` when there is none.
///
/// # Errors
///
/// Returns an error when the file exists but cannot be read or parsed.
pub fn read_interfaces(path: &Path) -> Result<Option<ProvidedInterfaces>, InterfacesError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(InterfacesError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    parse_interfaces(&raw)
        .map(Some)
        .map_err(|detail| InterfacesError::Parse {
            path: path.to_path_buf(),
            detail,
        })
}

/// Parse a declaration's TOML.
///
/// # Errors
///
/// Returns the reason when `raw` is not a declaration, names a malformed
/// interface, service or version, lists a runtime service under a name that
/// is an interface of its own, or lists runtime services without serving
/// the runtime interface.
pub fn parse_interfaces(raw: &str) -> Result<ProvidedInterfaces, String> {
    let provided: ProvidedInterfaces = toml::from_str(raw).map_err(|error| error.to_string())?;
    for (name, version) in &provided.interfaces {
        InterfaceRequirement::new(name, *version).map_err(|error| error.to_string())?;
    }
    for service in &provided.runtime.services {
        if !tilo_app_package::interfaces::valid_interface_name(service) {
            return Err(format!("runtime service {service:?} is not a service name"));
        }
        if provided.interfaces.contains_key(service) {
            return Err(format!(
                "{service} is an interface of its own and a runtime service"
            ));
        }
    }
    if !provided.runtime.services.is_empty()
        && !provided
            .interfaces
            .contains_key(tilo_app_package::interfaces::RUNTIME_INTERFACE)
    {
        return Err("runtime services are listed but the runtime interface is not".into());
    }
    Ok(provided)
}

/// The interfaces this OS serves; `None` on a host that declares none.
///
/// A declaration that cannot be read is reported on stderr once and treated
/// as absent: judging packages against a guess would refuse ones that work.
#[allow(
    clippy::print_stderr,
    reason = "a one-time operator warning; the reader may run before tracing is initialised"
)]
pub fn system_interfaces() -> Option<ProvidedInterfaces> {
    match read_interfaces(&system_interfaces_path()) {
        Ok(provided) => provided,
        Err(error) => {
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| eprintln!("package-store: {error}"));
            None
        }
    }
}

/// The interface versions `manifest` speaks that `provided` does not serve
/// ([`Manifest::interface_versions`]). Empty when it fits, or when there is
/// no declaration to judge by.
pub fn unmet(provided: Option<&ProvidedInterfaces>, manifest: &Manifest) -> Vec<Unmet> {
    provided.map_or_else(Vec::new, |provided| {
        provided.unmet(&manifest.interface_versions())
    })
}

/// [`unmet`] for the manifest an installed row names. A manifest that
/// cannot be read is not judged here; the launcher reports it on launch.
pub fn unmet_at(provided: Option<&ProvidedInterfaces>, manifest_path: &Path) -> Vec<Unmet> {
    let Some(provided) = provided else {
        return Vec::new();
    };
    let mut unmet = std::fs::read_to_string(manifest_path)
        .ok()
        .and_then(|raw| Manifest::from_toml_str(&raw).ok())
        .map_or_else(Vec::new, |manifest| unmet(Some(provided), &manifest));
    unmet.extend(package_format_unmet(manifest_path));
    unmet
}

/// The interface name an installed package's recorded format is judged as.
pub const PACKAGE_INTERFACE: &str = "package";

/// An installed package whose signed record, beside its manifest, is at
/// another package format than this build reads: it was installed before
/// the format changed. It is behind like an app built for an older
/// interface, and its next install replaces it
/// ([`crate::integrity::record_current`]).
fn package_format_unmet(manifest_path: &Path) -> Option<Unmet> {
    let signed = manifest_path
        .parent()?
        .join(crate::layout::SIGNED_ROOT_FILE);
    let bytes = std::fs::read(signed).ok()?;
    let format = tilo_app_package::SignedRoot::format_of(&bytes)?;
    (format != tilo_app_package::TAPP_FORMAT_VERSION).then(|| Unmet {
        requirement: format!("{PACKAGE_INTERFACE}@{format}"),
        provided: Some(tilo_app_package::TAPP_FORMAT_VERSION),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETTINGS: &str = r#"
[package]
id = "so.tilo.settings"
version = "2.0.0"
kind = "app"

[publisher]
identity = "ed25519:0000000000000000000000000000000000000000000000000000000000000000"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[sandbox.services]
collab = "optional@1"
system = "required@2"
"#;

    #[test]
    fn a_package_fits_only_the_versions_the_image_serves() {
        let manifest = Manifest::from_toml_str(SETTINGS).unwrap();
        let os = |system: u32, runtime: u32| {
            parse_interfaces(&format!(
                "[interfaces]\nsystem = {system}\nruntime = {runtime}\n\n\
                 [runtime]\nservices = [\"collab\"]\n"
            ))
            .unwrap()
        };

        let older = unmet(Some(&os(1, 1)), &manifest);
        assert_eq!(older.len(), 1);
        assert_eq!(
            older[0].to_string(),
            "system@2 (this system provides system@1)"
        );
        assert!(unmet(Some(&os(2, 1)), &manifest).is_empty());
        // An optional service is spoken at its version where it is there.
        let runtime_moved = unmet(Some(&os(2, 2)), &manifest);
        assert_eq!(
            runtime_moved[0].to_string(),
            "collab@1 (this system provides collab@2)"
        );
        assert!(tilo_app_package::interfaces::package_is_behind(
            &runtime_moved
        ));
        // A host that declares nothing judges nothing.
        assert!(unmet(None, &manifest).is_empty());
    }

    #[test]
    fn declarations_are_read_strictly_and_absence_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("system-interfaces.toml");
        assert!(read_interfaces(&path).unwrap().is_none());

        std::fs::write(&path, "[interfaces]\nsystem = 1\n").unwrap();
        let provided = read_interfaces(&path).unwrap().unwrap();
        assert_eq!(provided.version("system"), Some(1));

        for bad in [
            "system = 1\n",
            "[interfaces]\nsystem = [1]\n",
            "[interfaces]\nsystem = [1, 2]\n",
            "[interfaces]\nsystem = 0\n",
            "[interfaces]\n\"Sys tem\" = 1\n",
            "[interfaces]\nsystem = \"1\"\n",
            "[interfaces]\nsystem = 1\n[runtime]\nservices = [\"collab\"]\n",
            "[interfaces]\nruntime = 1\nsystem = 1\n[runtime]\nservices = [\"system\"]\n",
        ] {
            std::fs::write(&path, bad).unwrap();
            assert!(read_interfaces(&path).is_err(), "{bad:?} parsed");
        }
    }

    #[test]
    fn an_installed_row_is_judged_by_its_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("manifest.toml");
        std::fs::write(&manifest, SETTINGS).unwrap();
        let old_os = parse_interfaces("[interfaces]\nsystem = 1\n").unwrap();
        assert_eq!(unmet_at(Some(&old_os), &manifest).len(), 2);
        assert!(unmet_at(Some(&old_os), &dir.path().join("missing.toml")).is_empty());
    }

    /// A package installed before the package format changed is behind,
    /// like an app built for an older interface: it is updated, never
    /// refused on every launch.
    #[test]
    fn an_installed_record_at_another_package_format_is_behind() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("manifest.toml");
        std::fs::write(&manifest, SETTINGS).unwrap();
        let os = parse_interfaces("[interfaces]\nsystem = 2\n").unwrap();
        let current = unmet_at(Some(&os), &manifest).len();
        let root = |format: u32| {
            let mut bytes = b"TLOPKG\0\0".to_vec();
            bytes.extend(format.to_le_bytes());
            bytes.resize(tilo_app_package::SIGNED_ROOT_LEN, 0);
            bytes
        };
        let signed = dir.path().join(crate::layout::SIGNED_ROOT_FILE);
        std::fs::write(&signed, root(tilo_app_package::TAPP_FORMAT_VERSION)).unwrap();
        assert_eq!(unmet_at(Some(&os), &manifest).len(), current);
        let older = tilo_app_package::TAPP_FORMAT_VERSION - 1;
        std::fs::write(&signed, root(older)).unwrap();
        let unmet = unmet_at(Some(&os), &manifest);
        assert_eq!(unmet.len(), current + 1);
        let format = unmet.last().unwrap();
        assert_eq!(format.requirement, format!("package@{older}"));
        assert_eq!(
            format.behind(),
            tilo_app_package::interfaces::Behind::Package
        );
    }
}
