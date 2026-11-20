use std::fs::File;
use std::io::{Seek, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::process::{Command, Stdio};

use rustix::fs::MemfdFlags;
use thiserror::Error;

use crate::CapsuleCommandPlan;

pub const SECCOMP_CHILD_FD: i32 = 32;
pub(crate) const PREFIX_CHILD_FD: i32 = 33;
pub(crate) const HOME_CHILD_FD: i32 = 34;
pub(crate) const PASSWD_CHILD_FD: i32 = 36;
pub(crate) const GROUP_CHILD_FD: i32 = 37;
/// Where a Windows appliance presentation reads the RDP password from.
pub(crate) const PASSWORD_CHILD_FD: i32 = 38;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionOutcome {
    pub success: bool,
    pub exit_code: Option<i32>,
    /// The last of what a program that failed by itself wrote on its error stream, when it wrote
    /// anything of its own: what the person is told beside its status ([`last_words`]).
    pub said: Option<String>,
}

impl ExecutionOutcome {
    /// A program that ended with `exit_code`, of which the service read nothing it said: a
    /// success only with status 0.
    #[must_use]
    pub const fn exited(exit_code: Option<i32>) -> Self {
        Self {
            success: matches!(exit_code, Some(0)),
            exit_code,
            said: None,
        }
    }

    /// A program that ended with `exit_code` after writing `stderr` and `stdout`, `failed` when it
    /// stopped by itself in failure. What a program said is its reason only then: a program the
    /// person quit says nothing, and neither does one whose only words were its graphics
    /// implementation's.
    #[must_use]
    pub fn ended(
        exit_code: Option<i32>,
        failed: bool,
        stderr: &[String],
        stdout: &[String],
    ) -> Self {
        Self {
            success: !failed,
            exit_code,
            said: failed
                .then(|| last_words(stderr).or_else(|| last_words(stdout)))
                .flatten(),
        }
    }
}

/// How many of a failed program's last lines its end carries.
const SAID_LINES: usize = 2;
/// How long what a failed program's end carries may be, in characters: a sentence a person
/// reads, never a dump of its log.
const SAID_CHARS: usize = 200;

/// The last lines of `lines` that say anything, joined and bounded for a sentence a person reads;
/// none when nothing was said. What was cut is the oldest part, since a program says why it
/// stopped last. Diagnostics of what it ran on are not the program's words: a real program's last
/// line is often a Wine `fixme` or a warning from its graphics implementation that says nothing
/// about why it stopped.
#[must_use]
pub fn last_words(lines: &[String]) -> Option<String> {
    let said: Vec<&str> = lines
        .iter()
        .map(|line| line.trim())
        .filter(|line| {
            !line.is_empty()
                && !is_wine_diagnostic(line)
                && !is_graphics_diagnostic(line)
                && !is_wine_startup(line)
        })
        .collect();
    let said = said
        .get(said.len().saturating_sub(SAID_LINES)..)
        .unwrap_or_default()
        .join(" ");
    let length = said.chars().count();
    if length == 0 {
        return None;
    }
    if length <= SAID_CHARS {
        return Some(said);
    }
    let kept: String = said
        .chars()
        .skip(length.saturating_sub(SAID_CHARS.saturating_sub(1)))
        .collect();
    Some(format!("…{}", kept.trim_start()))
}

/// A line of Wine's debug output, such as `0024:fixme:kernelbase:AppPolicy...`: a thread id in
/// hex, the class, then the Wine module that wrote it.
fn is_wine_diagnostic(line: &str) -> bool {
    let mut parts = line.splitn(3, ':');
    let (Some(thread), Some(class), Some(_)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !thread.is_empty()
        && thread.chars().all(|c| c.is_ascii_hexdigit())
        && matches!(class, "fixme" | "err" | "warn" | "trace")
}

/// Whether `line` is the graphics implementation's own warning rather than anything the program
/// said: Mesa's loader and drivers, the Vulkan loader and the software rasteriser, the D3D12
/// driver Wine reaches, and Wine's own loader messages about them. They carry no thread id, so
/// the Wine filter above does not see them, and a program that prints nothing of its own would
/// otherwise be explained by them.
fn is_graphics_diagnostic(line: &str) -> bool {
    const PREFIXES: [&str; 11] = [
        "MESA:",
        "MESA-",
        "X Error",
        "Xlib:",
        "d3d12:",
        "dzn:",
        "err:d3d",
        "err:vulkan",
        "fixme:vulkan",
        "vulkan",
        "Vulkan",
    ];
    const GRAPHICS_LOADERS: [&str; 3] = ["libEGL", "libGL", "libvulkan"];
    PREFIXES.iter().any(|prefix| line.starts_with(prefix))
        || GRAPHICS_LOADERS
            .iter()
            .any(|loader| line.starts_with(loader))
        || (line.starts_with("WARNING:") && line.contains("lavapipe"))
}

/// Whether `line` is Wine setting its own prefix up rather than the program saying anything: the
/// configuration directory it made or updated, or the timeout it read for its debug output. It is
/// not a
/// reason the program stopped, though Wine prints it to the same stream.
fn is_wine_startup(line: &str) -> bool {
    const PREFIXES: [&str; 3] = [
        "wine: created the configuration directory",
        "wine: configuration in",
        "wine: Read enabled Timeout",
    ];
    PREFIXES.iter().any(|prefix| line.starts_with(prefix))
}

/// A program an executor started, which goes on after its launch request is answered: its end
/// is waited for on a thread of the service's own, and it can be ended at any time.
pub trait StartedProgram: Send + Sync {
    /// Wait until the program ended, and say how.
    fn wait(&self) -> Result<ExecutionOutcome, ExecutionError>;
    /// End the program now.
    fn stop(&self);
}

/// The command that starts a planned sandbox: Bubblewrap and its arguments, the descriptors it
/// inherits at the numbers its arguments name, and whether it starts with the game performance
/// hints.
///
/// Started directly ([`Self::spawn`]) or handed to whoever starts it for the session: a program
/// the session keeps open through a desktop restart starts behind the session's own launch, which
/// hands the same descriptors down ([`launcher::inherit`]).
pub struct SandboxCommand {
    pub program: std::path::PathBuf,
    pub args: Vec<std::ffi::OsString>,
    /// Each descriptor with the number the program finds it at.
    pub inherited: Vec<(std::os::fd::OwnedFd, i32)>,
    pub latency_sensitive: bool,
}

impl SandboxCommand {
    /// `program` with `args`, inheriting nothing.
    #[must_use]
    pub fn new(
        program: impl Into<std::path::PathBuf>,
        args: impl IntoIterator<Item = impl Into<std::ffi::OsString>>,
    ) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            inherited: Vec::new(),
            latency_sensitive: false,
        }
    }

    /// Start the command as this process's child.
    ///
    /// # Errors
    /// The program could not be started.
    pub fn spawn(self) -> Result<Box<dyn StartedProgram>, ExecutionError> {
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        let handed: Vec<_> = self
            .inherited
            .iter()
            .map(|(fd, target)| (fd.as_raw_fd(), *target))
            .collect();
        launcher::inherit::hand_down(&mut command, handed, self.latency_sensitive);
        let child = command.spawn().map_err(ExecutionError::Spawn)?;
        drop(self.inherited);
        Ok(Box::new(ChildProgram::new(child)?))
    }
}

pub trait SandboxExecutor: Send + Sync {
    /// The command that starts the already-validated Bubblewrap plan, without a shell.
    fn command(&self, plan: &CapsuleCommandPlan) -> Result<SandboxCommand, ExecutionError>;

    /// Start the plan as this process's child.
    fn start(&self, plan: &CapsuleCommandPlan) -> Result<Box<dyn StartedProgram>, ExecutionError> {
        self.command(plan)?.spawn()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BwrapExecutor;

impl SandboxExecutor for BwrapExecutor {
    fn command(&self, plan: &CapsuleCommandPlan) -> Result<SandboxCommand, ExecutionError> {
        let (uid, gid) = (
            rustix::process::geteuid().as_raw(),
            rustix::process::getegid().as_raw(),
        );
        validate_identity_uid(plan.identity_uid, uid)?;
        // Every source is duplicated above the numbers the program finds them at, so handing one
        // down never overwrites the source of the next.
        let mut inherited = Vec::new();
        if let Some(profile) = plan.seccomp_profile {
            let filter = launcher::seccomp::compile_app_baseline(profile, plan.seccomp_grants)
                .map_err(|error| ExecutionError::Seccomp(error.to_string()))?;
            let mut file = create_memfd("tilo-compat-seccomp")?;
            file.write_all(&filter)?;
            file.sync_all()?;
            // Bubblewrap reads from the inherited descriptor's current offset.
            // dup2 shares that offset, so an EOF position yields an empty BPF
            // program and PR_SET_SECCOMP fails with EINVAL.
            file.rewind()?;
            inherited.push((
                duplicate_above_handed_numbers(file.as_fd())?,
                SECCOMP_CHILD_FD,
            ));
        }
        let [prefix, home] = plan.directory_fds();
        if plan.prefix_for_proton {
            crate::store::prepare_for_proton(prefix)
                .map_err(|error| ExecutionError::Io(std::io::Error::other(error.to_string())))?;
        }
        inherited.push((duplicate_above_handed_numbers(prefix)?, PREFIX_CHILD_FD));
        inherited.push((duplicate_above_handed_numbers(home)?, HOME_CHILD_FD));
        for (name, contents, target) in identity_files(uid, gid) {
            let mut file = create_memfd(name)?;
            file.write_all(contents.as_bytes())?;
            file.rewind()?;
            inherited.push((duplicate_above_handed_numbers(file.as_fd())?, target));
        }
        let (program, args) = sandbox_program(
            plan.network_proxy.as_ref(),
            plan.audio_graph.as_deref(),
            plan.x11,
            &plan.bwrap_argv,
        );
        Ok(SandboxCommand {
            program,
            args,
            inherited,
            latency_sensitive: plan.latency_sensitive,
        })
    }
}

/// A program this process started as its child: ended through its pidfd, so a stop never
/// reaches another process that took its pid after it was reaped.
pub struct ChildProgram {
    child: std::sync::Mutex<std::process::Child>,
    pidfd: std::os::fd::OwnedFd,
}

impl ChildProgram {
    /// Wait for and end `child`.
    ///
    /// # Errors
    /// Its pidfd could not be opened; the child is ended then.
    pub fn new(mut child: std::process::Child) -> Result<Self, ExecutionError> {
        let pid = rustix::process::Pid::from_child(&child);
        match rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty()) {
            Ok(pidfd) => Ok(Self {
                child: std::sync::Mutex::new(child),
                pidfd,
            }),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(ExecutionError::Io(error.into()))
            }
        }
    }
}

impl StartedProgram for ChildProgram {
    fn wait(&self) -> Result<ExecutionOutcome, ExecutionError> {
        let status = self
            .child
            .lock()
            .map_err(|_| ExecutionError::Io(std::io::Error::other("the program's state is lost")))?
            .wait()?;
        Ok(ExecutionOutcome {
            success: status.success(),
            exit_code: status.code(),
            said: None,
        })
    }

    fn stop(&self) {
        // Bubblewrap dies of it, and the sandbox's processes with it (`--die-with-parent`).
        let _ = rustix::process::pidfd_send_signal(&self.pidfd, rustix::process::Signal::KILL);
    }
}

pub(crate) fn duplicate_above_handed_numbers(
    fd: BorrowedFd<'_>,
) -> Result<std::os::fd::OwnedFd, std::io::Error> {
    Ok(rustix::io::fcntl_dupfd_cloexec(fd, PASSWORD_CHILD_FD + 1)?)
}

/// Bubblewrap with `bwrap_argv`, started by the launcher's `windows-network` and `windows-audio`
/// steps after the private network proxy and audio graph prepared for the launch when there are
/// any: each step's processes are children of the sandbox's own process then, so they end with
/// the program whoever started it, and a program kept open while the desktop restarts keeps its
/// network and its sound. A sandbox for a program that shows through `x11` is given the X11
/// display its keeper serves first (`windows-x11-sandbox`), which the keeper picks only as it
/// starts.
fn sandbox_program(
    network_proxy: Option<&crate::plan::NetworkProxy>,
    audio_graph: Option<&std::path::Path>,
    x11: bool,
    bwrap_argv: &[String],
) -> (std::path::PathBuf, Vec<std::ffi::OsString>) {
    const BWRAP: &str = "/usr/bin/bwrap";
    let mut wrapped: Vec<std::ffi::OsString> = Vec::new();
    if x11 {
        wrapped.extend(["windows-x11-sandbox".into(), "--".into()]);
        if audio_graph.is_some() {
            wrapped.push(crate::plan::TILO_LAUNCHER.into());
        }
    }
    if let Some(graph) = audio_graph {
        wrapped.extend([
            "windows-audio".into(),
            "--graph".into(),
            graph.into(),
            "--".into(),
        ]);
    }
    let (program, args) = if wrapped.is_empty() {
        (
            std::path::PathBuf::from(BWRAP),
            bwrap_argv.iter().map(Into::into).collect(),
        )
    } else {
        wrapped.push(BWRAP.into());
        wrapped.extend(bwrap_argv.iter().map(Into::into));
        (
            std::path::PathBuf::from(crate::plan::TILO_LAUNCHER),
            wrapped,
        )
    };
    if let Some(proxy) = network_proxy {
        let mut behind: Vec<std::ffi::OsString> = [
            std::ffi::OsString::from("windows-network"),
            "--dir".into(),
            proxy.dir.clone().into(),
            "--control".into(),
            proxy.control.clone().into(),
            "--".into(),
            program.into_os_string(),
        ]
        .into_iter()
        .collect();
        behind.extend(args);
        return (crate::plan::TILO_LAUNCHER.into(), behind);
    }
    (program, args)
}

fn validate_identity_uid(planned: u32, effective: u32) -> Result<(), ExecutionError> {
    if planned != effective {
        return Err(ExecutionError::IdentityMismatch { planned, effective });
    }
    Ok(())
}

fn identity_files(uid: u32, gid: u32) -> [(&'static str, String, i32); 2] {
    [
        (
            "tilo-capsule-passwd",
            format!(
                "tilo:x:{uid}:{gid}:Tilo capsule:{}:/usr/bin/nologin\n",
                crate::plan::HOME_MOUNT
            ),
            PASSWD_CHILD_FD,
        ),
        (
            "tilo-capsule-group",
            format!("tilo:x:{gid}:\n"),
            GROUP_CHILD_FD,
        ),
    ]
}

fn create_memfd(name: &str) -> Result<File, ExecutionError> {
    let fd = rustix::fs::memfd_create(name, MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING)
        .map_err(|error| ExecutionError::Memfd(error.into()))?;
    Ok(fd.into())
}

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("capsule session UID {planned} differs from executor UID {effective}")]
    IdentityMismatch { planned: u32, effective: u32 },
    #[error("could not compile the Tilo seccomp profile: {0}")]
    Seccomp(String),
    #[error("could not create a private sandbox memfd: {0}")]
    Memfd(std::io::Error),
    #[error("could not spawn Bubblewrap: {0}")]
    Spawn(std::io::Error),
    #[error("sandbox process failed: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real program's stderr ends in Wine's own diagnostics more often than in its reason.
    #[test]
    fn what_a_program_said_is_its_own_words_not_wines() {
        let lines = [
            "Unable to open key file: no such file",
            "0024:fixme:kernelbase:AppPolicyGetProcessTerminationMethod FFFFFFFFFFFFFFFA",
            "00a4:err:module:loader_init Importing dlls failed, status c0000135",
            "",
        ]
        .map(str::to_owned);
        assert_eq!(
            last_words(&lines).as_deref(),
            Some("Unable to open key file: no such file")
        );
        assert_eq!(last_words(&lines[1..]), None);
        let said = ["error: fixme: the file is missing".to_owned()];
        assert_eq!(last_words(&said).as_deref(), Some(said[0].as_str()));
    }

    /// What the graphics stack under Wine prints is no reason a program stopped: a program that
    /// quit and said nothing of its own says nothing, and one that failed says its own last line
    /// with the warnings around it.
    #[test]
    fn a_graphics_warning_is_no_reason_a_program_stopped() {
        let warnings = [
            "MESA-LOADER: failed to open swrast: /usr/lib64/dri/swrast_dri.so: cannot open shared object file",
            "MESA: error: ZINK: failed to choose pdev",
            "dzn: D3D12CreateDevice failed with 0x80070005",
            "err:vulkan:wine_vk_init Failed to load Wine graphics driver.",
            "WARNING: lavapipe is not a conformant Vulkan implementation, testing use only.",
            "wine: created the configuration directory '/var/home/tilo/.wine'",
        ]
        .map(str::to_owned)
        .to_vec();
        assert_eq!(last_words(&warnings), None, "no reason, only diagnostics");

        let quit = ExecutionOutcome::ended(Some(0), false, &warnings, &[]);
        assert!(quit.success && quit.said.is_none(), "{quit:?}");

        let mut failed = warnings;
        failed.push("Unable to open key file: no such file".into());
        let end = ExecutionOutcome::ended(Some(3), true, &failed, &[]);
        assert!(!end.success);
        assert_eq!(
            end.said.as_deref(),
            Some("Unable to open key file: no such file"),
            "the program's own last line, not the warnings"
        );
    }

    #[test]
    fn a_sandbox_with_sound_and_a_network_proxy_starts_after_both() {
        let bwrap_argv = vec!["--die-with-parent".to_owned()];
        let (program, args) = sandbox_program(None, None, false, &bwrap_argv);
        assert_eq!(program, std::path::Path::new("/usr/bin/bwrap"));
        assert_eq!(args, ["--die-with-parent"]);
        let lease = std::path::Path::new("/lease");
        let (program, args) = sandbox_program(None, Some(lease), false, &bwrap_argv);
        assert_eq!(program, std::path::Path::new(crate::plan::TILO_LAUNCHER));
        assert_eq!(
            args,
            [
                "windows-audio",
                "--graph",
                "/lease",
                "--",
                "/usr/bin/bwrap",
                "--die-with-parent"
            ]
        );
        // The display is added to the sandbox's own arguments, past the audio graph's.
        let (program, args) = sandbox_program(None, Some(lease), true, &bwrap_argv);
        assert_eq!(program, std::path::Path::new(crate::plan::TILO_LAUNCHER));
        assert_eq!(
            args,
            [
                "windows-x11-sandbox",
                "--",
                crate::plan::TILO_LAUNCHER,
                "windows-audio",
                "--graph",
                "/lease",
                "--",
                "/usr/bin/bwrap",
                "--die-with-parent"
            ]
        );
        let proxy = crate::plan::NetworkProxy {
            dir: "/lease/netproxy".into(),
            control: "/keeper/network-proxy-control".into(),
        };
        let (program, args) = sandbox_program(Some(&proxy), Some(lease), false, &bwrap_argv);
        assert_eq!(program, std::path::Path::new(crate::plan::TILO_LAUNCHER));
        assert_eq!(
            args,
            [
                "windows-network",
                "--dir",
                "/lease/netproxy",
                "--control",
                "/keeper/network-proxy-control",
                "--",
                crate::plan::TILO_LAUNCHER,
                "windows-audio",
                "--graph",
                "/lease",
                "--",
                "/usr/bin/bwrap",
                "--die-with-parent"
            ]
        );
        let (_, args) = sandbox_program(None, None, true, &bwrap_argv);
        assert_eq!(
            args,
            [
                "windows-x11-sandbox",
                "--",
                "/usr/bin/bwrap",
                "--die-with-parent"
            ]
        );
    }

    #[test]
    fn execution_identity_is_fixed_and_fails_closed_when_changed() {
        {
            for (uid, gid) in [(0, 0), (1000, 2000), (u32::MAX, u32::MAX)] {
                let files = identity_files(uid, gid);
                assert_eq!(
                    files[0].1,
                    format!(
                        "tilo:x:{uid}:{gid}:Tilo capsule:/run/tilo/capsule/home:/usr/bin/nologin\n"
                    )
                );
                assert_eq!(files[1].1, format!("tilo:x:{gid}:\n"));
                assert_eq!(files[0].2, PASSWD_CHILD_FD);
                assert_eq!(files[1].2, GROUP_CHILD_FD);
            }
        }
        {
            assert!(validate_identity_uid(1000, 1000).is_ok());
            assert!(matches!(
                validate_identity_uid(1000, 0),
                Err(ExecutionError::IdentityMismatch {
                    planned: 1000,
                    effective: 0
                })
            ));
        }
    }
}
