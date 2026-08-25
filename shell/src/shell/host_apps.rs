//! Starting applications qOS did not package.
//!
//! The counterpart to [`crate::shell::launcher`], which starts `.qpkg`
//! packages. Both end up execing `qos-launcher` on a worker thread and
//! reporting through the same [`launcher::LaunchEvent`] queue, so the
//! chrome drains one stream of launch outcomes rather than two. What
//! differs is the subcommand and, with it, everything about the process
//! that comes out the other end.
//!
//! ## What a host launch is not
//!
//! `qos-launcher run-host` builds **no sandbox**. It cannot: a
//! `.desktop` entry names a command line, not a set of permissions, so
//! there is nothing to derive binds, a seccomp profile or a Landlock
//! ruleset from. For a Flatpak it is worse than under-specified —
//! `flatpak run` needs `unshare`/`mount`/`pivot_root` to build its own
//! sandbox and qOS's baseline filter denies all three, so nesting the
//! two produces an app that fails to start rather than one confined
//! twice. See `launcher::host` for the full argument.
//!
//! Consequently a host app gets **no room id, no capability file and no
//! per-room state directory**, and this module never passes any of
//! them. `launch` takes no `RoomId` for that reason: there is no room
//! to pass, and an argument that looked like one would be a lie about
//! what the process is subject to.
//!
//! ## Telling them apart
//!
//! The palette puts host apps under their own
//! [`ResourceKind::HostApplication`] chip and group header, their tabs
//! are labelled from the same scan, and the launcher prints an
//! UNCONFINED notice naming each one it starts. The risk being managed
//! is not escape — there was never a sandbox to escape — but a user who
//! cannot tell which of the two kinds of application they are looking
//! at.
//!
//! [`ResourceKind::HostApplication`]: crate::shell::search::ResourceKind::HostApplication

use std::path::{Path, PathBuf};

use desktop_entry::{DesktopEntry, EntrySource};

use crate::shell::launcher::{self, LaunchError};

/// One host application the palette can offer.
///
/// A projection of [`DesktopEntry`] holding only what the shell needs,
/// so the search provider can be tested against a fixed list without a
/// filesystem full of `.desktop` files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostApp {
    /// Desktop-file id — `com.spotify.Client`, `org.gnome.Nautilus`.
    /// The identity `run-host` resolves and the handle window
    /// correlation matches on.
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

/// Whether this session offers the machine's own applications at all.
///
/// Enumerating them means reading the `.desktop` files of whatever
/// desktop qOS happens to be nested inside, and that is a **development
/// affordance**: on a workstation it puts the programs already
/// installed there within reach of a compositor being worked on. It is
/// not a property of qOS, and it is not something every session should
/// inherit merely by running on a machine that has other software.
///
/// Two sessions say so, and both say it about themselves rather than
/// about which applications exist:
///
/// * A session handed an app store of its own — `QOS_APPS_DATA_DIR`
///   with no `QOS_APPS_SYSTEM_DIR`, which
///   [`launcher::registry::store_is_scoped_to_this_session`] already
///   defines as "the store is exactly here". Having been told which
///   applications it has, it does not then go looking for more on the
///   surrounding machine. This is what a scoped session — a demo rig, a
///   recording, a test fixture — gets without asking for it, and it is
///   why the palette of such a session lists the packages installed in
///   its own registry and nothing else.
/// * `QOS_SHOTS_NO_HOST_APPS`, set by the screenshot harness, which has
///   no store of its own to be scoped by. Kept as its own switch
///   because the harness photographs a machine that is not the
///   viewer's and needs the answer regardless of how its store is
///   configured.
///
/// Deliberately narrow: this governs what the *palette offers*. The
/// scan itself stays available, so [`find`] and [`app_for_window_id`]
/// still name a foreign toplevel that arrives anyway — a window on
/// screen with a name is not the same thing as a menu of programs to
/// start.
///
/// [`launcher::registry::store_is_scoped_to_this_session`]:
///     launcher::registry::store_is_scoped_to_this_session
pub fn available_to_this_session() -> bool {
    if launcher_registry_is_scoped() {
        return false;
    }
    !std::env::var_os("QOS_SHOTS_NO_HOST_APPS").is_some_and(|v| !v.is_empty())
}

/// [`launcher::registry::store_is_scoped_to_this_session`], named here
/// so the rule above reads as one sentence and the dependency on the
/// packaging crate is a single call.
fn launcher_registry_is_scoped() -> bool {
    ::launcher::registry::store_is_scoped_to_this_session()
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

/// Render the `qos-launcher run-host` invocation for `app`.
///
/// Pure — no I/O, no spawning — so the exact command a launch would use
/// is assertable without a process appearing.
///
/// There is no `--workspace-id` and no `--capabilities`, and their
/// absence is the point: a host app is in no room and confined by
/// nothing qOS controls, so passing either would assert something
/// false. Compare [`launcher::build_launch_command`], which passes
/// both.
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

/// [`launch`], additionally asking the app to open `document`.
///
/// The path expands the entry's own `%f`/`%F`/`%u`/`%U` field codes.
/// Unlike a qOS launch there is no grant to make alongside it: the app
/// already has the user's filesystem, which is exactly what makes it a
/// host app.
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

    // The same note the qOS path leaves, so a window arriving in a
    // moment can be put where the user asked for it and labelled with
    // the app's real name rather than a raw wayland app-id.
    launcher::remember_launch_named(&app.desktop_id, &app.window_identities, &app.name);
    launcher::spawn_launch(app.desktop_id.clone(), app.name.clone(), command);
    Ok(app)
}

/// Where a host app's window should land, recorded before the launch.
///
/// A foreign toplevel arrives with an `app_id` qOS did not mint, so the
/// note is keyed on every identity the entry says its window might use
/// — `StartupWMClass` first, then the desktop-file id, then the Flatpak
/// application id.
pub fn note_placement(app: &HostApp, placement: crate::compositor::tab_binding::Placement) {
    launcher::set_pending_placement_for_identities(
        &app.desktop_id,
        &app.window_identities,
        placement,
    );
}

/// Host application whose window would announce itself as `app_id`.
///
/// Consulted when a foreign toplevel is adopted, so its tab shows
/// "Spotify" rather than "spotify" — and so the compositor can tell a
/// host app's window from a qOS app's at all.
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

    /// Set the three variables that decide the answer, run `f`, and put
    /// back whatever was there. They are process-wide, so every test
    /// that uses this is on the `process_env` serial group.
    fn with_env<T>(
        data_dir: Option<&str>,
        system_dir: Option<&str>,
        no_host_apps: Option<&str>,
        f: impl FnOnce() -> T,
    ) -> T {
        let keys = [
            "QOS_APPS_DATA_DIR",
            "QOS_APPS_SYSTEM_DIR",
            "QOS_SHOTS_NO_HOST_APPS",
        ];
        let saved: Vec<Option<std::ffi::OsString>> =
            keys.iter().map(|k| std::env::var_os(k)).collect();
        let set = |key: &str, value: Option<&std::ffi::OsStr>| {
            // SAFETY: serialised on `process_env`, so no other thread in
            // this binary is reading the environment meanwhile.
            unsafe {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        };
        for (key, value) in keys.iter().zip([data_dir, system_dir, no_host_apps]) {
            set(key, value.map(std::ffi::OsStr::new));
        }
        let out = f();
        for (key, value) in keys.iter().zip(saved.iter()) {
            set(key, value.as_deref());
        }
        out
    }

    /// A workstation running qOS nested keeps the affordance: nothing
    /// has said which applications this session has, so the machine's
    /// are within reach.
    #[test]
    #[serial_test::serial(process_env)]
    fn an_unscoped_session_offers_the_machines_applications() {
        assert!(with_env(None, None, None, available_to_this_session));
    }

    /// A session handed a store of its own has already been told what
    /// it has, and the machine around it is not part of the answer.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_session_scoped_to_its_own_store_does_not() {
        assert!(
            !with_env(
                Some("/tmp/qos-apps-fixture"),
                None,
                None,
                available_to_this_session
            ),
            "a session told `the store is exactly here` must not go on to \
             enumerate the surrounding machine's applications"
        );
    }

    /// `QOS_APPS_SYSTEM_DIR` beside it means "read both stores", which
    /// is a machine-wide reading and not a scoped session — so the
    /// affordance stays.
    #[test]
    #[serial_test::serial(process_env)]
    fn naming_the_system_store_too_is_not_a_scoped_session() {
        assert!(with_env(
            Some("/tmp/qos-apps-fixture"),
            Some("/tmp/qos-apps-system"),
            None,
            available_to_this_session
        ));
    }

    /// The screenshot harness has no store of its own to be scoped by,
    /// and still must not photograph the operator's applications.
    #[test]
    #[serial_test::serial(process_env)]
    fn the_shots_switch_stands_the_scan_down_on_its_own() {
        assert!(!with_env(None, None, Some("1"), available_to_this_session));
        assert!(
            with_env(None, None, Some(""), available_to_this_session),
            "an empty value is not a switch somebody set"
        );
    }

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
        // A machine with no desktop entries at all is a legitimate
        // state (a minimal container), so this asserts the *shape* of
        // whatever was found rather than a count.
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
