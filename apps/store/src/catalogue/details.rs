//! What an app's page shows beyond its catalogue row: the localized
//! description, screenshots, what changed in each release, its licence,
//! developer and size, and what it may do on this device in plain words.
//!
//! A Flathub app's details come from its `AppStream` document and build
//! summary, fetched when its page opens and kept on disk so the page shows
//! them at once next time. A Tilo app's come from its signed listing and,
//! once it is on this device, its signed manifest.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The most screenshots a page shows.
const MAX_SCREENSHOTS: usize = 8;
/// The most releases a page lists under what's new.
const MAX_RELEASES: usize = 6;
/// The longest description kept, in characters.
const MAX_DESCRIPTION_CHARS: usize = 8_000;
/// A details file larger than this is not read.
const MAX_FILE_BYTES: u64 = 512 * 1024;
/// How long kept details stand in before the page fetches them again.
pub const FRESH_FOR_SECONDS: u64 = 24 * 60 * 60;

/// One release, newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    /// Unix seconds; 0 when the source gives no date.
    pub date_unix: u64,
    /// What changed, as paragraphs and list items in reading order.
    pub notes: Vec<String>,
}

/// What an app may do on this device, in the words a person reads on its
/// page. Each has one fluent message (`store-permission-*`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Permission {
    /// It can run programs outside its sandbox: a Flatpak that talks to
    /// Flatpak itself. Listed first because it undoes every other limit.
    LeavesSandbox,
    AllFiles,
    HomeFolder,
    /// Folders by their well-known name (`Downloads`, `Music`, ...).
    Folder(String),
    ChosenFiles,
    Internet,
    Camera,
    Microphone,
    Sound,
    ScreenCapture,
    Location,
    AllDevices,
    Bluetooth,
    LegacyDisplay,
    SystemServices,
    Secrets,
    SshKeys,
    Printing,
    Clipboard,
    Notifications,
    RunsPrograms,
    OpensLinks,
}

impl Permission {
    /// The fluent message that says this permission in plain words.
    #[must_use]
    pub fn message(&self) -> &'static str {
        match self {
            Self::LeavesSandbox => "store-permission-leaves-sandbox",
            Self::AllFiles => "store-permission-all-files",
            Self::HomeFolder => "store-permission-home",
            Self::Folder(_) => "store-permission-folder",
            Self::ChosenFiles => "store-permission-chosen-files",
            Self::Internet => "store-permission-internet",
            Self::Camera => "store-permission-camera",
            Self::Microphone => "store-permission-microphone",
            Self::Sound => "store-permission-sound",
            Self::ScreenCapture => "store-permission-screen-capture",
            Self::Location => "store-permission-location",
            Self::AllDevices => "store-permission-all-devices",
            Self::Bluetooth => "store-permission-bluetooth",
            Self::LegacyDisplay => "store-permission-legacy-display",
            Self::SystemServices => "store-permission-system-services",
            Self::Secrets => "store-permission-secrets",
            Self::SshKeys => "store-permission-ssh-keys",
            Self::Printing => "store-permission-printing",
            Self::Clipboard => "store-permission-clipboard",
            Self::Notifications => "store-permission-notifications",
            Self::RunsPrograms => "store-permission-runs-programs",
            Self::OpensLinks => "store-permission-opens-links",
        }
    }

    /// The sentence for this permission in the current language.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Folder(folder) => ui::i18n::I18nManager::format(
                self.message(),
                &[("folder", ui::i18n::ArgValue::Text(folder.clone()))],
            ),
            _ => ui::i18n::I18nManager::lookup_plain(self.message()),
        }
    }

    /// Whether the permission reaches past what a person handed the app:
    /// such rows are listed before the rest.
    #[must_use]
    pub fn is_broad(&self) -> bool {
        matches!(
            self,
            Self::LeavesSandbox
                | Self::AllFiles
                | Self::HomeFolder
                | Self::AllDevices
                | Self::SystemServices
                | Self::SshKeys
        )
    }
}

/// What a Tilo app's declared capabilities let it do. Capabilities that
/// only shape how it runs (threads, the GPU, its own files) say nothing a
/// person decides on and are left out.
#[must_use]
pub fn tilo_permissions(capabilities: &[String]) -> Vec<Permission> {
    let mut permissions: Vec<Permission> = capabilities
        .iter()
        .filter_map(|capability| {
            Some(match capability.as_str() {
                "network.http" | "network.sockets" | "network.fetch-proxy" => Permission::Internet,
                "files.open" | "files.save" | "files.paths" => Permission::ChosenFiles,
                "clipboard" => Permission::Clipboard,
                "processes" => Permission::RunsPrograms,
                "secrets" => Permission::Secrets,
                "open-uri" => Permission::OpensLinks,
                "notifications" => Permission::Notifications,
                "camera" => Permission::Camera,
                "microphone" => Permission::Microphone,
                "location" => Permission::Location,
                "screen-capture" => Permission::ScreenCapture,
                _ => return None,
            })
        })
        .collect();
    permissions.sort();
    permissions.dedup();
    permissions
}

/// What a Flathub app's sandbox lets it do, from the `permissions` object
/// of its build summary (Flatpak's finish arguments).
#[must_use]
pub fn flatpak_permissions(permissions: &serde_json::Value) -> Vec<Permission> {
    fn strings<'a>(value: Option<&'a serde_json::Value>) -> Vec<&'a str> {
        value
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect()
            })
            .unwrap_or_default()
    }
    /// The bus names a policy object (`{"talk": [...]}`) or map
    /// (`{"org.x": "talk"}`) lets the app reach.
    fn bus_names(value: Option<&serde_json::Value>) -> Vec<String> {
        let Some(value) = value else {
            return Vec::new();
        };
        if let Some(object) = value.as_object() {
            let listed: Vec<String> = ["talk", "own", "see", "call"]
                .iter()
                .flat_map(|key| strings(object.get(*key)))
                .map(str::to_owned)
                .collect();
            if !listed.is_empty() {
                return listed;
            }
            return object.keys().cloned().collect();
        }
        strings(Some(value))
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
    let mut out = Vec::new();
    for shared in strings(permissions.get("shared")) {
        if shared == "network" {
            out.push(Permission::Internet);
        }
    }
    for socket in strings(permissions.get("sockets")) {
        match socket {
            "x11" => out.push(Permission::LegacyDisplay),
            "pulseaudio" => out.push(Permission::Sound),
            "system-bus" | "session-bus" => out.push(Permission::SystemServices),
            "ssh-auth" | "gpg-agent" => out.push(Permission::SshKeys),
            "cups" => out.push(Permission::Printing),
            _ => {}
        }
    }
    for device in strings(permissions.get("devices")) {
        match device {
            "all" => out.push(Permission::AllDevices),
            "bluetooth" => out.push(Permission::Bluetooth),
            _ => {}
        }
    }
    for feature in strings(permissions.get("features")) {
        if feature == "bluetooth" {
            out.push(Permission::Bluetooth);
        }
    }
    for filesystem in strings(permissions.get("filesystems")) {
        let place = filesystem.split(':').next().unwrap_or(filesystem);
        let place = place.trim_end_matches('/');
        match place {
            "host" | "host-os" | "host-etc" | "/" => out.push(Permission::AllFiles),
            "home" | "~" => out.push(Permission::HomeFolder),
            _ => {
                if let Some(folder) = well_known_folder(place) {
                    out.push(Permission::Folder(folder));
                }
            }
        }
    }
    for name in bus_names(permissions.get("session-bus")) {
        match name.as_str() {
            "org.freedesktop.Flatpak" => out.push(Permission::LeavesSandbox),
            "org.freedesktop.Notifications" => out.push(Permission::Notifications),
            "org.freedesktop.secrets" => out.push(Permission::Secrets),
            _ => {}
        }
    }
    if !bus_names(permissions.get("system-bus")).is_empty() {
        out.push(Permission::SystemServices);
    }
    out.sort();
    out.dedup();
    out
}

/// The folder a Flatpak `xdg-*` filesystem names, as the person knows it.
fn well_known_folder(place: &str) -> Option<String> {
    let key = match place.split('/').next().unwrap_or(place) {
        "xdg-download" => "store-folder-downloads",
        "xdg-documents" => "store-folder-documents",
        "xdg-music" => "store-folder-music",
        "xdg-pictures" => "store-folder-pictures",
        "xdg-videos" => "store-folder-videos",
        "xdg-desktop" => "store-folder-desktop",
        "xdg-public-share" => "store-folder-public",
        "xdg-templates" => "store-folder-templates",
        _ => return None,
    };
    Some(ui::i18n::I18nManager::lookup_plain(key))
}

/// What an app's page shows beyond its catalogue row.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppDetails {
    pub id: String,
    /// The language the localized fields are in (BCP 47).
    pub locale: String,
    /// The description as paragraphs and list items, in reading order.
    pub paragraphs: Vec<String>,
    pub developer: String,
    /// SPDX licence expression.
    pub license: String,
    /// Screenshot URLs, in the order the app lists them.
    pub screenshots: Vec<String>,
    /// Newest first.
    pub releases: Vec<Release>,
    /// Bytes downloaded to install it; 0 when unknown.
    pub download_bytes: u64,
    /// Bytes it takes once installed; 0 when unknown.
    pub installed_bytes: u64,
    pub permissions: Vec<Permission>,
    /// Whether `permissions` is known: an empty list then means it asks for
    /// nothing, rather than that nobody said.
    pub permissions_known: bool,
    pub homepage: Option<String>,
    /// When these were fetched, Unix seconds.
    pub fetched_unix: u64,
}

impl AppDetails {
    /// Whether these were fetched recently enough to show without asking
    /// again.
    #[must_use]
    pub fn is_fresh(&self, now_unix: u64) -> bool {
        now_unix.saturating_sub(self.fetched_unix) < FRESH_FOR_SECONDS
    }

    /// A Flathub app's details from its `AppStream` document and its build
    /// summary (either may be missing).
    #[must_use]
    pub fn from_flathub(
        id: &str,
        locale: &str,
        appstream: Option<&serde_json::Value>,
        summary: Option<&serde_json::Value>,
        now_unix: u64,
    ) -> Self {
        let mut details = Self {
            id: id.to_owned(),
            locale: locale.to_owned(),
            fetched_unix: now_unix,
            ..Self::default()
        };
        if let Some(document) = appstream {
            let text = |names: &[&str]| {
                names.iter().find_map(|name| {
                    document
                        .get(*name)
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
            };
            details.paragraphs = paragraphs(&text(&["description"]).unwrap_or_default());
            details.developer = text(&["developer_name"])
                .or_else(|| {
                    document
                        .get("developer")
                        .and_then(|developer| developer.get("name"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_default();
            details.license = text(&["project_license", "license"]).unwrap_or_default();
            details.homepage = document
                .get("urls")
                .and_then(|urls| urls.get("homepage"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .or_else(|| text(&["homepage"]));
            details.screenshots = screenshot_urls(document.get("screenshots"));
            details.releases = releases(document.get("releases"));
        }
        if let Some(summary) = summary {
            details.download_bytes = size_field(summary.get("download_size"));
            details.installed_bytes = size_field(summary.get("installed_size"));
            if let Some(permissions) = summary
                .get("metadata")
                .and_then(|metadata| metadata.get("permissions"))
                .or_else(|| summary.get("permissions"))
            {
                details.permissions = flatpak_permissions(permissions);
                details.permissions_known = true;
            }
        }
        details
    }
}

/// A size that is one number, or one per architecture.
fn size_field(value: Option<&serde_json::Value>) -> u64 {
    let Some(value) = value else {
        return 0;
    };
    value
        .as_u64()
        .or_else(|| {
            value
                .get(std::env::consts::ARCH)
                .and_then(serde_json::Value::as_u64)
        })
        .unwrap_or(0)
}

/// The largest image of each screenshot, in order.
fn screenshot_urls(value: Option<&serde_json::Value>) -> Vec<String> {
    let Some(shots) = value.and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    shots
        .iter()
        .filter_map(|shot| {
            if let Some(sizes) = shot.get("sizes") {
                // Either a list of {src, width} or a map of "WxH" → URL.
                if let Some(list) = sizes.as_array() {
                    return list
                        .iter()
                        .filter_map(|size| {
                            let src = size.get("src").and_then(serde_json::Value::as_str)?;
                            let width = size
                                .get("width")
                                .and_then(|width| {
                                    width
                                        .as_u64()
                                        .or_else(|| width.as_str().and_then(|w| w.parse().ok()))
                                })
                                .unwrap_or(0);
                            Some((width, src))
                        })
                        .filter(|(width, _)| *width <= 1600)
                        .max_by_key(|(width, _)| *width)
                        .map(|(_, src)| src.to_owned());
                }
                if let Some(map) = sizes.as_object() {
                    return map
                        .iter()
                        .filter_map(|(dimensions, url)| {
                            let width: u64 = dimensions.split('x').next()?.parse().ok()?;
                            Some((width, url.as_str()?))
                        })
                        .filter(|(width, _)| *width <= 1600)
                        .max_by_key(|(width, _)| *width)
                        .map(|(_, src)| src.to_owned());
                }
            }
            ["imgDesktopUrl", "url", "src"].iter().find_map(|name| {
                shot.get(*name)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
        })
        .filter(|url| url.starts_with("https://"))
        .take(MAX_SCREENSHOTS)
        .collect()
}

fn releases(value: Option<&serde_json::Value>) -> Vec<Release> {
    let Some(list) = value.and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|release| {
            let version = release
                .get("version")
                .and_then(serde_json::Value::as_str)?
                .to_owned();
            let date_unix = release
                .get("timestamp")
                .and_then(|value| {
                    value
                        .as_u64()
                        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
                })
                .unwrap_or(0);
            let notes = release
                .get("description")
                .and_then(serde_json::Value::as_str)
                .map(paragraphs)
                .unwrap_or_default();
            Some(Release {
                version,
                date_unix,
                notes,
            })
        })
        .take(MAX_RELEASES)
        .collect()
}

/// `AppStream` description markup (`<p>`, `<ul><li>`, `<em>`, `<code>`) as
/// plain paragraphs and list items, entities decoded, in reading order.
#[must_use]
pub fn paragraphs(markup: &str) -> Vec<String> {
    fn flush(current: &mut String, blocks: &mut Vec<String>) {
        let text = current.split_whitespace().collect::<Vec<_>>().join(" ");
        if !text.is_empty() {
            blocks.push(text);
        }
        current.clear();
    }
    let mut blocks = Vec::new();
    let mut current = String::new();
    let mut chars = markup.chars().peekable();
    let mut total = 0usize;
    while let Some(character) = chars.next() {
        if total > MAX_DESCRIPTION_CHARS {
            break;
        }
        match character {
            '<' => {
                let mut tag = String::new();
                for next in chars.by_ref() {
                    if next == '>' {
                        break;
                    }
                    tag.push(next);
                }
                let name = tag
                    .trim_start_matches('/')
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if matches!(name.as_str(), "p" | "li" | "br" | "ul" | "ol") {
                    flush(&mut current, &mut blocks);
                }
            }
            '&' => {
                let mut entity = String::new();
                while let Some(&next) = chars.peek() {
                    chars.next();
                    if next == ';' || entity.len() > 8 {
                        break;
                    }
                    entity.push(next);
                }
                current.push_str(match entity.as_str() {
                    "amp" => "&",
                    "lt" => "<",
                    "gt" => ">",
                    "quot" => "\"",
                    "apos" | "#39" => "'",
                    "nbsp" => " ",
                    _ => "",
                });
            }
            '\n' => current.push(' '),
            _ => {
                total += 1;
                current.push(character);
            }
        }
    }
    flush(&mut current, &mut blocks);
    blocks
}

/// Where kept details live: one file per app under the Store's cache.
fn path(id: &str) -> Option<PathBuf> {
    if !crate::reports::valid_app_id(id) {
        return None;
    }
    crate::config::cache_dir().map(|dir| dir.join("details").join(format!("{id}.json")))
}

/// The details kept for `id` in `locale`, however old; `None` when there are
/// none or they are in another language.
#[must_use]
pub fn load(id: &str, locale: &str) -> Option<AppDetails> {
    let path = path(id)?;
    if std::fs::metadata(&path).ok()?.len() > MAX_FILE_BYTES {
        return None;
    }
    let details: AppDetails = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    (details.id == id && details.locale == locale).then_some(details)
}

/// Keep `details` for the next time the page opens.
///
/// # Errors
///
/// Returns why they could not be written.
pub fn save(details: &AppDetails) -> Result<(), String> {
    let path = path(&details.id).ok_or_else(|| "no cache directory".to_owned())?;
    let bytes = serde_json::to_vec(details).map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write_creating_parent(&path, &bytes)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appstream_markup_reads_as_paragraphs_and_list_items() {
        assert_eq!(
            paragraphs(
                "<p>Edit &amp; share   photos.</p><ul><li>Crop</li><li>Rotate <em>fast</em></li></ul>"
            ),
            ["Edit & share photos.", "Crop", "Rotate fast"]
        );
        assert!(paragraphs("").is_empty());
    }

    #[test]
    fn flathub_details_name_size_licence_permissions_and_releases() {
        let appstream = serde_json::json!({
            "description": "<p>A music player.</p>",
            "developer_name": "Example Devs",
            "project_license": "GPL-3.0-or-later",
            "urls": {"homepage": "https://example.org"},
            "screenshots": [
                {"sizes": [{"src": "https://img/small.png", "width": "624"},
                           {"src": "https://img/large.png", "width": "1248"}]},
                {"sizes": {"624x351": "https://img/b.png"}},
                {"url": "http://insecure/c.png"}
            ],
            "releases": [{"version": "2.0", "timestamp": "1760000000",
                          "description": "<ul><li>Gapless playback</li></ul>"}]
        });
        let mut installed = serde_json::Map::new();
        installed.insert(std::env::consts::ARCH.into(), 12_000_000u64.into());
        let summary = serde_json::json!({
            "download_size": 4_000_000u64,
            "installed_size": installed,
            "metadata": {"permissions": {
                "shared": ["network", "ipc"],
                "sockets": ["wayland", "pulseaudio"],
                "filesystems": ["xdg-music:ro"],
                "session-bus": {"talk": ["org.freedesktop.Notifications"]}
            }}
        });
        let details = AppDetails::from_flathub(
            "org.example.Music",
            "en-US",
            Some(&appstream),
            Some(&summary),
            10,
        );
        assert_eq!(details.paragraphs, ["A music player."]);
        assert_eq!(details.developer, "Example Devs");
        assert_eq!(details.license, "GPL-3.0-or-later");
        assert_eq!(details.homepage.as_deref(), Some("https://example.org"));
        assert_eq!(
            details.screenshots,
            ["https://img/large.png", "https://img/b.png"],
            "the largest image of each screenshot, never an insecure one"
        );
        assert_eq!(details.releases[0].version, "2.0");
        assert_eq!(details.releases[0].date_unix, 1_760_000_000);
        assert_eq!(details.releases[0].notes, ["Gapless playback"]);
        assert_eq!(details.download_bytes, 4_000_000);
        assert_eq!(details.installed_bytes, 12_000_000);
        assert!(details.permissions_known);
        assert!(details.permissions.contains(&Permission::Internet));
        assert!(details.permissions.contains(&Permission::Sound));
        assert!(details.permissions.contains(&Permission::Notifications));
        assert!(
            details
                .permissions
                .iter()
                .any(|permission| matches!(permission, Permission::Folder(_)))
        );
        assert!(details.is_fresh(11));
        assert!(!details.is_fresh(10 + FRESH_FOR_SECONDS));
    }

    #[test]
    fn a_flatpak_that_talks_to_flatpak_is_said_to_leave_its_sandbox_first() {
        let permissions = flatpak_permissions(&serde_json::json!({
            "filesystems": ["home"],
            "session-bus": {"org.freedesktop.Flatpak": "talk"}
        }));
        assert_eq!(permissions.first(), Some(&Permission::LeavesSandbox));
        assert!(permissions.contains(&Permission::HomeFolder));
    }

    #[test]
    fn tilo_capabilities_that_shape_how_an_app_runs_are_not_listed() {
        let capabilities = [
            "threads",
            "gpu",
            "app-files",
            "network.http",
            "files.open",
            "files.save",
        ]
        .map(String::from);
        assert_eq!(
            tilo_permissions(&capabilities),
            [Permission::ChosenFiles, Permission::Internet]
        );
    }
}
