//! The system Flatpak installation: what a device's daemon installs the Flatpak apps its
//! workspaces run into, and what it reads of it. A Flatpak app a workspace's tree names is
//! installed for the device, not for one person, and every person who uses the device runs that
//! one copy; its permissions are Flatpak's own, shown beside it, and the workspace's ordinary
//! grants are what decide who may name it.
//!
//! `TILO_FLATPAK` names the program (`flatpak` unless a test names its own), so a test drives a
//! scratch installation and never the host's.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

/// The most bytes a flatpak command's answer is kept to: a catalogue of one remote is the
/// largest answer, and one that runs past this is cut rather than held whole.
const MAX_OUTPUT: usize = 8 * 1024 * 1024;

/// An app installed in the device's system Flatpak installation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    /// The full reference (`app/org.example.App/x86_64/stable`).
    pub reference: String,
    /// The application id (`org.example.App`).
    pub app: String,
    pub arch: String,
    pub branch: String,
    /// The origin remote the app came from.
    pub origin: String,
    pub version: String,
    /// The permissions Flatpak gives it, as `flatpak info` writes them; empty when unread.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub permissions: String,
}

/// A remote the device's system installation reads from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remote {
    pub name: String,
    pub title: String,
    pub url: String,
    pub disabled: bool,
}

/// What the device's Flatpak installation has now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// Whether this device has Flatpak at all. A device without it keeps the record all the same
    /// and runs it on another of the person's devices.
    pub available: bool,
    pub installed: Vec<Installed>,
    pub remotes: Vec<Remote>,
}

/// The Flatpak program a device acts through.
#[derive(Clone, Debug)]
pub struct Flatpak {
    program: PathBuf,
}

impl Default for Flatpak {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Flatpak {
    /// The Flatpak this device runs, `flatpak` unless `TILO_FLATPAK` names another.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            program: std::env::var_os("TILO_FLATPAK")
                .map_or_else(|| PathBuf::from("flatpak"), PathBuf::from),
        }
    }

    /// The Flatpak program at `program`.
    #[must_use]
    pub fn at(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }

    /// Whether this device has Flatpak.
    #[must_use]
    pub fn available(&self) -> bool {
        self.run(&["--version"]).is_ok()
    }

    /// What the system installation has now: the apps, the remotes, and whether Flatpak is here.
    ///
    /// # Errors
    /// Flatpak is here but its installation could not be read.
    pub fn status(&self) -> Result<Status, String> {
        if !self.available() {
            return Ok(Status::default());
        }
        let mut installed = self.installed()?;
        for app in &mut installed {
            app.permissions = self.permissions(&app.app).unwrap_or_default();
        }
        Ok(Status {
            available: true,
            installed,
            remotes: self.remotes()?,
        })
    }

    /// The apps in the system installation.
    ///
    /// # Errors
    /// Flatpak could not list them.
    pub fn installed(&self) -> Result<Vec<Installed>, String> {
        let rows = self.run(&[
            "list",
            "--system",
            "--app",
            "--columns=application,branch,arch,origin,version",
        ])?;
        Ok(rows
            .lines()
            .filter_map(|line| {
                let mut columns = line.split('\t');
                let app = columns.next()?.trim().to_owned();
                let branch = columns.next().unwrap_or_default().trim().to_owned();
                let arch = columns.next().unwrap_or_default().trim().to_owned();
                let origin = columns.next().unwrap_or_default().trim().to_owned();
                let version = columns.next().unwrap_or_default().trim().to_owned();
                if app.is_empty() || branch.is_empty() || arch.is_empty() {
                    return None;
                }
                Some(Installed {
                    reference: format!("app/{app}/{arch}/{branch}"),
                    app,
                    arch,
                    branch,
                    origin,
                    version,
                    permissions: String::new(),
                })
            })
            .collect())
    }

    /// The remotes of the system installation.
    ///
    /// # Errors
    /// Flatpak could not list them.
    pub fn remotes(&self) -> Result<Vec<Remote>, String> {
        let rows = self.run(&["remotes", "--system", "--columns=name,url,title,options"])?;
        Ok(rows
            .lines()
            .filter_map(|line| {
                let mut columns = line.split('\t');
                let name = columns.next()?.trim().to_owned();
                if name.is_empty() {
                    return None;
                }
                let url = columns.next().unwrap_or_default().trim().to_owned();
                let title = columns.next().unwrap_or_default().trim().to_owned();
                let options = columns.next().unwrap_or_default().trim().to_owned();
                Some(Remote {
                    name,
                    title,
                    url,
                    disabled: options.split(',').any(|option| option.trim() == "disabled"),
                })
            })
            .collect())
    }

    /// The permissions Flatpak gives `app`, as `flatpak info` writes them.
    ///
    /// # Errors
    /// Flatpak could not read them.
    pub fn permissions(&self, app: &str) -> Result<String, String> {
        require_selector(app)?;
        self.run(&["info", "--system", "--show-permissions", "--", app])
    }

    /// The applications `remote` offers, one tab-separated row each: application, name,
    /// description, version, branch, origin.
    ///
    /// # Errors
    /// Flatpak could not read the remote's catalogue.
    pub fn catalogue(&self, remote: &str) -> Result<String, String> {
        require_selector(remote)?;
        let columns = "--columns=application,name,description,version,branch,origin";
        let args = [
            "remote-ls",
            "--system",
            "--app",
            "--arch",
            std::env::consts::ARCH,
            columns,
            "--",
            remote,
        ];
        let mut cached = args.to_vec();
        cached.insert(1, "--cached");
        match self.run(&cached) {
            Ok(rows) => Ok(rows),
            // A remote configured a moment ago may have no cached summary yet: reading it
            // fetches and verifies one with the remote's configured GPG policy.
            Err(error) if error.contains("No cached summary") => self.run(&args),
            Err(error) => Err(error),
        }
    }

    /// Install `reference` (`app/org.example.App/x86_64/stable`) into the system installation.
    ///
    /// # Errors
    /// Flatpak refused or failed the install.
    pub fn install(&self, reference: &str) -> Result<(), String> {
        require_reference(reference)?;
        self.run(&["install", "--system", "--noninteractive", "--", reference])
            .map(drop)
    }

    /// Install `app`'s `branch` from `remote` into the system installation.
    ///
    /// # Errors
    /// Flatpak refused or failed the install.
    pub fn install_from(&self, remote: &str, app: &str, branch: &str) -> Result<(), String> {
        require_selector(remote)?;
        require_selector(app)?;
        require_selector(branch)?;
        self.run(&[
            "install",
            "--system",
            "--noninteractive",
            "--",
            remote,
            &format!("{app}//{branch}"),
        ])
        .map(drop)
    }

    /// Remove `app` from the system installation, then the runtimes no app there uses any more.
    ///
    /// # Errors
    /// Flatpak refused or failed the removal.
    pub fn remove(&self, app: &str) -> Result<(), String> {
        require_selector(app)?;
        self.run(&["uninstall", "--system", "--noninteractive", "--", app])
            .map(drop)?;
        if let Err(error) = self.run(&["uninstall", "--system", "--unused", "--noninteractive"]) {
            tracing::warn!(%app, %error, "could not remove runtimes no Flatpak app uses");
        }
        Ok(())
    }

    /// Add the remote `name` at `url` to the system installation, unless it is there.
    ///
    /// # Errors
    /// Flatpak refused or failed the change.
    pub fn remote_add(&self, name: &str, url: &str) -> Result<(), String> {
        require_selector(name)?;
        if url.is_empty() || url.starts_with('-') || url.chars().any(char::is_control) {
            return Err(format!("{url:?} is no remote address"));
        }
        self.run(&["remote-add", "--system", "--if-not-exists", "--", name, url])
            .map(drop)
    }

    /// Add Flathub as the system installation's remote, if it is not there.
    ///
    /// # Errors
    /// Flatpak refused or failed the change.
    pub fn flathub_add(&self) -> Result<(), String> {
        self.remote_add("flathub", "https://dl.flathub.org/repo/flathub.flatpakrepo")
    }

    /// Enable or disable the remote `name`.
    ///
    /// # Errors
    /// Flatpak refused or failed the change.
    pub fn remote_set_enabled(&self, name: &str, enabled: bool) -> Result<(), String> {
        require_selector(name)?;
        self.run(&[
            "remote-modify",
            "--system",
            if enabled { "--enable" } else { "--disable" },
            "--",
            name,
        ])
        .map(drop)
    }

    /// Remove the remote `name` from the system installation.
    ///
    /// # Errors
    /// Flatpak refused or failed the change.
    pub fn remote_remove(&self, name: &str) -> Result<(), String> {
        require_selector(name)?;
        self.run(&["remote-delete", "--system", "--force", "--", name])
            .map(drop)
    }

    /// Run the program with `args`, answering its standard output.
    fn run(&self, args: &[&str]) -> Result<String, String> {
        let subcommand = args.first().copied().unwrap_or_default();
        let output = Command::new(&self.program)
            .args(args)
            .env("LC_ALL", "C")
            .output()
            .map_err(|error| format!("{} {subcommand}: {error}", self.program.display()))?;
        if output.status.success() {
            let mut stdout = output.stdout;
            stdout.truncate(MAX_OUTPUT);
            Ok(String::from_utf8_lossy(&stdout).into_owned())
        } else {
            Err(format!(
                "{} {subcommand}: {}",
                self.program.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }
}

/// Whether `value` reads as one Flatpak selector, never an option: a remote, application or
/// branch name.
fn require_selector(value: &str) -> Result<(), String> {
    if !value.is_empty()
        && value.len() <= 255
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        Ok(())
    } else {
        Err(format!(
            "{value:?} is not a Flatpak repository or application"
        ))
    }
}

/// Whether `reference` reads as one Flatpak reference (`app/id/arch/branch`).
fn require_reference(reference: &str) -> Result<(), String> {
    let mut parts = reference.split('/');
    let (kind, id, arch, branch) = (parts.next(), parts.next(), parts.next(), parts.next());
    if parts.next().is_none()
        && kind == Some("app")
        && id.is_some_and(|v| require_selector(v).is_ok())
        && arch.is_some_and(|v| require_selector(v).is_ok())
        && branch.is_some_and(|v| require_selector(v).is_ok())
    {
        Ok(())
    } else {
        Err(format!(
            "{reference:?} is not a Flatpak reference `app/<application>/<arch>/<branch>`"
        ))
    }
}

/// The directory a test's Flatpak program keeps its installation in: `TILO_FLATPAK_TEST_ROOT`
/// when a test names one, else none.
#[must_use]
pub fn test_root() -> Option<PathBuf> {
    std::env::var_os("TILO_FLATPAK_TEST_ROOT").map(PathBuf::from)
}

/// What the reading `id` answers with `arguments`, when it is one of the device's package cache
/// serves ([`crate::readings`]).
///
/// # Errors
/// The reading failed, as the person is told.
#[must_use]
pub fn reading(
    id: &str,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Option<Result<serde_json::Value, String>> {
    let flatpak = Flatpak::from_env();
    let answer = match id {
        "flatpak" => flatpak
            .status()
            .and_then(|status| serde_json::to_value(status).map_err(|error| error.to_string())),
        "flatpak-catalogue" => {
            let remote = arguments.get("name").and_then(serde_json::Value::as_str);
            match remote {
                Some(remote) => flatpak
                    .catalogue(remote)
                    .and_then(|rows| Ok(serde_json::json!({ "rows": rows }))),
                None => Err("`name` names the repository to read".into()),
            }
        }
        _ => return None,
    };
    Some(answer)
}

/// A state file the daemon keeps of the Flatpaks it installed, by the reference it installed.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Kept {
    #[serde(default)]
    pub references: Vec<String>,
}

impl Kept {
    /// Read the state at `path`; a missing file is an empty state.
    ///
    /// # Errors
    /// The file is there but does not decode.
    pub fn read(path: &Path) -> Result<Self, String> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| format!("{}: {error}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("{}: {error}", path.display())),
        }
    }

    /// Keep this state at `path`.
    ///
    /// # Errors
    /// The file could not be written.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        write_atomic(path, &bytes)
    }
}

/// Write `bytes` at `path` in one rename, so a reader sees the old file or the new.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let staging = path.with_extension("staging");
    std::fs::write(&staging, bytes).map_err(|error| format!("{}: {error}", staging.display()))?;
    std::fs::rename(&staging, path).map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_reference_names_an_application_and_a_selector_never_an_option() {
        assert!(require_reference("app/org.gnome.Calculator/x86_64/stable").is_ok());
        assert!(require_reference("runtime/org.freedesktop.Platform/x86_64/24.08").is_err());
        assert!(require_reference("app/org.gnome.Calculator/x86_64").is_err());
        assert!(require_reference("app/-oops/x86_64/stable").is_err());
        assert!(require_selector("flathub").is_ok());
        assert!(require_selector("--user").is_err());
        assert!(require_selector("a/b").is_err());
    }

    #[test]
    fn the_state_reads_back_as_it_was_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flatpaks.json");
        assert_eq!(Kept::read(&path).unwrap().references, Vec::<String>::new());
        Kept {
            references: vec!["app/org.gnome.Calculator/x86_64/stable".into()],
        }
        .save(&path)
        .unwrap();
        assert_eq!(
            Kept::read(&path).unwrap().references,
            ["app/org.gnome.Calculator/x86_64/stable"]
        );
    }

    /// A flatpak program of a test's own: it records what it was asked and answers as a system
    /// installation with one app, one remote and one catalogue would. The host's own Flatpak is
    /// never touched.
    fn fake(dir: &Path) -> Flatpak {
        let program = dir.join("flatpak");
        std::fs::write(
            &program,
            r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/calls"
case "$1" in
  --version) echo "Flatpak 1.16";;
  list) printf 'org.gnome.Calculator\tstable\tx86_64\tflathub\t48.0\n';;
  remotes) printf 'flathub\thttps://dl.flathub.org/repo\tFlathub\tsystem\n';;
  info) echo "[Context]"; echo "sockets=wayland;";;
  install|uninstall|remote-add|remote-modify|remote-delete) exit 0;;
  remote-ls) printf 'org.example.Editor\tEditor\tEdits\t1.0\tstable\tflathub\n';;
  *) exit 9;;
esac
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&program).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        std::fs::set_permissions(&program, permissions).unwrap();
        Flatpak::at(&program)
    }

    fn calls(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn the_status_reads_the_apps_their_permissions_and_the_remotes() {
        let dir = tempfile::tempdir().unwrap();
        let flatpak = fake(dir.path());
        let status = flatpak.status().unwrap();
        assert!(status.available);
        assert_eq!(status.installed.len(), 1);
        let app = &status.installed[0];
        assert_eq!(app.app, "org.gnome.Calculator");
        assert_eq!(app.reference, "app/org.gnome.Calculator/x86_64/stable");
        assert_eq!(app.origin, "flathub");
        assert_eq!(app.version, "48.0");
        assert!(app.permissions.contains("sockets=wayland"), "{app:?}");
        assert_eq!(status.remotes.len(), 1);
        assert_eq!(status.remotes[0].name, "flathub");
        assert!(!status.remotes[0].disabled);
        assert!(
            calls(dir.path())
                .iter()
                .all(|call| call.contains("--system"))
        );
    }

    #[test]
    fn a_missing_flatpak_is_a_state_and_not_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let status = Flatpak::at(dir.path().join("no-flatpak")).status().unwrap();
        assert!(!status.available);
        assert!(status.installed.is_empty() && status.remotes.is_empty());
    }

    #[test]
    fn installing_and_removing_act_on_the_system_installation() {
        let dir = tempfile::tempdir().unwrap();
        let flatpak = fake(dir.path());
        flatpak
            .install("app/org.gnome.Calculator/x86_64/stable")
            .unwrap();
        flatpak.remove("org.gnome.Calculator").unwrap();
        let calls = calls(dir.path());
        assert!(
            calls.iter().any(|call| call
                == "install --system --noninteractive -- app/org.gnome.Calculator/x86_64/stable"),
            "{calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|call| call == "uninstall --system --noninteractive -- org.gnome.Calculator"),
            "{calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|call| call == "uninstall --system --unused --noninteractive"),
            "{calls:?}"
        );
        assert!(
            flatpak.install("app/-oops/x86_64/stable").is_err(),
            "a reference never reads as an option"
        );
    }

    #[test]
    fn a_remote_is_added_enabled_and_removed_on_the_system_installation() {
        let dir = tempfile::tempdir().unwrap();
        let flatpak = fake(dir.path());
        flatpak
            .remote_add("tools", "https://tools.example/repo")
            .unwrap();
        flatpak.remote_set_enabled("tools", false).unwrap();
        flatpak.remote_set_enabled("tools", true).unwrap();
        flatpak.remote_remove("tools").unwrap();
        flatpak.flathub_add().unwrap();
        let calls = calls(dir.path());
        for expected in [
            "remote-add --system --if-not-exists -- tools https://tools.example/repo",
            "remote-modify --system --disable -- tools",
            "remote-modify --system --enable -- tools",
            "remote-delete --system --force -- tools",
            "remote-add --system --if-not-exists -- flathub https://dl.flathub.org/repo/flathub.flatpakrepo",
        ] {
            assert!(
                calls.iter().any(|call| call == expected),
                "{expected} in {calls:?}"
            );
        }
        assert!(flatpak.remote_add("--user", "https://x").is_err());
    }

    #[test]
    fn the_catalogue_reads_the_remotes_apps() {
        let dir = tempfile::tempdir().unwrap();
        let flatpak = fake(dir.path());
        let rows = flatpak.catalogue("flathub").unwrap();
        assert_eq!(
            rows,
            "org.example.Editor\tEditor\tEdits\t1.0\tstable\tflathub"
        );
        assert!(flatpak.catalogue("--user").is_err());
    }
}
