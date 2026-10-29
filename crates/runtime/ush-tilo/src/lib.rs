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
//!   one.
//! - **A program** that is none of the shell's own goes to [`Programs`]: the
//!   place that can run it (a Linux guest for the session, tier 3). This host
//!   never starts a process itself.
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
    AppendFileOp, FileAction, WriteOp, anchored, cli, normalize, permits, permits_in, ungranted,
};
use ush_shell::{DirEntry, FileCall, Host, Metadata, Output, Program, Run, Stdio, WriteMode};

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
}

/// A redirect's file: what the command writes is gathered, and becomes one
/// action of the environment's when the redirect closes.
struct Redirect {
    host: TiloHost,
    path: PathBuf,
    mode: WriteMode,
    run: Option<Run>,
    bytes: Vec<u8>,
}

impl Redirect {
    /// The action the redirect is, for what was written.
    fn action(&mut self) -> Result<FileAction, String> {
        let bytes = std::mem::take(&mut self.bytes);
        let files = vec![self.path.clone()];
        Ok(match self.mode {
            WriteMode::Truncate => FileAction::Write(WriteOp::of(files, bytes, false)),
            WriteMode::CreateNew => FileAction::Write(WriteOp::of(files, bytes, true)),
            // `>>` with nothing written still makes the file, as `touch`.
            WriteMode::Append if bytes.is_empty() => FileAction::Touch(AppendFileOp {
                files,
                text: String::new(),
            }),
            WriteMode::Append => match String::from_utf8(bytes) {
                Ok(text) => FileAction::Append(AppendFileOp { files, text }),
                // `append` carries text; bytes that are not text are added
                // by writing the whole file, what it held and then them.
                Err(error) => {
                    let mut whole = match self.host.open_read(&self.path) {
                        Ok(mut held) => {
                            let mut whole = Vec::new();
                            held.read_to_end(&mut whole)
                                .map_err(|e| ush_shell::commands::describe(&e))?;
                            whole
                        }
                        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
                        Err(e) => return Err(ush_shell::commands::describe(&e)),
                    };
                    whole.extend_from_slice(error.as_bytes());
                    FileAction::Write(WriteOp::of(files, whole, false))
                }
            },
        })
    }
}

impl Write for Redirect {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for Redirect {
    fn drop(&mut self) {
        let done = self.action().and_then(|action| {
            self.host
                .0
                .environment
                .act(action, self.run.as_ref())
                .map(drop)
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
