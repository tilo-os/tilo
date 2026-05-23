//! `SandboxProfile`: a serialisable description of a per-workspace
//! Bubblewrap sandbox, rendered into the argv expected by the `bwrap` CLI.
//!
//! The profile intentionally stays a pure value type — no I/O, no fd
//! plumbing — so we can build it from the workspace CRDT in one place
//! and feed the rendered argv to either `std::process::Command` (Phase 1
//! spawn helper) or future fd-passing launchers (Phase 2+).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// D-6 fix: per-directory memo of the entries we care about during
/// capability-grant application (`/dev/video*` for camera, the
/// pipewire socket + `/dev/snd` for microphone). `apply_grants` is
/// called once per process spawn into a workspace; without memoization
/// every cluster launch re-walks the same `/dev` tree (and re-stats
/// `pipewire-0` / `/dev/snd`). The TTL is conservative: 5s is short
/// enough that hotplugged hardware shows up within a couple of
/// subsequent launches, long enough to coalesce the rapid-fire
/// spawns that happen when a user clicks a launcher 3-4 times.
///
/// Cache shape: `directory path -> (filtered entries, captured-at)`.
/// The filtering function is opaque to the cache; consumers pass a
/// closure-free key so different filters of the same directory each
/// get their own cache slot.
static SCAN_MEMO: LazyLock<Mutex<HashMap<(PathBuf, &'static str), (Vec<PathBuf>, Instant)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// D-6: TTL for cached `/dev` and `XDG_RUNTIME_DIR` scans.
const SCAN_MEMO_TTL: Duration = Duration::from_secs(5);

/// D-6 test hook: counts the number of times the underlying directory
/// walk runs through the slow path (filesystem hit). The cached
/// happy-path increments nothing. Tests assert `take_count_since` to
/// confirm memoization. Production never reads this.
#[cfg(test)]
static SCAN_WALK_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
pub(crate) fn take_scan_walk_count() -> usize {
    SCAN_WALK_COUNT.swap(0, std::sync::atomic::Ordering::SeqCst)
}

#[cfg(test)]
pub(crate) fn clear_scan_memo() {
    SCAN_MEMO.lock().unwrap().clear();
}

/// D-6: memoised directory walk. `filter_tag` is a stable string id for
/// the filter being applied (e.g. `"video"` for `/dev/video*`) — it
/// keys the cache so two different filters of the same directory get
/// independent cache slots.
fn scan_dir_memoised(dir: &Path, filter_tag: &'static str, filter: fn(&str) -> bool) -> Vec<PathBuf> {
    let key = (dir.to_path_buf(), filter_tag);
    {
        let cache = SCAN_MEMO.lock().expect("SCAN_MEMO poisoned");
        if let Some((entries, captured)) = cache.get(&key) {
            if captured.elapsed() < SCAN_MEMO_TTL {
                return entries.clone();
            }
        }
    }
    // Slow path: actually walk the directory.
    #[cfg(test)]
    SCAN_WALK_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if filter(name) {
                    out.push(path);
                }
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
    /// Optional path to a compiled seccomp-bpf blob.
    ///
    /// C-5 fix: the spawn helper in [`crate::sandbox::spawn`] now
    /// pre-opens this file before forking, ensures the resulting fd
    /// stays open across `execve` (via `pre_exec`'s `dup2` into a
    /// reserved slot), and renders the matching `--seccomp <fd>` triple
    /// into argv with the *actual* fd number — not the previous
    /// hardcoded `10` which left bwrap reading whatever stale fd
    /// happened to be at slot 10. See [`SandboxProfile::into_argv`]
    /// and [`crate::sandbox::spawn::spawn_in_workspace_with_grants`].
    pub seccomp: Option<PathBuf>,
    /// Fd number to render in the `--seccomp <fd>` argv triple. The
    /// spawn helper plumbs the open seccomp.bpf file to this descriptor
    /// in the child via `dup2`. `None` when [`Self::seccomp`] is also
    /// `None`; when both are set the argv carries the matching number.
    ///
    /// This field is set by [`crate::sandbox::spawn`] just before
    /// rendering argv. Callers that build a profile by hand and skip
    /// the spawn helper (only the unit tests) should leave it `None`
    /// — the argv rendering then suppresses the `--seccomp` flag
    /// rather than emitting a stale magic number.
    pub seccomp_fd: Option<i32>,
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
            seccomp_fd: None,
            wayland_socket,
            dbus_socket: None,
            net_isolated: true,
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
        let video_devices = scan_dir_memoised(
            Path::new("/dev"),
            "video",
            |name| name.starts_with("video"),
        );
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
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .unwrap_or_else(|_| format!("/run/user/{}", self.uid));
        let pipewire_path =
            std::path::PathBuf::from(format!("{}/pipewire-0", runtime_dir));
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
    /// `Microphone` (Phase 5 Task 5.3), `Always`-scoped `Filesystem`
    /// grants (Phase 5 Task 5.1), and `Always`-scoped `Network` grants
    /// (Phase 5 Task 5.2). `Once` / `Session` filesystem grants are
    /// routed through the portal at runtime (it returns an open fd via
    /// cap-std, no bind-mount needed). `Screencast` is a no-op here and
    /// lands in the portal screencast follow-up.
    ///
    /// The plan says the policy is "without a grant, the device socket
    /// simply isn't bound" — so the contract is additive: this method
    /// only ever appends binds, never removes them. Callers that want a
    /// strictly minimal profile should start from
    /// [`for_workspace_test`] and pass an empty grants slice.
    pub fn apply_grants(&mut self, grants: &[crate::shell::state::Capability]) {
        use crate::shell::state::{Capability, GrantScope};
        for cap in grants {
            match cap {
                Capability::Camera { .. } => self.grant_camera(),
                Capability::Microphone { .. } => self.grant_microphone(),
                Capability::Filesystem { path, mode, scope } => {
                    // Only `Always`-scoped grants persist as bind-mounts;
                    // `Once` and `Session` are handled by the runtime
                    // portal (it returns an open fd via cap-std, no
                    // bind-mount needed).
                    if matches!(scope, GrantScope::Always) {
                        self.grant_filesystem_path(path, mode.clone());
                    }
                }
                Capability::Network { hosts: _, scope } => {
                    // Any `Always`-scope network grant disables the
                    // default network namespace isolation (Phase 1's
                    // `--unshare-net` flag). For v1 we treat
                    // `hosts: Some(allowlist)` and `hosts: None` the
                    // same: both lift isolation. Per-host nftables
                    // filtering is recorded by the data model
                    // (`Capability::Network::hosts`) but enforcement
                    // requires per-namespace nftables rule generation,
                    // which is deferred to a Phase 5.2 follow-up unit.
                    if matches!(scope, GrantScope::Always) {
                        self.net_isolated = false;
                    }
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
        mode: crate::shell::state::FsMode,
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

        // C-5 fix: only emit `--seccomp <fd>` when the spawn helper has
        // actually plumbed the file to a known descriptor (`seccomp_fd`
        // is `Some`). The previous code hardcoded fd `10` whenever
        // `self.seccomp` was set, which left bwrap reading whatever
        // happened to be at fd 10 — typically nothing, so the
        // deny-list filter was silently bypassed.
        //
        // Profiles built outside the spawn helper (e.g. unit tests
        // that render argv for inspection) leave `seccomp_fd` `None`,
        // which now correctly drops the flag entirely. The matching
        // `pre_exec` plumbing lives in
        // [`crate::sandbox::spawn::spawn_in_workspace_with_grants`].
        if let Some(fd) = self.seccomp_fd {
            argv.push("--seccomp".to_string());
            argv.push(fd.to_string());
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
    // `apply_grants` reads/writes the process-wide `SCAN_MEMO` static
    // (D-6's per-directory memoisation). Tests that exercise that path
    // are tagged `#[serial(scan_memo)]` so parallel `cargo test` runs
    // don't corrupt the shared walk counter the memo test asserts on.
    use serial_test::serial;

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

    #[test]
    fn grant_camera_appends_video_binds_when_devices_exist() {
        let mut p = SandboxProfile::for_workspace_test(
            Path::new("/tmp/ws"),
            Path::new("/tmp/cache"),
        );
        let initial_count = p.binds_rw.len();
        p.grant_camera();
        // /dev/video* may or may not exist on CI; assert "no shrink".
        assert!(p.binds_rw.len() >= initial_count);
        // If /dev/video0 exists on this host, expect it in binds_rw.
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
        let mut p = SandboxProfile::for_workspace_test(
            Path::new("/tmp/ws"),
            Path::new("/tmp/cache"),
        );
        p.grant_microphone();
        // Pipewire/ALSA may be absent in CI; the contract is "no panic".
    }

    #[test]
    #[serial(scan_memo)]
    fn apply_grants_with_camera_capability_invokes_grant_camera() {
        use crate::shell::state::{Capability, GrantScope};
        let mut p = SandboxProfile::for_workspace_test(
            Path::new("/tmp/ws"),
            Path::new("/tmp/cache"),
        );
        let initial_count = p.binds_rw.len();
        let grants = vec![Capability::Camera {
            scope: GrantScope::Session,
        }];
        p.apply_grants(&grants);
        assert!(p.binds_rw.len() >= initial_count);
    }

    #[test]
    #[serial(scan_memo)]
    fn apply_grants_with_no_camera_or_mic_is_noop_for_those_cases() {
        use crate::shell::state::{Capability, GrantScope};
        let mut p = SandboxProfile::for_workspace_test(
            Path::new("/tmp/ws"),
            Path::new("/tmp/cache"),
        );
        let before_rw = p.binds_rw.len();
        let before_ro = p.binds_ro.len();
        // Screencast remains the only out-of-scope variant for this
        // module — its enforcement lives behind a portal follow-up.
        let grants = vec![Capability::Screencast {
            scope: GrantScope::Always,
        }];
        p.apply_grants(&grants);
        assert_eq!(p.binds_rw.len(), before_rw);
        assert_eq!(p.binds_ro.len(), before_ro);
    }

    #[test]
    fn grant_filesystem_path_read_appends_to_binds_ro() {
        use crate::shell::state::FsMode;
        let mut p = SandboxProfile::for_workspace_test(
            Path::new("/tmp/ws"),
            Path::new("/tmp/cache"),
        );
        let before_ro = p.binds_ro.len();
        let before_rw = p.binds_rw.len();
        p.grant_filesystem_path(Path::new("/home/u/notes"), FsMode::Read);
        assert_eq!(p.binds_ro.len(), before_ro + 1);
        assert_eq!(p.binds_rw.len(), before_rw);
    }

    #[test]
    fn grant_filesystem_path_readwrite_appends_to_binds_rw() {
        use crate::shell::state::FsMode;
        let mut p = SandboxProfile::for_workspace_test(
            Path::new("/tmp/ws"),
            Path::new("/tmp/cache"),
        );
        p.grant_filesystem_path(Path::new("/home/u/work"), FsMode::ReadWrite);
        assert!(
            p.binds_rw
                .iter()
                .any(|(src, _)| src == Path::new("/home/u/work"))
        );
    }

    #[test]
    fn grant_filesystem_path_is_idempotent() {
        use crate::shell::state::FsMode;
        let mut p = SandboxProfile::for_workspace_test(
            Path::new("/tmp/ws"),
            Path::new("/tmp/cache"),
        );
        p.grant_filesystem_path(Path::new("/home/u/notes"), FsMode::Read);
        let count_after_first = p.binds_ro.len();
        p.grant_filesystem_path(Path::new("/home/u/notes"), FsMode::Read);
        assert_eq!(
            p.binds_ro.len(),
            count_after_first,
            "second grant of same path is a no-op"
        );
    }

    #[test]
    #[serial(scan_memo)]
    fn apply_grants_with_filesystem_always_scope_invokes_grant_filesystem() {
        use crate::shell::state::{Capability, FsMode, GrantScope};
        let mut p = SandboxProfile::for_workspace_test(
            Path::new("/tmp/ws"),
            Path::new("/tmp/cache"),
        );
        let grants = vec![Capability::Filesystem {
            path: "/home/u/work".into(),
            mode: FsMode::ReadWrite,
            scope: GrantScope::Always,
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
    fn apply_grants_with_filesystem_session_scope_does_not_bind() {
        use crate::shell::state::{Capability, FsMode, GrantScope};
        let mut p = SandboxProfile::for_workspace_test(
            Path::new("/tmp/ws"),
            Path::new("/tmp/cache"),
        );
        let before_rw = p.binds_rw.len();
        let grants = vec![Capability::Filesystem {
            path: "/home/u/secret".into(),
            mode: FsMode::ReadWrite,
            scope: GrantScope::Session,
        }];
        p.apply_grants(&grants);
        assert_eq!(
            p.binds_rw.len(),
            before_rw,
            "Session-scoped grants don't bind-mount; runtime portal handles them"
        );
    }

    #[test]
    #[serial(scan_memo)]
    fn apply_grants_with_network_always_scope_disables_isolation() {
        use crate::shell::state::{Capability, GrantScope};
        let mut p = SandboxProfile::for_workspace_test(
            Path::new("/tmp/ws"),
            Path::new("/tmp/cache"),
        );
        p.net_isolated = true;
        let grants = vec![Capability::Network {
            hosts: None,
            scope: GrantScope::Always,
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
        use crate::shell::state::{Capability, GrantScope};
        let mut p = SandboxProfile::for_workspace_test(
            Path::new("/tmp/ws"),
            Path::new("/tmp/cache"),
        );
        p.net_isolated = true;
        let grants = vec![Capability::Network {
            hosts: None,
            scope: GrantScope::Session,
        }];
        p.apply_grants(&grants);
        assert!(
            p.net_isolated,
            "Session-scope grants don't change permanent isolation"
        );
    }

    #[test]
    #[serial(scan_memo)]
    fn apply_grants_with_network_always_and_specific_hosts_still_disables_isolation_for_v1() {
        use crate::shell::state::{Capability, GrantScope};
        let mut p = SandboxProfile::for_workspace_test(
            Path::new("/tmp/ws"),
            Path::new("/tmp/cache"),
        );
        p.net_isolated = true;
        // v1 treats `Some([...])` and `None` the same — both disable
        // isolation. Per-host nftables filtering is a follow-up.
        let grants = vec![Capability::Network {
            hosts: Some(vec!["github.com".to_string()]),
            scope: GrantScope::Always,
        }];
        p.apply_grants(&grants);
        assert!(
            !p.net_isolated,
            "v1: any Always-network grant disables isolation"
        );
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
            seccomp_fd: p.seccomp_fd,
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

    /// C-5 regression: when a caller sets `profile.seccomp = Some(...)`
    /// but does NOT also set `profile.seccomp_fd`, the rendered argv
    /// must omit `--seccomp` entirely rather than emitting a stale
    /// hardcoded `--seccomp 10` that points at no real fd.
    ///
    /// The spawn helper is responsible for opening the bpf file,
    /// duping it to a known descriptor in the child, and stamping
    /// `seccomp_fd` with that number. A profile without `seccomp_fd`
    /// indicates the caller skipped that plumbing — in which case
    /// rendering the flag would tell bwrap to read from a fd that
    /// doesn't exist (the previous bug). Suppress instead.
    #[test]
    fn seccomp_some_does_not_emit_argv_without_fd_plumbing() {
        let mut p = SandboxProfile::for_workspace_test(
            Path::new("/tmp/ws"),
            Path::new("/tmp/cache"),
        );
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

    /// D-6 contract: `apply_grants` with a camera capability memoises
    /// the `/dev` scan. The first call hits the filesystem; the second
    /// call within the 5s TTL reuses the cached entry list and does
    /// NOT re-walk `/dev`.
    ///
    /// We count walks via the test-only `SCAN_WALK_COUNT` counter
    /// that the slow path increments. Before D-6's fix the counter
    /// would tick on every `apply_grants` call; the assertion guards
    /// against the cache being silently bypassed (e.g. a future
    /// refactor reaches into `std::fs::read_dir` directly).
    #[test]
    #[serial(scan_memo)]
    fn apply_grants_dev_scan_is_memoised() {
        use crate::shell::state::{Capability, GrantScope};
        // Reset both the cache and the counter so other tests can't
        // contaminate our measurement.
        super::clear_scan_memo();
        let _ = super::take_scan_walk_count();

        let grants = vec![Capability::Camera {
            scope: GrantScope::Session,
        }];

        let mut p1 = SandboxProfile::for_workspace_test(
            Path::new("/tmp/ws"),
            Path::new("/tmp/cache"),
        );
        p1.apply_grants(&grants);
        let walks_after_first = super::take_scan_walk_count();
        assert_eq!(
            walks_after_first, 1,
            "first apply_grants should hit the filesystem once for /dev; got {walks_after_first}"
        );

        let mut p2 = SandboxProfile::for_workspace_test(
            Path::new("/tmp/ws"),
            Path::new("/tmp/cache"),
        );
        p2.apply_grants(&grants);
        let walks_after_second = super::take_scan_walk_count();
        assert_eq!(
            walks_after_second, 0,
            "second apply_grants within the 5s TTL should reuse the \
             memoised scan; got {walks_after_second} additional walks"
        );
    }

    /// C-5 regression: when both `seccomp` AND `seccomp_fd` are set —
    /// the post-fix code path the spawn helper drives — the argv
    /// MUST carry the matching `--seccomp <fd>` triple using the
    /// actually-plumbed descriptor number, NOT a hardcoded `10`.
    #[test]
    fn seccomp_argv_uses_actually_loaded_fd() {
        let mut p = SandboxProfile::for_workspace_test(
            Path::new("/tmp/ws"),
            Path::new("/tmp/cache"),
        );
        p.seccomp = Some(PathBuf::from("/some/cached/blob.bpf"));
        // Simulate the spawn helper having opened the bpf file and
        // dup'd it to fd 16 in the child.
        p.seccomp_fd = Some(16);
        let argv = p.into_argv(&["true".into()]);
        // The flag must appear and reference fd 16, not the legacy
        // hardcoded `10`.
        let pos = argv
            .iter()
            .position(|a| a == "--seccomp")
            .expect("expected --seccomp triple in argv");
        assert_eq!(
            argv.get(pos + 1).map(|s| s.as_str()),
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
            !argv
                .windows(2)
                .any(|w| w[0] == "--seccomp" && w[1] == "10"),
            "argv accidentally re-introduced hardcoded `--seccomp 10` \
             (C-5 regression): {argv:?}"
        );
    }
}
