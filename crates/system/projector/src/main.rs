//! Tilo screen-projection protocol broker.
//!
//! This executable is intentionally separate from the compositor: discovery
//! touches the LAN, and protocol sessions will own receiver sockets for much
//! longer than the Win+P overlay is visible. The shell invokes the small JSON
//! command surface in `shell::shell::projection::SystemProjectionBackend`.
//!
//! [`cli`] parses that surface; [`discovery`] finds receivers; a `connect`
//! starts a detached [`session`] child (Cast and `FCast` in `session::cast`,
//! `AirPlay` and Miracast through an external sender in `session::external`)
//! that serves the compositor's video over [`http`].

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

mod cli;
mod discovery;
mod error;
mod http;
mod session;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(error) = cli::run(&args) {
        eprintln!("tilo-projector: {error}");
        std::process::exit(2);
    }
}
