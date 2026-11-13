//! Stand-ins for the programs code under test starts: a `tilo-launcher` that records its launch and
//! keeps running as an app would, a host app's program.
//!
//! A stand-in that keeps running must end when its test does, however the test ends: passing,
//! failing an assertion, or the whole test process being killed. A stand-in that instead waits for
//! a file the test writes runs on forever when the test fails before writing it, or when the test's
//! temporary tree, file and all, is deleted before the stand-in looks.
//!
//! [`stand_in`] gives every stand-in a lifeline instead: a FIFO beside the script whose only writer
//! is the [`StandIn`] the test holds, opened close-on-exec so nothing the test starts inherits it.
//! The script's `hold` reads from it, and a read of a FIFO with no writer left returns at once:
//! when the test drops the [`StandIn`], and when the kernel closes the test's descriptors because
//! the process is gone. Nothing polls and nothing waits for a time: the read returns the moment its
//! test is gone.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use crate::{Script, script};

/// A stand-in script and the lifeline that holds it running.
///
/// Dropping it lets every `hold` in a running copy of the script return, and a copy started after
/// that holds nothing.
#[derive(Debug)]
pub struct StandIn {
    script: Script,
    // Field order is drop order: the FIFO's path goes before its writer closes, so a copy that
    // starts after this can no longer open it.
    lifeline: Lifeline,
}

impl StandIn {
    pub fn path(&self) -> &Path {
        self.script.path()
    }

    pub fn script(&self) -> &Script {
        &self.script
    }

    pub fn lifeline(&self) -> &Path {
        &self.lifeline.path
    }
}

/// The writer end of a stand-in's FIFO, and its path.
#[derive(Debug)]
struct Lifeline {
    path: PathBuf,
    _writer: File,
}

impl Drop for Lifeline {
    fn drop(&mut self) {
        // Unlinked first: a copy of the script that starts now finds no FIFO and does not hold,
        // rather than opening one that has no writer left to close and waiting on it for ever.
        let _ = fs::remove_file(&self.path);
    }
}

/// Write `body` to `path` as a `/bin/sh` stand-in, with a `hold` it calls to keep running until the
/// test that wrote it is gone.
///
/// `body` is the script after its interpreter line: it records what it was asked
/// (`printf '%s\n' "$$" >> '…'`), then calls `hold` where the program it stands in for would keep
/// running. A stand-in that only records and exits needs no `hold`, and still gets the atomic write
/// of [`script`].
pub fn stand_in(path: impl AsRef<Path>, body: &str) -> io::Result<StandIn> {
    let path = path.as_ref();
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "a stand-in needs a name"))?;
    let mut fifo_name = std::ffi::OsString::from(".");
    fifo_name.push(name);
    fifo_name.push(".lifeline");
    let fifo = path.with_file_name(fifo_name);
    let lifeline = Lifeline::create(&fifo)?;
    let script = script(
        path,
        &format!(
            "#!/bin/sh\n\
             # Returns once the test that wrote this script is gone.\n\
             hold() {{ read -r _ 2>/dev/null < '{fifo}'; }}\n\
             {body}",
            fifo = fifo.display(),
        ),
    )?;
    Ok(StandIn { script, lifeline })
}

impl Lifeline {
    fn create(path: &Path) -> io::Result<Self> {
        use std::os::unix::ffi::OsStrExt as _;

        let _ = fs::remove_file(path);
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a NUL in the path"))?;
        // SAFETY: `c_path` is a valid NUL-terminated string for the call.
        if unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // Read and write: opening a FIFO for writing alone waits for a reader, and there is none
        // yet. Rust opens it close-on-exec, so this process holds the only writer and nothing it
        // starts inherits one.
        let writer = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self {
            path: path.to_owned(),
            _writer: writer,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SpawnUnderEnvLock as _;
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::thread;

    fn holding(stand_in: &StandIn, started: &Path) -> mpsc::Receiver<()> {
        let mut child = stand_in
            .script()
            .command()
            .stdin(Stdio::null())
            .spawn_locked()
            .expect("the stand-in starts");
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = child.wait();
            let _ = sender.send(());
        });
        crate::wait_until("the stand-in started", || started.exists());
        receiver
    }

    #[test]
    fn a_stand_in_ends_when_its_test_drops_it() {
        let dir = tempfile::tempdir().unwrap();
        let started = dir.path().join("started");
        let stand_in = stand_in(
            dir.path().join("tilo-launcher"),
            &format!("printf '%s\\n' \"$$\" > '{}'\nhold\n", started.display()),
        )
        .unwrap();
        let ended = holding(&stand_in, &started);
        assert!(
            ended.try_recv().is_err(),
            "the stand-in holds while its test runs"
        );

        drop(stand_in);
        crate::recv("the stand-in to end with its test", &ended);
    }

    #[test]
    fn a_stand_in_started_after_its_test_is_gone_does_not_hold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tilo-launcher");
        let stand_in = stand_in(&path, "hold\n").unwrap();
        let script = stand_in.script().clone();
        drop(stand_in);

        let (sender, ended) = mpsc::channel();
        let mut child = script
            .command()
            .stdin(Stdio::null())
            .spawn_locked()
            .unwrap();
        thread::spawn(move || {
            let _ = child.wait();
            let _ = sender.send(());
        });
        crate::recv("a late stand-in to end at once", &ended);
    }
}
