//! Automatic attachment at the native GUI boundary. No process environment is
//! mutated: the host supplies connected descriptors when it execs the app again.
use std::io;

/// The automatic host records which process received its descriptor transfer.
/// A normal later `Command` child retains environment strings, not that transfer.
pub(crate) fn connection_owner() -> io::Result<Option<u32>> {
    std::env::var_os("TILO_RUNTIME_CONNECTION_OWNER_PID")
        .map(|value| {
            value
                .to_str()
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|pid| *pid != 0)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "inherited runtime connection owner must be a nonzero process ID",
                    )
                })
        })
        .transpose()
}

pub(crate) fn connection_metadata_is_current() -> bool {
    connection_owner().is_ok_and(|owner| owner.is_none_or(|pid| pid == std::process::id()))
}

/// Join the shared native runtime, or retain an inherited launcher connection.
/// `TILO_RUNTIME_STANDALONE=1` explicitly disables automatic attachment.
///
/// # Errors
/// Invalid inherited descriptors, unavailable hosts and failed execs are
/// reported; they never silently select a different authority.
pub fn ensure_attached(app_id: &str) -> io::Result<()> {
    #[cfg(unix)]
    {
        unix::ensure_attached(app_id)
    }
    #[cfg(not(unix))]
    {
        let _ = app_id;
        if std::env::var_os("TILO_RUNTIME_STANDALONE").as_deref() == Some(std::ffi::OsStr::new("1"))
        {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "automatic native runtime attachment requires Unix",
        ))
    }
}

#[cfg(unix)]
mod unix {
    use std::{
        env,
        ffi::OsStr,
        io,
        os::unix::{fs::PermissionsExt, process::CommandExt},
        path::{Path, PathBuf},
        process::Command,
    };

    fn invalid(message: &str) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidInput, message)
    }

    fn inherited() -> io::Result<bool> {
        let owner = super::connection_owner()?;
        if owner.is_some_and(|pid| pid != std::process::id()) {
            return Ok(false);
        }
        let collab = env::var_os("TILO_SVC_COLLAB_FD");
        let identity = env::var_os("TILO_SVC_IDENTITY_FD");
        if collab.is_none() && identity.is_none() && owner.is_none() {
            return Ok(false);
        }
        // The automatic host hands over both services. The launcher hands
        // over the ones the launch has: an app that declares no
        // collaboration service still gets its identity broker, and a session
        // without an identity service still hands over collaboration. Every
        // descriptor that is named must be valid either way.
        let named: Vec<&Option<std::ffi::OsString>> = if owner.is_some() {
            vec![&collab, &identity]
        } else {
            [&collab, &identity]
                .into_iter()
                .filter(|value| value.is_some())
                .collect()
        };
        let mut descriptors = Vec::new();
        for value in named {
            let fd = value
                .as_deref()
                .and_then(OsStr::to_str)
                .and_then(|raw| raw.parse::<i32>().ok())
                .filter(|fd| *fd >= 3)
                .ok_or_else(|| {
                    invalid("inherited runtime connections are incomplete or malformed")
                })?;
            // SAFETY: getpeername only inspects an existing descriptor; it does
            // not borrow ownership or close descriptors belonging to SDK code.
            let connected = unsafe {
                let mut address: libc::sockaddr_storage = std::mem::zeroed();
                let mut length = libc::socklen_t::try_from(std::mem::size_of_val(&address))
                    .map_err(io::Error::other)?;
                let mut socket_type: libc::c_int = 0;
                let mut type_length =
                    libc::socklen_t::try_from(std::mem::size_of_val(&socket_type))
                        .map_err(io::Error::other)?;
                libc::getpeername(fd, (&raw mut address).cast(), &raw mut length) == 0
                    && i32::from(address.ss_family) == libc::AF_UNIX
                    && libc::getsockopt(
                        fd,
                        libc::SOL_SOCKET,
                        libc::SO_TYPE,
                        (&raw mut socket_type).cast(),
                        &raw mut type_length,
                    ) == 0
                    && socket_type == libc::SOCK_STREAM
            };
            if !connected {
                return Err(invalid(
                    "inherited runtime descriptor is not a connected Unix socket",
                ));
            }
            descriptors.push(fd);
        }
        if descriptors.len() > 1 && descriptors.first() == descriptors.get(1) {
            return Err(invalid(
                "inherited runtime services must have distinct descriptors",
            ));
        }
        Ok(true)
    }

    pub(super) fn ensure_attached(app_id: &str) -> io::Result<()> {
        if inherited()? {
            if let Some(raw) = env::var_os("TILO_RUNTIME_LAUNCH_GUARD_FD") {
                let fd = raw
                    .to_str()
                    .and_then(|raw| raw.parse::<i32>().ok())
                    .filter(|fd| *fd >= 3)
                    .ok_or_else(|| invalid("invalid native launch guard"))?;
                // SAFETY: this changes only descriptor flags on the inherited
                // lifetime guard. Keep its lock, but never pass it to an exec child.
                let changed = unsafe {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    flags >= 0 && libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) >= 0
                };
                if !changed {
                    return Err(io::Error::last_os_error());
                }
            }
            return Ok(());
        }
        if env::var_os("TILO_RUNTIME_STANDALONE").as_deref() == Some(OsStr::new("1")) {
            return Ok(());
        }
        if app_id.trim().is_empty() || app_id.len() > 4096 {
            return Err(invalid("invalid application identity"));
        }
        let executable = env::current_exe()?;
        let host = locate_host(&executable)?;
        Err(Command::new(host)
            .env("TILO_RUNTIME_RESUME_LIVE", app_id)
            .env(
                "TILO_RUNTIME_CONNECTION_OWNER_PID",
                std::process::id().to_string(),
            )
            .arg("--run-app")
            .arg(app_id)
            .arg("--")
            .arg(executable)
            .args(env::args_os().skip(1))
            .exec())
    }

    fn executable(path: &Path) -> bool {
        std::fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }

    /// The desktop name a Tilo login announces before its compositor starts;
    /// the compositor's own test for a real Tilo session reads the same value.
    const TILO_DESKTOP: &str = "tilo";

    /// Host executables to attach to, in preference order. Inside the Tilo
    /// desktop only the session's own host serves, so an app never attaches
    /// to a second authority beside it; elsewhere the standalone host comes
    /// first.
    fn host_names() -> &'static [&'static str] {
        if env::var_os("XDG_CURRENT_DESKTOP").as_deref() == Some(OsStr::new(TILO_DESKTOP)) {
            &["tilo-runtime-mcp"]
        } else {
            &["tilo-runtime", "tilo-runtime-mcp"]
        }
    }

    fn locate_host(app: &Path) -> io::Result<PathBuf> {
        if let Some(path) = env::var_os("TILO_RUNTIME_BINARY") {
            let path = PathBuf::from(path);
            if !path.is_absolute() || !executable(&path) {
                return Err(invalid(
                    "TILO_RUNTIME_BINARY must name an absolute executable file",
                ));
            }
            return Ok(path);
        }
        let names = host_names();
        for name in names {
            if let Some(parent) = app.parent() {
                let sibling = parent.join(name);
                if executable(&sibling) {
                    return Ok(sibling);
                }
            }
            if let Some(paths) = env::var_os("PATH") {
                for directory in env::split_paths(&paths) {
                    let candidate = directory.join(name);
                    if executable(&candidate) {
                        return candidate.canonicalize();
                    }
                }
            }
        }
        if let Some(target) = checkout_target(app) {
            for name in names {
                for profile in ["debug", "thin-app", "release"] {
                    let candidate = target.join(profile).join(name);
                    if executable(&candidate) {
                        return candidate.canonicalize();
                    }
                }
            }
            let build = if names == ["tilo-runtime-mcp"] {
                "cargo build --locked -p shell --bin tilo-runtime-mcp --no-default-features"
            } else {
                "cargo build --locked -p tilo-runtime --bin tilo-runtime"
            };
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "no runtime host found; in this checkout run `{build}` once, then rerun the app"
                ),
            ));
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "no runtime host found; install {} beside the app or on PATH",
                names.join(" or ")
            ),
        ))
    }

    /// A source app may use the host built by Cargo's default profile even
    /// when the app itself uses thin-app. Installed apps never search another
    /// profile or another checkout, and desktop host selection stays intact.
    fn checkout_target(app: &Path) -> Option<PathBuf> {
        let target = app.parent()?.parent()?;
        let root = target.parent()?;
        (target.file_name() == Some(OsStr::new("target"))
            && root.join("Cargo.toml").is_file()
            && root
                .join("crates/runtime/tilo-runtime/Cargo.toml")
                .is_file())
        .then(|| target.to_path_buf())
    }
}
