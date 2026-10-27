//! The terminal: a [`Terminal`] session resource and the [`TerminalWidget`]
//! surface that runs its process. The app is [`TerminalShell`]; the shell
//! also embeds the widget in a tile.
//!
//! See [`actions`] for the resource + action-model split: the shared document is
//! the emulator's live-screen projection, while Ctrl+K exposes real PTY controls
//! as non-undoable effects.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "app-local arithmetic; see the note above"
)]
pub mod actions;
mod graphics;
pub mod history;
mod i18n;
mod image_loads;
mod native_editor;
pub mod parse;
pub mod preferences;
pub mod resource;
pub mod runs;
mod scenarios;
pub mod screen;
pub mod target;
pub mod undoability;

pub use graphics::TerminalImagePlacement;

pub use crate::inner::{
    Attr, CellColor, GridPos, OpenError, Requester, Selection, TermModes, TerminalBackend,
    TerminalPerformer, TerminalWidget, parse_target_argument, resolve_login_shell,
};

/// Re-exported so hosts can build a command for [`TerminalWidget::spawn_command`]
/// without taking a direct dependency on `portable-pty`.
pub use portable_pty::CommandBuilder;

// Keep the large implementation private while exposing the stable types above.
mod inner {
    include!("main_body.rs");
    include!("scrollback_ui.rs");
    include!("target_ui.rs");
    include!("window_ui.rs");
}

mod async_open;
pub mod sessions;

mod app_widget_impl;
mod view;

pub use view::TerminalView;

/// A short scripted session, typed into a freshly spawned login shell, that
/// prints a few recognisable lines in that shell's own syntax. Hosts use it to
/// stage screenshots of a live terminal.
#[must_use]
pub fn demo_session_input() -> &'static str {
    #[cfg(windows)]
    {
        "echo Tilo embedded terminal (live PTY)\recho shell: %COMSPEC%\recho demo output:\recho primes below 20: 2 3 5 7 11 13 17 19\recho done\r"
    }
    #[cfg(not(windows))]
    {
        "echo 'Tilo embedded terminal (live PTY)'; echo \"shell: $SHELL\"; \
         echo 'demo output:'; echo '  primes below 20: 2 3 5 7 11 13 17 19'; echo done\n"
    }
}

#[cfg(test)]
mod run_tests;
#[cfg(test)]
mod target_tests;
#[cfg(test)]
pub(crate) mod tests;

/// Permission to discover the session-owned supervisor on the open worker.
/// A real host lends this instead of connecting on its compositor thread.
#[derive(Clone, Copy, Debug)]
pub struct HostedSessionSource;

/// Process connections explicitly granted to one hosted Terminal instance.
#[cfg_attr(not(unix), derive(Default))]
pub struct HostedProcessServices {
    #[cfg(unix)]
    pub ush: Option<std::os::unix::net::UnixStream>,
}

#[cfg(unix)]
impl Default for HostedProcessServices {
    fn default() -> Self {
        let ush = tilo_paths::xdg::runtime_dir()
            .and_then(|runtime| Self::discover_from_runtime_dir(&runtime).ush);
        Self { ush }
    }
}

impl HostedProcessServices {
    /// No process service: a host that shows a terminal (a capture, a
    /// fixture) without letting it open sessions.
    pub fn unavailable() -> Self {
        Self {
            #[cfg(unix)]
            ush: None,
        }
    }
}

#[cfg(unix)]
impl HostedProcessServices {
    /// Whether the host handed over the `ush` supervisor.
    pub fn has_ush(&self) -> bool {
        self.ush.is_some()
    }

    /// Open a session on `target` over `first`, or over a fresh connection
    /// to the session's supervisor once the first has been used.
    pub(crate) fn open_session(
        first: Option<Self>,
        target: &target::SessionTarget,
        requested_by: Requester,
        relay_session: Option<&str>,
        cancellation: Option<&runtime_portal::pending_connection::Cancellation>,
    ) -> Result<TerminalBackend, OpenError> {
        let stream = first
            .and_then(|services| services.ush)
            .or_else(|| Self::default().ush)
            .ok_or_else(|| {
                OpenError::NoRuntime("the required ush supervisor is not running".into())
            })?;
        if let Some(cancellation) = cancellation {
            cancellation
                .track(&stream)
                .map_err(|e| OpenError::Failed(e.to_string()))?;
        }
        crate::inner::ush_backend_on(&stream, &[], target, requested_by, relay_session)
    }

    /// One request to the session's supervisor over a fresh connection;
    /// returns its one-line answer.
    pub(crate) fn ask(request: &serde_json::Value) -> Result<String, String> {
        use std::io::{BufRead, Write};
        let stream = Self::default()
            .ush
            .ok_or_else(|| "the shell runtime is not running".to_string())?;
        let mut line = serde_json::to_vec(request).map_err(|e| e.to_string())?;
        line.push(b'\n');
        (&stream).write_all(&line).map_err(|e| e.to_string())?;
        let mut reply = String::new();
        std::io::BufReader::new(&stream)
            .read_line(&mut reply)
            .map_err(|e| e.to_string())?;
        Ok(reply)
    }

    /// Discover the session-owned `ush` supervisor below one runtime directory.
    ///
    /// This stays crate-visible so the socket lookup can be tested without
    /// mutating `XDG_RUNTIME_DIR`, which is process-global and shared by the
    /// terminal test suite. Production callers use [`Default`].
    pub(crate) fn discover_from_runtime_dir(runtime: &std::path::Path) -> Self {
        Self {
            ush: discover_ush(runtime).ok(),
        }
    }
}

#[cfg(unix)]
const USH_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);

/// Connect the owner-only session socket without letting a stale or saturated
/// endpoint stall shell construction indefinitely.
#[cfg(unix)]
fn discover_ush(runtime: &std::path::Path) -> std::io::Result<std::os::unix::net::UnixStream> {
    let socket = runtime.join("tilo/services/ush.sock");
    let stream =
        tilo_process::local_socket::connect_owner_only(runtime, &socket, USH_CONNECT_TIMEOUT)?;
    stream.set_read_timeout(Some(USH_CONNECT_TIMEOUT))?;
    stream.set_write_timeout(Some(USH_CONNECT_TIMEOUT))?;
    Ok(stream)
}

/// A required provider is missing or refused the session. Hosts may retry with a fresh grant.
#[derive(Debug)]
pub enum HostedTerminalError {
    RequiredServiceMissing(&'static str),
    ServiceFailed(String),
}
impl std::fmt::Display for HostedTerminalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RequiredServiceMissing(name) => {
                write!(f, "required process service {name} is unavailable")
            }
            Self::ServiceFailed(detail) => write!(f, "Terminal process service failed: {detail}"),
        }
    }
}
impl std::error::Error for HostedTerminalError {}
pub use resource::{Terminal, TerminalEffect, TerminalShell};

/// The terminal a host mounted, when `widget` is the root of a terminal
/// tab: for a host's test that types into the terminal it shows.
#[cfg(not(target_arch = "wasm32"))]
pub fn mounted_terminal(widget: &mut dyn ui::widget::Widget) -> Option<&mut TerminalWidget> {
    ui::framework::testing::surface_in_root::<TerminalShell, TerminalView>(widget)
        .map(TerminalView::terminal_mut)
}

/// The terminal's application definition: the one every host mounts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(TerminalShell)
}

/// Without a hosted process-service transport nothing opens here.
#[cfg(not(unix))]
impl HostedProcessServices {
    /// Whether the host handed over the `ush` supervisor: never, here.
    pub fn has_ush(&self) -> bool {
        false
    }

    pub(crate) fn open_session(
        _first: Option<Self>,
        _target: &target::SessionTarget,
        _requested_by: Requester,
        _relay_session: Option<&str>,
        _cancellation: Option<&runtime_portal::pending_connection::Cancellation>,
    ) -> Result<TerminalBackend, OpenError> {
        Err(OpenError::NoRuntime(
            HostedTerminalError::RequiredServiceMissing("ush").to_string(),
        ))
    }

    pub(crate) fn ask(_request: &serde_json::Value) -> Result<String, String> {
        Err("environments are managed through Tilo's shell runtime".into())
    }
}
