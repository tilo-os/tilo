//! `SandboxProfile`: a serialisable description of a per-workspace
//! Bubblewrap sandbox, rendered into the argv expected by the `bwrap` CLI.
//! A pure value type — no I/O, no fd plumbing — so it can be built from
//! the workspace CRDT in one place and rendered wherever the argv is
//! needed.
//!
//! This is not the enforcement point: nothing in the compositor spawns an
//! application through this type. Apps are launched by execing
//! `qos-launcher`, which builds its own bubblewrap namespace from the
//! package manifest **and** the workspace capabilities the shell hands it
//! (`launcher::capabilities`, via [`crate::shell::launcher`]) — that is
//! where a grant becomes a mount and a revocation becomes a missing one.
//! What remains here is the compositor-side *model*: the bind/env/uid
//! shape, the seccomp + Landlock plumbing points, and
//! [`SandboxProfile::apply_grants`], which mirrors the launcher's
//! capability rules. Treat a disagreement between the two as a bug here,
//! not in the launcher.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Per-directory memo of the entries we care about during
/// capability-grant application (`/dev/video*` for camera, the
/// pipewire socket + `/dev/snd` for microphone). `apply_grants` runs
/// once per process spawn into a workspace; without memoization every
/// cluster launch re-walks the same `/dev` tree. TTL is 5s: short enough
/// that hotplugged hardware shows up within a couple of launches, long
/// enough to coalesce the rapid-fire spawns from a user double-clicking
/// a launcher.
///
/// Cache shape: `directory path -> (filtered entries, captured-at)`.
/// The filtering function is opaque to the cache; consumers pass a
/// closure-free key so different filters of the same directory get
/// independent cache slots.
type ScanMemoKey = (PathBuf, &'static str);
/// Cached entries for a [`ScanMemoKey`], plus when they were captured.
type ScanMemoEntry = (Vec<PathBuf>, Instant);
static SCAN_MEMO: LazyLock<Mutex<HashMap<ScanMemoKey, ScanMemoEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// TTL for cached `/dev` and `XDG_RUNTIME_DIR` scans.
const SCAN_MEMO_TTL: Duration = Duration::from_secs(5);

/// Test hook: counts how many times the underlying directory walk runs
/// the slow path (filesystem hit); the cached happy path increments
/// nothing. Production never reads this.
#[cfg(test)]
static SCAN_WALK_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
pub(crate) fn take_scan_walk_count() -> usize {
    SCAN_WALK_COUNT.swap(0, std::sync::atomic::Ordering::SeqCst)
}

#[cfg(test)]
pub(crate) fn clear_scan_memo() {
    SCAN_MEMO.lock().unwrap().clear();
}

/// Memoised directory walk. `filter_tag` is a stable string id for the
/// filter being applied (e.g. `"video"` for `/dev/video*`) — it keys the
/// cache so two different filters of the same directory get independent
/// cache slots.
fn scan_dir_memoised(
    dir: &Path,
    filter_tag: &'static str,
    filter: fn(&str) -> bool,
) -> Vec<PathBuf> {
    let key = (dir.to_path_buf(), filter_tag);
    {
        let cache = SCAN_MEMO.lock().expect("SCAN_MEMO poisoned");
        if let Some((entries, captured)) = cache.get(&key)
            && captured.elapsed() < SCAN_MEMO_TTL
        {
            return entries.clone();
        }
    }
    // Slow path: actually walk the directory.
    #[cfg(test)]
    SCAN_WALK_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str())
                && filter(name)
            {
                out.push(path);
            }
        }
    }
    let mut cache = SCAN_MEMO.lock().expect("SCAN_MEMO poisoned");
    cache.insert(key, (out.clone(), Instant::now()));
    out
}

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
    /// Environment overrides applied via `--setenv`. `bwrap` already
    /// starts from a clean env when `--clearenv` is passed, but this
    /// doesn't emit it: the tests assert the exact argv shape, and
    /// adding the flag is a behaviour change that belongs to the
    /// launcher layer.
    pub env: HashMap<String, String>,
    /// User namespace mapping target. Rendered as `--uid`.
    pub uid: u32,
    /// Group namespace mapping target. Rendered as `--gid`.
    pub gid: u32,
    /// Optional path to a compiled seccomp-bpf blob.
    ///
    /// A spawner must pre-open this file before forking, keep the fd
    /// open across `execve` (`pre_exec` + `dup2` into a reserved slot),
    /// and record that descriptor in [`Self::seccomp_fd`] so the argv
    /// carries the *actual* fd number. Rendering `--seccomp` with a
    /// guessed number leaves bwrap reading whatever happens to be at
    /// that slot — typically nothing, silently dropping the filter.
    /// See [`SandboxProfile::into_argv`].
    pub seccomp: Option<PathBuf>,
    /// Fd number to render in the `--seccomp <fd>` argv triple, once a
    /// spawner has plumbed the blob to that descriptor in the child.
    /// `None` when [`Self::seccomp`] is also `None`; when both are set
    /// the argv carries the matching number.
    ///
    /// Left `None` on a profile built by hand: the argv rendering then
    /// suppresses the `--seccomp` flag rather than emitting a stale
    /// magic number.
    pub seccomp_fd: Option<i32>,
    /// Per-workspace Wayland bridge socket on the host. Bound into the
    /// sandbox at `/run/user/<uid>/wayland-0` by [`for_workspace_test`].
    pub wayland_socket: PathBuf,
    /// Optional per-workspace D-Bus session bus socket on the host.
    pub dbus_socket: Option<PathBuf>,
    /// When true, render `--unshare-net` so the sandbox gets an empty
    /// network namespace. Defaults to `true` for safety; the portal flow
    /// flips this for the owner session.
    pub net_isolated: bool,
    /// Optional path to the Landlock launcher shim (`plan/security.md`
    /// §7.1). When `Some`, [`Self::into_argv`] makes `bwrap` exec the
    /// shim — `<shim> __sandbox-exec --ro <p>… --rw <p>… [--net-restricted]
    /// -- <real argv>` — instead of the app directly. The shim applies an
    /// in-kernel Landlock ruleset (derived from this profile's binds) to
    /// the app process and all its descendants *after* bwrap has set up
    /// the namespace, then `execvp`s the real app. Layered with seccomp:
    /// both filters apply.
    ///
    /// A spawner points this at the running `shell` binary
    /// (`current_exe()`), mirroring the `seccomp_fd` plumbing. Profiles
    /// built by hand leave it `None`, in which case argv rendering
    /// appends the command verbatim — the process still runs inside
    /// bwrap + seccomp, just without the extra Landlock layer.
    pub landlock_shim: Option<PathBuf>,
}

impl SandboxProfile {
    /// Build a minimal profile for tests and v1 launches.
    pub fn for_workspace_test(ws_root: &Path, cache_root: &Path) -> Self {
        let wayland_socket = PathBuf::from("/run/user/1000").join(format!("quick-bridge-{}", 0u64));

        let mut env = HashMap::new();
        env.insert("WAYLAND_DISPLAY".to_string(), "wayland-0".to_string());
        env.insert("XDG_RUNTIME_DIR".to_string(), "/run/user/1000".to_string());
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
            seccomp_fd: None,
            wayland_socket,
            dbus_socket: None,
            net_isolated: true,
            // The spawn helper stamps this with `current_exe()` before
            // rendering argv; a bare test profile renders the command
            // verbatim (no shim) so existing argv-shape tests are unchanged.
            landlock_shim: None,
        }
    }

    /// Apply a camera capability grant: bind `/dev/video*` device nodes
    /// into the sandbox so the app can open camera devices. Without this,
    /// the devices simply aren't reachable from inside the sandbox — the
    /// kernel still controls read/write semantics on the device itself,
    /// so we mount them read-write (a `--ro-bind` would prevent
    /// `VIDIOC_*` ioctls that legitimate camera apps need).
    ///
    /// The walk over `/dev` is best-effort: on hosts without any
    /// `/dev/video*` nodes (CI, headless servers) this is a no-op and
    /// the grant becomes a paper tiger — that's fine for v1, since the
    /// alternative would be to fail the launch when the user has merely
    /// granted a permission that no hardware satisfies.
    pub fn grant_camera(&mut self) {
        // D-6: memoise the `/dev` walk so back-to-back cluster spawns
        // don't re-stat every entry. TTL = 5s; hotplug latency is
        // bounded by the next post-TTL launch.
        let video_devices =
            scan_dir_memoised(Path::new("/dev"), "video", |name| name.starts_with("video"));
        for path in video_devices {
            // Bind into the sandbox at the same path. Kernel controls
            // read/write semantics on the device itself, so we bind
            // rw rather than ro (a ro-bind blocks the VIDIOC_* ioctls
            // legitimate camera apps need).
            self.binds_rw.push((path.clone(), path));
        }
    }

    /// Apply a microphone capability grant: bind the per-user Pipewire
    /// socket so the app can connect to the audio server. Falls back to
    /// the legacy ALSA `/dev/snd` path when present, since some apps
    /// still link against `libasound` directly. Pipewire is the default
    /// on modern desktops; both paths are bound so apps can pick.
    ///
    /// Like [`grant_camera`], this is best-effort: a host without
    /// Pipewire and without `/dev/snd` simply gets nothing bound, and
    /// the launch still succeeds. The portal layer is responsible for
    /// not granting the capability on hosts where it can't be honoured.
    pub fn grant_microphone(&mut self) {
        let runtime_dir =
            std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| format!("/run/user/{}", self.uid));
        let pipewire_path = std::path::PathBuf::from(format!("{runtime_dir}/pipewire-0"));
        if pipewire_path.exists() {
            self.binds_rw.push((pipewire_path.clone(), pipewire_path));
        }
        // Fallback: bind /dev/snd if it exists (legacy ALSA path).
        let snd_path = std::path::PathBuf::from("/dev/snd");
        if snd_path.exists() {
            self.binds_rw.push((snd_path.clone(), snd_path));
        }
    }

    /// Walk a workspace's `grants` slice for one app and apply each
    /// granted capability to this profile. Handles `Camera` /
    /// `Microphone`, `Filesystem` grants scoped `Always` or `Session`,
    /// and `Always`-scoped `Network` grants. `Screencast` is a no-op here
    /// (portal screencast follow-up).
    ///
    /// Filesystem grants bind into the sandbox for every scope except
    /// `Once`: a one-shot grant that outlived the single request would
    /// over-persist the access, so it's delivered as a portal fd instead.
    /// This method only ever appends binds, never removes them — callers
    /// wanting a minimal profile should start from [`for_workspace_test`]
    /// with an empty grants slice.
    pub fn apply_grants(&mut self, grants: &[crate::shell::state::Capability]) {
        use crate::shell::state::{Capability, Expiry};
        for cap in grants {
            match cap {
                Capability::Camera { .. } => self.grant_camera(),
                Capability::Microphone { .. } => self.grant_microphone(),
                Capability::Filesystem { path, mode, scope } => {
                    // `EndOfSession` must bind: it's the scope a dropped
                    // file records, and refusing it would leave the grant
                    // recorded while the app can never read the file. The
                    // bind has session lifetime because it's rebuilt from
                    // the capability list on every spawn and is never
                    // written to disk. `AfterUse` stays fd-only — binding
                    // a one-shot into a longer-lived sandbox would
                    // over-persist it.
                    if !matches!(scope, Expiry::AfterUse) {
                        self.grant_filesystem_path(path, mode);
                    }
                }
                Capability::Network { hosts, scope } => {
                    // A network grant lifts the default namespace
                    // isolation (`--unshare-net`) — the only mechanism in
                    // play; there is no packet filter or nftables code
                    // anywhere in this repository. An allowlist naming
                    // specific hosts is recorded faithfully and then
                    // applied as unrestricted outbound access — say so
                    // out loud so an operator reading the journal can see
                    // that the allowlist is not what is protecting them.
                    if !scope.persists_across_restart() {
                        continue;
                    }
                    if let Some(list) = hosts {
                        let specific: Vec<&String> =
                            list.iter().filter(|h| h.as_str() != "*").collect();
                        if !specific.is_empty() {
                            tracing::warn!(
                                workspace = self.workspace_id,
                                hosts = ?specific,
                                "network grant names specific hosts, but the sandbox can only \
                                 share or unshare the whole network namespace: applying it as \
                                 UNRESTRICTED outbound network access. Per-host filtering is \
                                 not enforced."
                            );
                        }
                    }
                    self.net_isolated = false;
                }
                // Screencast: out of scope for this unit — handled by
                // the portal screencast follow-up.
                _ => {}
            }
        }
    }

    /// Bind a host path into the sandbox at the same path. `Read`
    /// grants land in `binds_ro`, `ReadWrite` in `binds_rw`. Idempotent:
    /// re-granting the same path is a no-op so callers can replay the
    /// CRDT's grant list on every spawn without bookkeeping.
    pub fn grant_filesystem_path(
        &mut self,
        path: &std::path::Path,
        mode: &crate::shell::state::FsMode,
    ) {
        use crate::shell::state::FsMode;
        let target = match mode {
            FsMode::Read => &mut self.binds_ro,
            FsMode::ReadWrite => &mut self.binds_rw,
        };
        if !target.iter().any(|(src, _)| src == path) {
            target.push((path.to_path_buf(), path.to_path_buf()));
        }
    }

    /// Render this profile to the argv slice passed to `bwrap`. Just the
    /// arguments — the caller invokes `bwrap` itself. `command` is
    /// appended verbatim so callers can pass through both program and
    /// arguments without escaping.
    ///
    /// Consumes `self` rather than borrowing: the `binds_*` vectors move
    /// into the rendered `Vec<String>` instead of allocating intermediate
    /// `String`s that get thrown away. Clone first if you need the
    /// profile afterwards.
    pub fn into_argv(self, command: &[String]) -> Vec<String> {
        // Capture the bits the Landlock shim needs BEFORE the consuming
        // `for (host, sandbox) in self.binds_*` loops move the binds out.
        // The shim runs *inside* bwrap's mount namespace, so it must
        // restrict the *sandbox-side* (destination) paths — `/work`,
        // `/usr`, … — not the host source paths.
        let landlock_shim = self.landlock_shim.clone();
        let (readonly_targets, readwrite_targets): (Vec<PathBuf>, Vec<PathBuf>) =
            if landlock_shim.is_some() {
                (
                    self.binds_ro
                        .iter()
                        .map(|(_, sandbox)| sandbox.clone())
                        .collect(),
                    self.binds_rw
                        .iter()
                        .map(|(_, sandbox)| sandbox.clone())
                        .collect(),
                )
            } else {
                (Vec::new(), Vec::new())
            };
        let net_restricted = self.net_isolated;

        // Pre-size: each bind is 3 args, each env entry is 3 args, plus
        // a handful of fixed flags, the optional Landlock shim wrapper
        // (shim + subcommand + 2 args per ro/rw path + flag + `--`), and
        // the command tail.
        let mut argv: Vec<String> = Vec::with_capacity(
            self.binds_ro
                .len()
                .saturating_mul(3)
                .saturating_add(self.binds_rw.len().saturating_mul(3))
                .saturating_add(self.env.len().saturating_mul(3))
                .saturating_add(8)
                .saturating_add(if landlock_shim.is_some() {
                    readonly_targets
                        .len()
                        .saturating_mul(2)
                        .saturating_add(readwrite_targets.len().saturating_mul(2))
                        .saturating_add(3)
                } else {
                    0
                })
                .saturating_add(command.len()),
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

        // HashMap iteration order is unspecified; the unit tests check
        // for *presence* of `--setenv K V` triples rather than a fixed
        // ordering.
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

        // Only emit `--seccomp <fd>` when a spawner actually plumbed the
        // file to a known descriptor (`seccomp_fd` is `Some`) — otherwise
        // bwrap reads whatever happens to be at a guessed fd, typically
        // nothing, and the filter is silently bypassed. A profile
        // rendered for inspection leaves `seccomp_fd` `None` and drops
        // the flag entirely.
        if let Some(fd) = self.seccomp_fd {
            argv.push("--seccomp".to_string());
            argv.push(fd.to_string());
        }

        // See `landlock_shim`'s field doc for the wire format. The shim
        // applies read-only Landlock rules for ro binds and
        // read+write+create for rw, and denies TCP bind/connect when
        // net-restricted, before `execvp`-ing the real command. seccomp
        // still applies on top.
        match landlock_shim {
            Some(shim) => {
                argv.push(path_to_string(&shim));
                argv.push(crate::sandbox::SANDBOX_EXEC_SUBCOMMAND.to_string());
                for sandbox_path in &readonly_targets {
                    argv.push("--ro".to_string());
                    argv.push(path_to_string(sandbox_path));
                }
                for sandbox_path in &readwrite_targets {
                    argv.push("--rw".to_string());
                    argv.push(path_to_string(sandbox_path));
                }
                if net_restricted {
                    argv.push("--net-restricted".to_string());
                }
                argv.push("--".to_string());
                argv.extend_from_slice(command);
            }
            None => argv.extend_from_slice(command),
        }
        argv
    }
}

/// Lossy conversion to keep `into_argv` returning `Vec<String>`.
///
/// Bubblewrap arguments hit the kernel via `execve`, so they are bytes,
/// not Unicode. The few non-UTF-8 paths seen in practice (mostly
/// user-chosen workspace names) are surfaced as replacement characters
/// rather than silently dropped, which keeps test output readable.
fn path_to_string(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    // `apply_grants` reads/writes the process-wide `SCAN_MEMO` static (the
    // per-directory memoisation cache). Tests exercising that path are
    // tagged `#[serial(scan_memo)]` so parallel `cargo test` runs don't
    // corrupt the shared walk counter the memo test asserts on.
    use serial_test::serial;

    #[test]
    fn profile_renders_expected_argv_for_minimal_workspace() {
        let p = SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        let argv = p.into_argv(&["sh".into(), "-c".into(), "echo hi".into()]);

        assert!(argv.iter().any(|a| a == "--ro-bind"));
        assert!(argv.iter().any(|a| a == "/usr"));

        assert!(argv.iter().any(|a| a == "--bind"));

        assert!(
            argv.windows(3).any(|w| w[0] == "--bind" && w[2] == "/work"),
            "expected `--bind <ws_root> /work` in argv: {argv:?}"
        );

        assert_eq!(argv.last().unwrap(), "echo hi");
    }

    #[test]
    fn profile_unshares_net_when_isolated() {
        let mut p =
            SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        p.net_isolated = true;
        let argv = p.into_argv(&["true".into()]);
        assert!(argv.iter().any(|a| a == "--unshare-net"));
    }

    #[test]
    fn profile_keeps_net_when_not_isolated() {
        let mut p =
            SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        p.net_isolated = false;
        let argv = p.into_argv(&["true".into()]);
        assert!(
            !argv.iter().any(|a| a == "--unshare-net"),
            "argv unexpectedly carries --unshare-net: {argv:?}"
        );
    }

    #[test]
    fn profile_renders_uid_and_gid() {
        let p = SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        let argv = p.into_argv(&["true".into()]);
        assert!(
            argv.windows(2).any(|w| w[0] == "--uid" && w[1] == "1000"),
            "expected `--uid 1000`: {argv:?}"
        );
        assert!(
            argv.windows(2).any(|w| w[0] == "--gid" && w[1] == "1000"),
            "expected `--gid 1000`: {argv:?}"
        );
    }

    #[test]
    fn profile_emits_setenv_for_each_env_var() {
        let p = SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        let argv = p.into_argv(&["true".into()]);

        for key in ["WAYLAND_DISPLAY", "XDG_RUNTIME_DIR", "HOME"] {
            assert!(
                argv.windows(2).any(|w| w[0] == "--setenv" && w[1] == key),
                "expected `--setenv {key}` in argv: {argv:?}"
            );
        }
    }

    #[test]
    fn grant_camera_appends_video_binds_when_devices_exist() {
        let mut p =
            SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        let initial_count = p.binds_rw.len();
        p.grant_camera();
        // /dev/video* may or may not exist on CI; assert "no shrink".
        assert!(p.binds_rw.len() >= initial_count);
        if std::path::Path::new("/dev/video0").exists() {
            assert!(
                p.binds_rw
                    .iter()
                    .any(|(src, _)| src == std::path::Path::new("/dev/video0"))
            );
        }
    }

    #[test]
    fn grant_microphone_does_not_panic() {
        let mut p =
            SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        p.grant_microphone();
        // Pipewire/ALSA may be absent in CI; the contract is "no panic".
    }

    #[test]
    #[serial(scan_memo)]
    fn apply_grants_with_camera_capability_invokes_grant_camera() {
        use crate::shell::state::{Capability, GRANT_SESSION};
        let mut p =
            SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        let initial_count = p.binds_rw.len();
        let grants = vec![Capability::Camera {
            scope: GRANT_SESSION,
        }];
        p.apply_grants(&grants);
        assert!(p.binds_rw.len() >= initial_count);
    }

    #[test]
    #[serial(scan_memo)]
    fn apply_grants_with_no_camera_or_mic_is_noop_for_those_cases() {
        use crate::shell::state::{Capability, GRANT_ALWAYS};
        let mut p =
            SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        let before_rw = p.binds_rw.len();
        let before_ro = p.binds_ro.len();
        // Screencast remains the only out-of-scope variant for this
        // module — its enforcement lives behind a portal follow-up.
        let grants = vec![Capability::Screencast {
            scope: GRANT_ALWAYS,
        }];
        p.apply_grants(&grants);
        assert_eq!(p.binds_rw.len(), before_rw);
        assert_eq!(p.binds_ro.len(), before_ro);
    }

    #[test]
    fn grant_filesystem_path_read_appends_to_binds_ro() {
        use crate::shell::state::FsMode;
        let mut p =
            SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        let before_readonly = p.binds_ro.len();
        let before_readwrite = p.binds_rw.len();
        p.grant_filesystem_path(Path::new("/home/u/notes"), &FsMode::Read);
        assert_eq!(p.binds_ro.len(), before_readonly + 1);
        assert_eq!(p.binds_rw.len(), before_readwrite);
    }

    #[test]
    fn grant_filesystem_path_readwrite_appends_to_binds_rw() {
        use crate::shell::state::FsMode;
        let mut p =
            SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        p.grant_filesystem_path(Path::new("/home/u/work"), &FsMode::ReadWrite);
        assert!(
            p.binds_rw
                .iter()
                .any(|(src, _)| src == Path::new("/home/u/work"))
        );
    }

    #[test]
    fn grant_filesystem_path_is_idempotent() {
        use crate::shell::state::FsMode;
        let mut p =
            SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        p.grant_filesystem_path(Path::new("/home/u/notes"), &FsMode::Read);
        let count_after_first = p.binds_ro.len();
        p.grant_filesystem_path(Path::new("/home/u/notes"), &FsMode::Read);
        assert_eq!(
            p.binds_ro.len(),
            count_after_first,
            "second grant of same path is a no-op"
        );
    }

    #[test]
    #[serial(scan_memo)]
    fn apply_grants_with_filesystem_always_scope_invokes_grant_filesystem() {
        use crate::shell::state::{Capability, FsMode, GRANT_ALWAYS};
        let mut p =
            SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        let grants = vec![Capability::Filesystem {
            path: "/home/u/work".into(),
            mode: FsMode::ReadWrite,
            scope: GRANT_ALWAYS,
        }];
        p.apply_grants(&grants);
        assert!(
            p.binds_rw
                .iter()
                .any(|(src, _)| src == Path::new("/home/u/work"))
        );
    }

    #[test]
    #[serial(scan_memo)]
    fn apply_grants_with_filesystem_session_scope_binds_for_the_session() {
        use crate::shell::state::{Capability, Expiry, FsMode};
        let mut p =
            SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        // Session-scoped (the dropped-file grant) must bind — otherwise
        // the grant is recorded but the app can never read the file.
        let granted = std::path::Path::new("/home/u/dropped");
        let grants = vec![Capability::Filesystem {
            path: granted.to_path_buf(),
            mode: FsMode::ReadWrite,
            scope: Expiry::EndOfSession,
        }];
        p.apply_grants(&grants);
        assert!(
            p.binds_rw
                .iter()
                .any(|(src, dst)| src == granted && dst == granted),
            "Session-scoped ReadWrite grant must bind the path into the \
             sandbox for the session; binds_rw = {:?}",
            p.binds_rw
        );
    }

    #[test]
    #[serial(scan_memo)]
    fn apply_grants_with_filesystem_session_scope_read_binds_read_only() {
        use crate::shell::state::{Capability, Expiry, FsMode};
        let mut p =
            SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        // A dropped file records a `Read` + `Session` grant. It must land
        // in the *read-only* bind set — least privilege — not read-write.
        let granted = std::path::Path::new("/home/u/notes.ts");
        let before_rw = p.binds_rw.len();
        let grants = vec![Capability::Filesystem {
            path: granted.to_path_buf(),
            mode: FsMode::Read,
            scope: Expiry::EndOfSession,
        }];
        p.apply_grants(&grants);
        assert!(
            p.binds_ro.iter().any(|(src, _)| src == granted),
            "Session-scoped Read grant must appear in binds_ro; got {:?}",
            p.binds_ro
        );
        assert_eq!(
            p.binds_rw.len(),
            before_rw,
            "a Read grant must not add a read-write bind: {:?}",
            p.binds_rw
        );
    }

    #[test]
    #[serial(scan_memo)]
    fn apply_grants_with_filesystem_once_scope_does_not_bind() {
        use crate::shell::state::{Capability, Expiry, FsMode};
        let mut p =
            SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        // `Once` is one-shot: it must NOT bind into the sandbox (which
        // outlives the single request). It's delivered as a portal fd
        // instead. Binding it would over-persist the access.
        let before_rw = p.binds_rw.len();
        let before_ro = p.binds_ro.len();
        let grants = vec![Capability::Filesystem {
            path: "/home/u/secret".into(),
            mode: FsMode::ReadWrite,
            scope: Expiry::AfterUse,
        }];
        p.apply_grants(&grants);
        assert_eq!(
            p.binds_rw.len(),
            before_rw,
            "Once-scoped grants must not bind-mount; the runtime portal \
             hands the app a single fd instead"
        );
        assert_eq!(p.binds_ro.len(), before_ro);
    }

    #[test]
    #[serial(scan_memo)]
    fn apply_grants_with_network_always_scope_disables_isolation() {
        use crate::shell::state::{Capability, GRANT_ALWAYS};
        let mut p =
            SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        p.net_isolated = true;
        let grants = vec![Capability::Network {
            hosts: None,
            scope: GRANT_ALWAYS,
        }];
        p.apply_grants(&grants);
        assert!(
            !p.net_isolated,
            "Always-network grant disables namespace isolation in v1"
        );
    }

    #[test]
    #[serial(scan_memo)]
    fn apply_grants_with_network_session_scope_does_not_change_isolation() {
        use crate::shell::state::{Capability, GRANT_SESSION};
        let mut p =
            SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        p.net_isolated = true;
        let grants = vec![Capability::Network {
            hosts: None,
            scope: GRANT_SESSION,
        }];
        p.apply_grants(&grants);
        assert!(
            p.net_isolated,
            "Session-scope grants don't change permanent isolation"
        );
    }

    /// A network allowlist reaches no enforcement mechanism: the *only*
    /// thing a host allowlist does is toggle network-namespace isolation.
    /// The hostname the user named appears nowhere in the rendered
    /// `bwrap` argv — bwrap has no vocabulary for it and there is no
    /// packet filter behind it. Per-host filtering would need
    /// per-namespace nftables rules, which this repository does not have.
    #[test]
    #[serial(scan_memo)]
    fn network_allowlist_only_toggles_namespace_isolation_and_reaches_no_host_filter() {
        use crate::shell::state::{Capability, GRANT_ALWAYS};
        let mut p =
            SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        p.net_isolated = true;
        let grants = vec![Capability::Network {
            hosts: Some(vec!["github.com".to_string()]),
            scope: GRANT_ALWAYS,
        }];
        p.apply_grants(&grants);
        assert!(
            !p.net_isolated,
            "an allowlisted-host grant lifts namespace isolation — that is the whole of its effect"
        );

        // The rendered argv is the sandbox: nothing downstream of it can
        // restrict the app to `github.com`.
        let argv = p.into_argv(&["true".into()]);
        assert!(
            !argv.iter().any(|a| a.contains("github.com")),
            "the recorded allowlist reaches no enforcement mechanism — it is absent from the \
             argv that builds the sandbox: {argv:?}"
        );
        assert!(
            !argv.iter().any(|a| a == "--unshare-net"),
            "isolation was lifted, so the app has unrestricted outbound network access, not \
             access to github.com: {argv:?}"
        );
    }

    /// Smoke test for the spawn helper. Only runs when `bwrap` is
    /// installed and the kernel allows unprivileged user namespaces;
    /// otherwise it skips cleanly so CI on lean images isn't gated on
    /// system configuration.
    ///
    /// This extends the default profile with the minimum viable mounts
    /// to actually execute a binary (`/bin`, `/lib`, `/lib64`, `/proc`,
    /// `/dev`). The default profile is intentionally minimal — those
    /// mounts come from the per-workspace launcher — so the integration
    /// test takes responsibility for them itself.
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
        profile
            .binds_rw
            .retain(|(host, _)| host != &profile.wayland_socket);
        // Add the host paths needed for `cat` to actually link + run.
        for ro in ["/bin", "/lib", "/lib64"] {
            let p = PathBuf::from(ro);
            if p.exists() {
                profile.binds_ro.push((p.clone(), p));
            }
        }
        // bwrap needs `--proc /proc` and `--dev /dev` to populate those
        // pseudo-filesystems; render those manually as an argv suffix
        // since `SandboxProfile` doesn't model them.
        let mut argv = clone_for_test(&profile).into_argv(&[]);
        argv.extend(
            [
                "--proc",
                "/proc",
                "--dev",
                "/dev",
                "/bin/cat",
                "/work/notes.txt",
            ]
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
            seccomp_fd: p.seccomp_fd,
            wayland_socket: p.wayland_socket.clone(),
            dbus_socket: p.dbus_socket.clone(),
            net_isolated: p.net_isolated,
            landlock_shim: p.landlock_shim.clone(),
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
        let base =
            std::env::temp_dir().join(format!("quick-shell-sandbox-test-{}", std::process::id()));
        std::fs::create_dir_all(&base).expect("create tempdir");
        base
    }

    /// When a caller sets `profile.seccomp = Some(...)` but not
    /// `profile.seccomp_fd`, the rendered argv must omit `--seccomp`
    /// entirely rather than emitting a stale hardcoded fd that points at
    /// nothing.
    ///
    /// The spawn helper is responsible for opening the bpf file, duping
    /// it to a known descriptor in the child, and stamping `seccomp_fd`
    /// with that number. A profile without `seccomp_fd` means the caller
    /// skipped that plumbing, so the flag is suppressed rather than
    /// telling bwrap to read from a fd that doesn't exist.
    #[test]
    fn seccomp_some_does_not_emit_argv_without_fd_plumbing() {
        let mut p =
            SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        p.seccomp = Some(PathBuf::from("/some/cached/blob.bpf"));
        // Note: seccomp_fd left as None — simulates a caller that
        // didn't go through the spawn helper.
        let argv = p.into_argv(&["true".into()]);
        assert!(
            !argv.iter().any(|a| a == "--seccomp"),
            "argv must not carry --seccomp without matching fd \
             plumbing (C-5 regression): {argv:?}"
        );
    }

    /// `apply_grants` with a camera capability memoises the `/dev` scan:
    /// the first call hits the filesystem, the second call within the 5s
    /// TTL reuses the cached entry list and does NOT re-walk `/dev`.
    ///
    /// Counted via the test-only `SCAN_WALK_COUNT` counter the slow path
    /// increments — this guards against the cache being silently
    /// bypassed (e.g. a future refactor reaching into `std::fs::read_dir`
    /// directly).
    #[test]
    #[serial(scan_memo)]
    fn apply_grants_dev_scan_is_memoised() {
        use crate::shell::state::{Capability, GRANT_SESSION};
        // Reset both the cache and the counter so other tests can't
        // contaminate our measurement.
        super::clear_scan_memo();
        let _ = super::take_scan_walk_count();

        let grants = vec![Capability::Camera {
            scope: GRANT_SESSION,
        }];

        let mut p1 =
            SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        p1.apply_grants(&grants);
        let walks_after_first = super::take_scan_walk_count();
        assert_eq!(
            walks_after_first, 1,
            "first apply_grants should hit the filesystem once for /dev; got {walks_after_first}"
        );

        let mut p2 =
            SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        p2.apply_grants(&grants);
        let walks_after_second = super::take_scan_walk_count();
        assert_eq!(
            walks_after_second, 0,
            "second apply_grants within the 5s TTL should reuse the \
             memoised scan; got {walks_after_second} additional walks"
        );
    }

    /// Landlock (`plan/security.md` §7.1): when `landlock_shim` is set,
    /// `into_argv` must make bwrap exec the shim — `<shim> __sandbox-exec
    /// --ro <p>… --rw <p>… [--net-restricted] -- <app argv>` — rather than
    /// the app directly. The `--ro`/`--rw` paths must be the *sandbox-side*
    /// (destination) bind paths, and the real command must follow `--`.
    #[test]
    fn landlock_shim_wraps_command_with_ro_rw_and_separator() {
        let mut p =
            SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        p.landlock_shim = Some(PathBuf::from("/usr/bin/shell"));
        let argv = clone_for_test(&p).into_argv(&["/usr/bin/terminal".into(), "--foo".into()]);

        // The shim binary + hidden subcommand appear, in order.
        let shim_pos = argv
            .iter()
            .position(|a| a == "/usr/bin/shell")
            .expect("shim path present in argv");
        assert_eq!(
            argv.get(shim_pos + 1).map(String::as_str),
            Some(super::super::SANDBOX_EXEC_SUBCOMMAND),
            "shim path must be immediately followed by the __sandbox-exec token: {argv:?}"
        );

        // Exactly one `--` separator, and the real command follows it.
        let sep_pos = argv
            .iter()
            .position(|a| a == "--")
            .expect("`--` separator present");
        assert_eq!(
            argv.iter().filter(|a| a.as_str() == "--").count(),
            1,
            "exactly one `--` separator expected: {argv:?}"
        );
        assert_eq!(
            &argv[sep_pos + 1..],
            &["/usr/bin/terminal".to_string(), "--foo".to_string()],
            "the real app argv must follow `--` verbatim: {argv:?}"
        );

        // `--ro /usr` and `--ro /run/user/1000/wayland-0` are sandbox-side
        // (destination) bind paths from the default profile.
        let readonly_paths: Vec<&String> = argv
            .windows(2)
            .filter(|w| w[0] == "--ro")
            .map(|w| &w[1])
            .collect();
        assert!(
            readonly_paths.iter().any(|p| p.as_str() == "/usr"),
            "expected `--ro /usr` (sandbox-side ro bind): {argv:?}"
        );

        // `--rw /work` is the sandbox-side destination of the ws-root bind
        // (NOT the host `/tmp/ws` source) — Landlock runs inside the
        // namespace where `/work` is what exists.
        let readwrite_paths: Vec<&String> = argv
            .windows(2)
            .filter(|w| w[0] == "--rw")
            .map(|w| &w[1])
            .collect();
        assert!(
            readwrite_paths.iter().any(|p| p.as_str() == "/work"),
            "expected `--rw /work` (sandbox-side rw bind): {argv:?}"
        );
        assert!(
            !readwrite_paths.iter().any(|p| p.as_str() == "/tmp/ws"),
            "Landlock rules must use sandbox-side paths, not host sources: {argv:?}"
        );

        // The shim/`--ro`/`--rw` wrapping sits AFTER the bwrap flags: the
        // shim path must come after the last `--bind`/`--ro-bind`/`--setenv`.
        let last_bwrap_flag = argv
            .iter()
            .rposition(|a| a == "--bind" || a == "--ro-bind" || a == "--setenv")
            .expect("bwrap flags present");
        assert!(
            shim_pos > last_bwrap_flag,
            "shim must be the bwrap command tail, after all bwrap flags: {argv:?}"
        );
    }

    /// Landlock net policy: `--net-restricted` is emitted iff the profile
    /// is `net_isolated` (it mirrors `--unshare-net`). A network-granted
    /// workspace (`net_isolated = false`) must NOT carry it.
    #[test]
    fn landlock_shim_net_restricted_tracks_net_isolated() {
        let mut isolated =
            SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        isolated.landlock_shim = Some(PathBuf::from("/usr/bin/shell"));
        isolated.net_isolated = true;
        let argv = clone_for_test(&isolated).into_argv(&["/bin/true".into()]);
        assert!(
            argv.iter().any(|a| a == "--net-restricted"),
            "net_isolated profile must pass --net-restricted: {argv:?}"
        );

        let mut open = clone_for_test(&isolated);
        open.net_isolated = false;
        let argv = open.into_argv(&["/bin/true".into()]);
        assert!(
            !argv.iter().any(|a| a == "--net-restricted"),
            "network-granted profile must NOT pass --net-restricted: {argv:?}"
        );
    }

    /// Without `landlock_shim` set (the default for hand-built profiles),
    /// `into_argv` appends the command verbatim and emits no shim wrapping
    /// — the backward-compatible path the other argv tests rely on.
    #[test]
    fn no_landlock_shim_appends_command_verbatim() {
        let p = SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        assert!(p.landlock_shim.is_none(), "default profile has no shim");
        let argv = p.into_argv(&["/bin/echo".into(), "hi".into()]);
        assert!(
            !argv
                .iter()
                .any(|a| a == super::super::SANDBOX_EXEC_SUBCOMMAND),
            "no shim token without landlock_shim: {argv:?}"
        );
        assert!(
            !argv
                .iter()
                .any(|a| a == "--ro" || a == "--rw" || a == "--net-restricted"),
            "no shim flags without landlock_shim: {argv:?}"
        );
        assert_eq!(argv.last().unwrap(), "hi");
    }

    /// When both `seccomp` AND `seccomp_fd` are set — the code path the
    /// spawn helper drives — the argv MUST carry the matching
    /// `--seccomp <fd>` triple using the actually-plumbed descriptor
    /// number, not a hardcoded one.
    #[test]
    fn seccomp_argv_uses_actually_loaded_fd() {
        let mut p =
            SandboxProfile::for_workspace_test(Path::new("/tmp/ws"), Path::new("/tmp/cache"));
        p.seccomp = Some(PathBuf::from("/some/cached/blob.bpf"));
        // Simulate the spawn helper having opened the bpf file and
        // dup'd it to fd 16 in the child.
        p.seccomp_fd = Some(16);
        let argv = p.into_argv(&["true".into()]);
        // The flag must appear and reference fd 16.
        let pos = argv
            .iter()
            .position(|a| a == "--seccomp")
            .expect("expected --seccomp triple in argv");
        assert_eq!(
            argv.get(pos + 1).map(std::string::String::as_str),
            Some("16"),
            "argv carries --seccomp at index {pos} but the fd value is \
             {:?} (must match seccomp_fd=Some(16), not the legacy 10): \
             {argv:?}",
            argv.get(pos + 1)
        );
        // And critically — there must be NO literal `10` immediately
        // after a `--seccomp`. If a refactor accidentally re-introduces
        // the hardcoded constant, this catches it.
        assert!(
            !argv.windows(2).any(|w| w[0] == "--seccomp" && w[1] == "10"),
            "argv accidentally re-introduced hardcoded `--seccomp 10` \
             (C-5 regression): {argv:?}"
        );
    }
}
