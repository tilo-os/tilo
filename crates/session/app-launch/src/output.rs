//! What an app writes to its standard streams, and what the session's log
//! keeps of it.
//!
//! An app that fails inside itself says why on its standard error, and
//! nowhere else: the session started the process, so the only account of a
//! launch that never reached a window is the last thing the process wrote
//! before it ended. So both streams are read as the app runs and each line is
//! relayed into the session's log, labelled with the app it came from
//! (`[app so.tilo.notes stderr] …`), which the runner keeps in the journal and
//! the flight record beside everything else the session reports.
//!
//! The app's output is not the session's: it is untrusted, and an app that
//! chatters must not be able to fill the log a person reads to find out what
//! went wrong. So a relay is bounded three ways — a line is cut at
//! [`LINE_BYTES`], a stream relays a burst and then a rate ([`LINES_PER_SECOND`]),
//! and a launch relays at most [`RELAY_LINES_MAX`] lines per stream — and every
//! line it does not relay is counted and said, once, in the same stream's
//! words. The last stderr lines are kept as well ([`TAIL_LINES`]): an app that
//! ends before its window appears has them repeated at `warn` by
//! [`report_ended`], which is the report the person's notice stands on.

use std::collections::VecDeque;
use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tilo_lock::MutexRecover as _;

/// The `tracing` target of what an app writes: a line per line of either
/// stream, labelled with the app, so a launch that failed inside the app says
/// in the log why. Kept whatever the session's `RUST_LOG` says
/// ([`tilo_startup_metrics::APP_OUTPUT_TARGET`]).
const TARGET: &str = tilo_startup_metrics::APP_OUTPUT_TARGET;

/// The most bytes kept of one line: reading to a newline would let an app
/// exhaust the session's memory with one unterminated write.
pub const LINE_BYTES: usize = 8 * 1024;
/// How many lines a stream relays at once, and how many a second after that:
/// enough for an app's start-up and a warning burst, too little to bury the
/// log.
const BURST_LINES: i64 = 200;
const LINES_PER_SECOND: i64 = 100;
/// The microseconds one relayed line costs: the bucket's refill rate.
const MICROS_PER_LINE: i64 = 1_000_000 / LINES_PER_SECOND;
/// Lines one launch relays per stream before it says it has stopped and keeps
/// the rest out: a chatty app is bounded in total, not only in rate.
const RELAY_LINES_MAX: u64 = 5_000;
/// How many of an app's last stderr lines its end keeps for the report.
pub const TAIL_LINES: usize = 12;

/// What an app wrote that the log kept for its end: the last stderr lines, and
/// how many lines were not relayed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputTail {
    /// Its last stderr lines, oldest first.
    pub stderr: Vec<String>,
    /// Lines of either stream the log did not relay.
    pub dropped: u64,
}

/// The line one write of an app's becomes in the session's log: the app and
/// the stream it came on, then what it said.
#[must_use]
pub fn line(app: &str, stream: &str, said: &str) -> String {
    format!("[app {app} {stream}] {said}")
}

/// One launch's share of its app's output: the tail of its stderr and the
/// counts the report is made of, shared with the threads reading the pipes.
#[derive(Debug)]
pub struct AppOutput {
    app: String,
    /// Where the app's stderr also goes when the session's desk names a
    /// directory for it (`TILO_APP_STDERR_DIR`), as a headless acceptance
    /// reads it.
    file: Option<Mutex<std::fs::File>>,
    stderr_tail: Mutex<VecDeque<String>>,
    relayed: AtomicU64,
    dropped: AtomicU64,
}

impl AppOutput {
    /// The output of `app`, whose stderr also goes to `file` when one is
    /// given.
    #[must_use]
    pub fn new(app: String, file: Option<std::fs::File>) -> Self {
        Self {
            app,
            file: file.map(Mutex::new),
            stderr_tail: Mutex::new(VecDeque::with_capacity(TAIL_LINES)),
            relayed: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        }
    }

    /// Read `stdout` on a thread of its own, relaying each line as it comes.
    pub fn relay_stdout(self: &Arc<Self>, stdout: impl std::io::Read + Send + 'static) {
        let output = Arc::clone(self);
        Self::on_thread(&self.app, "stdout", move || {
            output.read(stdout, "stdout", None);
        });
    }

    /// Read `stderr` on a thread of its own, relaying each line as it comes and
    /// keeping the last of them for the launch's end.
    pub fn relay_stderr(self: &Arc<Self>, stderr: impl std::io::Read + Send + 'static) {
        let output = Arc::clone(self);
        Self::on_thread(&self.app, "stderr", move || {
            let file = output.file.as_ref();
            output.read(stderr, "stderr", file);
        });
    }

    fn on_thread(app: &str, stream: &str, read: impl FnOnce() + Send + 'static) {
        if let Err(error) = std::thread::Builder::new()
            .name(format!("app-{stream}-{}", app.replace('.', "-")))
            .spawn(read)
        {
            // Without a reader the app fills the pipe and blocks on its next
            // write, so this must be said loudly, not passed over.
            tracing::warn!(
                target: TARGET,
                app,
                stream,
                %error,
                "an app's output is read by no one: it blocks when the pipe fills"
            );
        }
    }

    /// Relay what `reader` writes, a line at a time, until it ends.
    fn read(
        &self,
        mut reader: impl std::io::Read,
        stream: &'static str,
        file: Option<&Mutex<std::fs::File>>,
    ) {
        let mut bucket = Bucket::full();
        let mut chunk = [0_u8; 4096];
        let mut partial = Vec::with_capacity(LINE_BYTES.min(chunk.len()));
        let mut truncated = false;
        loop {
            let read = match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            let bytes = chunk.get(..read).unwrap_or_default();
            for &byte in bytes {
                if byte == b'\n' {
                    self.keep(stream, &mut partial, &mut truncated, file, &mut bucket);
                } else if partial.len() < LINE_BYTES {
                    partial.push(byte);
                } else {
                    truncated = true;
                }
            }
        }
        if !partial.is_empty() || truncated {
            self.keep(stream, &mut partial, &mut truncated, file, &mut bucket);
        }
    }

    /// One line of `stream`: kept in the tail, written to the file, and
    /// relayed, unless the launch has relayed all it will or is past its rate.
    fn keep(
        &self,
        stream: &'static str,
        partial: &mut Vec<u8>,
        truncated: &mut bool,
        file: Option<&Mutex<std::fs::File>>,
        bucket: &mut Bucket,
    ) {
        let mut said = String::from_utf8_lossy(partial).into_owned();
        if *truncated {
            said.push_str("… [truncated]");
        }
        partial.clear();
        *truncated = false;
        if let Some(file) = file.filter(|_| stream == "stderr")
            && let Ok(mut file) = file.lock()
        {
            let _ = writeln!(file, "{said}");
        }
        if stream == "stderr" {
            let mut tail = self.stderr_tail.lock_recover();
            if tail.len() == TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(said.clone());
        }
        let relayed = self.relayed.load(Ordering::Relaxed);
        if relayed >= RELAY_LINES_MAX {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if !bucket.take() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        // A line that relayed after some that did not says what was left out:
        // a log that silently skipped lines would read as if nothing came.
        let dropped = self.dropped.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            tracing::info!(
                target: TARGET,
                app = %self.app,
                stream,
                "{dropped} lines of this app were not relayed"
            );
        }
        self.relayed.fetch_add(1, Ordering::Relaxed);
        tracing::info!(
            target: TARGET,
            app = %self.app,
            stream,
            "{}",
            line(&self.app, stream, &said)
        );
        if relayed.saturating_add(1) == RELAY_LINES_MAX {
            tracing::info!(
                target: TARGET,
                app = %self.app,
                stream,
                "this app has written {RELAY_LINES_MAX} lines; the rest of its {stream} is not relayed"
            );
        }
    }

    /// What the launch ended with: the last stderr lines kept, and how many
    /// lines were not relayed.
    #[must_use]
    pub fn tail(&self) -> OutputTail {
        OutputTail {
            stderr: self.stderr_tail.lock_recover().iter().cloned().collect(),
            dropped: self.dropped.load(Ordering::Relaxed),
        }
    }
}

/// A stream's relay allowance: a burst that refills at [`LINES_PER_SECOND`].
struct Bucket {
    tokens: i64,
    at: Instant,
}

impl Bucket {
    fn full() -> Self {
        Self {
            tokens: BURST_LINES,
            at: Instant::now(),
        }
    }

    /// Take one line's allowance, or say there is none left yet.
    fn take(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = i64::try_from(now.duration_since(self.at).as_micros()).unwrap_or(i64::MAX);
        if elapsed >= MICROS_PER_LINE {
            self.tokens = self
                .tokens
                .saturating_add(elapsed / MICROS_PER_LINE)
                .min(BURST_LINES);
            self.at = now;
        }
        if self.tokens > 0 {
            self.tokens = self.tokens.saturating_sub(1);
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A relay whose lines a test can read: `tracing` writes into a string.
    fn output() -> Arc<AppOutput> {
        Arc::new(AppOutput::new("so.tilo.notes".to_owned(), None))
    }

    fn read(output: &AppOutput, text: &str, stream: &'static str) {
        output.read(std::io::Cursor::new(text.as_bytes().to_vec()), stream, None);
    }

    #[test]
    fn the_lines_an_app_writes_are_the_ones_it_is_read_back_with() {
        let app = output();
        read(&app, "one\ntwo\nthree without a newline", "stderr");
        assert_eq!(
            app.tail().stderr,
            ["one", "two", "three without a newline"],
            "every line is kept, the last one without its terminator too"
        );
        assert_eq!(app.tail().dropped, 0);
    }

    #[test]
    fn a_line_is_cut_at_the_bound_and_says_so() {
        let app = output();
        let long = "x".repeat(LINE_BYTES.saturating_mul(2));
        read(&app, &format!("{long}\n"), "stdout");
        let kept = app.tail().stderr;
        assert!(kept.is_empty(), "a stdout line is not the stderr tail");
    }

    #[test]
    fn a_chatty_app_is_rate_limited_and_the_log_says_how_much_it_left_out() {
        let app = output();
        // Written at once, well past the burst.
        let burst = usize::try_from(BURST_LINES).unwrap_or(0);
        let text = "line\n".repeat(burst.saturating_add(500));
        read(&app, &text, "stderr");
        let output = app.tail();
        assert!(
            output.dropped > 0,
            "lines past the burst are not relayed: {output:?}"
        );
        assert_eq!(
            output.stderr.len(),
            TAIL_LINES,
            "the tail keeps the newest lines whatever the rate"
        );
    }

    #[test]
    fn a_launch_relays_at_most_its_bound_of_lines() {
        let app = output();
        let mut bucket = Bucket::full();
        let mut partial = Vec::new();
        let mut truncated = false;
        for _ in 0..5000 {
            // The bucket refilled between each, as lines over time are.
            bucket.tokens = BURST_LINES;
            app.keep("stdout", &mut partial, &mut truncated, None, &mut bucket);
        }
        assert_eq!(app.relayed.load(Ordering::Relaxed), RELAY_LINES_MAX);
    }
}
