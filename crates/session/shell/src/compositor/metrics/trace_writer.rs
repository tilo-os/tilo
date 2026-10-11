//! The per-tick CSV trace's file, written off the compositor loop.
//!
//! The loop formats each tick's row into memory and hands the rows over once a
//! second; a thread of its own does every `write(2)`. A write to a file can
//! wait on the disk (a journal commit, dirty-page throttling, another
//! process's flush) for as long as the disk is busy, and on a machine that is
//! compiling that was hundreds of milliseconds with the compositor thread in
//! uninterruptible sleep: the loop cannot take input, answer clients or flip a
//! frame meanwhile. The trace is on in every session, so its writes were on
//! the path of every key.

use std::io::{self, Write};
use std::sync::mpsc::{Sender, channel};
use std::thread::JoinHandle;

/// What the writer thread is asked to do with the file.
pub(super) enum Job {
    /// Add rows to the end.
    Append(Vec<u8>),
    /// Replace the whole file (the trace's rolling window moved on).
    Replace(Vec<u8>),
}

/// A [`Write`] whose bytes reach their sink on another thread.
pub(super) struct TraceWriter {
    pending: Vec<u8>,
    jobs: Option<Sender<Job>>,
    worker: Option<JoinHandle<()>>,
}

impl TraceWriter {
    /// Write to `path`, which the caller already created and wrote the header
    /// to, appending; [`Job::Replace`] recreates it.
    pub(super) fn to_file(file: std::fs::File, path: std::path::PathBuf) -> Self {
        let mut file = file;
        Self::with_sink(move |job| match job {
            Job::Append(bytes) => file.write_all(&bytes),
            Job::Replace(bytes) => {
                file = std::fs::File::create(&path)?;
                file.write_all(&bytes)
            }
        })
    }

    /// Hand every job to `sink` on a thread of its own, in order. The sink
    /// failing ends the thread; the next hand-over then reports a broken pipe.
    pub(super) fn with_sink(mut sink: impl FnMut(Job) -> io::Result<()> + Send + 'static) -> Self {
        let (jobs, received) = channel::<Job>();
        let worker = std::thread::Builder::new()
            .name("frame-trace".to_owned())
            .spawn(move || {
                for job in received {
                    if let Err(error) = sink(job) {
                        eprintln!("frame-metrics: trace write failed ({error}); disabling trace");
                        return;
                    }
                }
            })
            .ok();
        Self {
            pending: Vec::new(),
            jobs: worker.is_some().then_some(jobs),
            worker,
        }
    }

    /// Replace the file with `bytes`, after everything handed over so far.
    pub(super) fn replace(&mut self, bytes: Vec<u8>) -> io::Result<()> {
        self.flush()?;
        self.send(Job::Replace(bytes))
    }

    /// Hand over what is pending and wait for the file to have it. Only for
    /// the end of a session, when nothing is waiting on the loop any more.
    pub(super) fn close(&mut self) -> io::Result<()> {
        let handed = self.flush();
        self.jobs = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        handed
    }

    fn send(&self, job: Job) -> io::Result<()> {
        self.jobs
            .as_ref()
            .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?
            .send(job)
            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))
    }
}

impl Write for TraceWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(buf);
        Ok(buf.len())
    }

    /// Hands the pending rows to the writer thread; returns without waiting
    /// for them to reach the file.
    fn flush(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let rows = std::mem::take(&mut self.pending);
        self.send(Job::Append(rows))
    }
}

impl Drop for TraceWriter {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn a_stalled_disk_never_holds_the_thread_that_writes_the_trace() {
        // The sink stands for a disk that does not answer: it takes nothing
        // until the test lets it.
        let (open_gate, gate) = channel::<()>();
        let written = Arc::new(Mutex::new(Vec::<u8>::new()));
        let seen = Arc::clone(&written);
        let mut trace = TraceWriter::with_sink(move |job| {
            let _ = gate.recv();
            if let Job::Append(bytes) | Job::Replace(bytes) = job {
                seen.lock().expect("sink").extend_from_slice(&bytes);
            }
            Ok(())
        });
        // A second of ticks, many times over, with the loop flushing as it does.
        for tick in 0..2000 {
            writeln!(trace, "row {tick}").expect("format");
            if tick % 60 == 0 {
                trace.flush().expect("hand over");
            }
        }
        trace.replace(b"window\n".to_vec()).expect("replace");
        assert!(
            written.lock().expect("sink").is_empty(),
            "the loop reached this point with nothing written: it did not wait"
        );
        // The disk comes back; every byte arrives, in order.
        for _ in 0..64 {
            let _ = open_gate.send(());
        }
        drop(open_gate);
        trace.close().expect("close");
        let bytes = written.lock().expect("sink").clone();
        let text = String::from_utf8(bytes).expect("utf8");
        assert!(text.starts_with("row 0\n"));
        assert!(text.contains("row 1999\n"));
        assert!(text.ends_with("window\n"));
    }

    #[test]
    fn a_failed_write_disables_the_trace_instead_of_blocking_or_losing_the_loop() {
        let mut trace = TraceWriter::with_sink(|_| Err(io::Error::other("disk gone")));
        writeln!(trace, "row").expect("format");
        trace.flush().expect("the first hand-over is accepted");
        let mut failed = false;
        for _ in 0..1000 {
            writeln!(trace, "row").expect("format");
            if trace.flush().is_err() {
                failed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(failed, "the loop learns the trace is gone and drops it");
    }
}
