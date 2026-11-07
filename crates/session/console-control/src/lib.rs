//! The action protocol for console session apps.
//! The installer and the greeter run under `cage` straight from greetd, before any
//! Tilo compositor exists, so this crate lets such an app answer the *same*
//! semantic half of the `tilo-control` wire itself: `actions_enumerate`,
//! `actions_invoke` (through the same resolve, availability and strict-binding door
//! every other caller uses, [`catalog::admit`]) and `state_dump`. Input injection
//! is deliberately not served: the console surface is semantic only.
//! [`server::Endpoint::discover`] names at most two local endpoints: a Unix socket
//! in the app user's private runtime directory, and the virtio-serial port
//! [`server::VIRTIO_PORT`], which exists only in a VM. Neither is a network
//! listener, and neither grants anything the app's own user could not already do.

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
