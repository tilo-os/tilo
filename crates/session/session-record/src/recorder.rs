//! The session flight recorder: relays the compositor's output to stdout
//! while keeping a bounded private copy of it.
//!
//! Next to the full record (`latest.log`) the recorder keeps the session's
//! log events on their own (`events.log`): the lines the compositor's
//! `tracing` subscriber writes, each starting with its timestamp and level.
//! The full record is mostly the periodic measurement stream (`paint-stats`
//! every two seconds, `FRAME-LIFECYCLE`, `PUMP-PHASE`, ...), which fills its
//! bound in a few hours. A launch's timing lines, an Xwayland bridge start or
//! an error from the morning would be gone by the afternoon; in their own
//! record, under the same bound, only other events can displace them.

use std::fs::{self, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use regex::bytes::Regex;

/// Bytes read from the compositor at a time.
pub const READ_CHUNK: usize = 64 * 1024;
/// How much of each record is kept.
pub const LOG_LIMIT: usize = 4 * 1024 * 1024;
/// How soon after output the record is published.
pub const PERSIST_INTERVAL: Duration = Duration::from_millis(250);
/// After storage refused a publication (a full disk, say), wait this long
/// before trying again instead of retrying on every output burst.
pub const PERSIST_RETRY: Duration = Duration::from_secs(5);
/// The event record's file name, next to the full record.
pub const EVENTS_NAME: &str = "events.log";

/// A `tracing_subscriber::fmt` record: RFC 3339 UTC timestamp, then the level,
/// each possibly wrapped in ANSI colour codes.
static EVENT_RECORD: LazyLock<Option<Regex>> = LazyLock::new(|| {
    let ansi = r"(?:\x1b\[[0-9;]*m)*";
    Regex::new(&format!(
        r"(?-u)^{ansi}\d{{4}}-\d{{2}}-\d{{2}}T\d{{2}}:\d{{2}}:\d{{2}}(?:\.\d+)?Z{ansi}\s+{ansi}\s*(?:TRACE|DEBUG|INFO|WARN|ERROR)\b"
    ))
    .ok()
});

/// Whether one output line is a `tracing` record rather than stream output.
#[must_use]
pub fn is_event(line: &[u8]) -> bool {
    EVENT_RECORD
        .as_ref()
        .is_some_and(|pattern| pattern.is_match(line))
}

/// The last `limit` bytes of a stream.
#[derive(Debug)]
pub struct Retained {
    limit: usize,
    data: Vec<u8>,
}

impl Default for Retained {
    fn default() -> Self {
        Self::new(LOG_LIMIT)
    }
}

impl Retained {
    /// Keeps at most `limit` bytes.
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            data: Vec::new(),
        }
    }

    /// Appends `chunk`, dropping the oldest bytes past the limit.
    pub fn extend(&mut self, chunk: &[u8]) {
        self.data.extend_from_slice(chunk);
        let excess = self.data.len().saturating_sub(self.limit);
        if excess > 0 {
            self.data.drain(..excess);
        }
    }

    /// The retained bytes.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

/// Lines of `bytes`, each with its terminator, split at `\n`, `\r\n` and
/// `\r`.
fn lines_with_ends(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = bytes;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let end = match rest.iter().position(|byte| matches!(byte, b'\n' | b'\r')) {
            None => rest.len(),
            Some(at) => {
                let after = rest.get(at..).unwrap_or_default();
                // `\r\n` ends a line once; a lone `\r` or `\n` too.
                let terminator = if after.starts_with(b"\r\n") { 2 } else { 1 };
                at.saturating_add(terminator).min(rest.len())
            }
        };
        let (line, after) = rest.split_at(end);
        rest = after;
        Some(line)
    })
}

/// Picks the complete `tracing` lines out of a byte stream split anywhere.
#[derive(Debug)]
pub struct EventFilter {
    limit: usize,
    partial: Vec<u8>,
}

impl Default for EventFilter {
    fn default() -> Self {
        Self::new(LOG_LIMIT)
    }
}

impl EventFilter {
    /// A filter holding at most `limit` bytes of an unfinished line.
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            partial: Vec::new(),
        }
    }

    /// The event lines `chunk` completes.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.partial.extend_from_slice(chunk);
        let Some(end) = self.partial.iter().rposition(|byte| *byte == b'\n') else {
            // A line longer than the record cannot be kept whole; its head
            // is enough to classify it, so keep a bounded prefix.
            self.partial.truncate(self.limit);
            return Vec::new();
        };
        let complete: Vec<u8> = self.partial.drain(..=end).collect();
        lines_with_ends(&complete)
            .filter(|line| is_event(line))
            .flatten()
            .copied()
            .collect()
    }

    /// The last line, if it is an unterminated event, at end of stream.
    pub fn finish(&mut self) -> Vec<u8> {
        let mut rest = std::mem::take(&mut self.partial);
        if !rest.is_empty() && is_event(&rest) {
            rest.push(b'\n');
            rest
        } else {
            Vec::new()
        }
    }

    /// Bytes of an unfinished line held.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.partial.len()
    }
}

/// Publishes the record at `path`; false when storage refused it (a full
/// disk, an I/O error).
///
/// The recorder sits between the compositor and its stderr. If it died on a
/// failed write, the compositor's next log line would hit a closed pipe, so a
/// refused write only skips this publication; the record stays in memory and
/// the next attempt carries all of it.
pub fn persist(path: &Path, retained: &[u8]) -> bool {
    publish(path, retained).is_ok()
}

fn publish(path: &Path, retained: &[u8]) -> io::Result<()> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    // A restarting session can overlap its predecessor. Each publication owns
    // its staging inode; a shared name would let one recorder consume the
    // other's file between writing and replacement.
    let mut staging = tempfile::Builder::new()
        .prefix(&format!(".{name}."))
        .tempfile_in(directory)?;
    // Replacement installs this inode's mode. Diagnostics are private
    // regardless of the caller's umask.
    staging
        .as_file()
        .set_permissions(Permissions::from_mode(0o600))?;
    staging.write_all(retained)?;
    staging.flush()?;
    staging.as_file().sync_all()?;
    // A failed replacement drops (and so removes) the staging file.
    staging.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// Tells the session wrapper the relay runs, whether storage took the start
/// record or not.
fn mark_ready(ready: Option<&Path>) {
    if let Some(ready) = ready {
        let _ = fs::OpenOptions::new().create(true).append(true).open(ready);
    }
}

/// How the recorder was started.
#[derive(Clone, Debug)]
pub struct Recording {
    /// The full record (`latest.log`).
    pub path: PathBuf,
    /// The start record's fields: timestamp, compositor pid, compositor path.
    pub start: Option<(String, String, String)>,
    /// A file to create once the relay runs (`TILO_FLIGHT_RECORD_READY`).
    pub ready: Option<PathBuf>,
}

impl Recording {
    /// Reads the recorder's arguments (after the program name): the record
    /// path, optionally followed by the start record's timestamp, pid and
    /// compositor. `None` for any other shape.
    #[must_use]
    pub fn from_args(args: &[String]) -> Option<Self> {
        let (path, start) = match args {
            [path] => (path, None),
            [path, timestamp, pid, shell] => {
                (path, Some((timestamp.clone(), pid.clone(), shell.clone())))
            }
            _ => return None,
        };
        Some(Self {
            path: PathBuf::from(path),
            start,
            ready: std::env::var_os("TILO_FLIGHT_RECORD_READY")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
        })
    }
}

/// The two records and whether the event record still needs publishing.
struct Records {
    path: PathBuf,
    events_path: PathBuf,
    full: Retained,
    events: Retained,
    events_dirty: bool,
}

impl Records {
    /// Persists both records; false when storage refused either.
    fn publish(&mut self) -> bool {
        if self.events_dirty && persist(&self.events_path, self.events.data()) {
            self.events_dirty = false;
        }
        persist(&self.path, self.full.data()) && !self.events_dirty
    }
}

/// What the relay loop waits for: compositor output, or the request to
/// finish.
enum Feed {
    Output(Vec<u8>),
    Finish,
}

/// Turns SIGTERM into a [`Feed::Finish`] on `feed`.
///
/// The session wrapper cannot always wait for end of input: an app the
/// compositor started can hold the pipe's write end open after the
/// compositor exits. It then sends SIGTERM, and the recorder publishes what
/// it holds before it exits. Dying at once instead lost everything since the
/// last publication, and a publication still in progress (an fsync on a busy
/// disk easily takes seconds) lost the compositor's last words entirely.
///
/// SIGTERM is blocked here, before any other thread exists, so every thread
/// inherits the block and only the waiting thread ever receives it.
fn finish_on_term(feed: mpsc::SyncSender<Feed>) {
    // SAFETY: `sigset_t` is plain data; sigemptyset initialises it before
    // any use.
    let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: `set` is a valid, exclusively borrowed signal set.
    unsafe {
        libc::sigemptyset(&raw mut set);
        libc::sigaddset(&raw mut set, libc::SIGTERM);
    }
    // SAFETY: `set` is initialised; no previous mask is asked for.
    if unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &raw const set, std::ptr::null_mut()) } != 0
    {
        return;
    }
    let waiter = std::thread::Builder::new()
        .name("flight-finish".to_owned())
        .spawn(move || {
            let mut signal = 0;
            // SAFETY: `set` is initialised and `signal` is a valid out pointer.
            if unsafe { libc::sigwait(&raw const set, &raw mut signal) } == 0 {
                let _ = feed.send(Feed::Finish);
            }
        });
    if waiter.is_err() {
        // Without a thread to receive it, SIGTERM ends the recorder as before.
        // SAFETY: `set` is initialised; no previous mask is asked for.
        unsafe {
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &raw const set, std::ptr::null_mut());
        }
    }
}

/// Relays `input` to `output` until `input` ends or SIGTERM asks the
/// recorder to finish, keeping the records.
///
/// Every publication rewrites and fsyncs the whole record. Doing that per read
/// turned a chatty session into a continuous multi-megabyte write stream, and
/// a writer blocked on a full pipe stalls the compositor's stderr. Output is
/// relayed at once; the record follows within [`PERSIST_INTERVAL`], and in
/// full at end of stream or on SIGTERM.
///
/// It takes over SIGTERM for the whole process, so it runs on the main
/// thread before any other thread starts.
pub fn record(recording: &Recording, input: impl Read + Send + 'static, mut output: impl Write) {
    // The read blocks, so it runs on its own thread; this one waits on the
    // channel with the publication deadline as its timeout.
    let (feed, received) = mpsc::sync_channel::<Feed>(4);
    finish_on_term(feed.clone());
    let events_path = recording.path.with_file_name(EVENTS_NAME);
    let mut records = Records {
        path: recording.path.clone(),
        events_path,
        full: Retained::default(),
        events: Retained::default(),
        events_dirty: false,
    };
    let mut filter = EventFilter::default();
    if let Some((timestamp, pid, shell)) = &recording.start {
        let header = format!("tilo-flight-record-v1 start={timestamp} pid={pid} shell={shell}\n");
        records.full.extend(header.as_bytes());
        records.events.extend(header.as_bytes());
        persist(&records.events_path, records.events.data());
        persist(&records.path, records.full.data());
    }
    mark_ready(recording.ready.as_deref());

    let reader = std::thread::Builder::new()
        .name("flight-read".to_owned())
        .spawn(move || read_input(input, &feed));
    if reader.is_err() {
        return;
    }
    let mut dirty_since: Option<Instant> = None;
    // After a refused publication the next attempt comes PERSIST_RETRY later.
    let retry_at = || {
        Instant::now()
            .checked_add(PERSIST_RETRY.saturating_sub(PERSIST_INTERVAL))
            .unwrap_or_else(Instant::now)
    };
    loop {
        let chunk = match dirty_since {
            Some(since) => {
                let wait = since
                    .checked_add(PERSIST_INTERVAL)
                    .map_or(Duration::ZERO, |due| {
                        due.saturating_duration_since(Instant::now())
                    });
                match received.recv_timeout(wait) {
                    Ok(Feed::Output(chunk)) => Some(chunk),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        dirty_since = if records.publish() {
                            None
                        } else {
                            Some(retry_at())
                        };
                        continue;
                    }
                    Ok(Feed::Finish) | Err(mpsc::RecvTimeoutError::Disconnected) => None,
                }
            }
            None => match received.recv() {
                Ok(Feed::Output(chunk)) => Some(chunk),
                Ok(Feed::Finish) | Err(_) => None,
            },
        };
        let Some(chunk) = chunk else {
            break;
        };
        // A closed or failing stdout never stops the recorder.
        let _ = output.write_all(&chunk).and_then(|()| output.flush());
        records.full.extend(&chunk);
        let picked = filter.feed(&chunk);
        if !picked.is_empty() {
            records.events.extend(&picked);
            records.events_dirty = true;
        }
        match dirty_since {
            None => dirty_since = Some(Instant::now()),
            Some(since) if since.elapsed() >= PERSIST_INTERVAL => {
                dirty_since = if records.publish() {
                    None
                } else {
                    Some(retry_at())
                };
            }
            Some(_) => {}
        }
    }
    let rest = filter.finish();
    if !rest.is_empty() {
        records.events.extend(&rest);
        records.events_dirty = true;
    }
    if dirty_since.is_some() || records.events_dirty {
        records.publish();
    }
}

/// Feeds `input` to the relay loop, then [`Feed::Finish`] at its end. The
/// SIGTERM waiter keeps the channel open, so the end is said, not implied.
fn read_input(mut input: impl Read, chunks: &mpsc::SyncSender<Feed>) {
    let mut buffer = vec![0_u8; READ_CHUNK];
    loop {
        match input.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let chunk = buffer.get(..read).unwrap_or_default().to_vec();
                if chunks.send(Feed::Output(chunk)).is_err() {
                    return;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let _ = chunks.send(Feed::Finish);
}

#[cfg(test)]
mod tests {
    use super::*;

    // What the compositor writes: `tracing` records among the periodic
    // measurement stream and relayed app output.
    const LAUNCH: &[u8] = b"\x1b[2m2026-10-11T02:22:31.204118Z\x1b[0m \x1b[32m INFO\x1b[0m \x1b[2mtilo::launch_latency\x1b[0m\x1b[2m:\x1b[0m application launch requested package_id=com.onepassword.OnePassword\n";
    const XWAYLAND: &[u8] = b"2026-10-11T02:22:31.981004Z  INFO tilo::xwayland: starting Xwayland bridge for its first client display=:1\n";
    const PAINT_STATS: &[u8] = b"paint-stats: 3/14 painted | events=0 revision=0 anim=0 session=0 keepalive=0 | locked=false blanked=true | anim_sources host=0x0000 host_labels=\"\" | 60.000 Hz target\n";
    const FRAME_LIFECYCLE: &[u8] =
        b"FRAME-LIFECYCLE serial=183875 event=render-retired queue_wait_us=9 render_us=36917\n";
    const APP_STDERR: &[u8] =
        b"[app com.onepassword.OnePassword stderr] INFO  2026-10-11T02:22:33.1+00:00 started\n";

    #[test]
    fn tracing_records_are_events_and_stream_output_is_not() {
        for line in [
            LAUNCH,
            XWAYLAND,
            b"2026-10-11T02:22:31Z ERROR tilo::app_launch: stopped\n".as_slice(),
        ] {
            assert!(is_event(line), "{}", String::from_utf8_lossy(line));
        }
        for line in [
            PAINT_STATS,
            FRAME_LIFECYCLE,
            APP_STDERR,
            b"\n".as_slice(),
            b"INFO no timestamp\n".as_slice(),
        ] {
            assert!(!is_event(line), "{}", String::from_utf8_lossy(line));
        }
    }

    #[test]
    fn a_record_split_across_reads_is_kept_whole_once() {
        let stream = [PAINT_STATS, LAUNCH, FRAME_LIFECYCLE, XWAYLAND].concat();
        let expected = [LAUNCH, XWAYLAND].concat();
        for cut in 1..stream.len() {
            let mut filter = EventFilter::default();
            let mut picked = filter.feed(&stream[..cut]);
            picked.extend(filter.feed(&stream[cut..]));
            picked.extend(filter.finish());
            assert_eq!(picked, expected, "cut at {cut}");
        }
    }

    #[test]
    fn an_unterminated_last_record_is_kept_at_end_of_stream_and_long_lines_stay_bounded() {
        let mut filter = EventFilter::default();
        assert!(filter.feed(&XWAYLAND[..XWAYLAND.len() - 1]).is_empty());
        assert_eq!(filter.finish(), XWAYLAND);

        let mut filter = EventFilter::new(1024);
        for _ in 0..64 {
            assert!(filter.feed(&[b'x'; 4096]).is_empty());
        }
        assert!(filter.pending() <= 1024);
    }

    fn entries(directory: &Path) -> Vec<PathBuf> {
        let mut entries: Vec<PathBuf> = fs::read_dir(directory)
            .expect("read directory")
            .map(|entry| entry.expect("entry").path())
            .collect();
        entries.sort();
        entries
    }

    #[test]
    fn publications_are_private_atomic_and_leave_no_staging_behind() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("latest.log");
        fs::write(&path, b"previous record").expect("previous");
        fs::set_permissions(&path, Permissions::from_mode(0o644)).expect("permissive");
        assert!(persist(&path, b"private diagnostics"));
        assert_eq!(fs::read(&path).expect("record"), b"private diagnostics");
        assert_eq!(
            fs::metadata(&path).expect("metadata").permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(entries(directory.path()), vec![path.clone()]);

        let gone = directory.path().join("gone").join("latest.log");
        assert!(!persist(&gone, b"record"));
        assert_eq!(entries(directory.path()), vec![path]);
    }
}
