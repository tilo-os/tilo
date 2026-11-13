//! The terminal: a [`Terminal`] session resource and the [`TerminalWidget`]
//! surface that runs its process. The app is [`TerminalShell`]; the shell
//! also embeds the widget in a tile.

// An app runs in its own sandboxed process, so an integer that wraps here costs one app rather than the
// desktop: overflow is allowed here, while the lints whose fault is a crash stay denied. `shell`, `ui`
// and `crates/` keep the full set: a fault there takes the session, or every app with it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "app-local arithmetic; see the note above"
)]
pub mod actions;
mod emulator;
mod graphics;
pub mod history;
mod i18n;
mod image_loads;
mod native_editor;
pub mod parse;
pub mod preferences;
pub mod resource;
pub mod runs;
#[cfg(feature = "samples")]
mod scenarios;

pub mod screen;
pub mod target;
pub mod undoability;

pub use graphics::TerminalImagePlacement;

pub use crate::inner::{
    Attr, CellColor, GridPos, OpenError, Requester, Selection, TermModes, TerminalBackend,
    TerminalWidget, parse_target_argument,
};

pub use portable_pty::CommandBuilder;

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

#[derive(Clone, Copy, Debug)]
pub struct HostedSessionSource;

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
    pub fn unavailable() -> Self {
        Self {
            #[cfg(unix)]
            ush: None,
        }
    }
}

#[cfg(unix)]
impl HostedProcessServices {
    pub fn has_ush(&self) -> bool {
        self.ush.is_some()
    }

    pub(crate) fn open_session(
        first: Option<Self>,
        target: &target::SessionTarget,
        requested_by: Requester,
        relay_session: Option<&str>,
        workspace: Option<&str>,
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
        crate::inner::ush_backend_on(&stream, &[], target, requested_by, relay_session, workspace)
    }

    pub(crate) fn ask(
        request: &serde_json::Value,
        workspace: Option<&str>,
    ) -> Result<String, String> {
        use std::io::{BufRead, Write};
        let stream = Self::default()
            .ush
            .ok_or_else(|| "the shell runtime is not running".to_string())?;
        let mut request = request.clone();
        sessions::with_workspace(&mut request, workspace);
        let mut line = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
        line.push(b'\n');
        (&stream).write_all(&line).map_err(|e| e.to_string())?;
        let mut reply = String::new();
        std::io::BufReader::new(&stream)
            .read_line(&mut reply)
            .map_err(|e| e.to_string())?;
        Ok(reply)
    }

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

#[cfg(not(target_arch = "wasm32"))]
pub fn mounted_terminal(widget: &mut dyn ui::widget::Widget) -> Option<&mut TerminalWidget> {
    ui::framework::testing::surface_in_root::<TerminalShell, TerminalView>(widget)
        .map(TerminalView::terminal_mut)
}

pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(TerminalShell)
}

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
        _workspace: Option<&str>,
        _cancellation: Option<&runtime_portal::pending_connection::Cancellation>,
    ) -> Result<TerminalBackend, OpenError> {
        Err(OpenError::NoRuntime(
            HostedTerminalError::RequiredServiceMissing("ush").to_string(),
        ))
    }

    pub(crate) fn ask(
        _request: &serde_json::Value,
        _workspace: Option<&str>,
    ) -> Result<String, String> {
        Err("environments are managed through Tilo's shell runtime".into())
    }
}

#[cfg(test)]
mod action_keywords_tests;
