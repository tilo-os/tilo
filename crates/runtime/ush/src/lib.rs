//! `ush` — an application runtime that hosts a shell.
//!
//! Not a shell language. `ush` composes a root filesystem out of the host's
//! (read-only) and a stack of user-owned layers, puts a shell inside it, and
//! brokers every request that would reach past it.
//!
//! # Why this exists in Tilo
//!
//! `apps/terminal` used to `exec` `$SHELL` directly, which fails with
//! `ENOENT` inside a Tilo app sandbox: the system read profile binds the
//! loader, fonts, `/sys` and the CA bundle but no executable directory —
//! adding `/usr/bin` would hand every app the host's whole command set, and
//! per-directory capabilities would prompt for `/usr/share/terminfo`. A
//! read-only host rootfs gives you `/usr/bin`, terminfo and `/etc/passwd` by
//! construction, not as a grant — everything below follows from that.
//!
//! # The trust boundary
//!
//! Three principals, in decreasing authority. Read this before changing
//! anything in [`guest`], [`broker`] or [`profile`].
//!
//! 1. **The supervisor** ([`supervisor`], [`broker`]) is Tilo code, running
//!    in the user's session *outside* any app sandbox. It needs
//!    `unshare`/`mount`/`pivot_root` — syscalls apps are blanket-denied —
//!    because it is what *creates* the confinement, the same way `bwrap`
//!    is. It owns the layer store, policy file and audit log, none of which
//!    is ever visible inside a guest.
//!
//! 2. **The guest** ([`guest`]) is everything the user runs — untrusted. It
//!    runs as uid 0 *inside its own user namespace*, mapped to the
//!    unprivileged real user outside and conferring no host authority (this
//!    is what lets `rpm` chown into a layer the user owns). Its seccomp
//!    filter still denies `unshare`/`mount`/`pivot_root`/`setns`/`chroot`,
//!    so it cannot open a second namespace and climb out of the first —
//!    `bwrap` installs the filter after mounting and immediately before
//!    exec, so the supervisor's need for those syscalls never propagates.
//!
//! 3. **An app**, launched through `tilo-launcher`, is untrusted and cannot
//!    reach this runtime by executing it — a sandboxed app has neither the
//!    syscalls nor a path to the binary. `apps/terminal` asks the
//!    supervisor over its service socket and gets back a pty, not a
//!    namespace.
//!
//! [`Profile::Dsh`] is the single deliberate exception to (2); see its
//! documentation for what it gives up and how it is gated.
//!
//! # The layer model
//!
//! The guest's `/usr` and `/etc` are `overlayfs` mounts whose lower is the
//! host and whose upper is a user-owned layer: the host is read-only by
//! construction, an installed package is visible at its canonical path with
//! no restart, and deleting the layer deletes everything ever added.
//! [`layer::Layer::create`] exists because unprivileged overlayfs can
//! neither copy up a root-owned directory nor modify a root-owned file, so
//! the layer pre-creates the base's whole directory tree and seeds its own
//! copies of the rpm database and dnf's history — which also makes a layer
//! a *delta*, since installing `htop` downloads `htop`, not glibc.
//!
//! The base is read-only in a stronger sense than "mounted `ro`": you can
//! add to it and shadow it, but not alter a file that came from the image.
//!
//! # Permissions are Tilo's, not a second system
//!
//! `perm trust-dir`, and every other decision the broker makes, is a
//! `collab_types::caps::CapOverride` evaluated by `decide_capability` — the
//! same record the compositor stores, shows in its permissions panel and
//! can revoke. There is no `ush`-local permission type. See [`policy`].

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
pub mod layer;
pub mod pkg;
pub mod policy;
pub mod profile;
pub mod store;
pub mod supervisor;

pub use profile::Profile;
pub use store::Store;

/// The package id every `ush` grant is written against — an app id in the
/// capability vocabulary because that's what the compositor's permissions
/// panel renders and revokes; one subject for the whole runtime, not per
/// session, so trusting a directory stays trusted for the next shell too.
pub const RUNTIME_APP_ID: &str = "so.tilo.ush";

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
    // These are deliberately host-brokered. Running the image copies directly
    // inside the guest would update the shell layer instead of the user's apps,
    // and its private /run cannot reach systemd for image updates.
    "tilo",
    "app",
    "store",
    "system",
];

/// Commands for the user's installed legacy apps, from the legacy VM's
/// registry. A name the runtime already answers to keeps its meaning.
pub fn legacy_commands() -> Vec<String> {
    let Some(paths) = legacy_vm::config::Paths::for_user(legacy_vm::GuestOs::Linux) else {
        return Vec::new();
    };
    legacy_command_names(&legacy_vm::apps::Registry::load(&paths.apps()))
}

/// [`legacy_commands`] for one registry.
pub fn legacy_command_names(registry: &legacy_vm::apps::Registry) -> Vec<String> {
    let mut names: Vec<String> = registry
        .apps
        .iter()
        .map(|app| app.command.clone())
        .filter(|name| {
            legacy_vm::apps::valid_app_id(name)
                && name != "tilo-ush"
                && !GUEST_COMMANDS.contains(&name.as_str())
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

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

    #[test]
    fn legacy_apps_never_shadow_the_runtimes_own_commands() {
        let registry = legacy_vm::apps::Registry::from_guest(
            legacy_vm::GuestOs::Linux,
            &[
                installed_app("com.visualstudio.code"),
                installed_app("org.example.pkg"),
            ],
            &[],
        );
        assert_eq!(legacy_command_names(&registry), ["code"]);
    }

    fn installed_app(id: &str) -> legacy_vm::apps::InstalledApp {
        legacy_vm::apps::InstalledApp {
            id: id.into(),
            name: id.into(),
            window_class: None,
            comment: None,
            terminal: false,
        }
    }
}
