//! `ush` — an application runtime that hosts a shell.
//!
//! Not a shell language. `ush` is a small user-space operating system layered
//! on top of the base system: it composes a root filesystem out of the host's
//! (read-only) and a stack of layers that belong to the user, puts a shell
//! inside it, and brokers every request that would reach past it.
//!
//! # Why this exists in qOS
//!
//! `apps/terminal` used to `exec` `$SHELL` directly. Inside a qOS app sandbox
//! that fails with `ENOENT`: [`crate::profile`]'s system read profile binds the
//! loader, fonts, `/sys` and the CA bundle, and deliberately binds no
//! executable directory at all. Adding `/usr/bin` to that list would hand every
//! app on the machine the host's entire command set, and minting a capability
//! per directory would produce a permission dialog for `/usr/share/terminfo`.
//!
//! A read-only host rootfs gives you `/usr/bin`, terminfo and `/etc/passwd` by
//! construction — not as a grant, as the base you are standing on. That is the
//! whole idea, and everything below follows from it.
//!
//! # The trust boundary
//!
//! Three principals, in decreasing authority. Read this before changing
//! anything in [`guest`], [`broker`] or [`profile`].
//!
//! 1. **The supervisor** ([`supervisor`], [`broker`]) is qOS code and runs in
//!    the user's session *outside* any app sandbox. It builds namespaces, so it
//!    needs `unshare`/`mount`/`pivot_root` — the very syscalls
//!    `crates/packaging/launcher/src/seccomp.rs` blanket-denies to apps. It is
//!    trusted with them because it is the thing that *creates* the confinement,
//!    the same way `bwrap` is. It owns the layer store, the policy file and the
//!    audit log, and none of those three is ever visible inside a guest.
//!
//! 2. **The guest** ([`guest`]) is everything the user runs: the shell, `vim`,
//!    a downloaded binary. It is untrusted. It runs as uid 0 *inside its own
//!    user namespace*, which maps to the unprivileged real user outside and
//!    confers no host authority whatsoever — it is what lets `rpm` chown a file
//!    into a layer the user owns. The seccomp filter installed on it still
//!    denies `unshare`, `mount`, `pivot_root`, `setns` and `chroot`, so a guest
//!    cannot build a second namespace and climb out of the first. **The
//!    supervisor's need for those syscalls does not propagate**: `bwrap`
//!    installs the filter after it has finished mounting and immediately before
//!    it execs the guest, so the syscalls exist for exactly the process that
//!    performs the setup and for no descendant of it.
//!
//! 3. **An app** is what qOS launches through `qos-launcher`. Apps are
//!    untrusted, including first-party ones, and an app cannot reach this
//!    runtime by executing it — a sandboxed app has neither the syscalls nor a
//!    path to the binary. `apps/terminal` reaches a guest by asking the
//!    supervisor over the service socket it was handed at launch, and what it
//!    gets back is a pty, not a namespace.
//!
//! [`Profile::Dsh`] is the single deliberate exception to (2): see its
//! documentation for what it gives up and how it is gated.
//!
//! # The layer model
//!
//! A layer is a directory that looks like a fragment of a root filesystem and
//! is owned by the real user. The guest's `/usr` and `/etc` are `overlayfs`
//! mounts whose lower is the host's and whose upper is a layer, so:
//!
//! - the host filesystem is read-only by construction — nothing in the guest
//!   can write through to it, because the upper always catches the write;
//! - what a package installs lands in the user's layer, immediately visible at
//!   its canonical path, with no restart and no privilege;
//! - deleting the layer deletes everything that was ever added.
//!
//! Two properties of unprivileged overlayfs shape the implementation, and
//! [`layer::Layer::create`] exists because of them:
//!
//! - **A root-owned directory cannot be copied up.** Host `/usr/bin` is owned
//!   by uid 0, which is *not mapped* into the guest's user namespace, and
//!   `CAP_DAC_OVERRIDE` does not apply to an inode whose owner is unmapped. So
//!   the layer pre-creates the base's whole directory tree, owned by the user.
//!   Then no copy-up is needed to create a file anywhere the base has a
//!   directory.
//! - **A root-owned file cannot be modified.** The rpm database and dnf's
//!   transaction history are files the package manager must rewrite, so the
//!   layer seeds its own copies of them. That is also what makes a layer a
//!   *delta*: the transaction starts from the base image's installed set, and
//!   installing `htop` downloads `htop`, not glibc.
//!
//! The base is therefore read-only in a stronger sense than "mounted `ro`": you
//! can add to it and you can shadow it, but you cannot alter a file that came
//! from the image. That is a property worth having in an image-based OS.
//!
//! # Permissions are qOS's, not a second system
//!
//! `perm trust-dir`, and every other decision the broker makes, is a
//! `collab_types::caps::CapOverride` evaluated by `decide_capability` — the
//! same record the compositor stores on a workspace, shows in its permissions
//! panel and can revoke, and the same deny-biased function the launcher and the
//! filesystem call. There is no `ush`-local permission type. See [`policy`].

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

/// The package id every `ush` grant is written against.
///
/// It is an app id in the capability vocabulary because that is what the
/// compositor's permissions panel knows how to render and revoke. The runtime
/// is one subject, not one subject per session: a directory you trusted stays
/// trusted for the next shell you open, and revoking it in the panel takes it
/// away from all of them.
pub const RUNTIME_APP_ID: &str = "dev.qos.ush";

/// Where the guest can see the runtime's own binary and its command shims.
///
/// A `tmpfs` the supervisor populates, first on the guest's `PATH`. Nothing
/// under it is writable by the guest.
pub const GUEST_BIN_DIR: &str = "/run/ush/bin";

/// The broker socket, as the guest sees it.
pub const GUEST_BROKER_SOCK: &str = "/run/ush/broker.sock";

/// Names the runtime answers to from inside a guest, dispatched on `argv[0]`.
///
/// These are the commands the reference runtime publishes. They are symlinks to
/// the runtime binary rather than shell builtins, so they behave identically
/// under `bash`, `fish`, a script, or `xargs`.
pub const GUEST_COMMANDS: &[&str] = &["pkg", "perm", "scan", "guard", "quarantine", "restart"];
