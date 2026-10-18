//! Read-only inventory after migration consent. Credentials stay in their
//! source stores until a reviewed import or connection is executed.

use super::super::{account_transfer, flatpak_transfer, home_transfer};
use std::path::PathBuf;

#[derive(Default)]
pub(super) struct Belongings {
    pub windows: Option<super::super::windows_inventory::Inventory>,
    pub home: Option<PathBuf>,
    pub roots: Option<account_transfer::SourceRoots>,
    pub accounts: account_transfer::Inventory,
    pub flatpak: Option<flatpak_transfer::Inventory>,
    pub administrator_settings: Option<Vec<flatpak_transfer::AdministratorSettings>>,
    pub browser: super::browser::Selection,
    pub browser_unavailable: bool,
    pub sessions: super::service_sessions::Selection,
    pub sessions_unavailable: bool,
    pub home_unavailable: bool,
    pub flatpak_unavailable: bool,
}

impl Belongings {
    /// Windows inventory accepts only individually selected exact roots. This
    /// does not invoke host Home, Linux account, browser or Flatpak discovery.
    pub fn from_windows_roots(
        selected: &[super::super::windows_inventory::SelectedRoot],
    ) -> Result<Self, String> {
        let windows = super::super::windows_inventory::Inventory::discover(selected)?;
        let accounts = account_transfer::discover_windows_sources(&windows);
        Ok(Self {
            windows: Some(windows),
            accounts,
            ..Self::default()
        })
    }

    pub fn discover() -> Self {
        let Ok(home) = home_transfer::current_home() else {
            return Self {
                home_unavailable: true,
                flatpak_unavailable: true,
                ..Self::default()
            };
        };
        let config =
            std::env::var_os("XDG_CONFIG_HOME").map_or_else(|| home.join(".config"), PathBuf::from);
        Self::from_home_and_config(home, home_transfer::granted_root(&config))
    }

    fn from_home_and_config(home: PathBuf, config: std::io::Result<PathBuf>) -> Self {
        if let Ok(config) = config {
            Self::from_roots(account_transfer::SourceRoots { home, config })
        } else {
            let mut accounts = account_transfer::discover_home_profiles(&home);
            accounts
                .unreadable_sources
                .extend(unresolved_config().unreadable_sources);
            Self::from_inventory(home, None, accounts)
        }
    }

    pub fn from_roots(roots: account_transfer::SourceRoots) -> Self {
        let accounts = account_transfer::discover(&roots);
        Self::from_inventory(roots.home.clone(), Some(roots), accounts)
    }

    fn from_inventory(
        home: PathBuf,
        roots: Option<account_transfer::SourceRoots>,
        accounts: account_transfer::Inventory,
    ) -> Self {
        let flatpak = flatpak_transfer::discover(&flatpak_transfer::NativeRunner, &home);
        let flatpak_unavailable = flatpak.is_err();
        let administrator_settings = flatpak
            .as_ref()
            .ok()
            .and_then(|inventory| flatpak_transfer::administrator_settings(&home, inventory));
        let browser = super::browser::Selection::discover(&home, &accounts.candidates);
        let sessions = imager_backend::service_selection::supported()
            .then(|| super::service_sessions::Selection::discover(&home, &accounts.candidates));
        Self {
            windows: None,
            sessions_unavailable: sessions.as_ref().is_some_and(|result| {
                result
                    .as_ref()
                    .map_or(true, |selection| selection.unavailable())
            }),
            sessions: sessions.and_then(Result::ok).unwrap_or_default(),
            home: Some(home),
            roots,
            accounts,
            administrator_settings,
            flatpak: flatpak.ok(),
            browser_unavailable: browser.is_err(),
            browser: browser.unwrap_or_default(),
            home_unavailable: false,
            flatpak_unavailable,
        }
    }

    /// Discovery is metadata, not proof of a working login or a service grant.
    /// Keep identifying labels in the account review, not the overview.
    pub(super) fn accounts_overview_hint(&self) -> String {
        use super::super::strings::{t, t_args};
        use account_transfer::Provider;

        let providers = self
            .accounts
            .candidates
            .iter()
            .filter(|candidate| {
                candidate.can_import_credentials
                    || super::accounts::can_connect_candidate(candidate)
            })
            .map(|candidate| match candidate.provider {
                Provider::Google => "installer-accounts-google-drive",
                Provider::Microsoft => "installer-accounts-onedrive",
                Provider::Dropbox => "installer-accounts-dropbox",
                Provider::Nextcloud => "installer-accounts-nextcloud",
                Provider::WebDav => "installer-accounts-webdav",
                Provider::Spotify => "installer-accounts-spotify",
                Provider::Slack => "installer-service-slack-label",
                Provider::Discord => "installer-service-discord-label",
                Provider::Firefox => "installer-belongings-provider-firefox",
                Provider::Chromium => "installer-belongings-provider-chromium",
            })
            .chain(self.accounts.tilo_accounts.iter().map(|candidate| {
                match candidate.type_id.as_str() {
                    "google-drive" => "installer-accounts-google-drive",
                    "onedrive" => "installer-accounts-onedrive",
                    "dropbox" => "installer-accounts-dropbox",
                    "icloud-drive" => "installer-accounts-icloud",
                    "nextcloud" => "installer-accounts-nextcloud",
                    "webdav" => "installer-accounts-webdav",
                    "spotify" => "installer-accounts-spotify",
                    _ => "installer-belongings-provider-other",
                }
            }));
        let mut counts = std::collections::BTreeMap::<String, i64>::new();
        for provider in providers {
            *counts.entry(t(provider)).or_default() += 1;
        }
        if counts.is_empty() {
            return t("installer-preview-accounts-body");
        }
        counts
            .into_iter()
            .map(|(provider, count)| {
                t_args(
                    "installer-belongings-connections-found",
                    &[("provider", provider.into()), ("count", count.into())],
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub(super) fn warning_keys(&self) -> Vec<&'static str> {
        let mut warnings = Vec::new();
        if self.home_unavailable {
            warnings.push("installer-belongings-files-unchecked");
        }
        if self.flatpak_unavailable {
            warnings.push("installer-belongings-apps-unchecked");
        }
        if self.sessions_unavailable {
            warnings.push("installer-service-sources-unavailable");
        }
        if self.browser_unavailable {
            warnings.push("installer-belongings-browser-unchecked");
        }
        if !self.accounts.unreadable_sources.is_empty() {
            warnings.push("installer-belongings-accounts-unchecked");
        }
        warnings
    }

    pub fn report(&self) -> serde_json::Value {
        let sessions = if imager_backend::service_selection::supported() {
            self.sessions.report()
        } else {
            serde_json::Value::Null
        };
        serde_json::json!({
            "windows": self.windows,
            "windows_budget": self.windows.as_ref().map(|inventory| inventory.budget()),
            "windows_recovery": self.windows.as_ref().map(super::super::migration_recovery::RecoveryBudget::windows),
            "website_sessions":sessions,
            "home": self.home,
            "home_unavailable": self.home_unavailable,
            "browser_unavailable": self.browser_unavailable,
            "accounts": self.accounts,
            "flatpak": self.flatpak,
            "administrator_settings": self.administrator_settings,
            "flatpak_unavailable": self.flatpak_unavailable,
        })
    }
}

fn unresolved_config() -> account_transfer::Inventory {
    account_transfer::Inventory {
        unreadable_sources: vec!["Account configuration root"],
        ..account_transfer::Inventory::default()
    }
}

#[cfg(test)]
#[path = "belongings_overview_tests.rs"]
mod overview_tests;

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn refused_external_config_keeps_owned_home_firefox_inventory_and_warning() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let home_path = fixture.path().join("home");
        let profile = home_path.join(".mozilla/firefox/personal.default");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::set_permissions(&home_path, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(
            home_path.join(".mozilla/firefox/profiles.ini"),
            "[Profile0]\nName=Personal\nIsRelative=1\nPath=personal.default\n",
        )
        .unwrap();
        let places = rusqlite::Connection::open(profile.join("places.sqlite")).unwrap();
        places.execute_batch("CREATE TABLE moz_places(id INTEGER PRIMARY KEY,url TEXT); INSERT INTO moz_places VALUES(1,'https://example.invalid/owned');").unwrap();
        drop(places);
        let cookies = rusqlite::Connection::open(profile.join("cookies.sqlite")).unwrap();
        cookies.execute_batch("PRAGMA user_version=17;
            CREATE TABLE moz_cookies(id INTEGER PRIMARY KEY,host TEXT,name TEXT,value TEXT,path TEXT,
            expiry INTEGER,isSecure INTEGER,isHttpOnly INTEGER,sameSite INTEGER,
            originAttributes TEXT,isPartitionedAttributeSet INTEGER,inBrowserElement INTEGER);
            INSERT INTO moz_cookies VALUES(1,'.slack.com','d','private-home-cookie-sentinel','/',2000000000000,1,1,256,'',0,0);").unwrap();
        drop(cookies);
        // An actual invalid external root is refused by production admission.
        let external = fixture.path().join("external-config");
        std::fs::write(&external, b"external-private-config-sentinel").unwrap();
        // An otherwise valid Home/.config connection is NOT a substitute grant.
        let fallback = home_path.join(".config/Nextcloud/nextcloud.cfg");
        std::fs::create_dir_all(fallback.parent().unwrap()).unwrap();
        std::fs::write(
            &fallback,
            "[Accounts]\n0\\url=https://cloud.example\n0\\user=ungranted\n",
        )
        .unwrap();
        let before_places = std::fs::read(profile.join("places.sqlite")).unwrap();
        let before_cookies = std::fs::read(profile.join("cookies.sqlite")).unwrap();
        let home = home_transfer::granted_root(&home_path).unwrap();
        let refused = home_transfer::granted_root(&external);
        assert!(refused.is_err());
        let belongings = Belongings::from_home_and_config(home.clone(), refused);
        assert_eq!(belongings.home.as_ref(), Some(&home));
        assert!(
            belongings.roots.is_none(),
            "refused config never becomes source authority"
        );
        assert!(
            belongings
                .accounts
                .candidates
                .iter()
                .all(|c| c.provider == account_transfer::Provider::Firefox)
        );
        assert_eq!(belongings.accounts.candidates.len(), 1);
        assert!(
            belongings
                .warning_keys()
                .contains(&"installer-belongings-accounts-unchecked")
        );
        assert!(!belongings.browser_unavailable);
        assert!(!belongings.sessions_unavailable);
        assert_eq!(belongings.browser.selected().profiles().len(), 1);
        let sessions = belongings.sessions.report();
        assert_eq!(sessions["sources"].as_array().unwrap().len(), 1);
        assert_eq!(sessions["selected"].as_array().unwrap().len(), 1);
        assert!(
            !sessions
                .to_string()
                .contains("private-home-cookie-sentinel")
        );
        assert!(
            !sessions
                .to_string()
                .contains(&home.to_string_lossy().to_string())
        );
        assert_eq!(
            std::fs::read(profile.join("places.sqlite")).unwrap(),
            before_places
        );
        assert_eq!(
            std::fs::read(profile.join("cookies.sqlite")).unwrap(),
            before_cookies
        );
        assert_eq!(
            std::fs::read(&external).unwrap(),
            b"external-private-config-sentinel"
        );
        assert!(
            std::fs::read_to_string(fallback)
                .unwrap()
                .contains("ungranted")
        );
    }

    #[test]
    fn explicitly_granted_config_alias_discovers_accounts_without_following_members() {
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path().canonicalize().unwrap();
        let home = base.join("actual-home");
        let config = base.join("actual-config");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir_all(config.join("Nextcloud")).unwrap();
        symlink(&config, base.join("chosen-config")).unwrap();
        std::fs::write(
            config.join("Nextcloud/nextcloud.cfg"),
            "[Accounts]\n0\\url=https://cloud.example\n0\\user=fixture\n",
        )
        .unwrap();
        let roots = account_transfer::SourceRoots {
            home,
            config: home_transfer::granted_root(&base.join("chosen-config")).unwrap(),
        };
        let inventory = account_transfer::discover(&roots);
        assert!(
            inventory
                .candidates
                .iter()
                .any(|candidate| candidate.provider == account_transfer::Provider::Nextcloud)
        );
        std::fs::rename(config.join("Nextcloud"), base.join("external")).unwrap();
        symlink(base.join("external"), config.join("Nextcloud")).unwrap();
        let inventory = account_transfer::discover(&roots);
        assert!(inventory.candidates.is_empty());
        assert!(inventory.unreadable_sources.contains(&"Nextcloud"));
        assert!(home_transfer::granted_root(std::path::Path::new("relative-config")).is_err());
        assert!(!unresolved_config().unreadable_sources.is_empty());
    }
}
