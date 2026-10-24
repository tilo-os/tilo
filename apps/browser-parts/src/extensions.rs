//! Unpacked extension inventory and explicit manifest-scoped startup grants.
//! CEF's Chrome runtime loads approved MV3 packages on the next browser launch.

use std::path::{Path, PathBuf};

use bounded_zip::{BoundedArchive, ZipError, ZipLimits};
use ui::webview::ExtensionPermissionSet;

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

/// Ad blockers carry their filter lists: Adblock Plus is 78 MB packed and
/// 347 MB unpacked.
const MAX_ARCHIVE_BYTES: usize = 256 * 1024 * 1024;
/// Entry count and declared unpacked size are checked before anything is
/// inflated or written to disk.
const ARCHIVE_LIMITS: ZipLimits = ZipLimits {
    max_parts: 4_096,
    max_part_bytes: 256 * 1024 * 1024,
    max_total_bytes: 1024 * 1024 * 1024,
};
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
/// Largest `_locales/<locale>/messages.json` read for an extension's names.
const MAX_MESSAGES_BYTES: u64 = 1024 * 1024;
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
/// Plain zips (`.xpi`, `.zip`) are accepted as unsigned packages. With
/// `expected_id`, a package signed for any other identifier is refused.
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
    let mut info = parse_manifest_with(&manifest_text, |file| {
        zip.read_text(file, MAX_MESSAGES_BYTES).ok()
    })?;
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
    // Bounded on the way to disk: the declared sizes checked above are the
    // package's own claim, and a header that lies small must not fill the
    // extensions directory through an unbounded inflate.
    zip.extract_to(&dest)
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
    // The profile's own files (approvals, pins, update claims) are no
    // extensions and count for none of the bound.
    let mut found: Vec<ExtensionInfo> = entries
        .flatten()
        .filter(|entry| {
            entry
                .file_type()
                .is_ok_and(|kind| kind.is_dir() || kind.is_symlink())
        })
        .take(MAX_INSTALLED_EXTENSIONS)
        .filter_map(|entry| {
            let manifest = entry.path().join("manifest.json");
            let text = read_manifest(&manifest).ok()?;
            let root = entry.path();
            let mut info =
                parse_manifest_with(&text, |file| read_bounded(&root.join(file))).ok()?;
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

/// A file of at most [`MAX_MESSAGES_BYTES`], as text.
pub(crate) fn read_bounded(path: &Path) -> Option<String> {
    let length = std::fs::metadata(path).ok()?.len();
    (length <= MAX_MESSAGES_BYTES)
        .then(|| std::fs::read_to_string(path).ok())
        .flatten()
}

/// An extension's messages in its default locale, keyed in lower case:
/// Chromium resolves `__MSG_name__` without regard to case.
pub type Messages = std::collections::HashMap<String, String>;

/// Where an extension keeps its default locale's messages, relative to its
/// root: `_locales/<default_locale>/messages.json`. `None` without a
/// default locale, or with one that is no plain locale name.
pub fn messages_file(manifest: &serde_json::Value) -> Option<String> {
    let locale = manifest
        .get("default_locale")
        .and_then(serde_json::Value::as_str)
        .filter(|locale| {
            !locale.is_empty()
                && locale.len() <= 16
                && locale
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || "_-".contains(character))
        })?;
    Some(format!("_locales/{locale}/messages.json"))
}

/// The messages a `messages.json` holds, ignoring any it cannot read.
pub fn messages(text: &str) -> Messages {
    let Ok(serde_json::Value::Object(entries)) = serde_json::from_str(text) else {
        return Messages::new();
    };
    entries
        .into_iter()
        .filter_map(|(key, entry)| {
            let message = entry.get("message")?.as_str()?;
            Some((key.to_lowercase(), message.to_owned()))
        })
        .collect()
}

/// `text` with each `__MSG_name__` replaced by its message, as Chromium
/// shows an extension's name and titles. A name it has no message for stays
/// as written rather than going blank.
pub fn localize(text: &str, messages: &Messages) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((before, after)) = rest.split_once("__MSG_") {
        let Some((name, tail)) = after.split_once("__") else {
            break;
        };
        out.push_str(before);
        if let Some(message) = messages.get(&name.to_lowercase()) {
            out.push_str(message);
        } else {
            out.push_str("__MSG_");
            out.push_str(name);
            out.push_str("__");
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
fn parse_manifest(text: &str) -> Result<ExtensionInfo, ExtensionError> {
    parse_manifest_with(text, |_| None)
}

/// The manifest's inventory record, its names in the extension's default
/// locale: `read` gives the text of a file of the package by its path
/// within it.
fn parse_manifest_with(
    text: &str,
    read: impl FnOnce(&str) -> Option<String>,
) -> Result<ExtensionInfo, ExtensionError> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| ExtensionError::Manifest(e.to_string()))?;
    let messages = messages_file(&value)
        .and_then(|file| read(&file))
        .map(|text| messages(&text))
        .unwrap_or_default();
    let name = bounded_manifest_string(
        &localize(
            value
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| ExtensionError::Manifest("manifest has no name".into()))?,
            &messages,
        ),
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
        &localize(
            value
                .get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default(),
            &messages,
        ),
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

    /// Stage `archive` as a picked file and install it, as the browser does.
    fn install_from_bytes(archive: &[u8], root: &Path) -> Result<ExtensionInfo, ExtensionError> {
        stage_file(archive.to_vec())?.install(root)
    }

    /// Stage `archive` as the Web Store item `expected_id` and install it, as
    /// the browser does.
    fn install_verified_crx(
        archive: &[u8],
        root: &Path,
        expected_id: &str,
    ) -> Result<ExtensionInfo, ExtensionError> {
        stage_package(archive.to_vec(), expected_id)?.install(root)
    }

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
    /// A package named in its locales, as most Web Store extensions are.
    fn localized_zip(name: &str) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default();
            zip.start_file("manifest.json", opts).unwrap();
            zip.write_all(
                br#"{"manifest_version":3,"name":"__MSG_extName__","version":"1.0",
                "description":"__MSG_extDesc__ for pages","default_locale":"en",
                "action":{"default_title":"__MSG_EXTNAME__"}}"#,
            )
            .unwrap();
            zip.start_file("_locales/en/messages.json", opts).unwrap();
            zip.write_all(
                format!(
                    r#"{{"extName":{{"message":"{name}"}},"extDesc":{{"message":"Blocks ads"}}}}"#
                )
                .as_bytes(),
            )
            .unwrap();
            zip.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn an_update_that_asks_for_more_access_waits_for_the_person() {
        let lines = |list: &[&str]| {
            list.iter()
                .map(|line| (*line).to_owned())
                .collect::<Vec<_>>()
        };
        let allowed = lines(&[
            "Read and change all your data on all websites",
            "Block content on any page",
        ]);
        assert!(!asks_for_more(
            &allowed,
            &lines(&["Block content on any page"])
        ));
        assert!(!asks_for_more(&allowed, &allowed));
        assert!(asks_for_more(
            &allowed,
            &lines(&["Block content on any page", "Read your browsing history"])
        ));
    }

    #[test]
    fn an_offer_says_what_an_extension_may_do_as_chrome_does() {
        // uBlock Origin Lite's manifest, as the Web Store serves it.
        let ublock = r#"{"manifest_version":3,"name":"uBlock Origin Lite","version":"1",
            "permissions":["activeTab","alarms","declarativeNetRequest","offscreen","scripting",
            "storage","unlimitedStorage","userScripts"],"host_permissions":["<all_urls>"]}"#;
        assert_eq!(
            summarize_manifest(ublock).unwrap(),
            [
                "Read and change all your data on all websites",
                "Block content on any page"
            ]
        );
        // A tab-reading ad blocker limited to two sites by its content scripts.
        let narrow = r#"{"manifest_version":3,"name":"x","version":"1",
            "permissions":["tabs","storage","declarativeNetRequest"],
            "content_scripts":[{"matches":["*://*.example.org/*","https://news.example/*"]}]}"#;
        assert_eq!(
            summarize_manifest(narrow).unwrap(),
            [
                "Read and change your data on 2 websites",
                "Block content on any page",
                "Read your browsing history"
            ]
        );
        assert_eq!(
            summarize_manifest(r#"{"name":"x","version":"1","permissions":["storage"]}"#).unwrap(),
            ["It needs no special access"]
        );
    }

    #[test]
    fn an_extension_is_named_in_its_own_locale() {
        let staged = stage_file(localized_zip("uBlock Origin Lite")).unwrap();
        assert_eq!(staged.info.name, "uBlock Origin Lite", "the offer names it");
        assert_eq!(staged.info.description, "Blocks ads for pages");
        let root = tempfile::tempdir().unwrap();
        let first = staged.install(root.path()).unwrap();
        // Two such packages are two extensions, not one written over the other.
        let second = stage_file(localized_zip("Dark Reader"))
            .unwrap()
            .install(root.path())
            .unwrap();
        assert_ne!(first.id, second.id);
        let listed = list_in(root.path());
        assert_eq!(
            listed
                .iter()
                .map(|info| info.name.as_str())
                .collect::<Vec<_>>(),
            ["Dark Reader", "uBlock Origin Lite"]
        );
        let installed = crate::extension_toolbar::Installed::read(&listed[1], true, false).unwrap();
        assert_eq!(
            installed.action.unwrap().title.as_deref(),
            Some("uBlock Origin Lite"),
            "message names are matched as Chromium does, whatever their case"
        );
        // A name with no message keeps what it says.
        assert_eq!(
            localize("__MSG_missing__", &Messages::new()),
            "__MSG_missing__"
        );
    }

    #[test]
    fn extension_archives_install_list_and_reject_invalid_packages() {
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

        {
            let root = tempfile::tempdir().unwrap();
            install_from_bytes(&fixture_zip(), root.path()).unwrap();
            let listed = list_in(root.path());
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].name, "Dark Reader");
        }

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

        {
            let bytes = vec![0; MAX_ARCHIVE_BYTES + 1];
            assert!(matches!(
                install_from_bytes(&bytes, std::path::Path::new("/unused")),
                Err(ExtensionError::Archive(_))
            ));
        }
    }

    #[test]
    fn extension_identifiers_and_manifests_are_safe_and_bounded() {
        {
            let manifest = r#"{"manifest_version": 3, "name": "My Cool Tool!", "version": "1.0"}"#;
            let info = parse_manifest(manifest).unwrap();
            assert_eq!(info.id, "my-cool-tool");
            assert_eq!(info.manifest_version, 3);
        }

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
    fn one_browser_a_day_checks_for_updates() {
        let root = tempfile::tempdir().unwrap();
        let day = 24 * 60 * 60;
        let monday = 20_000 * day + 60;
        assert!(claim_update_check(root.path(), monday), "the first to ask");
        assert!(
            !claim_update_check(root.path(), monday + 3_600),
            "another Browser, later that day"
        );
        assert!(
            claim_update_check(root.path(), monday + day),
            "the next day"
        );
        assert_eq!(
            std::fs::read_dir(root.path()).unwrap().count(),
            1,
            "only the day's claim is kept"
        );
    }

    #[test]
    fn the_profiles_own_files_count_for_no_extension() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..=MAX_INSTALLED_EXTENSIONS {
            std::fs::write(
                root.path().join(format!(".tilo-approval-{index}.json")),
                "x",
            )
            .unwrap();
        }
        let path = root.path().join("addon");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(
            path.join("manifest.json"),
            r#"{"name":"fixture","version":"1","manifest_version":3}"#,
        )
        .unwrap();
        assert_eq!(list_in(root.path()).len(), 1);
    }

    #[test]
    fn approval_is_explicit_and_invalidated_by_manifest_changes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("addon");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("manifest.json"),r#"{"name":"fixture","version":"1","manifest_version":3,"permissions":["storage"],"host_permissions":["https://example.org/*"]}"#).unwrap();
        let extension = list_in(root.path()).pop().unwrap();
        assert!(!is_approved(&extension));
        assert_eq!(
            permission_summary(&extension).unwrap(),
            ["Read and change your data on example.org"]
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

    #[test]
    fn what_an_extension_was_granted_later_stays_with_it_until_it_goes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("addon");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(
            path.join("manifest.json"),
            r#"{"name":"fixture","version":"1","manifest_version":3,"optional_permissions":["tabs"]}"#,
        )
        .unwrap();
        let extension = list_in(root.path()).pop().unwrap();
        assert!(granted(&extension).is_empty());
        let tabs = ExtensionPermissionSet {
            permissions: vec!["tabs".into()],
            origins: vec!["https://*.example.org/*".into()],
        };
        set_granted(&extension, &tabs).unwrap();
        assert_eq!(granted(&extension), tabs);
        // A record is no extension.
        assert_eq!(list_in(root.path()).len(), 1);
        // An update keeps it: the record is the extension's place's.
        std::fs::write(
            path.join("manifest.json"),
            r#"{"name":"fixture","version":"2","manifest_version":3,"optional_permissions":["tabs"]}"#,
        )
        .unwrap();
        assert_eq!(granted(&list_in(root.path()).pop().unwrap()), tabs);
        // Given up, it leaves nothing behind.
        set_granted(&extension, &ExtensionPermissionSet::default()).unwrap();
        assert!(granted(&extension).is_empty());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        set_granted(&extension, &tabs).unwrap();
        uninstall(&extension, root.path()).unwrap();
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_request_for_more_access_is_put_as_its_addition_was() {
        let asked = |permissions: &[&str], origins: &[&str]| {
            asked_summary(&ExtensionPermissionSet {
                permissions: permissions.iter().map(|&entry| entry.to_owned()).collect(),
                origins: origins.iter().map(|&entry| entry.to_owned()).collect(),
            })
        };
        assert_eq!(
            asked(&["tabs"], &["http://127.0.0.1/*"]),
            [
                "Read and change your data on 127.0.0.1",
                "Read your browsing history"
            ]
        );
        assert_eq!(
            asked(&[], &["<all_urls>"]),
            ["Read and change all your data on all websites"]
        );
        // Nothing Chrome asks about: granted without a question.
        assert!(asked(&["storage", "alarms"], &[]).is_empty());
    }
}

fn approval_path(extension: &ExtensionInfo) -> Option<PathBuf> {
    approval_path_of(&extension.path)
}
fn approval_path_of(root: &Path) -> Option<PathBuf> {
    let path = root.canonicalize().ok()?;
    let key = blake3::hash(path.to_str()?.as_bytes()).to_hex();
    Some(path.parent()?.join(format!(".tilo-approval-{key}.json")))
}
fn manifest_digest(extension: &ExtensionInfo) -> Result<String, ExtensionError> {
    let text = read_manifest(&extension.path.join("manifest.json"))?;
    Ok(digest_of(&text))
}
fn digest_of(manifest: &str) -> String {
    blake3::hash(manifest.as_bytes()).to_hex().to_string()
}
pub fn is_approved(extension: &ExtensionInfo) -> bool {
    extension.manifest_version == 3
        && approval_path(extension)
            .and_then(|path| std::fs::read_to_string(path).ok())
            .zip(manifest_digest(extension).ok())
            .is_some_and(|(approved, current)| approved == current)
}
/// Update startup approval for this installed extension.
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

/// What updating an extension from the Web Store did.
#[derive(Debug)]
pub enum Updated {
    /// The newer version is in place, on if it was.
    Installed(ExtensionInfo),
    /// The newer version asks for more access than the person allowed: it
    /// is off until they turn it on, as Chrome does.
    AwaitsApproval(ExtensionInfo),
}

/// Bring a Web Store extension up to date: fetch a newer version, verify it
/// is the same extension, and unpack it over the old one, under `root`.
/// `None` when it is current or not from the store.
///
/// # Errors
/// Returns an error when the store cannot be reached or the package does
/// not verify or unpack.
pub fn update_from_store(
    extension: &ExtensionInfo,
    chromium_version: &str,
    root: &Path,
) -> Result<Option<Updated>, ExtensionError> {
    if !crate::crx::is_extension_id(&extension.id) {
        return Ok(None);
    }
    let Some(bytes) =
        crate::web_store::update(&extension.id, &extension.version, chromium_version)?
    else {
        return Ok(None);
    };
    let staged = stage_package(bytes, &extension.id)?;
    if staged.info.version == extension.version {
        return Ok(None);
    }
    let was_on = is_approved(extension);
    let allowed = permission_summary(extension).unwrap_or_default();
    let info = staged.install(root)?;
    let asks_more = asks_for_more(&allowed, &staged.permissions);
    if was_on && asks_more {
        return Ok(Some(Updated::AwaitsApproval(info)));
    }
    if was_on {
        set_approved(&info, true)?;
    }
    Ok(Some(Updated::Installed(info)))
}

/// The name of the file by which a Browser claims a day's update check.
const UPDATE_CLAIM: &str = ".tilo-update-day-";

/// Claim the update check of day `unix_seconds` falls in for the
/// extensions in `root`. Every Browser on the profile asks as it starts; the
/// first each day is given it, so the store is asked once a day and no two
/// Browsers unpack the same update over each other.
pub fn claim_update_check(root: &Path, unix_seconds: u64) -> bool {
    let claim = format!("{UPDATE_CLAIM}{}", unix_seconds / (24 * 60 * 60));
    if std::fs::create_dir_all(root).is_err()
        || std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(root.join(&claim))
            .is_err()
    {
        return false;
    }
    // Earlier days' claims are done with.
    for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let name = entry.file_name();
        if name
            .to_str()
            .is_some_and(|name| name.starts_with(UPDATE_CLAIM) && name != claim)
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    true
}

/// Whether an update's access, `now`, goes beyond what was `allowed`: any
/// line of it the person was not shown when they added it.
fn asks_for_more(allowed: &[String], now: &[String]) -> bool {
    now.iter().any(|line| !allowed.contains(line))
}

/// The Browser's own extension, which turns the others on and off as
/// Chrome's extensions page does: turning one off keeps what it stored, its
/// settings and logins, where Chromium's `DevTools` can only uninstall it.
const MANAGER_FILES: [(&str, &str); 4] = [
    (
        "manifest.json",
        include_str!("../assets/extension-manager/manifest.json"),
    ),
    (
        "manage.html",
        include_str!("../assets/extension-manager/manage.html"),
    ),
    (
        "manage.js",
        include_str!("../assets/extension-manager/manage.js"),
    ),
    (
        "worker.js",
        include_str!("../assets/extension-manager/worker.js"),
    ),
];

/// The token the manager's worker reports to the engine with, beside it
/// (`worker.js`): what extensions' rules acted on in the Browser's tabs.
const MANAGER_TOKEN_FILE: &str = "tilo-bridge-token.js";

/// Where the manager extension is, written beside the profile's extensions
/// and never listed among them; `None` when it cannot be written.
pub fn manager_dir() -> Option<PathBuf> {
    let dir = crate::storage::data_dir()?.join("extension-manager");
    write_manager(&dir)?;
    dir.canonicalize().ok()
}

/// Write the manager's files into `dir`, with its worker's token, which
/// lasts from one start to the next.
fn write_manager(dir: &Path) -> Option<()> {
    std::fs::create_dir_all(dir).ok()?;
    for (name, text) in MANAGER_FILES {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(text) {
            std::fs::write(&path, text).ok()?;
        }
    }
    if manager_token(dir).is_none() {
        let token = new_token()?;
        std::fs::write(
            dir.join(MANAGER_TOKEN_FILE),
            format!("globalThis.__tiloBridgeToken = \"{token}\";\n"),
        )
        .ok()?;
    }
    Some(())
}

/// The token the worker of the manager at `dir` reports to the engine with.
pub fn manager_token(dir: &Path) -> Option<String> {
    tabs_token(&std::fs::read_to_string(dir.join(MANAGER_TOKEN_FILE)).ok()?)
}

/// The identifier Chromium gives the manager unpacked at `dir`.
pub fn manager_id(dir: &Path) -> Option<String> {
    Some(crate::crx::extension_id(dir.to_str()?.as_bytes()))
}

/// What the engine starts with: the manager, then the extensions turned on.
pub fn startup_paths() -> Vec<PathBuf> {
    let approved = approved_paths();
    // The engine reads an extension's worker as it loads it.
    for path in &approved {
        prepare_tabs(path);
    }
    manager_dir().into_iter().chain(approved).collect()
}

/// The script that gives an extension the tab APIs Chromium does not give it
/// here: the engine answers them (`extension_tabs.h` in the engine host).
const TABS_SCRIPT: &str = include_str!("../../browser/chromium/src/extension_tabs.js");
const TABS_FILE: &str = "tilo-extension-tabs.js";
/// The worker earlier Browsers put in place of the extension's own, which
/// named it on its first line after [`WORKER_MARK`].
const WORKER_FILE: &str = "tilo-worker.js";
const WORKER_MARK: &str = "// The extension's own worker: ";
/// How much of a worker's start is read to tell it runs the tab script.
const WORKER_HEAD_BYTES: u64 = 64 * 1024;

/// Give the extension at `root` the tab APIs. Its own service worker runs the
/// tab script first, and the tab script carries a token by which the engine
/// knows the worker as this extension's (no other extension or page can read
/// it). The worker keeps its address: extensions read it (`location`,
/// `getManifest().background`) and resolve their own scripts against it.
/// Its pages and worker may reach the engine whatever its security policy
/// lets them connect to. Returns the token, or `None` for an extension
/// without a worker: the engine knows its pages by their address.
pub fn prepare_tabs(root: &Path) -> Option<String> {
    let manifest_path = root.join("manifest.json");
    let shipped = read_manifest(&manifest_path).ok()?;
    let mut manifest: serde_json::Value = serde_json::from_str(&shipped).ok()?;
    let admitted = admit_bridge(&mut manifest);
    admit_bridge_in_pages(root);
    let worker = give_worker_tabs(root, &mut manifest);
    let restored = worker.as_ref().is_some_and(|(_, restored)| *restored);
    if admitted || restored {
        rewrite_manifest(root, &shipped, &manifest)?;
    }
    if restored {
        let _ = std::fs::remove_file(root.join(WORKER_FILE));
    }
    worker.map(|(token, _)| token)
}

/// Make the extension's worker run the tab script first: the token it
/// carries, and whether the manifest names the extension's own worker again
/// in place of one an earlier Browser put there.
fn give_worker_tabs(root: &Path, manifest: &mut serde_json::Value) -> Option<(String, bool)> {
    let background = manifest.get_mut("background")?.as_object_mut()?;
    let worker = background.get("service_worker")?.as_str()?.to_owned();
    let module = background.get("type").and_then(serde_json::Value::as_str) == Some("module");
    let own = if worker == WORKER_FILE {
        std::fs::read_to_string(root.join(WORKER_FILE))
            .ok()?
            .lines()
            .next()?
            .strip_prefix(WORKER_MARK)?
            .to_owned()
    } else {
        worker.trim_start_matches('/').to_owned()
    };
    let safe = |path: &str| {
        !path.is_empty()
            && !path.contains(['\\', '\'', '"', '\n', '\r', ':', '?', '#'])
            && path.split('/').all(|part| !part.is_empty() && part != "..")
    };
    if !safe(&own) || !root.join(&own).is_file() {
        return None;
    }
    let token = std::fs::read_to_string(root.join(TABS_FILE))
        .ok()
        .and_then(|text| tabs_token(&text))
        .or_else(new_token)?;
    let script = format!("globalThis.__tiloBridgeToken = \"{token}\";\n{TABS_SCRIPT}");
    if std::fs::read_to_string(root.join(TABS_FILE))
        .ok()
        .as_deref()
        != Some(script.as_str())
    {
        std::fs::write(root.join(TABS_FILE), &script).ok()?;
    }
    run_tabs_first(&root.join(&own), module)?;
    let restored = worker == WORKER_FILE;
    if restored {
        // Back to the extension's own worker, now that it runs the script.
        background.insert("service_worker".into(), own.into());
    }
    Some((token, restored))
}

/// Where the tab script asks the engine (`kExtensionBridge` in the host).
const BRIDGE_ORIGIN: &str = "https://extension-bridge.tilo.invalid";

/// Let the extension's pages and worker reach the engine. Password managers
/// and others list the only servers their pages may connect to
/// (`connect-src`, or `default-src` without one), and Chromium refuses the
/// tab script's requests to any other. Returns whether the policy changed.
fn admit_bridge(manifest: &mut serde_json::Value) -> bool {
    let policy = match manifest.get_mut("content_security_policy") {
        // Manifest V2 has one policy for every page.
        Some(serde_json::Value::String(policy)) => policy,
        Some(serde_json::Value::Object(policies)) => match policies.get_mut("extension_pages") {
            Some(serde_json::Value::String(policy)) => policy,
            _ => return false,
        },
        _ => return false,
    };
    match policy_with_bridge(policy) {
        Some(admitted) => {
            *policy = admitted;
            true
        }
        None => false,
    }
}

/// `policy` with the bridge among the places it may connect to; `None` when
/// it already may.
fn policy_with_bridge(policy: &str) -> Option<String> {
    let mut directives: Vec<String> = policy
        .split(';')
        .map(str::trim)
        .filter(|directive| !directive.is_empty())
        .map(str::to_owned)
        .collect();
    let named = |name: &str| {
        directives.iter().position(|directive| {
            directive
                .split_ascii_whitespace()
                .next()
                .is_some_and(|first| first.eq_ignore_ascii_case(name))
        })
    };
    // The first of a directive is the one that counts.
    let connect = named("connect-src");
    let governing = connect.or_else(|| named("default-src"))?;
    let mut sources: Vec<&str> = directives
        .get(governing)?
        .split_ascii_whitespace()
        .skip(1)
        .filter(|source| !source.eq_ignore_ascii_case("'none'"))
        .collect();
    let covered = |source: &&str| {
        let source = source.trim_end_matches('/');
        source == "*"
            || source.eq_ignore_ascii_case("https:")
            || source.eq_ignore_ascii_case(BRIDGE_ORIGIN)
    };
    if sources.iter().any(covered) {
        return None;
    }
    sources.push(BRIDGE_ORIGIN);
    let directive = format!("connect-src {}", sources.join(" "));
    match connect.and_then(|index| directives.get_mut(index)) {
        Some(slot) => *slot = directive,
        None => directives.push(directive),
    }
    Some(directives.join("; "))
}

/// Marks an extension whose pages' own policies admit the bridge.
const PAGES_FILE: &str = "tilo-extension-pages";
/// The most files of an extension looked through for its pages.
const MAX_PAGE_SEARCH: usize = 8_192;
const MAX_PAGE_BYTES: u64 = 2 * 1024 * 1024;

/// Let the extension's pages that declare a policy of their own in a
/// `<meta>` tag (as the popups of some password managers do) reach the
/// engine too. Done once per unpacked copy: an update unpacks a new one.
fn admit_bridge_in_pages(root: &Path) {
    if root.join(PAGES_FILE).exists() {
        return;
    }
    let mut pending = vec![root.to_path_buf()];
    let mut seen = 0;
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_PAGE_SEARCH {
                return;
            }
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let page = path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("html") || extension.eq_ignore_ascii_case("htm")
                });
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file()
                && page
                && entry
                    .metadata()
                    .is_ok_and(|meta| meta.len() <= MAX_PAGE_BYTES)
                && let Some(admitted) = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|html| page_with_bridge(&html))
            {
                let _ = std::fs::write(&path, admitted);
            }
        }
    }
    let _ = std::fs::write(root.join(PAGES_FILE), "1");
}

/// `html` with the bridge admitted by each policy it declares in a `<meta>`;
/// `None` when none needed it.
fn page_with_bridge(html: &str) -> Option<String> {
    let mut out = String::with_capacity(html.len() + BRIDGE_ORIGIN.len());
    let mut rest = html;
    let mut changed = false;
    while let Some(start) = find_ascii(rest, "<meta") {
        let (before, tag_on) = rest.split_at_checked(start)?;
        let Some(end) = tag_on.find('>') else {
            break;
        };
        let (tag, after) = tag_on.split_at_checked(end)?;
        out.push_str(before);
        if let Some(admitted) = tag_with_bridge(tag) {
            out.push_str(&admitted);
            changed = true;
        } else {
            out.push_str(tag);
        }
        rest = after;
    }
    out.push_str(rest);
    changed.then_some(out)
}

/// A `<meta http-equiv="Content-Security-Policy" content="…">` tag with
/// the bridge admitted; `None` for any other tag, or one that admits it.
fn tag_with_bridge(tag: &str) -> Option<String> {
    let (_, equiv, _) = attribute(tag, "http-equiv")?;
    if !equiv.eq_ignore_ascii_case("content-security-policy") {
        return None;
    }
    let (before, value, after) = attribute(tag, "content")?;
    let quote = before.chars().last()?;
    let admitted = policy_with_bridge(&unescape(value)?)?;
    let mut out = String::from(before);
    for character in admitted.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '"' if quote == '"' => out.push_str("&quot;"),
            '\'' if quote == '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out.push_str(after);
    Some(out)
}

/// Where `needle` (ASCII) first is in `text`, whatever its case.
fn find_ascii(text: &str, needle: &str) -> Option<usize> {
    text.as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

/// The quoted value of attribute `name` in `tag`, with what comes before
/// it (up to its opening quote) and after it (from its closing quote).
fn attribute<'a>(tag: &'a str, name: &str) -> Option<(&'a str, &'a str, &'a str)> {
    let mut from = 0;
    loop {
        let found = from + find_ascii(tag.get(from..)?, name)?;
        from = found + name.len();
        let (head, tail) = tag.split_at_checked(found)?;
        if !head.ends_with(|c: char| c.is_ascii_whitespace()) {
            continue;
        }
        let Some(value) = tail.get(name.len()..)?.trim_start().strip_prefix('=') else {
            continue;
        };
        let value = value.trim_start();
        let quote = value.chars().next().filter(|c| matches!(c, '"' | '\''))?;
        let inner = value.get(1..)?;
        let (text, _) = inner.split_once(quote)?;
        let (before, on) = tag.split_at_checked(tag.len() - inner.len())?;
        return Some((before, text, on.get(text.len()..)?));
    }
}

/// An attribute value's text; `None` for a character reference this does
/// not know, which is then left alone.
fn unescape(value: &str) -> Option<String> {
    let mut text = String::with_capacity(value.len());
    let mut rest = value;
    while let Some((before, after)) = rest.split_once('&') {
        text.push_str(before);
        let (reference, after) = after.split_once(';')?;
        let character = match reference {
            "amp" => '&',
            "quot" => '"',
            "apos" => '\'',
            "lt" => '<',
            "gt" => '>',
            numeric => {
                let digits = numeric.strip_prefix('#')?;
                let code = match digits.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                    None => digits.parse().ok()?,
                };
                char::from_u32(code)?
            }
        };
        text.push(character);
        rest = after;
    }
    text.push_str(rest);
    Some(text)
}

/// Write the prepared manifest. An extension the person turned on stays on:
/// what changed is the Browser's own doing, not the extension's.
fn rewrite_manifest(root: &Path, before: &str, manifest: &serde_json::Value) -> Option<()> {
    let approval = approval_path_of(root);
    let approved = approval
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .is_some_and(|digest| digest == digest_of(before));
    let after = String::from_utf8(serde_json::to_vec_pretty(manifest).ok()?).ok()?;
    std::fs::write(root.join("manifest.json"), &after).ok()?;
    if approved && let Some(path) = approval {
        std::fs::write(path, digest_of(&after)).ok()?;
    }
    Some(())
}

/// The statement that runs the tab script, first in a worker. It adds no
/// line, so the worker's own lines keep their numbers, and does nothing in a
/// page of the extension that loads the same script (pages have the tab
/// script already).
fn tabs_statement(module: bool) -> String {
    if module {
        format!(";import '/{TABS_FILE}';")
    } else {
        format!(";typeof importScripts=='function'&&importScripts('/{TABS_FILE}');")
    }
}

/// Make the worker at `path` run the tab script before anything of its own,
/// once. Only its start is read when it does already.
fn run_tabs_first(path: &Path, module: bool) -> Option<()> {
    use std::io::Read as _;
    let statement = tabs_statement(module);
    let mut head = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(WORKER_HEAD_BYTES)
        .read_to_end(&mut head)
        .ok()?;
    if runs_first(&head, &statement) {
        return Some(());
    }
    let mut text = std::fs::read(path).ok()?;
    // A start too long for the head (a licence header before its
    // directives) is told from the whole worker.
    if runs_first(&text, &statement) {
        return Some(());
    }
    let at = prologue_end(&text);
    text.splice(at..at, statement.bytes());
    let partial = path.with_file_name(format!(".{TABS_FILE}.partial"));
    std::fs::write(&partial, &text).ok()?;
    std::fs::rename(&partial, path).ok()
}

/// Whether the worker starting with `head` runs `statement` first already.
fn runs_first(head: &[u8], statement: &str) -> bool {
    // Where a directive ended without a semicolon, the statement's own
    // semicolon now ends it, so the statement may start one byte earlier.
    let end = prologue_end(head);
    let start = end.saturating_sub(1);
    head.get(start..(end + statement.len()).min(head.len()))
        .is_some_and(|window| {
            window
                .windows(statement.len())
                .any(|w| w == statement.as_bytes())
        })
}

/// Where a script's own statements start: after a byte-order mark and its
/// directive prologue (`"use strict";`), which only applies while first.
fn prologue_end(text: &[u8]) -> usize {
    let mut end = if text.starts_with(b"\xEF\xBB\xBF") {
        3
    } else {
        0
    };
    loop {
        let at = skip_blank(text, end);
        let Some(&quote) = text.get(at).filter(|byte| matches!(**byte, b'"' | b'\'')) else {
            return end;
        };
        let mut i = at + 1;
        loop {
            match text.get(i) {
                None | Some(b'\n' | b'\r') => return end,
                Some(b'\\') => i += 2,
                Some(&byte) if byte == quote => break,
                Some(_) => i += 1,
            }
        }
        i += 1;
        loop {
            match (text.get(i), text.get(i + 1)) {
                (Some(b' ' | b'\t'), _) => i += 1,
                (Some(b'/'), Some(b'*')) => {
                    let rest = text.get(i + 2..).unwrap_or_default();
                    let Some(close) = rest.windows(2).position(|pair| pair == b"*/") else {
                        return end;
                    };
                    // A comment over lines ends the directive's line.
                    if rest.iter().take(close).any(|b| matches!(b, b'\n' | b'\r')) {
                        break;
                    }
                    i += close + 4;
                }
                _ => break,
            }
        }
        // A directive is a string alone, up to a semicolon or its line's end,
        // which a comment after it on the line may reach.
        end = match (text.get(i), text.get(i + 1)) {
            (Some(b';'), _) => i + 1,
            (None | Some(b'\n' | b'\r'), _) | (Some(b'/'), Some(b'/' | b'*')) => i,
            _ => return end,
        };
    }
}

/// Past whitespace and comments.
fn skip_blank(text: &[u8], mut at: usize) -> usize {
    loop {
        match (text.get(at), text.get(at + 1)) {
            (Some(b' ' | b'\t' | b'\n' | b'\r'), _) => at += 1,
            (Some(b'/'), Some(b'/')) => {
                while !matches!(text.get(at), None | Some(b'\n')) {
                    at += 1;
                }
            }
            (Some(b'/'), Some(b'*')) => {
                let rest = text.get(at + 2..).unwrap_or_default();
                match rest.windows(2).position(|pair| pair == b"*/") {
                    Some(close) => at += close + 4,
                    None => return text.len(),
                }
            }
            _ => return at,
        }
    }
}

/// The worker token the extension's tab script carries.
pub fn tabs_token_of(root: &Path) -> Option<String> {
    tabs_token(&std::fs::read_to_string(root.join(TABS_FILE)).ok()?)
}

fn tabs_token(script: &str) -> Option<String> {
    let token = script
        .lines()
        .next()?
        .strip_prefix("globalThis.__tiloBridgeToken = \"")?
        .strip_suffix("\";")?;
    (token.len() == 48 && token.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| token.to_owned())
}

fn new_token() -> Option<String> {
    use ring::rand::SecureRandom as _;
    let mut bytes = [0u8; 24];
    ring::rand::SystemRandom::new().fill(&mut bytes).ok()?;
    Some(
        bytes
            .iter()
            .fold(String::with_capacity(48), |mut token, byte| {
                use std::fmt::Write as _;
                let _ = write!(token, "{byte:02x}");
                token
            }),
    )
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
    set_granted(extension, &ExtensionPermissionSet::default())?;
    std::fs::remove_dir_all(&path)?;
    Ok(())
}

/// Where what the person granted an extension of what it may ask for is
/// kept: beside it, as its approval is.
fn granted_path(extension: &ExtensionInfo) -> Option<PathBuf> {
    let path = extension.path.canonicalize().ok()?;
    let key = blake3::hash(path.to_str()?.as_bytes()).to_hex();
    Some(path.parent()?.join(format!(".tilo-granted-{key}.json")))
}

/// What the person granted the extension of what its manifest lets it ask
/// for later (`chrome.permissions.request`): kept across runs and updates,
/// and gone with the extension. The engine holds it to what the manifest
/// still offers.
pub fn granted(extension: &ExtensionInfo) -> ExtensionPermissionSet {
    let read = granted_path(extension)
        .and_then(|path| read_bounded(&path))
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
    let list = |key: &str| -> Vec<String> {
        read.as_ref()
            .and_then(|value| value.get(key))
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .filter(|entry| entry.len() <= MAX_EXTENSION_PERMISSION_BYTES)
            .take(MAX_EXTENSION_PERMISSIONS)
            .map(str::to_owned)
            .collect()
    };
    ExtensionPermissionSet {
        permissions: list("permissions"),
        origins: list("origins"),
    }
}

/// Keep what the extension holds now of what it may ask for; nothing
/// leaves no file.
///
/// # Errors
/// Returns an error when the record cannot be written.
pub fn set_granted(
    extension: &ExtensionInfo,
    granted: &ExtensionPermissionSet,
) -> Result<(), ExtensionError> {
    let path = granted_path(extension)
        .ok_or_else(|| ExtensionError::Manifest("Extension path is invalid".into()))?;
    if granted.is_empty() {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        return Ok(());
    }
    let text = serde_json::json!({"permissions": granted.permissions, "origins": granted.origins});
    std::fs::write(path, text.to_string())?;
    Ok(())
}

/// What an extension asking for `asked` asks to do, as the person is asked
/// it: one plain line per kind of access, as when it was added. Empty when
/// none of it is access Chrome would ask about (`storage`, `alarms`), which
/// Chrome grants without asking.
pub fn asked_summary(asked: &ExtensionPermissionSet) -> Vec<String> {
    let manifest = serde_json::json!({
        "permissions": asked.permissions,
        "host_permissions": asked.origins,
    });
    let quiet = crate::CATALOG.t("browser-extension-no-access");
    summarize_manifest(&manifest.to_string())
        .unwrap_or_default()
        .into_iter()
        .filter(|line| *line != quiet)
        .collect()
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
pub fn permission_summary(extension: &ExtensionInfo) -> Result<Vec<String>, ExtensionError> {
    summarize_manifest(&read_manifest(&extension.path.join("manifest.json"))?)
}

/// What the extension may do, as Chrome warns of it when it is added: one
/// plain line per kind of access its manifest asks for, broadest first.
/// Optional permissions are asked for later, by the extension, and are not
/// listed.
///
/// # Errors
/// Returns [`ExtensionError::Manifest`] when the manifest is not JSON.
pub fn summarize_manifest(text: &str) -> Result<Vec<String>, ExtensionError> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|error| ExtensionError::Manifest(error.to_string()))?;
    let strings = |key: &str| -> Vec<String> {
        value
            .get(key)
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let permissions = strings("permissions");
    // Sites: the host permissions, MV2's hosts among its permissions, and
    // where its content scripts run.
    let mut patterns = strings("host_permissions");
    patterns.extend(
        permissions
            .iter()
            .filter(|permission| permission.contains("://") || *permission == "<all_urls>")
            .cloned(),
    );
    if let Some(scripts) = value
        .get("content_scripts")
        .and_then(serde_json::Value::as_array)
    {
        for script in scripts {
            if let Some(matches) = script.get("matches").and_then(serde_json::Value::as_array) {
                patterns.extend(matches.iter().filter_map(|m| m.as_str().map(str::to_owned)));
            }
        }
    }
    let t = |key: &str| crate::CATALOG.t(key);
    let mut lines = Vec::new();
    let mut sites: Vec<String> = Vec::new();
    let mut every_site = permissions.iter().any(|p| p == "debugger" || p == "proxy");
    for pattern in &patterns {
        match site_of(pattern) {
            None => every_site = true,
            Some(site) if !sites.contains(&site) => sites.push(site),
            Some(_) => {}
        }
    }
    if every_site {
        lines.push(t("browser-extension-access-all-sites"));
    } else if let [site] = sites.as_slice() {
        lines.push(crate::CATALOG.t_args(
            "browser-extension-access-site",
            &[("site", ui::i18n::ArgValue::Text(site.clone()))],
        ));
    } else if !sites.is_empty() {
        lines.push(crate::CATALOG.t_args(
            "browser-extension-access-sites",
            &[("count", ui::i18n::ArgValue::Number(sites.len() as f64))],
        ));
    }
    let has = |names: &[&str]| permissions.iter().any(|p| names.contains(&p.as_str()));
    for (names, key) in [
        (
            &[
                "declarativeNetRequest",
                "declarativeNetRequestWithHostAccess",
            ][..],
            "browser-extension-block-content",
        ),
        (&["history"][..], "browser-extension-change-history"),
        (
            &["tabs", "webNavigation"][..],
            "browser-extension-read-history",
        ),
        (&["bookmarks"][..], "browser-extension-bookmarks"),
        (&["downloads"][..], "browser-extension-downloads"),
        (&["clipboardRead"][..], "browser-extension-clipboard-read"),
        (&["clipboardWrite"][..], "browser-extension-clipboard-write"),
        (&["notifications"][..], "browser-extension-notifications"),
        (&["geolocation"][..], "browser-extension-location"),
        (&["management"][..], "browser-extension-management"),
        (&["nativeMessaging"][..], "browser-extension-native"),
        (
            &["privacy", "contentSettings"][..],
            "browser-extension-privacy",
        ),
        (&["topSites"][..], "browser-extension-top-sites"),
        (
            &["desktopCapture", "tabCapture"][..],
            "browser-extension-screen",
        ),
        (&["debugger"][..], "browser-extension-debugger"),
    ] {
        // Reading history is part of changing it.
        if key == "browser-extension-read-history" && has(&["history"]) {
            continue;
        }
        if has(names) {
            lines.push(t(key));
        }
    }
    if lines.is_empty() {
        lines.push(t("browser-extension-no-access"));
    }
    Ok(lines)
}

/// The site a match pattern names (`*://*.example.org/*` is `example.org`),
/// or `None` for one that matches every site.
fn site_of(pattern: &str) -> Option<String> {
    if pattern == "<all_urls>" {
        return None;
    }
    let host = pattern.split_once("://").map_or(pattern, |(_, rest)| rest);
    let host = host.split('/').next().unwrap_or_default();
    let host = host.strip_prefix("*.").unwrap_or(host);
    (!host.is_empty() && host != "*").then(|| host.to_owned())
}

/// A verified package, read but not yet installed: what the person is shown
/// before they agree to add it.
#[derive(Debug, Clone)]
pub struct StagedPackage {
    pub info: ExtensionInfo,
    /// What it may do, one line per kind of access.
    pub permissions: Vec<String>,
    pub bytes: std::sync::Arc<Vec<u8>>,
    /// The identifier its CRX3 signature names; `None` for an unsigned zip.
    pub signed_id: Option<String>,
}

impl StagedPackage {
    /// Unpack the staged package under `root`.
    ///
    /// # Errors
    /// Returns [`ExtensionError::Archive`] if the package fails verification,
    /// [`ExtensionError::Manifest`] if its manifest is invalid, or
    /// [`ExtensionError::Io`] if unpacking it under `root` fails.
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
    let info = parse_manifest_with(&manifest, |file| {
        zip.read_text(file, MAX_MESSAGES_BYTES).ok()
    })?;
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
    let mut info = parse_manifest_with(&manifest, |file| {
        zip.read_text(file, MAX_MESSAGES_BYTES).ok()
    })?;
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

#[cfg(test)]
mod tabs_tests {
    use super::*;

    fn extension(manifest: &str, files: &[(&str, &str)]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("manifest.json"), manifest).unwrap();
        for (name, text) in files {
            let path = root.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        root
    }

    fn read(root: &tempfile::TempDir, name: &str) -> String {
        std::fs::read_to_string(root.path().join(name)).unwrap()
    }

    fn worker_of(root: &tempfile::TempDir) -> serde_json::Value {
        serde_json::from_str::<serde_json::Value>(&read(root, "manifest.json")).unwrap()
            ["background"]["service_worker"]
            .clone()
    }

    const RUNS_TABS: &str =
        ";typeof importScripts=='function'&&importScripts('/tilo-extension-tabs.js');";

    #[test]
    fn the_manager_keeps_the_token_its_worker_reports_with() {
        let root = tempfile::tempdir().expect("temp dir");
        let dir = root.path().join("extension-manager");
        write_manager(&dir).expect("the manager is written");
        let token = manager_token(&dir).expect("its worker is given a token");
        assert_eq!(token.len(), 48);
        let worker = std::fs::read_to_string(dir.join("worker.js")).expect("worker");
        assert!(worker.contains(&format!("importScripts('{MANAGER_TOKEN_FILE}')")));
        // Written again at the next start: the same token.
        write_manager(&dir).expect("the manager is written again");
        assert_eq!(manager_token(&dir), Some(token));
    }

    #[test]
    fn an_extensions_worker_runs_the_tab_script_first_with_its_token() {
        let root = extension(
            r#"{"manifest_version":3,"background":{"service_worker":"bg/main.js"}}"#,
            &[("bg/main.js", "self.mine = 1;\nself.two = 2;\n")],
        );
        let token = prepare_tabs(root.path()).expect("a worker is given the tab APIs");
        // The worker keeps its address: an extension reads it and resolves
        // its own scripts against it.
        assert_eq!(worker_of(&root), "bg/main.js");
        let worker = read(&root, "bg/main.js");
        assert_eq!(
            worker,
            format!("{RUNS_TABS}self.mine = 1;\nself.two = 2;\n")
        );
        let script = read(&root, TABS_FILE);
        assert!(script.starts_with(&format!("globalThis.__tiloBridgeToken = \"{token}\";\n")));
        assert!(script.ends_with(TABS_SCRIPT));
        assert_eq!(tabs_token_of(root.path()).as_deref(), Some(token.as_str()));
        // Again, as at every start: the same token, the script run once.
        assert_eq!(prepare_tabs(root.path()), Some(token));
        assert_eq!(read(&root, "bg/main.js"), worker);
        assert!(!root.path().join(WORKER_FILE).exists());
    }

    #[test]
    fn an_extension_turned_on_stays_on_once_its_worker_is_wrapped() {
        // As the Browser adds one from the store: unpacked into the
        // profile, turned on, then read, which wraps its worker.
        let profile = tempfile::tempdir().unwrap();
        let root = profile.path().join("blocker");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("manifest.json"),
            r#"{"manifest_version":3,"name":"Blocker","version":"1",
            "background":{"service_worker":"worker.js"}}"#,
        )
        .unwrap();
        std::fs::write(root.join("worker.js"), "self.mine = 1;").unwrap();
        let added = list_in(profile.path()).pop().unwrap();
        set_approved(&added, true).unwrap();
        assert!(prepare_tabs(&root).is_some());
        let read = list_in(profile.path()).pop().unwrap();
        assert!(is_approved(&read), "the person turned it on");
        assert_eq!(approved_in(profile.path()), [root.canonicalize().unwrap()]);
        // Read again at the next start: still on.
        assert!(prepare_tabs(&root).is_some());
        assert!(is_approved(&read));

        // One the person never turned on is not turned on by the wrap.
        let other = profile.path().join("other");
        std::fs::create_dir(&other).unwrap();
        std::fs::write(
            other.join("manifest.json"),
            r#"{"manifest_version":3,"name":"Other","version":"1",
            "background":{"service_worker":"worker.js"}}"#,
        )
        .unwrap();
        std::fs::write(other.join("worker.js"), "").unwrap();
        assert!(prepare_tabs(&other).is_some());
        let other = list_in(profile.path())
            .into_iter()
            .find(|info| info.name == "Other")
            .unwrap();
        assert!(!is_approved(&other));
    }

    #[test]
    fn a_module_worker_imports_the_tab_script_first() {
        let root = extension(
            r#"{"background":{"service_worker":"/worker.js","type":"module"}}"#,
            &[("worker.js", "import './lib.js';\nexport {};")],
        );
        assert!(prepare_tabs(root.path()).is_some());
        assert_eq!(worker_of(&root), "/worker.js");
        assert_eq!(
            read(&root, "worker.js"),
            ";import '/tilo-extension-tabs.js';import './lib.js';\nexport {};"
        );
        assert!(prepare_tabs(root.path()).is_some());
        assert_eq!(
            read(&root, "worker.js"),
            ";import '/tilo-extension-tabs.js';import './lib.js';\nexport {};"
        );
    }

    #[test]
    fn a_strict_worker_stays_strict() {
        // A directive applies only while it comes first; a byte-order mark
        // and comments may precede it.
        for (own, prepared) in [
            (
                "\u{feff}/* (c) */\n'use strict';\nx();",
                format!("\u{feff}/* (c) */\n'use strict';{RUNS_TABS}\nx();"),
            ),
            (
                "\"use strict\"\n\"use asm\"; x();",
                format!("\"use strict\"\n\"use asm\";{RUNS_TABS} x();"),
            ),
            (
                "\"use strict\"\nx();",
                format!("\"use strict\"{RUNS_TABS}\nx();"),
            ),
            (
                "'use strict' // strict\nx();",
                format!("'use strict' {RUNS_TABS}// strict\nx();"),
            ),
            (
                "'use strict' /* a\nb */ x();",
                format!("'use strict' {RUNS_TABS}/* a\nb */ x();"),
            ),
            (
                "'use strict' /* c */; x();",
                format!("'use strict' /* c */;{RUNS_TABS} x();"),
            ),
            (
                "'use strict' /* c */ + x();",
                format!("{RUNS_TABS}'use strict' /* c */ + x();"),
            ),
            (
                "// header\n'not'.split(''); \"use strict\";",
                format!("{RUNS_TABS}// header\n'not'.split(''); \"use strict\";"),
            ),
        ] {
            let root = extension(
                r#"{"background":{"service_worker":"w.js"}}"#,
                &[("w.js", own)],
            );
            assert!(prepare_tabs(root.path()).is_some());
            assert_eq!(read(&root, "w.js"), prepared, "{own:?}");
            assert!(prepare_tabs(root.path()).is_some());
            assert_eq!(read(&root, "w.js"), prepared, "again: {own:?}");
        }
    }

    #[test]
    fn a_strict_worker_under_a_long_header_runs_the_tab_script_once() {
        // Licences gathered into one comment, longer than the head read to
        // tell the worker runs the script already.
        let header = format!("/*{}*/\n", "x".repeat(WORKER_HEAD_BYTES as usize));
        let root = extension(
            r#"{"background":{"service_worker":"w.js"}}"#,
            &[("w.js", &format!("{header}'use strict';x();"))],
        );
        let prepared = format!("{header}'use strict';{RUNS_TABS}x();");
        for start in ["first", "next", "after that"] {
            assert!(prepare_tabs(root.path()).is_some());
            assert!(read(&root, "w.js") == prepared, "{start} start");
        }
    }

    #[test]
    fn a_worker_an_earlier_browser_replaced_gets_its_address_back() {
        // Turned on by the person while an earlier Browser had replaced its
        // worker in the manifest.
        let profile = tempfile::tempdir().unwrap();
        let root = profile.path().join("keeper");
        for (name, text) in [
            (
                "manifest.json",
                r#"{"manifest_version":3,"name":"Keeper","version":"1",
                "background":{"service_worker":"tilo-worker.js"}}"#,
            ),
            (
                WORKER_FILE,
                "// The extension's own worker: js/bg.js\nimportScripts('/tilo-extension-tabs.js', '/js/bg.js');\n",
            ),
            ("js/bg.js", "go();"),
        ] {
            std::fs::create_dir_all(root.join(name).parent().unwrap()).unwrap();
            std::fs::write(root.join(name), text).unwrap();
        }
        set_approved(&list_in(profile.path()).pop().unwrap(), true).unwrap();
        let token = prepare_tabs(&root).expect("the worker is given the tab APIs");
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["background"]["service_worker"], "js/bg.js");
        assert_eq!(
            std::fs::read_to_string(root.join("js/bg.js")).unwrap(),
            format!("{RUNS_TABS}go();")
        );
        assert!(!root.join(WORKER_FILE).exists());
        assert!(
            is_approved(&list_in(profile.path()).pop().unwrap()),
            "still on"
        );
        assert_eq!(prepare_tabs(&root), Some(token));
    }

    #[test]
    fn an_extension_without_a_worker_or_with_a_strange_one_is_left_as_it_is() {
        let none = extension(r#"{"action":{"default_popup":"p.html"}}"#, &[]);
        assert_eq!(prepare_tabs(none.path()), None);
        for worker in ["../outside.js", "missing.js", "a'b.js", "https://x/y.js"] {
            let root = extension(
                &format!(r#"{{"background":{{"service_worker":"{worker}"}}}}"#),
                &[],
            );
            assert_eq!(prepare_tabs(root.path()), None, "{worker}");
            assert!(!root.path().join(WORKER_FILE).exists());
        }
    }
}

#[cfg(test)]
mod bridge_policy_tests {
    use super::*;

    #[test]
    fn a_policy_that_lists_where_pages_connect_also_lists_the_bridge() {
        // As 1Password's and LastPass's do.
        assert_eq!(
            policy_with_bridge(
                "default-src 'none'; script-src 'self'; connect-src https://*.vault.example wss://push.vault.example"
            ),
            Some(format!(
                "default-src 'none'; script-src 'self'; connect-src https://*.vault.example wss://push.vault.example {BRIDGE_ORIGIN}"
            ))
        );
        assert_eq!(
            policy_with_bridge("connect-src 'none'"),
            Some(format!("connect-src {BRIDGE_ORIGIN}"))
        );
    }

    #[test]
    fn a_default_that_governs_connections_is_kept_for_them_with_the_bridge() {
        assert_eq!(
            policy_with_bridge("default-src 'self' https://api.example; img-src *;"),
            Some(format!(
                "default-src 'self' https://api.example; img-src *; connect-src 'self' https://api.example {BRIDGE_ORIGIN}"
            ))
        );
    }

    #[test]
    fn a_policy_that_already_lets_pages_reach_the_bridge_is_left_alone() {
        for policy in [
            "script-src 'self'; object-src 'self'",
            "default-src 'self'; connect-src 'self' * data:",
            "connect-src https:",
            &format!("connect-src 'self' {BRIDGE_ORIGIN}"),
        ] {
            assert_eq!(policy_with_bridge(policy), None, "{policy}");
        }
    }

    #[test]
    fn the_extension_pages_policy_and_a_manifest_v2_one_admit_the_bridge_once() {
        let mut v3 = serde_json::json!({"content_security_policy": {
            "extension_pages": "default-src 'self'; connect-src https://vault.example",
            "sandbox": "sandbox allow-scripts; script-src 'self'"}});
        assert!(admit_bridge(&mut v3));
        assert!(!admit_bridge(&mut v3));
        assert_eq!(
            v3["content_security_policy"]["extension_pages"],
            format!("default-src 'self'; connect-src https://vault.example {BRIDGE_ORIGIN}")
        );
        assert_eq!(
            v3["content_security_policy"]["sandbox"],
            "sandbox allow-scripts; script-src 'self'"
        );
        let mut v2 = serde_json::json!({"content_security_policy": "default-src 'self'"});
        assert!(admit_bridge(&mut v2));
        assert_eq!(
            v2["content_security_policy"],
            format!("default-src 'self'; connect-src 'self' {BRIDGE_ORIGIN}")
        );
        assert!(!admit_bridge(&mut serde_json::json!({"name": "no policy"})));
    }

    #[test]
    fn a_page_that_declares_its_own_policy_admits_the_bridge() {
        // 1Password's popup.
        let page = r#"<html><head><META http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'self';" /><meta name="description" content="connect-src x"></head></html>"#;
        assert_eq!(
            page_with_bridge(page).as_deref(),
            Some(
                format!(
                    r#"<html><head><META http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'self'; connect-src {BRIDGE_ORIGIN}" /><meta name="description" content="connect-src x"></head></html>"#
                )
                .as_str()
            )
        );
        // NordPass's, its quotes written as references.
        let page = "<meta http-equiv='content-security-policy' content='connect-src &#39;self&#39; https://a.example'>";
        assert_eq!(
            page_with_bridge(page),
            Some(format!(
                "<meta http-equiv='content-security-policy' content='connect-src &#39;self&#39; https://a.example {BRIDGE_ORIGIN}'>"
            ))
        );
        let admitted = page_with_bridge(page).unwrap();
        assert_eq!(page_with_bridge(&admitted), None);
        for untouched in [
            "<html><body>no policy</body></html>",
            "<meta http-equiv=\"Content-Security-Policy\" content=\"script-src 'self'\">",
            "<meta http-equiv=\"Content-Security-Policy\" content=\"connect-src &unknown; x\">",
        ] {
            assert_eq!(page_with_bridge(untouched), None, "{untouched}");
        }
    }

    #[test]
    fn preparing_an_extension_admits_the_bridge_in_its_pages_once() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("manifest.json"),
            r#"{"manifest_version":3}"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.path().join("popup")).unwrap();
        let policy = r#"<meta http-equiv="Content-Security-Policy" content="default-src 'none'">"#;
        std::fs::write(root.path().join("popup/index.html"), policy).unwrap();
        std::fs::write(root.path().join("popup/index.js"), policy).unwrap();
        assert_eq!(prepare_tabs(root.path()), None);
        let page = std::fs::read_to_string(root.path().join("popup/index.html")).unwrap();
        assert!(page.contains(BRIDGE_ORIGIN), "{page}");
        assert_eq!(
            std::fs::read_to_string(root.path().join("popup/index.js")).unwrap(),
            policy
        );
        // Once: what is written later is the extension's own.
        std::fs::write(root.path().join("popup/index.html"), policy).unwrap();
        prepare_tabs(root.path());
        assert_eq!(
            std::fs::read_to_string(root.path().join("popup/index.html")).unwrap(),
            policy
        );
    }

    #[test]
    fn an_extension_turned_on_stays_on_when_the_browser_prepares_it() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("vault");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(
            path.join("manifest.json"),
            r#"{"name":"Vault","version":"1","manifest_version":3,"background":{"service_worker":"bg.js"},"content_security_policy":{"extension_pages":"default-src 'self'; connect-src https://vault.example"}}"#,
        )
        .unwrap();
        std::fs::write(path.join("bg.js"), "").unwrap();
        let extension = list_in(root.path()).pop().unwrap();
        set_approved(&extension, true).unwrap();
        assert!(prepare_tabs(&path).is_some());
        assert!(is_approved(&extension), "preparing it turned it off");
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["background"]["service_worker"], "bg.js");
        assert!(
            manifest["content_security_policy"]["extension_pages"]
                .as_str()
                .unwrap()
                .ends_with(BRIDGE_ORIGIN)
        );
        // One the person did not turn on is not turned on by it.
        let other = root.path().join("other");
        std::fs::create_dir(&other).unwrap();
        std::fs::write(
            other.join("manifest.json"),
            r#"{"name":"Other","version":"1","manifest_version":3,"content_security_policy":{"extension_pages":"default-src 'self'"}}"#,
        )
        .unwrap();
        assert_eq!(prepare_tabs(&other), None);
        let others = list_in(root.path());
        let other = others.iter().find(|info| info.name == "Other").unwrap();
        assert!(!is_approved(other));
        assert!(
            std::fs::read_to_string(other.path.join("manifest.json"))
                .unwrap()
                .contains(BRIDGE_ORIGIN)
        );
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::crx::fixtures::{Signer, crx};
    use std::io::{Cursor, Write};

    /// Stage `archive` as the Web Store item `expected_id` and install it, as
    /// the browser does.
    fn install_verified_crx(
        archive: &[u8],
        root: &Path,
        expected_id: &str,
    ) -> Result<ExtensionInfo, ExtensionError> {
        stage_package(archive.to_vec(), expected_id)?.install(root)
    }

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
        assert_eq!(
            staged.permissions.first().map(String::as_str),
            Some("Read and change all your data on all websites")
        );
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
        assert_eq!(
            permission_summary(&info)
                .unwrap()
                .first()
                .map(String::as_str),
            Some("Read and change all your data on all websites")
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
