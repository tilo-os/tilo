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
    let bytes = reqwest::blocking::get(url)
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(reqwest::blocking::Response::bytes)
        .map_err(|e| ExtensionError::Download(e.to_string()))?;
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
    let zip_bytes = strip_crx_header(archive)?;
    let mut zip = zip::ZipArchive::new(Cursor::new(zip_bytes))
        .map_err(|e| ExtensionError::Archive(e.to_string()))?;

    // Manifest first, so a broken package never touches the disk.
    let manifest_text = {
        let mut file = zip
            .by_name("manifest.json")
            .map_err(|_| ExtensionError::Manifest("no manifest.json in package".into()))?;
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

fn list_in(root: &Path) -> Vec<ExtensionInfo> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found: Vec<ExtensionInfo> = entries
        .flatten()
        .filter_map(|entry| {
            let manifest = entry.path().join("manifest.json");
            let text = std::fs::read_to_string(manifest).ok()?;
            let mut info = parse_manifest(&text).ok()?;
            info.path = entry.path();
            Some(info)
        })
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
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
    let name = value
        .get("name")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| ExtensionError::Manifest("manifest has no name".into()))?
        .to_string();
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| ExtensionError::Manifest("manifest has no version".into()))?
        .to_string();
    let manifest_version = value
        .get("manifest_version")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(2);
    let description = value
        .get("description")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let permissions = value
        .get("permissions")
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|p| p.as_str().map(str::to_string))
                .collect()
        })
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
    let id = gecko_id.map_or_else(|| slug(&name), str::to_string);

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
    out.trim_matches('-').to_string()
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
}
