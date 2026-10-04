//! Both ends of the Wayland forwarder.
//!
//! In the guest every windowed app runs under `waypipe server`, which gives
//! the app its own Wayland display and carries the connection over vsock to
//! the host. On the host one `waypipe client` per VM listens on a vsock port
//! and opens one ordinary connection to the compositor for each app, so each
//! window is a normal `xdg_toplevel`.

use crate::{HOST_CID, WAYPIPE_PORT};

/// The variable through which a launch tells the guest which host port its
/// forwarder listens on.
pub const PORT_ENV: &str = "TILO_LEGACY_WAYPIPE_PORT";

/// The host port of one user's forwarder. vsock ports are host-global, so
/// every local user's VM gets its own.
#[must_use]
pub fn host_port_for_uid(uid: u32) -> u32 {
    WAYPIPE_PORT.wrapping_add(uid.wrapping_mul(2)).max(1024)
}

/// `waypipe --vsock -s 2:<port> server -- <argv…>`, run in the guest.
#[must_use]
pub fn server_argv(waypipe: &str, port: u32, argv: &[String]) -> Vec<String> {
    let mut out = vec![
        waypipe.to_string(),
        "--vsock".to_string(),
        "-s".to_string(),
        format!("{HOST_CID}:{port}"),
        "server".to_string(),
        "--".to_string(),
    ];
    out.extend(argv.iter().cloned());
    out
}

/// `waypipe --vsock -s <port> client`, run on the host with
/// `WAYLAND_DISPLAY` naming the compositor.
#[must_use]
pub fn client_argv(waypipe: &str, port: u32) -> Vec<String> {
    vec![
        waypipe.to_string(),
        "--vsock".to_string(),
        "-s".to_string(),
        port.to_string(),
        "client".to_string(),
    ]
}

/// `waypipe -s <path> client`: the host side when the guest's vsock is
/// mapped onto Unix sockets (see [`crate::link::AgentLink::Hybrid`]).
#[must_use]
pub fn client_argv_unix(waypipe: &str, path: &std::path::Path) -> Vec<String> {
    vec![
        waypipe.to_string(),
        "-s".to_string(),
        path.display().to_string(),
        "client".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwarder_argv_pairs_the_guest_server_with_the_host_client() {
        // phase: the guest wraps the app's argv verbatim after `--`
        {
            let argv = vec![
                "flatpak".to_string(),
                "run".into(),
                "--".into(),
                "-x".into(),
            ];
            assert_eq!(
                server_argv("waypipe", 9000, &argv),
                [
                    "waypipe", "--vsock", "-s", "2:9000", "server", "--", "flatpak", "run", "--",
                    "-x"
                ]
            );
        }
        // phase: the host listens on the port the guest connects to
        {
            let port = host_port_for_uid(1000);
            assert_eq!(
                client_argv("/usr/bin/waypipe", port),
                [
                    "/usr/bin/waypipe",
                    "--vsock",
                    "-s",
                    &port.to_string(),
                    "client"
                ]
            );
            assert_eq!(server_argv("waypipe", port, &[])[3], format!("2:{port}"));
            assert_ne!(host_port_for_uid(1000), host_port_for_uid(1001));
            assert!(host_port_for_uid(0) >= 1024);
            assert_eq!(
                client_argv_unix("waypipe", std::path::Path::new("/run/v.sock_7111")),
                ["waypipe", "-s", "/run/v.sock_7111", "client"]
            );
        }
    }
}
