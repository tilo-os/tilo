//! Hosted VMs: one VM per hosted workspace, which runs its Linux programs and its
//! Browser. A Firecracker sibling of the legacy VM, with the same guest agent and
//! lifecycle; one service per host ([`service`]) owns every VM and answers ush over
//! a Unix socket ([`control`]).

#![allow(
    clippy::missing_errors_doc,
    reason = "errors are typed enums whose variants document themselves"
)]

pub mod browser;
pub mod control;
pub mod files;
pub mod firecracker;
pub mod guest;
pub mod jail;
pub mod memory;
pub mod meter;
pub mod network;
pub mod placement;
pub mod plan;
pub mod service;
pub mod size;
pub mod vm;

/// Usable as a file name: ASCII letters, digits, `-`, `_` and `.`, not starting with `.`, at most
/// 128 bytes.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_plain_file_names() {
        for good in ["acct-1", "ws_2", "default", "a.b"] {
            assert!(valid_name(good), "{good}");
        }
        for bad in [
            "",
            ".",
            "..",
            ".hidden",
            "a/b",
            "a b",
            "ä",
            &"x".repeat(129),
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
    }
}
