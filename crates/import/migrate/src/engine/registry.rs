//! Every source the engine brings things from, in one table: its name, what it is, how it is
//! connected, which [`super::Source`] reads it and where History keeps what it brought. `sources`, the help text,
//! the names commands accept and `undo` all read it, so a new source is one row here and one
//! engine arm.

use anyhow::{Result, bail};

/// What reads a source and lands what it holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    /// A Thunderbird profile on this computer: mail, address books, calendars, accounts.
    Thunderbird,
    /// A service reached with a key or password (Outline, Linear, Forgejo, a mail account).
    Service,
    /// Folders on this computer, copied into the workspace's files.
    Folders,
    /// A cloud drive, copied into the workspace's files.
    Cloud,
    /// Browser profiles on this computer: bookmarks, history and saved passwords.
    Browsers,
    /// Accounts this computer is signed in to, connected as the workspace's accounts.
    Accounts,
    /// The settings and hidden files of this computer's home, copied into the workspace's files.
    Settings,
    /// The folders Windows names outside the home, copied into the workspace's files.
    Windows,
    /// The Flatpak apps on this computer, installed again through the Store.
    Flatpaks,
}

/// One source.
#[derive(Debug)]
pub struct Row {
    /// The name commands take (`tilo migrate start gdrive`).
    pub key: &'static str,
    /// Other names commands accept for it.
    pub aliases: &'static [&'static str],
    /// What it is, as `tilo migrate sources` says.
    pub title: &'static str,
    pub engine: Engine,
    /// The arguments `tilo migrate connect <key>` needs, when it is connected first.
    pub connect: Option<&'static str>,
    /// The resources its migrations change, where `undo` finds their History entries.
    pub resources: &'static [&'static str],
}

impl Row {
    /// The command that connects it, when it needs one.
    #[must_use]
    pub fn connect_command(&self) -> Option<String> {
        self.connect.map(|arguments| {
            if arguments.is_empty() {
                format!("tilo migrate connect {}", self.key)
            } else {
                format!("tilo migrate connect {} {arguments}", self.key)
            }
        })
    }
}

/// Files a migration puts into the workspace's tree go in the tree's History.
const FILES: &[&str] = &["files/files.tree"];

pub const SOURCES: &[Row] = &[
    Row {
        key: "home",
        aliases: &["folders", "files"],
        title: "Folders on this computer (Documents, Pictures, … or any folder)",
        engine: Engine::Folders,
        connect: None,
        resources: FILES,
    },
    Row {
        key: "browsers",
        aliases: &["browser", "firefox", "chrome", "chromium"],
        title: "Browsers on this computer: bookmarks, history and saved passwords",
        engine: Engine::Browsers,
        connect: None,
        resources: &[
            "browser/browser.personal-bookmarks",
            "browser/browser.browsing-history",
            "passwords/passwords.vault",
        ],
    },
    Row {
        key: "settings",
        aliases: &["dotfiles", "hidden"],
        title: "Settings and hidden files in this computer's home (.config, .ssh, app data, …)",
        engine: Engine::Settings,
        connect: None,
        resources: FILES,
    },
    Row {
        key: "windows",
        aliases: &["outlook", "steam"],
        title: "Folders Windows keeps: OneDrive's, Outlook archives, Steam game saves, moved Documents",
        engine: Engine::Windows,
        connect: None,
        resources: FILES,
    },
    Row {
        key: "accounts",
        aliases: &["signed-in", "rclone", "gnome-online-accounts"],
        title: "Accounts this computer is signed in to: GNOME Online Accounts, the Nextcloud app, rclone, Tilo, OneDrive",
        engine: Engine::Accounts,
        connect: None,
        resources: &[],
    },
    Row {
        key: "flatpaks",
        aliases: &["flatpak", "apps"],
        title: "Flatpak apps on this computer, installed again from Flathub",
        engine: Engine::Flatpaks,
        connect: None,
        resources: &["store"],
    },
    Row {
        key: "thunderbird",
        aliases: &[],
        title: "Thunderbird on this computer: mail, address books, calendars",
        engine: Engine::Thunderbird,
        connect: None,
        resources: &[
            "workspace/mail-accounts",
            "contacts",
            "calendar/calendar.calendar",
        ],
    },
    Row {
        key: "mail",
        aliases: &["imap"],
        title: "A mail account on its server (IMAP)",
        engine: Engine::Service,
        connect: Some("--login you@example.com"),
        resources: &["mail/mail.mailbox"],
    },
    Row {
        key: "gdrive",
        aliases: &["google-drive", "drive"],
        title: "Google Drive",
        engine: Engine::Cloud,
        connect: Some(""),
        resources: FILES,
    },
    Row {
        key: "onedrive",
        aliases: &[],
        title: "OneDrive",
        engine: Engine::Cloud,
        connect: Some(""),
        resources: FILES,
    },
    Row {
        key: "dropbox",
        aliases: &[],
        title: "Dropbox",
        engine: Engine::Cloud,
        connect: Some(""),
        resources: FILES,
    },
    Row {
        key: "icloud",
        aliases: &["icloud-drive"],
        title: "iCloud Drive",
        engine: Engine::Cloud,
        connect: Some("--login you@icloud.com"),
        resources: FILES,
    },
    Row {
        key: "nextcloud",
        aliases: &[],
        title: "Nextcloud",
        engine: Engine::Cloud,
        connect: Some("--url https://cloud.example.com --login you"),
        resources: FILES,
    },
    Row {
        key: "webdav",
        aliases: &["dav"],
        title: "A WebDAV server",
        engine: Engine::Cloud,
        connect: Some("--url https://dav.example.com/files --login you"),
        resources: FILES,
    },
    Row {
        key: "outline",
        aliases: &[],
        title: "Outline",
        engine: Engine::Service,
        connect: Some("--url https://your-team.getoutline.com"),
        resources: &["notes"],
    },
    Row {
        key: "linear",
        aliases: &[],
        title: "Linear",
        engine: Engine::Service,
        connect: Some(""),
        resources: &["tasks"],
    },
    Row {
        key: "forgejo",
        aliases: &["gitea"],
        title: "Forgejo (or Gitea) issues",
        engine: Engine::Service,
        connect: Some("--url https://forge.example/owner/repo"),
        resources: &["tasks"],
    },
];

/// The source `name` names, by its key or another name it goes by.
///
/// # Errors
/// No source goes by that name; the error lists them.
pub fn named(name: &str) -> Result<&'static Row> {
    let name = name.trim().to_ascii_lowercase();
    if let Some(source) = SOURCES
        .iter()
        .find(|source| source.key == name || source.aliases.contains(&name.as_str()))
    {
        return Ok(source);
    }
    bail!(
        "{name:?} is not a source; the sources are {} (`tilo migrate sources` lists them with how to connect each)",
        keys().join(", ")
    )
}

/// Every source's key, in the table's order.
#[must_use]
pub fn keys() -> Vec<&'static str> {
    SOURCES.iter().map(|source| source.key).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_a_source_goes_by_finds_it_once() {
        let mut seen = std::collections::BTreeSet::new();
        for source in SOURCES {
            for name in std::iter::once(&source.key).chain(source.aliases) {
                assert!(seen.insert(*name), "{name} names two sources");
                assert_eq!(named(name).unwrap().key, source.key);
            }
        }
        assert_eq!(named(" Google-Drive ").unwrap().key, "gdrive");
        let refused = named("myspace").unwrap_err().to_string();
        assert!(refused.contains("gdrive") && refused.contains("tilo migrate sources"));
    }
}
