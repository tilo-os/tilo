//! The action protocol for console session apps.
//!
//! The installer (and the greeter) run under `cage` straight from greetd,
//! before any Tilo compositor exists, so the compositor's control socket and
//! its action broker never see them. This crate lets such an app answer the
//! *same* semantic half of the `tilo-control` wire itself:
//!
//! * `actions_enumerate` — the app's published resource tree as inert
//!   descriptors (address, typed parameters, contract, availability);
//! * `actions_invoke` — one action by address, through the same resolve /
//!   availability / confirmation / strict-binding door every other caller
//!   uses ([`catalog::admit`]);
//! * `state_dump` — what the app believes is on screen, as JSON.
//!
//! `tilo-control --socket <path> actions list` therefore works unchanged
//! against a console app. Input injection (`click`, `key`, `type`, …) is
//! deliberately not served: the console surface is semantic only, and a
//! caller that needs pixels has the hypervisor's own screen and keyboard.
//!
//! # Where it listens
//!
//! [`server::Endpoint::discover`] names at most two endpoints, both local:
//!
//! * a Unix socket in the app user's private runtime directory
//!   (`$XDG_RUNTIME_DIR/tilo-console/<app>.sock`, directory 0700, socket
//!   0600) — the local console and assistive tooling running as that user;
//! * the virtio-serial port [`server::VIRTIO_PORT`], which exists only when a
//!   hypervisor attached a port with exactly that name. Real hardware has no
//!   such device, so the channel cannot exist there; in a VM only the host
//!   user who started QEMU can reach its host end.
//!
//! Neither endpoint is a network listener, and neither grants anything the
//! app's own user could not already do on that machine.

#![cfg_attr(
    test,
    allow(
        clippy::arithmetic_side_effects,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod catalog;
pub mod server;
pub mod wire;

pub use catalog::{Admitted, Refusal, admit, enumerate};
pub use server::{ConsoleServer, Endpoint, Pending, VIRTIO_PORT};
pub use wire::Request;

/// The composefs digest this system booted, from the kernel command line.
/// A console app reports it in its state, so a caller can check that a
/// first boot runs the tree the installer wrote (the installer and the
/// installed target seal the same composefs tree).
pub fn booted_composefs() -> Option<String> {
    composefs_from_cmdline(&std::fs::read_to_string("/proc/cmdline").ok()?)
}

/// The `composefs=` value of one kernel command line.
pub fn composefs_from_cmdline(cmdline: &str) -> Option<String> {
    cmdline
        .split_whitespace()
        .find_map(|token| token.strip_prefix("composefs="))
        .filter(|digest| !digest.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_booted_tree_is_read_from_the_command_line() {
        assert_eq!(
            super::composefs_from_cmdline("root=tilo-auto composefs=abc123 quiet").as_deref(),
            Some("abc123")
        );
        assert_eq!(super::composefs_from_cmdline("quiet composefs="), None);
        assert_eq!(super::composefs_from_cmdline("quiet"), None);
    }
}
