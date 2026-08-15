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
            let _ = std::fs::remove_dir_all(&work);
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
pub fn build_argv(plan: &GuestPlan, seccomp_fd: Option<std::os::fd::RawFd>) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    let mut push = |s: &str| a.push(OsString::from(s));

    push("bwrap");

    // ---- namespaces --------------------------------------------------
    // A fresh user namespace is what makes every mount below possible without
    // privilege, and uid 0 inside it is what lets `rpm` chown a file into the
    // layer. Outside the namespace that uid 0 is the unprivileged real user; it
    // confers nothing on the host.
    push("--unshare-user");
    push("--uid");
    push("0");
    push("--gid");
    push("0");
    push("--unshare-pid");
    push("--unshare-uts");
    push("--unshare-ipc");
    push("--unshare-cgroup-try");
    push("--hostname");
    push(plan.profile.as_str());
    // The guest must not outlive the supervisor. Without this a crashed
    // supervisor leaves a shell running in a namespace nothing is watching.
    push("--die-with-parent");
    // Deliberately *not* `--new-session`. It calls `setsid`, which detaches the
    // guest from its controlling terminal and takes job control with it — no
    // Ctrl-C, no Ctrl-Z, no foreground process group. `bwrap` documents it as
    // incompatible with interactive use, and an interactive shell is the only
    // thing this runtime hosts. What it defends against is `TIOCSTI` injection
    // back into the parent's terminal, which the kernel has refused by default
    // since `dev.tty.legacy_tiocsti` shipped off.

    drop(push);

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

    // usrmerge. The overlay destinations are `/usr` and `/etc`, so the new root
    // has no `/bin` at all until these exist — and `execvp("/bin/sh")`, which
    // is what half the world's scripts start with, would fail.
    for (target, link) in [
        ("usr/bin", "/bin"),
        ("usr/sbin", "/sbin"),
        ("usr/lib", "/lib"),
        ("usr/lib64", "/lib64"),
    ] {
        a.push("--symlink".into());
        a.push(target.into());
        a.push(link.into());
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
    for dest in [plan.home_mountpoint.clone(), PathBuf::from("/root")] {
        a.push("--bind".into());
        a.push(plan.guest_home.clone().into_os_string());
        a.push(dest.into_os_string());
    }

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
    let argv = build_argv(plan, Some(fd));
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    cmd.exec()
}

/// Spawn the guest as a child.
pub fn spawn(plan: &GuestPlan) -> std::io::Result<std::process::Child> {
    let (_blob, fd) = open_seccomp_fd(&plan.seccomp_blob)?;
    let argv = build_argv(plan, Some(fd));
    std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .spawn()
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
        let argv = build_argv(&p, Some(9));
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
        let argv = strings(&build_argv(&p, Some(9)));
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
    fn the_usrmerge_symlinks_exist_so_bin_sh_resolves() {
        let argv = build_argv(&plan(), Some(9));
        let w3 = window(&argv, 3);
        for (target, link) in [
            ("usr/bin", "/bin"),
            ("usr/sbin", "/sbin"),
            ("usr/lib", "/lib"),
            ("usr/lib64", "/lib64"),
        ] {
            assert!(
                w3.iter()
                    .any(|w| w[0] == "--symlink" && w[1] == target && w[2] == link),
                "{link} -> {target} missing; /bin/sh would not exist in the guest"
            );
        }
    }

    #[test]
    fn the_guest_home_is_the_stores_copy_and_never_the_real_one() {
        let argv = build_argv(&plan(), Some(9));
        let w3 = window(&argv, 3);
        assert!(
            w3.iter().any(|w| w[0] == "--bind"
                && w[1] == "/store/home"
                && w[2] == "/var/home/you"),
            "the store's home must be bound over the real home path"
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
        let argv = build_argv(&p, Some(9));
        let w3 = window(&argv, 3);
        assert!(w3.iter().any(|w| w[0] == "--bind-try"
            && w[1] == "/var/home/you/src"
            && w[2] == "/var/home/you/src"));
        assert!(w3.iter().any(|w| w[0] == "--ro-bind-try"
            && w[1] == "/var/home/you/notes"
            && w[2] == "/var/home/you/notes"));
    }

    /// A guest with no grants must see no host directory beyond the base. This
    /// is "an app sees your files only where you said it may", asserted on the
    /// argv rather than on the prose.
    #[test]
    fn a_guest_with_no_grants_binds_no_host_directory() {
        let argv = strings(&build_argv(&plan(), Some(9)));
        let mut host_binds = Vec::new();
        for (i, arg) in argv.iter().enumerate() {
            if arg.starts_with("--bind") || arg.starts_with("--ro-bind") {
                host_binds.push(argv[i + 1].clone());
            }
        }
        for src in &host_binds {
            let allowed = src.starts_with("/store/")
                || src.starts_with("/usr/libexec/")
                || src.starts_with("/var/home/you/../"); // never taken; kept explicit
            assert!(
                allowed,
                "a guest with no grants binds {src}, which is neither the store \
                 nor the runtime binary"
            );
        }
    }

    #[test]
    fn the_seccomp_blob_is_applied_and_the_guest_argv_comes_last() {
        let argv = strings(&build_argv(&plan(), Some(9)));
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
        let argv = strings(&build_argv(&p, Some(9)));
        let sep = argv.iter().position(|a| a == "--").unwrap();
        assert_eq!(&argv[sep + 1..], &["/usr/bin/bash", "-c", "echo hi"]);
        assert!(
            !argv[sep..].contains(&"-i".to_string()),
            "`ush -c` must not open an interactive shell"
        );
    }

    #[test]
    fn the_runtime_commands_are_on_path_ahead_of_the_layer() {
        let argv = strings(&build_argv(&plan(), Some(9)));
        let i = argv.iter().position(|a| a == "PATH").unwrap();
        assert!(
            argv[i + 1].starts_with(&format!("{}:", crate::GUEST_BIN_DIR)),
            "PATH is {}; the runtime's own commands must come first",
            argv[i + 1]
        );
        let w3 = window(&build_argv(&plan(), Some(9)), 3);
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
        let argv = strings(&build_argv(&plan(), Some(9)));
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
        let argv = strings(&build_argv(&plan(), Some(9)));
        let i = argv.iter().position(|a| a == "--unsetenv").unwrap();
        assert_eq!(argv[i + 1], crate::store::STORE_ENV);
    }

    #[test]
    fn only_the_developer_profile_gets_dev_fuse() {
        let ush = strings(&build_argv(&plan(), Some(9)));
        assert!(!ush.iter().any(|a| a == "/dev/fuse"));

        let mut p = plan();
        p.profile = Profile::Dsh;
        let dsh = strings(&build_argv(&p, Some(9)));
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
        let argv = strings(&build_argv(&p, Some(9)));

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
