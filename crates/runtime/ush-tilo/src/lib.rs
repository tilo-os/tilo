//! ush's shell on the Tilo side: a [`Host`] that reaches nothing but the
//! environment it runs for.
//!
//! The interpreter ([`ush_shell`]) touches nothing itself; this host decides
//! what its calls mean, and every one of them goes through Tilo:
//!
//! - **Reads** (`ls`, `cat`, `grep`, a `<` redirect) are asked of the
//!   environment's grants first, with the same check every grant of an
//!   environment is asked with ([`tilo_environment::files::permits`], which
//!   is `decide_capability`). A path no grant covers is refused before
//!   anything is read, and so is one a symbolic link leads out of a grant:
//!   the question is asked of the path as written and of the file it reaches.
//! - **Writes** are the environment's actions, all of them: the file
//!   commands (`cp`, `mv`, `rm`, …) and a redirect alike. `> file` is the
//!   record's `write`, `>> file` its `append`, each checked against the
//!   grants by the verb, recorded in History and undone from there. The
//!   writes of one command line carry its [`Run`], so History groups them as
//!   one. A redirect of any size is one action: past
//!   [`MAX_TEXT_BYTES`](tilo_environment::files::MAX_TEXT_BYTES), what the
//!   command writes is streamed into the environment's content store
//!   ([`Environment::keep`]) and the action names it by reference.
//! - **A program** that is none of the shell's own goes to [`Programs`]: the
//!   place that can run it (a Linux guest for the session, tier 3). This host
//!   never starts a process itself. As part of one of the shell's jobs, the
//!   place stops, continues and ends it with the job; Tab completes the
//!   names of the programs the place has.
//! - **At the prompt**, the line editor's terminal (raw keys, its width) and
//!   Ctrl+Z are the place's ([`Place`]); the person's history is the
//!   place's when it holds the record, else the line editor asks the app
//!   showing the session.
//!
//! Nothing here assumes Linux. Where the granted files are ([`Files`]), how
//! actions reach the record ([`Environment`]), where programs run
//! ([`Programs`]) and what the place offers besides (a clock, Ctrl-C:
//! [`Place`]) are the caller's, so the same host runs on a device, on a
//! workspace's host, or in a browser. [`NativeFiles`] reads files through
//! the standard library, for a place that has them on disk.

mod native;

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use collab_types::caps::Access;
/// What [`Environment::grants`] answers: the record's grants.
pub use collab_types::caps::CapOverride;
use tilo_environment::files::{
    AppendFileOp, FileAction, MAX_TEXT_BYTES, WriteOp, anchored, cli, normalize, permits,
    permits_in, ungranted,
};
use ush_shell::jobs::Job;
use ush_shell::{
    DirEntry, FileCall, HistoryQuery, Host, Metadata, Output, Program, Run, Stdio, WriteMode,
};

pub use native::NativeFiles;

/// The environment the shell runs for: its grants, and its file actions.
pub trait Environment: Send + Sync {
    /// The record's grants as they stand now.
    fn grants(&self) -> Vec<CapOverride>;

    /// Invoke `action` on the record as part of `run`: checked against its
    /// grants, recorded in its History, and carried out.
    ///
    /// # Errors
    ///
    /// Why it was refused or did not get carried out, in words.
    fn act(&self, action: FileAction, run: Option<&Run>) -> Result<Acted, String>;

    /// Start keeping content in the environment's content store, for an
    /// action to name by reference: what is written to the answer is
    /// streamed there, whatever its size, and [`Keeping::finish`] says what
    /// it was kept as.
    ///
    /// # Errors
    ///
    /// The store could not be written to.
    fn keep(&self) -> io::Result<Box<dyn Keeping>>;
}

/// Content on its way into the environment's content store.
pub trait Keeping: Write + Send {
    /// Everything was written: the content as kept.
    ///
    /// # Errors
    ///
    /// The store could not keep it.
    fn finish(self: Box<Self>) -> io::Result<Kept>;
}

/// Content kept in the environment's content store.
pub struct Kept {
    /// Its hash (BLAKE3, lower-case hex): what an action names it by.
    pub hash: String,
    /// What keeps the content from being collected until the action that
    /// names it is recorded: dropped after that.
    pub hold: Option<Box<dyn Send>>,
}

/// What an action did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Acted {
    /// The record's entry for it, what History undoes.
    pub entry: Option<String>,
    /// How many operations carrying it out took.
    pub operations: usize,
}

/// Where the granted files are read from. Asked only about paths the grants
/// already allow.
pub trait Files: Send + Sync {
    fn metadata(&self, path: &Path) -> io::Result<Metadata>;
    fn symlink_metadata(&self, path: &Path) -> io::Result<Metadata>;
    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>>;
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>>;
    fn read_link(&self, path: &Path) -> io::Result<PathBuf>;
    /// `path` with every symbolic link in it followed: the file a read
    /// would reach.
    fn resolve(&self, path: &Path) -> io::Result<PathBuf>;
}

/// Where a program that is none of the shell's own runs.
pub trait Programs: Send + Sync {
    /// Run `argv` with `env` as its whole environment, in `cwd`, on `io`,
    /// until it ends, or until `stop` says to stop it.
    fn run(
        &self,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        io: &Stdio,
        stop: Option<&dyn Fn() -> bool>,
    ) -> Program;

    /// [`Self::run`] as part of the shell's `job`: the place attaches to
    /// the job what stops, continues and ends the program
    /// ([`Job::attach`]), and tells the job when the program stopped. A
    /// place that cannot signal its programs can only end them.
    fn run_in_job(
        &self,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        io: &Stdio,
        job: &Arc<Job>,
    ) -> Program {
        self.run(argv, env, cwd, io, Some(&|| job.killed()))
    }

    /// The programs there are to run, by name, looked for along `path`:
    /// what Tab completes in a command's place.
    fn names(&self, path: &str) -> Vec<String> {
        let _ = path;
        Vec::new()
    }
}

/// No place to run programs: each says which tier it would need.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPrograms;

impl Programs for NoPrograms {
    fn run(
        &self,
        _: &[String],
        _: &[(String, String)],
        _: &Path,
        _: &Stdio,
        _: Option<&dyn Fn() -> bool>,
    ) -> Program {
        Program::NeedsTier
    }
}

/// What the place running the shell offers besides files and programs. Each
/// has the answer of a place that offers nothing.
pub trait Place: Send + Sync {
    /// Whether the person asked to stop what is running (Ctrl-C).
    fn interrupted(&self) -> bool {
        false
    }

    fn clear_interrupt(&self) {}

    /// Seconds since the Unix epoch.
    fn now(&self) -> i64 {
        0
    }

    fn now_ms(&self) -> i64 {
        self.now().saturating_mul(1000)
    }

    fn utc_offset(&self, _at: i64) -> i32 {
        0
    }

    fn sleep(&self, _millis: u64) {}

    fn user_name(&self, _uid: u32) -> Option<String> {
        None
    }

    fn group_name(&self, _gid: u32) -> Option<String> {
        None
    }

    fn pid(&self) -> u32 {
        1
    }

    fn hostname(&self) -> String {
        String::from("localhost")
    }

    /// Whether the person pressed Ctrl+Z since the last call.
    fn take_suspend(&self) -> bool {
        false
    }

    /// Switch the session's terminal to taking keys one at a time for the
    /// line editor, or back ([`Host::edit_mode`]). Answers whether there is
    /// a terminal to edit on.
    fn edit_mode(&self, on: bool) -> bool {
        let _ = on;
        false
    }

    /// The session's terminal's width in columns.
    fn columns(&self) -> usize {
        80
    }

    /// The person's command history (`terminal.history`), when this place
    /// holds the record ([`Host::history`]). `None` where the app showing
    /// the session holds it: the line editor asks it on the session's
    /// terminal.
    fn history(&self, query: &HistoryQuery) -> Option<Vec<String>> {
        let _ = query;
        None
    }

    /// The app showing the session, editing the line at the prompt as a
    /// field ([`Host::field`]).
    fn field(&self) -> Option<Arc<dyn ush_shell::field::Field>> {
        None
    }
}

/// A place that offers nothing beyond files and programs.
#[derive(Debug, Clone, Copy, Default)]
pub struct Nowhere;

impl Place for Nowhere {}

struct Inner {
    environment: Arc<dyn Environment>,
    files: Arc<dyn Files>,
    programs: Arc<dyn Programs>,
    place: Arc<dyn Place>,
    /// Where a write that could not be made after its command ended says
    /// so: the session's standard error.
    notices: Output,
}

/// The shell's host on the Tilo side. Cheap to clone.
#[derive(Clone)]
pub struct TiloHost(Arc<Inner>);

impl TiloHost {
    /// A host for the shell of `environment`, reading its granted files from
    /// `files`, running programs in `programs`, and saying on `notices` what
    /// a redirect could not write.
    pub fn new(
        environment: Arc<dyn Environment>,
        files: Arc<dyn Files>,
        programs: Arc<dyn Programs>,
        place: Arc<dyn Place>,
        notices: Output,
    ) -> Self {
        Self(Arc::new(Inner {
            environment,
            files,
            programs,
            place,
            notices,
        }))
    }

    fn now(&self) -> Option<u64> {
        u64::try_from(self.0.place.now())
            .ok()
            .filter(|now| *now > 0)
    }

    /// The path a read of `path` reaches, once the grants allow both it and
    /// the path as written. With `follow`, a symbolic link at the end is
    /// followed too; without, only the directories leading to it are.
    fn readable(&self, path: &Path, follow: bool) -> io::Result<PathBuf> {
        let grants = self.0.environment.grants();
        let now = self.now();
        let written = normalize(path);
        let allowed = |path: &Path| permits(&grants, path, Access::Read, now);
        if !allowed(&written) {
            return Err(refused(path));
        }
        let reached = if follow {
            self.0.files.resolve(&written)?
        } else {
            match (written.parent(), written.file_name()) {
                (Some(parent), Some(name)) => self.0.files.resolve(parent)?.join(name),
                _ => self.0.files.resolve(&written)?,
            }
        };
        if !allowed(&reached) {
            return Err(refused(path));
        }
        Ok(reached)
    }
}

/// The refusal of a path no grant covers.
fn refused(path: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, ungranted(path))
}

impl Host for TiloHost {
    fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        let path = self.readable(path, true)?;
        self.0.files.metadata(&path)
    }

    fn symlink_metadata(&self, path: &Path) -> io::Result<Metadata> {
        let path = self.readable(path, false)?;
        self.0.files.symlink_metadata(&path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        let path = self.readable(path, true)?;
        self.0.files.read_dir(&path)
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        let path = self.readable(path, true)?;
        self.0.files.open_read(&path)
    }

    fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        let path = self.readable(path, false)?;
        self.0.files.read_link(&path)
    }

    fn open_write(
        &self,
        path: &Path,
        mode: WriteMode,
        run: Option<&Run>,
    ) -> io::Result<Box<dyn Write + Send>> {
        let path = normalize(path);
        // Asked now, so a redirect outside the grants fails before its
        // command runs; the action is asked again, as a whole, when it is
        // invoked.
        if !permits_in(
            &self.0.environment.grants(),
            &path,
            Access::Write,
            self.now(),
        ) {
            return Err(refused(&path));
        }
        if mode == WriteMode::CreateNew && self.0.files.symlink_metadata(&path).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "cannot overwrite existing file",
            ));
        }
        Ok(Box::new(Redirect {
            host: self.clone(),
            path,
            mode,
            run: run.cloned(),
            bytes: Vec::new(),
            keeping: None,
        }))
    }

    fn file_action(&self, call: &FileCall<'_>) -> i32 {
        let mut out = OutputWriter(call.io.stdout.clone());
        let mut err = OutputWriter(call.io.stderr.clone());
        let Some(tool) = cli::Tool::of(call.name) else {
            let _ = writeln!(err, "{}: not one of the environment's actions", call.name);
            return 1;
        };
        let (json, args) = cli::take_json(call.args);
        let action = match cli::parse(tool, &args) {
            Ok(action) => anchored(action, call.cwd),
            Err(error) => {
                return cli::answer_refused(
                    &mut out,
                    &mut err,
                    json,
                    call.name,
                    &error.to_string(),
                );
            }
        };
        let verb = action.verb();
        match self.0.environment.act(action, call.run) {
            Ok(acted) => cli::answer_done(
                &mut out,
                json,
                verb,
                acted.entry.as_deref(),
                acted.operations,
            ),
            Err(reason) => cli::answer_refused(&mut out, &mut err, json, call.name, &reason),
        }
    }

    fn program(
        &self,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        io: &Stdio,
    ) -> Program {
        self.0.programs.run(argv, env, cwd, io, None)
    }

    fn program_until(
        &self,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        io: &Stdio,
        stop: &dyn Fn() -> bool,
    ) -> Program {
        self.0.programs.run(argv, env, cwd, io, Some(stop))
    }

    fn program_in_job(
        &self,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        io: &Stdio,
        job: &Arc<Job>,
    ) -> Program {
        self.0.programs.run_in_job(argv, env, cwd, io, job)
    }

    fn take_suspend(&self) -> bool {
        self.0.place.take_suspend()
    }

    fn interrupted(&self) -> bool {
        self.0.place.interrupted()
    }

    fn clear_interrupt(&self) {
        self.0.place.clear_interrupt();
    }

    fn now(&self) -> i64 {
        self.0.place.now()
    }

    fn now_ms(&self) -> i64 {
        self.0.place.now_ms()
    }

    fn utc_offset(&self, at: i64) -> i32 {
        self.0.place.utc_offset(at)
    }

    fn sleep(&self, millis: u64) {
        self.0.place.sleep(millis);
    }

    fn user_name(&self, uid: u32) -> Option<String> {
        self.0.place.user_name(uid)
    }

    fn group_name(&self, gid: u32) -> Option<String> {
        self.0.place.group_name(gid)
    }

    fn pid(&self) -> u32 {
        self.0.place.pid()
    }

    fn hostname(&self) -> String {
        self.0.place.hostname()
    }

    fn edit_mode(&self, on: bool) -> bool {
        self.0.place.edit_mode(on)
    }

    fn columns(&self) -> usize {
        self.0.place.columns()
    }

    fn history(&self, query: &HistoryQuery) -> Option<Vec<String>> {
        self.0.place.history(query)
    }

    fn programs(&self, path: &str) -> Vec<String> {
        self.0.programs.names(path)
    }

    fn field(&self) -> Option<Arc<dyn ush_shell::field::Field>> {
        self.0.place.field()
    }
}

/// A redirect's file: what the command writes is gathered, and becomes one
/// action of the environment's when the redirect closes. Up to
/// [`MAX_TEXT_BYTES`] it travels inside the action; past that it is
/// streamed into the environment's content store as it is written, and the
/// action names it by reference, so a redirect of any size is one action.
struct Redirect {
    host: TiloHost,
    path: PathBuf,
    mode: WriteMode,
    run: Option<Run>,
    bytes: Vec<u8>,
    /// Where what was written goes once it is too much to carry.
    keeping: Option<Box<dyn Keeping>>,
}

impl Redirect {
    /// Keep what was written so far, and everything after it, in the
    /// content store.
    fn spill(&mut self) -> io::Result<&mut Box<dyn Keeping>> {
        if self.keeping.is_none() {
            let mut keeping = self.host.0.environment.keep()?;
            keeping.write_all(&self.bytes)?;
            self.bytes = Vec::new();
            self.keeping = Some(keeping);
        }
        self.keeping
            .as_mut()
            .ok_or_else(|| io::Error::other("the content store went away"))
    }

    /// The action the redirect is, for what was written, and what holds
    /// its content until the action is recorded.
    fn action(&mut self) -> Result<(FileAction, Option<Kept>), String> {
        let files = vec![self.path.clone()];
        let append = self.mode == WriteMode::Append;
        // `append` carries text: bytes that are not text go by reference.
        if append && !self.bytes.is_empty() && std::str::from_utf8(&self.bytes).is_err() {
            self.spill()
                .map_err(|e| ush_shell::commands::describe(&e))?;
        }
        if let Some(keeping) = self.keeping.take() {
            let kept = keeping
                .finish()
                .map_err(|e| ush_shell::commands::describe(&e))?;
            let hash = kept.hash.clone();
            let action = match self.mode {
                WriteMode::Truncate => FileAction::Write(WriteOp::of_content(files, hash, false)),
                WriteMode::CreateNew => FileAction::Write(WriteOp::of_content(files, hash, true)),
                WriteMode::Append => FileAction::Append(AppendFileOp {
                    files,
                    text: String::new(),
                    content: Some(hash),
                }),
            };
            return Ok((action, Some(kept)));
        }
        let bytes = std::mem::take(&mut self.bytes);
        Ok((
            match self.mode {
                WriteMode::Truncate => FileAction::Write(WriteOp::of(files, bytes, false)),
                WriteMode::CreateNew => FileAction::Write(WriteOp::of(files, bytes, true)),
                // `>>` with nothing written still makes the file, as `touch`.
                WriteMode::Append if bytes.is_empty() => FileAction::Touch(AppendFileOp {
                    files,
                    ..AppendFileOp::default()
                }),
                WriteMode::Append => FileAction::Append(AppendFileOp {
                    files,
                    text: String::from_utf8(bytes).map_err(|e| e.to_string())?,
                    content: None,
                }),
            },
            None,
        ))
    }
}

impl Write for Redirect {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.keeping.is_none() && self.bytes.len().saturating_add(buf.len()) <= MAX_TEXT_BYTES {
            self.bytes.extend_from_slice(buf);
            return Ok(buf.len());
        }
        self.spill()?.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for Redirect {
    fn drop(&mut self) {
        let done = self.action().and_then(|(action, kept)| {
            let acted = self
                .host
                .0
                .environment
                .act(action, self.run.as_ref())
                .map(drop);
            // Held until the action that names the content is recorded.
            drop(kept);
            acted
        });
        if let Err(reason) = done {
            let _ = self.host.0.notices.write_str(&format!(
                "ush: {}: not written: {reason}\n",
                self.path.display()
            ));
        }
    }
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

#[cfg(test)]
mod tests;
