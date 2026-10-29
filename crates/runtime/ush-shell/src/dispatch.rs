//! Commands that run commands: `find -exec`, `xargs`, `env`, `timeout`,
//! `nice` and `nohup` hand their command back to the shell, which resolves it
//! as it resolves any utility (one of Tilo's own commands, a file action, a
//! program) in a copy of itself. Nothing forks or execs, and everything the
//! command does belongs to the run that asked for it.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::commands::{Dispatch, EnvChange};
use crate::host::{DirEntry, Host, Metadata, Program, WriteMode};
use crate::io::Stdio;
use crate::shell::{Fd, Flow, Shell};

/// The shell, as the commands it runs see it.
pub(crate) struct ShellDispatch<'a> {
    shell: &'a Shell,
}

impl<'a> ShellDispatch<'a> {
    pub(crate) const fn new(shell: &'a Shell) -> Self {
        Self { shell }
    }

    /// A copy of the shell for one command: its own directory and streams,
    /// and no functions, which a utility never names.
    fn copy(&self, cwd: &Path, io: &Stdio) -> Shell {
        let mut shell = self.shell.clone();
        shell.functions.clear();
        shell.cwd = cwd.to_path_buf();
        shell.set_stdio(io.clone());
        shell
    }
}

impl Dispatch for ShellDispatch<'_> {
    fn run(&self, argv: &[String], cwd: &Path, io: &Stdio) -> i32 {
        let mut shell = self.copy(cwd, io);
        shell.run_utility(argv)
    }

    fn run_with_env(&self, argv: &[String], cwd: &Path, io: &Stdio, env: &EnvChange) -> i32 {
        let mut shell = self.copy(cwd, io);
        if env.clear {
            let exported: Vec<String> = shell
                .vars
                .all()
                .into_iter()
                .filter(|(_, var)| var.exported)
                .map(|(name, _)| name.clone())
                .collect();
            for name in exported {
                if shell.vars.unset(&name).is_err()
                    && let Some(var) = shell.vars.get_mut(&name)
                {
                    var.exported = false;
                }
            }
        }
        for name in &env.unset {
            if shell.vars.unset(name).is_err()
                && let Some(var) = shell.vars.get_mut(name)
            {
                var.exported = false;
            }
        }
        for (name, value) in &env.set {
            if let Err(e) = shell.vars.set(name, value.as_str()) {
                let _ = io.stderr.write_str(&format!("env: {e}\n"));
                return 125;
            }
            shell.vars.declare(name).exported = true;
        }
        shell.run_utility(argv)
    }

    fn run_for(&self, argv: &[String], cwd: &Path, io: &Stdio, millis: u64) -> Option<i32> {
        let mut shell = self.copy(cwd, io);
        let deadline = Arc::new(Deadline {
            inner: Arc::clone(&shell.host),
            at: shell
                .host
                .now_ms()
                .saturating_add(i64::try_from(millis).unwrap_or(i64::MAX)),
            fired: AtomicBool::new(false),
        });
        shell.host = Arc::clone(&deadline) as Arc<dyn Host>;
        let status = shell.run_utility(argv);
        drop(shell);
        (!deadline.fired.load(Ordering::SeqCst)).then_some(status)
    }

    fn confirm(&self, question: &str) -> bool {
        let io = self.shell.stdio();
        let _ = io.stderr.write_str(question);
        // The terminal, not what the command reads: `find -ok` asks the
        // person even when its own input is a pipe.
        let answer = match self.shell.fds.get(&0) {
            Some(Fd::In(input)) => input.read_line_until(b'\n').ok().flatten(),
            _ => None,
        };
        answer.is_some_and(|line| matches!(line.first(), Some(b'y' | b'Y')))
    }
}

impl Shell {
    /// Run `argv` as a utility: a regular built-in that is also a
    /// utility (`echo`, `printf`, `test`), one of Tilo's own commands, a file
    /// action, or a program. Never a function or a special built-in.
    pub(crate) fn run_utility(&mut self, argv: &[String]) -> i32 {
        let Some(name) = argv.first() else {
            return 0;
        };
        let operands = argv.get(1..).unwrap_or(&[]);
        if crate::builtins::special(name).is_none()
            && let Some(builtin) = crate::builtins::regular(name)
        {
            return match builtin(self, operands) {
                Ok(status) => status,
                Err(Flow::Exit(s) | Flow::Fatal(s) | Flow::Error(s) | Flow::Return(s)) => s,
                Err(Flow::Break(_) | Flow::Continue(_)) => 0,
            };
        }
        if crate::commands::FILE_ACTIONS.contains(&name.as_str()) {
            let io = self.stdio();
            return self.host.file_action(&crate::host::FileCall {
                name,
                args: operands,
                cwd: &self.cwd,
                io: &io,
                run: self.run.as_deref(),
            });
        }
        if let Some(command) = crate::commands::lookup(name) {
            return self.run_tier1(name, command, operands);
        }
        self.run_other(argv)
    }
}

/// The host, as a command under `timeout` sees it: interrupted once its
/// time is up.
struct Deadline {
    inner: Arc<dyn Host>,
    at: i64,
    fired: AtomicBool,
}

impl Deadline {
    fn passed(&self) -> bool {
        if self.inner.now_ms() >= self.at {
            self.fired.store(true, Ordering::SeqCst);
        }
        self.fired.load(Ordering::SeqCst)
    }
}

impl Host for Deadline {
    fn metadata(&self, path: &Path) -> std::io::Result<Metadata> {
        self.inner.metadata(path)
    }
    fn symlink_metadata(&self, path: &Path) -> std::io::Result<Metadata> {
        self.inner.symlink_metadata(path)
    }
    fn read_dir(&self, path: &Path) -> std::io::Result<Vec<DirEntry>> {
        self.inner.read_dir(path)
    }
    fn open_read(&self, path: &Path) -> std::io::Result<Box<dyn Read + Send>> {
        self.inner.open_read(path)
    }
    fn open_write(
        &self,
        path: &Path,
        mode: WriteMode,
        run: Option<&crate::host::Run>,
    ) -> std::io::Result<Box<dyn Write + Send>> {
        self.inner.open_write(path, mode, run)
    }
    fn read_link(&self, path: &Path) -> std::io::Result<PathBuf> {
        self.inner.read_link(path)
    }
    fn file_action(&self, call: &crate::host::FileCall<'_>) -> i32 {
        self.inner.file_action(call)
    }
    fn program(
        &self,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        io: &Stdio,
    ) -> Program {
        self.inner.program_until(argv, env, cwd, io, &|| {
            self.passed() || self.inner.interrupted()
        })
    }
    fn program_until(
        &self,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        io: &Stdio,
        stop: &dyn Fn() -> bool,
    ) -> Program {
        self.inner
            .program_until(argv, env, cwd, io, &|| self.passed() || stop())
    }
    fn interrupted(&self) -> bool {
        self.passed() || self.inner.interrupted()
    }
    fn clear_interrupt(&self) {
        self.inner.clear_interrupt();
    }
    fn take_suspend(&self) -> bool {
        self.inner.take_suspend()
    }
    fn now(&self) -> i64 {
        self.inner.now()
    }
    fn now_ms(&self) -> i64 {
        self.inner.now_ms()
    }
    fn utc_offset(&self, at: i64) -> i32 {
        self.inner.utc_offset(at)
    }
    fn sleep(&self, millis: u64) {
        // Sleep no further than the deadline.
        let left = u64::try_from(self.at.saturating_sub(self.inner.now_ms())).unwrap_or(0);
        self.inner.sleep(millis.min(left));
        let _ = self.passed();
    }
    fn user_name(&self, uid: u32) -> Option<String> {
        self.inner.user_name(uid)
    }
    fn home_of(&self, name: &str) -> Option<PathBuf> {
        self.inner.home_of(name)
    }
    fn group_name(&self, gid: u32) -> Option<String> {
        self.inner.group_name(gid)
    }
    fn pid(&self) -> u32 {
        self.inner.pid()
    }
    fn hostname(&self) -> String {
        self.inner.hostname()
    }
    fn edit_mode(&self, on: bool) -> bool {
        self.inner.edit_mode(on)
    }
    fn columns(&self) -> usize {
        self.inner.columns()
    }
    fn history(&self, query: &crate::host::HistoryQuery) -> Option<Vec<String>> {
        self.inner.history(query)
    }
    fn programs(&self, path: &str) -> Vec<String> {
        self.inner.programs(path)
    }
}
