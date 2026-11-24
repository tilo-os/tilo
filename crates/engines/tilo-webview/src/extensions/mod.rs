//! The extensions the Browser installs: a package unpacked under the identifier its signing key
//! names, the manifest that says what it may do, and whether the person turned it on. Chromium
//! loads the ones that are on at launch, so an installed extension is there the next time a page
//! window starts.

pub mod crx;
/// The Chrome Web Store's own pages and update service: a desktop request, so not on the web.
#[cfg(not(target_arch = "wasm32"))]
pub mod web_store;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use bounded_zip::{BoundedArchive, ZipError, ZipLimits};

/// One installed extension, as its manifest says what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extension {
    /// Its identifier: the one its signing key names, or a slug of its name for an unsigned
    /// package.
    pub id: String,
    pub name: String,
    pub version: String,
    /// `2` or `3`; Chrome loads the startup ones only from `3`.
    pub manifest_version: u64,
    pub description: String,
    /// What it asks for in its manifest, as its own words (`storage`, `tabs`, a site pattern).
    pub permissions: Vec<String>,
    /// Its keyword in the address (its manifest's `omnibox.keyword`), when it has one a person
    /// can type: one word, without spaces.
    pub keyword: Option<String>,
    /// Whether it may show a page beside the page the person browses (its `sidePanel`
    /// permission).
    pub side_panel: bool,
    /// Where it stands on this device.
    pub path: PathBuf,
}

/// Why an extension could not be installed, read, or removed. The kind is what the app says in
/// the person's language; the detail is for the log and for someone who asks the engine.
#[derive(Debug)]
pub enum ExtensionError {
    NotAPackage,
    TooLarge,
    Damaged(String),
    Broken(String),
    NotSaved(std::io::Error),
    /// The store could not be asked, or its answer left Google's hosts.
    Download(String),
    /// The store has no package for this browser.
    NoPackage,
    /// What was given names no Chrome Web Store item: neither an extension's id nor its page.
    NoItem,
}

impl ExtensionError {
    /// A short word for the kind of failure: what a person is told happened, in their language.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NotAPackage => "package",
            Self::TooLarge => "too-large",
            Self::Damaged(_) => "damaged",
            Self::Broken(_) => "broken",
            Self::NotSaved(_) => "not-saved",
            Self::Download(_) => "download",
            Self::NoPackage => "no-package",
            Self::NoItem => "item",
        }
    }
}

impl std::fmt::Display for ExtensionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAPackage => f.write_str("the file is no extension package"),
            Self::TooLarge => f.write_str("the package is over the size limit"),
            Self::Damaged(detail) => write!(f, "the package is damaged: {detail}"),
            Self::Broken(detail) => write!(f, "the manifest is broken: {detail}"),
            Self::NotSaved(error) => write!(f, "the extension could not be saved: {error}"),
            Self::Download(detail) => {
                write!(f, "the Chrome Web Store could not be reached: {detail}")
            }
            Self::NoPackage => f.write_str("the Chrome Web Store has no package for this browser"),
            Self::NoItem => f.write_str("this is no Chrome Web Store item"),
        }
    }
}

impl From<std::io::Error> for ExtensionError {
    fn from(error: std::io::Error) -> Self {
        Self::NotSaved(error)
    }
}

/// Ad blockers carry their filter lists: Adblock Plus is 78 MB packed.
const MAX_PACKAGE_BYTES: usize = 256 * 1024 * 1024;
/// Checked before anything is inflated or written to disk.
const ARCHIVE_LIMITS: ZipLimits = ZipLimits {
    max_parts: 4_096,
    max_part_bytes: 256 * 1024 * 1024,
    max_total_bytes: 1024 * 1024 * 1024,
};
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_MESSAGES_BYTES: u64 = 1024 * 1024;
/// The most installed extensions the list and the launch read, so a damaged folder cannot turn
/// a page's start into unbounded disk work.
pub const MAX_INSTALLED: usize = 100;
const MAX_NAME_BYTES: usize = 512;
const MAX_VERSION_BYTES: usize = 128;
const MAX_DESCRIPTION_BYTES: usize = 8 * 1024;
const MAX_PERMISSIONS: usize = 128;
const MAX_PERMISSION_BYTES: usize = 512;

/// The folder this device's installed extensions stand in.
#[must_use]
pub fn extensions_dir() -> Option<PathBuf> {
    tilo_paths::data_dir().map(|dir| dir.join("tilo-browser/extensions"))
}

/// The paths of the extensions the person turned on, for the engine to launch with.
#[must_use]
pub fn approved_paths(root: &Path) -> Vec<PathBuf> {
    installed(root)
        .into_iter()
        .filter(|(_, enabled)| *enabled)
        .map(|(extension, _)| extension.path)
        .collect()
}

/// What is installed in `root`, by name, and whether the person turned each on.
#[must_use]
pub fn installed(root: &Path) -> Vec<(Extension, bool)> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found: Vec<(Extension, bool)> = entries
        .flatten()
        .filter(|entry| {
            entry
                .file_type()
                .is_ok_and(|kind| kind.is_dir() || kind.is_symlink())
        })
        .take(MAX_INSTALLED)
        .filter_map(|entry| {
            let path = entry.path();
            let text = read_text(&path.join("manifest.json")).ok()?;
            let mut extension =
                parse_manifest(&text, |file| read_text(&path.join(file)).ok()).ok()?;
            if let Some(name) = entry.file_name().to_str()
                && crx::is_extension_id(name)
            {
                name.clone_into(&mut extension.id);
            }
            extension.path = path;
            Some(extension)
        })
        .map(|extension| {
            let enabled = is_approved(&extension);
            (extension, enabled)
        })
        .collect();
    found.sort_by(|(a, _), (b, _)| a.name.cmp(&b.name));
    found
}

/// Install the package `bytes` under `root` and answer what it is. A CRX3 package must verify,
/// and its identifier is the one its signing key names; a plain zip is accepted unsigned. With
/// `expected`, a package signed for another identifier is refused.
///
/// # Errors
///
/// Why nothing was installed: not a package, over a limit, damaged, or its manifest broken.
pub fn install(
    bytes: &[u8],
    root: &Path,
    expected: Option<&str>,
) -> Result<Extension, ExtensionError> {
    if bytes.len() > MAX_PACKAGE_BYTES {
        return Err(ExtensionError::TooLarge);
    }
    let (archive, signed) = if crx::is_crx(bytes) {
        let verified =
            crx::verify(bytes).map_err(|error| ExtensionError::Damaged(error.to_string()))?;
        if expected.is_some_and(|expected| expected != verified.id) {
            return Err(ExtensionError::Damaged(
                "the package is signed for another extension".to_owned(),
            ));
        }
        (verified.archive, Some((verified.id, verified.public_key)))
    } else if bytes.starts_with(b"PK") {
        (bytes, None)
    } else {
        return Err(ExtensionError::NotAPackage);
    };
    let mut zip = BoundedArchive::from_bytes(archive, ARCHIVE_LIMITS)
        .map_err(|error| package_error(&error))?;
    zip.check_declared_sizes()
        .map_err(|error| package_error(&error))?;

    // The manifest first, so a broken package never touches the disk.
    let manifest_text = zip
        .read_text("manifest.json", MAX_MANIFEST_BYTES)
        .map_err(|error| match error {
            ZipError::Missing { .. } => {
                ExtensionError::Broken("the package holds no manifest.json".to_owned())
            }
            error if error.is_limit_breach() => {
                ExtensionError::Broken("manifest.json is too large".to_owned())
            }
            error => ExtensionError::Broken(error.to_string()),
        })?;
    let mut extension = parse_manifest(&manifest_text, |file| {
        zip.read_text(file, MAX_MESSAGES_BYTES).ok()
    })?;
    let keyed = match &signed {
        Some((id, key)) => {
            extension.id.clone_from(id);
            Some(manifest_with_key(&manifest_text, key)?)
        }
        None => None,
    };

    let dest = root.join(&extension.id);
    if dest.exists() {
        std::fs::remove_dir_all(&dest)?;
    }
    std::fs::create_dir_all(&dest)?;
    // Bounded on the way to disk: the sizes checked were the package's own claim, and a header
    // that lies small must not fill the directory.
    zip.extract_to(&dest)
        .map_err(|error| package_error(&error))?;
    if let Some(manifest) = keyed {
        std::fs::write(dest.join("manifest.json"), manifest)?;
    }
    extension.path = dest;
    Ok(extension)
}

/// How a store package is fetched: the extension's id and, for an update, the version this
/// device has, answered with the package of a newer version, or `None` when the store has none.
#[cfg(not(target_arch = "wasm32"))]
pub type Fetch<'a> = &'a dyn Fn(&str, Option<&str>) -> Result<Option<Vec<u8>>, ExtensionError>;

/// Install the store's item `said` (a store page's address, or the extension's id) under
/// `root`, and turn it on as installing from the store does. Its identifier is the one its
/// signing key names, which the package must be signed for.
///
/// # Errors
///
/// Why nothing was installed: `said` names no item, the store could not be asked, it has no
/// package for this browser, or the package is damaged or broken.
#[cfg(not(target_arch = "wasm32"))]
pub fn add(said: &str, root: &Path, fetch: Fetch<'_>) -> Result<Extension, ExtensionError> {
    let item = web_store::item(said).ok_or(ExtensionError::NoItem)?;
    let bytes = fetch(&item.id, None)?.ok_or(ExtensionError::NoPackage)?;
    let extension = install(&bytes, root, Some(&item.id))?;
    set_approved(&extension, true)?;
    Ok(extension)
}

/// One extension the store answered a newer package for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Updated {
    /// The version this device had.
    pub from: String,
    pub extension: Extension,
}

/// What asking the store for newer versions came to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Updates {
    /// How many installed store extensions were asked about.
    pub checked: usize,
    /// The ones that were updated, each with the version it replaced.
    pub updated: Vec<Updated>,
    /// The ones that could not be checked or installed, in the store's or the package's words.
    pub failed: Vec<String>,
}

/// Ask the store for newer versions of the store extensions installed under `root` and install
/// them, keeping each one as the person had it: one that was on stays on.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn check_updates(root: &Path, fetch: Fetch<'_>) -> Updates {
    let mut updates = Updates::default();
    for (extension, enabled) in installed(root) {
        // Only a package signed for an extension id has an update service entry; an unsigned
        // zip the person installed by hand is theirs to replace.
        if !crx::is_extension_id(&extension.id) {
            continue;
        }
        updates.checked += 1;
        let package = match fetch(&extension.id, Some(&extension.version)) {
            Ok(package) => package,
            Err(error) => {
                updates.failed.push(format!("{}: {error}", extension.name));
                continue;
            }
        };
        let Some(bytes) = package else {
            continue;
        };
        match install(&bytes, root, Some(&extension.id)) {
            Ok(updated) => {
                // The folder was rewritten, so its manifest's digest changed with it: the
                // approval is keyed to that digest and has to be written again.
                if enabled && let Err(error) = set_approved(&updated, true) {
                    updates.failed.push(format!("{}: {error}", updated.name));
                    continue;
                }
                updates.updated.push(Updated {
                    from: extension.version,
                    extension: updated,
                });
            }
            Err(error) => updates.failed.push(format!("{}: {error}", extension.name)),
        }
    }
    updates
}

/// Forget the extension `id`: its folder under `root` and its approval.
///
/// # Errors
///
/// Why it could not be removed, or when `id` names no extension there.
pub fn remove(root: &Path, id: &str) -> Result<(), ExtensionError> {
    if !crx::is_extension_id(id)
        && !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    {
        return Err(ExtensionError::Broken("no such extension".to_owned()));
    }
    let dir = root.join(id);
    if !dir.is_dir() {
        return Err(ExtensionError::Broken("no such extension".to_owned()));
    }
    std::fs::remove_dir_all(&dir)?;
    let approval = approval_path_of(&dir);
    if let Some(approval) = approval
        && approval.exists()
    {
        let _ = std::fs::remove_file(approval);
    }
    Ok(())
}

/// Whether the person turned `extension` on. Chrome loads startup extensions only from
/// Manifest V3.
#[must_use]
pub fn is_approved(extension: &Extension) -> bool {
    extension.manifest_version == 3
        && approval_path(extension)
            .and_then(|path| std::fs::read_to_string(path).ok())
            .zip(digest_of(&extension.path.join("manifest.json")).ok())
            .is_some_and(|(approved, current)| approved == current)
}

/// Turn `extension` on or off. Turning one off keeps everything it stored.
///
/// # Errors
///
/// Why it could not be written, or when it is no Manifest V3 extension to turn on.
pub fn set_approved(extension: &Extension, approved: bool) -> Result<(), ExtensionError> {
    let path = approval_path(extension)
        .ok_or_else(|| ExtensionError::Broken("the extension's path is invalid".to_owned()))?;
    if approved {
        if extension.manifest_version != 3 {
            return Err(ExtensionError::Broken(
                "only Manifest V3 extensions can be turned on".to_owned(),
            ));
        }
        std::fs::write(path, digest_of(&extension.path.join("manifest.json"))?)?;
    } else if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// What an extension may do with the Browser's pages, as its manifest asks: its API
/// permissions, the sites it may read and script, and the sites its content scripts run on in a
/// page's frames too. As in Chrome, content script sites give no access to the tab APIs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Access {
    pub permissions: Vec<String>,
    pub hosts: Vec<String>,
    pub scriptable: Vec<String>,
    pub framed: Vec<String>,
    /// Its manifest's `commands`, whose suggested keys the engine gives and runs as Chrome does.
    pub commands: Option<serde_json::Value>,
}

/// The access the extension standing at `root` asks for, from the manifest it installed: `None`
/// when that manifest cannot be read.
#[must_use]
pub fn access_of(root: &Path) -> Option<Access> {
    let text = read_text(&root.join("manifest.json")).ok()?;
    let manifest: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(access(&manifest))
}

/// The access `manifest` asks for.
#[must_use]
pub fn access(manifest: &serde_json::Value) -> Access {
    let strings = |value: Option<&serde_json::Value>| -> Vec<String> {
        value
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_owned)
            .collect()
    };
    // Manifest V2 lists sites among its permissions; from V3 on Chrome grants none it finds
    // there, so sites are asked for in `host_permissions` alone.
    let before_v3 = manifest
        .get("manifest_version")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(2)
        < 3;
    let site = |entry: &str| entry == "<all_urls>" || entry.contains("://");
    let mut permissions = Vec::new();
    let mut hosts = strings(manifest.get("host_permissions"));
    for entry in strings(manifest.get("permissions")) {
        if before_v3 && site(&entry) {
            hosts.push(entry);
        } else {
            permissions.push(entry);
        }
    }
    let scripts: Vec<serde_json::Value> = manifest
        .get("content_scripts")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut scriptable = Vec::new();
    let mut framed = Vec::new();
    for script in &scripts {
        let matches = strings(script.get("matches"));
        if script
            .get("all_frames")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            framed.extend(matches.iter().cloned());
        }
        scriptable.extend(matches);
    }
    Access {
        permissions,
        hosts,
        scriptable,
        framed,
        commands: manifest.get("commands").filter(|c| c.is_object()).cloned(),
    }
}

fn package_error(error: &ZipError) -> ExtensionError {
    if error.is_limit_breach() {
        ExtensionError::TooLarge
    } else {
        ExtensionError::Damaged(error.to_string())
    }
}

/// The manifest with the package's public key as `key`: the Web Store strips it, and Chromium
/// derives the identifier from it.
fn manifest_with_key(manifest: &str, public_key: &[u8]) -> Result<String, ExtensionError> {
    use base64::Engine;
    let mut value: serde_json::Value = serde_json::from_str(manifest)
        .map_err(|error| ExtensionError::Broken(error.to_string()))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| ExtensionError::Broken("the manifest is no object".to_owned()))?;
    object.insert(
        "key".to_owned(),
        serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(public_key)),
    );
    serde_json::to_string(&value).map_err(|error| ExtensionError::Broken(error.to_string()))
}

fn approval_path(extension: &Extension) -> Option<PathBuf> {
    approval_path_of(&extension.path)
}

/// The approval of the extension standing at `root`, beside it: keyed by the folder's own path,
/// so the file names the extension and never another.
fn approval_path_of(root: &Path) -> Option<PathBuf> {
    let path = root.canonicalize().ok()?;
    let key = blake3::hash(path.to_str()?.as_bytes()).to_hex();
    Some(path.parent()?.join(format!(".tilo-approval-{key}.json")))
}

fn digest_of(manifest: &Path) -> Result<String, ExtensionError> {
    let text = read_text(manifest)?;
    Ok(blake3::hash(text.as_bytes()).to_hex().to_string())
}

fn read_text(path: &Path) -> Result<String, ExtensionError> {
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(ExtensionError::Broken(format!(
            "{} is too large",
            path.display()
        )));
    }
    let bytes = std::fs::read(path)?;
    String::from_utf8(bytes).map_err(|error| ExtensionError::NotSaved(std::io::Error::other(error)))
}

/// A type's localized messages, keyed in lower case: Chromium resolves `__MSG_name__` without
/// regard to case.
type Messages = HashMap<String, String>;

/// Where an extension keeps its default locale's messages, or `None` without one.
fn messages_file(manifest: &serde_json::Value) -> Option<String> {
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

fn messages(text: &str) -> Messages {
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

/// `text` with each `__MSG_name__` replaced by its message; a name with no message stays as
/// written rather than going blank.
fn localize(text: &str, messages: &Messages) -> String {
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

/// The manifest's extension with its names in the default locale; `read` gives a file of the
/// package by its path within it.
fn parse_manifest(
    text: &str,
    read: impl FnOnce(&str) -> Option<String>,
) -> Result<Extension, ExtensionError> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|error| ExtensionError::Broken(error.to_string()))?;
    let messages = messages_file(&value)
        .and_then(|file| read(&file))
        .map(|text| messages(&text))
        .unwrap_or_default();
    let name = bounded(
        &localize(
            value
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| ExtensionError::Broken("the manifest has no name".to_owned()))?,
            &messages,
        ),
        "name",
        MAX_NAME_BYTES,
    )?;
    let version = bounded(
        value
            .get("version")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ExtensionError::Broken("the manifest has no version".to_owned()))?,
        "version",
        MAX_VERSION_BYTES,
    )?;
    let manifest_version = value
        .get("manifest_version")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(2);
    let description = bounded(
        &localize(
            value
                .get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default(),
            &messages,
        ),
        "description",
        MAX_DESCRIPTION_BYTES,
    )?;
    let permissions = value
        .get("permissions")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            if entries.len() > MAX_PERMISSIONS {
                return Err(ExtensionError::Broken(
                    "the manifest has too many permissions".to_owned(),
                ));
            }
            entries
                .iter()
                .filter_map(|entry| entry.as_str().map(str::to_owned))
                .map(|permission| bounded(&permission, "permission", MAX_PERMISSION_BYTES))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok(Extension {
        id: slug(&name),
        name,
        version,
        manifest_version,
        description,
        side_panel: permissions
            .iter()
            .any(|permission| permission == "sidePanel"),
        keyword: keyword_of(&value),
        permissions,
        path: PathBuf::new(),
    })
}

/// The longest keyword a manifest gives, in characters.
const MAX_KEYWORD_CHARS: usize = 64;

/// The manifest's `omnibox.keyword`, when it is one a person can type: one word, without
/// spaces, as Chrome takes it.
fn keyword_of(manifest: &serde_json::Value) -> Option<String> {
    let keyword = manifest.get("omnibox")?.get("keyword")?.as_str()?.trim();
    (!keyword.is_empty()
        && keyword.chars().count() <= MAX_KEYWORD_CHARS
        && !keyword.chars().any(char::is_whitespace)
        && !keyword.chars().any(char::is_control))
    .then(|| keyword.to_owned())
}

fn bounded(value: &str, field: &str, most: usize) -> Result<String, ExtensionError> {
    if value.len() > most {
        return Err(ExtensionError::Broken(format!(
            "the manifest's {field} is too long"
        )));
    }
    Ok(value.to_owned())
}

/// A name as a folder: lower case, letters and digits, one dash between. A name with no ASCII
/// letter keeps a digest, which is never an empty path.
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
        format!("extension-{:.16}", blake3::hash(name.as_bytes()).to_hex())
    } else {
        out.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bounded_zip::write_zip;

    /// A package holding `manifest` and its default locale's messages.
    fn package(manifest: &str) -> Vec<u8> {
        let entries: Vec<(String, Vec<u8>)> = vec![
            ("manifest.json".to_owned(), manifest.as_bytes().to_vec()),
            (
                "_locales/en/messages.json".to_owned(),
                br#"{"extName":{"message":"Reader"},"extDescription":{"message":"Reads pages"}}"#
                    .to_vec(),
            ),
        ];
        write_zip(&entries, &ZipLimits::default()).expect("the package writes")
    }

    fn manifest(version: u64) -> String {
        format!(
            r#"{{"manifest_version":{version},"default_locale":"en","name":"__MSG_extName__","version":"1.2.3","description":"__MSG_extDescription__","permissions":["storage","tabs"],"host_permissions":["https://example.org/*"]}}"#
        )
    }

    tilo_test_env::scenario! {
        fn an_extension_installs_reads_and_is_forgotten;

        phase a_package_installs_localized_and_under_its_own_folder;
        {
            let root = tempfile::tempdir().expect("a root");
            let extension = install(&package(&manifest(3)), root.path(), None).expect("installed");
            assert_eq!(extension.name, "Reader", "the name comes from its messages");
            assert_eq!(extension.description, "Reads pages");
            assert_eq!(extension.version, "1.2.3");
            assert_eq!(extension.manifest_version, 3);
            assert_eq!(extension.permissions, ["storage", "tabs"]);
            assert!(extension.path.join("manifest.json").is_file());
            assert_eq!(installed(root.path()).len(), 1);
            assert!(!is_approved(&extension), "installing does not turn it on");
        }

        phase turning_one_on_launches_it_and_turning_it_off_stops_that;
        {
            let root = tempfile::tempdir().expect("a root");
            let extension = install(&package(&manifest(3)), root.path(), None).expect("installed");
            assert!(approved_paths(root.path()).is_empty());
            set_approved(&extension, true).expect("turned on");
            assert!(is_approved(&extension));
            assert_eq!(
                approved_paths(root.path()),
                std::slice::from_ref(&extension.path)
            );
            set_approved(&extension, false).expect("turned off");
            assert!(approved_paths(root.path()).is_empty());
            assert!(extension.path.exists(), "what it stored stays");
        }

        phase what_is_no_manifest_v3_package_stays_off_and_a_stranger_is_refused;
        {
            let root = tempfile::tempdir().expect("a root");
            let old = install(&package(&manifest(2)), root.path(), None).expect("installed");
            assert!(set_approved(&old, true).is_err(), "Manifest V2 never loads");
            assert!(!is_approved(&old));
            assert!(install(b"not a package", root.path(), None).is_err());
            assert!(install(&package("{}"), root.path(), None).is_err());
            remove(root.path(), &old.id).expect("forgotten");
            assert!(installed(root.path()).is_empty());
            assert!(remove(root.path(), "../etc").is_err(), "no path leaves the folder");
        }

        phase a_signed_package_keeps_the_identifier_its_key_names;
        {
            let root = tempfile::tempdir().expect("a root");
            let signer = crx::fixtures::Signer::new();
            let signed = crx::fixtures::crx(&signer, &[&signer], &package(&manifest(3)));
            let extension = install(&signed, root.path(), None).expect("installed");
            assert_eq!(extension.id, crx::extension_id(&signer.spki));
            assert!(crx::is_extension_id(&extension.id));
            assert_eq!(
                installed(root.path())[0].0.id,
                extension.id,
                "the folder's identifier is the one it shows"
            );
            let stranger = crx::fixtures::Signer::new();
            let other = crx::fixtures::crx(&stranger, &[&stranger], &package(&manifest(3)));
            assert!(
                install(&other, root.path(), Some(&extension.id)).is_err(),
                "a package signed for another identifier is refused"
            );
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn store_packages_install_on_and_are_kept_up_to_date() {
        let root = tempfile::tempdir().expect("a root");
        let signer = crx::fixtures::Signer::new();
        let id = crx::extension_id(&signer.spki);
        let manifest = |version: &str| {
            format!(r#"{{"manifest_version":3,"name":"Reader","version":"{version}"}}"#)
        };
        let signed =
            |version: &str| crx::fixtures::crx(&signer, &[&signer], &package(&manifest(version)));

        let newest = |asked: &str, installed: Option<&str>| {
            assert_eq!(
                asked,
                id.as_str(),
                "the store is asked for the item's own id"
            );
            assert!(
                installed.is_none(),
                "installing asks for the newest package"
            );
            Ok(Some(signed("1.2.3")))
        };
        let extension = add(
            &format!("https://chromewebstore.google.com/detail/reader/{id}"),
            root.path(),
            &newest,
        )
        .expect("installed");
        assert_eq!(
            extension.id, id,
            "the package's identifier is the one it is signed for"
        );
        assert_eq!(extension.version, "1.2.3");
        assert!(
            is_approved(&extension),
            "installing from the store turns it on"
        );
        assert_eq!(
            approved_paths(root.path()),
            std::slice::from_ref(&extension.path)
        );

        // Nothing newer: the store answers with no package, and what is installed stays.
        let current = |_: &str, installed: Option<&str>| {
            assert_eq!(installed, Some("1.2.3"));
            Ok(None)
        };
        let none = check_updates(root.path(), &current);
        assert_eq!(none.checked, 1, "the store extension is asked about");
        assert!(none.updated.is_empty() && none.failed.is_empty());

        // A newer package installs, stays on, and is what the inventory then reads.
        let newer = |_: &str, installed: Option<&str>| {
            assert_eq!(
                installed,
                Some("1.2.3"),
                "the update names the version it has"
            );
            Ok(Some(signed("1.4.0")))
        };
        let updates = check_updates(root.path(), &newer);
        assert_eq!(updates.failed, Vec::<String>::new());
        assert_eq!(updates.updated.len(), 1);
        assert_eq!(updates.updated[0].from, "1.2.3");
        assert_eq!(updates.updated[0].extension.version, "1.4.0");
        let listed = installed(root.path());
        assert_eq!(listed.len(), 1, "the update replaced the package");
        assert_eq!(listed[0].0.version, "1.4.0");
        assert!(listed[0].1, "an extension that was on stays on");

        // A store that cannot be reached says which extension it could not check.
        let unreachable =
            |_: &str, _: Option<&str>| Err(ExtensionError::Download("no network".to_owned()));
        let failed = check_updates(root.path(), &unreachable);
        assert_eq!(failed.updated.len(), 0);
        assert_eq!(failed.failed.len(), 1);
        assert!(failed.failed[0].contains("Reader"), "{:?}", failed.failed);
    }

    #[test]
    fn access_reads_sites_scripts_and_frames() {
        let manifest: serde_json::Value = serde_json::from_str(
            r#"{"manifest_version":3,
                "host_permissions":["https://example.org/*"],
                "permissions":["storage"],
                "content_scripts":[{"matches":["https://a.example/*"]},
                                   {"matches":["https://b.example/*"],"all_frames":true}],
                "commands":{"toggle":{"suggested_key":{"default":"Ctrl+Shift+Y"}}}}"#,
        )
        .expect("a manifest");
        let access = access(&manifest);
        assert_eq!(access.permissions, ["storage"]);
        assert_eq!(access.hosts, ["https://example.org/*"]);
        assert_eq!(
            access.scriptable,
            ["https://a.example/*", "https://b.example/*"]
        );
        assert_eq!(access.framed, ["https://b.example/*"]);
        assert!(access.commands.is_some());
    }
}
