//! A served run in a workspace's machine, from its host's side
//! ([`legacy_agent_protocol::served`]).
//!
//! A command line a workspace's shell runs gets its files and the internet.
//! A program that needs what only its host may hold (a person's model, the
//! workspace's actions, a sign-in) is started here instead: in the
//! workspace's own machine, admitted and paid for like any Linux program of
//! the workspace, through the guest agent's serving mode. What comes back is
//! the run's channel: the frames it sends ([`ServedRun::hear`]) and the
//! frames it is sent ([`ServedRun::tell`]).
//!
//! The run lasts as long as its program does: nothing here bounds it, and
//! the machine is not put to sleep under a program that runs. It ends when
//! the program ends, when the host asks it to ([`ToRun::Stop`]), when the
//! host lets go of it ([`ServedRun::leave`]) and when its handle is dropped.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use legacy_agent_protocol::served::{self, FromRun, ToRun};
use legacy_agent_protocol::{recv, send};
use tilo_actions::remote::host_account::Refusal;
use ush_shell::io::{PipeWriter, pipe};
use ush_shell::{Input, Output, Program, Stdio};
use ush_tilo::Programs as _;

use crate::workspace_shell::{Payer, WorkspaceShells};

/// The frames of a served run and what both of its sides agree on, for a
/// host that holds one.
pub mod wire {
    pub use legacy_agent_protocol::served::*;
    pub use legacy_agent_protocol::{ExitStatus, recv, send};
}

/// A program to start in a workspace's machine, served by its host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServeRun {
    /// The program and its arguments, as the machine finds them. An
    /// endpoint's [`served::placeholder`] in an argument is replaced by its
    /// address in the machine.
    pub argv: Vec<String>,
    /// Variables beside the ones every program in the machine starts with;
    /// a placeholder in a value is replaced as in an argument.
    pub env: Vec<(String, String)>,
    /// Where it starts: a folder of the workspace's files when relative, a
    /// path of the machine's own when absolute.
    pub directory: PathBuf,
    /// The endpoints the run asks its host at, by name.
    pub endpoints: Vec<String>,
    /// How long the program is given to end once asked.
    pub stop_grace: Duration,
    /// The agent program in the machine: [`served::AGENT_PROGRAM`] wherever
    /// the machine is a hosted guest.
    pub agent_program: String,
}

/// How a served run went, once it is over.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Over {
    /// The launcher's exit status, as a shell reads one: the program's own
    /// once it ran.
    pub exit: i32,
    /// Why the host would not give the run a machine, typed.
    pub refusal: Option<Refusal>,
    /// What was said beside the frames: the launcher's own words, and this
    /// host's about the machine.
    pub words: String,
}

/// What a served run's host hears of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Heard {
    Frame(FromRun),
    /// The run is over, however it ended; nothing follows.
    Over(Over),
}

/// A served run, held by its host.
pub struct ServedRun {
    /// The frames to the run; none once the host let go of it, and once the
    /// run is over.
    telling: Arc<Mutex<Option<PipeWriter>>>,
    heard: Mutex<Receiver<Heard>>,
    /// Ends the run without asking.
    cut: Arc<AtomicBool>,
}

impl ServedRun {
    /// Send `frame` to the run; `false` once it is over or let go of.
    pub fn tell(&self, frame: &ToRun) -> bool {
        let mut telling = self.telling.lock().unwrap_or_else(PoisonError::into_inner);
        let told = telling
            .as_mut()
            .is_some_and(|writer| send(writer, frame).is_ok());
        if !told {
            *telling = None;
        }
        drop(telling);
        told
    }

    /// What the run says next, waited for at most `wait`; `None` when it
    /// said nothing in that time.
    pub fn hear(&self, wait: Duration) -> Option<Heard> {
        let heard = self.heard.lock().unwrap_or_else(PoisonError::into_inner);
        let next = heard.recv_timeout(wait);
        drop(heard);
        match next {
            Ok(heard) => Some(heard),
            Err(RecvTimeoutError::Timeout) => None,
            // Its thread ended without saying how: over all the same.
            Err(RecvTimeoutError::Disconnected) => Some(Heard::Over(Over {
                exit: 1,
                refusal: None,
                words: "the run ended without saying how".into(),
            })),
        }
    }

    /// Let go of the run: its frames end, which has the program asked to
    /// end at once. What it still says is heard until it is over.
    pub fn leave(&self) {
        *self.telling.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// End the run now, without asking the program.
    pub fn cut(&self) {
        self.cut.store(true, Ordering::SeqCst);
        self.leave();
    }
}

impl Drop for ServedRun {
    fn drop(&mut self) {
        // A handle nobody holds is a host that went away.
        self.cut();
    }
}

/// `directory` as a path in the machine: below the workspace's `files` when
/// relative, and never above them by a `..`; its own when absolute.
fn place(files: &Path, directory: &Path) -> Option<PathBuf> {
    if directory.is_absolute() {
        return directory
            .components()
            .all(|part| !matches!(part, Component::ParentDir))
            .then(|| directory.to_path_buf());
    }
    let mut at = files.to_path_buf();
    for part in directory.components() {
        match part {
            Component::Normal(name) => at.push(name),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(at)
}

impl WorkspaceShells {
    /// Start `start` in `workspace`'s machine, paid for by `payer` rather
    /// than this host's own account when given, and answer its channel.
    ///
    /// # Errors
    ///
    /// This host runs no machines for its workspaces, the workspace's files
    /// could not be made, or the run names a folder outside them.
    pub fn serve_run(
        &self,
        workspace: &str,
        payer: Option<&Payer>,
        start: ServeRun,
    ) -> io::Result<ServedRun> {
        let Some((programs, files)) = self.machine(workspace, payer)? else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "this host runs no Linux machines for its workspaces",
            ));
        };
        let Some(directory) = place(&files, &start.directory) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a run starts in a folder of the workspace's files, or at a path of its machine",
            ));
        };
        let refused = Arc::new(Mutex::new(None));
        let programs = programs.reporting_refusals(Arc::clone(&refused));
        let endpoints: Vec<&str> = start.endpoints.iter().map(String::as_str).collect();
        let mut argv = vec![start.agent_program.clone()];
        argv.extend(served::arguments(&endpoints, start.stop_grace, &start.argv));
        // Asked as an agent asks: a machine the account cannot have yet is
        // refused in words at once, never waited for at a prompt nobody sees.
        let mut env = vec![("USH_RENDER".to_owned(), "agent".to_owned())];
        env.extend(start.env);
        let (frames_in, telling) = pipe();
        let telling = Arc::new(Mutex::new(Some(telling)));
        let told = Arc::clone(&telling);
        let (mut frames_out, saying) = pipe();
        let (beside, words) = Output::buffer();
        let (hearing, heard) = mpsc::channel();
        let cut = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&cut);
        std::thread::Builder::new()
            .name("served-run".into())
            .spawn(move || {
                let frames = hearing.clone();
                let reading = std::thread::spawn(move || {
                    while let Ok(Some(frame)) = recv::<_, FromRun>(&mut frames_out) {
                        if frames.send(Heard::Frame(frame)).is_err() {
                            break;
                        }
                    }
                });
                let io = Stdio {
                    stdin: Input::new(frames_in),
                    stdout: Output::new(saying),
                    stderr: beside,
                };
                let stop = || stopping.load(Ordering::SeqCst);
                let program = programs.run(&argv, &env, &directory, &io, Some(&stop));
                // Nothing is told to a run that is over, and whatever still
                // reads the frames to it finds their end.
                *told.lock().unwrap_or_else(PoisonError::into_inner) = None;
                // The run's last frames are read before it is said to be over.
                drop(io);
                let _ = reading.join();
                let exit = match program {
                    Program::Exited(exit) => exit,
                    Program::NotFound => 127,
                    Program::NeedsTier => 126,
                };
                let refusal = refused
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                let _ = hearing.send(Heard::Over(Over {
                    exit,
                    refusal,
                    words: String::from_utf8_lossy(&words.take()).into_owned(),
                }));
            })?;
        Ok(ServedRun {
            telling,
            heard: Mutex::new(heard),
            cut,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn a_run_starts_in_the_workspaces_files_or_at_a_path_of_its_machine;

        phase a_relative_folder_is_below_the_workspaces_files;
        {
            let files = Path::new("/srv/ws/files");
            assert_eq!(place(files, Path::new("")), Some(files.to_path_buf()));
            assert_eq!(
                place(files, Path::new("code/./app")),
                Some(PathBuf::from("/srv/ws/files/code/app"))
            );
            assert_eq!(place(files, Path::new("code/../../other")), None);
        }

        phase an_absolute_path_is_the_machines_own;
        {
            let files = Path::new("/srv/ws/files");
            assert_eq!(
                place(files, Path::new("/home/tilo/work/app")),
                Some(PathBuf::from("/home/tilo/work/app"))
            );
            assert_eq!(place(files, Path::new("/home/tilo/../root")), None);
        }
    }
}

#[cfg(test)]
#[path = "served_runs_tests.rs"]
mod through_a_machine;
