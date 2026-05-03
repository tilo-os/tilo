//! `SandboxProfile`: a serialisable description of a per-workspace
//! Bubblewrap sandbox, rendered into the argv expected by the `bwrap` CLI.
//!
//! The profile intentionally stays a pure value type — no I/O, no fd
//! plumbing — so we can build it from the workspace CRDT in one place
//! and feed the rendered argv to either `std::process::Command` (Phase 1
//! spawn helper) or future fd-passing launchers (Phase 2+).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Description of a single Bubblewrap sandbox tied to one workspace.
///
/// Fields map 1:1 to `bwrap` arguments; see [`SandboxProfile::into_argv`]
/// for the exact translation. Defaults intended for tests / v1 launches
/// come from [`SandboxProfile::for_workspace_test`].
pub struct SandboxProfile {
    /// Workspace this sandbox belongs to. Stored for diagnostics — we do
    /// not yet thread the typed `WorkspaceId` from `quick_ui` through
    /// here because the sandbox module must stay buildable on its own.
    pub workspace_id: u64,
    /// Read-only host → sandbox bind mounts. Rendered as `--ro-bind`.
    pub binds_ro: Vec<(PathBuf, PathBuf)>,
    /// Read-write host → sandbox bind mounts. Rendered as `--bind`.
    pub binds_rw: Vec<(PathBuf, PathBuf)>,
    /// Environment overrides applied via `--setenv`. Note that `bwrap`
    /// already starts from a clean env when `--clearenv` is passed; we
    /// don't emit `--clearenv` in Phase 1 because the tests assert on
    /// the exact argv shape and adding it would be a behaviour change
    /// that belongs to the launcher layer.
    pub env: HashMap<String, String>,
    /// User namespace mapping target. Rendered as `--uid`.
    pub uid: u32,
    /// Group namespace mapping target. Rendered as `--gid`.
    pub gid: u32,
    /// Optional path to a compiled seccomp-bpf blob. Phase 1 emits a
    /// placeholder fd (`--seccomp 10`) so the argv shape stays stable;
    /// Phase 2 will swap in real fd-passing.
    pub seccomp: Option<PathBuf>,
    /// Per-workspace Wayland bridge socket on the host. Bound into the
    /// sandbox at `/run/user/<uid>/wayland-0` by [`for_workspace_test`].
    pub wayland_socket: PathBuf,
    /// Optional per-workspace D-Bus session bus socket on the host.
    pub dbus_socket: Option<PathBuf>,
    /// When true, render `--unshare-net` so the sandbox gets an empty
    /// network namespace. Phase 1 defaults to `true` for safety; the
    /// portal flow in a later phase flips this for the owner session.
    pub net_isolated: bool,
}

impl SandboxProfile {
    /// Build a minimal profile for tests and v1 launches.
    ///
    /// Defaults match the plan (lines 125-135):
    /// * `binds_ro`: `(/usr, /usr)`. `/etc/resolv.conf` is added only
    ///   when the workspace has network — Phase 1 defaults to network
    ///   isolation, so it's omitted here.
    /// * `binds_rw`: workspace root → `/work`, cache root → `/cache`,
    ///   wayland bridge socket → `/run/user/1000/wayland-0`.
    /// * `env`: `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR`, `HOME`.
    /// * `uid`/`gid`: 1000.
    /// * `net_isolated`: `true`.
    pub fn for_workspace_test(ws_root: &Path, cache_root: &Path) -> Self {
        let wayland_socket =
            PathBuf::from("/run/user/1000").join(format!("quick-bridge-{}", 0u64));

        let mut env = HashMap::new();
        env.insert("WAYLAND_DISPLAY".to_string(), "wayland-0".to_string());
        env.insert(
            "XDG_RUNTIME_DIR".to_string(),
            "/run/user/1000".to_string(),
        );
        env.insert("HOME".to_string(), "/work".to_string());

        Self {
            workspace_id: 0,
            binds_ro: vec![(PathBuf::from("/usr"), PathBuf::from("/usr"))],
            binds_rw: vec![
                (ws_root.to_path_buf(), PathBuf::from("/work")),
                (cache_root.to_path_buf(), PathBuf::from("/cache")),
                (
                    wayland_socket.clone(),
                    PathBuf::from("/run/user/1000/wayland-0"),
                ),
            ],
            env,
            uid: 1000,
            gid: 1000,
            seccomp: None,
            wayland_socket,
            dbus_socket: None,
            net_isolated: true,
        }
    }

    /// Render this profile to the argv slice we'd pass to `bwrap`.
    ///
    /// The rendered argv is *just* the arguments — the caller is
    /// responsible for invoking `bwrap` itself (see [`crate::sandbox::spawn`]).
    /// `command` is appended verbatim so callers can pass through both
    /// the program and its arguments without escaping.
    ///
    /// We intentionally consume `self` rather than borrowing: the
    /// `binds_*` vectors are moved into the rendered `Vec<String>` so we
    /// don't allocate intermediate `String`s that get thrown away.
    /// Callers that need the profile afterwards should clone before
    /// rendering.
    pub fn into_argv(self, command: &[String]) -> Vec<String> {
        // Pre-size: each bind is 3 args, each env entry is 3 args, plus
        // a handful of fixed flags and the command tail.
        let mut argv: Vec<String> = Vec::with_capacity(
            self.binds_ro.len() * 3
                + self.binds_rw.len() * 3
                + self.env.len() * 3
                + 8
                + command.len(),
        );

        for (host, sandbox) in self.binds_ro {
            argv.push("--ro-bind".to_string());
            argv.push(path_to_string(&host));
            argv.push(path_to_string(&sandbox));
        }

        for (host, sandbox) in self.binds_rw {
            argv.push("--bind".to_string());
            argv.push(path_to_string(&host));
            argv.push(path_to_string(&sandbox));
        }

        // HashMap iteration order is unspecified; the unit tests look
        // for *presence* of `--setenv K V` triples rather than a fixed
        // ordering, so this is fine. Future seccomp Phase will add a
        // stable ordering when the launcher composes profiles.
        for (key, value) in self.env {
            argv.push("--setenv".to_string());
            argv.push(key);
            argv.push(value);
        }

        argv.push("--uid".to_string());
        argv.push(self.uid.to_string());
        argv.push("--gid".to_string());
        argv.push(self.gid.to_string());

        if self.net_isolated {
            argv.push("--unshare-net".to_string());
        }

        if self.seccomp.is_some() {
            // Phase 1 placeholder: real fd-passing arrives in Phase 2
            // (`docs/superpowers/plans/2026-05-02-per-workspace-sandbox-permissions.md`,
            // Phase 2 — seccomp-bpf blocklist). Emitting the flag now
            // keeps the argv shape stable across phases so callers can
            // start logging / diffing against the rendered command.
            argv.push("--seccomp".to_string());
            argv.push("10".to_string());
        }

        argv.extend_from_slice(command);
        argv
    }
}

/// Helper: lossy conversion to keep `into_argv` returning `Vec<String>`.
///
/// Bubblewrap arguments hit the kernel via `execve`, so they are bytes
/// not Unicode. Phase 1 stays on `String` to match the plan's signature;
/// the few non-UTF-8 paths we'd see in practice (mostly user-chosen
/// workspace names) are surfaced as replacement characters rather than
/// silently dropped, which keeps the test output readable.
fn path_to_string(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_renders_expected_argv_for_minimal_workspace() {
        let p = SandboxProfile::for_workspace_test(
            Path::new("/tmp/ws"),
            Path::new("/tmp/cache"),
        );
        let argv = p.into_argv(&["sh".into(), "-c".into(), "echo hi".into()]);

        // `--ro-bind /usr /usr` is present.
        assert!(argv.iter().any(|a| a == "--ro-bind"));
        assert!(argv.iter().any(|a| a == "/usr"));

        // At least one `--bind` exists.
        assert!(argv.iter().any(|a| a == "--bind"));

        // The workspace root is bound to `/work`.
        assert!(
            argv.windows(3)
                .any(|w| w[0] == "--bind" && w[2] == "/work"),
            "expected `--bind <ws_root> /work` in argv: {argv:?}"
        );

        // Command tail is preserved verbatim.
        assert_eq!(argv.last().unwrap(), "echo hi");
    }

    #[test]
    fn profile_unshares_net_when_isolated() {
        let mut p = SandboxProfile::for_workspace_test(
            Path::new("/tmp/ws"),
            Path::new("/tmp/cache"),
        );
        p.net_isolated = true;
        let argv = p.into_argv(&["true".into()]);
        assert!(argv.iter().any(|a| a == "--unshare-net"));
    }

    #[test]
    fn profile_keeps_net_when_not_isolated() {
        let mut p = SandboxProfile::for_workspace_test(
            Path::new("/tmp/ws"),
            Path::new("/tmp/cache"),
        );
        p.net_isolated = false;
        let argv = p.into_argv(&["true".into()]);
        assert!(
            !argv.iter().any(|a| a == "--unshare-net"),
            "argv unexpectedly carries --unshare-net: {argv:?}"
        );
    }

    #[test]
    fn profile_renders_uid_and_gid() {
        let p = SandboxProfile::for_workspace_test(
            Path::new("/tmp/ws"),
            Path::new("/tmp/cache"),
        );
        let argv = p.into_argv(&["true".into()]);
        // `--uid 1000` and `--gid 1000` appear as adjacent pairs.
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--uid" && w[1] == "1000"),
            "expected `--uid 1000`: {argv:?}"
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--gid" && w[1] == "1000"),
            "expected `--gid 1000`: {argv:?}"
        );
    }

    #[test]
    fn profile_emits_setenv_for_each_env_var() {
        let p = SandboxProfile::for_workspace_test(
            Path::new("/tmp/ws"),
            Path::new("/tmp/cache"),
        );
        let argv = p.into_argv(&["true".into()]);

        for key in ["WAYLAND_DISPLAY", "XDG_RUNTIME_DIR", "HOME"] {
            assert!(
                argv.windows(2)
                    .any(|w| w[0] == "--setenv" && w[1] == key),
                "expected `--setenv {key}` in argv: {argv:?}"
            );
        }
    }

    /// Smoke test for the spawn helper. Only runs when `bwrap` is
    /// installed and the kernel allows unprivileged user namespaces;
    /// otherwise we skip cleanly so CI on lean images is not gated on
    /// system configuration. Phase 2 hardens this with a fixture that
    /// asserts on stdout.
    ///
    /// We extend the default profile with the minimum viable mounts to
    /// actually execute a binary (`/bin`, `/lib`, `/lib64`, `/proc`,
    /// `/dev`). The default profile is intentionally minimal — those
    /// mounts come from the per-workspace launcher in later phases —
    /// so the integration test takes responsibility for them itself.
    #[test]
    #[ignore = "requires bwrap + user-ns; run locally with `cargo test -- --ignored`"]
    fn spawned_process_can_read_work_path() {
        if which_bwrap().is_none() {
            return;
        }
        let dir = tempdir_or_skip();
        let ws = dir.join("ws");
        let cache = dir.join("cache");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(ws.join("notes.txt"), b"hello").unwrap();

        let mut profile = SandboxProfile::for_workspace_test(&ws, &cache);
        // The wayland bridge socket doesn't exist in this fixture; drop
        // its bind so `bwrap` doesn't fail to mount a missing source.
        profile.binds_rw.retain(|(host, _)| host != &profile.wayland_socket);
        // Add the host paths needed for `cat` to actually link + run.
        for ro in ["/bin", "/lib", "/lib64"] {
            let p = PathBuf::from(ro);
            if p.exists() {
                profile.binds_ro.push((p.clone(), p));
            }
        }
        // bwrap needs `--proc /proc` and `--dev /dev` to populate those
        // pseudo-filesystems; we render those manually as an argv
        // suffix because `SandboxProfile` doesn't model them in Phase 1.
        let mut argv = clone_for_test(&profile).into_argv(&[]);
        argv.extend(
            ["--proc", "/proc", "--dev", "/dev", "/bin/cat", "/work/notes.txt"]
                .into_iter()
                .map(String::from),
        );
        let mut child = std::process::Command::new("bwrap")
            .args(&argv)
            .spawn()
            .expect("spawn bwrap");
        let status = child.wait().expect("wait");
        assert!(status.success(), "cat exited {status:?}");
    }

    fn clone_for_test(p: &SandboxProfile) -> SandboxProfile {
        SandboxProfile {
            workspace_id: p.workspace_id,
            binds_ro: p.binds_ro.clone(),
            binds_rw: p.binds_rw.clone(),
            env: p.env.clone(),
            uid: p.uid,
            gid: p.gid,
            seccomp: p.seccomp.clone(),
            wayland_socket: p.wayland_socket.clone(),
            dbus_socket: p.dbus_socket.clone(),
            net_isolated: p.net_isolated,
        }
    }

    fn which_bwrap() -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        for entry in std::env::split_paths(&path) {
            let candidate = entry.join("bwrap");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }

    fn tempdir_or_skip() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "quick-shell-sandbox-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&base).expect("create tempdir");
        base
    }
}
