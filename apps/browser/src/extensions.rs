//! Unpacked extension inventory and explicit manifest-scoped startup grants.
//! CEF's Chrome runtime loads approved MV3 packages on the next browser launch.

use std::path::{Path, PathBuf};

use bounded_zip::{BoundedArchive, ZipError, ZipLimits};

/// Inventory record for one unpacked extension.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtensionInfo {
    /// Stable identifier: the Firefox add-on id when the manifest
    /// declares one, otherwise a slug of the name.
    pub id: String,
    pub name: String,
    pub version: String,
    pub manifest_version: u64,
    pub description: String,
    pub permissions: Vec<String>,
    /// Unpacked location on disk.
    pub path: PathBuf,
}

#[derive(Debug)]
pub enum ExtensionError {
    Download(String),
    Archive(String),
    Manifest(String),
    Io(std::io::Error),
}

const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;
/// Entry count and declared unpacked size are checked before anything is
/// inflated or written to disk.
const ARCHIVE_LIMITS: ZipLimits = ZipLimits {
    max_parts: 4_096,
    max_part_bytes: 256 * 1024 * 1024,
    max_total_bytes: 256 * 1024 * 1024,
};
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
/// The manager and resource surface materialize at most this many locally
/// installed extensions. This keeps a hostile or accidentally huge profile
/// from turning a refresh into unbounded filesystem work.
pub const MAX_INSTALLED_EXTENSIONS: usize = 100;
const MAX_EXTENSION_NAME_BYTES: usize = 512;
const MAX_EXTENSION_VERSION_BYTES: usize = 128;
const MAX_EXTENSION_DESCRIPTION_BYTES: usize = 8 * 1024;
const MAX_EXTENSION_PERMISSIONS: usize = 128;
const MAX_EXTENSION_PERMISSION_BYTES: usize = 512;

impl std::fmt::Display for ExtensionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Download(e) => write!(f, "download failed: {e}"),
            Self::Archive(e) => write!(f, "archive invalid: {e}"),
            Self::Manifest(e) => write!(f, "manifest invalid: {e}"),
            Self::Io(e) => write!(f, "io: {e}"),
        }
    }
}

fn package_error(error: ZipError) -> ExtensionError {
    ExtensionError::Archive(match error {
        ZipError::TooManyEntries { .. } => "extension package has too many files".into(),
        error if error.is_limit_breach() => {
            "extension package expands beyond the safety limit".into()
        }
        error => error.to_string(),
    })
}

impl From<std::io::Error> for ExtensionError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Root directory for unpacked extensions.
pub fn extensions_dir() -> PathBuf {
    crate::storage::data_dir()
        .unwrap_or_else(|| std::env::temp_dir().join("tilo-browser"))
        .join("extensions")
}

/// Unpack an extension archive into `root` and inventory it.
///
/// A CRX package must carry a valid CRX3 signature; its extension identifier
/// is the one its signing key names and its public key is written into the
/// unpacked manifest as `key`, so the unpacked copy keeps that identifier.
/// Plain zips (`.xpi`, `.zip`) are accepted as unsigned packages.
///
/// # Errors
///
/// Returns [`ExtensionError::Archive`] if `archive` isn't a valid CRX/zip
/// package or fails verification, [`ExtensionError::Manifest`] if it has no
/// valid `manifest.json`, or [`ExtensionError::Io`] if unpacking it under
/// `root` fails.
pub fn install_from_bytes(archive: &[u8], root: &Path) -> Result<ExtensionInfo, ExtensionError> {
    install_package(archive, root, None)
}

/// Like [`install_from_bytes`], for a package that must be the Web Store item
/// `expected_id`: a signed package for any other identifier is refused.
///
/// # Errors
///
/// As [`install_from_bytes`], plus [`ExtensionError::Archive`] when the
/// verified identifier is not `expected_id`.
pub fn install_verified_crx(
    archive: &[u8],
    root: &Path,
    expected_id: &str,
) -> Result<ExtensionInfo, ExtensionError> {
    if !crate::crx::is_crx(archive) {
        return Err(ExtensionError::Archive("not a CRX package".into()));
    }
    install_package(archive, root, Some(expected_id))
}

fn install_package(
    archive: &[u8],
    root: &Path,
    expected_id: Option<&str>,
) -> Result<ExtensionInfo, ExtensionError> {
    if archive.len() > MAX_ARCHIVE_BYTES {
        return Err(ExtensionError::Archive(
            "extension package is too large".into(),
        ));
    }
    let (zip_bytes, signed) = if crate::crx::is_crx(archive) {
        let verified = crate::crx::verify(archive)
            .map_err(|error| ExtensionError::Archive(error.to_string()))?;
        if expected_id.is_some_and(|expected| expected != verified.id) {
            return Err(ExtensionError::Archive(
                "package is signed for a different extension".into(),
            ));
        }
        (verified.archive, Some((verified.id, verified.public_key)))
    } else {
        (archive, None)
    };
    let mut zip = BoundedArchive::from_bytes(zip_bytes, ARCHIVE_LIMITS).map_err(package_error)?;
    zip.check_declared_sizes().map_err(package_error)?;

    // Manifest first, so a broken package never touches the disk.
    let manifest_text = zip
        .read_text("manifest.json", MAX_MANIFEST_BYTES)
        .map_err(|error| match error {
            ZipError::Missing { .. } => {
                ExtensionError::Manifest("no manifest.json in package".into())
            }
            error if error.is_limit_breach() => {
                ExtensionError::Manifest("manifest.json is too large".into())
            }
            error => ExtensionError::Manifest(error.to_string()),
        })?;
    let mut info = parse_manifest(&manifest_text)?;
    let keyed_manifest = match &signed {
        Some((id, key)) => {
            info.id.clone_from(id);
            Some(manifest_with_key(&manifest_text, key)?)
        }
        None => None,
    };

    let dest = root.join(&info.id);
    if dest.exists() {
        std::fs::remove_dir_all(&dest)?;
    }
    std::fs::create_dir_all(&dest)?;
    zip.archive_mut()
        .extract(&dest)
        .map_err(|e| ExtensionError::Archive(e.to_string()))?;
    if let Some(manifest) = keyed_manifest {
        std::fs::write(dest.join("manifest.json"), manifest)?;
    }
    info.path = dest;
    Ok(info)
}

/// The manifest with the package's public key as `key`: Chromium derives an
/// unpacked extension's identifier from that key, and the Web Store strips it.
fn manifest_with_key(manifest: &str, public_key: &[u8]) -> Result<String, ExtensionError> {
    use base64::Engine;
    let mut value: serde_json::Value =
        serde_json::from_str(manifest).map_err(|e| ExtensionError::Manifest(e.to_string()))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| ExtensionError::Manifest("manifest is not an object".into()))?;
    object.insert(
        "key".into(),
        serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(public_key)),
    );
    serde_json::to_string(&value).map_err(|e| ExtensionError::Manifest(e.to_string()))
}

/// All extensions currently unpacked under [`extensions_dir`].
pub fn list_installed() -> Vec<ExtensionInfo> {
    list_in(&extensions_dir())
}

/// Stable, address-safe resource identity for an installed extension.
///
/// Extension identifiers are validated when manifests are parsed, so this
/// preserves the user-visible add-on identity without admitting an action
/// address separator or a filesystem path.
pub fn resource_id(id: &str) -> String {
    format!("browser.extension:{id}")
}

fn list_in(root: &Path) -> Vec<ExtensionInfo> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found: Vec<ExtensionInfo> = entries
        .take(MAX_INSTALLED_EXTENSIONS)
        .flatten()
        .filter_map(|entry| {
            let manifest = entry.path().join("manifest.json");
            let text = read_manifest(&manifest).ok()?;
            let mut info = parse_manifest(&text).ok()?;
            // A package installed from a signed CRX lives under the
            // identifier its key names, which is the identity Chromium shows.
            if let Some(name) = entry.file_name().to_str()
                && crate::crx::is_extension_id(name)
            {
                name.clone_into(&mut info.id);
            }
            info.path = entry.path();
            Some(info)
        })
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

fn read_manifest(path: &Path) -> Result<String, ExtensionError> {
    let length = std::fs::metadata(path)?.len();
    if length > MAX_MANIFEST_BYTES {
        return Err(ExtensionError::Manifest(
            "manifest.json is too large".into(),
        ));
    }
    std::fs::read_to_string(path).map_err(ExtensionError::Io)
}

fn parse_manifest(text: &str) -> Result<ExtensionInfo, ExtensionError> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| ExtensionError::Manifest(e.to_string()))?;
    let name = bounded_manifest_string(
        value
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ExtensionError::Manifest("manifest has no name".into()))?,
        "name",
        MAX_EXTENSION_NAME_BYTES,
    )?;
    let version = bounded_manifest_string(
        value
            .get("version")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ExtensionError::Manifest("manifest has no version".into()))?,
        "version",
        MAX_EXTENSION_VERSION_BYTES,
    )?;
    let manifest_version = value
        .get("manifest_version")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(2);
    let description = bounded_manifest_string(
        value
            .get("description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default(),
        "description",
        MAX_EXTENSION_DESCRIPTION_BYTES,
    )?;
    let permissions = value
        .get("permissions")
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            if a.len() > MAX_EXTENSION_PERMISSIONS {
                return Err(ExtensionError::Manifest(
                    "manifest has too many permissions".into(),
                ));
            }
            a.iter()
                .filter_map(|p| p.as_str().map(str::to_string))
                .map(|permission| {
                    bounded_manifest_string(
                        &permission,
                        "permission",
                        MAX_EXTENSION_PERMISSION_BYTES,
                    )
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    // Firefox packages carry a stable add-on id.
    let nested_id = |top: &str| {
        value
            .get(top)
            .and_then(|v| v.get("gecko"))
            .and_then(|v| v.get("id"))
            .and_then(serde_json::Value::as_str)
    };
    let gecko_id = nested_id("browser_specific_settings").or_else(|| nested_id("applications"));
    let id = gecko_id
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 255
                && id.chars().all(|character| {
                    character.is_ascii_alphanumeric() || "@._-".contains(character)
                })
        })
        .map_or_else(|| slug(&name), str::to_string);

    Ok(ExtensionInfo {
        id,
        name,
        version,
        manifest_version,
        description,
        permissions,
        path: PathBuf::new(),
    })
}

fn bounded_manifest_string(
    value: &str,
    field: &str,
    max_bytes: usize,
) -> Result<String, ExtensionError> {
    if value.len() > max_bytes {
        return Err(ExtensionError::Manifest(format!(
            "manifest {field} exceeds the safety limit"
        )));
    }
    Ok(value.to_string())
}

fn slug(name: &str) -> String {
    let mut out: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    out.truncate(64);
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let out = out.trim_matches('-');
    if out.is_empty() {
        // Keep international display names valid even when no ASCII letter is
        // available for a filesystem component. A deterministic digest avoids
        // the dangerous empty component (`root.join(\"\") == root`).
        format!("extension-{:.16}", blake3::hash(name.as_bytes()).to_hex())
    } else {
        out.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    const MANIFEST: &str = r#"{
        "manifest_version": 2,
        "name": "Dark Reader",
        "version": "4.9.0",
        "description": "Dark mode for every website",
        "permissions": ["storage", "tabs", "<all_urls>"],
        "browser_specific_settings": {"gecko": {"id": "addon@darkreader.org"}}
    }"#;

    fn fixture_zip() -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default();
            zip.start_file("manifest.json", opts).unwrap();
            zip.write_all(MANIFEST.as_bytes()).unwrap();
            zip.start_file("background.js", opts).unwrap();
            zip.write_all(b"// noop").unwrap();
            zip.finish().unwrap();
        }
        buf.into_inner()
    }

    /// Installing XPI and CRX3, listing and rejecting invalid archives.
    #[test]
    fn extension_archives_install_list_and_reject_invalid_packages() {
        // Phase `installs_xpi_and_reads_manifest`.
        {
            let root = tempfile::tempdir().unwrap();
            let info = install_from_bytes(&fixture_zip(), root.path()).unwrap();
            assert_eq!(info.id, "addon@darkreader.org");
            assert_eq!(info.name, "Dark Reader");
            assert_eq!(info.manifest_version, 2);
            assert!(info.permissions.contains(&"storage".to_string()));
            assert!(info.path.join("background.js").exists());
        }

        // Phase `installs_signed_crx3_under_its_signed_identity_with_key`.
        {
            use crate::crx::fixtures::{Signer, crx};
            let signer = Signer::new();
            let package = crx(&signer, &[&signer], &fixture_zip());
            let root = tempfile::tempdir().unwrap();
            let info = install_from_bytes(&package, root.path()).unwrap();
            assert_eq!(info.name, "Dark Reader");
            assert_eq!(info.id, crate::crx::extension_id(&signer.spki));
            assert_eq!(info.path, root.path().join(&info.id));
            // The unpacked manifest carries the key, so Chromium derives the
            // same identifier when it loads the directory.
            let manifest: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(info.path.join("manifest.json")).unwrap(),
            )
            .unwrap();
            let key = manifest["key"].as_str().unwrap();
            let der =
                base64::Engine::decode(&base64::engine::general_purpose::STANDARD, key).unwrap();
            assert_eq!(crate::crx::extension_id(&der), info.id);
            assert_eq!(list_in(root.path())[0].id, info.id);

            // A package signed for another extension is not the requested item.
            assert!(install_verified_crx(&package, root.path(), &"a".repeat(32)).is_err());
            assert!(install_verified_crx(&package, root.path(), &info.id).is_ok());

            // An unsigned or tampered CRX never reaches the disk.
            let mut tampered = package;
            *tampered.last_mut().unwrap() ^= 1;
            let empty = tempfile::tempdir().unwrap();
            assert!(matches!(
                install_from_bytes(&tampered, empty.path()),
                Err(ExtensionError::Archive(_))
            ));
            let mut unsigned = b"Cr24".to_vec();
            unsigned.extend(3u32.to_le_bytes());
            unsigned.extend(20u32.to_le_bytes());
            unsigned.extend([0xAAu8; 20]);
            unsigned.extend(fixture_zip());
            assert!(install_from_bytes(&unsigned, empty.path()).is_err());
            assert_eq!(std::fs::read_dir(empty.path()).unwrap().count(), 0);
        }

        // Phase `lists_installed_extensions`.
        {
            let root = tempfile::tempdir().unwrap();
            install_from_bytes(&fixture_zip(), root.path()).unwrap();
            let listed = list_in(root.path());
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].name, "Dark Reader");
        }

        // Phase `rejects_zip_without_manifest`.
        {
            let mut buf = Cursor::new(Vec::new());
            {
                let mut zip = zip::ZipWriter::new(&mut buf);
                let opts = zip::write::SimpleFileOptions::default();
                zip.start_file("readme.txt", opts).unwrap();
                zip.write_all(b"hi").unwrap();
                zip.finish().unwrap();
            }
            let root = tempfile::tempdir().unwrap();
            assert!(matches!(
                install_from_bytes(&buf.into_inner(), root.path()),
                Err(ExtensionError::Manifest(_))
            ));
        }

        // Phase `oversized_archive_is_rejected_before_parsing`.
        {
            let bytes = vec![0; MAX_ARCHIVE_BYTES + 1];
            assert!(matches!(
                install_from_bytes(&bytes, std::path::Path::new("/unused")),
                Err(ExtensionError::Archive(_))
            ));
        }
    }

    /// Identifier fallbacks, path containment and manifest bounds.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn extension_identifiers_and_manifests_are_safe_and_bounded() {
        // Phase `slug_fallback_for_manifests_without_gecko_id`.
        {
            let manifest = r#"{"manifest_version": 3, "name": "My Cool Tool!", "version": "1.0"}"#;
            let info = parse_manifest(manifest).unwrap();
            assert_eq!(info.id, "my-cool-tool");
            assert_eq!(info.manifest_version, 3);
        }

        // Phase `manifest_identifier_cannot_escape_extension_root`.
        {
            let manifest = r#"{
                "manifest_version": 3,
                "name": "Safe Name",
                "version": "1.0",
                "browser_specific_settings": {"gecko": {"id": "../../outside"}}
            }"#;
            let info = parse_manifest(manifest).unwrap();
            assert_eq!(info.id, "safe-name");
            assert!(!info.id.contains('/'));
        }

        // Phase `non_ascii_name_gets_a_nonempty_safe_fallback_identifier`.
        {
            let manifest = r#"{"manifest_version": 3, "name": "🌙", "version": "1.0"}"#;
            let info = parse_manifest(manifest).unwrap();
            assert!(info.id.starts_with("extension-"));
            assert!(info.id.len() > "extension-".len());
            assert_eq!(
                resource_id(&info.id),
                format!("browser.extension:{}", info.id)
            );
        }

        // Phase `inventory_skips_manifests_larger_than_the_install_limit`.
        {
            let root = tempfile::tempdir().unwrap();
            let extension = root.path().join("oversized");
            std::fs::create_dir(&extension).unwrap();
            std::fs::write(
                extension.join("manifest.json"),
                vec![b' '; MAX_MANIFEST_BYTES as usize + 1],
            )
            .unwrap();

            assert!(list_in(root.path()).is_empty());
        }

        // Phase `manifest_fields_and_permission_lists_are_bounded`.
        {
            let oversized_name = "a".repeat(MAX_EXTENSION_NAME_BYTES + 1);
            let manifest = format!(
                r#"{{"manifest_version": 3, "name": "{oversized_name}", "version": "1.0"}}"#
            );
            assert!(matches!(
                parse_manifest(&manifest),
                Err(ExtensionError::Manifest(_))
            ));

            let permissions = std::iter::repeat_n("tabs", MAX_EXTENSION_PERMISSIONS + 1)
                .map(|permission| format!("\"{permission}\""))
                .collect::<Vec<_>>()
                .join(",");
            let manifest = format!(
                r#"{{"manifest_version": 3, "name": "Safe", "version": "1.0", "permissions": [{permissions}]}}"#
            );
            assert!(matches!(
                parse_manifest(&manifest),
                Err(ExtensionError::Manifest(_))
            ));
        }
    }
}

#[cfg(test)]
mod approval_tests {
    use super::*;
    #[test]
    fn approval_is_explicit_and_invalidated_by_manifest_changes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("addon");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("manifest.json"),r#"{"name":"fixture","version":"1","manifest_version":3,"permissions":["storage"],"host_permissions":["https://example.org/*"]}"#).unwrap();
        let extension = list_in(root.path()).pop().unwrap();
        assert!(!is_approved(&extension));
        assert!(
            permission_summary(&extension)
                .unwrap()
                .contains("https://example.org/*")
        );
        set_approved(&extension, true).unwrap();
        assert!(is_approved(&extension));
        std::fs::write(
            path.join("manifest.json"),
            r#"{"name":"fixture","version":"2","manifest_version":3,"permissions":["tabs"]}"#,
        )
        .unwrap();
        assert!(!is_approved(&extension));
    }
}

fn approval_path(extension: &ExtensionInfo) -> Option<PathBuf> {
    let path = extension.path.canonicalize().ok()?;
    let key = blake3::hash(path.to_str()?.as_bytes()).to_hex();
    Some(path.parent()?.join(format!(".tilo-approval-{key}.json")))
}
fn manifest_digest(extension: &ExtensionInfo) -> Result<String, ExtensionError> {
    let text = read_manifest(&extension.path.join("manifest.json"))?;
    Ok(blake3::hash(text.as_bytes()).to_hex().to_string())
}
pub fn is_approved(extension: &ExtensionInfo) -> bool {
    extension.manifest_version == 3
        && approval_path(extension)
            .and_then(|path| std::fs::read_to_string(path).ok())
            .zip(manifest_digest(extension).ok())
            .is_some_and(|(approved, current)| approved == current)
}
/// Update startup approval for this installed extension.
///
/// # Errors
/// Returns an error for an invalid extension path, unsupported manifest version or filesystem failure.
pub fn set_approved(extension: &ExtensionInfo, approved: bool) -> Result<(), ExtensionError> {
    let path = approval_path(extension)
        .ok_or_else(|| ExtensionError::Manifest("Extension path is invalid".into()))?;
    if approved {
        if extension.manifest_version != 3 {
            return Err(ExtensionError::Manifest(
                "Only Manifest V3 startup loading is supported".into(),
            ));
        }
        std::fs::write(path, manifest_digest(extension)?)?;
    } else if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}
pub fn approved_paths() -> Vec<PathBuf> {
    approved_in(&extensions_dir())
}

fn approved_in(root: &Path) -> Vec<PathBuf> {
    list_in(root)
        .iter()
        .filter(|extension| is_approved(extension))
        .filter_map(|extension| extension.path.canonicalize().ok())
        .collect()
}
/// Remove an installed extension: its files, its approval and its pin.
///
/// # Errors
/// Returns an error when the directory is not an installed extension of
/// `root` or cannot be removed.
pub fn uninstall(extension: &ExtensionInfo, root: &Path) -> Result<(), ExtensionError> {
    let path = extension.path.canonicalize()?;
    let root = root.canonicalize()?;
    if path.parent() != Some(root.as_path()) {
        return Err(ExtensionError::Manifest(
            "Extension is not installed in the profile".into(),
        ));
    }
    set_approved(extension, false)?;
    set_pinned_in(&root, &extension.id, false)?;
    std::fs::remove_dir_all(&path)?;
    Ok(())
}

fn pinned_path(root: &Path) -> PathBuf {
    root.join(".tilo-pinned.json")
}

fn pinned_in(root: &Path) -> Vec<String> {
    std::fs::read_to_string(pinned_path(root))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Whether the person pinned this extension's button to the toolbar.
pub fn is_pinned(id: &str) -> bool {
    pinned_in(&extensions_dir())
        .iter()
        .any(|pinned| pinned == id)
}

fn set_pinned_in(root: &Path, id: &str, pinned: bool) -> Result<(), ExtensionError> {
    let mut ids = pinned_in(root);
    ids.retain(|existing| existing != id);
    if pinned {
        ids.push(id.to_owned());
    }
    let path = pinned_path(root);
    if ids.is_empty() && !path.exists() {
        return Ok(());
    }
    let text = serde_json::to_string(&ids).map_err(|e| ExtensionError::Manifest(e.to_string()))?;
    std::fs::write(path, text)?;
    Ok(())
}

/// Pin or unpin an extension's toolbar button.
///
/// # Errors
/// Returns an error when the preference cannot be written.
pub fn set_pinned(id: &str, pinned: bool) -> Result<(), ExtensionError> {
    let root = extensions_dir();
    std::fs::create_dir_all(&root)?;
    set_pinned_in(&root, id, pinned)
}

/// Summarize permissions declared by an installed extension.
///
/// # Errors
/// Returns an error if the manifest cannot be read or parsed.
pub fn permission_summary(extension: &ExtensionInfo) -> Result<String, ExtensionError> {
    summarize_manifest(&read_manifest(&extension.path.join("manifest.json"))?)
}

/// The permission summary for manifest text, before anything is installed.
///
/// # Errors
/// Returns an error if the manifest cannot be parsed.
pub fn summarize_manifest(text: &str) -> Result<String, ExtensionError> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|error| ExtensionError::Manifest(error.to_string()))?;
    let mut permissions = Vec::new();
    for key in [
        "permissions",
        "host_permissions",
        "optional_permissions",
        "optional_host_permissions",
    ] {
        if let Some(items) = value.get(key).and_then(serde_json::Value::as_array) {
            for item in items {
                if let Some(item) = item.as_str() {
                    permissions.push(format!("{key}: {item}"));
                }
            }
        }
    }
    if let Some(scripts) = value
        .get("content_scripts")
        .and_then(serde_json::Value::as_array)
    {
        for script in scripts {
            if let Some(matches) = script.get("matches").and_then(serde_json::Value::as_array) {
                for pattern in matches {
                    if let Some(pattern) = pattern.as_str() {
                        permissions.push(format!("content scripts: {pattern}"));
                    }
                }
            }
        }
    }
    Ok(if permissions.is_empty() {
        "No declared permissions".into()
    } else {
        permissions.join("; ")
    })
}

/// A verified package, read but not yet installed: what the person is shown
/// before they agree to add it.
#[derive(Debug, Clone)]
pub struct StagedPackage {
    pub info: ExtensionInfo,
    pub permissions: String,
    pub bytes: std::sync::Arc<Vec<u8>>,
    /// The identifier its CRX3 signature names; `None` for an unsigned zip.
    pub signed_id: Option<String>,
}

impl StagedPackage {
    /// Unpack the staged package under `root`.
    ///
    /// # Errors
    /// As [`install_from_bytes`].
    pub fn install(&self, root: &Path) -> Result<ExtensionInfo, ExtensionError> {
        install_package(&self.bytes, root, self.signed_id.as_deref())
    }
}

/// Verify a package from a file (a CRX3 signed by its own key, or an
/// unsigned zip) and read its manifest.
///
/// # Errors
/// As [`stage_package`]; an unsigned zip needs only a valid manifest.
pub fn stage_file(bytes: Vec<u8>) -> Result<StagedPackage, ExtensionError> {
    if crate::crx::is_crx(&bytes) {
        let id = crate::crx::verify(&bytes)
            .map_err(|error| ExtensionError::Archive(error.to_string()))?
            .id;
        return stage_package(bytes, &id);
    }
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(ExtensionError::Archive(
            "extension package is too large".into(),
        ));
    }
    let mut zip = BoundedArchive::from_bytes(&bytes, ARCHIVE_LIMITS).map_err(package_error)?;
    zip.check_declared_sizes().map_err(package_error)?;
    let manifest = zip
        .read_text("manifest.json", MAX_MANIFEST_BYTES)
        .map_err(|error| ExtensionError::Manifest(error.to_string()))?;
    let info = parse_manifest(&manifest)?;
    let permissions = summarize_manifest(&manifest)?;
    Ok(StagedPackage {
        info,
        permissions,
        bytes: std::sync::Arc::new(bytes),
        signed_id: None,
    })
}

/// Verify a Web Store package for `expected_id` and read its manifest without
/// touching the disk.
///
/// # Errors
/// Returns an error when the package is not a valid CRX3 signed for
/// `expected_id`, or its manifest is missing or invalid.
pub fn stage_package(bytes: Vec<u8>, expected_id: &str) -> Result<StagedPackage, ExtensionError> {
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(ExtensionError::Archive(
            "extension package is too large".into(),
        ));
    }
    let verified =
        crate::crx::verify(&bytes).map_err(|error| ExtensionError::Archive(error.to_string()))?;
    if verified.id != expected_id {
        return Err(ExtensionError::Archive(
            "package is signed for a different extension".into(),
        ));
    }
    let mut zip =
        BoundedArchive::from_bytes(verified.archive, ARCHIVE_LIMITS).map_err(package_error)?;
    zip.check_declared_sizes().map_err(package_error)?;
    let manifest = zip
        .read_text("manifest.json", MAX_MANIFEST_BYTES)
        .map_err(|error| ExtensionError::Manifest(error.to_string()))?;
    let mut info = parse_manifest(&manifest)?;
    info.id = verified.id;
    let permissions = summarize_manifest(&manifest)?;
    Ok(StagedPackage {
        info,
        permissions,
        bytes: std::sync::Arc::new(bytes),
        signed_id: Some(expected_id.to_owned()),
    })
}

/// The identifier Chromium gives this extension when it loads its directory:
/// derived from the manifest `key` when there is one, otherwise from the
/// absolute path.
pub fn chrome_id(extension: &ExtensionInfo) -> Option<String> {
    use base64::Engine;
    let manifest: serde_json::Value =
        serde_json::from_str(&read_manifest(&extension.path.join("manifest.json")).ok()?).ok()?;
    let bytes = if let Some(key) = manifest.get("key").and_then(serde_json::Value::as_str) {
        base64::engine::general_purpose::STANDARD.decode(key).ok()?
    } else {
        extension
            .path
            .canonicalize()
            .ok()?
            .to_str()?
            .as_bytes()
            .to_vec()
    };
    Some(crate::crx::extension_id(&bytes))
}

/// Chromium derives an unpacked extension identity from its key, or its absolute path.
pub fn action_url(extension: &ExtensionInfo) -> Option<String> {
    let manifest: serde_json::Value =
        serde_json::from_str(&read_manifest(&extension.path.join("manifest.json")).ok()?).ok()?;
    let popup = manifest.get("action")?.get("default_popup")?.as_str()?;
    if popup.starts_with('/')
        || popup.split('/').any(|part| part == "..")
        || popup.contains(['\\', ':', '#', '?'])
    {
        return None;
    }
    Some(format!(
        "chrome-extension://{}/{popup}",
        chrome_id(extension)?
    ))
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::crx::fixtures::{Signer, crx};
    use std::io::{Cursor, Write};

    fn fixture_zip_only() -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default();
            zip.start_file("manifest.json", opts).unwrap();
            zip.write_all(br#"{"manifest_version":3,"name":"Plain","version":"1"}"#)
                .unwrap();
            zip.finish().unwrap();
        }
        buf.into_inner()
    }

    fn package(signer: &Signer) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default();
            zip.start_file("manifest.json", opts).unwrap();
            zip.write_all(
                br#"{"manifest_version":3,"name":"Lite","version":"1.0","permissions":["storage"],"host_permissions":["<all_urls>"],"action":{"default_popup":"popup.html"}}"#,
            )
            .unwrap();
            zip.start_file("popup.html", opts).unwrap();
            zip.write_all(b"<title>popup</title>").unwrap();
            zip.finish().unwrap();
        }
        crx(signer, &[signer], &buf.into_inner())
    }

    #[test]
    fn install_enable_pin_disable_and_uninstall() {
        let root = tempfile::tempdir().unwrap();
        let signer = Signer::new();
        let id = crate::crx::extension_id(&signer.spki);

        // phase: staging_verifies_and_summarises_without_touching_the_disk
        let staged = stage_package(package(&signer), &id).unwrap();
        assert_eq!(staged.info.name, "Lite");
        assert_eq!(staged.info.id, id);
        assert!(staged.permissions.contains("host_permissions: <all_urls>"));
        assert!(list_in(root.path()).is_empty());
        assert!(stage_package(package(&signer), &"a".repeat(32)).is_err());
        assert_eq!(
            stage_file(package(&signer)).unwrap().signed_id,
            Some(id.clone())
        );
        let unsigned = stage_file(fixture_zip_only()).unwrap();
        assert_eq!(unsigned.signed_id, None);
        assert!(stage_file(b"neither a crx nor a zip".to_vec()).is_err());
        let installed = staged.install(root.path()).unwrap();
        assert_eq!(installed.id, id);
        std::fs::remove_dir_all(&installed.path).unwrap();

        // phase: installed_extensions_start_disabled_and_unpinned
        let info = install_verified_crx(&package(&signer), root.path(), &id).unwrap();
        assert_eq!(info.id, id);
        assert!(!is_approved(&info));
        assert!(approved_in(root.path()).is_empty());
        assert!(pinned_in(root.path()).is_empty());
        assert!(
            permission_summary(&info)
                .unwrap()
                .contains("host_permissions: <all_urls>")
        );

        // phase: enabling_loads_the_directory_and_pinning_is_independent
        set_approved(&info, true).unwrap();
        assert_eq!(
            approved_in(root.path()),
            vec![info.path.canonicalize().unwrap()]
        );
        set_pinned_in(root.path(), &id, true).unwrap();
        assert_eq!(pinned_in(root.path()), vec![id.clone()]);
        set_pinned_in(root.path(), &id, true).unwrap();
        assert_eq!(pinned_in(root.path()).len(), 1);

        // phase: disabling_keeps_files_and_pin
        set_approved(&info, false).unwrap();
        assert!(approved_in(root.path()).is_empty());
        assert!(info.path.join("popup.html").exists());
        assert_eq!(pinned_in(root.path()), vec![id.clone()]);

        // phase: reinstall_after_update_requires_a_fresh_approval
        set_approved(&info, true).unwrap();
        let updated = install_verified_crx(&package(&signer), root.path(), &id).unwrap();
        assert!(is_approved(&updated), "same package, same manifest digest");

        // phase: uninstall_removes_files_approval_and_pin
        uninstall(&updated, root.path()).unwrap();
        assert!(!info.path.exists());
        assert!(list_in(root.path()).is_empty());
        assert!(pinned_in(root.path()).is_empty());
        assert!(
            std::fs::read_dir(root.path())
                .unwrap()
                .flatten()
                .all(|entry| !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".tilo-approval-"))
        );

        // phase: uninstall_refuses_directories_outside_the_profile
        let outside = tempfile::tempdir().unwrap();
        let foreign = ExtensionInfo {
            path: outside.path().to_owned(),
            ..updated
        };
        assert!(uninstall(&foreign, root.path()).is_err());
        assert!(outside.path().exists());
    }
}
