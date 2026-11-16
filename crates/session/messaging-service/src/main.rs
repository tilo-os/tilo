//! `tilo-messaging`: the person's messaging accounts, started by the
//! compositor with the session (see the crate docs).
//!
//! `tilo-messaging --principal <hex> --device <id> --profile <dir>`; the
//! session door is standard input and output.

use std::path::PathBuf;

use tilo_messaging_service::door::{DEVICE_ARG, DoorLink, PRINCIPAL_ARG, PROFILE_ARG};
use tilo_messaging_service::service::{DoorHandler, Service, listen};

fn arguments() -> Option<(String, String, PathBuf)> {
    let mut principal = None;
    let mut device = None;
    let mut profile = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next()?;
        match flag.as_str() {
            PRINCIPAL_ARG => principal = Some(value),
            DEVICE_ARG => device = Some(value),
            PROFILE_ARG => profile = Some(PathBuf::from(value)),
            _ => return None,
        }
    }
    Some((principal?, device?, profile?))
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    // A compositor killed outright still takes the service with it.
    // SAFETY: prctl(PR_SET_PDEATHSIG) sets only this process's own
    // parent-death signal; it reads and writes no memory of ours.
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
    }
    let Some((principal, device, profile)) = arguments() else {
        eprintln!(
            "usage: tilo-messaging {PRINCIPAL_ARG} <hex> {DEVICE_ARG} <id> {PROFILE_ARG} <dir>"
        );
        std::process::exit(2);
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .thread_name("tilo-messaging")
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(target: "tilo::messaging", %error, "no runtime");
            std::process::exit(1);
        }
    };
    let handler = DoorHandler::new();
    let door = match DoorLink::start(
        "tilo-messaging-door",
        std::io::stdin(),
        std::io::stdout(),
        handler.clone(),
    ) {
        Ok(door) => door,
        Err(error) => {
            tracing::error!(target: "tilo::messaging", %error, "the session door did not open");
            std::process::exit(1);
        }
    };
    let service = match Service::new(
        &principal,
        &device,
        &profile,
        door,
        runtime.handle().clone(),
    ) {
        Ok(service) => service,
        Err(error) => {
            // No custody on this device yet: no accounts to hold.
            tracing::warn!(target: "tilo::messaging", %error, "the person's device custody is unavailable");
            std::process::exit(0);
        }
    };
    handler.bind(&service);
    if let Err(error) = listen(&service) {
        tracing::error!(target: "tilo::messaging", %error, "the messaging socket did not open");
        std::process::exit(1);
    }
    // The door closing ends the process; until then the runtime serves.
    runtime.block_on(std::future::pending::<()>());
}
