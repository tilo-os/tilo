//! `tilo-device-vpn`: the device VPN's root helper.

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

    fn serve() -> ExitCode {
        // SAFETY: called at startup before any thread is spawned, so nothing
        // else touches the environment while systemd activation is adopted.
        let listener = match unsafe { peer_facts::server::listen(Path::new(SOCKET_PATH)) } {
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
            |stream| peer_facts::server::identify_peer(stream, |facts| peer::decide(&seat, facts)),
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
