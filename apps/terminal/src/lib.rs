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
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
mod graphics;
mod native_editor;
pub mod resource;
mod scenarios;
pub mod screen;
pub mod target;

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
    include!("target_ui.rs");
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
    ///
    /// # Errors
    /// Returns why the session did not open.
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
    ///
    /// # Errors
    /// Returns why the supervisor could not be asked.
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
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    if !runtime.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "XDG_RUNTIME_DIR is not absolute",
        ));
    }
    let tilo = runtime.join("tilo");
    let services = tilo.join("services");
    let socket = services.join("ush.sock");
    // SAFETY: `geteuid` reads the current process credential and has no memory
    // or descriptor ownership effects.
    let owner = unsafe { libc::geteuid() };
    for directory in [runtime, tilo.as_path(), services.as_path()] {
        let metadata = std::fs::symlink_metadata(directory)?;
        if !metadata.file_type().is_dir() || metadata.uid() != owner || metadata.mode() & 0o077 != 0
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "ush service directory is not private to this user",
            ));
        }
    }
    let metadata = std::fs::symlink_metadata(&socket)?;
    if !metadata.file_type().is_socket() || metadata.uid() != owner || metadata.mode() & 0o077 != 0
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "ush service socket is not owner-only",
        ));
    }

    let stream = connect_unix_socket_bounded(&socket, USH_CONNECT_TIMEOUT)?;
    stream.set_read_timeout(Some(USH_CONNECT_TIMEOUT))?;
    stream.set_write_timeout(Some(USH_CONNECT_TIMEOUT))?;
    Ok(stream)
}

/// Non-blocking Unix-domain connect with a finite deadline.
#[cfg(unix)]
fn connect_unix_socket_bounded(
    path: &std::path::Path,
    timeout: std::time::Duration,
) -> std::io::Result<std::os::unix::net::UnixStream> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    let bytes = path.as_os_str().as_bytes();
    if bytes.is_empty()
        || bytes.len()
            >= std::mem::size_of::<libc::sockaddr_un>()
                - std::mem::offset_of!(libc::sockaddr_un, sun_path)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "ush service socket path is too long",
        ));
    }
    if bytes.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "ush service socket path contains NUL",
        ));
    }

    // SAFETY: `socket` has no pointer arguments and returns an owned descriptor.
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `raw` was just returned by `socket` and has no other owner.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: `fcntl` only changes descriptor flags on the owned descriptor.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the descriptor is owned and remains live for this call.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `fcntl` only changes descriptor status flags on the owned descriptor.
    let status = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if status < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the descriptor is owned and remains live for this call.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, status | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }

    // SAFETY: zeroed `sockaddr_un` is valid before its family and pathname are
    // initialized; `bytes` fits with room for a terminating NUL as checked above.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    // SAFETY: both ranges are valid and non-overlapping, with the destination
    // bounded by the path-length check above.
    unsafe {
        std::ptr::copy_nonoverlapping(
            bytes.as_ptr().cast::<libc::c_char>(),
            address.sun_path.as_mut_ptr(),
            bytes.len(),
        );
    }
    let address_len = libc::socklen_t::try_from(
        std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1,
    )
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid ush path"))?;
    // SAFETY: `address` is initialized for the stated length and `fd` is live.
    let connected = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            (&raw const address).cast::<libc::sockaddr>(),
            address_len,
        )
    };
    if connected < 0 {
        let error = std::io::Error::last_os_error();
        if !unix_connect_is_pending(error.raw_os_error()) {
            return Err(error);
        }
        wait_for_unix_connect(fd.as_raw_fd(), timeout)?;
    }

    // SAFETY: `fcntl` only changes the status flags of the owned descriptor.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, status) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(std::os::unix::net::UnixStream::from(fd))
}

#[cfg(unix)]
fn unix_connect_is_pending(error: Option<i32>) -> bool {
    matches!(
        error,
        Some(libc::EINPROGRESS | libc::EALREADY | libc::EAGAIN)
    )
}

#[cfg(unix)]
fn wait_for_unix_connect(
    fd: std::os::fd::RawFd,
    timeout: std::time::Duration,
) -> std::io::Result<()> {
    let deadline = ui::time::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(ui::time::now());
        let timeout_ms = i32::try_from(remaining.as_millis()).unwrap_or(i32::MAX);
        let mut event = libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: `event` is initialized and live for the duration of this call.
        let result = unsafe { libc::poll(&raw mut event, 1, timeout_ms) };
        if result == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "ush service connection timed out",
            ));
        }
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(error);
        }
        let mut socket_error = 0;
        let mut length = libc::socklen_t::try_from(std::mem::size_of_val(&socket_error))
            .map_err(|_| std::io::Error::other("invalid ush socket error length"))?;
        // SAFETY: both output pointers are valid for their stated lengths.
        if unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&raw mut socket_error).cast(),
                &raw mut length,
            )
        } < 0
        {
            return Err(std::io::Error::last_os_error());
        }
        if socket_error != 0 {
            return Err(std::io::Error::from_raw_os_error(socket_error));
        }
        return Ok(());
    }
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
    ui::framework::definition::<TerminalShell>()
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
