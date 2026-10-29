//! Jobs: command lines that run side by side with the shell (`&`), or were
//! stopped with Ctrl+Z, and the built-ins that move them (`jobs`, `fg`,
//! `bg`, `wait`, `kill %n`).
//!
//! A job is a copy of the shell running on a thread of its own, so the
//! shell's own commands run concurrently in-process; nothing forks. A
//! program a job runs is the host's ([`Host::program_in_job`]): the host
//! tells the job how to signal it, so stopping, continuing and ending a job
//! reach its programs as well as its own commands.
//!
//! The shell's own commands stop where they look at the host: before each
//! write and each step of a loop ([`Host::interrupted`]). A stopped job
//! waits there until it is continued or ended.
//!
//! An interactive shell runs each command line it reads as a foreground job,
//! so Ctrl+Z gives the prompt back with the line stopped. What a stopped
//! line had changed in the shell (a `cd`, a variable) before it stopped
//! stays with the job, as it would in a subshell.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use crate::host::{DirEntry, FileCall, HistoryQuery, Host, Metadata, Program, WriteMode};
use crate::io::Stdio;

/// What the shell asks of a job's programs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// Stop until continued (Ctrl+Z, `kill -STOP`).
    Stop,
    /// Go on (`fg`, `bg`, `kill -CONT`). A job in the foreground has the
    /// terminal again first.
    Continue,
    /// End (`kill`).
    Terminate,
    /// Ctrl+C.
    Interrupt,
    /// End at once (`kill -9`).
    Kill,
}

/// Where a job is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Running,
    Stopped,
    Done(i32),
}

type Signaller = Arc<dyn Fn(Signal) + Send + Sync>;

/// One job: its command line, where it is, and how to reach its programs.
pub struct Job {
    /// What `jobs` calls it.
    pub command: String,
    /// The number `$!`, `wait` and `kill` know it by.
    pub pid: u32,
    phase: Mutex<Phase>,
    changed: Condvar,
    killed: AtomicBool,
    foreground: AtomicBool,
    signallers: Mutex<Vec<(u64, Signaller)>>,
    next_signaller: Mutex<u64>,
    /// The host's own name for the group of programs this job runs (a
    /// process group), set by the host when it starts the first.
    group: Mutex<Option<i64>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job")
            .field("command", &self.command)
            .field("pid", &self.pid)
            .field("phase", &self.phase())
            .finish_non_exhaustive()
    }
}

/// While held, the job can signal one program the host started for it.
pub struct Attached {
    job: Arc<Job>,
    id: u64,
}

impl Drop for Attached {
    fn drop(&mut self) {
        lock(&self.job.signallers).retain(|(id, _)| *id != self.id);
    }
}

impl Job {
    pub fn new(command: impl Into<String>, pid: u32, foreground: bool) -> Arc<Self> {
        Arc::new(Self {
            command: command.into(),
            pid,
            phase: Mutex::new(Phase::Running),
            changed: Condvar::new(),
            killed: AtomicBool::new(false),
            foreground: AtomicBool::new(foreground),
            signallers: Mutex::new(Vec::new()),
            next_signaller: Mutex::new(0),
            group: Mutex::new(None),
        })
    }

    pub fn phase(&self) -> Phase {
        *lock(&self.phase)
    }

    /// Whether the job has the terminal: Ctrl+C and Ctrl+Z are for it.
    pub fn foreground(&self) -> bool {
        self.foreground.load(Ordering::SeqCst)
    }

    pub fn set_foreground(&self, foreground: bool) {
        self.foreground.store(foreground, Ordering::SeqCst);
    }

    /// Whether the job was ended (`kill`): its commands stop.
    pub fn killed(&self) -> bool {
        self.killed.load(Ordering::SeqCst)
    }

    /// The host's group for this job's programs, for the host to set when
    /// it starts the first and read when it starts the next.
    pub fn group(&self) -> MutexGuard<'_, Option<i64>> {
        lock(&self.group)
    }

    /// The host started a program for the job: `signal` reaches it, until
    /// the answer is dropped.
    pub fn attach(self: &Arc<Self>, signal: impl Fn(Signal) + Send + Sync + 'static) -> Attached {
        let mut next = lock(&self.next_signaller);
        *next = next.wrapping_add(1);
        let id = *next;
        drop(next);
        lock(&self.signallers).push((id, Arc::new(signal)));
        Attached {
            job: Arc::clone(self),
            id,
        }
    }

    /// How many programs the host runs for the job now.
    pub fn programs_running(&self) -> usize {
        lock(&self.signallers).len()
    }

    fn signal_programs(&self, signal: Signal) {
        let signallers: Vec<Signaller> = lock(&self.signallers)
            .iter()
            .map(|(_, s)| Arc::clone(s))
            .collect();
        for signaller in signallers {
            signaller(signal);
        }
    }

    fn set_phase(&self, phase: Phase) {
        let mut now = lock(&self.phase);
        if matches!(*now, Phase::Done(_)) {
            return;
        }
        *now = phase;
        drop(now);
        self.changed.notify_all();
    }

    /// The host saw one of the job's programs stop (Ctrl+Z reached it).
    pub fn stopped(&self) {
        self.set_phase(Phase::Stopped);
    }

    /// Stop the job: its programs, and its own commands at their next step.
    pub fn stop(&self) {
        self.set_phase(Phase::Stopped);
        self.signal_programs(Signal::Stop);
    }

    /// Let the job go on, in the foreground or not.
    pub fn resume(&self, foreground: bool) {
        self.set_foreground(foreground);
        self.set_phase(Phase::Running);
        self.signal_programs(Signal::Continue);
    }

    /// End the job: its commands stop at their next step, its programs get
    /// `signal`.
    pub fn kill(&self, signal: Signal) {
        self.killed.store(true, Ordering::SeqCst);
        self.signal_programs(signal);
        // A stopped job must run to end.
        if self.phase() == Phase::Stopped {
            self.set_phase(Phase::Running);
            self.signal_programs(Signal::Continue);
        }
        self.changed.notify_all();
    }

    /// The job's commands ended with `status`.
    pub(crate) fn finish(&self, status: i32) {
        *lock(&self.phase) = Phase::Done(status);
        self.changed.notify_all();
    }

    /// Wait while the job runs: until it stopped or ended.
    pub fn wait_settled(&self) -> Phase {
        let mut phase = lock(&self.phase);
        while *phase == Phase::Running {
            phase = self
                .changed
                .wait(phase)
                .unwrap_or_else(PoisonError::into_inner);
        }
        *phase
    }

    /// [`Self::wait_settled`], giving up after `millis` so the caller can
    /// look whether it was interrupted meanwhile.
    pub(crate) fn wait_settled_for(&self, millis: u64) -> Phase {
        let phase = lock(&self.phase);
        let (phase, _) = self
            .changed
            .wait_timeout_while(phase, std::time::Duration::from_millis(millis), |p| {
                *p == Phase::Running
            })
            .unwrap_or_else(PoisonError::into_inner);
        *phase
    }

    /// Where the job's own commands look in: a Ctrl+Z for the foreground
    /// stops it here, and a stopped job waits here until it goes on.
    fn checkpoint(&self, host: &dyn Host) {
        if self.foreground() && host.take_suspend() {
            self.stop();
        }
        let phase = lock(&self.phase);
        drop(
            self.changed
                .wait_while(phase, |p| *p == Phase::Stopped && !self.killed())
                .unwrap_or_else(PoisonError::into_inner),
        );
    }
}

/// The jobs of one shell.
#[derive(Debug, Default)]
pub(crate) struct Table {
    /// By job number, lowest first.
    jobs: Vec<(u32, Arc<Job>)>,
    /// The job `%+` names, and the one before it (`%-`).
    current: Option<u32>,
    previous: Option<u32>,
    /// The number the next job's `pid` takes.
    next_pid: u32,
}

impl Table {
    /// Add `job`, answering its number.
    pub(crate) fn add(&mut self, job: Arc<Job>) -> u32 {
        let number = self.jobs.last().map_or(1, |(n, _)| n.saturating_add(1));
        self.jobs.push((number, job));
        self.previous = self.current;
        self.current = Some(number);
        number
    }

    /// A number for a new job's `pid`, after `base` (the shell's own).
    pub(crate) fn pid(&mut self, base: u32) -> u32 {
        if self.next_pid <= base {
            self.next_pid = base.saturating_add(1);
        }
        let pid = self.next_pid;
        self.next_pid = self.next_pid.saturating_add(1);
        pid
    }

    pub(crate) fn remove(&mut self, number: u32) {
        self.jobs.retain(|(n, _)| *n != number);
        if self.current == Some(number) {
            self.current = self.previous.take();
        }
        if self.previous == Some(number) {
            self.previous = None;
        }
        if self.current.is_none() {
            self.current = self.jobs.last().map(|(n, _)| *n);
        }
    }

    /// Make `number` the current job (`%+`).
    pub(crate) fn make_current(&mut self, number: u32) {
        if self.current != Some(number) {
            self.previous = self.current;
            self.current = Some(number);
        }
    }

    pub(crate) fn all(&self) -> Vec<(u32, Arc<Job>)> {
        self.jobs.clone()
    }

    /// `+` for the current job, `-` for the one before, else a space.
    pub(crate) fn mark(&self, number: u32) -> char {
        if self.current == Some(number) {
            '+'
        } else if self.previous == Some(number) {
            '-'
        } else {
            ' '
        }
    }

    /// The job `spec` names: `%n`, `%+`, `%%`, `%-`, `%name` (a command
    /// starting with name), `%?text` (one containing text), or a `pid`.
    pub(crate) fn find(&self, spec: &str) -> Result<(u32, Arc<Job>), String> {
        let by_number = |n: u32| {
            self.jobs
                .iter()
                .find(|(number, _)| *number == n)
                .cloned()
                .ok_or_else(|| format!("{spec}: no such job"))
        };
        let Some(rest) = spec.strip_prefix('%') else {
            let pid: u32 = spec
                .parse()
                .map_err(|_| format!("{spec}: arguments must be process or job IDs"))?;
            return self
                .jobs
                .iter()
                .find(|(_, job)| job.pid == pid)
                .cloned()
                .ok_or_else(|| format!("{spec}: no such job"));
        };
        match rest {
            "" | "+" | "%" => self
                .current
                .ok_or_else(|| "current: no such job".to_owned())
                .and_then(by_number),
            "-" => self
                .previous
                .ok_or_else(|| "previous: no such job".to_owned())
                .and_then(by_number),
            n if n.chars().all(|c| c.is_ascii_digit()) => {
                by_number(n.parse().map_err(|_| format!("{spec}: no such job"))?)
            }
            text => {
                let found: Vec<_> = self
                    .jobs
                    .iter()
                    .filter(|(_, job)| match text.strip_prefix('?') {
                        Some(inside) => job.command.contains(inside),
                        None => job.command.starts_with(text),
                    })
                    .collect();
                match found.as_slice() {
                    [one] => Ok((*one).clone()),
                    [] => Err(format!("{spec}: no such job")),
                    _ => Err(format!("{spec}: ambiguous job spec")),
                }
            }
        }
    }
}

/// The host, as one job sees it: Ctrl+C and Ctrl+Z reach the job only
/// while it is in the foreground, `kill` ends it, and a stopped job waits
/// at its next step. Its programs run as the job's.
pub(crate) struct JobHost {
    pub(crate) inner: Arc<dyn Host>,
    pub(crate) job: Arc<Job>,
}

impl JobHost {
    fn ended(&self) -> bool {
        self.job.killed() || (self.job.foreground() && self.inner.interrupted())
    }
}

impl Host for JobHost {
    fn metadata(&self, path: &Path) -> std::io::Result<Metadata> {
        self.inner.metadata(path)
    }
    fn symlink_metadata(&self, path: &Path) -> std::io::Result<Metadata> {
        self.inner.symlink_metadata(path)
    }
    fn read_dir(&self, path: &Path) -> std::io::Result<Vec<DirEntry>> {
        self.job.checkpoint(self.inner.as_ref());
        self.inner.read_dir(path)
    }
    fn open_read(&self, path: &Path) -> std::io::Result<Box<dyn Read + Send>> {
        self.job.checkpoint(self.inner.as_ref());
        self.inner.open_read(path)
    }
    fn open_write(
        &self,
        path: &Path,
        mode: WriteMode,
        run: Option<&crate::host::Run>,
    ) -> std::io::Result<Box<dyn Write + Send>> {
        self.job.checkpoint(self.inner.as_ref());
        self.inner.open_write(path, mode, run)
    }
    fn read_link(&self, path: &Path) -> std::io::Result<PathBuf> {
        self.inner.read_link(path)
    }
    fn file_action(&self, call: &FileCall<'_>) -> i32 {
        self.job.checkpoint(self.inner.as_ref());
        if self.ended() {
            return 130;
        }
        self.inner.file_action(call)
    }
    fn program(
        &self,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        io: &Stdio,
    ) -> Program {
        self.job.checkpoint(self.inner.as_ref());
        if self.job.killed() {
            return Program::Exited(143);
        }
        self.inner.program_in_job(argv, env, cwd, io, &self.job)
    }
    fn program_until(
        &self,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        io: &Stdio,
        stop: &dyn Fn() -> bool,
    ) -> Program {
        self.job.checkpoint(self.inner.as_ref());
        self.inner
            .program_until(argv, env, cwd, io, &|| stop() || self.job.killed())
    }
    fn program_in_job(
        &self,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        io: &Stdio,
        job: &Arc<Job>,
    ) -> Program {
        self.inner.program_in_job(argv, env, cwd, io, job)
    }
    fn interrupted(&self) -> bool {
        self.job.checkpoint(self.inner.as_ref());
        self.ended()
    }
    fn clear_interrupt(&self) {
        if self.job.foreground() {
            self.inner.clear_interrupt();
        }
    }
    fn take_suspend(&self) -> bool {
        self.job.foreground() && self.inner.take_suspend()
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
        // In steps, so a stop, a Ctrl+C or a `kill` is seen within one.
        let mut left = millis;
        while left > 0 {
            self.job.checkpoint(self.inner.as_ref());
            if self.ended() {
                return;
            }
            let step = left.min(50);
            self.inner.sleep(step);
            left = left.saturating_sub(step);
        }
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
    fn history(&self, query: &HistoryQuery) -> Option<Vec<String>> {
        self.inner.history(query)
    }
    fn programs(&self, path: &str) -> Vec<String> {
        self.inner.programs(path)
    }
}

/// A job's output: it waits while the job is stopped, and fails once the
/// job was ended, so a command writing it stops. A background job's output
/// is bracketed by `marks` so Terminal keeps it with the job's run.
pub(crate) struct JobWriter {
    pub(crate) inner: crate::io::Output,
    pub(crate) job: Arc<Job>,
    pub(crate) host: Arc<dyn Host>,
    /// The marks around each write: what opens and what closes it.
    pub(crate) marks: Option<(String, String)>,
}

impl Write for JobWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.job.checkpoint(self.host.as_ref());
        if self.job.killed() {
            return Err(std::io::ErrorKind::BrokenPipe.into());
        }
        match &self.marks {
            Some((open, close)) => {
                let mut bytes = Vec::with_capacity(
                    buf.len()
                        .saturating_add(open.len())
                        .saturating_add(close.len()),
                );
                bytes.extend_from_slice(open.as_bytes());
                bytes.extend_from_slice(buf);
                bytes.extend_from_slice(close.as_bytes());
                self.inner.write_all(&bytes)?;
            }
            None => self.inner.write_all(buf)?,
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Terminal's run marks, shared by the shell and its jobs: each run, a
/// job's included, takes the next number.
pub(crate) struct Marks {
    pub(crate) key: String,
    count: std::sync::atomic::AtomicU64,
    /// The session's terminal, where the marks go.
    out: crate::io::Output,
}

impl Marks {
    pub(crate) fn new(key: String, out: crate::io::Output) -> Self {
        Self {
            key,
            count: std::sync::atomic::AtomicU64::new(0),
            out,
        }
    }

    /// The number the next run takes.
    pub(crate) fn next(&self) -> u64 {
        self.count.fetch_add(1, Ordering::SeqCst).saturating_add(1)
    }

    pub(crate) fn write(&self, mark: &str) {
        let _ = self.out.write_str(mark);
    }

    /// `C`: run `number` starts, `command` in `cwd`; `job` says it is a
    /// job's.
    pub(crate) fn started(&self, number: u64, command: &str, cwd: &Path, job: Option<u32>) {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        let job = job.map(|n| format!(";j={n}")).unwrap_or_default();
        self.write(&format!(
            "\x1b]133;C;k={};cmd={};cwd={};t={number}{job}\x07",
            self.key,
            b64.encode(command),
            b64.encode(cwd.to_string_lossy().as_bytes()),
        ));
    }

    /// `D`: run `number` ended with `status`.
    pub(crate) fn ended(&self, number: u64, status: i32, job: Option<u32>) {
        let job = job.map(|n| format!(";j={n}")).unwrap_or_default();
        self.write(&format!(
            "\x1b]133;D;{status};k={};t={number}{job}\x07",
            self.key
        ));
    }

    /// What goes around each write of job run `number`: `J` opens it, `E`
    /// closes it, so Terminal keeps it with that run.
    pub(crate) fn brackets(&self, number: u64) -> (String, String) {
        (
            format!("\x1b]133;J;k={};t={number}\x07", self.key),
            format!("\x1b]133;E;k={};t={number}\x07", self.key),
        )
    }
}

impl crate::shell::Shell {
    /// Lock the job table.
    pub(crate) fn jobs(&self) -> MutexGuard<'_, Table> {
        lock(&self.jobs)
    }

    /// The outputs of a job's copy of the shell, wrapped so they wait while
    /// it is stopped and fail once it was ended, with `marks` around each
    /// write. A stream that is the host's own stays so (a program run in
    /// the foreground gets the terminal itself).
    pub(crate) fn wrap_outputs(&mut self, job: &Arc<Job>, marks: Option<&(String, String)>) {
        for fd in [1_u32, 2] {
            let Some(output) = self.output(fd) else {
                continue;
            };
            let writer = JobWriter {
                inner: output.clone(),
                job: Arc::clone(job),
                host: Arc::clone(&self.host),
                marks: marks.cloned(),
            };
            let wrapped = match output.host_stream() {
                Some(stream) if marks.is_none() => crate::io::Output::host(stream, writer),
                _ => crate::io::Output::new(writer),
            };
            self.fds.insert(fd, crate::shell::Fd::Out(wrapped));
        }
    }

    /// `and_or &`: run it as a background job, on a copy of the shell, and
    /// go on at once.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn background(&mut self, and_or: &brush_parser::ast::AndOrList) -> i32 {
        // What `jobs` shows: on one line, as it was typed.
        let command = and_or.to_string().replace(";\n", "; ").replace('\n', " ");
        let pid = self.jobs().pid(self.host.pid());
        let job = Job::new(command.clone(), pid, false);
        let number = self.jobs().add(Arc::clone(&job));
        self.last_background = Some(pid);
        let mut sub = self.clone();
        sub.loop_depth = 0;
        sub.interactive = false;
        sub.host = Arc::new(JobHost {
            inner: Arc::clone(&self.host),
            job: Arc::clone(&job),
        });
        // A job in the background reads nothing from the terminal.
        if self
            .input(0)
            .is_some_and(|input| input.host_stream().is_some())
        {
            sub.fds
                .insert(0, crate::shell::Fd::In(crate::io::Input::null()));
        }
        let marks = self.marks.clone();
        let run_number = marks.as_ref().map(|m| m.next());
        let brackets = marks.as_ref().zip(run_number).map(|(m, n)| m.brackets(n));
        sub.wrap_outputs(&job, brackets.as_ref());
        // Its own run: what its file actions are stamped with, and the run
        // Terminal records for it.
        let session = self
            .run
            .as_ref()
            .map_or_else(|| format!("ush-{}", self.host.pid()), |run| run.id.clone());
        sub.set_run(Some(crate::host::Run {
            id: format!("{session}-job-{number}"),
            label: command.clone(),
        }));
        if let (Some(marks), Some(n)) = (&marks, run_number) {
            marks.started(n, &command, &self.cwd, Some(number));
        }
        if self.interactive {
            self.error_plain(&format!("[{number}] {pid}\n"));
        }
        let and_or = and_or.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("ush-job-{number}"))
            .spawn(move || {
                let status = match sub.run_and_or_owned(&and_or) {
                    Ok(status)
                    | Err(
                        crate::shell::Flow::Exit(status)
                        | crate::shell::Flow::Fatal(status)
                        | crate::shell::Flow::Return(status)
                        | crate::shell::Flow::Error(status),
                    ) => status,
                    Err(_) => sub.last_status,
                };
                let status = sub.exit_trap(status);
                drop(sub);
                job.finish(status);
                if let (Some(marks), Some(n)) = (marks, run_number) {
                    marks.ended(n, status, Some(number));
                }
            });
        if spawned.is_err() {
            self.error("cannot start a background job");
            self.jobs().remove(number);
            return 1;
        }
        0
    }

    /// Without threads (WebAssembly in a page), nothing runs beside the
    /// shell.
    #[cfg(target_family = "wasm")]
    pub(crate) fn background(&mut self, _: &brush_parser::ast::AndOrList) -> i32 {
        self.error(
            "running a command in the background (`&`) needs threads, which this host has none of",
        );
        2
    }

    /// Say `text` on standard error as it is.
    pub(crate) fn error_plain(&self, text: &str) {
        if let Some(err) = self.output(2) {
            let _ = err.write_str(text);
        }
    }

    /// Run `text` as the foreground job: on a thread of its own, so Ctrl+Z
    /// gives the prompt back with the line stopped. What the line changed
    /// in the shell stays when it ends; a stopped line keeps its changes
    /// with the job.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn run_in_foreground(&mut self, text: &str) -> crate::shell::Exec {
        let pid = self.jobs().pid(self.host.pid());
        let job = Job::new(text.trim_end_matches('\n'), pid, true);
        let host = Arc::clone(&self.host);
        let snapshot = self.clone();
        let mut worker = std::mem::replace(self, snapshot);
        let streams: Vec<(u32, crate::io::Output)> = [1, 2]
            .iter()
            .filter_map(|fd| Some((*fd, worker.output(*fd)?)))
            .collect();
        worker.host = Arc::new(JobHost {
            inner: Arc::clone(&host),
            job: Arc::clone(&job),
        });
        worker.wrap_outputs(&job, None);
        let wrapped: Vec<(u32, crate::io::Output)> = [1, 2]
            .iter()
            .filter_map(|fd| Some((*fd, worker.output(*fd)?)))
            .collect();
        let back: Arc<Mutex<Option<(crate::shell::Shell, crate::shell::Exec)>>> = Arc::default();
        let slot = Arc::clone(&back);
        let line = text.to_owned();
        let finished = Arc::clone(&job);
        let spawned = std::thread::Builder::new()
            .name("ush-line".into())
            .spawn(move || {
                let result = worker.eval_text(&line);
                let status = match &result {
                    Ok(s) => *s,
                    Err(_) => worker.last_status,
                };
                *lock(&slot) = Some((worker, result));
                finished.finish(status);
            });
        if spawned.is_err() {
            // No thread to run it on: run it here, as a script would.
            return self.eval_text(text);
        }
        match job.wait_settled() {
            Phase::Done(_) => {
                let Some((mut worker, result)) = lock(&back).take() else {
                    return Ok(1);
                };
                worker.host = host;
                // Streams the line left as they were go back to the
                // shell's own; one it redirected for good (`exec >log`)
                // stays.
                for ((fd, original), (_, wrapped)) in streams.into_iter().zip(wrapped) {
                    if worker.output(fd).is_some_and(|now| now.same(&wrapped)) {
                        worker.fds.insert(fd, crate::shell::Fd::Out(original));
                    }
                }
                worker.editor = Arc::clone(&self.editor);
                *self = worker;
                result
            }
            Phase::Stopped | Phase::Running => {
                job.set_foreground(false);
                let number = self.jobs().add(Arc::clone(&job));
                self.error_plain(&format!(
                    "\n[{number}]+  Stopped                 {}\n",
                    job.command
                ));
                Ok(148)
            }
        }
    }

    #[cfg(target_family = "wasm")]
    pub(crate) fn run_in_foreground(&mut self, text: &str) -> crate::shell::Exec {
        self.eval_text(text)
    }

    /// Say which jobs ended since the last prompt, and forget them.
    pub(crate) fn report_jobs(&self) {
        let done: Vec<(u32, Arc<Job>, char)> = {
            let table = self.jobs();
            table
                .all()
                .into_iter()
                .filter(|(_, job)| matches!(job.phase(), Phase::Done(_)))
                .map(|(n, job)| {
                    let mark = table.mark(n);
                    (n, job, mark)
                })
                .collect()
        };
        for (number, job, mark) in done {
            if self.interactive {
                let status = match job.phase() {
                    Phase::Done(0) => "Done".to_owned(),
                    Phase::Done(s) => format!("Exit {s}"),
                    _ => String::new(),
                };
                self.error_plain(&format!("[{number}]{mark}  {status:<24}{}\n", job.command));
            }
            self.jobs().remove(number);
        }
    }
}

/// The line `jobs` shows for one job.
fn describe(number: u32, mark: char, job: &Job, with_pid: bool) -> String {
    let state = match job.phase() {
        Phase::Running => "Running".to_owned(),
        Phase::Stopped => "Stopped".to_owned(),
        Phase::Done(0) => "Done".to_owned(),
        Phase::Done(status) => format!("Exit {status}"),
    };
    let pid = if with_pid {
        format!("{} ", job.pid)
    } else {
        String::new()
    };
    let amp = if job.phase() == Phase::Running && !job.foreground() {
        " &"
    } else {
        ""
    };
    format!("[{number}]{mark}  {pid}{state:<24}{}{amp}\n", job.command)
}

/// `jobs [-l|-p] [SPEC…]`.
#[expect(
    clippy::unnecessary_wraps,
    reason = "a `Builtin`: every built-in returns `Exec`"
)]
pub(crate) fn jobs_builtin(sh: &mut crate::shell::Shell, args: &[String]) -> crate::shell::Exec {
    let mut with_pid = false;
    let mut pids_only = false;
    let mut specs = Vec::new();
    for arg in args {
        match arg.as_str() {
            "-l" => with_pid = true,
            "-p" => pids_only = true,
            flag if flag.starts_with('-') && flag.len() > 1 => {
                sh.error(format!("jobs: {flag}: invalid option"));
                return Ok(2);
            }
            spec => specs.push(spec.to_owned()),
        }
    }
    let table = sh.jobs();
    let listed: Vec<(u32, Arc<Job>)> = if specs.is_empty() {
        table.all()
    } else {
        let mut found = Vec::new();
        for spec in &specs {
            match table.find(spec) {
                Ok(job) => found.push(job),
                Err(e) => {
                    drop(table);
                    sh.error(format!("jobs: {e}"));
                    return Ok(1);
                }
            }
        }
        found
    };
    let mut out = String::new();
    for (number, job) in &listed {
        if pids_only {
            let _ = writeln!(out, "{}", job.pid);
        } else {
            out.push_str(&describe(*number, table.mark(*number), job, with_pid));
        }
    }
    let done: Vec<u32> = listed
        .iter()
        .filter(|(_, job)| matches!(job.phase(), Phase::Done(_)))
        .map(|(n, _)| *n)
        .collect();
    drop(table);
    sh.out(&out);
    for number in done {
        sh.jobs().remove(number);
    }
    Ok(0)
}

/// The job a `fg` or `bg` operand names, or the current one.
fn job_of(sh: &crate::shell::Shell, name: &str, args: &[String]) -> Result<(u32, Arc<Job>), i32> {
    let spec = args.first().map_or("%+", String::as_str);
    sh.jobs().find(spec).map_err(|e| {
        sh.error(format!("{name}: {e}"));
        1
    })
}

/// `fg [SPEC]`: bring a job to the foreground and wait for it.
#[expect(
    clippy::unnecessary_wraps,
    reason = "a `Builtin`: every built-in returns `Exec`"
)]
pub(crate) fn fg_builtin(sh: &mut crate::shell::Shell, args: &[String]) -> crate::shell::Exec {
    let (number, job) = match job_of(sh, "fg", args) {
        Ok(found) => found,
        Err(status) => return Ok(status),
    };
    sh.error_plain(&format!("{}\n", job.command));
    sh.jobs().make_current(number);
    if let Phase::Done(status) = job.phase() {
        sh.jobs().remove(number);
        return Ok(status);
    }
    job.resume(true);
    if let Phase::Done(status) = job.wait_settled() {
        sh.jobs().remove(number);
        return Ok(status);
    }
    job.set_foreground(false);
    sh.error_plain(&format!(
        "\n[{number}]+  Stopped                 {}\n",
        job.command
    ));
    Ok(148)
}

/// `bg [SPEC…]`: let stopped jobs go on in the background.
#[expect(
    clippy::unnecessary_wraps,
    reason = "a `Builtin`: every built-in returns `Exec`"
)]
pub(crate) fn bg_builtin(sh: &mut crate::shell::Shell, args: &[String]) -> crate::shell::Exec {
    let specs: Vec<String> = if args.is_empty() {
        vec!["%+".into()]
    } else {
        args.to_vec()
    };
    let mut status = 0;
    for spec in specs {
        match job_of(sh, "bg", std::slice::from_ref(&spec)) {
            Ok((number, job)) => {
                if job.phase() == Phase::Running && !job.foreground() {
                    sh.error(format!("bg: job {number} already in background"));
                    continue;
                }
                job.resume(false);
                let mark = sh.jobs().mark(number);
                sh.error_plain(&format!("[{number}]{mark} {} &\n", job.command));
            }
            Err(s) => status = s,
        }
    }
    Ok(status)
}

/// `wait [SPEC…]`: wait for jobs to end; every running one without operands.
/// Ctrl+C stops the waiting, not the jobs.
#[expect(
    clippy::unnecessary_wraps,
    reason = "a `Builtin`: every built-in returns `Exec`"
)]
pub(crate) fn wait_builtin(sh: &mut crate::shell::Shell, args: &[String]) -> crate::shell::Exec {
    let named: Vec<(Option<u32>, Option<Arc<Job>>)> = if args.is_empty() {
        sh.jobs()
            .all()
            .into_iter()
            .filter(|(_, job)| job.phase() != Phase::Stopped)
            .map(|(n, job)| (Some(n), Some(job)))
            .collect()
    } else {
        args.iter()
            .filter(|a| a.as_str() != "--")
            .map(|spec| {
                let found = sh.jobs().find(spec);
                found.map_or((None, None), |(n, job)| (Some(n), Some(job)))
            })
            .collect()
    };
    let mut status = 0;
    for (number, job) in named {
        let Some(job) = job else {
            // Not a job of this shell's: bash answers 127.
            status = 127;
            continue;
        };
        status = loop {
            match job.wait_settled_for(50) {
                Phase::Done(s) => break s,
                Phase::Stopped if !args.is_empty() => break 148,
                _ => {}
            }
            if sh.host.interrupted() {
                return Ok(130);
            }
        };
        if let (Some(number), false) = (number, sh.interactive) {
            sh.jobs().remove(number);
        } else if let Some(number) = number
            && !args.is_empty()
        {
            sh.jobs().remove(number);
        }
    }
    Ok(if args.is_empty() { 0 } else { status })
}

/// What a `kill` signal name or number asks of a job.
fn signal_named(name: &str) -> Option<Signal> {
    let name = name.trim_start_matches("SIG").to_ascii_uppercase();
    Some(match name.as_str() {
        "TERM" | "15" | "HUP" | "1" | "QUIT" | "3" | "USR1" | "10" | "USR2" | "12" => {
            Signal::Terminate
        }
        "KILL" | "9" => Signal::Kill,
        "INT" | "2" => Signal::Interrupt,
        "STOP" | "19" | "TSTP" | "20" | "TTIN" | "21" | "TTOU" | "22" => Signal::Stop,
        "CONT" | "18" => Signal::Continue,
        _ => return None,
    })
}

/// `kill [-s SIG | -SIG] TARGET…`: a target naming one of the shell's jobs
/// (`%n`, or the number `$!` gave) is the shell's to signal; anything else
/// is the host's `kill`.
#[expect(
    clippy::unnecessary_wraps,
    reason = "a `Builtin`: every built-in returns `Exec`"
)]
pub(crate) fn kill_builtin(sh: &mut crate::shell::Shell, args: &[String]) -> crate::shell::Exec {
    let mut signal = "TERM".to_owned();
    let mut rest = args;
    match args.first().map(String::as_str) {
        Some("-l" | "-L") => return Ok(sh.run_other(&prefixed("kill", args))),
        Some("-s" | "-n") => {
            let Some(name) = args.get(1) else {
                sh.error("kill: -s: option requires an argument");
                return Ok(2);
            };
            signal.clone_from(name);
            rest = args.get(2..).unwrap_or(&[]);
        }
        Some(flag) if flag.starts_with('-') && flag.len() > 1 && flag != "--" => {
            flag.trim_start_matches('-').clone_into(&mut signal);
            rest = args.get(1..).unwrap_or(&[]);
        }
        _ => {}
    }
    if rest.first().is_some_and(|a| a == "--") {
        rest = rest.get(1..).unwrap_or(&[]);
    }
    let Some(asked) = signal_named(&signal) else {
        // A signal the shell's jobs have no meaning for: the host's `kill`.
        return Ok(sh.run_other(&prefixed("kill", args)));
    };
    let mut status = 0;
    let mut others = Vec::new();
    for target in rest {
        let found = sh.jobs().find(target);
        match found {
            Ok((_, job)) => match asked {
                Signal::Stop => job.stop(),
                Signal::Continue => job.resume(job.foreground()),
                end => job.kill(end),
            },
            Err(e) if target.starts_with('%') => {
                sh.error(format!("kill: {e}"));
                status = 1;
            }
            Err(_) => others.push(target.clone()),
        }
    }
    if !others.is_empty() {
        let mut line = vec!["kill".to_owned(), format!("-{signal}")];
        line.extend(others);
        let other = sh.run_other(&line);
        if other != 0 {
            status = other;
        }
    }
    Ok(status)
}

fn prefixed(name: &str, args: &[String]) -> Vec<String> {
    let mut line = vec![name.to_owned()];
    line.extend(args.iter().cloned());
    line
}
