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

pub mod agent_door;
pub mod approved_checks;
pub mod audit;
pub mod branch;
pub mod broker;
pub mod content;
pub mod environment_browsers;
pub mod environment_runs;
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
pub mod policy;
pub mod records;
pub mod runs;
mod runtime_import;
pub mod seccomp;
pub mod setup;
pub mod shell_host;
pub mod shell_start;
pub mod store;
pub mod supervisor;
pub mod terminal_sessions;
pub mod tilo_side;
pub mod version;
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
    "tilo-runtime-mcp",
    // A file's history, read and restored through the mount's own protocols, so a script
    // reads the same revisions the file manager's History view lists.
    "version",
    "ush-sh",
];

/// Commands for the user's installed legacy apps, from the legacy VM's
/// registry. A name the runtime already answers to keeps its meaning.
pub fn legacy_commands() -> Vec<String> {
    let Some(paths) = legacy_vm::config::Paths::for_user(legacy_vm::GuestOs::Linux) else {
        return Vec::new();
    };
    legacy_command_names(&legacy_vm::apps::Registry::load(&paths.apps()))
}

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

#[cfg(test)]
macro_rules! require_trusted_loader {
    () => {
        tilo_test_env::require_capability!(
            TrustedLoader,
            $crate::helper_runtime::native_loader_trusted(),
            "the helper's native loader"
        );
    };
}
#[cfg(test)]
pub(crate) use require_trusted_loader;

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
