//! Browser-extension groundwork: acquire and inventory extension
//! packages from the Firefox (AMO) / Chrome web stores.
//!
//! The runtime that actually *executes* extensions is `WebKit`'s upstream
//! `WebExtension` support (Safari's engine-level implementation, being
//! exposed through the GTK/WPE `GLib` API — manifest handling shipped in
//! 2.52, the controller classes follow in later releases). Until the
//! controller lands, this module covers everything up to activation:
//!
//!  * download an `.xpi` / `.crx` from a store URL,
//!  * strip the CRX header when present (both are zip archives),
//!  * unpack into the local extension directory,
//!  * read `manifest.json` into an [`ExtensionInfo`] inventory record
//!    (name, version, permissions — what the manager UI and the
//!    permission prompt need).
//!
//! Installed extensions stay inert; activation is the single missing
//! piece and plugs in against the upstream API when it ships.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

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
const MAX_UNPACKED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 4_096;
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

impl From<std::io::Error> for ExtensionError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Root directory for unpacked extensions.
pub fn extensions_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("qos-browser")
        .join("extensions")
}

/// Download and install an extension package from a web-store URL
/// (AMO serves `.xpi`, the Chrome store `.crx`).
///
/// # Errors
///
/// Returns [`ExtensionError::Download`] if the URL can't be fetched or
/// responds with an error status, or any error [`install_from_bytes`] can
/// return for the downloaded package.
pub fn install_from_url(url: &str) -> Result<ExtensionInfo, ExtensionError> {
    let parsed =
        url::Url::parse(url).map_err(|error| ExtensionError::Download(error.to_string()))?;
    if parsed.scheme() != "https" || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ExtensionError::Download(
            "extension packages require an HTTPS URL without embedded credentials".into(),
        ));
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|error| ExtensionError::Download(error.to_string()))?;
    let response = client
        .get(parsed)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| ExtensionError::Download(error.to_string()))?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_ARCHIVE_BYTES as u64)
    {
        return Err(ExtensionError::Download(
            "extension package is too large".into(),
        ));
    }
    let mut bytes = Vec::new();
    response
        .take((MAX_ARCHIVE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| ExtensionError::Download(error.to_string()))?;
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(ExtensionError::Download(
            "extension package is too large".into(),
        ));
    }
    install_from_bytes(&bytes, &extensions_dir())
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
    let mut zip = zip::ZipArchive::new(Cursor::new(zip_bytes))
        .map_err(|e| ExtensionError::Archive(e.to_string()))?;
    if zip.len() > MAX_ARCHIVE_ENTRIES {
        return Err(ExtensionError::Archive(
            "extension package has too many files".into(),
        ));
    }
    let mut unpacked = 0u64;
    for index in 0..zip.len() {
        let file = zip
            .by_index(index)
            .map_err(|error| ExtensionError::Archive(error.to_string()))?;
        unpacked = unpacked.saturating_add(file.size());
        if unpacked > MAX_UNPACKED_BYTES {
            return Err(ExtensionError::Archive(
                "extension package expands beyond the safety limit".into(),
            ));
        }
    }

    // Manifest first, so a broken package never touches the disk.
    let manifest_text = {
        let mut file = zip
            .by_name("manifest.json")
            .map_err(|_| ExtensionError::Manifest("no manifest.json in package".into()))?;
        if file.size() > MAX_MANIFEST_BYTES {
            return Err(ExtensionError::Manifest(
                "manifest.json is too large".into(),
            ));
        }
        let mut text = String::new();
        file.read_to_string(&mut text)
            .map_err(|e| ExtensionError::Manifest(e.to_string()))?;
        text
    };
    let mut info = parse_manifest(&manifest_text)?;

    let dest = root.join(&info.id);
    if dest.exists() {
        std::fs::remove_dir_all(&dest)?;
    }
    std::fs::create_dir_all(&dest)?;
    zip.extract(&dest)
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
    use std::io::Write;

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

    #[test]
    fn installs_xpi_and_reads_manifest() {
        let root = tempfile::tempdir().unwrap();
        let info = install_from_bytes(&fixture_zip(), root.path()).unwrap();
        assert_eq!(info.id, "addon@darkreader.org");
        assert_eq!(info.name, "Dark Reader");
        assert_eq!(info.manifest_version, 2);
        assert!(info.permissions.contains(&"storage".to_string()));
        assert!(info.path.join("background.js").exists());
    }

    #[test]
    fn installs_crx3_by_stripping_header() {
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

    #[test]
    fn lists_installed_extensions() {
        let root = tempfile::tempdir().unwrap();
        install_from_bytes(&fixture_zip(), root.path()).unwrap();
        let listed = list_in(root.path());
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Dark Reader");
    }

    #[test]
    fn rejects_zip_without_manifest() {
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

    #[test]
    fn slug_fallback_for_manifests_without_gecko_id() {
        let manifest = r#"{"manifest_version": 3, "name": "My Cool Tool!", "version": "1.0"}"#;
        let info = parse_manifest(manifest).unwrap();
        assert_eq!(info.id, "my-cool-tool");
        assert_eq!(info.manifest_version, 3);
    }

    #[test]
    fn manifest_identifier_cannot_escape_extension_root() {
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

    #[test]
    fn non_ascii_name_gets_a_nonempty_safe_fallback_identifier() {
        let manifest = r#"{"manifest_version": 3, "name": "🌙", "version": "1.0"}"#;
        let info = parse_manifest(manifest).unwrap();
        assert!(info.id.starts_with("extension-"));
        assert!(info.id.len() > "extension-".len());
        assert_eq!(
            resource_id(&info.id),
            format!("browser.extension:{}", info.id)
        );
    }

    #[test]
    fn inventory_skips_manifests_larger_than_the_install_limit() {
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

    #[test]
    fn manifest_fields_and_permission_lists_are_bounded() {
        let oversized_name = "a".repeat(MAX_EXTENSION_NAME_BYTES + 1);
        let manifest =
            format!(r#"{{"manifest_version": 3, "name": "{oversized_name}", "version": "1.0"}}"#);
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

    #[test]
    fn oversized_archive_is_rejected_before_parsing() {
        let bytes = vec![0; MAX_ARCHIVE_BYTES + 1];
        assert!(matches!(
            install_from_bytes(&bytes, std::path::Path::new("/unused")),
            Err(ExtensionError::Archive(_))
        ));
    }
}
