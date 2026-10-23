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
    let mut found: Vec<ExtensionInfo> = entries
        .take(MAX_INSTALLED_EXTENSIONS)
        .flatten()
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
