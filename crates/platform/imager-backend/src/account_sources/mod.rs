//! Bounded metadata discovery beneath explicitly consented source roots.

#[cfg(target_os = "linux")]
mod goa;
#[cfg(target_os = "linux")]
mod nextcloud;
#[cfg(test)]
mod tests;
mod tilo;

#[cfg(target_os = "linux")]
pub use goa::{Definition as GoaDefinition, definitions as goa_definitions};
#[cfg(target_os = "linux")]
pub use nextcloud::{
    NativeAccount as NextcloudAccount, accounts as nextcloud_accounts,
    discover as discover_nextcloud, source_id as nextcloud_source_id,
};
pub use tilo::{TiloCandidate, fingerprint};

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Serialize;
use zeroize::Zeroizing;

/// Explicit roots granted by the person; no implicit environment probing.
#[derive(Clone, Debug)]
pub struct SourceRoots {
    pub home: PathBuf,
    pub config: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    Google,
    Microsoft,
    Dropbox,
    Nextcloud,
    WebDav,
    Spotify,
    Slack,
    Discord,
    Firefox,
    Chromium,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    Gnome,
    Nextcloud,
    Rclone,
    Application,
    Browser,
}

/// Metadata only, safe for the local review. Presence means a configured
/// account or profile was found; it does not establish a valid login.
#[derive(Clone, Debug, Serialize)]
pub struct Candidate {
    pub id: String,
    pub provider: Provider,
    pub origin: Origin,
    pub label: String,
    pub identity: Option<String>,
    pub server: Option<String>,
    /// Source path for the private home/profile transfer, never a cookie value.
    pub profile: PathBuf,
    pub needs_sign_in: bool,
    /// Supports selected credential extraction; discovery alone does not verify a login.
    pub can_import_credentials: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Inventory {
    pub candidates: Vec<Candidate>,
    pub tilo_accounts: Vec<TiloCandidate>,
    /// Existing sources that could not be examined; do not silently call an
    /// incomplete scan complete. Paths are not included in diagnostic strings.
    pub unreadable_sources: Vec<&'static str>,
}

const MAX_CONFIG: u64 = 2 * 1024 * 1024;

/// Read only recognized metadata from roots the person chose to migrate.
/// No network access, application launch, keyring access or writes.
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
        ("chromium", Provider::Chromium, "Chromium"),
        ("google-chrome", Provider::Chromium, "Google Chrome"),
        ("BraveSoftware/Brave-Browser", Provider::Chromium, "Brave"),
    ] {
        profile(&roots.config, relative, provider, label, &mut inventory);
    }
    append_home_profiles(&roots.home, &mut inventory);
    inventory
}

/// Independent recognized profiles beneath the already admitted Home root.
/// This never reads a config root or substitutes Home/.config for a refused one.
pub fn discover_home_profiles(home: &Path) -> Inventory {
    let mut inventory = Inventory::default();
    append_home_profiles(home, &mut inventory);
    inventory
}

fn append_home_profiles(home: &Path, inventory: &mut Inventory) {
    profile(
        home,
        ".mozilla/firefox",
        Provider::Firefox,
        "Firefox",
        inventory,
    );
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
        (
            "org.mozilla.firefox",
            ".mozilla/firefox",
            Provider::Firefox,
            "Firefox",
        ),
        (
            "org.chromium.Chromium",
            "config/chromium",
            Provider::Chromium,
            "Chromium",
        ),
        (
            "com.google.Chrome",
            "config/google-chrome",
            Provider::Chromium,
            "Google Chrome",
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
    let root_handle = std::fs::File::open(root).map_err(|_| ())?;
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
    let mut text = Zeroizing::new(String::new());
    file.take(MAX_CONFIG + 1)
        .read_to_string(&mut text)
        .map_err(|_| ())?;
    if text.len() as u64 > MAX_CONFIG {
        return Err(());
    }
    Ok(Some(text))
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
    });
}

// Keep only allowlisted non-secret keys. Foreign configs may include plaintext
// passwords; their other fields never enter the review or a serializable value.
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
    // Match the connector's canonical DAV transport policy: HTTPS, or HTTP
    // only on loopback. Discovery must retain endpoints it can actually import.
    if account_custody::nextcloud::server_url(raw).is_err()
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
            Some("google") => Provider::Google,
            Some("ms_graph" | "windows_live" | "microsoft") => Provider::Microsoft,
            Some("owncloud" | "nextcloud") => Provider::Nextcloud,
            Some("webdav") => Provider::WebDav,
            Some("dropbox") => Provider::Dropbox,
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
    #[cfg(target_os = "linux")]
    goa::discover(text, path, inventory);
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
    for (label, config) in sections(
        text,
        &[
            "type",
            "url",
            "user",
            "vendor",
            "client_id",
            "auth_url",
            "token_url",
            "client_credentials",
            "service_account_file",
            "service_account_credentials",
            "impersonate",
            "tenant_url",
            "team_member",
            "resource_key",
            "region",
            "tenant",
            "root_folder_id",
            "team_drive",
            "drive_id",
            "scope",
        ],
    ) {
        let provider = match config.get("type").map(String::as_str) {
            Some("drive") => Provider::Google,
            Some("onedrive") => Provider::Microsoft,
            Some("dropbox") => Provider::Dropbox,
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
        #[cfg(unix)]
        if matches!(
            provider,
            Provider::Google | Provider::Microsoft | Provider::Dropbox
        ) {
            let Some(candidate) = inventory.candidates.last_mut() else {
                continue;
            };
            use sha2::{Digest, Sha256};
            let definition = serde_json::to_vec(&config).unwrap_or_default();
            let mut digest = Sha256::new();
            digest.update(label.as_bytes());
            digest.update(&definition);
            candidate.id = format!("rclone-oauth-source-{}", hex::encode(digest.finalize()));
            candidate.can_import_credentials = cfg!(unix) && rclone_supported(&config);
        }
    }
}

pub fn rclone_supported(values: &BTreeMap<String, String>) -> bool {
    let empty = |key| values.get(key).is_none_or(String::is_empty);
    [
        "auth_url",
        "token_url",
        "service_account_file",
        "service_account_credentials",
        "impersonate",
        "tenant_url",
        "team_member",
        "resource_key",
    ]
    .into_iter()
    .all(empty)
        && values
            .get("client_credentials")
            .is_none_or(|s| s.is_empty() || s == "false")
        && values
            .get("region")
            .is_none_or(|s| s.is_empty() || s == "global")
        && values
            .get("tenant")
            .is_none_or(|s| s.is_empty() || s == "common")
        && values
            .get("scope")
            .is_none_or(|s| s.is_empty() || matches!(s.as_str(), "drive" | "drive.readonly"))
}

#[cfg(test)]
mod server_policy_tests {
    use super::safe_server;

    #[test]
    fn discovery_preserves_canonical_loopback_dav_endpoints() {
        for server in [
            "https://files.example.test/dav/",
            "http://127.0.0.1:8080/dav/",
            "http://[::1]:8080/dav/",
            "http://localhost:8080/dav/",
        ] {
            assert_eq!(safe_server(server).as_deref(), Some(server));
        }
    }

    #[test]
    fn discovery_still_refuses_remote_http_and_secret_bearing_endpoints() {
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
