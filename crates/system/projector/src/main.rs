//! Tilo screen-projection protocol broker.
//!
//! This executable is intentionally separate from the compositor: discovery
//! touches the LAN, and protocol sessions own receiver sockets for much
//! longer than the Win+P overlay is visible. It runs as the
//! socket-activated user service `tilo-projector.service`; the shell talks to
//! it over the control socket (`tilo_projector::control`) from
//! `shell::shell::projection::SystemProjectionBackend`.
//!
//! [`cli`] parses the command line; [`service`] serves the control socket and
//! supervises one [`session`] at a time (Cast and `FCast` in `session::cast`,
//! `AirPlay` through an external sender in `session::external`, Miracast in
//! `session::miracast`), which serves the compositor's video over [`http`]; [`discovery`] finds
//! receivers.

mod cli;
mod discovery;
mod error;
mod http;
mod service;
mod session;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(error) = cli::run(&args) {
        eprintln!("tilo-projector: {error}");
        std::process::exit(2);
    }
}
