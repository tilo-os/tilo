//! `tilo-legacy-agent`: the guest side of a Tilo legacy VM.

use std::process::ExitCode;
use std::sync::Arc;

use legacy_agent::server::{Agent, boot_id, guest_user};
use legacy_agent::user::DEFAULT_USER;
use legacy_agent_protocol::vsock::VsockListener;
use legacy_agent_protocol::{AGENT_PORT, HOST_CID};

fn main() -> ExitCode {
    let name = std::env::args()
        .skip_while(|arg| arg != "--user")
        .nth(1)
        .unwrap_or_else(|| DEFAULT_USER.to_string());
    let Some(user) = guest_user(&name) else {
        eprintln!("tilo-legacy-agent: no ordinary user {name:?} in /etc/passwd");
        return ExitCode::FAILURE;
    };
    let listener = match VsockListener::bind(AGENT_PORT) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("tilo-legacy-agent: listen on vsock port {AGENT_PORT}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let agent = Arc::new(Agent::new(user, boot_id()));
    eprintln!("tilo-legacy-agent: serving vsock port {AGENT_PORT} for {name}");
    loop {
        let (stream, peer) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(error) => {
                eprintln!("tilo-legacy-agent: accept: {error}");
                continue;
            }
        };
        // Only the host speaks to the agent; vsock has no guest-to-guest
        // path, so this holds by construction and is checked anyway.
        if peer != HOST_CID {
            continue;
        }
        let agent = Arc::clone(&agent);
        std::thread::spawn(move || {
            if let Err(error) = agent.serve(stream) {
                eprintln!("tilo-legacy-agent: {error}");
            }
        });
    }
}
