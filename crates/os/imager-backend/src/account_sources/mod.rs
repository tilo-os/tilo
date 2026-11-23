//! Bounded metadata discovery beneath explicitly consented source roots.

#[cfg(target_os = "linux")]
mod nextcloud;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod thunderbird_tests;
pub use tilo_migrate::thunderbird;
mod tilo;

#[cfg(target_os = "linux")]
pub use nextcloud::{
    NativeAccount as NextcloudAccount, accounts as nextcloud_accounts,
    discover as discover_nextcloud, source_id as nextcloud_source_id,
};
pub use thunderbird::{
    ReviewedThunderbird, ThunderbirdAccount, ThunderbirdAddressBook, ThunderbirdCalendar,
    ThunderbirdFolder, ThunderbirdProfile,
};
pub use tilo::{TiloCandidate, fingerprint};

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

#[derive(Clone, Debug)]
pub struct SourceRoots {
    pub home: PathBuf,
    pub config: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    Google,
    Microsoft,
    Nextcloud,
    WebDav,
    Spotify,
    Slack,
    Discord,
    Firefox,
    Chromium,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    Gnome,
    Nextcloud,
    Rclone,
    Application,
    Browser,
}

/// What the person consented to, not an inference from a provider name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Service {
    Files,
    Mail,
    Calendar,
    Contacts,
}

/// Presence means a configured account or profile was found; it does not establish a valid login.
#[derive(Clone, Debug, Serialize)]
pub struct Candidate {
    pub id: String,
    pub provider: Provider,
    pub origin: Origin,
    pub label: String,
    pub identity: Option<String>,
    pub server: Option<String>,
    pub profile: PathBuf,
    pub needs_sign_in: bool,
    pub can_import_credentials: bool,
    /// Empty when the source does not describe them; never guessed from a provider.
    #[serde(default)]
    pub services: Vec<Service>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Inventory {
    pub candidates: Vec<Candidate>,
    pub tilo_accounts: Vec<TiloCandidate>,
    #[serde(default)]
    pub thunderbird: Vec<ThunderbirdProfile>,
    /// They retain private profile paths and are never serialized into receipts.
    #[serde(skip)]
    pub thunderbird_reviewed: Vec<ReviewedThunderbird>,
    /// An incomplete scan is never called complete, and paths stay out of diagnostics.
    pub unreadable_sources: Vec<&'static str>,
}

const MAX_CONFIG: u64 = 2 * 1024 * 1024;

pub fn discover(roots: &SourceRoots) -> Inventory {
    let mut inventory = Inventory::default();
    tilo::discover(roots, &mut inventory);
    for (relative, origin, label) in [
        ("goa-1.0/accounts.conf", Origin::Gnome, "Online accounts"),
        ("Nextcloud/nextcloud.cfg", Origin::Nextcloud, "Nextcloud"),
        ("rclone/rclone.conf", Origin::Rclone, "rclone"),
    ] {
        let path = roots.config.join(relative);
        match metadata(&roots.config, &path) {
            Ok(Some(text)) => match origin {
                Origin::Gnome => gnome(&text, &path, &mut inventory),
                Origin::Nextcloud => nextcloud(&text, &path, &mut inventory),
                Origin::Rclone => rclone(&text, &path, &mut inventory),
                _ => {}
            },
            Ok(None) => {}
            Err(()) => inventory.unreadable_sources.push(label),
        }
    }
    for (relative, provider, label) in [
        ("Slack", Provider::Slack, "Slack"),
        ("discord", Provider::Discord, "Discord"),
        ("spotify", Provider::Spotify, "Spotify"),
    ] {
        profile(&roots.config, relative, provider, label, &mut inventory);
    }
    append_browsers(&roots.home, Some(&roots.config), &mut inventory);
    append_home_profiles(&roots.home, &mut inventory);
    append_thunderbird(&roots.home, &mut inventory);
    inventory
}

/// This never reads a config root or substitutes Home/.config for a refused one.
pub fn discover_home_profiles(home: &Path) -> Inventory {
    let mut inventory = Inventory::default();
    append_browsers(home, None, &mut inventory);
    append_home_profiles(home, &mut inventory);
    append_thunderbird(home, &mut inventory);
    inventory
}

/// Keep the serializable profile list and the pinned roots the importer reads
/// through, and name every source that could not be read.
fn append_thunderbird(home: &Path, inventory: &mut Inventory) {
    let scan = thunderbird::scan(home);
    for reviewed in scan.reviewed {
        inventory.thunderbird.push(reviewed.profile.clone());
        inventory.thunderbird_reviewed.push(reviewed);
    }
    inventory.unreadable_sources.extend(
        scan.problems
            .iter()
            .map(tilo_migrate::thunderbird::Unreadable::source),
    );
}

/// The browsers [`tilo_migrate::browsers::LOCATIONS`] lists: under the config directory
/// when one is given (a refused one is never replaced by the home's), under the home and in its
/// Flatpaks.
fn append_browsers(home: &Path, config: Option<&Path>, inventory: &mut Inventory) {
    use tilo_migrate::browsers::{Engine, LOCATIONS, Root};
    for location in LOCATIONS {
        let provider = match location.engine {
            Engine::Chromium => Provider::Chromium,
            Engine::Firefox => Provider::Firefox,
        };
        match (location.root, config) {
            (Root::Config, Some(config)) => {
                profile(config, location.path, provider, location.label, inventory);
            }
            (Root::Config, None) => {}
            (Root::Home, _) => profile(home, location.path, provider, location.label, inventory),
            (Root::Flatpak(app), _) => profile(
                home,
                &format!(".var/app/{app}/{}", location.path),
                provider,
                location.label,
                inventory,
            ),
        }
    }
}

fn append_home_profiles(home: &Path, inventory: &mut Inventory) {
    for (app, relative, provider, label) in [
        ("com.slack.Slack", "config/Slack", Provider::Slack, "Slack"),
        (
            "com.discordapp.Discord",
            "config/discord",
            Provider::Discord,
            "Discord",
        ),
        (
            "com.spotify.Client",
            "config/spotify",
            Provider::Spotify,
            "Spotify",
        ),
    ] {
        profile(
            home,
            &format!(".var/app/{app}/{relative}"),
            provider,
            label,
            inventory,
        );
    }
}

pub fn metadata(root: &Path, path: &Path) -> Result<Option<Zeroizing<String>>, ()> {
    let Some(bytes) = metadata_bytes(root, path)? else {
        return Ok(None);
    };
    let text = std::str::from_utf8(&bytes).map_err(|_| ())?;
    Ok(Some(Zeroizing::new(text.to_owned())))
}

/// The bounded bytes of the file at `path` below `root`, refusing a link anywhere in between.
pub fn metadata_bytes(root: &Path, path: &Path) -> Result<Option<Zeroizing<Vec<u8>>>, ()> {
    let exists = std::fs::symlink_metadata(path);
    match exists {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
        Ok(info) if !info.is_file() || info.len() > MAX_CONFIG => return Err(()),
        Ok(_) => {}
    }
    check_components(root, path)?;
    use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
    use cap_primitives::fs::{FollowSymlinks, OpenOptions};
    // A directory opens as a directory on every host; `File::open` refuses one
    // on Windows, which would make every saved account unreadable there.
    let root_handle =
        cap_primitives::fs::open_ambient_dir(root, cap_primitives::ambient_authority())
            .map_err(|_| ())?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = cap_primitives::fs::open(
        &root_handle,
        path.strip_prefix(root).map_err(|_| ())?,
        &options,
    )
    .map_err(|_| ())?;
    if !file.metadata().map_err(|_| ())?.is_file() {
        return Err(());
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MAX_CONFIG + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() as u64 > MAX_CONFIG {
        return Err(());
    }
    Ok(Some(bytes))
}

fn check_components(root: &Path, path: &Path) -> Result<(), ()> {
    let relative = path.strip_prefix(root).map_err(|_| ())?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err(());
        }
        current.push(component);
        if std::fs::symlink_metadata(&current)
            .map_err(|_| ())?
            .is_symlink()
        {
            return Err(());
        }
    }
    Ok(())
}

fn profile(
    root: &Path,
    relative: &str,
    provider: Provider,
    label: &str,
    inventory: &mut Inventory,
) {
    let path = root.join(relative);
    match std::fs::symlink_metadata(&path) {
        Ok(info) if info.is_dir() && check_components(root, &path).is_ok() => {
            add(
                inventory,
                provider,
                if matches!(provider, Provider::Firefox | Provider::Chromium) {
                    Origin::Browser
                } else {
                    Origin::Application
                },
                label,
                None,
                None,
                &path,
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) | Err(_) => inventory.unreadable_sources.push("Application profile"),
    }
}

fn add(
    inventory: &mut Inventory,
    provider: Provider,
    origin: Origin,
    label: &str,
    identity: Option<String>,
    server: Option<String>,
    path: &Path,
) {
    inventory.candidates.push(Candidate {
        id: format!("account-source-{}", inventory.candidates.len()),
        provider,
        origin,
        label: label.chars().take(256).collect(),
        identity,
        server,
        profile: path.to_path_buf(),
        needs_sign_in: true,
        can_import_credentials: origin == Origin::Rclone
            && matches!(provider, Provider::WebDav | Provider::Nextcloud),
        services: Vec::new(),
    });
}

// Keep only allowlisted non-secret keys: foreign configs may include plaintext passwords, and
// their other fields never enter the review or a serializable value.
pub fn sections(text: &str, allowed: &[&str]) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut result: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = name.to_string();
        } else if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let leaf = key.rsplit('\\').next().unwrap_or(key);
            if allowed.contains(&leaf) && value.len() <= 2048 {
                result
                    .entry(section.clone())
                    .or_default()
                    .insert(key.into(), value.trim().into());
            }
        }
    }
    result
}

pub fn safe_server(raw: &str) -> Option<String> {
    let url = url::Url::parse(raw).ok()?;
    // Match the connector's canonical DAV policy: HTTPS, or HTTP only on loopback; discovery keeps
    // endpoints it can actually import.
    if account_credentials::nextcloud::server_url(raw).is_err()
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    Some(url.to_string())
}

fn gnome(text: &str, path: &Path, inventory: &mut Inventory) {
    for (section, config) in sections(
        text,
        &["Provider", "Identity", "PresentationIdentity", "Uri"],
    ) {
        if !section.starts_with("Account ") {
            continue;
        }
        let provider = match config.get("Provider").map(String::as_str) {
            Some("owncloud" | "nextcloud") => Provider::Nextcloud,
            Some("webdav") => Provider::WebDav,
            _ => continue,
        };
        let identity = config
            .get("PresentationIdentity")
            .or_else(|| config.get("Identity"))
            .cloned();
        let server = config.get("Uri").and_then(|value| safe_server(value));
        add(
            inventory,
            provider,
            Origin::Gnome,
            identity.as_deref().unwrap_or("Online account"),
            identity.clone(),
            server,
            path,
        );
    }
}

#[cfg(target_os = "linux")]
fn nextcloud(text: &str, path: &Path, inventory: &mut Inventory) {
    nextcloud::discover(text, path, inventory);
}
#[cfg(not(target_os = "linux"))]
fn nextcloud(text: &str, path: &Path, inventory: &mut Inventory) {
    let parsed = sections(
        text,
        &["url", "user", "http_user", "webflow_user", "displayName"],
    );
    let Some(accounts) = parsed.get("Accounts") else {
        return;
    };
    let mut groups: BTreeMap<&str, BTreeMap<&str, &str>> = BTreeMap::new();
    for (key, value) in accounts {
        if let Some((id, leaf)) = key.split_once('\\') {
            if !leaf.contains('\\') {
                groups.entry(id).or_default().insert(leaf, value);
            }
        }
    }
    for (_, config) in groups {
        let Some(server) = config.get("url").and_then(|raw| safe_server(raw)) else {
            continue;
        };
        let identity = config
            .get("webflow_user")
            .or_else(|| config.get("http_user"))
            .or_else(|| config.get("user"))
            .map(|s| (*s).to_string());
        let label = config
            .get("displayName")
            .copied()
            .or(identity.as_deref())
            .unwrap_or("Nextcloud");
        add(
            inventory,
            Provider::Nextcloud,
            Origin::Nextcloud,
            label,
            identity.clone(),
            Some(server),
            path,
        );
    }
}

pub fn rclone(text: &str, path: &Path, inventory: &mut Inventory) {
    if text.trim_start().starts_with("RCLONE_ENCRYPT_V0:") {
        inventory
            .unreadable_sources
            .push("Encrypted rclone accounts");
        return;
    }
    for (label, config) in sections(text, &["type", "url", "user", "vendor"]) {
        let provider = match config.get("type").map(String::as_str) {
            Some("webdav") if config.get("vendor").is_some_and(|s| s == "nextcloud") => {
                Provider::Nextcloud
            }
            Some("webdav") => Provider::WebDav,
            _ => continue,
        };
        add(
            inventory,
            provider,
            Origin::Rclone,
            &label,
            config.get("user").cloned(),
            config.get("url").and_then(|s| safe_server(s)),
            path,
        );
    }
}

#[cfg(test)]
mod server_policy_tests {
    use super::safe_server;

    tilo_test_env::scenario! {
        fn discovery_admits_only_canonical_dav_endpoints;

        phase discovery_preserves_canonical_loopback_dav_endpoints;
        {
            for server in [
                "https://files.example.test/dav/",
                "http://127.0.0.1:8080/dav/",
                "http://[::1]:8080/dav/",
                "http://localhost:8080/dav/",
            ] {
                assert_eq!(safe_server(server).as_deref(), Some(server));
            }
        }

        phase discovery_still_refuses_remote_http_and_secret_bearing_endpoints;
        {
            for server in [
                "http://files.example.test/dav/",
                "http://192.168.1.10/dav/",
                "http://127.0.0.1.example.test/dav/",
                "ftp://127.0.0.1/dav/",
                "http://ada@127.0.0.1/dav/",
                "http://ada:private@127.0.0.1/dav/", // Invalid credential-bearing URL example.
                "http://127.0.0.1/dav/?token=private",
                "http://127.0.0.1/dav/#private",
                "https://files.example.test/dav/?token=private",
            ] {
                assert!(safe_server(server).is_none());
            }
        }
    }
}
