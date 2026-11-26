//! `ush`: an application runtime that hosts a shell. It builds a guest from an
//! environment record — `/usr` and `/etc` a read-only stack of images, its home its
//! own, granted directories bound at their own paths — puts a shell inside it, and
//! brokers every request that would reach past it. See `docs/app-model/ush.md`.
//!
//! Three parties, in decreasing authority:
//!
//! 1. the supervisor ([`supervisor`], [`broker`]), Tilo code outside any app sandbox,
//!    which needs `unshare`/`mount`/`pivot_root` because it creates the confinement;
//! 2. the guest ([`guest`]), untrusted: the real uid in its own user namespace, a
//!    network namespace reaching only the hosts its record grants ([`tilo_egress`]),
//!    and a seccomp filter denying the namespace syscalls so it cannot climb out;
//! 3. an app, which cannot reach this runtime at all: `apps/terminal` asks the
//!    supervisor over its service socket for a target and gets back a pty.
//!
//! Images are read-only, with no overlay upper: a guest adds software only by asking
//! (`pkg install` builds a tool image and adds it to the record). Permissions are the
//! record's `CapOverride`s, evaluated by `decide_capability` ([`policy`]), not a
//! second system.

pub mod approved_checks;
pub mod audit;
pub mod branch;
pub mod broker;
pub mod command_marks;
pub mod content;
pub mod environment_browsers;
pub mod environment_commands;
pub mod environment_terminals;
pub mod file_actions;
pub mod fleet;
pub mod guard;
pub mod guest;
pub mod guest_exec;
pub mod helper_runtime;
pub mod home;
pub mod host;
pub mod hosted_vm;
pub mod images;
pub mod machine_programs;
pub mod mcp_server;
pub mod policy;
pub mod records;
mod runtime_import;
pub mod seccomp;
pub mod setup;
pub mod shell_host;
pub mod shell_start;
pub mod store;
pub mod supervisor;
pub mod terminal_sessions;
pub mod tilo_side;
pub mod workspace;
pub mod workspace_shell;

pub use store::Store;
pub use tilo_environment::RUNTIME_APP_ID;

/// Where the guest sees the runtime's binary and its command shims: a `tmpfs` the
/// supervisor populates, first on `PATH`, not guest-writable.
pub const GUEST_BIN_DIR: &str = "/run/ush/bin";

pub const GUEST_BROKER_SOCK: &str = "/run/ush/broker.sock";

/// Names the runtime answers to from inside a guest, dispatched on `argv[0]`. Symlinks
/// to the runtime binary rather than shell builtins, so they behave the same under any
/// shell, a script or `xargs`.
pub const GUEST_COMMANDS: &[&str] = &[
    "pkg",
    "perm",
    "scan",
    "guard",
    "quarantine",
    "restart",
    // Deliberately host-brokered: the guest's images are not the user's apps or system, and
    // its private /run cannot reach systemd for image updates.
    "tilo",
    "app",
    "store",
    "system",
    // GNU's names, each one the environment record's own verb, checked against its grants
    // and undone from its History. A guest that ships its own copy gets this one instead,
    // because the copy could neither see the grants nor be an action.
    "cp",
    "mv",
    "rm",
    "mkdir",
    "rmdir",
    "ln",
    "truncate",
    // With only `truncate` and `cp`, changing a file's contents meant rewriting it;
    // `touch` creates without truncating and `append` adds.
    "touch",
    "append",
    "history",
    "tilo-runtime",
    "ush-sh",
];

/// Commands for the user's installed Linux apps, from the Linux VM's
/// registry. A name the runtime already answers to keeps its meaning.
pub fn linux_vm_commands() -> Vec<String> {
    let Some(paths) = linux_vm::config::Paths::for_user(linux_vm::GuestOs::Linux) else {
        return Vec::new();
    };
    linux_vm_command_names(&linux_vm::apps::Registry::load(&paths.apps()))
}

pub fn linux_vm_command_names(registry: &linux_vm::apps::Registry) -> Vec<String> {
    let mut names: Vec<String> = registry
        .apps
        .iter()
        .map(|app| app.command.clone())
        .filter(|name| {
            linux_vm::apps::valid_app_id(name)
                && name != "tilo-ush"
                && !GUEST_COMMANDS.contains(&name.as_str())
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

pub fn guest_command_available(base: &std::path::Path, command: &str) -> bool {
    match command {
        // `system` is an argv0 convenience alias for `tilo system`, not a generic name for any
        // machine with the `tilo` client installed. Do not put it on a guest's PATH unless this
        // is a Tilo system image with the deployment provider behind the client.
        "system" => system_provider_available(base),
        "tilo" => base.join("usr/bin/tilo").is_file(),
        "app" | "store" => base.join("usr/bin/app").is_file(),
        _ => true,
    }
}

fn system_provider_available(base: &std::path::Path) -> bool {
    base.join("usr/bin/tilo").is_file() && base.join("usr/libexec/tilo-deployment").is_file()
}
