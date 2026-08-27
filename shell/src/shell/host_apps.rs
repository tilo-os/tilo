//! Starting applications qOS did not package.
//!
//! Counterpart to [`crate::shell::launcher`], which starts `.qpkg`
//! packages; both exec `qos-launcher` and report through the same
//! [`launcher::LaunchEvent`] queue.
//!
//! `qos-launcher run-host` builds **no sandbox**: a `.desktop` entry names
//! a command line, not a set of permissions, and nesting a Flatpak's own
//! sandbox inside qOS's baseline filter just fails to start rather than
//! confining twice (see `launcher::host`). A host app therefore gets no
//! room id, no capability file, no per-room state directory — `launch`
//! takes no `RoomId` because passing one would misstate what it's subject to.
//!
//! The palette labels host apps under [`ResourceKind::HostApplication`]
//! and the launcher logs an UNCONFINED notice for each — managing the risk
//! of a user unable to tell which kind of app they're looking at, since
//! there was never a sandbox to escape.
//!
//! [`ResourceKind::HostApplication`]: crate::shell::search::ResourceKind::HostApplication

use std::path::{Path, PathBuf};

use desktop_entry::{DesktopEntry, EntrySource};

use crate::shell::launcher::{self, LaunchError};

/// One host application the palette can offer — a projection of
/// [`DesktopEntry`] holding only what the shell needs, so the provider can
/// be tested against a fixed list without real `.desktop` files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostApp {
    /// Desktop-file id (`com.spotify.Client`, `org.gnome.Nautilus`) — the
    /// identity `run-host` resolves and window correlation matches on.
    pub desktop_id: String,
    pub name: String,
    /// `Flatpak` or `System`, shown in the row so provenance is visible
    /// without opening anything.
    pub source_label: &'static str,
    /// `Categories=`, matched by the provider so typing a category word
    /// offers the apps in it.
    pub categories: Vec<String>,
    /// `Keywords=`, matched the same way.
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

    /// Whether this is a Flatpak, for callers that want to say so.
    pub fn is_flatpak(&self) -> bool {
        self.source_label == EntrySource::Flatpak { app_id: String::new() }.label()
    }
}

/// Every launchable host application, memoised by the scanner.
pub fn host_apps() -> Vec<HostApp> {
    desktop_entry::cache::cached_scan()
        .entries
        .iter()
        .map(HostApp::from_entry)
        .collect()
}

/// Drop the memoised scan. Used after something is known to have
/// changed, and by tests, which share process-wide state.
pub fn invalidate_cache() {
    desktop_entry::cache::invalidate();
}

/// Look one up by desktop-file id.
pub fn find(desktop_id: &str) -> Option<HostApp> {
    desktop_entry::cache::cached_scan()
        .find(desktop_id)
        .map(HostApp::from_entry)
}

/// Render the `qos-launcher run-host` invocation for `app`. Pure — no I/O,
/// no spawning — so the exact command is assertable without a process
/// appearing.
///
/// No `--workspace-id` and no `--capabilities`: a host app is in no room
/// and confined by nothing qOS controls, so passing either would assert
/// something false. Compare [`launcher::build_launch_command`], which
/// passes both.
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
    }
}

/// Resolve `desktop_id` and start it, unconfined.
///
/// Returns as soon as the launch is handed to its worker thread. The
/// `Ok` value is the resolved app so callers can name it in their UI
/// without a second lookup.
pub fn launch(desktop_id: &str) -> Result<HostApp, LaunchError> {
    launch_with_document(desktop_id, None)
}

/// [`launch`], additionally asking the app to open `document` — the path
/// expands the entry's own `%f`/`%F`/`%u`/`%U` field codes. Unlike a qOS
/// launch there is no grant to make alongside it: the app already has the
/// user's filesystem, which is exactly what makes it a host app.
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

    // Same note the qOS path leaves, so an arriving window lands where
    // asked and is labelled with the app's real name, not a raw wayland app-id.
    launcher::remember_launch_named(&app.desktop_id, &app.window_identities, &app.name);
    launcher::spawn_launch(app.desktop_id.clone(), app.name.clone(), command);
    Ok(app)
}

/// Where a host app's window should land, recorded before the launch. A
/// foreign toplevel arrives with an `app_id` qOS didn't mint, so the note
/// is keyed on every identity the entry might use (`StartupWMClass`,
/// desktop-file id, Flatpak app id).
pub fn note_placement(app: &HostApp, placement: crate::compositor::tab_binding::Placement) {
    launcher::set_pending_placement_for_identities(
        &app.desktop_id,
        &app.window_identities,
        placement,
    );
}

/// Host application whose window would announce itself as `app_id`.
/// Consulted when a foreign toplevel is adopted, so its tab shows
/// "Spotify" not "spotify" — and so the compositor can tell a host app's
/// window from a qOS app's at all.
pub fn app_for_window_id(app_id: &str) -> Option<HostApp> {
    let needle = app_id.trim();
    if needle.is_empty() {
        return None;
    }
    host_apps()
        .into_iter()
        .find(|app| app.window_identities.iter().any(|id| id.eq_ignore_ascii_case(needle)))
}

/// Path to the compositor's `.desktop` search path, for diagnostics.
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
            window_identities: identities.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// The absence of `--workspace-id` and `--capabilities` is the
    /// whole difference between this and a qOS launch, so it is
    /// asserted rather than assumed.
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
        let app = app("org.gnome.TextEditor", "Text Editor", &["org.gnome.TextEditor"]);
        let cmd = build_launch_command(
            &app,
            Path::new("/usr/libexec/qos-launcher"),
            Path::new("/run/user/1000/wayland-1"),
            Some(Path::new("/home/u/notes.txt")),
        );
        assert_eq!(cmd.args[5], "--open");
        assert_eq!(cmd.args[6], "/home/u/notes.txt");
    }

    /// The real scan of this machine. Not a fixture: the point of the
    /// scanner is that it finds what is actually installed, and a test
    /// that only ever reads files it wrote itself cannot show that.
    #[test]
    fn the_host_scan_returns_entries_with_usable_identities() {
        invalidate_cache();
        let apps = host_apps();
        // No desktop entries is a legitimate state (a minimal container),
        // so assert shape, not count.
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
