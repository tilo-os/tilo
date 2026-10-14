//! Background compile worker.
//!
//! Typst compilation of a multi-page document is tens to hundreds of
//! milliseconds — too slow to run on the UI thread per keystroke. This worker
//! owns a thread that holds the shared [`WorldParts`] (parsed fonts + standard
//! library) and compiles whatever the latest queued source is, coalescing a
//! burst of edits into a single compile.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use crate::compile::{CompileOutput, WorldParts, compile_project};

struct Job {
    seq: u64,
    main_name: String,
    source: String,
    files: Vec<(String, String)>,
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
                let output =
                    compile_project(&parts, &job.main_name, &job.source, &job.files, job.ppp);
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
        self.request_project("main.typ".to_owned(), source, Vec::new(), ppp)
    }

    /// Queue an owned project snapshot. Names are project-root-relative paths;
    /// `source` overrides any saved entry for `main_name`. Coalescing replaces
    /// the whole snapshot, so includes cannot come from a different edit.
    pub fn request_project(
        &mut self,
        main_name: String,
        source: String,
        files: Vec<(String, String)>,
        ppp: f32,
    ) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        let _ = self.job_tx.send(Job {
            seq,
            main_name,
            source,
            files,
            ppp,
        });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_compiles_owned_project_snapshots_and_single_source_requests() {
        let mut worker = CompileWorker::spawn(WorldParts::new()).expect("compile worker starts");
        let seq = worker.request_project(
            "docs/main.typ".into(),
            "#include \"body.typ\"".into(),
            vec![("docs/body.typ".into(), "Worker snapshot".into())],
            1.0,
        );
        assert!(worker.in_flight());
        let result = worker
            .result_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("project compile finishes");
        assert_eq!(result.seq, seq);
        assert!(result.output.ok, "{:?}", result.output.diagnostics);
        let text: String = result
            .output
            .page_text
            .iter()
            .flatten()
            .flat_map(|run| &run.cells)
            .map(|cell| cell.text.as_str())
            .collect();
        assert!(text.contains("Worker snapshot"), "{text}");

        let next = worker.request("#include \"docs/body.typ\"".into(), 1.0);
        assert_eq!(next, seq + 1);
        let result = worker
            .result_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("single-source compile finishes");
        assert_eq!(result.seq, next);
        assert!(
            !result.output.ok,
            "single-source request must not retain files"
        );
        assert!(result.output.error_count() > 0);
    }
}
