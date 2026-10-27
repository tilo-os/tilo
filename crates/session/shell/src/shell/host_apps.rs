//! Starting applications Tilo did not package.
//!
//! Counterpart to [`crate::shell::launcher`], which starts `.tapp`
//! packages; both exec `tilo-launcher` and report through the same
//! [`launcher::LaunchEvent`] queue.
//!
//! `tilo-launcher run-host` builds **no sandbox** — nesting a Flatpak's
//! own sandbox inside Tilo's baseline filter just fails to start. A host
//! app gets no room id, no capability file, no per-room state
//! directory. `launch` accepts only an opaque portal-attribution room string:
//! it lets an existing Flatpak's authenticated portal request resolve to the
//! workspace containing its window, but is not passed to the app and grants
//! no Tilo service or sandbox authority. Search lists host apps with every
//! other app and states on each row which kind of app it is (Flatpak,
//! `AppImage`, program or part of the system). Software that cannot speak Wayland reaches the
//! private rootless-Xwayland display of the workspace it opens in, and only
//! that one: X11 has no client isolation, so one X server never spans two
//! workspaces.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use desktop_entry::DesktopEntry;

use crate::shell::launcher::{self, LaunchError};
use tilo_lock::MutexRecover as _;

/// A projection of [`DesktopEntry`] holding only what the shell needs,
/// testable against a fixed list without real `.desktop` files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostApp {
    /// The identity `run-host` resolves and window correlation matches on.
    pub desktop_id: String,
    pub name: String,
    /// What kind of app it is, the same answer the person's app list gives.
    pub kind: desktop_entry::EntryKind,
    pub categories: Vec<String>,
    pub keywords: Vec<String>,
    /// Every string this app's window might announce itself as. See
    /// [`DesktopEntry::window_identities`].
    pub window_identities: Vec<String>,
    /// `MimeType=`: what the entry says it opens, including
    /// `x-scheme-handler/*` for URI schemes.
    pub mime_types: Vec<String>,
    /// `Terminal=true`: a console program, run inside the Tilo terminal.
    pub terminal: bool,
    /// `Icon=`: an icon-theme name or an absolute path, resolved by
    /// [`crate::app_icons`].
    pub icon: Option<String>,
    /// For a program the person downloaded: the file it starts and whether
    /// that file is still there ([`desktop_entry::downloaded`]).
    pub downloaded: Option<desktop_entry::downloaded::DownloadedProgram>,
}

/// A palette launch held until the compositor can mint its external
/// `xdg_activation_v1` token on the compositor thread.
#[derive(Debug, Clone)]
pub struct QueuedLaunch {
    pub app: HostApp,
    pub placement: crate::shell::tab_binding::Placement,
    pub portal_workspace_id: Option<String>,
    pub requested_at: std::time::Instant,
    /// Whether an app that is still running in the tray with no window may
    /// be reopened through its tray item (`reopen_from_tray`) instead.
    /// Cleared on the launch that follows an item refusing.
    pub reopen_from_tray: bool,
}

/// Whether `tree` shows a window of `app`.
pub(crate) fn has_window(app: &HostApp, tree: &crate::shell::state::WorkspaceTree) -> bool {
    tree.desktop
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .any(|tab| match &tab.tile {
            crate::shell::state::TileContent::Foreign { app_id, .. } => app
                .window_identities
                .iter()
                .any(|identity| identity.eq_ignore_ascii_case(app_id.trim())),
            _ => false,
        })
}

/// Reopen an app that lives on in the tray after its last window closed
/// (1Password, Discord, Steam, `KeePassXC`) by activating its tray item, as
/// clicking that icon would, rather than starting a second copy. A second
/// copy only finds the first one running, hands over and exits; for an
/// Electron app in a Flatpak that is a full sandbox and browser start, many
/// seconds on a loaded machine, before anything appears.
///
/// Only an item whose owner is proven to be this app counts
/// ([`tilo_tray_host::ItemAddress::owner_app`]), and only while the app shows
/// no window: with a window open, a launch asks it for a new one. The
/// placement the request staged is kept, so the window that the item shows
/// opens where the launch would have put it. If the item refuses, the
/// launch is queued again without this step and `wake` runs the loop.
///
/// Returns whether the item was asked.
pub(crate) fn reopen_from_tray(
    request: &QueuedLaunch,
    tray: &tilo_tray_host::Tray,
    tree: &crate::shell::state::WorkspaceTree,
    wake: crate::session_runtime::Wake,
) -> bool {
    if !request.reopen_from_tray || has_window(&request.app, tree) {
        return false;
    }
    let mut identities = request.app.window_identities.clone();
    identities.push(request.app.desktop_id.clone());
    let Some(item) = tray.item_owned_by(&identities) else {
        return false;
    };
    let mut retry = request.clone();
    retry.reopen_from_tray = false;
    let asked = tray.request_or_else(
        &item.key,
        tilo_tray_host::TrayRequest::Activate { x: 0, y: 0 },
        move |error| {
            tracing::warn!(
                target: "tilo::app_launch",
                desktop_id = %retry.app.desktop_id,
                %error,
                "the tray item refused to reopen its app; starting it instead"
            );
            if queue_launch(retry).is_ok() {
                wake();
            }
        },
    );
    match asked {
        Ok(()) => {
            tracing::info!(
                target: "tilo::app_launch",
                desktop_id = %request.app.desktop_id,
                tray_item = %item.key,
                "reopening a running app through its tray item instead of starting a second copy"
            );
            true
        }
        Err(error) => {
            tracing::debug!(
                target: "tilo::app_launch",
                desktop_id = %request.app.desktop_id,
                %error,
                "tray item unavailable; starting the app"
            );
            false
        }
    }
}

fn queued_launches() -> &'static std::sync::Mutex<Vec<QueuedLaunch>> {
    static QUEUED: OnceLock<std::sync::Mutex<Vec<QueuedLaunch>>> = OnceLock::new();
    QUEUED.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

#[cfg_attr(
    target_os = "linux",
    expect(
        clippy::unnecessary_wraps,
        reason = "the shared provider-selection interface must return typed unavailability on other hosts"
    )
)]
fn require_host_execution() -> Result<(), LaunchError> {
    #[cfg(target_os = "linux")]
    {
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(LaunchError::ProviderUnavailable {
            provider: "Linux host application execution",
        })
    }
}

pub(crate) fn queue_launch(request: QueuedLaunch) -> Result<(), LaunchError> {
    require_host_execution()?;
    queued_launches().lock_recover().push(request);
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn drain_queued_launches() -> Vec<QueuedLaunch> {
    std::mem::take(&mut *queued_launches().lock_recover())
}

/// Take the queued launches of `desktop_id` only, leaving other tests'.
#[cfg(test)]
pub(crate) fn take_queued_launches_of(desktop_id: &str) -> Vec<QueuedLaunch> {
    let mut queued = queued_launches().lock_recover();
    let (taken, kept) = std::mem::take(&mut *queued)
        .into_iter()
        .partition(|request| request.app.desktop_id == desktop_id);
    *queued = kept;
    taken
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct X11Session {
    pub display: String,
    pub authority: PathBuf,
}

fn push_x11_args(args: &mut Vec<String>, x11: &X11Session) {
    args.push("--x11-display".to_string());
    args.push(x11.display.clone());
    args.push("--x11-authority".to_string());
    args.push(x11.authority.display().to_string());
}

/// Give a host launch its workspace's X11 display, on the launch's own
/// thread ([`launcher::LaunchCommand::x11_workspace`]).
pub(crate) fn attach_x11_display(command: &mut launcher::LaunchCommand) {
    let Some(workspace) = command.x11_workspace.take() else {
        return;
    };
    if let Some(x11) = x11_session_for(Some(&workspace)) {
        push_x11_args(&mut command.args, &x11);
    }
}

/// The X11 connection a host launch into `workspace` may inherit: that
/// workspace's own display, created on first use. A launch attributed to no
/// workspace gets none, and callers never fall back to ambient
/// DISPLAY/XAUTHORITY.
fn x11_session_for(workspace: Option<&str>) -> Option<X11Session> {
    #[cfg(target_os = "linux")]
    {
        workspace.and_then(crate::compositor::xwayland::session_for_workspace)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = workspace;
        None
    }
}

impl HostApp {
    pub(crate) fn from_entry(entry: &DesktopEntry) -> Self {
        Self {
            desktop_id: entry.id.clone(),
            name: entry.name.clone(),
            kind: entry.kind(
                tilo_paths::xdg::data_home()
                    .map(|home| home.join("applications"))
                    .as_deref(),
            ),
            categories: entry.categories.clone(),
            keywords: entry.keywords.clone(),
            window_identities: entry.window_identities(),
            mime_types: entry.mime_types.clone(),
            terminal: entry.terminal,
            icon: entry.icon.clone(),
            downloaded: entry.downloaded.clone(),
        }
    }

    /// What kind of app it is, in the words the person's app list uses.
    pub fn kind_label(&self) -> String {
        crate::shell::i18n::t(match self.kind {
            desktop_entry::EntryKind::Flatpak => "app-kind-flatpak",
            desktop_entry::EntryKind::AppImage => "app-kind-appimage",
            desktop_entry::EntryKind::Program => "app-kind-program",
            desktop_entry::EntryKind::System => "app-kind-system",
        })
    }

    pub fn is_flatpak(&self) -> bool {
        self.kind == desktop_entry::EntryKind::Flatpak
    }
}

pub fn host_apps() -> Vec<HostApp> {
    deduplicate(
        desktop_entry::cache::cached_scan()
            .entries
            .iter()
            .map(HostApp::from_entry)
            .collect(),
    )
}

/// Host-app source snapshot paired with the desktop-entry generation used to
/// construct it.  The provider retains this projection between queries.
pub fn host_apps_snapshot() -> (u64, Vec<HostApp>) {
    let (generation, scan) = desktop_entry::cache::cached_scan_snapshot();
    (
        generation,
        deduplicate(scan.entries.iter().map(HostApp::from_entry).collect()),
    )
}

/// Current host-app source generation, without cloning the scan.
#[must_use]
pub fn host_apps_generation() -> u64 {
    desktop_entry::cache::cached_scan_generation()
}

/// Collapse packaging aliases while preserving genuinely distinct launch
/// modes. A shared display name alone is not identity; a shared declared
/// window identity is. Prefer Flatpak when a user/system export and a distro
/// file both describe the same app because it retains its original sandbox.
pub(crate) fn deduplicate(apps: Vec<HostApp>) -> Vec<HostApp> {
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

/// Resolve a durable host-app identity. New saves use the exact desktop-file
/// id, while older state may still carry a window-system identity.
pub fn resolve(identity: &str) -> Option<HostApp> {
    find(identity).or_else(|| app_for_window_id(identity))
}

/// Pure — no I/O, so the exact command is assertable. No
/// `--workspace-id` and no `--capabilities`: a host app is in no room
/// and confined by nothing Tilo controls, so passing either would
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
        push_x11_args(&mut args, x11);
    }
    let mut env = vec![(
        "XDG_RUNTIME_DIR".to_string(),
        launcher::runtime_dir().display().to_string(),
    )];
    if let Some(token) = activation_token.filter(|token| !token.is_empty()) {
        // The launcher is an intermediary. Keep this out of argv/process
        // listings; it converts it into the standard child-facing variable.
        env.push(("TILO_HOST_ACTIVATION_TOKEN".to_string(), token.to_string()));
    }
    launcher::LaunchCommand {
        program: launcher_bin.to_path_buf(),
        args,
        env,
        service_identity: None,
        portal_workspace_id,
        managed_agent_id: None,
        x11_workspace: None,
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
/// when the compositor or `tilo-launcher` binary can't be reached.
pub fn launch(
    desktop_id: &str,
    portal_workspace_id: Option<String>,
) -> Result<HostApp, LaunchError> {
    launch_with_document_and_activation(desktop_id, None, portal_workspace_id, None)
}

/// Unlike a Tilo launch there's no grant to make alongside it — the app
/// already has the user's filesystem, which is what makes it a host app.
///
/// # Errors
///
/// Returns [`LaunchError::HostAppNotFound`] when `desktop_id` isn't
/// registered, or [`LaunchError::NoDisplay`]/a launcher-resolution error
/// when the compositor or `tilo-launcher` binary can't be reached.
pub fn launch_with_document(
    desktop_id: &str,
    document: Option<&Path>,
    portal_workspace_id: Option<String>,
) -> Result<HostApp, LaunchError> {
    launch_with_document_and_activation(desktop_id, document, portal_workspace_id, None)
}

/// Host launch with a compositor-minted `xdg_activation_v1` token. The token
/// travels only as the standard desktop environment variable and grants no
/// Tilo sandbox or service authority.
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
    if let Err(error) = require_host_execution() {
        launcher::clear_pending_placement(desktop_id);
        return Err(error);
    }
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
    launch_resolved_with_activation(app, document, portal_workspace_id, activation_token)
}

/// Launch an entry already resolved by the catalogue/search provider. This
/// preserves the exact trusted projection selected by the user and removes a
/// second catalogue lookup between request and spawn.
pub(crate) fn launch_resolved_with_activation(
    app: HostApp,
    document: Option<&Path>,
    portal_workspace_id: Option<String>,
    activation_token: Option<&str>,
) -> Result<HostApp, LaunchError> {
    launch_resolved(
        app,
        document.map(HostOpen::Document),
        portal_workspace_id,
        activation_token,
    )
}

/// What a host launch hands the application, if anything.
#[derive(Debug, Clone, Copy)]
pub enum HostOpen<'a> {
    Document(&'a Path),
    Url(&'a str),
}

/// Open `url` in a host application, the way a host `x-scheme-handler` is used.
///
/// # Errors
///
/// Returns the same launch-resolution errors as [`launch_with_document`].
pub fn launch_with_url(
    desktop_id: &str,
    url: &str,
    portal_workspace_id: Option<String>,
) -> Result<HostApp, LaunchError> {
    require_host_execution()?;
    let app = find(desktop_id).ok_or_else(|| LaunchError::HostAppNotFound {
        desktop_id: desktop_id.to_string(),
    })?;
    launch_resolved(app, Some(HostOpen::Url(url)), portal_workspace_id, None)
}

/// The argv a `Terminal=true` entry runs, with `%f`/`%u` expanded exactly as
/// an unconfined launch would.
fn terminal_argv(app: &HostApp, open: Option<HostOpen<'_>>) -> Result<Vec<String>, LaunchError> {
    let entry = desktop_entry::cache::cached_scan()
        .find(&app.desktop_id)
        .cloned()
        .ok_or_else(|| LaunchError::HostAppNotFound {
            desktop_id: app.desktop_id.clone(),
        })?;
    let env = desktop_entry::launch::LaunchEnv {
        files: match open {
            Some(HostOpen::Document(path)) => vec![path.display().to_string()],
            _ => Vec::new(),
        },
        urls: match open {
            Some(HostOpen::Url(url)) => vec![url.to_string()],
            _ => Vec::new(),
        },
        ..desktop_entry::launch::LaunchEnv::default()
    };
    desktop_entry::launch::terminal_argv(&entry, &env).map_err(|error| {
        LaunchError::HostAppRefused {
            desktop_id: app.desktop_id.clone(),
            detail: error.to_string(),
        }
    })
}

fn launch_resolved(
    app: HostApp,
    open: Option<HostOpen<'_>>,
    portal_workspace_id: Option<String>,
    activation_token: Option<&str>,
) -> Result<HostApp, LaunchError> {
    if app.terminal {
        // The window that appears is the Terminal's, not one announcing this
        // entry's identities, so a placement noted for them would go stale.
        launcher::clear_pending_placement(&app.desktop_id);
        let argv = terminal_argv(&app, open)?;
        let room = portal_workspace_id
            .as_deref()
            .and_then(crate::shell::state::RoomId::parse)
            .ok_or(LaunchError::NoWorkspace)?;
        // Queued: the caller can be the compositor thread.
        launcher::queue_terminal_command(&argv, room)?;
        return Ok(app);
    }
    let document = match open {
        Some(HostOpen::Document(path)) => Some(path),
        _ => None,
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
    let x11_workspace = portal_workspace_id.clone();
    let mut command = build_launch_command(
        &app,
        &launcher_bin,
        &socket,
        document,
        None,
        portal_workspace_id,
        activation_token,
    );
    command.x11_workspace = x11_workspace;
    if let Some(HostOpen::Url(url)) = open {
        command.args.push("--open-url".to_string());
        command.args.push(url.to_string());
    }
    // Selecting an app asks for a window. A singleton may remain alive after
    // its last surface closes, so prefer an explicit desktop New Window
    // action when the entry offers one.
    command.args.push("--new-window".to_string());

    launcher::remember_launch_named(&app.desktop_id, &app.window_identities, &app.name);
    let _ = launcher::spawn_launch(app.desktop_id.clone(), app.name.clone(), command);
    Ok(app)
}

/// Run the program file at `program` itself, unconfined like every host
/// application: a downloaded `AppImage` or executable opened in Files. The
/// launcher makes it runnable first. Returns the name it runs under (the
/// file's), as soon as the launch is handed to its worker.
///
/// # Errors
///
/// No display, no launcher, or no host execution on this platform.
pub(crate) fn launch_program(
    program: &Path,
    portal_workspace_id: Option<String>,
) -> Result<String, LaunchError> {
    require_host_execution()?;
    let socket = launcher::wayland_socket().ok_or(LaunchError::NoDisplay)?;
    let launcher_bin = launcher::resolve_launcher_bin()?;
    let command = build_program_command(program, &launcher_bin, &socket, portal_workspace_id);
    let name = program
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let _ = launcher::spawn_launch(name.clone(), name.clone(), command);
    Ok(name)
}

/// The `tilo-launcher run-host --program` command for [`launch_program`].
/// Pure, like [`build_launch_command`].
pub(crate) fn build_program_command(
    program: &Path,
    launcher_bin: &Path,
    wayland_socket: &Path,
    portal_workspace_id: Option<String>,
) -> launcher::LaunchCommand {
    let name = program
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    launcher::LaunchCommand {
        program: launcher_bin.to_path_buf(),
        args: vec![
            "run-host".to_string(),
            "--desktop-id".to_string(),
            name,
            "--program".to_string(),
            program.display().to_string(),
            "--wayland-socket".to_string(),
            wayland_socket.display().to_string(),
        ],
        env: vec![(
            "XDG_RUNTIME_DIR".to_string(),
            launcher::runtime_dir().display().to_string(),
        )],
        service_identity: None,
        x11_workspace: portal_workspace_id.clone(),
        portal_workspace_id,
        managed_agent_id: None,
    }
}

/// The `tilo-launcher run-host` command for an XDG autostart entry. It names
/// the file itself, since autostart entries need not be in any
/// `applications` directory. Pure, like [`build_launch_command`].
pub(crate) fn build_autostart_command(
    entry: &DesktopEntry,
    launcher_bin: &Path,
    wayland_socket: &Path,
    x11: Option<&X11Session>,
    portal_workspace_id: Option<String>,
) -> launcher::LaunchCommand {
    let mut command = build_launch_command(
        &HostApp::from_entry(entry),
        launcher_bin,
        wayland_socket,
        None,
        x11,
        portal_workspace_id,
        None,
    );
    command.args.push("--desktop-file".to_string());
    command.args.push(entry.path.display().to_string());
    command
}

/// Answers autostart's questions about this machine.
struct HostAutostartProbe;

impl desktop_entry::autostart::AutostartProbe for HostAutostartProbe {
    fn program_exists(&self, program: &str) -> bool {
        desktop_entry::autostart::program_on_path(program)
    }

    /// `gsettings get`, bounded: login must not wait on a wedged dconf.
    fn gsettings_bool(&self, schema: &str, key: &str) -> Option<bool> {
        let mut command = std::process::Command::new("gsettings");
        command.args(["get", schema, key]);
        let output = bounded_command::run_prepared(
            command,
            None,
            4 * 1024,
            std::time::Duration::from_secs(2),
        )
        .ok()?;
        if !output.status.success() {
            return None;
        }
        match String::from_utf8_lossy(&output.stdout).trim() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    }

    fn config_file_exists(&self, relative: &str) -> bool {
        desktop_entry::autostart::config_file_exists(relative)
    }
}

/// Start the session's XDG autostart entries, attributing their portal
/// requests to `portal_workspace_id`. Console programs (`Terminal=true`)
/// are not started at login: a terminal window nobody asked for is noise.
///
/// # Errors
///
/// Returns [`LaunchError::NoDisplay`] or a launcher-resolution error when
/// nothing can be started at all.
pub fn launch_autostart(portal_workspace_id: Option<&str>) -> Result<usize, LaunchError> {
    require_host_execution()?;
    let socket = launcher::wayland_socket().ok_or(LaunchError::NoDisplay)?;
    let launcher_bin = launcher::resolve_launcher_bin()?;
    let scan = desktop_entry::autostart::scan_autostart(
        &desktop_entry::autostart::autostart_dirs(),
        &HostAutostartProbe,
    );
    for (path, reason) in &scan.skipped {
        tracing::info!(path = %path.display(), ?reason, "autostart entry not started");
    }
    let startable: Vec<&desktop_entry::autostart::Autostart> = scan
        .start
        .iter()
        .filter(|start| {
            if start.entry.terminal {
                tracing::info!(id = %start.entry.id, "autostart console program not started");
            }
            !start.entry.terminal
        })
        .collect();
    for start in &startable {
        let entry = &start.entry;
        let mut command = build_autostart_command(
            entry,
            &launcher_bin,
            &socket,
            None,
            portal_workspace_id.map(str::to_string),
        );
        command.x11_workspace = portal_workspace_id.map(str::to_string);
        launcher::note_session_helper(&entry.id);
        if start.auto_restart {
            launcher::note_restartable_session_helper(&entry.id, &entry.name, command.clone());
        }
        launcher::remember_launch_named(&entry.id, &entry.window_identities(), &entry.name);
        let _ = launcher::spawn_launch(entry.id.clone(), entry.name.clone(), command);
    }
    Ok(startable.len())
}

/// A foreign toplevel arrives with an `app_id` Tilo didn't mint, so the
/// note is keyed on every identity the entry might use.
pub fn note_placement(app: &HostApp, placement: crate::shell::tab_binding::Placement) {
    launcher::set_pending_placement_for_identities(
        &app.desktop_id,
        &app.window_identities,
        placement,
    );
}

/// Consulted when a foreign toplevel is adopted, so its tab shows
/// "Spotify" not "spotify", and so the compositor can tell a host app's
/// window from a Tilo app's at all.
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

    #[test]
    fn a_window_counts_for_the_app_whichever_identity_it_announces() {
        let password_manager = app(
            "com.onepassword.OnePassword.desktop",
            "1Password",
            &[
                "1Password",
                "com.onepassword.OnePassword.desktop",
                "com.onepassword.OnePassword",
            ],
        );
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        assert!(!has_window(&password_manager, &tree), "an empty desktop");
        crate::shell::tab_binding::adopt_toplevel_at(
            &mut tree,
            7,
            Some("org.gnome.TextEditor".into()),
            crate::shell::tab_binding::Placement::NewGroup,
        );
        assert!(
            !has_window(&password_manager, &tree),
            "another app's window"
        );
        crate::shell::tab_binding::adopt_toplevel_at(
            &mut tree,
            8,
            Some("1password".into()),
            crate::shell::tab_binding::Placement::NewGroup,
        );
        assert!(
            has_window(&password_manager, &tree),
            "an X11 window announcing its WM class, in any case"
        );
    }

    fn app(desktop_id: &str, name: &str, identities: &[&str]) -> HostApp {
        HostApp {
            desktop_id: desktop_id.to_string(),
            name: name.to_string(),
            kind: desktop_entry::EntryKind::Flatpak,
            categories: Vec::new(),
            keywords: Vec::new(),
            mime_types: Vec::new(),
            terminal: false,
            icon: None,
            downloaded: None,
            window_identities: identities
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
        }
    }

    #[test]
    fn host_launches_carry_no_room_or_capabilities_and_bridge_x11() {
        fn a_host_launch_command_carries_no_room_and_no_capabilities() {
            let app = app("com.spotify.Client", "Spotify", &["spotify"]);
            let cmd = build_launch_command(
                &app,
                Path::new("/usr/libexec/tilo-launcher"),
                Path::new("/run/user/1000/wayland-1"),
                None,
                None,
                None,
                None,
            );
            assert_eq!(cmd.program, PathBuf::from("/usr/libexec/tilo-launcher"));
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

        fn a_document_rides_across_as_open() {
            let app = app(
                "org.gnome.TextEditor",
                "Text Editor",
                &["org.gnome.TextEditor"],
            );
            let cmd = build_launch_command(
                &app,
                Path::new("/usr/libexec/tilo-launcher"),
                Path::new("/run/user/1000/wayland-1"),
                Some(Path::new("/home/u/notes.txt")),
                None,
                None,
                None,
            );
            assert_eq!(cmd.args[5], "--open");
            assert_eq!(cmd.args[6], "/home/u/notes.txt");
        }

        fn an_x11_bridge_rides_to_the_unconfined_launcher() {
            let app = app("com.valvesoftware.Steam", "Steam", &["steam"]);
            let x11 = X11Session {
                display: ":1234".into(),
                authority: PathBuf::from("/run/user/1000/tilo-session/Xauthority-1234"),
            };
            let cmd = build_launch_command(
                &app,
                Path::new("/usr/libexec/tilo/tilo-launcher"),
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
                    "/run/user/1000/tilo-session/Xauthority-1234",
                ]
            }));
            assert_eq!(cmd.portal_workspace_id.as_deref(), Some("room-7"));
            assert!(
                !cmd.env.iter().any(|(key, _)| key.contains("WORKSPACE")),
                "portal attribution is compositor bookkeeping, not ambient authority"
            );
        }

        a_host_launch_command_carries_no_room_and_no_capabilities();
        a_document_rides_across_as_open();
        an_x11_bridge_rides_to_the_unconfined_launcher();
    }

    #[test]
    fn host_scan_identities_aliases_and_adoption_are_canonical() {
        /// Scan the host for installed applications.
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

        fn cross_source_aliases_are_one_app_and_keep_flatpak_confinement() {
            let system = HostApp {
                kind: desktop_entry::EntryKind::System,
                ..app("vendor-chat", "Chat", &["Chat"])
            };
            let flatpak = HostApp {
                kind: desktop_entry::EntryKind::Flatpak,
                ..app("org.example.Chat", "Chat", &["org.example.Chat", "Chat"])
            };
            let apps = deduplicate(vec![system, flatpak.clone()]);
            assert_eq!(apps, vec![flatpak]);
        }

        fn same_named_distinct_modes_remain_visible() {
            let apps = deduplicate(vec![
                app("org.example.Editor", "Editor", &["Editor"]),
                app("org.example.EditorSafe", "Editor", &["EditorSafe"]),
            ]);
            assert_eq!(apps.len(), 2);
        }

        fn window_adoption_uses_the_canonical_alias() {
            let system = HostApp {
                kind: desktop_entry::EntryKind::System,
                ..app("vendor-chat", "Chat", &["Chat"])
            };
            let flatpak = HostApp {
                kind: desktop_entry::EntryKind::Flatpak,
                ..app("org.example.Chat", "Chat", &["org.example.Chat", "Chat"])
            };
            let canonical = deduplicate(vec![system, flatpak.clone()]);
            assert_eq!(app_for_window_id_in("Chat", canonical), Some(flatpak));
        }

        the_host_scan_returns_entries_with_usable_identities();
        cross_source_aliases_are_one_app_and_keep_flatpak_confinement();
        same_named_distinct_modes_remain_visible();
        window_adoption_uses_the_canonical_alias();
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn a_refused_host_launch_cannot_leave_a_stale_placement() {
        const MISSING: &str = "org.tilo.test.definitely-not-a-desktop-entry";
        launcher::clear_pending_placements();
        launcher::set_pending_placement(MISSING, crate::shell::tab_binding::Placement::NewGroup);

        let result = launch(MISSING, None);
        #[cfg(target_os = "linux")]
        assert!(matches!(result, Err(LaunchError::HostAppNotFound { .. })));
        #[cfg(not(target_os = "linux"))]
        assert!(matches!(
            result,
            Err(LaunchError::ProviderUnavailable { .. })
        ));
        assert!(
            launcher::placement_for_toplevel(MISSING, 0xC1_05_ED).is_none(),
            "a later unrelated window must not inherit a launch that was refused"
        );
    }
}

#[cfg(test)]
#[cfg(not(target_os = "linux"))]
mod unavailable_provider_tests {
    use super::*;

    #[test]
    fn unavailable_host_execution_fails_before_catalogue_discovery() {
        assert!(matches!(
            launch("fixture.desktop", None),
            Err(LaunchError::ProviderUnavailable { .. })
        ));
    }

    fn fixture_hosts(dir: &Path) -> Vec<HostApp> {
        let write = |name: &str, body: &str| std::fs::write(dir.join(name), body).unwrap();
        write(
            "org.gnome.Evince.desktop",
            "[Desktop Entry]\nType=Application\nName=Document Viewer\nExec=evince %U\n\
             MimeType=application/pdf;image/tiff;\n",
        );
        write(
            "okular.desktop",
            "[Desktop Entry]\nType=Application\nName=Okular\nExec=okular %U\n\
             MimeType=application/pdf;\n",
        );
        write(
            "mutt.desktop",
            "[Desktop Entry]\nType=Application\nName=Mutt\nExec=mutt %u\nTerminal=true\n\
             MimeType=x-scheme-handler/mailto;\n",
        );
        desktop_entry::scan_dirs(&[dir.to_path_buf()], &["tilo".to_string()])
            .entries
            .iter()
            .map(HostApp::from_entry)
            .collect()
    }

    #[test]
    fn host_mime_claims_and_mimeapps_defaults_feed_the_resolver() {
        use desktop_policy::{MimeAppsChain, MimeAppsList, PersonalDefaults, ResourceKind};
        let dir = tempfile::tempdir().unwrap();
        let hosts = fixture_hosts(dir.path());
        let mutt = hosts.iter().find(|host| host.desktop_id == "mutt").unwrap();
        assert!(mutt.terminal, "Terminal=true is carried to the launch path");

        let chain = MimeAppsChain::new(vec![MimeAppsList::parse(
            "[Default Applications]\napplication/pdf=okular.desktop;\n\
             [Removed Associations]\nimage/tiff=org.gnome.Evince.desktop;\n\
             [Added Associations]\ntext/plain=okular.desktop;\n",
        )]);
        let candidates = crate::shell::launcher::host_app_candidates(&hosts, &chain);
        let evince = candidates
            .iter()
            .find(|c| c.package_id == "org.gnome.Evince.desktop")
            .unwrap();
        assert!(evince.handles(&ResourceKind::mime("application/pdf").unwrap()));
        assert!(
            !evince.handles(&ResourceKind::mime("image/tiff").unwrap()),
            "a removed association no longer counts"
        );
        let okular = candidates
            .iter()
            .find(|c| c.package_id == "okular.desktop")
            .unwrap();
        assert!(
            okular.handles(&ResourceKind::mime("text/plain").unwrap()),
            "an added association counts"
        );
        let mutt = candidates
            .iter()
            .find(|c| c.package_id == "mutt.desktop")
            .unwrap();
        assert!(mutt.handles(&ResourceKind::scheme("mailto").unwrap()));

        let pdf = ResourceKind::mime("application/pdf").unwrap();
        let selected = desktop_policy::DefaultAppResolver::new(&candidates)
            .with_system_defaults(&chain)
            .resolve(&pdf, "", &PersonalDefaults::default(), None)
            .unwrap();
        assert_eq!(selected.package_id, "okular.desktop");

        let personal = PersonalDefaults {
            handlers: [(pdf, "org.gnome.Evince.desktop".to_string())].into(),
        };
        let published =
            crate::shell::launcher::resolved_host_defaults(&candidates, &chain, &personal);
        assert_eq!(
            published.defaults.get("application/pdf"),
            Some(&vec!["org.gnome.Evince.desktop".to_string()]),
            "the published list carries the person's choice for host tools"
        );
        assert_eq!(
            published.defaults.get("x-scheme-handler/mailto"),
            Some(&vec!["mutt.desktop".to_string()])
        );
    }

    #[test]
    fn a_package_claim_is_not_published_to_host_tools() {
        use desktop_policy::{AppCandidate, MimeAppsChain, PersonalDefaults, ResourceKind};
        let dir = tempfile::tempdir().unwrap();
        let hosts = fixture_hosts(dir.path());
        let chain = MimeAppsChain::default();
        let mut candidates = crate::shell::launcher::host_app_candidates(&hosts, &chain);
        candidates.push(AppCandidate {
            package_id: "so.tilo.pdf-viewer".into(),
            display_name: "PDF Viewer".into(),
            resources: [ResourceKind::mime("application/pdf").unwrap()].into(),
            secondary: false,
        });
        let published = crate::shell::launcher::resolved_host_defaults(
            &candidates,
            &chain,
            &PersonalDefaults::default(),
        );
        assert!(!published.defaults.contains_key("application/pdf"));
        assert!(published.defaults.contains_key("image/tiff"));
    }

    #[test]
    fn an_autostart_entry_is_started_by_its_file_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sync-client.desktop");
        std::fs::write(
            &path,
            "[Desktop Entry]\nType=Application\nName=Sync\nExec=sync-client --background\n",
        )
        .unwrap();
        let scan = desktop_entry::autostart::scan_autostart(
            &[dir.path().to_path_buf()],
            &HostAutostartProbe,
        );
        let [start] = scan.start.as_slice() else {
            panic!("one entry to start: {scan:?}");
        };
        let command = build_autostart_command(
            &start.entry,
            Path::new("/usr/libexec/tilo/tilo-launcher"),
            Path::new("/run/user/1000/wayland-1"),
            None,
            Some("room".to_string()),
        );
        assert_eq!(
            command.args,
            vec![
                "run-host",
                "--desktop-id",
                "sync-client",
                "--wayland-socket",
                "/run/user/1000/wayland-1",
                "--desktop-file",
                &path.display().to_string(),
            ]
        );
        assert_eq!(command.portal_workspace_id.as_deref(), Some("room"));
    }
}
