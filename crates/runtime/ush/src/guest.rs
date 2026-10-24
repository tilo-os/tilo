//! Building the guest: the root filesystem, the namespaces, and the argv
//! that makes them. Everything here is a pure function of a [`GuestPlan`]
//! until [`spawn`] is called, so confinement can be asserted on without a
//! kernel.
//!
//! `bwrap` performs the whole unshare/mount/`pivot_root`/seccomp/`exec`
//! sequence itself, with the filter landing after the last privileged
//! operation and before the guest exists — the supervisor never calls
//! `mount(2)`.
//!
//! ```text
//! /usr, /etc     read-only overlay: lowers = [tool images…, env-runtime]
//!                (/etc also carries the session's passwd, group and resolver
//!                on top); no writable upper anywhere
//! /bin /sbin /lib /lib64   symlinks into /usr, as on any usrmerge system
//! /home/<user>   the environment's home
//! <grants>       one bind per FsPath grant, at the same path as on the host
//! /tmp /run /proc /dev     fresh
//! /var           tmpfs, plus the environment's own /var/lib and /var/cache
//! /run/ush/bin   the runtime and its command shims, read-only
//! ```
//!
//! The guest runs as the real uid and gid — a single-id user namespace
//! mapping — so a file written to a granted directory belongs to the user.
//! It starts inside the session's own network namespace (see
//! [`tilo_egress`]), whose resolver is the supervisor's DNS stub; `bwrap`'s
//! user namespace nests inside the one that owns that network namespace, so
//! nothing in the guest has any capability over its network.
//! A path the guest was not granted is not missing a permission — it is not
//! in the namespace.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The subtrees the images provide. Everything else in the guest's root is
/// built fresh per session.
pub const IMAGE_SUBTREES: &[&str] = &["usr", "etc"];

/// The environment's own state under `/var`, bound from the store.
pub const ENVIRONMENT_VAR_DIRS: &[&str] = &["lib", "cache"];

/// `PATH` inside a guest. The runtime's own commands come first so `pkg`
/// means `pkg` even if a tool ships something by that name.
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

/// Who the guest runs as: the real user, by the same uid and gid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestUser {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
}

impl GuestUser {
    /// The user running this process.
    pub fn current() -> Self {
        let (uid, gid) = (
            rustix::process::geteuid().as_raw(),
            rustix::process::getegid().as_raw(),
        );
        let name = std::env::var("USER")
            .ok()
            .filter(|n| is_user_name(n))
            .unwrap_or_else(|| "user".to_owned());
        Self { name, uid, gid }
    }

    /// Where the guest's home is mounted.
    pub fn home(&self) -> PathBuf {
        PathBuf::from("/home").join(&self.name)
    }
}

/// A name safe to put in `passwd` and in a path: what `useradd` accepts.
fn is_user_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        && name != "."
        && name != ".."
}

/// Everything needed to build one guest.
#[derive(Debug, Clone)]
pub struct GuestPlan {
    /// Image roots stacked for `/usr` and `/etc`, topmost first: the tools,
    /// newest first, then the runtime last.
    pub images: Vec<PathBuf>,
    /// The machine's root: where the resolver configuration is read from and
    /// where the brokered management commands (`tilo`, `app`) live.
    pub host_root: PathBuf,
    /// Per-session scratch on the store: the generated `/etc` entries.
    pub session_dir: PathBuf,
    /// The environment's `/var` state; holds [`ENVIRONMENT_VAR_DIRS`].
    pub var_dir: PathBuf,
    /// The environment's home, mounted at [`GuestUser::home`].
    pub guest_home: PathBuf,
    pub user: GuestUser,
    /// The environment's name, for the prompt.
    pub environment: String,
    /// Host paths a grant may cover but the guest must never see — the
    /// runtime's own store and the environment records. Covered by a tmpfs
    /// after the grants are bound.
    pub masked: Vec<PathBuf>,
    pub trusted_dirs: Vec<PathBuf>,
    /// Directories granted read but not write.
    pub readable_dirs: Vec<PathBuf>,
    /// Where the granted directories' contents are, when not at their own
    /// paths: a grant of `/a/b` binds `<grant_root>/a/b` at `/a/b`.
    pub grant_root: Option<PathBuf>,
    /// The runtime binary, bound in so the guest's `pkg`/`perm`/… resolve.
    pub runtime_bin: PathBuf,
    /// Private loader closure for a dynamically linked runtime.
    pub helper_runtime: Option<crate::helper_runtime::HelperRuntime>,
    /// Installed legacy apps' commands: shims that ask the broker for a
    /// `LegacyLaunch`.
    pub legacy_commands: Vec<String>,
    pub broker_socket: PathBuf,
    /// The compiled seccomp blob.
    pub seccomp_blob: PathBuf,
    /// What to run. Empty means the interactive shell.
    pub command: Vec<String>,
    /// The shell to host when `command` is empty.
    pub shell: PathBuf,
    /// The key the shell marks its command runs with ([`crate::runs`]):
    /// its rc file then ends with the marks.
    pub runs: Option<String>,
    /// [`Self::write_shell_rc`] wrote the interactive `bash`'s rc file, which
    /// the guest then has at [`crate::shell_start::GUEST_RC`].
    pub shell_rc: bool,
    pub term: String,
    /// Extra environment, applied last.
    pub env: Vec<(String, String)>,
    /// The guest's `/etc/resolv.conf`: the session's DNS stub. `None` copies
    /// the machine's.
    pub resolver: Option<String>,
    /// Optional qualified native producer, never requested via a guest path.
    pub editor: Option<crate::editor::GuestEditor>,
}

impl GuestPlan {
    /// Where a granted directory's contents are on this machine.
    pub fn grant_source(&self, granted: &Path) -> PathBuf {
        match &self.grant_root {
            Some(root) => root.join(granted.strip_prefix("/").unwrap_or(granted)),
            None => granted.to_path_buf(),
        }
    }

    /// The generated tree stacked on top of the images' `/etc`.
    pub fn etc_overlay(&self) -> PathBuf {
        self.session_dir.join("root")
    }

    /// Write the rc file of the interactive shell, when it is `bash`: the
    /// native editor's prelude (when [`Self::editor`] is set), then what a
    /// login shell reads, then the marks when [`Self::runs`] names a key
    /// ([`crate::shell_start::bash_rc`]). The Host's shell starts the same way.
    ///
    /// # Errors
    ///
    /// Returns an error if the file can't be written.
    pub fn write_shell_rc(&mut self) -> std::io::Result<()> {
        if !crate::runs::is_bash(&self.shell) {
            return Ok(());
        }
        let prelude = self
            .editor
            .as_ref()
            .map_or("", |editor| editor.prelude.as_str());
        std::fs::create_dir_all(&self.session_dir)?;
        std::fs::write(
            self.session_dir.join("shell.bash"),
            crate::shell_start::bash_rc(prelude, self.runs.is_some()),
        )?;
        self.shell_rc = true;
        Ok(())
    }

    /// The interactive shell's argv: [`Self::shell`] as the session's login
    /// shell ([`crate::shell_start::interactive_args`]).
    #[must_use]
    pub fn shell_argv(&self) -> Vec<OsString> {
        let rc = self
            .shell_rc
            .then_some(Path::new(crate::shell_start::GUEST_RC));
        let mut argv = vec![self.shell.clone().into_os_string()];
        argv.extend(crate::shell_start::interactive_args(&self.shell, rc));
        argv
    }

    /// Create what the plan refers to: the home, the environment's `/var`
    /// state, and the session's own `/etc` entries — `passwd` and `group`
    /// naming the real user (the images know only their system accounts) and
    /// the resolver ([`Self::resolver`], else the machine's).
    ///
    /// # Errors
    ///
    /// Returns an error if a directory or file can't be created.
    pub fn prepare(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.session_dir)?;
        if let Some(helper) = &self.helper_runtime {
            helper.write_wrapper(&self.session_dir.join("helper-shim"))?;
        }
        std::fs::create_dir_all(&self.guest_home)?;
        for dir in ENVIRONMENT_VAR_DIRS {
            std::fs::create_dir_all(self.var_dir.join(dir))?;
        }
        let etc = self.etc_overlay().join("etc");
        std::fs::create_dir_all(&etc)?;
        let image_file = |name: &str| {
            self.images
                .iter()
                .map(|image| image.join("etc").join(name))
                .find(|path| path.is_file())
                .and_then(|path| std::fs::read_to_string(path).ok())
                .unwrap_or_default()
        };
        std::fs::write(
            etc.join("passwd"),
            passwd_with_user(&image_file("passwd"), &self.user, &self.shell),
        )?;
        std::fs::write(
            etc.join("group"),
            group_with_user(&image_file("group"), &self.user),
        )?;
        if let Some(resolver) = self
            .resolver
            .clone()
            .or_else(|| host_resolver(&self.host_root))
        {
            std::fs::write(etc.join("resolv.conf"), resolver)?;
        }
        Ok(())
    }
}

/// The images' `passwd` with the real user added, replacing any entry that
/// claims the same name or uid.
pub fn passwd_with_user(image: &str, user: &GuestUser, shell: &Path) -> String {
    let mut out = without_claims(image, &user.name, user.uid);
    out.push_str(&user.name);
    out.push_str(":x:");
    out.push_str(&user.uid.to_string());
    out.push(':');
    out.push_str(&user.gid.to_string());
    out.push_str("::");
    out.push_str(&user.home().to_string_lossy());
    out.push(':');
    out.push_str(&shell.to_string_lossy());
    out.push('\n');
    out
}

/// The lines of a `passwd`/`group` file that claim neither `name` nor `id`
/// (the third field), each newline-terminated.
fn without_claims(file: &str, name: &str, id: u32) -> String {
    let mut out = String::new();
    for line in file.lines() {
        let mut fields = line.split(':');
        let claimed_name = fields.next().unwrap_or_default();
        let claimed_id = fields.nth(1).and_then(|u| u.parse::<u32>().ok());
        if claimed_name != name && claimed_id != Some(id) {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// The images' `group` with the user's primary group added.
pub fn group_with_user(image: &str, user: &GuestUser) -> String {
    let mut out = without_claims(image, &user.name, user.gid);
    out.push_str(&user.name);
    out.push_str(":x:");
    out.push_str(&user.gid.to_string());
    out.push_str(":\n");
    out
}

/// The machine's resolver configuration, read through `/etc/resolv.conf`
/// (on a systemd-resolved host a link into `/run`). It describes the network
/// the machine is on and carries no user content. `None` on a machine
/// without one.
pub fn host_resolver(host_root: &Path) -> Option<String> {
    std::fs::read_to_string(host_root.join("etc/resolv.conf")).ok()
}

/// Build the `bwrap` argv for a plan. Pure: the vector starts with `bwrap`
/// and ends with the guest's own argv, so `-v` output pastes into a shell.
///
/// Each stage of the argv gets its own helper below, in the order `bwrap`
/// applies flags — that ordering is load-bearing (the `/var` state binds
/// land on the `/var` tmpfs, the masks after the grants they cover), so keep
/// the call order matching the section order in this function.
pub fn build_argv(plan: &GuestPlan, seccomp_fd: Option<std::os::fd::RawFd>) -> Vec<OsString> {
    build_guest_argv(plan, seccomp_fd, None)
}

/// Build a broker-free, network-isolated check guest using the same image,
/// namespace, seccomp and helper closure as ordinary environment guests.
/// The trusted supervisor supplies both paths; neither comes from an effect.
#[must_use]
pub fn build_check_argv(
    plan: &GuestPlan,
    seccomp_fd: std::os::fd::RawFd,
    checkout: &Path,
    build: &Path,
) -> Vec<OsString> {
    build_guest_argv(plan, Some(seccomp_fd), Some((checkout, build)))
}

fn build_guest_argv(
    plan: &GuestPlan,
    seccomp_fd: Option<std::os::fd::RawFd>,
    check: Option<(&Path, &Path)>,
) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    push_namespace_flags(&mut a, plan);
    if check.is_some() {
        a.push("--unshare-net".into());
    }
    push_image_mounts(&mut a, plan);
    push_symlinks(&mut a);
    push_pseudo_filesystems_and_var(&mut a, plan);
    push_runtime_and_broker(&mut a, plan, check.is_none());
    push_shell_rc(&mut a, plan);
    push_home_and_grants(&mut a, plan);
    if let Some((checkout, build)) = check {
        a.extend([
            "--ro-bind".into(),
            checkout.as_os_str().into(),
            "/project".into(),
            "--bind".into(),
            build.as_os_str().into(),
            "/project/target".into(),
            "--chdir".into(),
            "/project".into(),
        ]);
    }
    push_environment(&mut a, plan, check.is_none());
    push_seccomp_and_command(&mut a, plan, seccomp_fd);
    a
}

/// `--clearenv` and the namespace/identity flags — the part of the argv
/// that exists before any filesystem is touched.
fn push_namespace_flags(a: &mut Vec<OsString>, plan: &GuestPlan) {
    let push = |a: &mut Vec<OsString>, s: &str| a.push(OsString::from(s));

    push(a, "bwrap");
    // `bwrap` hands the guest the parent's whole environment unless told
    // otherwise; clearing it first is what makes the environment block below a
    // complete list rather than overrides on the supervisor's own environment.
    push(a, "--clearenv");

    // A fresh user namespace makes the mounts below possible without privilege.
    // It maps exactly one id, the real one, so the guest is the user inside and
    // out: a file it writes to a granted directory is the user's, and nothing
    // in it is root.
    push(a, "--unshare-user");
    push(a, "--uid");
    a.push(plan.user.uid.to_string().into());
    push(a, "--gid");
    a.push(plan.user.gid.to_string().into());
    push(a, "--unshare-pid");
    push(a, "--unshare-uts");
    push(a, "--unshare-ipc");
    push(a, "--unshare-cgroup-try");
    // The guest keeps the host's name in a namespace of its own: naming it
    // takes `sethostname`, which a container's seccomp profile refuses, and
    // bwrap then starts no shell at all.
    // The guest must not outlive the supervisor. Without this a crashed
    // supervisor leaves a shell running in a namespace nothing is watching.
    push(a, "--die-with-parent");
    // Deliberately *not* `--new-session`: it calls `setsid`, detaching the guest
    // from its controlling terminal and taking job control with it (no Ctrl-C,
    // no Ctrl-Z). `bwrap` documents it as incompatible with interactive use; what
    // it defends against — `TIOCSTI` injection — the kernel already refuses.
}

/// The read-only lowers for one subtree, topmost first.
pub fn subtree_lowers(plan: &GuestPlan, subtree: &str) -> Vec<PathBuf> {
    let generated = (subtree == "etc").then(|| plan.etc_overlay().join("etc"));
    generated
        .into_iter()
        .chain(plan.images.iter().map(|image| image.join(subtree)))
        .filter(|dir| dir.is_dir())
        .collect()
}

/// `/usr` and `/etc`: the images stacked read-only. Overlayfs needs two
/// lowers to mount without an upper, so a single tree is bound read-only
/// instead — the same view.
fn push_image_mounts(a: &mut Vec<OsString>, plan: &GuestPlan) {
    for subtree in IMAGE_SUBTREES {
        let dest = PathBuf::from("/").join(subtree).into_os_string();
        match subtree_lowers(plan, subtree).as_slice() {
            [] => {}
            [only] => {
                a.push("--ro-bind".into());
                a.push(only.clone().into_os_string());
                a.push(dest);
            }
            lowers => {
                // `--overlay-src` entries accumulate for the next overlay with
                // the first named at the *bottom* — the reverse of the
                // kernel's `lowerdir` — so the runtime goes first.
                for lower in lowers.iter().rev() {
                    a.push("--overlay-src".into());
                    a.push(lower.clone().into_os_string());
                }
                a.push("--ro-overlay".into());
                a.push(dest);
            }
        }
    }
}

/// The usrmerge links every image-built root needs, and `/var/run` and
/// `/var/lock` into `/run` (they are symlinks there on every modern
/// distribution, and a program that opens a path under one gets `ENOENT`
/// rather than a clear error when missing).
pub fn root_symlinks() -> Vec<(PathBuf, PathBuf)> {
    [
        ("usr/bin", "/bin"),
        ("usr/sbin", "/sbin"),
        ("usr/lib", "/lib"),
        ("usr/lib64", "/lib64"),
        ("../run", "/var/run"),
        ("../run/lock", "/var/lock"),
    ]
    .into_iter()
    .map(|(target, link)| (PathBuf::from(target), PathBuf::from(link)))
    .collect()
}

fn push_symlinks(a: &mut Vec<OsString>) {
    // `/var` is a tmpfs mounted below; its links are created after it.
    for (target, link) in root_symlinks()
        .into_iter()
        .filter(|(_, link)| !link.starts_with("/var"))
    {
        a.push("--symlink".into());
        a.push(target.into_os_string());
        a.push(link.into_os_string());
    }
}

/// `/proc`, `/dev`, the fresh tmpfs mounts, and the environment's own `/var`
/// state on top of the `/var` tmpfs.
fn push_pseudo_filesystems_and_var(a: &mut Vec<OsString>, plan: &GuestPlan) {
    for (flag, dest) in [
        ("--proc", "/proc"),
        ("--dev", "/dev"),
        ("--tmpfs", "/tmp"),
        ("--tmpfs", "/run"),
        ("--tmpfs", "/var"),
    ] {
        a.push(flag.into());
        a.push(dest.into());
    }
    for (target, link) in root_symlinks()
        .into_iter()
        .filter(|(_, link)| link.starts_with("/var"))
    {
        a.push("--symlink".into());
        a.push(target.into_os_string());
        a.push(link.into_os_string());
    }
    for dir in ENVIRONMENT_VAR_DIRS {
        a.push("--bind".into());
        a.push(plan.var_dir.join(dir).into_os_string());
        a.push(PathBuf::from("/var").join(dir).into_os_string());
    }
}

/// The runtime binary (read-only, with its command shims) and the broker
/// socket the guest sends requests over.
fn push_runtime_and_broker(a: &mut Vec<OsString>, plan: &GuestPlan, broker: bool) {
    if let Some(editor) = &plan.editor {
        a.extend([
            "--dir".into(),
            crate::editor::GUEST_DIR.into(),
            "--ro-bind".into(),
            editor.module.clone().into_os_string(),
            format!("{}/producer.so", crate::editor::GUEST_DIR).into(),
        ]);
    }
    a.push("--dir".into());
    a.push(crate::GUEST_BIN_DIR.into());
    a.push("--ro-bind".into());
    a.push(plan.runtime_bin.clone().into_os_string());
    a.push(if plan.helper_runtime.is_some() {
        crate::helper_runtime::EXECUTABLE.into()
    } else {
        format!("{}/tilo-ush", crate::GUEST_BIN_DIR).into()
    });
    if let Some(helper) = &plan.helper_runtime {
        a.extend(["--dir".into(), crate::helper_runtime::LIB_DIR.into()]);
        for (name, path) in &helper.libraries {
            a.extend([
                "--ro-bind".into(),
                path.clone().into_os_string(),
                Path::new(crate::helper_runtime::LIB_DIR)
                    .join(name)
                    .into_os_string(),
            ]);
        }
        a.extend([
            "--ro-bind".into(),
            helper.loader.clone().into_os_string(),
            crate::helper_runtime::LOADER.into(),
        ]);
        a.extend([
            "--ro-bind".into(),
            plan.session_dir.join("helper-shim").into_os_string(),
            format!("{}/tilo-ush", crate::GUEST_BIN_DIR).into(),
        ]);
    }
    if !broker {
        return;
    }
    for cmd in crate::GUEST_COMMANDS {
        if !crate::guest_command_available(&plan.host_root, cmd) {
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

/// The interactive shell's rc file, read-only at
/// [`crate::shell_start::GUEST_RC`].
fn push_shell_rc(a: &mut Vec<OsString>, plan: &GuestPlan) {
    if !plan.shell_rc {
        return;
    }
    a.push("--ro-bind".into());
    a.push(plan.session_dir.join("shell.bash").into_os_string());
    a.push(crate::shell_start::GUEST_RC.into());
}

/// The environment's home, then the grants at their host paths, then the
/// masks over anything a grant would otherwise reveal of the runtime.
fn push_home_and_grants(a: &mut Vec<OsString>, plan: &GuestPlan) {
    let home = plan.user.home().into_os_string();
    a.push("--dir".into());
    a.push(home.clone());
    a.push("--bind".into());
    a.push(plan.guest_home.clone().into_os_string());
    a.push(home);

    for dir in &plan.readable_dirs {
        a.push("--ro-bind-try".into());
        a.push(plan.grant_source(dir).into_os_string());
        a.push(dir.clone().into_os_string());
    }
    for dir in &plan.trusted_dirs {
        a.push("--bind-try".into());
        a.push(plan.grant_source(dir).into_os_string());
        a.push(dir.clone().into_os_string());
    }
    // After the grants, so a grant of a directory that contains the store (the
    // whole home, say) still cannot reach the store: other environments' homes,
    // the audit log, and the records that decide what the next session sees.
    for masked in &plan.masked {
        let covered: Vec<PathBuf> = plan
            .trusted_dirs
            .iter()
            .chain(&plan.readable_dirs)
            .filter_map(|granted| {
                let inside = masked.strip_prefix(plan.grant_source(granted)).ok()?;
                Some(if inside.as_os_str().is_empty() {
                    granted.clone()
                } else {
                    granted.join(inside)
                })
            })
            .collect();
        for guest_path in covered {
            let already = a.windows(2).any(|w| {
                matches!(w, [flag, covered] if flag == "--tmpfs" && covered == guest_path.as_os_str())
            });
            if !already {
                a.push("--tmpfs".into());
                a.push(guest_path.into_os_string());
            }
        }
    }
}

/// The guest's whole environment: fixed variables, the locale/timezone
/// carried from the supervisor, the plan's own extras, and finally
/// unsetting the store path so nothing inside the namespace can reach it.
fn push_environment(a: &mut Vec<OsString>, plan: &GuestPlan, broker: bool) {
    let setenv = |k: &str, v: &str, a: &mut Vec<OsString>| {
        a.push("--setenv".into());
        a.push(k.into());
        a.push(v.into());
    };
    setenv("PATH", GUEST_PATH, a);
    setenv("HOME", &plan.user.home().to_string_lossy(), a);
    setenv("TERM", &plan.term, a);
    setenv("USER", &plan.user.name, a);
    setenv("LOGNAME", &plan.user.name, a);
    setenv("SHELL", &plan.shell.to_string_lossy(), a);
    setenv("TILO_USH_ENVIRONMENT", &plan.environment, a);
    if let Some(key) = &plan.runs {
        setenv(crate::runs::KEY_ENV, key, a);
    }
    if broker {
        setenv("TILO_USH_BROKER", crate::GUEST_BROKER_SOCK, a);
    }
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
    // Applied by `bwrap` immediately before it execs, after every mount above —
    // this is what stops the supervisor's need for `mount`/`unshare` from
    // reaching anything the user runs. `bwrap` takes an already-open
    // descriptor rather than a path (see [`open_seccomp_fd`]).
    if let Some(fd) = seccomp_fd {
        a.push("--seccomp".into());
        a.push(fd.to_string().into());
    }

    a.push("--".into());
    if plan.command.is_empty() {
        a.extend(plan.shell_argv());
    } else {
        for arg in &plan.command {
            a.push(arg.into());
        }
    }
}

/// `remove_dir_all`, but restoring traversal and write permission as it
/// descends: a tool may leave read-only directories in its home or `/var`,
/// and an ordinary recursive delete stops there with "permission denied".
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

/// Spawn the guest as a child, inside `netns` when given (`bwrap` then
/// nests its user namespace in the one that owns it).
///
/// # Errors
///
/// Returns an error if the seccomp filter can't be opened, or the child
/// process can't be spawned.
pub fn spawn(
    plan: &GuestPlan,
    netns: Option<&tilo_egress::Netns>,
) -> std::io::Result<std::process::Child> {
    let (_blob, fd) = open_seccomp_fd(&plan.seccomp_blob)?;
    let argv = build_argv(plan, Some(fd));
    // `build_argv` always pushes "bwrap" first, so this never actually
    // misses, but the return type has no way to say that.
    let Some((program, rest)) = argv.split_first() else {
        return Err(std::io::Error::other("build_argv produced an empty argv"));
    };
    let mut cmd = std::process::Command::new(program);
    cmd.args(rest);
    // Direct `ush -c` must expose its command output and startup errors;
    // served sessions inherit the supervisor's already-attached pty here.
    cmd.stdout(std::process::Stdio::inherit());
    cmd.stderr(std::process::Stdio::inherit());
    if let Some(netns) = netns {
        netns.enter(&mut cmd);
    }
    if let Some(editor) = &plan.editor {
        use std::os::fd::{AsFd, AsRawFd};
        use std::os::unix::process::CommandExt;
        let fd = editor.producer.as_fd().as_raw_fd();
        // SAFETY: only the qualified producer endpoint crosses this trusted exec.
        unsafe {
            cmd.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    cmd.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approved_check_mounts_source_read_only_target_writeable_and_no_broker() {
        let dir = tempfile::tempdir().unwrap();
        let plan = plan_in(dir.path());
        let argv = build_check_argv(
            &plan,
            42,
            Path::new("/private/source"),
            Path::new("/private/build"),
        );
        assert!(argv.iter().any(|s| s == "--unshare-net"));
        assert!(
            argv.windows(3)
                .any(|w| w == ["--ro-bind", "/private/source", "/project"])
        );
        assert!(
            argv.windows(3)
                .any(|w| w == ["--bind", "/private/build", "/project/target"])
        );
        assert!(argv.windows(2).any(|w| w == ["--seccomp", "42"]));
        assert!(!argv.iter().any(|s| s == crate::GUEST_BROKER_SOCK));
        assert!(!argv.iter().any(|s| s == plan.broker_socket.as_os_str()));
    }

    #[test]
    fn native_editor_mounts_remain_read_only_with_broker_free_check_guests() {
        let dir = tempfile::tempdir().unwrap();
        let mut plan = plan_in(dir.path());
        let (_host, producer) = runtime_portal::editor::Socket::pair().unwrap();
        let module = dir.path().join("producer.so");
        plan.editor = Some(crate::editor::GuestEditor {
            producer: std::sync::Arc::new(producer),
            module: module.clone(),
            prelude: String::new(),
        });
        let ordinary = build_argv(&plan, Some(42));
        let check = build_check_argv(
            &plan,
            42,
            Path::new("/private/source"),
            Path::new("/private/build"),
        );
        for argv in [&ordinary, &check] {
            assert!(argv.windows(3).any(|w| {
                w[0] == "--ro-bind"
                    && w[1] == module.as_os_str()
                    && w[2] == "/run/ush-editor/producer.so"
            }));
        }
        assert!(ordinary.iter().any(|s| s == crate::GUEST_BROKER_SOCK));
        assert!(!check.iter().any(|s| s == crate::GUEST_BROKER_SOCK));
        assert!(!check.iter().any(|s| s == plan.broker_socket.as_os_str()));
    }

    /// A guest's `bash` starts as the Host's does: on one rc file that runs
    /// the native editor's prelude, then a login shell's startup files, then
    /// the marks, mounted read-only where the argv names it.
    #[test]
    fn a_guest_bash_starts_on_the_same_rc_as_the_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let mut plan = plan_in(dir.path());
        let (_host, producer) = runtime_portal::editor::Socket::pair().unwrap();
        plan.editor = Some(crate::editor::GuestEditor {
            producer: std::sync::Arc::new(producer),
            module: dir.path().join("producer.so"),
            prelude: "enable tilo_editable\n".into(),
        });
        plan.runs = Some("k3y0fth1ss3ss10n".into());
        plan.write_shell_rc().unwrap();
        let rc = std::fs::read_to_string(plan.session_dir.join("shell.bash")).unwrap();
        assert_eq!(
            rc,
            crate::shell_start::bash_rc("enable tilo_editable\n", true)
        );
        let argv = strings(&build_argv(&plan, Some(9)));
        let sep = argv.iter().position(|a| a == "--").unwrap();
        assert_eq!(
            &argv[sep + 1..],
            &[
                "/usr/bin/bash",
                "--rcfile",
                crate::shell_start::GUEST_RC,
                "-i"
            ]
        );
        let rc_path = plan.session_dir.join("shell.bash").display().to_string();
        assert!(argv.windows(3).any(|w| {
            w[0] == "--ro-bind" && w[1] == rc_path && w[2] == crate::shell_start::GUEST_RC
        }));
    }

    fn plan_in(dir: &Path) -> GuestPlan {
        let tool = dir.join("pkgs/tool");
        let runtime = dir.join("pkgs/runtime");
        for tree in [&tool, &runtime] {
            std::fs::create_dir_all(tree.join("usr/bin")).unwrap();
        }
        std::fs::create_dir_all(runtime.join("etc")).unwrap();
        std::fs::write(
            runtime.join("etc/passwd"),
            "root:x:0:0:root:/root:/bin/bash\nyou:x:4242:4242::/nowhere:/bin/sh\n",
        )
        .unwrap();
        std::fs::write(runtime.join("etc/group"), "root:x:0:\nwheel:x:10:\n").unwrap();
        GuestPlan {
            images: vec![tool, runtime],
            host_root: dir.join("host"),
            session_dir: dir.join("store/sessions/s1"),
            var_dir: dir.join("store/environments/default/var"),
            guest_home: dir.join("store/environments/default/home"),
            user: GuestUser {
                name: "you".into(),
                uid: 1000,
                gid: 1000,
            },
            environment: "Default".into(),
            masked: vec![dir.join("store")],
            trusted_dirs: Vec::new(),
            readable_dirs: Vec::new(),
            grant_root: None,
            runtime_bin: PathBuf::from("/usr/libexec/tilo-ush"),
            helper_runtime: None,
            legacy_commands: vec!["code".into()],
            broker_socket: dir.join("store/sessions/s1/broker.sock"),
            seccomp_blob: dir.join("store/sessions/s1/ush.bpf"),
            command: Vec::new(),
            shell: PathBuf::from("/usr/bin/bash"),
            runs: None,
            shell_rc: false,
            term: "xterm-256color".into(),
            env: Vec::new(),
            resolver: None,
            editor: None,
        }
    }

    fn strings(argv: &[OsString]) -> Vec<String> {
        argv.iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }

    fn windows(argv: &[OsString], n: usize) -> Vec<Vec<String>> {
        strings(argv).windows(n).map(<[String]>::to_vec).collect()
    }

    fn path(p: &Path) -> String {
        p.display().to_string()
    }

    #[test]
    fn the_guest_has_its_own_uts_namespace_without_renaming_it() {
        let dir = tempfile::tempdir().unwrap();
        let argv = strings(&build_argv(&plan_in(dir.path()), Some(9)));
        assert!(argv.iter().any(|arg| arg == "--unshare-uts"));
        assert!(
            !argv.iter().any(|arg| arg == "--hostname"),
            "a container's seccomp profile refuses sethostname"
        );
    }

    #[test]
    fn installed_legacy_apps_get_a_shim_on_the_guest_path() {
        let dir = tempfile::tempdir().unwrap();
        let p = plan_in(dir.path());
        let shims = windows(&build_argv(&p, Some(9)), 3);
        assert!(
            shims.iter().any(|w| w[0] == "--symlink"
                && w[1] == "tilo-ush"
                && w[2] == format!("{}/code", crate::GUEST_BIN_DIR)),
            "an installed legacy app has no shim in the guest"
        );
    }

    #[test]
    fn the_images_stack_read_only_with_no_writable_upper() {
        let dir = tempfile::tempdir().unwrap();
        let p = plan_in(dir.path());
        p.prepare().unwrap();
        let argv = strings(&build_argv(&p, Some(9)));

        // `/usr`: the tool over the runtime, mounted read-only. `bwrap` takes
        // the bottom layer first, so the runtime is named before the tool.
        let usr = argv
            .windows(2)
            .position(|w| w[0] == "--ro-overlay" && w[1] == "/usr")
            .expect("/usr is not a read-only overlay");
        let srcs: Vec<&String> = argv[..usr]
            .windows(2)
            .filter(|w| w[0] == "--overlay-src")
            .map(|w| &w[1])
            .collect();
        assert_eq!(
            srcs,
            vec![
                &path(&p.images[1].join("usr")),
                &path(&p.images[0].join("usr"))
            ]
        );
        // `/etc`: the session's generated entries over the runtime's.
        assert!(
            argv.windows(2)
                .any(|w| w[0] == "--ro-overlay" && w[1] == "/etc")
        );
        assert!(argv.contains(&path(&p.etc_overlay().join("etc"))));

        // The property the design rests on: nothing is writable below an
        // image, so a guest can add to its environment only through the record.
        assert!(
            !argv
                .iter()
                .any(|a| a == "--overlay" || a == "--tmp-overlay"),
            "a writable overlay upper crept in: {argv:?}"
        );
        for (i, arg) in argv.iter().enumerate() {
            if arg == "--bind" || arg == "--bind-try" || arg == "--dev-bind" {
                let src = Path::new(&argv[i + 1]);
                assert!(
                    !p.images.iter().any(|image| src.starts_with(image)),
                    "{arg} {} would make an image writable",
                    src.display()
                );
            }
        }
    }

    #[test]
    fn a_single_tree_is_bound_read_only_since_overlayfs_needs_two() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = plan_in(dir.path());
        p.images.remove(0);
        let argv = windows(&build_argv(&p, Some(9)), 3);
        assert!(
            argv.iter().any(|w| w[0] == "--ro-bind"
                && w[1] == path(&p.images[0].join("usr"))
                && w[2] == "/usr"),
            "{argv:?}"
        );
    }

    #[test]
    fn the_guest_is_the_real_user_by_a_single_id_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let p = plan_in(dir.path());
        let argv = strings(&build_argv(&p, Some(9)));
        let uid = argv.iter().position(|a| a == "--uid").unwrap();
        let gid = argv.iter().position(|a| a == "--gid").unwrap();
        assert_eq!(argv[uid + 1], "1000", "the guest must not be uid 0");
        assert_eq!(argv[gid + 1], "1000");
        assert!(argv.contains(&"--unshare-user".to_string()));
        assert!(
            !argv.iter().any(|a| a == "--userns" || a == "--cap-add"),
            "no prepared namespace and no capabilities: {argv:?}"
        );
        let w3 = windows(&build_argv(&p, Some(9)), 3);
        assert!(
            w3.iter()
                .any(|w| w[0] == "--setenv" && w[1] == "USER" && w[2] == "you")
        );
        assert!(
            w3.iter()
                .any(|w| w[0] == "--setenv" && w[1] == "HOME" && w[2] == "/home/you")
        );

        // passwd names the user at its real uid, replacing the image's claim on
        // the name; group gains the primary group.
        p.prepare().unwrap();
        let passwd = std::fs::read_to_string(p.etc_overlay().join("etc/passwd")).unwrap();
        assert!(passwd.contains("root:x:0:0:"), "{passwd}");
        assert!(
            passwd.contains("you:x:1000:1000::/home/you:/usr/bin/bash"),
            "{passwd}"
        );
        assert!(!passwd.contains("4242"), "{passwd}");
        let group = std::fs::read_to_string(p.etc_overlay().join("etc/group")).unwrap();
        assert!(
            group.contains("wheel:x:10:") && group.contains("you:x:1000:"),
            "{group}"
        );
    }

    #[test]
    fn home_var_and_grants_are_bound_and_the_store_stays_covered() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = plan_in(dir.path());
        let argv = windows(&build_argv(&p, Some(9)), 3);
        assert!(
            argv.iter()
                .any(|w| w[0] == "--bind" && w[1] == path(&p.guest_home) && w[2] == "/home/you")
        );
        for sub in ENVIRONMENT_VAR_DIRS {
            assert!(argv.iter().any(|w| w[0] == "--bind"
                && w[1] == path(&p.var_dir.join(sub))
                && w[2] == format!("/var/{sub}")));
        }
        let flat = strings(&build_argv(&p, Some(9)));
        let var_tmpfs = flat
            .windows(2)
            .position(|w| w[0] == "--tmpfs" && w[1] == "/var")
            .unwrap();
        let var_lib = flat.iter().position(|a| a == "/var/lib").unwrap();
        assert!(
            var_lib > var_tmpfs,
            "the state bind would be covered by the tmpfs"
        );
        assert!(
            !flat
                .windows(2)
                .any(|w| w[0] == "--tmpfs" && w[1] == path(&p.masked[0])),
            "with no grant there is nothing to cover"
        );

        // A grant is bound at its host path; one covering the store gets the
        // store masked after it.
        p.trusted_dirs = vec![dir.path().to_path_buf()];
        p.readable_dirs = vec![PathBuf::from("/srv/notes")];
        let flat = strings(&build_argv(&p, Some(9)));
        let w3 = windows(&build_argv(&p, Some(9)), 3);
        assert!(
            w3.iter().any(|w| w[0] == "--bind-try"
                && w[1] == path(dir.path())
                && w[2] == path(dir.path()))
        );
        assert!(
            w3.iter()
                .any(|w| w[0] == "--ro-bind-try" && w[1] == "/srv/notes" && w[2] == "/srv/notes")
        );
        let grant = flat.iter().position(|a| a == "--bind-try").unwrap();
        let mask = flat
            .windows(2)
            .position(|w| w[0] == "--tmpfs" && w[1] == path(&p.masked[0]))
            .expect("the store must be covered under a grant that contains it");
        assert!(mask > grant);
    }

    #[test]
    fn with_a_grant_root_a_grant_binds_its_contents_there_at_its_own_path() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = plan_in(dir.path());
        let files = dir.path().join("member/files");
        p.grant_root = Some(files.clone());
        p.trusted_dirs = vec![PathBuf::from("/home/alice/src")];
        // The member's store sits inside a granted source: it is covered at
        // the path the guest sees it by.
        p.masked = vec![files.join("home/alice/src/.store")];
        let w3 = windows(&build_argv(&p, Some(9)), 3);
        assert!(w3.iter().any(|w| w[0] == "--bind-try"
            && w[1] == path(&files.join("home/alice/src"))
            && w[2] == "/home/alice/src"));
        let flat = strings(&build_argv(&p, Some(9)));
        assert!(
            flat.windows(2)
                .any(|w| w[0] == "--tmpfs" && w[1] == "/home/alice/src/.store")
        );
    }

    #[test]
    fn the_command_line_ends_with_seccomp_and_the_guest_argv() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = plan_in(dir.path());
        let argv = strings(&build_argv(&p, Some(9)));
        let sep = argv.iter().position(|a| a == "--").unwrap();
        let blob = argv.iter().position(|a| a == "--seccomp").unwrap();
        assert!(blob < sep);
        assert_eq!(argv[blob + 1], "9");
        assert_eq!(&argv[sep + 1..], &["/usr/bin/bash", "-l"]);

        p.command = vec!["/usr/bin/bash".into(), "-c".into(), "echo hi".into()];
        let argv = strings(&build_argv(&p, Some(9)));
        let sep = argv.iter().position(|a| a == "--").unwrap();
        assert_eq!(&argv[sep + 1..], &["/usr/bin/bash", "-c", "echo hi"]);

        let path_at = argv.iter().position(|a| a == "PATH").unwrap();
        assert!(argv[path_at + 1].starts_with(&format!("{}:", crate::GUEST_BIN_DIR)));
        let rt = argv
            .iter()
            .position(|a| a == "/usr/libexec/tilo-ush")
            .unwrap();
        assert_eq!(
            argv[rt - 1],
            "--ro-bind",
            "the runtime binary must be read-only"
        );
        let unset = argv.iter().position(|a| a == "--unsetenv").unwrap();
        assert_eq!(argv[unset + 1], crate::store::STORE_ENV);
    }

    #[test]
    fn the_root_carries_usrmerge_and_var_links_after_the_var_tmpfs() {
        let dir = tempfile::tempdir().unwrap();
        let argv = strings(&build_argv(&plan_in(dir.path()), Some(9)));
        for (target, link) in root_symlinks() {
            let at = argv
                .windows(3)
                .position(|w| w[0] == "--symlink" && w[1] == path(&target) && w[2] == path(&link))
                .unwrap_or_else(|| panic!("{} -> {} missing", link.display(), target.display()));
            if link.starts_with("/var") {
                let var = argv
                    .windows(2)
                    .position(|w| w[0] == "--tmpfs" && w[1] == "/var")
                    .unwrap();
                assert!(
                    at > var,
                    "{} would be covered by the /var tmpfs",
                    link.display()
                );
            }
        }
    }

    #[test]
    fn the_resolver_is_copied_from_the_host() {
        let dir = tempfile::tempdir().unwrap();
        let p = plan_in(dir.path());
        std::fs::create_dir_all(p.host_root.join("etc")).unwrap();
        std::fs::write(
            p.host_root.join("etc/resolv.conf"),
            "nameserver 127.0.0.53\n",
        )
        .unwrap();
        p.prepare().unwrap();
        assert_eq!(
            std::fs::read_to_string(p.etc_overlay().join("etc/resolv.conf")).unwrap(),
            "nameserver 127.0.0.53\n"
        );
    }

    #[test]
    fn the_sessions_resolver_replaces_the_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = plan_in(dir.path());
        std::fs::create_dir_all(p.host_root.join("etc")).unwrap();
        std::fs::write(p.host_root.join("etc/resolv.conf"), "nameserver 10.0.0.1\n").unwrap();
        p.resolver = Some(tilo_egress::RESOLV_CONF.to_owned());
        p.prepare().unwrap();
        let written = std::fs::read_to_string(p.etc_overlay().join("etc/resolv.conf")).unwrap();
        assert!(written.contains("nameserver 127.0.0.1"), "{written}");
        assert!(!written.contains("10.0.0.1"), "{written}");
    }

    #[test]
    fn user_names_that_are_not_safe_fall_back() {
        assert!(is_user_name("quinten"));
        for bad in ["", "..", "a/b", "-x", "a b"] {
            assert!(!is_user_name(bad), "{bad:?}");
        }
    }
}
