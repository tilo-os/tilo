//! The actions of console session apps.
//! The installer and the greeter run under `cage` straight from greetd, before any
//! Tilo compositor exists, so this crate lets such an app serve its actions on the one
//! action wire itself: frames ([`tilo_actions::remote::RemoteActionFrame`]), one per line,
//! answered as an [`ActionProvider`](tilo_actions::remote::ActionProvider) whose answers
//! the app's own thread gives ([`ConsoleApp`]), and the `state_dump` a session's control
//! socket answers too. Input injection is deliberately not served: the console surface is
//! semantic only.
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

pub mod server;

pub use server::{Asked, ConsoleApp, ConsoleServer, Endpoint, VIRTIO_PORT, admit_to};

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
