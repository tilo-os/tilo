//! Building the guest: the root filesystem, the namespaces, and the argv
//! that makes them. Everything here is a pure function of a [`GuestPlan`]
//! until [`spawn`] is called, so confinement can be asserted on without a
//! kernel.
//!
//! `bwrap` performs the whole unshare/mount/`pivot_root`/seccomp/`exec`
//! sequence itself, with the filter landing after the last privileged
//! operation and before the guest exists — the supervisor never calls
//! `mount(2)`, which is why its own seccomp filter can deny that syscall
//! outright.
//!
//! ```text
//! /usr    overlay: lower = [ephemeral layer,] persistent layer, host /usr
//!                  upper = the session's writable layer
//! /etc    overlay: same stack
//! /bin /sbin /lib /lib64   symlinks into /usr, as on any usrmerge system
//! /var/lib /var/cache /var/log   the layer's own, bound
//! /var/tmp /tmp /run /proc /dev  fresh
//! /run/ush/bin            the runtime and its command shims, read-only
//! $HOME                   terminal: the caller's home; `ush`: the store copy, over the real path
//! <trusted dirs>          bound read-write, one per `perm trust-dir`
//! ```
//!
//! A path the guest was not granted is not missing a permission — it is
//! not in the namespace.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::layer::BASE_SUBTREES;
use crate::profile::Profile;

/// Directories the layer owns outright rather than shadowing: `/var` can't
/// be an overlay destination (see [`crate::layer::BASE_SUBTREES`]), and the
/// host's `/var` is machine state a shell has no business reading.
const LAYER_VAR_DIRS: &[&str] = &["var/lib", "var/cache", "var/log"];

/// `PATH` inside a guest. The runtime's own commands come first so `pkg` means
/// `pkg` even if a layer installs something by that name.
pub const GUEST_PATH: &str = "/run/ush/bin:/usr/local/bin:/usr/bin:/usr/local/sbin:/usr/sbin";

/// The only variables a guest inherits from the supervisor's environment;
/// the rest is built fresh in [`build_argv`]. These describe the person,
/// not the session's wiring, so dropping them costs the user nothing.
pub const INHERITED_ENV: &[&str] = &[
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_COLLATE",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LC_MONETARY",
    "LC_NUMERIC",
    "LC_TIME",
    "TZ",
    "COLORTERM",
];

/// Everything needed to build one guest.
#[derive(Debug, Clone)]
pub struct GuestPlan {
    pub profile: Profile,
    /// The host root being shadowed. `/` in production.
    pub base: PathBuf,
    /// The writable layer — the overlay `upperdir`.
    pub upper: PathBuf,
    /// Layers stacked between the upper and the base, nearest the upper first.
    /// Empty for a persistent session; `[persistent]` for an ephemeral one.
    pub lower_layers: Vec<PathBuf>,
    /// Where the overlay `workdir`s go; must be on the same filesystem as
    /// `upper`, hence the store's session directory rather than `/tmp`.
    pub workdir_root: PathBuf,
    pub guest_home: PathBuf,
    /// Where it is mounted — the real user's home path, so `~` and any
    /// absolute path a script has baked in still resolve.
    pub home_mountpoint: PathBuf,
    /// When this session is the user's terminal, the store (policy and audit)
    /// lives inside the home that was just bound. Covering it lets a typed
    /// `~/file` land in the real home without letting the guest edit its grants.
    pub masked_store: Option<PathBuf>,
    /// Directories the user trusted: bound read-write.
    pub trusted_dirs: Vec<PathBuf>,
    /// Directories granted read but not write.
    pub readable_dirs: Vec<PathBuf>,
    /// The runtime binary, bound in so the guest's `pkg`/`perm`/… resolve.
    pub runtime_bin: PathBuf,
    /// Installed legacy apps' commands: shims that ask the broker for a
    /// `LegacyLaunch`.
    pub legacy_commands: Vec<String>,
    pub broker_socket: PathBuf,
    /// The compiled seccomp blob for [`Self::profile`].
    pub seccomp_blob: PathBuf,
    /// What to run. Empty means the interactive shell.
    pub command: Vec<String>,
    /// The shell to host when `command` is empty.
    pub shell: PathBuf,
    pub term: String,
    /// Extra environment, applied last.
    pub env: Vec<(String, String)>,
}

impl GuestPlan {
    /// The overlay `workdir` for one base subtree.
    pub fn workdir_for(&self, subtree: &str) -> PathBuf {
        self.workdir_root.join("work").join(subtree)
    }

    /// Create the per-session directories the plan refers to: the `workdir`s
    /// must exist and be empty, since `bwrap` will not create them.
    ///
    /// # Errors
    ///
    /// Returns an error if a stale workdir can't be cleared, or a workdir
    /// or the guest home can't be created.
    pub fn prepare(&self) -> std::io::Result<()> {
        for subtree in BASE_SUBTREES {
            let work = self.workdir_for(subtree);
            // A workdir left behind by a crashed session holds overlayfs's half-finished
            // copy-ups; reusing it fails the next mount with a message that reads like a bug.
            remove_dir_all_forced(&work);
            std::fs::create_dir_all(&work)?;
        }
        std::fs::create_dir_all(&self.guest_home)?;
        Ok(())
    }
}

/// Build the `bwrap` argv for a plan. Pure: the vector starts with `bwrap`
/// and ends with the guest's own argv, so `-v` output pastes into a shell.
///
/// Each stage of the argv gets its own helper below, in the order `bwrap`
/// applies flags — that ordering is load-bearing (e.g. the resolver bind
/// must land after the `/run` tmpfs it would otherwise be covered by), so
/// keep the call order matching the section order in this function.
pub fn build_argv(
    plan: &GuestPlan,
    seccomp_fd: Option<std::os::fd::RawFd>,
    userns_fd: Option<std::os::fd::RawFd>,
) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    push_namespace_flags(&mut a, plan, userns_fd);
    push_overlay_mounts(&mut a, plan);
    push_symlinks_and_layer_dirs(&mut a, plan);
    push_pseudo_filesystems_and_resolver(&mut a, plan);
    push_runtime_and_broker(&mut a, plan);
    push_home_and_grants(&mut a, plan);
    push_capabilities(&mut a, plan);
    push_environment(&mut a, plan);
    push_seccomp_and_command(&mut a, plan, seccomp_fd);
    a
}

/// `--clearenv`, the namespace/identity flags, and the hostname — the part
/// of the argv that exists before any filesystem is touched.
fn push_namespace_flags(
    a: &mut Vec<OsString>,
    plan: &GuestPlan,
    userns_fd: Option<std::os::fd::RawFd>,
) {
    let push = |a: &mut Vec<OsString>, s: &str| a.push(OsString::from(s));

    push(a, "bwrap");

    // `bwrap` hands the guest the parent's whole environment unless told
    // otherwise; clearing it first is what makes the environment block below a
    // complete list rather than overrides on the supervisor's own environment.
    // Comes first because `--clearenv` keeps only what a later `--setenv` restores.
    push(a, "--clearenv");

    // A fresh user namespace makes the mounts below possible without privilege,
    // and uid 0 inside it is what lets `rpm` chown into the layer — outside the
    // namespace that uid confers nothing on the host. With a namespace prepared
    // externally (see [`prepare_userns`]) `bwrap` joins it instead of creating
    // one, which is how the guest gets more than a single mapped uid.
    match userns_fd {
        Some(fd) => {
            push(a, "--userns");
            a.push(fd.to_string().into());
        }
        None => push(a, "--unshare-user"),
    }
    push(a, "--uid");
    push(a, "0");
    push(a, "--gid");
    push(a, "0");
    push(a, "--unshare-pid");
    push(a, "--unshare-uts");
    push(a, "--unshare-ipc");
    push(a, "--unshare-cgroup-try");
    push(a, "--hostname");
    push(a, plan.profile.as_str());
    // The guest must not outlive the supervisor. Without this a crashed
    // supervisor leaves a shell running in a namespace nothing is watching.
    push(a, "--die-with-parent");
    // Deliberately *not* `--new-session`: it calls `setsid`, detaching the guest
    // from its controlling terminal and taking job control with it (no Ctrl-C,
    // no Ctrl-Z). `bwrap` documents it as incompatible with interactive use; what
    // it defends against — `TIOCSTI` injection — the kernel already refuses.
}

/// The `/usr` and `/etc` overlay stacks: layer(s) nearest the upper, base
/// last.
fn push_overlay_mounts(a: &mut Vec<OsString>, plan: &GuestPlan) {
    for subtree in BASE_SUBTREES {
        // `--overlay-src` entries accumulate for the next `--overlay`; layers are
        // pushed nearest-the-upper first, base last, so the base ends at the bottom.
        for lower in &plan.lower_layers {
            let dir = lower.join(subtree);
            if dir.is_dir() {
                a.push("--overlay-src".into());
                a.push(dir.into_os_string());
            }
        }
        a.push("--overlay-src".into());
        a.push(plan.base.join(subtree).into_os_string());

        a.push("--overlay".into());
        a.push(plan.upper.join(subtree).into_os_string());
        a.push(plan.workdir_for(subtree).into_os_string());
        a.push(PathBuf::from("/").join(subtree).into_os_string());
    }
}

/// The usrmerge/`/var` symlinks a real root carries, and the layer-owned
/// `/var` subdirectories (see [`LAYER_VAR_DIRS`]) bound straight in.
fn push_symlinks_and_layer_dirs(a: &mut Vec<OsString>, plan: &GuestPlan) {
    // The symlinks a Fedora root has and a hand-built one does not. usrmerge
    // first: the overlay destinations are `/usr` and `/etc`, so the new root has
    // no `/bin` until these exist, and `execvp("/bin/sh")` would fail. `/var/run`
    // and `/var/lock` matter the same way one level down — they're symlinks into
    // `/run` on every modern distribution, and a program that opens a path under
    // one gets `ENOENT` rather than a clear permission error when missing.
    for (target, link) in root_symlinks(&plan.base) {
        a.push("--symlink".into());
        a.push(target.into_os_string());
        a.push(link.into_os_string());
    }

    for dir in LAYER_VAR_DIRS {
        a.push("--bind".into());
        a.push(plan.upper.join(dir).into_os_string());
        a.push(PathBuf::from("/").join(dir).into_os_string());
    }
}

/// `/proc`, `/dev`, the fresh tmpfs mounts, and the resolver bind — the
/// resolver is ordered after the `/run` tmpfs so it isn't covered over.
fn push_pseudo_filesystems_and_resolver(a: &mut Vec<OsString>, plan: &GuestPlan) {
    for (flag, dest) in [
        ("--proc", "/proc"),
        ("--dev", "/dev"),
        ("--tmpfs", "/tmp"),
        ("--tmpfs", "/run"),
        ("--tmpfs", "/var/tmp"),
    ] {
        a.push(flag.into());
        a.push(dest.into());
    }

    // `/etc/resolv.conf` is a symlink into `/run` on a systemd-resolved host, and
    // `/run` is a fresh tmpfs here, so binding the file the link *points at*, at
    // that path, makes the base's existing link resolve — no write into `/etc`.
    // Ordered after the tmpfs: `bwrap` applies flags in sequence, so a bind under
    // `/run` placed earlier would be mounted and then covered over.
    if let Some((src, dst)) = resolv_bind(&plan.base) {
        a.push("--ro-bind-try".into());
        a.push(src.into_os_string());
        a.push(dst.into_os_string());
    }
}

/// The runtime binary (read-only, with its command shims) and the broker
/// socket the guest sends permission requests over.
fn push_runtime_and_broker(a: &mut Vec<OsString>, plan: &GuestPlan) {
    a.push("--dir".into());
    a.push(crate::GUEST_BIN_DIR.into());
    a.push("--ro-bind".into());
    a.push(plan.runtime_bin.clone().into_os_string());
    a.push(format!("{}/tilo-ush", crate::GUEST_BIN_DIR).into());
    for cmd in crate::GUEST_COMMANDS {
        if !crate::guest_command_available(&plan.base, cmd) {
            continue;
        }
        a.push("--symlink".into());
        a.push("tilo-ush".into());
        a.push(format!("{}/{cmd}", crate::GUEST_BIN_DIR).into());
    }
    for cmd in &plan.legacy_commands {
        a.push("--symlink".into());
        a.push("tilo-ush".into());
        a.push(format!("{}/{cmd}", crate::GUEST_BIN_DIR).into());
    }
    a.push("--bind".into());
    a.push(plan.broker_socket.clone().into_os_string());
    a.push(crate::GUEST_BROKER_SOCK.into());
}

/// The store's guest home bound over the real home path (plus the `/root`
/// symlink uid 0 resolves to), and the user's trusted/readable grants.
fn push_home_and_grants(a: &mut Vec<OsString>, plan: &GuestPlan) {
    // Bound over the real home path, so the guest sees the store's copy at `~`,
    // never the user's files. `bwrap` won't create a mount point missing at the
    // top of a fresh root, so `--dir` creates the mountpoint first.
    a.push("--dir".into());
    a.push(plan.home_mountpoint.clone().into_os_string());
    a.push("--bind".into());
    a.push(plan.guest_home.clone().into_os_string());
    a.push(plan.home_mountpoint.clone().into_os_string());
    // The guest is uid 0, so `getpwuid` says its home is `/root`. A symlink
    // rather than a second bind: two mounts of one source is two things to keep
    // in step, and `~` should resolve to the same inode `$HOME` does.
    a.push("--symlink".into());
    a.push(plan.home_mountpoint.clone().into_os_string());
    a.push("/root".into());

    for dir in &plan.readable_dirs {
        a.push("--ro-bind-try".into());
        a.push(dir.clone().into_os_string());
        a.push(dir.clone().into_os_string());
    }
    for dir in &plan.trusted_dirs {
        a.push("--bind-try".into());
        a.push(dir.clone().into_os_string());
        a.push(dir.clone().into_os_string());
    }
    // After the home bind, so it covers the store even when `~` is the real
    // home. A tmpfs here is not the user's files — it is the runtime's own
    // policy and audit, which must stay unreachable from the guest.
    if let Some(store) = &plan.masked_store {
        a.push("--tmpfs".into());
        a.push(store.clone().into_os_string());
    }
}

/// `/dev/fuse` and the capability bounding set — both profile-gated escape
/// hatches, never granted to a sandboxed guest.
fn push_capabilities(a: &mut Vec<OsString>, plan: &GuestPlan) {
    if plan.profile.grants_fuse() {
        a.push("--dev-bind-try".into());
        a.push("/dev/fuse".into());
        a.push("/dev/fuse".into());
    }
    if !plan.profile.confines_namespaces() {
        // `bwrap` empties the capability bounding set before it execs, so a guest
        // can `unshare(CLONE_NEWUSER)` and then not write the new namespace's
        // `uid_map` — where nested rootless podman stops. These capabilities exist
        // only inside the guest's own namespace, so only the developer profile gets them.
        a.push("--cap-add".into());
        a.push("ALL".into());
    }
}

/// The guest's whole environment: fixed variables, the locale/timezone
/// carried from the supervisor, the plan's own extras, and finally
/// unsetting the store path so nothing inside the namespace can reach it.
fn push_environment(a: &mut Vec<OsString>, plan: &GuestPlan) {
    let setenv = |k: &str, v: &str, a: &mut Vec<OsString>| {
        a.push("--setenv".into());
        a.push(k.into());
        a.push(v.into());
    };
    setenv("PATH", GUEST_PATH, a);
    setenv("HOME", &plan.home_mountpoint.to_string_lossy(), a);
    setenv("TERM", &plan.term, a);
    setenv("USER", "root", a);
    setenv("LOGNAME", "root", a);
    setenv("SHELL", &plan.shell.to_string_lossy(), a);
    setenv("TILO_USH_PROFILE", plan.profile.as_str(), a);
    setenv("TILO_USH_BROKER", crate::GUEST_BROKER_SOCK, a);
    // Carried across explicitly since the guest no longer inherits it: a shell
    // with no `LANG` answers in the C locale, breaking `ls`, `date`, and messages.
    for key in INHERITED_ENV {
        let Some(value) = std::env::var_os(key) else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        setenv(key, &value.to_string_lossy(), a);
    }
    for (k, v) in &plan.env {
        setenv(k, v, a);
    }
    // The store holds the policy file and audit log; unreachable from inside the
    // namespace anyway, but leaving the variable set would follow a program out.
    a.push("--unsetenv".into());
    a.push(crate::store::STORE_ENV.into());
}

/// The seccomp filter — last of the sandbox flags, applied immediately
/// before `bwrap` execs — followed by the guest's own argv.
fn push_seccomp_and_command(
    a: &mut Vec<OsString>,
    plan: &GuestPlan,
    seccomp_fd: Option<std::os::fd::RawFd>,
) {
    // Placed last of the sandbox flags and applied by `bwrap` immediately before
    // it execs, after every mount above — this is what stops the supervisor's
    // need for `mount`/`unshare` from reaching anything the user runs. `bwrap`
    // takes an already-open descriptor rather than a path, so [`exec`] and
    // [`spawn`] open the blob (see [`open_seccomp_fd`]) themselves.
    if let Some(fd) = seccomp_fd {
        a.push("--seccomp".into());
        a.push(fd.to_string().into());
    }

    a.push("--".into());
    if plan.command.is_empty() {
        a.push(plan.shell.clone().into_os_string());
        a.push("-i".into());
    } else {
        for arg in &plan.command {
            a.push(arg.into());
        }
    }
}

/// `remove_dir_all`, but restoring traversal permission as it descends:
/// overlayfs creates `<workdir>/work` with mode `000`, so an ordinary
/// recursive delete stops there with "permission denied".
pub fn remove_dir_all_forced(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = path.symlink_metadata() else {
        return;
    };
    if meta.is_dir() {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                remove_dir_all_forced(&entry.path());
            }
        }
        let _ = std::fs::remove_dir(path);
    } else {
        let _ = std::fs::remove_file(path);
    }
}

/// The symlinks the base's root directory has, recreated in the guest's.
///
/// A root built from overlays and binds starts with only the directories
/// those produced, so every symlink the real root carries is missing —
/// `/bin`/`/sbin`/`/lib`/`/lib64` → `usr/…` (without them `execvp("/bin/sh")`
/// fails), `/home` → `var/home` on an ostree system, and `/var/run`/
/// `/var/lock` → `/run`. Read from the base rather than hard-coded, since a
/// guest that disagrees with its base about where `/bin` points is a guest
/// where paths mean two different things; the usrmerge set is only the floor
/// for a base that lists none.
pub fn root_symlinks(base: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut out: Vec<(PathBuf, PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(base) {
        let mut found: Vec<(PathBuf, PathBuf)> = entries
            .flatten()
            .filter_map(|e| {
                let path = e.path();
                if !path.symlink_metadata().ok()?.file_type().is_symlink() {
                    return None;
                }
                let name = path.file_name()?;
                // `/root` is the runtime's to place: the guest is uid 0, so `getpwuid` sends
                // it there, and it must land on the guest's home, not the base's root account.
                if name == "root" {
                    return None;
                }
                let target = std::fs::read_link(&path).ok()?;
                Some((target, PathBuf::from("/").join(name)))
            })
            .collect();
        found.sort();
        out.extend(found);
    }
    for (target, link) in [
        ("usr/bin", "/bin"),
        ("usr/sbin", "/sbin"),
        ("usr/lib", "/lib"),
        ("usr/lib64", "/lib64"),
    ] {
        let link = PathBuf::from(link);
        if !out.iter().any(|(_, l)| *l == link) {
            out.push((PathBuf::from(target), link));
        }
    }
    // One level down, so not in the root listing.
    out.push((PathBuf::from("../run"), PathBuf::from("/var/run")));
    out.push((PathBuf::from("../run/lock"), PathBuf::from("/var/lock")));
    out
}

/// Where to bind the host's resolver: `(source on host, destination in guest)`.
///
/// Destination is the *link target* when `/etc/resolv.conf` is a symlink
/// (usually under `/run`, replaced by a tmpfs), else the path itself. `None`
/// means the base has no resolver — a machine with no network, not an error.
pub fn resolv_bind(base: &Path) -> Option<(PathBuf, PathBuf)> {
    let link = base.join("etc/resolv.conf");
    let meta = link.symlink_metadata().ok()?;
    let resolved = std::fs::canonicalize(&link).ok()?;
    if !meta.file_type().is_symlink() {
        return Some((resolved, PathBuf::from("/etc/resolv.conf")));
    }
    // The guest-visible path is the resolved one with the base stripped off.
    let inside = resolved
        .strip_prefix(base)
        .map_or_else(|_| resolved.clone(), |p| PathBuf::from("/").join(p));
    Some((resolved, inside))
}

/// The subordinate id range the real user owns, from `/etc/subuid`. `dsh`
/// needs it: rootless `podman` allocates uids from this range to own an
/// image's files. `None` is a normal state on a machine with no containers.
pub fn subid_range(subid_file: &Path, user: &str, uid: u32) -> Option<(u32, u32)> {
    let text = std::fs::read_to_string(subid_file).ok()?;
    for line in text.lines() {
        let mut parts = line.split(':');
        let who = parts.next()?.trim();
        let start: u32 = parts.next()?.trim().parse().ok()?;
        let count: u32 = parts.next()?.trim().parse().ok()?;
        if who == user || who.parse::<u32>() == Ok(uid) {
            return Some((start, count));
        }
    }
    None
}

/// Build a user namespace with the caller's whole subordinate id range
/// mapped, and return a descriptor naming it. Only [`Profile::Dsh`] uses
/// this: `bwrap --unshare-user` maps exactly one uid, but rootless `podman`
/// needs a range since an image's files are owned by a spread of uids.
/// Installing the range needs `CAP_SETUID` in the parent namespace, which
/// `newuidmap`/`newgidmap` carry as a file capability instead.
///
/// Forks a child into a bare user namespace, lets the helpers write its maps
/// from outside, then holds a descriptor to the namespace while the child
/// exits. `None` when the user has no allocation or the helpers are absent —
/// `dsh` then starts with a single mapped uid rather than failing.
pub fn prepare_userns(subid_dir: &Path) -> Option<std::fs::File> {
    // SAFETY: `geteuid`/`getegid` cannot fail.
    let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
    let user = std::env::var("USER").unwrap_or_default();
    let (uid_start, uid_count) = subid_range(&subid_dir.join("subuid"), &user, uid)?;
    let (gid_start, gid_count) = subid_range(&subid_dir.join("subgid"), &user, gid)?;

    // A pipe is how the child says "the namespace exists" and how the parent
    // says "the maps are written, you may go".
    let mut ready = [0i32; 2];
    let mut go = [0i32; 2];
    // SAFETY: both arrays are two-element `int` buffers, which is what `pipe`
    // fills in.
    if unsafe { libc::pipe(ready.as_mut_ptr()) } != 0 || unsafe { libc::pipe(go.as_mut_ptr()) } != 0
    {
        return None;
    }

    // SAFETY: the child performs only async-signal-safe work — `unshare`,
    // `write`, `read`, `_exit` — and never returns into this program.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return None;
    }
    if pid == 0 {
        // SAFETY: as above; every call here is async-signal-safe.
        unsafe {
            libc::close(ready[0]);
            libc::close(go[1]);
            if libc::unshare(libc::CLONE_NEWUSER) != 0 {
                libc::_exit(1);
            }
            let byte = 1u8;
            libc::write(ready[1], (&raw const byte).cast::<libc::c_void>(), 1);
            let mut buf = 0u8;
            libc::read(go[0], (&raw mut buf).cast::<libc::c_void>(), 1);
            libc::_exit(0)
        }
    }

    // SAFETY: descriptors this process owns.
    unsafe {
        libc::close(ready[1]);
        libc::close(go[0]);
    }
    let mut buf = 0u8;
    // SAFETY: `ready[0]` is a live read end owned by this process.
    let got = unsafe { libc::read(ready[0], (&raw mut buf).cast::<libc::c_void>(), 1) };
    if got != 1 {
        reap(pid, go[1], ready[0]);
        return None;
    }

    // uid 0 in the guest is the real user; 1.. is the subordinate range. The
    // same shape rootless podman installs on the host.
    let ok = run_idmap("newuidmap", pid, &[(0, uid, 1), (1, uid_start, uid_count)])
        && run_idmap("newgidmap", pid, &[(0, gid, 1), (1, gid_start, gid_count)]);
    if !ok {
        reap(pid, go[1], ready[0]);
        return None;
    }

    // Holding this open is what keeps the namespace alive after the child exits.
    let ns = std::fs::File::open(format!("/proc/{pid}/ns/user")).ok();
    reap(pid, go[1], ready[0]);
    ns.and_then(|f| clear_cloexec(f).ok())
}

fn reap(pid: libc::pid_t, go_write: i32, ready_read: i32) {
    let byte = 1u8;
    // SAFETY: descriptors and a child pid this process owns.
    unsafe {
        libc::write(go_write, (&raw const byte).cast::<libc::c_void>(), 1);
        libc::close(go_write);
        libc::close(ready_read);
        let mut status = 0i32;
        libc::waitpid(pid, &raw mut status, 0);
    }
}

fn run_idmap(program: &str, pid: libc::pid_t, ranges: &[(u32, u32, u32)]) -> bool {
    let mut cmd = std::process::Command::new(program);
    cmd.arg(pid.to_string());
    for (inside, outside, count) in ranges {
        cmd.arg(inside.to_string());
        cmd.arg(outside.to_string());
        cmd.arg(count.to_string());
    }
    match cmd.status() {
        Ok(s) if s.success() => true,
        Ok(s) => {
            tracing::warn!("{program} exited {s}; the guest gets a single mapped uid");
            false
        }
        Err(e) => {
            tracing::warn!("{program} could not be run ({e}); the guest gets a single mapped uid");
            false
        }
    }
}

fn clear_cloexec(file: std::fs::File) -> std::io::Result<std::fs::File> {
    use std::os::fd::AsRawFd;
    let fd = file.as_raw_fd();
    // SAFETY: `fd` is owned by `file`, which is returned to the caller.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(file)
}

/// Open the compiled filter and hand back a descriptor `bwrap` will inherit.
/// `O_CLOEXEC` must come off so the number in the argv still names this file
/// after `exec`; the `File` is returned too so the caller keeps it alive.
///
/// # Errors
///
/// Returns an error if `blob` can't be opened, or its close-on-exec flag
/// can't be read or cleared.
pub fn open_seccomp_fd(blob: &Path) -> std::io::Result<(std::fs::File, std::os::fd::RawFd)> {
    use std::os::fd::AsRawFd;
    let file = std::fs::File::open(blob)?;
    let fd = file.as_raw_fd();
    // SAFETY: `fd` is owned by `file`, which outlives this call.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((file, fd))
}

/// Spawn the guest, replacing the current process. Never returns on success:
/// `bwrap` becomes the process the caller was, so a `SIGINT` reaches the
/// guest and the exit status is the guest's own.
pub fn exec(plan: &GuestPlan) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    let (_blob, fd) = match open_seccomp_fd(&plan.seccomp_blob) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let (_ns, ns_fd) = userns_for(plan);
    let argv = build_argv(plan, Some(fd), ns_fd);
    // `build_argv` always pushes "bwrap" first, so this never actually
    // misses, but the return type has no way to say that.
    let Some((program, rest)) = argv.split_first() else {
        return std::io::Error::other("build_argv produced an empty argv");
    };
    let mut cmd = std::process::Command::new(program);
    cmd.args(rest);
    cmd.exec()
}

/// Spawn the guest as a child.
///
/// # Errors
///
/// Returns an error if the seccomp filter can't be opened, or the child
/// process can't be spawned.
pub fn spawn(plan: &GuestPlan) -> std::io::Result<std::process::Child> {
    let (_blob, fd) = open_seccomp_fd(&plan.seccomp_blob)?;
    let (_ns, ns_fd) = userns_for(plan);
    let argv = build_argv(plan, Some(fd), ns_fd);
    // `build_argv` always pushes "bwrap" first, so this never actually
    // misses, but the return type has no way to say that.
    let Some((program, rest)) = argv.split_first() else {
        return Err(std::io::Error::other("build_argv produced an empty argv"));
    };
    std::process::Command::new(program).args(rest).spawn()
}

/// The prepared user namespace for a plan, if its profile asks for one; the
/// `File` is returned so the caller keeps it alive until `bwrap` joins it.
fn userns_for(plan: &GuestPlan) -> (Option<std::fs::File>, Option<std::os::fd::RawFd>) {
    use std::os::fd::AsRawFd;
    if !plan.profile.grants_subid_range() {
        return (None, None);
    }
    if let Some(file) = prepare_userns(Path::new("/etc")) {
        let fd = file.as_raw_fd();
        (Some(file), Some(fd))
    } else {
        tracing::warn!(
            "no subordinate id range is available; this session gets a \
             single mapped uid and nested rootless containers will need \
             storage.conf's ignore_chown_errors"
        );
        (None, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> GuestPlan {
        GuestPlan {
            profile: Profile::Ush,
            base: PathBuf::from("/"),
            upper: PathBuf::from("/store/layers/persistent"),
            lower_layers: Vec::new(),
            workdir_root: PathBuf::from("/store/sessions/s1"),
            guest_home: PathBuf::from("/store/home"),
            home_mountpoint: PathBuf::from("/var/home/you"),
            masked_store: None,
            trusted_dirs: Vec::new(),
            readable_dirs: Vec::new(),
            runtime_bin: PathBuf::from("/usr/libexec/tilo-ush"),
            legacy_commands: vec!["code".into()],
            broker_socket: PathBuf::from("/store/sessions/s1/broker.sock"),
            seccomp_blob: PathBuf::from("/store/sessions/s1/ush.bpf"),
            command: Vec::new(),
            shell: PathBuf::from("/usr/bin/bash"),
            term: "xterm-256color".into(),
            env: Vec::new(),
        }
    }

    fn strings(argv: &[OsString]) -> Vec<String> {
        argv.iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }

    fn window(argv: &[OsString], n: usize) -> Vec<Vec<String>> {
        strings(argv)
            .windows(n)
            .map(<[std::string::String]>::to_vec)
            .collect()
    }

    #[test]
    fn the_base_is_overlaid_by_layers_and_never_bound_writable() {
        // phase: the_base_subtrees_are_overlaid_with_the_layer_on_top
        {
            let p = plan();
            let argv = build_argv(&p, Some(9), None);
            let w4 = window(&argv, 4);
            for subtree in ["usr", "etc"] {
                assert!(
                    w4.iter().any(|w| w[0] == "--overlay"
                        && w[1] == format!("/store/layers/persistent/{subtree}")
                        && w[2] == format!("/store/sessions/s1/work/{subtree}")
                        && w[3] == format!("/{subtree}")),
                    "/{subtree} is not overlaid onto the layer: {:?}",
                    strings(&argv)
                );
            }
        }
        // phase: nothing_binds_a_base_subtree_writable
        // The property the whole design rests on. If the host's `/usr` were ever
        // bound read-write, a guest could rewrite the operating system.
        {
            let p = plan();
            let argv = strings(&build_argv(&p, Some(9), None));
            for (i, arg) in argv.iter().enumerate() {
                if arg == "--bind" || arg == "--bind-try" || arg == "--dev-bind" {
                    let src = &argv[i + 1];
                    for subtree in BASE_SUBTREES {
                        let owned = format!("/{subtree}");
                        assert!(
                            !(src == &owned || src.starts_with(&format!("{owned}/"))),
                            "{arg} {src} would make the host's {owned} writable"
                        );
                    }
                }
            }
        }
        // phase: an_ephemeral_session_stacks_the_persistent_layer_below_it
        {
            let dir = tempfile::tempdir().unwrap();
            let persistent = dir.path().join("persistent");
            std::fs::create_dir_all(persistent.join("usr")).unwrap();
            std::fs::create_dir_all(persistent.join("etc")).unwrap();

            let mut p = plan();
            p.upper = dir.path().join("ephemeral/s1");
            p.lower_layers = vec![persistent.clone()];
            let argv = strings(&build_argv(&p, Some(9), None));

            let usr_overlay = argv.iter().position(|a| a == "--overlay").unwrap();
            let srcs: Vec<&String> = argv[..usr_overlay]
                .iter()
                .enumerate()
                .filter(|(i, a)| *a == "--overlay-src" && *i + 1 < usr_overlay)
                .map(|(i, _)| &argv[i + 1])
                .collect();
            assert_eq!(
                srcs,
                vec![
                    &persistent.join("usr").to_string_lossy().into_owned(),
                    &"/usr".to_string()
                ],
                "the persistent layer must sit between the session layer and the base"
            );
        }
        // phase: prepare_clears_a_stale_workdir
        {
            let dir = tempfile::tempdir().unwrap();
            let mut p = plan();
            p.workdir_root = dir.path().join("s1");
            p.guest_home = dir.path().join("home");
            let stale = p.workdir_for("usr").join("work/leftover");
            std::fs::create_dir_all(&stale).unwrap();
            p.prepare().unwrap();
            assert!(p.workdir_for("usr").is_dir());
            assert!(
                !stale.exists(),
                "a workdir left by a crashed session makes the next mount fail"
            );
            assert!(p.guest_home.is_dir());
        }
    }

    #[test]
    fn the_root_carries_usrmerge_links_and_mirrors_the_bases_symlinks() {
        // phase: the_root_carries_the_usrmerge_links_even_for_a_base_that_lists_none
        {
            let dir = tempfile::tempdir().unwrap();
            let links = root_symlinks(dir.path());
            for (target, link) in [
                ("usr/bin", "/bin"),
                ("usr/sbin", "/sbin"),
                ("usr/lib", "/lib"),
                ("usr/lib64", "/lib64"),
                // Without these, a path under /var/run reads ENOENT rather than confined.
                ("../run", "/var/run"),
                ("../run/lock", "/var/lock"),
            ] {
                assert!(
                    links
                        .iter()
                        .any(|(t, l)| t == Path::new(target) && l == Path::new(link)),
                    "{link} -> {target} missing; a guest without it cannot run /bin/sh \
                 or reach /var/run: {links:?}"
                );
            }
        }
        // phase: the_root_mirrors_the_bases_own_symlinks
        // `/home` is a symlink to `var/home` on an ostree system; without mirroring
        // it, paths the user types under `/home` miss where the runtime bound it.
        {
            let dir = tempfile::tempdir().unwrap();
            let base = dir.path();
            std::fs::create_dir_all(base.join("var/home")).unwrap();
            std::os::unix::fs::symlink("var/home", base.join("home")).unwrap();
            std::os::unix::fs::symlink("usr/bin", base.join("bin")).unwrap();

            // A base that keeps root's home elsewhere, as an ostree system does.
            std::os::unix::fs::symlink("var/roothome", base.join("root")).unwrap();

            let links = root_symlinks(base);
            assert!(
                links
                    .iter()
                    .any(|(t, l)| t == Path::new("var/home") && l == Path::new("/home")),
                "the base's /home -> var/home was not mirrored: {links:?}"
            );
            assert!(
                !links.iter().any(|(_, l)| l == Path::new("/root")),
                "/root belongs to the runtime — the guest is uid 0, so getpwuid \
             sends it there and it must reach the guest's home, not the base's \
             root account: {links:?}"
            );
            assert_eq!(
                links.iter().filter(|(_, l)| l == Path::new("/bin")).count(),
                1,
                "a link the base already declares must not be added twice: {links:?}"
            );

            let mut p = plan();
            p.base = base.to_path_buf();
            let argv = build_argv(&p, Some(9), None);
            assert!(
                window(&argv, 3)
                    .iter()
                    .any(|w| w[0] == "--symlink" && w[1] == "var/home" && w[2] == "/home"),
                "the mirrored link never reached the argv"
            );
        }
    }

    #[test]
    fn the_guest_binds_only_the_store_home_grants_and_runtime() {
        // phase: the_guest_home_is_the_stores_copy_and_never_the_real_one
        {
            let argv = build_argv(&plan(), Some(9), None);
            let w3 = window(&argv, 3);
            assert!(
                w3.iter()
                    .any(|w| w[0] == "--bind" && w[1] == "/store/home" && w[2] == "/var/home/you"),
                "the store's home must be bound over the real home path"
            );
            assert!(
                w3.iter()
                    .any(|w| w[0] == "--symlink" && w[1] == "/var/home/you" && w[2] == "/root"),
                "the guest is uid 0, so getpwuid points at /root; it has to reach              the same directory as $HOME"
            );
            assert!(
                !strings(&argv)
                    .windows(2)
                    .any(|w| w[0].starts_with("--bind") && w[1] == "/var/home/you"),
                "the real home directory must not be a bind source"
            );
        }
        // phase: a_trusted_directory_is_bound_read_write_and_a_readable_one_read_only
        {
            let mut p = plan();
            p.trusted_dirs = vec![PathBuf::from("/var/home/you/src")];
            p.readable_dirs = vec![PathBuf::from("/var/home/you/notes")];
            let argv = build_argv(&p, Some(9), None);
            let w3 = window(&argv, 3);
            assert!(w3.iter().any(|w| w[0] == "--bind-try"
                && w[1] == "/var/home/you/src"
                && w[2] == "/var/home/you/src"));
            assert!(w3.iter().any(|w| w[0] == "--ro-bind-try"
                && w[1] == "/var/home/you/notes"
                && w[2] == "/var/home/you/notes"));
        }
        // phase: a_guest_with_no_grants_binds_no_user_content
        // A guest with no grants must see no host *content* beyond the base —
        // asserted on the argv rather than on the prose. Three things are bound and
        // each must justify itself: the store, the runtime binary (read-only, so
        // the guest can't rewrite the next session's confinement), and the resolver.
        {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("etc")).unwrap();
            std::fs::write(dir.path().join("etc/resolv.conf"), b"nameserver 1.1.1.1\n").unwrap();
            let mut p = plan();
            p.base = dir.path().to_path_buf();

            let argv = strings(&build_argv(&p, Some(9), None));
            let resolver = dir
                .path()
                .join("etc/resolv.conf")
                .canonicalize()
                .unwrap()
                .display()
                .to_string();

            for (i, arg) in argv.iter().enumerate() {
                if !(arg.starts_with("--bind") || arg.starts_with("--ro-bind")) {
                    continue;
                }
                let src = &argv[i + 1];
                if src == &resolver {
                    assert!(
                        arg.starts_with("--ro-bind"),
                        "the resolver must be read-only: {arg} {src}"
                    );
                    continue;
                }
                assert!(
                    src.starts_with("/store/") || src.starts_with("/usr/libexec/"),
                    "a guest with no grants binds {src}, which is neither the store, \
                 the runtime binary, nor the resolver"
                );
            }
        }
        // phase: the_runtime_binary_is_bound_read_only
        {
            let argv = strings(&build_argv(&plan(), Some(9), None));
            let i = argv
                .iter()
                .position(|a| a == "/usr/libexec/tilo-ush")
                .expect("runtime binary not bound");
            assert_eq!(
                argv[i - 1],
                "--ro-bind",
                "a guest that could rewrite the runtime binary could rewrite the \
             next session's confinement"
            );
        }
        // phase: the_store_location_is_unset_in_the_guest
        {
            let argv = strings(&build_argv(&plan(), Some(9), None));
            let i = argv.iter().position(|a| a == "--unsetenv").unwrap();
            assert_eq!(argv[i + 1], crate::store::STORE_ENV);
        }
    }

    #[test]
    fn the_guest_command_line_applies_seccomp_and_runtime_shims() {
        // phase: the_seccomp_blob_is_applied_and_the_guest_argv_comes_last
        {
            let argv = strings(&build_argv(&plan(), Some(9), None));
            let sep = argv
                .iter()
                .position(|a| a == "--")
                .expect("no -- separator");
            let blob = argv
                .iter()
                .position(|a| a == "--seccomp")
                .expect("no seccomp filter");
            assert!(
                blob < sep,
                "the filter must be a sandbox flag, not part of the guest's argv"
            );
            assert_eq!(argv[blob + 1], "9");
            assert_eq!(argv[sep + 1], "/usr/bin/bash");
        }
        // phase: a_command_replaces_the_interactive_shell
        {
            let mut p = plan();
            p.command = vec!["/usr/bin/bash".into(), "-c".into(), "echo hi".into()];
            let argv = strings(&build_argv(&p, Some(9), None));
            let sep = argv.iter().position(|a| a == "--").unwrap();
            assert_eq!(&argv[sep + 1..], &["/usr/bin/bash", "-c", "echo hi"]);
            assert!(
                !argv[sep..].contains(&"-i".to_string()),
                "`ush -c` must not open an interactive shell"
            );
        }
        // phase: the_runtime_commands_are_on_path_ahead_of_the_layer
        {
            let argv = strings(&build_argv(&plan(), Some(9), None));
            let i = argv.iter().position(|a| a == "PATH").unwrap();
            assert!(
                argv[i + 1].starts_with(&format!("{}:", crate::GUEST_BIN_DIR)),
                "PATH is {}; the runtime's own commands must come first",
                argv[i + 1]
            );
            let w3 = window(&build_argv(&plan(), Some(9), None), 3);
            for cmd in crate::GUEST_COMMANDS {
                if !crate::guest_command_available(&plan().base, cmd) {
                    continue;
                }
                assert!(
                    w3.iter().any(|w| w[0] == "--symlink"
                        && w[1] == "tilo-ush"
                        && w[2] == format!("{}/{cmd}", crate::GUEST_BIN_DIR)),
                    "`{cmd}` has no shim in the guest"
                );
            }
            assert!(
                w3.iter().any(|w| w[0] == "--symlink"
                    && w[1] == "tilo-ush"
                    && w[2] == format!("{}/code", crate::GUEST_BIN_DIR)),
                "an installed legacy app has no shim in the guest"
            );
        }
        // phase: management_shims_follow_provider_availability
        {
            let dir = tempfile::tempdir().unwrap();
            let mut p = plan();
            p.base = dir.path().to_path_buf();
            let without = strings(&build_argv(&p, Some(9), None));
            assert!(!without.iter().any(|arg| arg.ends_with("/tilo")));
            assert!(!without.iter().any(|arg| arg.ends_with("/app")));

            let bin = dir.path().join("usr/bin");
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::write(bin.join("tilo"), "").unwrap();
            std::fs::write(bin.join("app"), "").unwrap();
            let provider = dir.path().join("usr/libexec/tilo-deployment");
            std::fs::create_dir_all(provider.parent().unwrap()).unwrap();
            std::fs::write(provider, "").unwrap();
            let with = strings(&build_argv(&p, Some(9), None));
            for command in ["tilo", "system", "app", "store"] {
                assert!(
                    with.iter()
                        .any(|arg| arg == &format!("{}/{command}", crate::GUEST_BIN_DIR)),
                    "{command} shim missing despite provider"
                );
            }
        }
    }

    #[test]
    fn only_the_developer_profile_gets_fuse_capabilities_and_userns_changes() {
        // phase: only_the_developer_profile_gets_dev_fuse
        {
            let ush = strings(&build_argv(&plan(), Some(9), None));
            assert!(!ush.iter().any(|a| a == "/dev/fuse"));

            let mut p = plan();
            p.profile = Profile::Dsh;
            let dsh = strings(&build_argv(&p, Some(9), None));
            assert!(
                dsh.windows(3).any(|w| w[0] == "--dev-bind-try"
                    && w[1] == "/dev/fuse"
                    && w[2] == "/dev/fuse"),
                "dsh must have /dev/fuse; container tooling needs it"
            );
        }
        // phase: only_the_developer_profile_keeps_a_capability_bounding_set
        // The capability bounding set is the difference between "can create a
        // namespace" and "can populate one". Only the profile that is not a
        // sandbox gets it.
        {
            let ush = strings(&build_argv(&plan(), Some(9), None));
            assert!(
                !ush.iter().any(|a| a == "--cap-add"),
                "a sandboxed guest must exec with an empty bounding set: {ush:?}"
            );

            let mut p = plan();
            p.profile = Profile::Dsh;
            let dsh = strings(&build_argv(&p, Some(9), None));
            assert!(
                dsh.windows(2).any(|w| w[0] == "--cap-add" && w[1] == "ALL"),
                "without the bounding set a dsh guest can unshare a user namespace \
             and then not write its uid_map, which is where nested rootless \
             podman stops: {dsh:?}"
            );
        }
        // phase: a_prepared_user_namespace_replaces_unshare_user
        // A prepared namespace replaces `--unshare-user`; the two cannot both be
        // passed, and getting it wrong would silently drop the id mapping.
        {
            let without = strings(&build_argv(&plan(), Some(9), None));
            assert!(without.iter().any(|a| a == "--unshare-user"));
            assert!(!without.iter().any(|a| a == "--userns"));

            let with = strings(&build_argv(&plan(), Some(9), Some(11)));
            assert!(
                !with.iter().any(|a| a == "--unshare-user"),
                "bwrap refuses --userns together with --unshare-user"
            );
            let i = with.iter().position(|a| a == "--userns").unwrap();
            assert_eq!(with[i + 1], "11");
        }
    }

    #[test]
    fn the_resolver_is_bound_at_its_real_path_after_the_run_tmpfs() {
        // phase: the_resolver_is_bound_at_the_path_the_symlink_points_at
        // A dangling `/etc/resolv.conf` is the difference between a shell that can
        // install a package and one that reports every mirror as unreachable.
        {
            let dir = tempfile::tempdir().unwrap();
            let base = dir.path();
            std::fs::create_dir_all(base.join("etc")).unwrap();
            std::fs::create_dir_all(base.join("run/systemd/resolve")).unwrap();
            let real = base.join("run/systemd/resolve/stub-resolv.conf");
            std::fs::write(&real, b"nameserver 127.0.0.53\n").unwrap();
            std::os::unix::fs::symlink(
                "../run/systemd/resolve/stub-resolv.conf",
                base.join("etc/resolv.conf"),
            )
            .unwrap();

            let (src, dst) = resolv_bind(base).expect("no resolver bind");
            assert_eq!(src, real.canonicalize().unwrap());
            assert_eq!(
                dst,
                PathBuf::from("/run/systemd/resolve/stub-resolv.conf"),
                "binding it at /etc/resolv.conf would try to write through a \
             dangling symlink; the link target is the destination"
            );
        }
        // phase: the_resolver_bind_comes_after_the_run_tmpfs
        // `bwrap` applies its arguments in order, so a bind under `/run` placed
        // before `--tmpfs /run` gets mounted and covered over — symptom: every
        // name lookup fails, which reads as "no network" not an ordering bug.
        {
            let dir = tempfile::tempdir().unwrap();
            let base = dir.path();
            std::fs::create_dir_all(base.join("etc")).unwrap();
            std::fs::create_dir_all(base.join("run/systemd/resolve")).unwrap();
            std::fs::write(
                base.join("run/systemd/resolve/stub-resolv.conf"),
                b"nameserver 1.1.1.1\n",
            )
            .unwrap();
            std::os::unix::fs::symlink(
                "../run/systemd/resolve/stub-resolv.conf",
                base.join("etc/resolv.conf"),
            )
            .unwrap();

            let mut p = plan();
            p.base = base.to_path_buf();
            let argv = strings(&build_argv(&p, Some(9), None));

            let tmpfs_run = argv
                .windows(2)
                .position(|w| w[0] == "--tmpfs" && w[1] == "/run")
                .expect("no tmpfs on /run");
            let resolver = argv
                .iter()
                .position(|a| a == "/run/systemd/resolve/stub-resolv.conf")
                .expect("the resolver is not bound");
            assert!(
                resolver > tmpfs_run,
                "the resolver bind at {resolver} precedes the /run tmpfs at \
             {tmpfs_run} and would be covered over"
            );
        }
        // phase: a_plain_resolver_file_is_bound_where_it_is
        {
            let dir = tempfile::tempdir().unwrap();
            let base = dir.path();
            std::fs::create_dir_all(base.join("etc")).unwrap();
            std::fs::write(base.join("etc/resolv.conf"), b"nameserver 1.1.1.1\n").unwrap();
            let (_, dst) = resolv_bind(base).expect("no resolver bind");
            assert_eq!(dst, PathBuf::from("/etc/resolv.conf"));
        }
        // phase: a_base_with_no_resolver_is_not_an_error
        {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("etc")).unwrap();
            assert_eq!(resolv_bind(dir.path()), None);
        }
    }

    #[test]
    fn subid_range_reads_the_users_allocation_by_name_or_uid() {
        // phase: subid_range_reads_the_users_allocation
        {
            let dir = tempfile::tempdir().unwrap();
            let f = dir.path().join("subuid");
            std::fs::write(&f, "someone:100000:65536\nyou:524288:65536\n").unwrap();
            assert_eq!(subid_range(&f, "you", 1000), Some((524_288, 65_536)));
            assert_eq!(subid_range(&f, "nobody", 4242), None);
            assert_eq!(subid_range(Path::new("/nonexistent"), "you", 1000), None);
        }
        // phase: subid_range_matches_a_numeric_uid_line
        {
            let dir = tempfile::tempdir().unwrap();
            let f = dir.path().join("subuid");
            std::fs::write(&f, "1000:524288:65536\n").unwrap();
            assert_eq!(subid_range(&f, "you", 1000), Some((524_288, 65_536)));
        }
    }

    #[test]
    fn a_terminal_home_bind_is_the_caller_and_the_store_is_covered() {
        let mut terminal = plan();
        terminal.guest_home = PathBuf::from("/var/home/you");
        terminal.home_mountpoint = PathBuf::from("/var/home/you");
        terminal.masked_store = Some(PathBuf::from("/var/home/you/.local/share/ush"));
        let argv = strings(&build_argv(&terminal, Some(9), None));
        let w3: Vec<&[String]> = argv.windows(3).collect();
        assert!(
            w3.iter()
                .any(|w| w[0] == "--bind" && w[1] == "/var/home/you" && w[2] == "/var/home/you"),
            "the terminal shell must bind the caller's home: {argv:?}"
        );
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--tmpfs" && w[1] == "/var/home/you/.local/share/ush"),
            "policy and audit under that home must be covered: {argv:?}"
        );
        assert!(
            !argv
                .windows(2)
                .any(|w| w[0] == "--bind" && w[1] == "/store/home"),
            "the isolated copy must not replace the caller's home: {argv:?}"
        );
    }
}
