//! What an installed program keeps between its runs
//! ([`crate::agents::programs::kept`]), in a file of its installation.
//!
//! The file is written before a change is answered: a count a step was
//! told, and a note a person's assistant was told is kept, are still there
//! after a restart. It stands beside the installation's progress and is
//! written on its own, since a note is changed between the program's runs
//! too. Pausing a program and the permissions it carries are kept here as
//! well, and are changed only from outside its runs.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::agents::programs::api::AutomationStep;
use crate::agents::programs::host_steps::{HostStepError, Keeps};
use crate::agents::programs::kept::{Kept, LastRun, NoteChange, note_value};
use serde_json::Value;
use tilo_action_policy::Rule;
use tilo_lock::MutexRecover as _;

/// One installation's kept state and the file it is written to.
pub struct KeptFile {
    path: PathBuf,
    kept: Mutex<Kept>,
}

impl KeptFile {
    /// What is kept at `path`, or nothing yet when no file is there.
    ///
    /// # Errors
    /// The file is there and does not read.
    pub fn open(path: PathBuf) -> Result<Self, String> {
        let kept = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| format!("{}: {error}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Kept::default(),
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        Ok(Self {
            path,
            kept: Mutex::new(kept),
        })
    }

    /// What is kept now.
    #[must_use]
    pub fn read(&self) -> Kept {
        self.kept.lock_recover().clone()
    }

    /// Whether the program's person paused it.
    #[must_use]
    pub fn paused(&self) -> bool {
        self.kept.lock_recover().paused()
    }

    /// Pause the program, or let it go on. Kept at once.
    ///
    /// # Errors
    /// The file could not be written.
    pub fn pause(&self, paused: bool) -> Result<(), HostStepError> {
        self.change(|kept| {
            kept.pause(paused);
            Ok(())
        })
    }

    /// Let the program's work take one action that leaves the workspace
    /// without asking, from now on ([`Kept::permit`]). Kept at once.
    ///
    /// # Errors
    /// The permission is wider than one action, or the file could not be
    /// written.
    pub fn permit(&self, permission: Rule) -> Result<(), HostStepError> {
        self.change(|kept| kept.permit(permission))
    }

    /// Take a permission back: its action asks again. Kept at once.
    ///
    /// # Errors
    /// The file could not be written.
    pub fn withdraw(&self, permission: &Rule) -> Result<(), HostStepError> {
        self.change(|kept| {
            kept.withdraw(permission);
            Ok(())
        })
    }

    /// Keep how a run of the program ended ([`Kept::ran`]). Kept at once.
    ///
    /// # Errors
    /// The file could not be written.
    pub fn ran(&self, last: LastRun, again: bool) -> Result<(), HostStepError> {
        self.change(|kept| {
            kept.ran(last, again);
            Ok(())
        })
    }

    /// Make `change` and write the file before answering. The change is
    /// made to a copy, which takes the kept state's place only once the
    /// file holds it, so what is answered is what is kept. The lock is held
    /// across the write: one change is written at a time.
    fn change<T>(
        &self,
        change: impl FnOnce(&mut Kept) -> Result<T, String>,
    ) -> Result<T, HostStepError> {
        let mut kept = self.kept.lock_recover();
        let mut changed = kept.clone();
        let answer = change(&mut changed).map_err(HostStepError::Refused)?;
        if changed != *kept {
            write(&self.path, &changed).map_err(HostStepError::Unavailable)?;
            *kept = changed;
        }
        drop(kept);
        Ok(answer)
    }
}

/// Write `kept` to the file at `path`, whole.
fn write(path: &Path, kept: &Kept) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(kept).map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write(path, &bytes)
        .map_err(|error| format!("{}: {error}", path.display()))
}

impl Keeps for KeptFile {
    fn count(&self, step: &AutomationStep, name: &str) -> Result<u64, HostStepError> {
        self.change(|kept| kept.count(step, name))
    }

    fn keep(&self, change: NoteChange) -> Result<(), HostStepError> {
        self.change(|kept| kept.change_note(change))
    }

    fn note(&self) -> Value {
        note_value(self.kept.lock_recover().note())
    }

    fn permits(&self) -> Vec<Rule> {
        self.kept.lock_recover().permits().to_vec()
    }
}
