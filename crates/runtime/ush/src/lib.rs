//! `ush` — an application runtime that hosts a shell.
//!
//! Not a shell language. `ush` builds a guest from an environment record —
//! its `/usr` and `/etc` a read-only stack of images, its home its own, the
//! directories it was granted bound at their own paths — puts a shell inside
//! it, and brokers every request that would reach past it. See
//! `docs/app-model/environments.md` for the model and
//! `docs/app-model/ush.md` for this runtime.
//!
//! # Why this exists in Tilo
//!
//! `apps/terminal` cannot `exec` `$SHELL`: the app sandbox binds the loader,
//! fonts, `/sys` and the CA bundle but no executable directory, and adding
//! the host's `/usr/bin` would hand every app the host's whole command set.
//! A guest whose root is built from images gives a shell `/usr/bin`,
//! terminfo and `/etc/passwd` by construction, not as a grant.
//!
//! # The trust boundary
//!
//! Three principals, in decreasing authority. Read this before changing
//! anything in [`guest`], [`broker`] or [`seccomp`].
//!
//! 1. **The supervisor** ([`supervisor`], [`broker`]) is Tilo code, running
//!    in the user's session *outside* any app sandbox. It needs
//!    `unshare`/`mount`/`pivot_root` — syscalls apps are blanket-denied —
//!    because it is what *creates* the confinement, the same way `bwrap`
//!    is. It owns the store, the environment records and the audit log,
//!    none of which is visible inside a guest.
//!
//! 2. **The guest** ([`guest`]) is everything the user runs — untrusted. It
//!    runs as the real uid in its own user namespace, which maps that one id
//!    and nothing else, so nothing in it is root and a file it writes to a
//!    granted directory is the user's. It has a network namespace of its
//!    own, reaching only the hosts its record grants ([`tilo_egress`]),
//!    owned by a user namespace above its own. Its seccomp filter denies
//!    `unshare`/`mount`/`pivot_root`/`setns`/`chroot`, so it cannot open a
//!    second namespace and climb out of the first — `bwrap` installs the
//!    filter after mounting and immediately before exec, so the supervisor's
//!    need for those syscalls never propagates.
//!
//! 3. **An app**, launched through `tilo-launcher`, is untrusted and cannot
//!    reach this runtime by executing it — a sandboxed app has neither the
//!    syscalls nor a path to the binary. `apps/terminal` asks the
//!    supervisor over its service socket for a target and gets back a pty,
//!    not a namespace.
//!
//! # The images are read-only
//!
//! Nothing below an image is writable: there is no overlay upper. A guest
//! adds software only by asking — `pkg install` builds a tool image and adds
//! it to the record — so what an environment is is always what its record
//! says, on every device it opens on.
//!
//! # Permissions are Tilo's, not a second system
//!
//! `perm trust-dir`, and every other decision the broker makes, is a
//! `collab_types::caps::CapOverride` in the environment record evaluated by
//! `decide_capability` — the same record the compositor shows in its
//! permissions panel and can revoke. There is no `ush`-local permission type.
//! See [`policy`].

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

pub mod audit;
pub mod broker;
pub mod guard;
pub mod guest;
pub mod images;
pub mod policy;
pub mod records;
pub mod seccomp;
pub mod store;
pub mod supervisor;

pub use store::Store;
/// The package id every `ush` grant is written against.
pub use tilo_environment::RUNTIME_APP_ID;

/// Where the guest can see the runtime's own binary and its command shims:
/// a `tmpfs` the supervisor populates, first on `PATH`, not guest-writable.
pub const GUEST_BIN_DIR: &str = "/run/ush/bin";

/// The broker socket, as the guest sees it.
pub const GUEST_BROKER_SOCK: &str = "/run/ush/broker.sock";

/// Names the runtime answers to from inside a guest, dispatched on `argv[0]`.
/// These are symlinks to the runtime binary rather than shell builtins, so
/// they behave identically under `bash`, `fish`, a script, or `xargs`.
pub const GUEST_COMMANDS: &[&str] = &[
    "pkg",
    "perm",
    "scan",
    "guard",
    "quarantine",
    "restart",
    // These are deliberately host-brokered: the guest's images are not the
    // user's apps or system, and its private /run cannot reach systemd for
    // image updates.
    "tilo",
    "app",
    "store",
    "system",
];

/// Whether a command shim belongs in a guest built from `base`.
pub fn guest_command_available(base: &std::path::Path, command: &str) -> bool {
    match command {
        // `system` is an argv0 convenience alias for `tilo system`, not a
        // generic name for any machine with the `tilo` client installed.  Do
        // not put it on a guest's PATH unless this is actually a Tilo system
        // image with the deployment provider behind the client.
        "system" => system_provider_available(base),
        "tilo" => base.join("usr/bin/tilo").is_file(),
        "app" | "store" => base.join("usr/bin/app").is_file(),
        _ => true,
    }
}

fn system_provider_available(base: &std::path::Path) -> bool {
    base.join("usr/bin/tilo").is_file() && base.join("usr/libexec/tilo-deployment").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_shim_requires_the_tilo_system_provider() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("usr/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("tilo"), "").unwrap();

        assert!(guest_command_available(dir.path(), "tilo"));
        assert!(
            !guest_command_available(dir.path(), "system"),
            "a client binary alone must not expose the system alias"
        );

        let provider = dir.path().join("usr/libexec/tilo-deployment");
        std::fs::create_dir_all(provider.parent().unwrap()).unwrap();
        std::fs::write(provider, "").unwrap();
        assert!(guest_command_available(dir.path(), "system"));
    }
}
