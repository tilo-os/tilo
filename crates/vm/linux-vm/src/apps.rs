//! Installed Linux apps, as the host knows them: what the guest reports after
//! every install or removal, recorded in `apps.json`. Each app gets a desktop entry
//! the shell's search finds and a command name whose shim asks the broker for
//! `LinuxVmLaunch`.

use std::path::{Path, PathBuf};

use linux_vm_protocol::GuestOs;
pub use linux_vm_protocol::InstalledApp;
use linux_vm_protocol::window::host_app_id;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LinuxVmApp {
    pub guest: GuestOs,
    pub id: String,
    pub name: String,
    pub command: String,
    pub window_class: Option<String>,
    pub comment: Option<String>,
    pub terminal: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub apps: Vec<LinuxVmApp>,
}

impl Registry {
    #[must_use]
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        tilo_files::native::atomic::write_creating_parent(path, &serde_json::to_vec_pretty(self)?)
    }

    /// Replace the record with what the guest reports, keeping command
    /// names unique and away from `reserved` names.
    #[must_use]
    pub fn from_guest(guest: GuestOs, installed: &[InstalledApp], reserved: &[&str]) -> Self {
        let mut apps: Vec<LinuxVmApp> = Vec::new();
        for app in installed {
            if !valid_app_id(&app.id) {
                continue;
            }
            let base = command_name(&app.id);
            let mut command = base.clone();
            let mut suffix = 2_u32;
            while reserved.contains(&command.as_str())
                || apps.iter().any(|other| other.command == command)
            {
                command = format!("{base}-{suffix}");
                suffix = suffix.saturating_add(1);
            }
            apps.push(LinuxVmApp {
                guest,
                id: app.id.clone(),
                name: app.name.clone(),
                command,
                window_class: app.window_class.clone(),
                comment: app.comment.clone(),
                terminal: app.terminal,
            });
        }
        Self { apps }
    }

    /// The app a launch names, by id or command.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&LinuxVmApp> {
        self.apps
            .iter()
            .find(|app| app.id == name)
            .or_else(|| self.apps.iter().find(|app| app.command == name))
    }
}

/// Reverse-DNS-ish ids only: they become file names and command words.
///
/// Differs from [`tilo_app_id::AppId::parse`] by allowing a single segment
/// without a dot (Linux desktop-file names such as `firefox`, `ush` command
/// words such as `code`, and Flatpak ref parts such as `x86_64` may have no
/// dot) and forbidding a leading `-` so an id cannot be parsed as a flag when
/// placed in `Exec=tilo-linux-vm launch {id}` or exposed as a command word.
#[must_use]
pub fn valid_app_id(id: &str) -> bool {
    const GRAMMAR: tilo_app_id::Grammar = tilo_app_id::Grammar::CANONICAL
        .allow_single_segment()
        .forbid_leading_hyphen();
    GRAMMAR.matches(id)
}

#[must_use]
pub fn command_name(id: &str) -> String {
    let last = id.rsplit('.').next().unwrap_or(id);
    let name: String = last
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    if name.is_empty() {
        "app".to_string()
    } else {
        name
    }
}

impl LinuxVmApp {
    /// The app id its windows arrive with on the host.
    #[must_use]
    pub fn host_window_class(&self) -> String {
        host_app_id(
            self.guest,
            Some(self.window_class.as_deref().unwrap_or(&self.id)),
        )
    }

    #[must_use]
    pub fn desktop_file_name(&self) -> String {
        format!("tilo-linux-vm-{}-{}.desktop", self.guest.as_str(), self.id)
    }

    #[must_use]
    pub fn desktop_entry(&self) -> String {
        let comment = self
            .comment
            .clone()
            .unwrap_or_else(|| format!("{} app", self.guest.label()));
        format!(
            "[Desktop Entry]\n\
Type=Application\n\
Name={name} ({label})\n\
Comment={comment}\n\
Exec=tilo-linux-vm launch {id} %F\n\
TryExec=tilo-linux-vm\n\
Icon=application-x-executable\n\
Terminal={terminal}\n\
StartupWMClass={class}\n\
X-Tilo-LinuxVm={guest}\n",
            name = single_line(&self.name),
            label = self.guest.label(),
            comment = single_line(&comment),
            id = self.id,
            terminal = self.terminal,
            class = self.host_window_class(),
            guest = self.guest.as_str(),
        )
    }
}

fn single_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

pub fn sync_desktop_entries(
    dir: &Path,
    guest: GuestOs,
    registry: &Registry,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let prefix = format!("tilo-linux-vm-{}-", guest.as_str());
    let wanted: Vec<(String, String)> = registry
        .apps
        .iter()
        .filter(|app| app.guest == guest)
        .map(|app| (app.desktop_file_name(), app.desktop_entry()))
        .collect();
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&prefix)
            && name.ends_with(".desktop")
            && !wanted.iter().any(|(file, _)| *file == name)
        {
            std::fs::remove_file(entry.path())?;
        }
    }
    for (file, contents) in wanted {
        tilo_files::native::atomic::write_creating_parent(&dir.join(file), contents.as_bytes())?;
    }
    Ok(())
}

#[must_use]
pub fn applications_dir() -> Option<PathBuf> {
    Some(tilo_paths::xdg::data_home()?.join("applications"))
}
