//! The new shell on a Linux device: [`ush_shell`]'s interpreter, with this
//! machine as its host.
//!
//! Inside a guest the guest's root is the environment by construction: what
//! the record granted is bound at its path and nothing else is there, so the
//! tier-1 commands read through the guest's own file system and see exactly
//! what the grants allow. The file commands go to the broker as the
//! environment's actions, the same way the guest's `cp` shim sends them
//! ([`crate::file_actions::command`]). A command that is not tier 1 runs in
//! the guest, as it did under bash, so nothing a person relies on breaks
//! while tiers 2 and 3 are still to come.
//!
//! `tilo-ush sh` (the guest's `ush-sh`) is this shell: `-c SCRIPT`, a script
//! file, or, with no operand, interactive on its terminal, marking each run
//! for Terminal when the session's key is in [`crate::runs::KEY_ENV`].

use std::io::{self, IsTerminal as _, Read, Write};
use std::os::fd::{AsFd as _, OwnedFd};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio as ProcessStdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ush_shell::{
    DirEntry, FileCall, Host, Input, Kind, Metadata, Output, Program, Shell, Std, Stdio, WriteMode,
};

/// The name the shell answers to on a guest's `PATH`.
pub const SHELL_COMMAND: &str = "ush-sh";

/// The shell's path inside a guest: what `$SHELL` and the generated
/// `passwd` name.
pub fn guest_shell_path() -> PathBuf {
    Path::new(crate::GUEST_BIN_DIR).join(SHELL_COMMAND)
}

/// Whether `shell` is this shell.
pub fn is_ush(shell: &Path) -> bool {
    shell.file_name().is_some_and(|name| name == SHELL_COMMAND)
}

/// Set by the `SIGINT` handler: the person pressed Ctrl-C.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_interrupt(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Take Ctrl-C as a request to stop what is running, not as the end of the
/// shell. No `SA_RESTART`, so a read of the terminal returns at once.
fn catch_interrupt() {
    // SAFETY: the handler only stores to an atomic, which is
    // async-signal-safe, and the sigaction struct is fully initialised.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_interrupt as *const () as usize;
        action.sa_flags = 0;
        libc::sigemptyset(&raw mut action.sa_mask);
        libc::sigaction(libc::SIGINT, &raw const action, std::ptr::null_mut());
        // A program the shell runs gets the terminal's signals itself; the
        // shell must not die of SIGQUIT or a write to a closed pipe.
        libc::signal(libc::SIGQUIT, libc::SIG_IGN);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

/// This machine, as the shell's host.
pub struct DeviceHost {
    /// The broker of the guest the shell runs in; `None` outside a guest.
    broker: Option<PathBuf>,
    recoverable: PathBuf,
}

impl DeviceHost {
    /// The host for a shell in a guest when the broker is there, else for
    /// one outside every guest.
    pub fn discover() -> Self {
        let broker = std::env::var_os("TILO_USH_BROKER")
            .map_or_else(|| PathBuf::from(crate::GUEST_BROKER_SOCK), PathBuf::from);
        let inside = broker.exists();
        let home = tilo_paths::xdg::data_home().unwrap_or_else(|| PathBuf::from("/nonexistent"));
        Self {
            recoverable: if inside {
                PathBuf::from(crate::file_actions::RECOVERABLE_DIR)
            } else {
                home.join("ush/recoverable")
            },
            broker: inside.then_some(broker),
        }
    }
}

fn metadata_of(meta: &std::fs::Metadata) -> Metadata {
    let ft = meta.file_type();
    let kind = if ft.is_symlink() {
        Kind::Symlink
    } else if ft.is_dir() {
        Kind::Dir
    } else if ft.is_file() {
        Kind::File
    } else {
        // A fifo, a socket or a device.
        Kind::Other
    };
    Metadata {
        kind,
        len: meta.len(),
        mode: meta.mode() & 0o7777,
        modified: meta.mtime(),
        uid: meta.uid(),
        gid: meta.gid(),
        nlink: meta.nlink(),
        dev: meta.dev(),
        ino: meta.ino(),
    }
}

/// A file opened for reading that answers an interrupted read as one.
struct File(std::fs::File);

impl Read for File {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

/// The descriptor a child gets for one of the shell's streams: the host's
/// own when the stream is one, else a pipe the shell feeds or drains.
fn inherit(stream: Option<Std>) -> Option<OwnedFd> {
    match stream? {
        Std::In => io::stdin().as_fd().try_clone_to_owned().ok(),
        Std::Out => io::stdout().as_fd().try_clone_to_owned().ok(),
        Std::Err => io::stderr().as_fd().try_clone_to_owned().ok(),
    }
}

fn copy_out(mut from: impl Read, to: &Output) {
    let mut buf = [0_u8; 16 * 1024];
    loop {
        match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if to.write_all(buf.get(..n).unwrap_or(&[])).is_err() {
                    break;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
}

fn exit_status(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt as _;
    status
        .code()
        .or_else(|| status.signal().map(|s| s.saturating_add(128)))
        .unwrap_or(1)
}

impl Host for DeviceHost {
    fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        std::fs::metadata(path).map(|m| metadata_of(&m))
    }

    fn symlink_metadata(&self, path: &Path) -> io::Result<Metadata> {
        std::fs::symlink_metadata(path).map(|m| metadata_of(&m))
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let kind = match entry.file_type() {
                Ok(ft) if ft.is_symlink() => Kind::Symlink,
                Ok(ft) if ft.is_dir() => Kind::Dir,
                Ok(ft) if ft.is_file() => Kind::File,
                _ => Kind::Other,
            };
            out.push(DirEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                kind,
            });
        }
        Ok(out)
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        Ok(Box::new(File(std::fs::File::open(path)?)))
    }

    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn Write + Send>> {
        let mut options = std::fs::OpenOptions::new();
        match mode {
            WriteMode::Truncate => options.write(true).create(true).truncate(true),
            WriteMode::Append => options.append(true).create(true),
            WriteMode::CreateNew => options.write(true).create_new(true),
        };
        Ok(Box::new(options.open(path)?))
    }

    fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::read_link(path)
    }

    fn file_action(&self, call: &FileCall<'_>) -> i32 {
        let mut out = OutputWriter(call.io.stdout.clone());
        let mut err = OutputWriter(call.io.stderr.clone());
        let mut command_io = crate::file_actions::command::Io {
            out: &mut out,
            err: &mut err,
            broker: self.broker.as_deref(),
            recoverable: self.recoverable.clone(),
            progress: false,
        };
        let run = call.run.map(|run| crate::broker::RunGroup {
            id: run.id.clone(),
            label: run.label.clone(),
        });
        crate::file_actions::command::run_within(
            call.name,
            call.args,
            call.cwd,
            run.as_ref(),
            &mut command_io,
        )
    }

    fn program(
        &self,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        io: &Stdio,
    ) -> Program {
        run_program(argv, env, cwd, io, None)
    }

    fn program_until(
        &self,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        io: &Stdio,
        stop: &dyn Fn() -> bool,
    ) -> Program {
        run_program(argv, env, cwd, io, Some(stop))
    }

    fn interrupted(&self) -> bool {
        INTERRUPTED.load(Ordering::SeqCst)
    }

    fn clear_interrupt(&self) {
        INTERRUPTED.store(false, Ordering::SeqCst);
    }

    fn now(&self) -> i64 {
        i64::try_from(tilo_clock::unix_now()).unwrap_or(0)
    }

    fn utc_offset(&self, at: i64) -> i32 {
        let time: libc::time_t = at;
        // SAFETY: localtime_r writes only into the zeroed struct we own.
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&raw const time, &raw mut tm).is_null() {
                return 0;
            }
            i32::try_from(tm.tm_gmtoff).unwrap_or(0)
        }
    }

    fn sleep(&self, millis: u64) {
        let mut left = millis;
        while left > 0 && !self.interrupted() {
            let step = left.min(50);
            std::thread::sleep(std::time::Duration::from_millis(step));
            left = left.saturating_sub(step);
        }
    }

    fn user_name(&self, uid: u32) -> Option<String> {
        account_name("/etc/passwd", uid)
    }

    fn group_name(&self, gid: u32) -> Option<String> {
        account_name("/etc/group", gid)
    }

    fn pid(&self) -> u32 {
        std::process::id()
    }

    fn hostname(&self) -> String {
        std::fs::read_to_string("/proc/sys/kernel/hostname")
            .map_or_else(|_| "localhost".into(), |h| h.trim().to_string())
    }
}

/// The name an id has in `passwd` or `group`.
fn account_name(file: &str, id: u32) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?;
    text.lines().find_map(|line| {
        let mut fields = line.split(':');
        let name = fields.next()?;
        let _ = fields.next();
        (fields.next()?.parse::<u32>().ok()? == id).then(|| name.to_string())
    })
}

/// Run a program on this machine with the shell's streams, until it ends or
/// `stop` says to stop it. Without `stop` it is a plain wait: nothing polls.
fn run_program(
    argv: &[String],
    env: &[(String, String)],
    cwd: &Path,
    io: &Stdio,
    stop: Option<&dyn Fn() -> bool>,
) -> Program {
    let Some((name, operands)) = argv.split_first() else {
        return Program::Exited(0);
    };
    let mut command = Command::new(name);
    command
        .args(operands)
        .env_clear()
        .envs(env.iter().cloned())
        .current_dir(cwd);
    // SAFETY: between fork and exec this only calls signal(2), which is
    // async-signal-safe. The shell ignores SIGQUIT; the program must not
    // inherit that.
    unsafe {
        std::os::unix::process::CommandExt::pre_exec(&mut command, || {
            libc::signal(libc::SIGQUIT, libc::SIG_DFL);
            Ok(())
        });
    }
    let stdin = inherit(io.stdin.host_stream());
    let stdout = inherit(io.stdout.host_stream());
    let stderr = inherit(io.stderr.host_stream());
    let (feed, drain_out, drain_err) = (stdin.is_none(), stdout.is_none(), stderr.is_none());
    command.stdin(stdin.map_or_else(ProcessStdio::piped, ProcessStdio::from));
    command.stdout(stdout.map_or_else(ProcessStdio::piped, ProcessStdio::from));
    command.stderr(stderr.map_or_else(ProcessStdio::piped, ProcessStdio::from));
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Program::NotFound,
        Err(e) => {
            let _ = io.stderr.write_str(&format!("ush: {name}: {e}\n"));
            return Program::Exited(126);
        }
    };
    std::thread::scope(|scope| {
        if feed && let Some(mut to) = child.stdin.take() {
            let input = io.stdin.clone();
            scope.spawn(move || {
                let mut buf = [0_u8; 16 * 1024];
                while let Ok(n) = input.read(&mut buf) {
                    if n == 0 || to.write_all(buf.get(..n).unwrap_or(&[])).is_err() {
                        break;
                    }
                }
            });
        }
        if drain_out && let Some(from) = child.stdout.take() {
            let to = io.stdout.clone();
            scope.spawn(move || copy_out(from, &to));
        }
        if drain_err && let Some(from) = child.stderr.take() {
            let to = io.stderr.clone();
            scope.spawn(move || copy_out(from, &to));
        }
        // Polled only while something may stop it early (`timeout`);
        // otherwise, and once stopped, a plain wait.
        let mut stopped = false;
        let status = loop {
            if !stopped && stop.is_some_and(|stop| stop()) {
                stopped = true;
                let _ = child.kill();
            }
            let waited = if stopped || stop.is_none() {
                child.wait().map(Some)
            } else {
                child.try_wait()
            };
            match waited {
                Ok(Some(status)) => break exit_status(status),
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break 1,
            }
        };
        Program::Exited(status)
    })
}

/// An [`Output`] as a [`Write`].
struct OutputWriter(Output);

impl Write for OutputWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// `tilo-ush sh [-c SCRIPT [NAME [ARG…]] | FILE [ARG…]] [-i] [-l]`: run
/// the shell. Answers its exit status.
pub fn main(args: &[String]) -> i32 {
    let mut script = None;
    let mut operands = Vec::new();
    let mut interactive = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-c" => {
                script = iter.next().cloned();
                operands.extend(iter.by_ref().cloned());
            }
            "-i" => interactive = true,
            "-l" | "--login" | "-s" => {}
            "--" => operands.extend(iter.by_ref().cloned()),
            flag if flag.starts_with('-') && operands.is_empty() => {
                eprintln!("ush: {flag}: unsupported option");
                return 2;
            }
            other => {
                operands.push(other.to_string());
                operands.extend(iter.by_ref().cloned());
            }
        }
    }
    catch_interrupt();
    let host: Arc<dyn Host> = Arc::new(DeviceHost::discover());
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let stdin = std::io::stdin()
        .as_fd()
        .try_clone_to_owned()
        .map(|fd| File(std::fs::File::from(fd)));
    let io = Stdio {
        stdin: match stdin {
            Ok(file) => Input::host(file),
            Err(_) => Input::null(),
        },
        stdout: Output::host(Std::Out, std::io::stdout()),
        stderr: Output::host(Std::Err, std::io::stderr()),
    };
    let mut shell = Shell::new(Arc::clone(&host), std::env::vars(), cwd, io);
    let runs_key = shell
        .take_var(crate::runs::KEY_ENV)
        .filter(|k| crate::runs::valid_key(k));
    let _ = shell.take_var("TILO_USH_BROKER");
    shell.set_var("SHELL", &guest_shell_path().to_string_lossy());
    if let Some(script) = script {
        let mut rest = operands.into_iter();
        let name = rest.next().unwrap_or_else(|| "ush".into());
        shell.set_args(name, rest.collect());
        return shell.run_to_exit(&script);
    }
    if let Some((file, rest)) = operands.split_first() {
        let text = match std::fs::read_to_string(file) {
            Ok(text) => text,
            Err(e) => {
                eprintln!("ush: {file}: {e}");
                return 127;
            }
        };
        shell.set_args(file.clone(), rest.to_vec());
        return shell.run_to_exit(&text);
    }
    if interactive || std::io::stdin().is_terminal() {
        if let Some(home) = shell.var("HOME") {
            let rc = Path::new(&home).join(".ushrc");
            if let Ok(text) = std::fs::read_to_string(&rc) {
                shell.set_interactive(true);
                let _ = shell.run_script(&text);
            }
        }
        return shell.interactive(&ush_shell::Session { runs_key });
    }
    let mut text = String::new();
    if std::io::stdin().read_to_string(&mut text).is_err() {
        return 1;
    }
    shell.run_to_exit(&text)
}
