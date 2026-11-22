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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// What an app wrote that the log kept for its end: the last lines of each stream, and how many
/// lines did not reach the log. A program that stopped with an error says why on its error stream when
/// it says it anywhere; a console tool prints its usage on its normal output, so both are kept.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputTail {
    /// Its last stderr lines, oldest first.
    pub stderr: Vec<String>,
    /// Its last stdout lines, oldest first.
    pub stdout: Vec<String>,
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
    /// Where the app's stderr also goes when the session's shell names a
    /// directory for it (`TILO_APP_STDERR_DIR`), as a headless acceptance
    /// reads it.
    file: Option<Mutex<std::fs::File>>,
    stderr_tail: Mutex<VecDeque<String>>,
    stdout_tail: Mutex<VecDeque<String>>,
    /// The threads reading the app's streams, joined by [`Self::finish`].
    readers: Mutex<Vec<std::thread::JoinHandle<()>>>,
    /// The keeper's directory, for a launch kept across a restart: what this desktop reads from
    /// the launcher is the same bytes its keeper writes to its files, so what was read is kept
    /// there, and the desktop that follows reads on from it rather than logging any of it twice.
    #[cfg(all(target_os = "linux", feature = "keeper"))]
    kept: Mutex<Option<std::path::PathBuf>>,
    /// Set when the app ended: a reader following a stream that has not closed yet stops.
    stopping: Arc<AtomicBool>,
    relayed: AtomicU64,
    /// Lines the log did not relay, over the whole launch, as its report says.
    dropped: AtomicU64,
    /// Of those, the ones the log has not said it left out yet.
    unsaid: AtomicU64,
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
            stdout_tail: Mutex::new(VecDeque::with_capacity(TAIL_LINES)),
            readers: Mutex::new(Vec::new()),
            #[cfg(all(target_os = "linux", feature = "keeper"))]
            kept: Mutex::new(None),
            stopping: Arc::new(AtomicBool::new(false)),
            relayed: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            unsaid: AtomicU64::new(0),
        }
    }

    /// Read `stdout` on a thread of its own, keeping each line as it comes.
    pub fn keep_stdout(self: &Arc<Self>, stdout: impl std::io::Read + Send + 'static) {
        let output = Arc::clone(self);
        self.on_thread("stdout", move || {
            output.read(stdout, "stdout", None);
        });
    }

    /// Read `stderr` on a thread of its own, keeping each line as it comes and
    /// keeping the last of them for the launch's end.
    pub fn keep_stderr(self: &Arc<Self>, stderr: impl std::io::Read + Send + 'static) {
        let output = Arc::clone(self);
        self.on_thread("stderr", move || {
            let file = output.file.as_ref();
            output.read(stderr, "stderr", file);
        });
    }

    /// Keep what the app wrote before this desktop took it over, and from now on: its keeper
    /// kept both streams in `dir` while no session read them, and this desktop reads on from
    /// where the one before it stopped.
    #[cfg(all(target_os = "linux", feature = "keeper"))]
    pub fn follow(self: &Arc<Self>, dir: &std::path::Path) {
        self.seed_tail(dir);
        self.keep_followed(dir, false);
    }

    /// Keep what this desktop reads in `dir`, its keeper's directory: the desktop that follows
    /// reads on from it.
    #[cfg(all(target_os = "linux", feature = "keeper"))]
    pub fn keep_position_in(&self, dir: &std::path::Path) {
        *self.kept.lock_recover() = Some(dir.to_path_buf());
    }

    /// Keep the last lines its keeper holds as this launch's end report names them, without
    /// relaying them again: the desktop before this one read them, and only new lines go to the
    /// log, while a crash report still says what the app wrote before it.
    #[cfg(all(target_os = "linux", feature = "keeper"))]
    fn seed_tail(&self, dir: &std::path::Path) {
        for (stream, tail) in [("stderr", &self.stderr_tail), ("stdout", &self.stdout_tail)] {
            let path = tilo_app_keeper::output::path(dir, stream);
            let stopping = Arc::new(AtomicBool::new(true));
            let Ok(mut whole) = tilo_app_keeper::output::Follow::whole(&path, stopping) else {
                continue;
            };
            let mut kept = Vec::new();
            if std::io::Read::read_to_end(&mut whole, &mut kept).is_err() {
                continue;
            }
            let text = String::from_utf8_lossy(&kept);
            let lines = text.lines();
            let skip = lines.clone().count().saturating_sub(TAIL_LINES);
            let mut tail = tail.lock_recover();
            for line in lines.skip(skip) {
                if tail.len() == TAIL_LINES {
                    tail.pop_front();
                }
                tail.push_back(line.to_owned());
            }
        }
    }

    /// Keep all the app wrote that its keeper still holds in `dir`, as [`Self::follow`] does but
    /// from the oldest line kept: what an end report is made of, whose tail names the last lines
    /// whether or not a desktop's log read them.
    #[cfg(all(target_os = "linux", feature = "keeper"))]
    pub fn follow_whole(self: &Arc<Self>, dir: &std::path::Path) {
        self.keep_followed(dir, true);
    }

    #[cfg(all(target_os = "linux", feature = "keeper"))]
    fn keep_followed(self: &Arc<Self>, dir: &std::path::Path, whole: bool) {
        for stream in ["stdout", "stderr"] {
            let path = tilo_app_keeper::output::path(dir, stream);
            let stopping = Arc::clone(&self.stopping);
            let follow = match if whole {
                tilo_app_keeper::output::Follow::whole(&path, stopping)
            } else {
                tilo_app_keeper::output::Follow::open(&path, stopping)
            } {
                Ok(follow) => follow,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "what the app wrote is not read");
                    continue;
                }
            };
            match stream {
                "stdout" => self.keep_stdout(follow),
                _ => self.keep_stderr(follow),
            }
        }
    }

    /// Keep `received` bytes of `stream` as what this desktop read, for the desktop that follows.
    #[cfg(all(target_os = "linux", feature = "keeper"))]
    fn keep_position(&self, stream: &str, received: usize) {
        let Some(dir) = self.kept.lock_recover().clone() else {
            return;
        };
        let path = tilo_app_keeper::output::cursor(&tilo_app_keeper::output::path(&dir, stream));
        let _ = std::fs::write(path, received.to_string());
    }

    #[cfg(not(all(target_os = "linux", feature = "keeper")))]
    fn keep_position(&self, _: &str, _: usize) {}

    /// Wait until both streams are read to their end: the app's pipes close
    /// with it, and the tail is what it said only once they are drained.
    pub fn finish(&self) {
        self.stopping.store(true, Ordering::Release);
        let readers = std::mem::take(&mut *self.readers.lock_recover());
        for reader in readers {
            let _ = reader.join();
        }
    }

    fn on_thread(&self, stream: &str, read: impl FnOnce() + Send + 'static) {
        let spawned = std::thread::Builder::new()
            .name(format!("app-{stream}-{}", self.app.replace('.', "-")))
            .spawn(read);
        match spawned {
            Ok(reader) => self.readers.lock_recover().push(reader),
            Err(error) => {
                // Without a reader the app fills the pipe and blocks on its next
                // write, so this must be said loudly, not passed over.
                tracing::warn!(
                    target: TARGET,
                    app = %self.app,
                    stream,
                    %error,
                    "an app's output is read by no one: it blocks when the pipe fills"
                );
            }
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
        let mut received = 0_usize;
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
            received = received.saturating_add(read);
            self.keep_position(stream, received);
        }
        if !partial.is_empty() || truncated {
            self.keep(stream, &mut partial, &mut truncated, file, &mut bucket);
        }
        // A stream that ends with lines the log never passed on says so even when no later line
        // is: an app that chatters once and exits would otherwise leave the log looking as if
        // everything it wrote had been kept.
        self.notice_dropped(stream);
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
        {
            let tail = if stream == "stderr" {
                &self.stderr_tail
            } else {
                &self.stdout_tail
            };
            let mut tail = tail.lock_recover();
            if tail.len() == TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(said.clone());
        }
        let relayed = self.relayed.load(Ordering::Relaxed);
        if relayed >= RELAY_LINES_MAX {
            self.drop_line();
            return;
        }
        if !bucket.take() {
            self.drop_line();
            return;
        }
        // A line that relayed after some that did not says what was left out:
        // a log that silently skipped lines would read as if nothing came.
        self.notice_dropped(stream);
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

    /// A line the log does not relay: counted for the report, and for the
    /// next notice.
    fn drop_line(&self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
        self.unsaid.fetch_add(1, Ordering::Relaxed);
    }

    /// Say, once, how many lines of `stream` the log left out since it last said so.
    fn notice_dropped(&self, stream: &'static str) {
        let dropped = self.unsaid.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            tracing::info!(
                target: TARGET,
                app = %self.app,
                stream,
                "{dropped} lines of this app were not relayed"
            );
        }
    }

    /// What the launch ended with: the last stderr lines kept, and how many
    /// lines were not relayed.
    #[must_use]
    pub fn tail(&self) -> OutputTail {
        OutputTail {
            stderr: self.stderr_tail.lock_recover().iter().cloned().collect(),
            stdout: self.stdout_tail.lock_recover().iter().cloned().collect(),
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

    tilo_test_env::scenario! {
        fn an_apps_output_is_kept_cut_and_rate_limited;

        phase the_lines_an_app_writes_are_the_ones_it_is_read_back_with;
        {
            let app = output();
            read(&app, "one\ntwo\nthree without a newline", "stderr");
            assert_eq!(
                app.tail().stderr,
                ["one", "two", "three without a newline"],
                "every line is kept, the last one without its terminator too"
            );
            assert_eq!(app.tail().dropped, 0);
        }

        phase a_line_is_cut_at_the_bound_and_says_so;
        {
            let app = output();
            let long = "x".repeat(LINE_BYTES.saturating_mul(2));
            read(&app, &format!("{long}\n"), "stdout");
            let kept = app.tail();
            assert!(kept.stderr.is_empty(), "a stdout line is not the stderr tail");
            assert_eq!(
                kept.stdout,
                ["x".repeat(LINE_BYTES) + "… [truncated]"],
                "and it is kept as its own stream's line, cut at the bound"
            );
        }

        phase a_chatty_app_is_rate_limited_and_the_log_says_how_much_it_left_out;
        {
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

        phase a_launch_relays_at_most_its_bound_of_lines;
        {
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
}
