//! Automatic attachment at the native GUI boundary. No process environment is
//! mutated: the host supplies connected descriptors when it execs the app again.
use std::io;

/// The automatic host records which process received its descriptor transfer.
/// A normal later `Command` child retains environment strings, not that transfer.
pub(crate) fn connection_owner() -> io::Result<Option<u32>> {
    std::env::var_os("QOS_RUNTIME_CONNECTION_OWNER_PID")
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
/// `QOS_RUNTIME_STANDALONE=1` explicitly disables automatic attachment.
///
/// # Errors
/// Invalid inherited descriptors, unavailable hosts, failed developer builds and
/// failed execs are reported; they never silently select a different authority.
pub fn ensure_attached(app_id: &str) -> io::Result<()> {
    #[cfg(unix)]
    {
        unix::ensure_attached(app_id)
    }
    #[cfg(not(unix))]
    {
        let _ = app_id;
        if std::env::var_os("QOS_RUNTIME_STANDALONE").as_deref() == Some(std::ffi::OsStr::new("1"))
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
        fs::{File, OpenOptions},
        io::{self, BufRead, BufReader, Read, Write},
        os::unix::{
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
            process::CommandExt,
        },
        path::{Path, PathBuf},
        process::{Command, Stdio},
        time::{Duration, Instant},
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
        let mut descriptors = Vec::new();
        for value in [&collab, &identity] {
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
        if descriptors.first() == descriptors.get(1) {
            return Err(invalid(
                "inherited runtime services must have distinct descriptors",
            ));
        }
        Ok(true)
    }

    pub(super) fn ensure_attached(app_id: &str) -> io::Result<()> {
        if inherited()? {
            return Ok(());
        }
        if env::var_os("QOS_RUNTIME_STANDALONE").as_deref() == Some(OsStr::new("1")) {
            return Ok(());
        }
        if app_id.trim().is_empty() || app_id.len() > 4096 {
            return Err(invalid("invalid application identity"));
        }
        let executable = env::current_exe()?;
        let host = locate_host(&executable)?;
        Err(Command::new(host)
            .env("QOS_RUNTIME_RESUME_LIVE", app_id)
            .env(
                "QOS_RUNTIME_CONNECTION_OWNER_PID",
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

    fn locate_host(app: &Path) -> io::Result<PathBuf> {
        if let Some(path) = env::var_os("QOS_RUNTIME_BINARY") {
            let path = PathBuf::from(path);
            if !path.is_absolute() || !executable(&path) {
                return Err(invalid(
                    "QOS_RUNTIME_BINARY must name an absolute executable file",
                ));
            }
            return Ok(path);
        }
        if let Some(parent) = app.parent() {
            let sibling = parent.join("qos-runtime-mcp");
            if executable(&sibling) {
                return Ok(sibling);
            }
        }
        if let Some(paths) = env::var_os("PATH") {
            for directory in env::split_paths(&paths) {
                let candidate = directory.join("qos-runtime-mcp");
                if executable(&candidate) {
                    return candidate.canonicalize();
                }
            }
        }
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .filter(|root| {
                root.join("crates/session/shell/src/bin/qos-runtime-mcp.rs")
                    .is_file()
            })
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "qos-runtime-mcp is missing; install it beside the app or on PATH",
                )
            })?;
        let target =
            env::var_os("CARGO_TARGET_DIR").map_or_else(|| source.join("target"), PathBuf::from);
        let target = if target.is_absolute() {
            target
        } else {
            env::current_dir()?.join(target)
        };
        build_host(source, &target)
    }

    fn build_host(source: &Path, target: &Path) -> io::Result<PathBuf> {
        std::fs::create_dir_all(target)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(target.join(".qos-native-build.lock"))?;
        let metadata = lock.metadata()?;
        // SAFETY: geteuid has no memory preconditions.
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsafe native runtime build lock",
            ));
        }
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(600))
            .ok_or_else(|| io::Error::other("build deadline overflow"))?;
        lock_until(&lock, deadline)?;
        let host = target.join("debug/qos-runtime-mcp");
        if executable(&host) {
            return Ok(host);
        }
        if let Some(host) = cached_host(target)? {
            return Ok(host);
        }
        eprintln!("qOS: building the shared native runtime (first launch)");
        let mut child = Command::new("cargo")
            .args([
                "build",
                "-p",
                "shell",
                "--no-default-features",
                "--bin",
                "qos-runtime-mcp",
                "--message-format=json-render-diagnostics",
                "--target-dir",
            ])
            .arg(target)
            .current_dir(source)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("failed to capture Cargo artifact output"))?;
        let mut artifact = Some(std::thread::spawn(move || {
            cargo_artifact(BufReader::new(stdout))
        }));
        loop {
            if let Some(status) = child.try_wait()? {
                while !artifact
                    .as_ref()
                    .is_some_and(std::thread::JoinHandle::is_finished)
                {
                    if Instant::now() >= deadline {
                        kill_process_group(&mut child);
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "native runtime build output exceeded ten minutes",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                let reported = artifact
                    .take()
                    .ok_or_else(|| io::Error::other("Cargo artifact reader is missing"))?
                    .join()
                    .map_err(|_| io::Error::other("Cargo artifact reader panicked"))??;
                if status.success() {
                    let built = reported.filter(|path| executable(path)).or_else(|| {
                        // Retain compatibility with controlled Cargo wrappers
                        // and old Cargo versions that create the default output.
                        executable(&host).then_some(host)
                    });
                    if let Some(built) = built {
                        let built = built.canonicalize()?;
                        cache_host(target, &built)?;
                        return Ok(built);
                    }
                }
                return Err(io::Error::other(format!(
                    "native runtime build failed: {status}"
                )));
            }
            if Instant::now() >= deadline {
                kill_process_group(&mut child);
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "native runtime build exceeded ten minutes",
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn kill_process_group(child: &mut std::process::Child) {
        // The build owns this process group; terminate Cargo's compiler
        // children too, without touching any pre-existing build server.
        if let Ok(pid) = i32::try_from(child.id()) {
            // SAFETY: the negative PID names the new child's own group.
            unsafe {
                if let Some(group) = pid.checked_neg() {
                    libc::kill(group, libc::SIGKILL);
                }
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    fn cargo_artifact(reader: impl BufRead) -> io::Result<Option<PathBuf>> {
        let mut host = None;
        for line in reader.lines() {
            let line = line?;
            let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
                eprintln!("{line}");
                continue;
            };
            if message.get("reason").and_then(serde_json::Value::as_str) == Some("compiler-message")
                && let Some(rendered) = message
                    .get("message")
                    .and_then(|message| message.get("rendered"))
                    .and_then(serde_json::Value::as_str)
            {
                eprint!("{rendered}");
            }
            if message.get("reason").and_then(serde_json::Value::as_str)
                == Some("compiler-artifact")
                && message
                    .get("target")
                    .and_then(|target| target.get("name"))
                    .and_then(serde_json::Value::as_str)
                    == Some("qos-runtime-mcp")
            {
                host = message
                    .get("executable")
                    .and_then(serde_json::Value::as_str)
                    .map(PathBuf::from);
            }
        }
        Ok(host)
    }

    fn cached_host(target: &Path) -> io::Result<Option<PathBuf>> {
        let path = target.join(".qos-native-host-path");
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        validate_cache_file(&file)?;
        if file.metadata()?.len() > 16 * 1024 {
            return Err(invalid("native runtime build cache is oversized"));
        }
        let mut value = String::new();
        file.read_to_string(&mut value)?;
        let host = PathBuf::from(value.trim());
        let target = target.canonicalize()?;
        let Ok(host) = host.canonicalize() else {
            return Ok(None);
        };
        Ok((host.starts_with(target) && executable(&host)).then_some(host))
    }

    fn cache_host(target: &Path, host: &Path) -> io::Result<()> {
        let target = target.canonicalize()?;
        if !host.starts_with(&target) {
            return Err(invalid(
                "Cargo reported a native runtime outside its target directory",
            ));
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(target.join(".qos-native-host-path"))?;
        validate_cache_file(&file)?;
        file.set_len(0)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        writeln!(file, "{}", host.display())?;
        file.sync_data()
    }

    fn validate_cache_file(file: &File) -> io::Result<()> {
        let metadata = file.metadata()?;
        // SAFETY: geteuid has no memory preconditions.
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsafe native runtime build cache",
            ));
        }
        Ok(())
    }

    fn lock_until(file: &File, deadline: Instant) -> io::Result<()> {
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(()),
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
                Err(std::fs::TryLockError::WouldBlock) => {}
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "native runtime build lock timed out",
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
