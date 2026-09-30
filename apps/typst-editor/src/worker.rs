//! Background compile worker.
//!
//! Typst compilation of a multi-page document is tens to hundreds of
//! milliseconds — too slow to run on the UI thread per keystroke. This worker
//! owns a thread that holds the shared [`WorldParts`] (parsed fonts + standard
//! library) and compiles whatever the latest queued source is, coalescing a
//! burst of edits into a single compile.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use crate::compile::{CompileOutput, WorldParts, compile_source};

struct Job {
    seq: u64,
    source: String,
    ppp: f32,
}

/// A finished compile, tagged with the sequence number of the source it came
/// from so the UI can ignore stale results.
pub struct CompileResult {
    pub seq: u64,
    pub output: CompileOutput,
}

pub struct CompileWorker {
    job_tx: Sender<Job>,
    result_rx: Receiver<CompileResult>,
    next_seq: u64,
    last_done_seq: u64,
}

impl CompileWorker {
    /// Spawn the worker thread. `parts` is shared (cheap `Arc` clone). Returns
    /// `None` when the OS refuses to spawn the thread (e.g. resource
    /// exhaustion); the app then falls back to running without a background
    /// worker rather than crashing.
    pub fn spawn(parts: Arc<WorldParts>) -> Option<Self> {
        let (job_tx, job_rx) = channel::<Job>();
        let (result_tx, result_rx) = channel::<CompileResult>();
        ui::rt::spawn_thread("typst-compile", move || {
            while let Ok(mut job) = job_rx.recv() {
                // Drain to the freshest queued job — older edits are stale.
                loop {
                    match job_rx.try_recv() {
                        Ok(newer) => job = newer,
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => return,
                    }
                }
                let output = compile_source(&parts, &job.source, job.ppp);
                if result_tx
                    .send(CompileResult {
                        seq: job.seq,
                        output,
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .ok()?;

        Some(Self {
            job_tx,
            result_rx,
            next_seq: 1,
            last_done_seq: 0,
        })
    }

    /// Queue a compile of `source`. Returns the sequence number assigned.
    pub fn request(&mut self, source: String, ppp: f32) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        let _ = self.job_tx.send(Job { seq, source, ppp });
        seq
    }

    /// Drain finished results, returning the most recent one (older ones are
    /// superseded). Updates the completed-sequence watermark.
    pub fn poll(&mut self) -> Option<CompileResult> {
        let mut latest: Option<CompileResult> = None;
        while let Ok(r) = self.result_rx.try_recv() {
            self.last_done_seq = self.last_done_seq.max(r.seq);
            latest = Some(r);
        }
        latest
    }

    /// True while a queued source has not yet produced a result.
    pub fn in_flight(&self) -> bool {
        self.next_seq.saturating_sub(1) > self.last_done_seq
    }
}
