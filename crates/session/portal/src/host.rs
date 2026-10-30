//! What the portal does on the person's behalf outside the session: open a
//! link with the app they chose for it, read who they are on this device.
//!
//! Behind [`Host`] so a test sees what would have been opened instead of a
//! mail client appearing.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;

/// Who the person is on this device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    /// Their login name.
    pub id: String,
    /// Their name as they wrote it, else the login name.
    pub name: String,
    /// Their picture, as a file URI, when they set one.
    pub image: Option<String>,
}

/// The device the portal acts on.
pub trait Host: Send + Sync {
    /// Open `uri` with the app the person chose for its scheme; `token`
    /// activates its window.
    ///
    /// # Errors
    /// No app handles it, or it did not start.
    fn open(&self, uri: &str, token: Option<&str>) -> Result<(), String>;

    /// Who the person is.
    fn person(&self) -> Option<Person>;
}

/// This system: the default apps of `mimeapps.list`, the user database.
pub struct System;

impl Host for System {
    fn open(&self, uri: &str, token: Option<&str>) -> Result<(), String> {
        let scheme = uri
            .split_once(':')
            .map(|(scheme, _)| scheme)
            .ok_or("no scheme")?;
        let scan = desktop_entry::cache::cached_scan();
        let chain = desktop_policy::MimeAppsChain::load(&desktop_policy::mimeapps::search_path(
            &[desktop_policy::mimeapps::CURRENT_DESKTOP.to_owned()],
            tilo_paths::xdg::config_home().as_deref(),
            &tilo_paths::xdg::config_dirs(),
            tilo_paths::xdg::data_home().as_deref(),
            &tilo_paths::xdg::data_dirs(),
        ));
        let mime = format!("x-scheme-handler/{scheme}");
        let usable = |file: &str| scan.find(file.trim_end_matches(".desktop")).is_some();
        let entry = chain
            .default_for(&mime, usable)
            .and_then(|file| scan.find(file.trim_end_matches(".desktop")))
            .or_else(|| {
                scan.entries
                    .iter()
                    .find(|entry| entry.mime_types.contains(&mime))
            })
            .ok_or_else(|| format!("no app opens {scheme} links"))?;
        let env = desktop_entry::launch::LaunchEnv {
            wayland_socket: wayland_socket(),
            activation_token: token.map(str::to_owned),
            urls: vec![uri.to_owned()],
            ..desktop_entry::launch::LaunchEnv::default()
        };
        let launch = desktop_entry::launch::plan(entry, &env).map_err(|error| error.to_string())?;
        launch
            .to_command()
            .spawn()
            .map(drop)
            .map_err(|error| error.to_string())
    }

    fn person(&self) -> Option<Person> {
        let uid = rustix::process::getuid().as_raw();
        let passwd = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
        let (id, gecos, home) = passwd
            .lines()
            .find_map(|line| passwd_entry(line, uid))
            .or_else(|| {
                let id = std::env::var("USER").ok()?;
                Some((id, String::new(), tilo_paths::home()?))
            })?;
        let name = gecos
            .split(',')
            .next()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map_or_else(|| id.clone(), str::to_owned);
        let image = [
            home.join(".face"),
            PathBuf::from(format!("/var/lib/AccountsService/icons/{id}")),
        ]
        .into_iter()
        .find(|path| path.is_file())
        .and_then(|path| crate::file_chooser::file_uri(&path));
        Some(Person { id, name, image })
    }
}

/// The compositor socket apps started for the person connect to.
fn wayland_socket() -> Option<PathBuf> {
    let display = std::env::var_os("WAYLAND_DISPLAY")?;
    let display = PathBuf::from(display);
    if display.is_absolute() {
        return Some(display);
    }
    tilo_paths::runtime_dir().map(|dir| dir.join(display))
}

/// Login name, GECOS field and home of `line` of `/etc/passwd` when it is
/// user `uid`'s.
fn passwd_entry(line: &str, uid: u32) -> Option<(String, String, PathBuf)> {
    let mut fields = line.split(':');
    let id = fields.next()?;
    let _password = fields.next()?;
    let line_uid: u32 = fields.next()?.parse().ok()?;
    let _gid = fields.next()?;
    let gecos = fields.next()?;
    let home = fields.next()?;
    (line_uid == uid && !id.is_empty())
        .then(|| (id.to_owned(), gecos.to_owned(), PathBuf::from(home)))
}

/// A host that opens nothing and records what it was asked: for tests.
#[derive(Clone, Default)]
pub struct Recorded {
    pub opened: Arc<Mutex<Vec<String>>>,
    pub person: Option<Person>,
}

impl Host for Recorded {
    fn open(&self, uri: &str, _token: Option<&str>) -> Result<(), String> {
        self.opened.lock_recover().push(uri.to_owned());
        Ok(())
    }

    fn person(&self) -> Option<Person> {
        self.person.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_passwd_line_gives_name_and_home_for_its_uid_only() {
        let line = "ada:x:1000:1000:Ada Lovelace,,,:/home/ada:/bin/bash";
        assert_eq!(
            passwd_entry(line, 1000),
            Some((
                "ada".into(),
                "Ada Lovelace,,,".into(),
                PathBuf::from("/home/ada")
            ))
        );
        assert_eq!(passwd_entry(line, 1001), None);
        assert_eq!(passwd_entry("broken", 1000), None);
    }
}
