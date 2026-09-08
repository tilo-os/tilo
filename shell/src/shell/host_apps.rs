//! Starting applications qOS did not package.
//!
//! Counterpart to [`crate::shell::launcher`], which starts `.qpkg`
//! packages; both exec `qos-launcher` and report through the same
//! [`launcher::LaunchEvent`] queue.
//!
//! `qos-launcher run-host` builds **no sandbox** — nesting a Flatpak's
//! own sandbox inside qOS's baseline filter just fails to start. A host
//! app gets no room id, no capability file, no per-room state
//! directory. `launch` accepts only an opaque portal-attribution room string:
//! it lets an existing Flatpak's authenticated portal request resolve to the
//! workspace containing its window, but is not passed to the app and grants
//! no qOS service or sandbox authority. The palette labels host apps under
//! [`ResourceKind::HostApplication`] so a user can tell which kind of
//! app they're looking at. Host-desktop sessions also pass the private
//! rootless-Xwayland display for software that cannot speak Wayland.
//!
//! [`ResourceKind::HostApplication`]: crate::shell::search::ResourceKind::HostApplication

use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

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

/// A palette launch held until the compositor can mint its external
/// `xdg_activation_v1` token on the compositor thread.
#[derive(Debug, Clone)]
pub(crate) struct QueuedLaunch {
    pub desktop_id: String,
    pub display_name: String,
    pub placement: crate::compositor::tab_binding::Placement,
    pub portal_workspace_id: Option<String>,
}

fn queued_launches() -> &'static std::sync::Mutex<Vec<QueuedLaunch>> {
    static QUEUED: OnceLock<std::sync::Mutex<Vec<QueuedLaunch>>> = OnceLock::new();
    QUEUED.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

pub(crate) fn queue_launch(request: QueuedLaunch) {
    queued_launches()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(request);
}

pub(crate) fn drain_queued_launches() -> Vec<QueuedLaunch> {
    std::mem::take(
        &mut *queued_launches()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct X11Session {
    pub display: String,
    pub authority: PathBuf,
}

fn x11_slot() -> &'static RwLock<Option<X11Session>> {
    static SESSION: OnceLock<RwLock<Option<X11Session>>> = OnceLock::new();
    SESSION.get_or_init(|| RwLock::new(None))
}

pub(crate) fn set_x11_session(session: Option<X11Session>) {
    if let Ok(mut slot) = x11_slot().write() {
        *slot = session;
    }
}

/// The only X11 connection host launches may inherit. The compositor revokes
/// this as soon as its bridge exits; callers must treat `None` as a deliberate
/// Wayland-only launch, never fall back to ambient DISPLAY/XAUTHORITY.
pub(crate) fn x11_session() -> Option<X11Session> {
    x11_slot().read().ok().and_then(|slot| slot.clone())
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
    deduplicate_host_apps(
        desktop_entry::cache::cached_scan()
            .entries
            .iter()
            .map(HostApp::from_entry)
            .collect(),
    )
}

/// Collapse packaging aliases while preserving genuinely distinct launch
/// modes. A shared display name alone is not identity; a shared declared
/// window identity is. Prefer Flatpak when a user/system export and a distro
/// file both describe the same app because it retains its original sandbox.
fn deduplicate_host_apps(apps: Vec<HostApp>) -> Vec<HostApp> {
    let mut out: Vec<HostApp> = Vec::new();
    for app in apps {
        let duplicate = out.iter().position(|existing| {
            existing.name.eq_ignore_ascii_case(&app.name)
                && existing.window_identities.iter().any(|left| {
                    app.window_identities
                        .iter()
                        .any(|right| left.eq_ignore_ascii_case(right))
                })
        });
        if let Some(index) = duplicate {
            if let Some(existing) = out.get_mut(index)
                && app.is_flatpak()
                && !existing.is_flatpak()
            {
                *existing = app;
            }
        } else {
            out.push(app);
        }
    }
    out
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
pub(crate) fn build_launch_command(
    app: &HostApp,
    launcher_bin: &Path,
    wayland_socket: &Path,
    document: Option<&Path>,
    x11: Option<&X11Session>,
    portal_workspace_id: Option<String>,
    activation_token: Option<&str>,
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
    if let Some(x11) = x11 {
        args.push("--x11-display".to_string());
        args.push(x11.display.clone());
        args.push("--x11-authority".to_string());
        args.push(x11.authority.display().to_string());
    }
    let mut env = vec![(
        "XDG_RUNTIME_DIR".to_string(),
        launcher::runtime_dir().display().to_string(),
    )];
    if let Some(token) = activation_token.filter(|token| !token.is_empty()) {
        // The launcher is an intermediary. Keep this out of argv/process
        // listings; it converts it into the standard child-facing variable.
        env.push(("QOS_HOST_ACTIVATION_TOKEN".to_string(), token.to_string()));
    }
    launcher::LaunchCommand {
        program: launcher_bin.to_path_buf(),
        args,
        env,
        service_identity: None,
        portal_workspace_id,
        managed_agent_id: None,
    }
}

/// Returns as soon as the launch is handed to its worker thread; `Ok`
/// is the resolved app so callers can name it without a second lookup.
/// `portal_workspace_id` is compositor bookkeeping for standard desktop
/// portal attribution only; it never enters the unconfined child environment.
///
/// # Errors
///
/// Returns [`LaunchError::HostAppNotFound`] when `desktop_id` isn't
/// registered, or [`LaunchError::NoDisplay`]/a launcher-resolution error
/// when the compositor or `qos-launcher` binary can't be reached.
pub fn launch(
    desktop_id: &str,
    portal_workspace_id: Option<String>,
) -> Result<HostApp, LaunchError> {
    launch_with_document_and_activation(desktop_id, None, portal_workspace_id, None)
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
    portal_workspace_id: Option<String>,
) -> Result<HostApp, LaunchError> {
    launch_with_document_and_activation(desktop_id, document, portal_workspace_id, None)
}

/// Host launch with a compositor-minted `xdg_activation_v1` token. The token
/// travels only as the standard desktop environment variable and grants no
/// qOS sandbox or service authority.
///
/// # Errors
///
/// Returns the same launch-resolution errors as [`launch_with_document`].
pub fn launch_with_document_and_activation(
    desktop_id: &str,
    document: Option<&Path>,
    portal_workspace_id: Option<String>,
    activation_token: Option<&str>,
) -> Result<HostApp, LaunchError> {
    // Placement is recorded before this function is called, while the user
    // action's layout is still current.  A refusal before the worker starts
    // must retire that one-shot note or a later independently-opened window
    // could inherit this dead launch's destination.
    let Some(app) = find(desktop_id) else {
        launcher::clear_pending_placement(desktop_id);
        return Err(LaunchError::HostAppNotFound {
            desktop_id: desktop_id.to_string(),
        });
    };
    let Some(socket) = launcher::wayland_socket() else {
        launcher::clear_pending_placement(&app.desktop_id);
        return Err(LaunchError::NoDisplay);
    };
    let launcher_bin = match launcher::resolve_launcher_bin() {
        Ok(launcher_bin) => launcher_bin,
        Err(error) => {
            launcher::clear_pending_placement(&app.desktop_id);
            return Err(error);
        }
    };
    let x11 = x11_session();
    let command = build_launch_command(
        &app,
        &launcher_bin,
        &socket,
        document,
        x11.as_ref(),
        portal_workspace_id,
        activation_token,
    );

    launcher::remember_launch_named(&app.desktop_id, &app.window_identities, &app.name);
    let _ = launcher::spawn_launch(app.desktop_id.clone(), app.name.clone(), command);
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
    app_for_window_id_in(app_id, host_apps())
}

fn app_for_window_id_in(app_id: &str, apps: Vec<HostApp>) -> Option<HostApp> {
    let needle = app_id.trim();
    if needle.is_empty() {
        return None;
    }
    apps.into_iter().find(|app| {
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
            None,
            None,
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
            None,
            None,
            None,
        );
        assert_eq!(cmd.args[5], "--open");
        assert_eq!(cmd.args[6], "/home/u/notes.txt");
    }

    #[test]
    fn an_x11_bridge_rides_to_the_unconfined_launcher() {
        let app = app("com.valvesoftware.Steam", "Steam", &["steam"]);
        let x11 = X11Session {
            display: ":1234".into(),
            authority: PathBuf::from("/run/user/1000/qos-session/Xauthority-1234"),
        };
        let cmd = build_launch_command(
            &app,
            Path::new("/usr/bin/qos-launcher"),
            Path::new("/run/user/1000/wayland-2"),
            None,
            Some(&x11),
            Some("room-7".into()),
            None,
        );
        assert!(
            cmd.args
                .windows(2)
                .any(|pair| pair == ["--x11-display", ":1234"])
        );
        assert!(cmd.args.windows(2).any(|pair| {
            pair == [
                "--x11-authority",
                "/run/user/1000/qos-session/Xauthority-1234",
            ]
        }));
        assert_eq!(cmd.portal_workspace_id.as_deref(), Some("room-7"));
        assert!(
            !cmd.env.iter().any(|(key, _)| key.contains("WORKSPACE")),
            "portal attribution is compositor bookkeeping, not ambient authority"
        );
    }

    #[test]
    #[serial_test::serial]
    fn revoked_x11_session_cannot_leak_into_a_later_host_launch() {
        let app = app("com.example.Legacy", "Legacy", &["legacy"]);
        set_x11_session(Some(X11Session {
            display: ":1234".into(),
            authority: PathBuf::from("/run/user/1000/qos-session/Xauthority-1234"),
        }));
        assert!(x11_session().is_some(), "fixture published an X11 bridge");

        // This is the exact action SessionXwayland takes after reaping its
        // child. A later host launch must receive no stale bridge arguments.
        set_x11_session(None);
        let command = build_launch_command(
            &app,
            Path::new("/usr/bin/qos-launcher"),
            Path::new("/run/user/1000/wayland-2"),
            None,
            x11_session().as_ref(),
            None,
            None,
        );
        assert!(
            !command
                .args
                .iter()
                .any(|arg| arg == "--x11-display" || arg == "--x11-authority"),
            "a dead bridge must not hand a later X11/Electron launch stale connection details"
        );
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

    #[test]
    fn cross_source_aliases_are_one_app_and_keep_flatpak_confinement() {
        let system = HostApp {
            source_label: "System",
            ..app("vendor-chat", "Chat", &["Chat"])
        };
        let flatpak = HostApp {
            source_label: "Flatpak",
            ..app("org.example.Chat", "Chat", &["org.example.Chat", "Chat"])
        };
        let apps = deduplicate_host_apps(vec![system, flatpak.clone()]);
        assert_eq!(apps, vec![flatpak]);
    }

    #[test]
    fn same_named_distinct_modes_remain_visible() {
        let apps = deduplicate_host_apps(vec![
            app("org.example.Editor", "Editor", &["Editor"]),
            app("org.example.EditorSafe", "Editor", &["EditorSafe"]),
        ]);
        assert_eq!(apps.len(), 2);
    }

    #[test]
    fn window_adoption_uses_the_canonical_alias() {
        let system = HostApp {
            source_label: "System",
            ..app("vendor-chat", "Chat", &["Chat"])
        };
        let flatpak = HostApp {
            source_label: "Flatpak",
            ..app("org.example.Chat", "Chat", &["org.example.Chat", "Chat"])
        };
        let canonical = deduplicate_host_apps(vec![system, flatpak.clone()]);
        assert_eq!(app_for_window_id_in("Chat", canonical), Some(flatpak));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn a_refused_host_launch_cannot_leave_a_stale_placement() {
        const MISSING: &str = "org.qos.test.definitely-not-a-desktop-entry";
        launcher::clear_pending_placements();
        launcher::set_pending_placement(
            MISSING,
            crate::compositor::tab_binding::Placement::NewGroup,
        );

        assert!(matches!(
            launch(MISSING, None),
            Err(LaunchError::HostAppNotFound { .. })
        ));
        assert!(
            launcher::placement_for_toplevel(MISSING, 0xC1_05_ED).is_none(),
            "a later unrelated window must not inherit a launch that was refused"
        );
    }
}
