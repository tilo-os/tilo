//! Building the guest: the root filesystem, the namespaces, and the argv that
//! makes them.
//!
//! Everything here is a pure function of a [`GuestPlan`] until [`spawn`] is
//! called, so the shape of the confinement can be asserted on without a
//! kernel. The `bwrap` invocation is the whole of the mount and namespace
//! setup — the supervisor never calls `mount(2)` itself, which is why the
//! seccomp filter it installs on the guest can deny the syscall outright.
//!
//! # Why `bwrap` and not `mount(2)`
//!
//! `bwrap` already does exactly the sequence this needs — unshare, mount,
//! `pivot_root`, install a seccomp filter, `exec` — in that order, with the
//! filter landing after the last privileged operation and before the guest
//! exists. Writing that by hand would reproduce a well-reviewed program and
//! move the moment the filter is installed into code that is much easier to get
//! wrong. It is also already a hard dependency of qOS's app sandbox.
//!
//! # The root the guest ends up in
//!
//! ```text
//! /usr    overlay: lower = [ephemeral layer,] persistent layer, host /usr
//!                  upper = the session's writable layer
//! /etc    overlay: same stack
//! /bin /sbin /lib /lib64   symlinks into /usr, as on any usrmerge system
//! /var/lib /var/cache /var/log   the layer's own, bound
//! /var/tmp /tmp /run /proc /dev  fresh
//! /run/ush/bin            the runtime and its command shims, read-only
//! $HOME                   the store's guest home, bound over the real one
//! <trusted dirs>          bound read-write, one per `perm trust-dir`
//! ```
//!
//! Nothing else from the host is present. That is the enforcement: a path the
//! guest was not granted is not missing a permission, it is not in the
//! namespace.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::layer::BASE_SUBTREES;
use crate::profile::Profile;

/// Directories the layer owns outright rather than shadowing.
///
/// `/var` cannot be an overlay destination (see [`crate::layer::BASE_SUBTREES`]),
/// and the host's `/var` is machine state — package caches, service databases,
/// other users' homes — that a shell has no business reading. Binding the
/// layer's own is both what works and the tighter grant.
const LAYER_VAR_DIRS: &[&str] = &["var/lib", "var/cache", "var/log"];

/// `PATH` inside a guest. The runtime's own commands come first so `pkg` means
/// `pkg` even if a layer installs something by that name.
pub const GUEST_PATH: &str = "/run/ush/bin:/usr/local/bin:/usr/bin:/usr/local/sbin:/usr/sbin";

/// The only variables a guest inherits from the supervisor's own environment.
///
/// The rest of the guest's environment is *built* — see the environment block
/// in [`build_argv`] — and this is the short list of things that describe the
/// person rather than the session's wiring, so leaving them behind would
/// change what the user sees for no gain.
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
    /// Where the overlay `workdir`s go. Must be on the same filesystem as
    /// `upper`, which is why it is the session directory in the store and not
    /// `/tmp`.
    pub workdir_root: PathBuf,
    /// The guest's home, from the store.
    pub guest_home: PathBuf,
    /// Where it is mounted — the real user's home path, so `~` and any
    /// absolute path a script has baked in still resolve.
    pub home_mountpoint: PathBuf,
    /// Directories the user trusted: bound read-write.
    pub trusted_dirs: Vec<PathBuf>,
    /// Directories granted read but not write.
    pub readable_dirs: Vec<PathBuf>,
    /// The runtime binary, bound in so the guest's `pkg`/`perm`/… resolve.
    pub runtime_bin: PathBuf,
    /// The broker socket on the host.
    pub broker_socket: PathBuf,
    /// The compiled seccomp blob for [`Self::profile`].
    pub seccomp_blob: PathBuf,
    /// What to run. Empty means the interactive shell.
    pub command: Vec<String>,
    /// The shell to host when `command` is empty.
    pub shell: PathBuf,
    /// `TERM` for the guest.
    pub term: String,
    /// Extra environment, applied last.
    pub env: Vec<(String, String)>,
}

impl GuestPlan {
    /// The overlay `workdir` for one base subtree.
    pub fn workdir_for(&self, subtree: &str) -> PathBuf {
        self.workdir_root.join("work").join(subtree)
    }

    /// Create the per-session directories the plan refers to.
    ///
    /// The `workdir`s must exist and be empty; `bwrap` will not create them.
    pub fn prepare(&self) -> std::io::Result<()> {
        for subtree in BASE_SUBTREES {
            let work = self.workdir_for(subtree);
            // A workdir left behind by a crashed session holds overlayfs's
            // half-finished copy-ups. Reusing it makes the next mount fail with
            // a message about the workdir being in use, which reads as a bug in
            // the runtime rather than as leftover state.
            remove_dir_all_forced(&work);
            std::fs::create_dir_all(&work)?;
        }
        std::fs::create_dir_all(&self.guest_home)?;
        Ok(())
    }
}

/// Build the `bwrap` argv for a plan.
///
/// Pure. The returned vector starts with `bwrap` and ends with the guest's own
/// argv, so a caller can print it verbatim under `-v` and get something it can
/// paste into a shell.
pub fn build_argv(
    plan: &GuestPlan,
    seccomp_fd: Option<std::os::fd::RawFd>,
    userns_fd: Option<std::os::fd::RawFd>,
) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    let push = |a: &mut Vec<OsString>, s: &str| a.push(OsString::from(s));

    push(&mut a, "bwrap");

    // ---- environment, emptied ----------------------------------------
    // `bwrap` hands the parent's whole environment to the guest unless it is
    // told otherwise, and this parent is the supervisor: it knows where the
    // store is, where the broker listens, and what the session was started
    // with. The environment block further down builds the guest's from
    // scratch, and this is what makes that a complete list rather than a set
    // of overrides on top of whatever the supervisor was carrying.
    //
    // First, because `--clearenv` keeps only what a *later* `--setenv` puts
    // back.
    push(&mut a, "--clearenv");

    // ---- namespaces --------------------------------------------------
    // A fresh user namespace is what makes every mount below possible without
    // privilege, and uid 0 inside it is what lets `rpm` chown a file into the
    // layer. Outside the namespace that uid 0 is the unprivileged real user; it
    // confers nothing on the host.
    //
    // With a namespace prepared outside (see [`prepare_userns`]) `bwrap` joins
    // it rather than creating one — that is how the guest gets more than a
    // single mapped uid, which nested rootless container tooling needs.
    match userns_fd {
        Some(fd) => {
            push(&mut a, "--userns");
            a.push(fd.to_string().into());
        }
        None => push(&mut a, "--unshare-user"),
    }
    push(&mut a, "--uid");
    push(&mut a, "0");
    push(&mut a, "--gid");
    push(&mut a, "0");
    push(&mut a, "--unshare-pid");
    push(&mut a, "--unshare-uts");
    push(&mut a, "--unshare-ipc");
    push(&mut a, "--unshare-cgroup-try");
    push(&mut a, "--hostname");
    push(&mut a, plan.profile.as_str());
    // The guest must not outlive the supervisor. Without this a crashed
    // supervisor leaves a shell running in a namespace nothing is watching.
    push(&mut a, "--die-with-parent");
    // Deliberately *not* `--new-session`. It calls `setsid`, which detaches the
    // guest from its controlling terminal and takes job control with it — no
    // Ctrl-C, no Ctrl-Z, no foreground process group. `bwrap` documents it as
    // incompatible with interactive use, and an interactive shell is the only
    // thing this runtime hosts. What it defends against is `TIOCSTI` injection
    // back into the parent's terminal, which the kernel has refused by default
    // since `dev.tty.legacy_tiocsti` shipped off.



    // ---- the layered root --------------------------------------------
    for subtree in BASE_SUBTREES {
        // `--overlay-src` entries accumulate for the next `--overlay`. The
        // layers are pushed nearest-the-upper first and the base last, so the
        // base is the bottom of the stack.
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

    // The symlinks a Fedora root has and a hand-built one does not.
    //
    // usrmerge first: the overlay destinations are `/usr` and `/etc`, so the
    // new root has no `/bin` at all until these exist — and `execvp("/bin/sh")`,
    // which is what half the world's scripts start with, would fail.
    //
    // `/var/run` and `/var/lock` matter for the same reason one level up. They
    // are symlinks into `/run` on every modern distribution, and a program that
    // opens a path under one gets `ENOENT` rather than a permission error when
    // they are missing — which reads as the program being broken. `podman` is
    // the case that found this.
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

    // ---- fresh filesystems -------------------------------------------
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

    // ---- the resolver -------------------------------------------------
    // `/etc/resolv.conf` is a symlink into `/run` on a systemd-resolved host,
    // and `/run` is a fresh tmpfs in here, so the link dangles and every name
    // lookup fails. Binding the file the link *points at*, at that path, makes
    // the link the base already has resolve — no write into `/etc`, and no
    // guessing about which resolver manages the machine.
    //
    // Ordered after the tmpfs deliberately: `bwrap` applies these in sequence,
    // so a bind under `/run` placed before `--tmpfs /run` is mounted and then
    // covered over, and the symptom is every mirror being unreachable.
    if let Some((src, dst)) = resolv_bind(&plan.base) {
        a.push("--ro-bind-try".into());
        a.push(src.into_os_string());
        a.push(dst.into_os_string());
    }

    // ---- the runtime's own commands ----------------------------------
    a.push("--dir".into());
    a.push(crate::GUEST_BIN_DIR.into());
    a.push("--ro-bind".into());
    a.push(plan.runtime_bin.clone().into_os_string());
    a.push(format!("{}/qos-ush", crate::GUEST_BIN_DIR).into());
    for cmd in crate::GUEST_COMMANDS {
        a.push("--symlink".into());
        a.push("qos-ush".into());
        a.push(format!("{}/{cmd}", crate::GUEST_BIN_DIR).into());
    }
    a.push("--bind".into());
    a.push(plan.broker_socket.clone().into_os_string());
    a.push(crate::GUEST_BROKER_SOCK.into());

    // ---- the guest's home --------------------------------------------
    // Bound over the real home path, so what the guest sees at `~` is the
    // store's copy and never the user's files. `/root` gets the same directory
    // because the guest is uid 0 and `/etc/passwd` says that is where root
    // lives; a shell that disagreed with `getpwuid` about `$HOME` is a shell
    // that writes its history somewhere surprising.
    // `bwrap` will not create a mount point that does not exist at the top of a
    // fresh root, and the home path is produced by none of the mounts above.
    a.push("--dir".into());
    a.push(plan.home_mountpoint.clone().into_os_string());
    a.push("--bind".into());
    a.push(plan.guest_home.clone().into_os_string());
    a.push(plan.home_mountpoint.clone().into_os_string());
    // The guest is uid 0, so `getpwuid` says its home is `/root`. A symlink
    // rather than a second bind of the same directory: two mounts of one source
    // is two things to keep in step, and a program that resolves `~` through
    // the password database should land on the same inode as one that reads
    // `$HOME`, not merely on the same contents.
    a.push("--symlink".into());
    a.push(plan.home_mountpoint.clone().into_os_string());
    a.push("/root".into());

    // ---- what the user granted ---------------------------------------
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

    // ---- the developer profile's extras ------------------------------
    if plan.profile.grants_fuse() {
        a.push("--dev-bind-try".into());
        a.push("/dev/fuse".into());
        a.push("/dev/fuse".into());
    }
    if !plan.profile.confines_namespaces() {
        // `bwrap` empties the capability bounding set before it execs, so a
        // guest can call `unshare(CLONE_NEWUSER)` and then not be allowed to
        // write the new namespace's `uid_map` — which is where nested rootless
        // podman stops. These capabilities exist only inside the guest's own
        // user namespace, where host uid 0 is unmapped, so they buy authority
        // over the guest's children and none at all over the machine. They are
        // still the difference between a boundary and a convenience, which is
        // why only the developer profile gets them.
        a.push("--cap-add".into());
        a.push("ALL".into());
    }

    // ---- environment --------------------------------------------------
    let setenv = |k: &str, v: &str, a: &mut Vec<OsString>| {
        a.push("--setenv".into());
        a.push(k.into());
        a.push(v.into());
    };
    setenv("PATH", GUEST_PATH, &mut a);
    setenv("HOME", &plan.home_mountpoint.to_string_lossy(), &mut a);
    setenv("TERM", &plan.term, &mut a);
    setenv("USER", "root", &mut a);
    setenv("LOGNAME", "root", &mut a);
    setenv("SHELL", &plan.shell.to_string_lossy(), &mut a);
    setenv("QOS_USH_PROFILE", plan.profile.as_str(), &mut a);
    setenv("QOS_USH_BROKER", crate::GUEST_BROKER_SOCK, &mut a);
    // The user's language, carried across explicitly now that the guest no
    // longer inherits it. A shell whose `LANG` went missing answers in the C
    // locale: `ls` stops aligning non-ASCII names, `date` changes format, and
    // every message changes language mid-session.
    for key in INHERITED_ENV {
        let Some(value) = std::env::var_os(key) else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        setenv(key, &value.to_string_lossy(), &mut a);
    }
    for (k, v) in &plan.env {
        setenv(k, v, &mut a);
    }
    // The store holds the policy file and the audit log. A guest that learned
    // where it is could not read it — it is not in the namespace — but telling
    // it is pointless and the variable would follow a program back out.
    a.push("--unsetenv".into());
    a.push(crate::store::STORE_ENV.into());

    // ---- the syscall filter ------------------------------------------
    // Placed last of the sandbox flags and applied by `bwrap` immediately
    // before it execs, i.e. after every mount above has happened. This is the
    // line that stops the supervisor's need for `mount` and `unshare` from
    // reaching anything the user runs.
    //
    // `bwrap` takes an already-open descriptor rather than a path, so the
    // caller opens the blob (see [`open_seccomp_fd`]) and passes the number.
    // A plan with no descriptor produces an argv with no filter, which is why
    // [`exec`] and [`spawn`] open it themselves rather than leaving it to a
    // caller who might forget.
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
    a
}

/// `remove_dir_all`, but restoring traversal permission as it descends.
///
/// overlayfs creates `<workdir>/work` with mode `000`, so an ordinary recursive
/// delete stops there with "permission denied" and leaves the session directory
/// behind — the user owns it and simply cannot walk into it. Nothing here is
/// forced past an owner: it is the caller's own directory either way.
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
/// A root built out of overlays and binds starts with only the directories
/// those produced. Every symlink the real root carries is therefore missing,
/// and each one that is missing turns some path into `ENOENT`:
///
/// - `/bin`, `/sbin`, `/lib`, `/lib64` → `usr/…`. Without them
///   `execvp("/bin/sh")` fails, which is how half the world's scripts start.
/// - `/home` → `var/home` on an ostree system. Without it every path the user
///   types under `/home` misses the directory the runtime bound their home at.
/// - `/var/run`, `/var/lock` → `/run`. Nested one level down, so they are added
///   explicitly rather than read from the root listing.
///
/// Read from the base rather than hard-coded, because which of these a machine
/// has is a property of the distribution's layout, and a guest that disagrees
/// with its own base about where `/bin` points is a guest where paths mean two
/// different things. The usrmerge set is the floor for a base that lists none —
/// a base with no `/bin` at all cannot host a shell.
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
                // `/root` is the runtime's to place: the guest is uid 0, so
                // that is where `getpwuid` sends it, and it must land on the
                // guest's home rather than wherever the base keeps root's.
                // (`/root -> var/roothome` on an ostree system.)
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
    for (target, link) in [("usr/bin", "/bin"), ("usr/sbin", "/sbin"), ("usr/lib", "/lib"), ("usr/lib64", "/lib64")] {
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

/// Where to bind the host's resolver so `/etc/resolv.conf` resolves in the
/// guest: `(source on the host, destination in the guest)`.
///
/// The destination is the *link target* when the base's `/etc/resolv.conf` is a
/// symlink — usually somewhere under `/run`, which the guest replaces with a
/// tmpfs — and `/etc/resolv.conf` itself when it is a regular file. Returns
/// `None` when the base has no resolver at all, which is a machine with no
/// network rather than an error.
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
        .map(|p| PathBuf::from("/").join(p))
        .unwrap_or_else(|_| resolved.clone());
    Some((resolved, inside))
}

/// The subordinate id range the real user owns, from `/etc/subuid`.
///
/// `dsh` needs it: rootless `podman` allocates uids out of this range to own
/// the files in an image, and with one uid mapped it can only run images whose
/// files are all uid 0. Returns `None` when the user has no allocation, which
/// is a normal state on a machine that has never run a container — `dsh` then
/// starts with a single uid and says so rather than refusing.
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

/// Build a user namespace with the caller's whole subordinate id range mapped,
/// and return a descriptor naming it.
///
/// Only [`Profile::Dsh`] uses this. `bwrap --unshare-user` maps exactly one uid,
/// and rootless `podman` needs a range: an image's files are owned by a spread
/// of uids, and with one mapped it can only unpack images that are entirely
/// uid 0. The range lives in `/etc/subuid`, and installing it needs
/// `CAP_SETUID` in the parent namespace — which no unprivileged process has, and
/// which is exactly what `newuidmap` carries as a file capability.
///
/// So: fork a child into a bare user namespace, let `newuidmap` and `newgidmap`
/// write its maps from outside, then keep the namespace alive by holding a
/// descriptor to it while the child goes away. `bwrap --userns` joins it instead
/// of creating its own.
///
/// Returns `None` when the user has no allocation or the helpers are absent;
/// `dsh` then starts with a single uid, which is a weaker `dsh` and not a
/// failure.
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
            libc::write(ready[1], &byte as *const u8 as *const libc::c_void, 1);
            let mut buf = 0u8;
            libc::read(go[0], &mut buf as *mut u8 as *mut libc::c_void, 1);
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
    let got = unsafe { libc::read(ready[0], &mut buf as *mut u8 as *mut libc::c_void, 1) };
    if got != 1 {
        reap(pid, go[1], ready[0]);
        return None;
    }

    // uid 0 in the guest is the real user; 1.. is the subordinate range. The
    // same shape rootless podman installs on the host.
    let ok = run_idmap(
        "newuidmap",
        pid,
        &[(0, uid, 1), (1, uid_start, uid_count)],
    ) && run_idmap(
        "newgidmap",
        pid,
        &[(0, gid, 1), (1, gid_start, gid_count)],
    );
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
        libc::write(go_write, &byte as *const u8 as *const libc::c_void, 1);
        libc::close(go_write);
        libc::close(ready_read);
        let mut status = 0i32;
        libc::waitpid(pid, &mut status, 0);
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
///
/// `O_CLOEXEC` has to come off: the number in the argv has to still name this
/// file after the `exec`. The `File` is returned alongside so the caller keeps
/// it alive — dropping it would close the descriptor before `bwrap` reads it.
pub fn open_seccomp_fd(
    blob: &Path,
) -> std::io::Result<(std::fs::File, std::os::fd::RawFd)> {
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

/// Spawn the guest, replacing the current process.
///
/// Never returns on success. `bwrap` becomes the process the caller was, so a
/// `SIGINT` from a terminal reaches the guest and the exit status is the
/// guest's own.
pub fn exec(plan: &GuestPlan) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    let (_blob, fd) = match open_seccomp_fd(&plan.seccomp_blob) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let (_ns, ns_fd) = userns_for(plan);
    let argv = build_argv(plan, Some(fd), ns_fd);
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    cmd.exec()
}

/// Spawn the guest as a child.
pub fn spawn(plan: &GuestPlan) -> std::io::Result<std::process::Child> {
    let (_blob, fd) = open_seccomp_fd(&plan.seccomp_blob)?;
    let (_ns, ns_fd) = userns_for(plan);
    let argv = build_argv(plan, Some(fd), ns_fd);
    std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .spawn()
}

/// The prepared user namespace for a plan, if its profile asks for one. The
/// `File` is returned so the caller keeps it alive — closing it would destroy
/// the namespace before `bwrap` joins it.
fn userns_for(plan: &GuestPlan) -> (Option<std::fs::File>, Option<std::os::fd::RawFd>) {
    use std::os::fd::AsRawFd;
    if !plan.profile.grants_subid_range() {
        return (None, None);
    }
    match prepare_userns(Path::new("/etc")) {
        Some(file) => {
            let fd = file.as_raw_fd();
            (Some(file), Some(fd))
        }
        None => {
            tracing::warn!(
                "no subordinate id range is available; this session gets a \
                 single mapped uid and nested rootless containers will need \
                 storage.conf's ignore_chown_errors"
            );
            (None, None)
        }
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
            trusted_dirs: Vec::new(),
            readable_dirs: Vec::new(),
            runtime_bin: PathBuf::from("/usr/libexec/qos-ush"),
            broker_socket: PathBuf::from("/store/sessions/s1/broker.sock"),
            seccomp_blob: PathBuf::from("/store/sessions/s1/ush.bpf"),
            command: Vec::new(),
            shell: PathBuf::from("/usr/bin/bash"),
            term: "xterm-256color".into(),
            env: Vec::new(),
        }
    }

    fn strings(argv: &[OsString]) -> Vec<String> {
        argv.iter().map(|s| s.to_string_lossy().into_owned()).collect()
    }

    fn window(argv: &[OsString], n: usize) -> Vec<Vec<String>> {
        strings(argv).windows(n).map(|w| w.to_vec()).collect()
    }

    #[test]
    fn the_base_subtrees_are_overlaid_with_the_layer_on_top() {
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

    /// The property the whole design rests on. If the host's `/usr` were ever
    /// bound read-write, a guest could rewrite the operating system.
    #[test]
    fn nothing_binds_a_base_subtree_writable() {
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

    #[test]
    fn the_root_carries_the_usrmerge_links_even_for_a_base_that_lists_none() {
        let dir = tempfile::tempdir().unwrap();
        let links = root_symlinks(dir.path());
        for (target, link) in [
            ("usr/bin", "/bin"),
            ("usr/sbin", "/sbin"),
            ("usr/lib", "/lib"),
            ("usr/lib64", "/lib64"),
            // Without these, anything that opens a path under /var/run gets
            // ENOENT and reads as broken rather than as confined.
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

    /// The one that made `perm trust-dir` look broken: `/home` is a symlink to
    /// `var/home` on an ostree system, and without it every path the user types
    /// under `/home` misses the directory the runtime bound their home at.
    #[test]
    fn the_root_mirrors_the_bases_own_symlinks() {
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
            links
                .iter()
                .filter(|(_, l)| l == Path::new("/bin"))
                .count(),
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

    #[test]
    fn the_guest_home_is_the_stores_copy_and_never_the_real_one() {
        let argv = build_argv(&plan(), Some(9), None);
        let w3 = window(&argv, 3);
        assert!(
            w3.iter().any(|w| w[0] == "--bind"
                && w[1] == "/store/home"
                && w[2] == "/var/home/you"),
            "the store's home must be bound over the real home path"
        );
        assert!(
            w3.iter().any(|w| w[0] == "--symlink"
                && w[1] == "/var/home/you"
                && w[2] == "/root"),
            "the guest is uid 0, so getpwuid points at /root; it has to reach              the same directory as $HOME"
        );
        assert!(
            !strings(&argv)
                .windows(2)
                .any(|w| w[0].starts_with("--bind") && w[1] == "/var/home/you"),
            "the real home directory must not be a bind source"
        );
    }

    #[test]
    fn a_trusted_directory_is_bound_read_write_and_a_readable_one_read_only() {
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

    /// A guest with no grants must see no host *content* beyond the base. This
    /// is "an app sees your files only where you said it may", asserted on the
    /// argv rather than on the prose.
    ///
    /// Three things are bound and each has to justify itself: the store (the
    /// guest's own layer and home), the runtime binary (read-only, so the guest
    /// cannot rewrite the next session's confinement), and the machine's
    /// resolver. The resolver is the same judgement `launcher::profile` makes
    /// about `/etc/resolv.conf` — it describes the network the machine is on,
    /// carries no user content, and is read-only.
    #[test]
    fn a_guest_with_no_grants_binds_no_user_content() {
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

    #[test]
    fn the_seccomp_blob_is_applied_and_the_guest_argv_comes_last() {
        let argv = strings(&build_argv(&plan(), Some(9), None));
        let sep = argv.iter().position(|a| a == "--").expect("no -- separator");
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

    #[test]
    fn a_command_replaces_the_interactive_shell() {
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

    #[test]
    fn the_runtime_commands_are_on_path_ahead_of_the_layer() {
        let argv = strings(&build_argv(&plan(), Some(9), None));
        let i = argv.iter().position(|a| a == "PATH").unwrap();
        assert!(
            argv[i + 1].starts_with(&format!("{}:", crate::GUEST_BIN_DIR)),
            "PATH is {}; the runtime's own commands must come first",
            argv[i + 1]
        );
        let w3 = window(&build_argv(&plan(), Some(9), None), 3);
        for cmd in crate::GUEST_COMMANDS {
            assert!(
                w3.iter().any(|w| w[0] == "--symlink"
                    && w[1] == "qos-ush"
                    && w[2] == format!("{}/{cmd}", crate::GUEST_BIN_DIR)),
                "`{cmd}` has no shim in the guest"
            );
        }
    }

    #[test]
    fn the_runtime_binary_is_bound_read_only() {
        let argv = strings(&build_argv(&plan(), Some(9), None));
        let i = argv
            .iter()
            .position(|a| a == "/usr/libexec/qos-ush")
            .expect("runtime binary not bound");
        assert_eq!(
            argv[i - 1],
            "--ro-bind",
            "a guest that could rewrite the runtime binary could rewrite the \
             next session's confinement"
        );
    }

    #[test]
    fn the_store_location_is_unset_in_the_guest() {
        let argv = strings(&build_argv(&plan(), Some(9), None));
        let i = argv.iter().position(|a| a == "--unsetenv").unwrap();
        assert_eq!(argv[i + 1], crate::store::STORE_ENV);
    }

    #[test]
    fn only_the_developer_profile_gets_dev_fuse() {
        let ush = strings(&build_argv(&plan(), Some(9), None));
        assert!(!ush.iter().any(|a| a == "/dev/fuse"));

        let mut p = plan();
        p.profile = Profile::Dsh;
        let dsh = strings(&build_argv(&p, Some(9), None));
        assert!(
            dsh.windows(3)
                .any(|w| w[0] == "--dev-bind-try" && w[1] == "/dev/fuse" && w[2] == "/dev/fuse"),
            "dsh must have /dev/fuse; container tooling needs it"
        );
    }

    #[test]
    fn an_ephemeral_session_stacks_the_persistent_layer_below_it() {
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

    /// The capability bounding set is the difference between "can create a
    /// namespace" and "can populate one". Only the profile that is not a
    /// sandbox gets it.
    #[test]
    fn only_the_developer_profile_keeps_a_capability_bounding_set() {
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

    /// A prepared namespace replaces `--unshare-user`; the two cannot both be
    /// passed, and getting it wrong would silently drop the id mapping.
    #[test]
    fn a_prepared_user_namespace_replaces_unshare_user() {
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

    /// A dangling `/etc/resolv.conf` is the difference between a shell that can
    /// install a package and one that reports every mirror as unreachable.
    #[test]
    fn the_resolver_is_bound_at_the_path_the_symlink_points_at() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        std::fs::create_dir_all(base.join("etc")).unwrap();
        std::fs::create_dir_all(base.join("run/systemd/resolve")).unwrap();
        let real = base.join("run/systemd/resolve/stub-resolv.conf");
        std::fs::write(&real, b"nameserver 127.0.0.53\n").unwrap();
        std::os::unix::fs::symlink("../run/systemd/resolve/stub-resolv.conf", base.join("etc/resolv.conf"))
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

    /// `bwrap` applies its arguments in order, so a bind under `/run` placed
    /// before `--tmpfs /run` is mounted and then covered over. The symptom is
    /// every name lookup failing, which reads as "no network" rather than as an
    /// argv ordering bug.
    #[test]
    fn the_resolver_bind_comes_after_the_run_tmpfs() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        std::fs::create_dir_all(base.join("etc")).unwrap();
        std::fs::create_dir_all(base.join("run/systemd/resolve")).unwrap();
        std::fs::write(base.join("run/systemd/resolve/stub-resolv.conf"), b"nameserver 1.1.1.1\n")
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

    #[test]
    fn a_plain_resolver_file_is_bound_where_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        std::fs::create_dir_all(base.join("etc")).unwrap();
        std::fs::write(base.join("etc/resolv.conf"), b"nameserver 1.1.1.1\n").unwrap();
        let (_, dst) = resolv_bind(base).expect("no resolver bind");
        assert_eq!(dst, PathBuf::from("/etc/resolv.conf"));
    }

    #[test]
    fn a_base_with_no_resolver_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("etc")).unwrap();
        assert_eq!(resolv_bind(dir.path()), None);
    }

    #[test]
    fn subid_range_reads_the_users_allocation() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("subuid");
        std::fs::write(&f, "someone:100000:65536\nyou:524288:65536\n").unwrap();
        assert_eq!(subid_range(&f, "you", 1000), Some((524_288, 65_536)));
        assert_eq!(subid_range(&f, "nobody", 4242), None);
        assert_eq!(subid_range(Path::new("/nonexistent"), "you", 1000), None);
    }

    #[test]
    fn subid_range_matches_a_numeric_uid_line() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("subuid");
        std::fs::write(&f, "1000:524288:65536\n").unwrap();
        assert_eq!(subid_range(&f, "you", 1000), Some((524_288, 65_536)));
    }

    #[test]
    fn prepare_clears_a_stale_workdir() {
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
