//! `tilo-device-vpn`: the device VPN's root helper.
//!
//! - `serve`: answer the session's shell on the control socket.
//! - `reset`: undo what a helper left behind, at boot.

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("tilo-device-vpn runs on Linux only");
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

    use device_vpn::protocol::SOCKET_PATH;
    use device_vpn::service::linux::{Config, LinuxBackend};
    use device_vpn::service::{Backend as _, Service, peer, plan, server};

    pub fn main() -> ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();
        match args.first().map(String::as_str) {
            Some("serve") => serve(),
            Some("reset") => reset(),
            _ => {
                eprintln!("usage: tilo-device-vpn serve | reset");
                ExitCode::from(2)
            }
        }
    }

    /// The listening socket: the one systemd passes (`tilo-device-vpn.socket`),
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
            // The programs the helper runs must never inherit it.
            // SAFETY: setting a flag on the descriptor just adopted.
            unsafe { libc::fcntl(3, libc::F_SETFD, libc::FD_CLOEXEC) };
            return Ok(listener);
        }
        let socket = Path::new(SOCKET_PATH);
        if let Some(parent) = socket.parent() {
            fs::create_dir_all(parent)?;
        }
        let _ = fs::remove_file(socket);
        let listener = UnixListener::bind(socket)?;
        // Who may ask is decided per connection, from the kernel's account
        // of the caller; the socket itself is open to every local process.
        fs::set_permissions(socket, fs::Permissions::from_mode(0o666))?;
        Ok(listener)
    }

    fn serve() -> ExitCode {
        let listener = match listener() {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("tilo-device-vpn: listening on {SOCKET_PATH}: {error}");
                return ExitCode::FAILURE;
            }
        };
        let config = Config::default();
        let seat = config.seat.clone();
        let backend = LinuxBackend::new(config);
        let record = backend.load();
        let mut service = Service::new(backend, record);
        let stop = AtomicBool::new(false);
        server::serve(
            &listener,
            &mut service,
            |stream| {
                seat_peer::facts(stream)
                    .map_err(|error| format!("unidentified caller: {error}"))
                    .and_then(|facts| peer::decide(&seat, &facts))
            },
            server::Pacing {
                housekeeping: Duration::from_secs(2),
                idle_exit: Some(Duration::from_secs(60)),
            },
            &stop,
        );
        ExitCode::SUCCESS
    }

    /// Undo what an earlier helper left: at boot nothing it set up survived
    /// but `NetworkManager`'s global DNS, which `NetworkManager` keeps.
    fn reset() -> ExitCode {
        let backend = LinuxBackend::new(Config::default());
        let record = backend.load();
        let dns_marked = backend.dns_marked();
        let mut service = Service::new(backend, record);
        service.reset();
        if dns_marked {
            let mut backend = LinuxBackend::new(Config::default());
            if let Err(error) = backend.run(&plan::global_dns(false)) {
                eprintln!("tilo-device-vpn: putting DNS back: {error}");
                return ExitCode::FAILURE;
            }
            if let Err(error) = backend.save(service.record()) {
                eprintln!("tilo-device-vpn: {error}");
                return ExitCode::FAILURE;
            }
        }
        ExitCode::SUCCESS
    }
}
