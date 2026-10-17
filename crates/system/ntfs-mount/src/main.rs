//! `tilo-ntfs-mount`: the consent-gated NTFS mount service.
//!
//! - `serve`: answer the shell and Settings on the control socket.
//! - `record-boot-disks`: note which disks are attached at start-up.
//! - `check-volume`: the confined preflight step the service starts itself.

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("tilo-ntfs-mount runs on Linux only");
    std::process::ExitCode::FAILURE
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::path::Path;
    use std::process::ExitCode;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use ntfs_mount::boot::BootDisks;
    use ntfs_mount::grants::GrantStore;
    use ntfs_mount::protocol::SOCKET_PATH;
    use ntfs_mount::service::linux::{Config, LinuxBackend, check_volume};
    use ntfs_mount::service::{Service, peer, server};

    const GRANTS_DIR: &str = "/var/lib/tilo/ntfs-mount/grants";

    pub fn main() -> ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();
        match args.first().map(String::as_str) {
            Some("serve") => serve(),
            Some("record-boot-disks") => {
                let config = Config::default();
                match BootDisks::record(&config.sys_root)
                    .and_then(|disks| disks.save(&config.boot_disks))
                {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        eprintln!("tilo-ntfs-mount: recording boot disks: {error}");
                        ExitCode::FAILURE
                    }
                }
            }
            Some("check-volume") => ExitCode::from(u8::try_from(check_volume()).unwrap_or(1)),
            _ => {
                eprintln!("usage: tilo-ntfs-mount serve | record-boot-disks");
                ExitCode::from(2)
            }
        }
    }

    /// The listening socket: the one systemd passes (`tilo-ntfs-mount.socket`),
    /// or one bound here when started by hand.
    fn listener() -> std::io::Result<UnixListener> {
        use std::os::fd::FromRawFd;
        let activated = std::env::var("LISTEN_PID")
            .ok()
            .and_then(|pid| pid.parse::<u32>().ok())
            == Some(std::process::id())
            && std::env::var("LISTEN_FDS").as_deref() == Ok("1");
        if activated {
            // SAFETY: systemd passes the socket as descriptor 3 and nothing
            // else in this process has claimed it.
            let listener = unsafe { UnixListener::from_raw_fd(3) };
            // Helpers must never inherit it.
            // SAFETY: setting a flag on the descriptor just adopted.
            unsafe { libc::fcntl(3, libc::F_SETFD, libc::FD_CLOEXEC) };
            return Ok(listener);
        }
        let socket = Path::new(SOCKET_PATH);
        let _ = fs::remove_file(socket);
        let listener = UnixListener::bind(socket)?;
        // Who may ask what is decided per connection, from the kernel's
        // account of the caller; the socket itself is open to every local
        // process.
        fs::set_permissions(socket, fs::Permissions::from_mode(0o666))?;
        Ok(listener)
    }

    fn serve() -> ExitCode {
        let listener = match listener() {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("tilo-ntfs-mount: listening on {SOCKET_PATH}: {error}");
                return ExitCode::FAILURE;
            }
        };
        let config = Config::default();
        let policy = config.peer.clone();
        let mut service = Service::new(LinuxBackend::new(config), GrantStore::new(GRANTS_DIR));
        let stop = AtomicBool::new(false);
        server::serve(
            &listener,
            &mut service,
            |stream| {
                peer::facts(stream)
                    .map_err(|error| format!("unidentified caller: {error}"))
                    .and_then(|facts| policy.decide(&facts))
            },
            server::Pacing {
                housekeeping: Duration::from_secs(5),
                idle_exit: Some(Duration::from_secs(60)),
            },
            &stop,
        );
        ExitCode::SUCCESS
    }
}
