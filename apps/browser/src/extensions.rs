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
/// # Errors
///
/// Returns [`ExtensionError::Archive`] if `archive` isn't a valid CRX/zip
/// package, [`ExtensionError::Manifest`] if it has no valid `manifest.json`,
/// or [`ExtensionError::Io`] if unpacking it under `root` fails.
pub fn install_from_bytes(archive: &[u8], root: &Path) -> Result<ExtensionInfo, ExtensionError> {
    if archive.len() > MAX_ARCHIVE_BYTES {
        return Err(ExtensionError::Archive(
            "extension package is too large".into(),
        ));
    }
    let zip_bytes = strip_crx_header(archive)?;
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

    let dest = root.join(&info.id);
    if dest.exists() {
        std::fs::remove_dir_all(&dest)?;
    }
    std::fs::create_dir_all(&dest)?;
    zip.archive_mut()
        .extract(&dest)
        .map_err(|e| ExtensionError::Archive(e.to_string()))?;
    info.path = dest;
    Ok(info)
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

/// `.crx` files wrap the zip in a small header; `.xpi` files ARE zips.
/// Returns the zip portion.
fn strip_crx_header(bytes: &[u8]) -> Result<&[u8], ExtensionError> {
    if !bytes.starts_with(b"Cr24") {
        return Ok(bytes); // plain zip (.xpi or already-stripped .crx)
    }
    let word = |at: usize| -> Result<u32, ExtensionError> {
        bytes
            .get(at..at + 4)
            .and_then(|s| <[u8; 4]>::try_from(s).ok())
            .map(u32::from_le_bytes)
            .ok_or_else(|| ExtensionError::Archive("truncated CRX header".into()))
    };
    let version = word(4)?;
    let zip_start = match version {
        2 => {
            let pubkey_len = word(8)? as usize;
            let sig_len = word(12)? as usize;
            16 + pubkey_len + sig_len
        }
        3 => {
            let header_len = word(8)? as usize;
            12 + header_len
        }
        v => {
            return Err(ExtensionError::Archive(format!(
                "unsupported CRX version {v}"
            )));
        }
    };
    bytes
        .get(zip_start..)
        .ok_or_else(|| ExtensionError::Archive("CRX header exceeds file size".into()))
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

        // Phase `installs_crx3_by_stripping_header`.
        {
            let zip_data = fixture_zip();
            let mut crx = Vec::new();
            crx.extend_from_slice(b"Cr24");
            crx.extend_from_slice(&3u32.to_le_bytes());
            let header = [0xAAu8; 20]; // opaque protobuf header, length 20
            crx.extend_from_slice(&(header.len() as u32).to_le_bytes());
            crx.extend_from_slice(&header);
            crx.extend_from_slice(&zip_data);

            let root = tempfile::tempdir().unwrap();
            let info = install_from_bytes(&crx, root.path()).unwrap();
            assert_eq!(info.name, "Dark Reader");
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
    list_installed()
        .iter()
        .filter(|extension| is_approved(extension))
        .filter_map(|extension| extension.path.canonicalize().ok())
        .collect()
}
/// Summarize permissions declared by an installed extension.
///
/// # Errors
/// Returns an error if the manifest cannot be read or parsed.
pub fn permission_summary(extension: &ExtensionInfo) -> Result<String, ExtensionError> {
    let value: serde_json::Value =
        serde_json::from_str(&read_manifest(&extension.path.join("manifest.json"))?)
            .map_err(|error| ExtensionError::Manifest(error.to_string()))?;
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
/// Chromium derives an unpacked extension identity from its key, or its absolute path.
pub fn action_url(extension: &ExtensionInfo) -> Option<String> {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let manifest: serde_json::Value =
        serde_json::from_str(&read_manifest(&extension.path.join("manifest.json")).ok()?).ok()?;
    let popup = manifest.get("action")?.get("default_popup")?.as_str()?;
    if popup.starts_with('/')
        || popup.split('/').any(|part| part == "..")
        || popup.contains(['\\', ':', '#', '?'])
    {
        return None;
    }
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
    let hash = Sha256::digest(bytes);
    let id: String = hash
        .iter()
        .take(16)
        .flat_map(|byte| {
            [
                char::from(b'a' + (byte >> 4)),
                char::from(b'a' + (byte & 15)),
            ]
        })
        .collect();
    Some(format!("chrome-extension://{id}/{popup}"))
}
