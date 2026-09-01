//! Starting applications qOS did not package.
//!
//! Counterpart to [`crate::shell::launcher`], which starts `.qpkg`
//! packages; both exec `qos-launcher` and report through the same
//! [`launcher::LaunchEvent`] queue.
//!
//! `qos-launcher run-host` builds **no sandbox** — nesting a Flatpak's
//! own sandbox inside qOS's baseline filter just fails to start. A host
//! app gets no room id, no capability file, no per-room state
//! directory; `launch` takes no `RoomId` because passing one would
//! misstate what it's subject to. The palette labels host apps under
//! [`ResourceKind::HostApplication`] so a user can tell which kind of
//! app they're looking at.
//!
//! [`ResourceKind::HostApplication`]: crate::shell::search::ResourceKind::HostApplication

use std::path::{Path, PathBuf};

use desktop_entry::{DesktopEntry, EntrySource};

use crate::shell::launcher::{self, LaunchError};

/// A projection of [`DesktopEntry`] holding only what the shell needs,
/// testable against a fixed list without real `.desktop` files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostApp {
    /// The identity `run-host` resolves and window correlation matches on.
    pub desktop_id: String,
    pub name: String,
    /// `Flatpak` or `System`, shown so provenance is visible without opening.
    pub source_label: &'static str,
    pub categories: Vec<String>,
    pub keywords: Vec<String>,
    /// Every string this app's window might announce itself as. See
    /// [`DesktopEntry::window_identities`].
    pub window_identities: Vec<String>,
}

impl HostApp {
    fn from_entry(entry: &DesktopEntry) -> Self {
        Self {
            desktop_id: entry.id.clone(),
            name: entry.name.clone(),
            source_label: entry.source.label(),
            categories: entry.categories.clone(),
            keywords: entry.keywords.clone(),
            window_identities: entry.window_identities(),
        }
    }

    pub fn is_flatpak(&self) -> bool {
        self.source_label
            == EntrySource::Flatpak {
                app_id: String::new(),
            }
            .label()
    }
}

pub fn host_apps() -> Vec<HostApp> {
    desktop_entry::cache::cached_scan()
        .entries
        .iter()
        .map(HostApp::from_entry)
        .collect()
}

pub fn invalidate_cache() {
    desktop_entry::cache::invalidate();
}

pub fn find(desktop_id: &str) -> Option<HostApp> {
    desktop_entry::cache::cached_scan()
        .find(desktop_id)
        .map(HostApp::from_entry)
}

/// Pure — no I/O, so the exact command is assertable. No
/// `--workspace-id` and no `--capabilities`: a host app is in no room
/// and confined by nothing qOS controls, so passing either would
/// assert something false. Compare [`launcher::build_launch_command`].
pub fn build_launch_command(
    app: &HostApp,
    launcher_bin: &Path,
    wayland_socket: &Path,
    document: Option<&Path>,
) -> launcher::LaunchCommand {
    let mut args = vec![
        "run-host".to_string(),
        "--desktop-id".to_string(),
        app.desktop_id.clone(),
        "--wayland-socket".to_string(),
        wayland_socket.display().to_string(),
    ];
    if let Some(path) = document {
        args.push("--open".to_string());
        args.push(path.display().to_string());
    }
    launcher::LaunchCommand {
        program: launcher_bin.to_path_buf(),
        args,
        env: vec![(
            "XDG_RUNTIME_DIR".to_string(),
            launcher::runtime_dir().display().to_string(),
        )],
        service_identity: None,
    }
}

/// Returns as soon as the launch is handed to its worker thread; `Ok`
/// is the resolved app so callers can name it without a second lookup.
///
/// # Errors
///
/// Returns [`LaunchError::HostAppNotFound`] when `desktop_id` isn't
/// registered, or [`LaunchError::NoDisplay`]/a launcher-resolution error
/// when the compositor or `qos-launcher` binary can't be reached.
pub fn launch(desktop_id: &str) -> Result<HostApp, LaunchError> {
    launch_with_document(desktop_id, None)
}

/// Unlike a qOS launch there's no grant to make alongside it — the app
/// already has the user's filesystem, which is what makes it a host app.
///
/// # Errors
///
/// Returns [`LaunchError::HostAppNotFound`] when `desktop_id` isn't
/// registered, or [`LaunchError::NoDisplay`]/a launcher-resolution error
/// when the compositor or `qos-launcher` binary can't be reached.
pub fn launch_with_document(
    desktop_id: &str,
    document: Option<&Path>,
) -> Result<HostApp, LaunchError> {
    let app = find(desktop_id).ok_or_else(|| LaunchError::HostAppNotFound {
        desktop_id: desktop_id.to_string(),
    })?;
    let socket = launcher::wayland_socket().ok_or(LaunchError::NoDisplay)?;
    let launcher_bin = launcher::resolve_launcher_bin()?;
    let command = build_launch_command(&app, &launcher_bin, &socket, document);

    launcher::remember_launch_named(&app.desktop_id, &app.window_identities, &app.name);
    launcher::spawn_launch(app.desktop_id.clone(), app.name.clone(), command);
    Ok(app)
}

/// A foreign toplevel arrives with an `app_id` qOS didn't mint, so the
/// note is keyed on every identity the entry might use.
pub fn note_placement(app: &HostApp, placement: crate::compositor::tab_binding::Placement) {
    launcher::set_pending_placement_for_identities(
        &app.desktop_id,
        &app.window_identities,
        placement,
    );
}

/// Consulted when a foreign toplevel is adopted, so its tab shows
/// "Spotify" not "spotify", and so the compositor can tell a host app's
/// window from a qOS app's at all.
pub fn app_for_window_id(app_id: &str) -> Option<HostApp> {
    let needle = app_id.trim();
    if needle.is_empty() {
        return None;
    }
    host_apps().into_iter().find(|app| {
        app.window_identities
            .iter()
            .any(|id| id.eq_ignore_ascii_case(needle))
    })
}

pub fn search_path() -> Vec<PathBuf> {
    desktop_entry::scan::application_dirs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(desktop_id: &str, name: &str, identities: &[&str]) -> HostApp {
        HostApp {
            desktop_id: desktop_id.to_string(),
            name: name.to_string(),
            source_label: "Flatpak",
            categories: Vec::new(),
            keywords: Vec::new(),
            window_identities: identities
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
        }
    }

    #[test]
    fn a_host_launch_command_carries_no_room_and_no_capabilities() {
        let app = app("com.spotify.Client", "Spotify", &["spotify"]);
        let cmd = build_launch_command(
            &app,
            Path::new("/usr/libexec/qos-launcher"),
            Path::new("/run/user/1000/wayland-1"),
            None,
        );
        assert_eq!(cmd.program, PathBuf::from("/usr/libexec/qos-launcher"));
        assert_eq!(
            cmd.args,
            vec![
                "run-host",
                "--desktop-id",
                "com.spotify.Client",
                "--wayland-socket",
                "/run/user/1000/wayland-1",
            ]
        );
        assert!(
            !cmd.args.iter().any(|a| a == "--workspace-id"),
            "a host app is in no room; naming one would be false"
        );
        assert!(
            !cmd.args.iter().any(|a| a == "--capabilities"),
            "there is no sandbox to enforce a capability against"
        );
    }

    #[test]
    fn a_document_rides_across_as_open() {
        let app = app(
            "org.gnome.TextEditor",
            "Text Editor",
            &["org.gnome.TextEditor"],
        );
        let cmd = build_launch_command(
            &app,
            Path::new("/usr/libexec/qos-launcher"),
            Path::new("/run/user/1000/wayland-1"),
            Some(Path::new("/home/u/notes.txt")),
        );
        assert_eq!(cmd.args[5], "--open");
        assert_eq!(cmd.args[6], "/home/u/notes.txt");
    }

    /// The real scan of this machine, not a fixture.
    #[test]
    fn the_host_scan_returns_entries_with_usable_identities() {
        invalidate_cache();
        let apps = host_apps();
        // No entries is legitimate (a minimal container); assert shape, not count.
        for app in apps.iter().take(50) {
            assert!(!app.desktop_id.is_empty());
            assert!(!app.name.is_empty(), "{} has no name", app.desktop_id);
            assert!(
                !app.window_identities.is_empty(),
                "{} offers nothing to correlate a window against",
                app.desktop_id
            );
            assert!(
                !app.window_identities.iter().any(|i| i == "flatpak"),
                "{} would match every Flatpak window on the system",
                app.desktop_id
            );
        }
    }
}
