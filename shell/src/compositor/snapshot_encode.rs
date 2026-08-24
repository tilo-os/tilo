//! PNG encoding for `test_control` snapshots, on threads that are not
//! the compositor's event loop.
//!
//! # Why this exists
//!
//! A snapshot is two very different jobs wearing one name. Capturing is
//! a compositor job: it needs the live widget tree, the renderer and the
//! GPU, so it has to happen on the loop that owns them. Encoding is not
//! — it is `deflate` over a flat RGBA buffer and a write, and it needs
//! nothing from the compositor at all.
//!
//! Measured on an idle machine at 1920x1014, against an *empty* desktop
//! (the cheapest image this path will ever be handed):
//!
//! ```text
//! paint    0.0 ms
//! render   6.5 ms      GPU render + readback + de-pad
//! encode  24.8 ms      png::Encoder + write
//! total   31.4 ms
//! ```
//!
//! Encoding was 79% of it, and every one of those milliseconds was spent
//! on the event loop with input, protocol traffic and the demo's own
//! control commands queued behind it. A desktop with applications on it
//! compresses worse and costs more, so that 24.8 ms is a floor rather
//! than a typical figure.
//!
//! That cost is also what made the *caller* slow. The recording rig hands
//! the compositor a share of each snapshot's cost back before asking for
//! the next one, because a camera that owns the loop starves the demo it
//! is filming — so a dear snapshot bought a long pause as well as itself.
//! Taking encoding off the loop shortens both halves at once.
//!
//! # Shape
//!
//! A small pool of workers drains a **bounded** queue. Bounded matters:
//! one 1920x1014 frame is 7.8 MB, so an unbounded queue in front of a
//! disk that cannot keep up is an out-of-memory bug that only appears on
//! long takes. When the queue is full [`SnapshotEncoder::submit`] blocks
//! the caller, which degrades to the behaviour we had before — the loop
//! waits for encoding — instead of degrading to a crash.
//!
//! [`SnapshotEncoder::drain`] blocks until every submitted frame is on
//! disk. A recording is only complete once it has been called, and the
//! wire exposes it as `drain_snapshots` so a caller that defers can still
//! know when its frames are all there.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

/// How many frames may be waiting to be encoded before [`submit`] starts
/// blocking its caller.
///
/// At 7.8 MB a frame this is the memory ceiling the queue can reach, so
/// it is deliberately small: eight frames is ~62 MB, enough to absorb a
/// disk hiccup without becoming a second place where a long recording
/// can go wrong.
///
/// [`submit`]: SnapshotEncoder::submit
const QUEUE_DEPTH: usize = 8;

/// Workers spawned to encode, when the environment does not say
/// otherwise.
///
/// One 30 fps stream costs `30 * 25 ms` ≈ 0.75 of a core, so two would
/// serve it and three leaves room for the worse compression a real
/// desktop produces. It is deliberately *not* scaled to the machine:
/// several compositors run at once here, and a pool sized to the box
/// would have each of them claim it.
const DEFAULT_WORKERS: usize = 3;

/// Overrides [`DEFAULT_WORKERS`]. Clamped to `1..=8`; anything
/// unparseable leaves the default in place.
const WORKERS_ENV: &str = "QOS_SNAPSHOT_ENCODE_THREADS";

/// One frame on its way to disk.
struct Job {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    path: PathBuf,
}

/// Count of frames submitted but not yet written, and the condvar
/// [`SnapshotEncoder::drain`] waits on.
type Outstanding = Arc<(Mutex<usize>, Condvar)>;

/// A pool of encoder threads plus the queue feeding them.
pub struct SnapshotEncoder {
    /// `None` only between [`shutdown`](Self::shutdown) and drop —
    /// dropping the sender is what tells the workers to exit.
    tx: Option<SyncSender<Job>>,
    outstanding: Outstanding,
    workers: Vec<JoinHandle<()>>,
    /// Set once a worker has failed to write, so a broken recording
    /// reports itself instead of quietly producing a short film.
    failed: Arc<AtomicBool>,
}

impl SnapshotEncoder {
    /// Spawn the pool. Worker count comes from [`WORKERS_ENV`] when set
    /// and [`DEFAULT_WORKERS`] otherwise.
    pub fn new() -> Self {
        let workers_wanted = std::env::var(WORKERS_ENV)
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .map(|n| n.clamp(1, 8))
            .unwrap_or(DEFAULT_WORKERS);
        let (tx, rx) = sync_channel::<Job>(QUEUE_DEPTH);
        let rx = Arc::new(Mutex::new(rx));
        let outstanding: Outstanding = Arc::new((Mutex::new(0), Condvar::new()));
        let failed = Arc::new(AtomicBool::new(false));

        let mut workers = Vec::with_capacity(workers_wanted);
        for i in 0..workers_wanted {
            let rx = Arc::clone(&rx);
            let outstanding = Arc::clone(&outstanding);
            let failed = Arc::clone(&failed);
            let handle = std::thread::Builder::new()
                .name(format!("snapshot-encode-{i}"))
                .spawn(move || {
                    loop {
                        // Held only for the `recv` so the workers take
                        // from the queue in parallel rather than
                        // serialising on the lock while they encode.
                        let job = {
                            let guard = rx.lock().unwrap_or_else(|p| p.into_inner());
                            guard.recv()
                        };
                        let Ok(job) = job else { break };
                        if let Err(e) = encode_png(&job.rgba, job.width, job.height, &job.path) {
                            failed.store(true, Ordering::Relaxed);
                            eprintln!(
                                "nirios compositor: snapshot encode {:?}: {e}",
                                job.path
                            );
                        }
                        // After the write, not before: `drain` returning
                        // has to mean the bytes are on disk.
                        let (lock, cv) = &*outstanding;
                        let mut n = lock.lock().unwrap_or_else(|p| p.into_inner());
                        *n = n.saturating_sub(1);
                        if *n == 0 {
                            cv.notify_all();
                        }
                    }
                })
                .expect("spawn snapshot encoder thread");
            workers.push(handle);
        }
        Self {
            tx: Some(tx),
            outstanding,
            workers,
            failed,
        }
    }

    /// Queue one frame. Blocks while the queue is full — see the module
    /// docs on why that is the wanted failure mode.
    ///
    /// The counter is raised *before* the send so a `drain` racing a
    /// `submit` cannot observe zero for a frame that is already on its
    /// way.
    pub fn submit(&self, rgba: Vec<u8>, width: u32, height: u32, path: PathBuf) {
        let (lock, _) = &*self.outstanding;
        {
            let mut n = lock.lock().unwrap_or_else(|p| p.into_inner());
            *n += 1;
        }
        let job = Job {
            rgba,
            width,
            height,
            path,
        };
        let sent = match self.tx.as_ref() {
            Some(tx) => tx.send(job).is_ok(),
            None => false,
        };
        if !sent {
            // Every worker is gone, so nothing will ever decrement this
            // frame's share of the count. Give it back here or `drain`
            // waits forever for a frame that will never be written.
            let (lock, cv) = &*self.outstanding;
            let mut n = lock.lock().unwrap_or_else(|p| p.into_inner());
            *n = n.saturating_sub(1);
            if *n == 0 {
                cv.notify_all();
            }
            self.failed.store(true, Ordering::Relaxed);
            eprintln!("nirios compositor: snapshot encoder is gone; frame dropped");
        }
    }

    /// Block until every submitted frame has been written.
    ///
    /// Returns `false` if any write failed since the pool was created —
    /// a caller composing a video wants to know that before it treats
    /// the directory as complete.
    pub fn drain(&self) -> bool {
        let (lock, cv) = &*self.outstanding;
        let mut n = lock.lock().unwrap_or_else(|p| p.into_inner());
        while *n > 0 {
            n = cv.wait(n).unwrap_or_else(|p| p.into_inner());
        }
        !self.failed.load(Ordering::Relaxed)
    }

    /// Drain, then stop the workers and join them.
    pub fn shutdown(&mut self) {
        self.drain();
        self.tx = None;
        for handle in self.workers.drain(..) {
            let _ = handle.join();
        }
    }
}

impl Default for SnapshotEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SnapshotEncoder {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Encode `rgba` as an 8-bit RGBA PNG at `path`.
///
/// Shared by the deferred path above and the synchronous one in
/// `native`, so a frame written inline and a frame written by a worker
/// are byte-for-byte the same file.
pub fn encode_png(rgba: &[u8], width: u32, height: u32, path: &Path) -> std::io::Result<()> {
    use std::io::Write;
    let file = std::fs::File::create(path)?;
    let mut writer = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(&mut writer, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut png_writer = encoder
        .write_header()
        .map_err(|e| std::io::Error::other(format!("png header: {e}")))?;
    png_writer
        .write_image_data(rgba)
        .map_err(|e| std::io::Error::other(format!("png write: {e}")))?;
    // Drop the PNG writer first so the underlying BufWriter holds the
    // last bytes; then flush explicitly. Without this, a swiftly-
    // following assertion on file size could race against a still-
    // buffered tail.
    drop(png_writer);
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame handed to the pool lands on disk, and `drain` is what
    /// says so. Without the drain the assertion below is a race; with
    /// it, it is the contract the wire's `drain_snapshots` sells.
    #[test]
    fn submitted_frames_are_on_disk_after_drain() {
        let dir = tempfile::tempdir().expect("tempdir");
        let encoder = SnapshotEncoder::new();
        let paths: Vec<PathBuf> = (0..16)
            .map(|i| dir.path().join(format!("{i:03}.png")))
            .collect();
        for path in &paths {
            // 4x4 solid RGBA — small enough to keep the test quick,
            // real enough to go through the same encoder.
            encoder.submit(vec![0x40u8; 4 * 4 * 4], 4, 4, path.clone());
        }
        assert!(encoder.drain(), "no write should have failed");
        for path in &paths {
            let meta = std::fs::metadata(path)
                .unwrap_or_else(|e| panic!("{path:?} missing after drain: {e}"));
            assert!(meta.len() > 0, "{path:?} is empty after drain");
        }
    }

    /// More frames than the queue can hold must all arrive. This is the
    /// backpressure path: `submit` blocks rather than dropping, so the
    /// count out equals the count in even when the producer outruns the
    /// workers.
    #[test]
    fn backpressure_never_loses_a_frame() {
        let dir = tempfile::tempdir().expect("tempdir");
        let encoder = SnapshotEncoder::new();
        let n = QUEUE_DEPTH * 6;
        for i in 0..n {
            encoder.submit(
                vec![0x11u8; 8 * 8 * 4],
                8,
                8,
                dir.path().join(format!("{i:04}.png")),
            );
        }
        assert!(encoder.drain(), "no write should have failed");
        let written = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .filter(|e| e.is_ok())
            .count();
        assert_eq!(written, n, "every submitted frame must reach the disk");
    }

    /// `drain` on an idle pool returns immediately and reports success,
    /// so a caller that drains twice (or drains without submitting)
    /// does not hang.
    #[test]
    fn draining_an_idle_pool_returns() {
        let encoder = SnapshotEncoder::new();
        assert!(encoder.drain());
        assert!(encoder.drain());
    }

    /// A write into a directory that does not exist is reported rather
    /// than swallowed — `drain` returning `false` is how a recording
    /// finds out it is incomplete.
    #[test]
    fn a_failed_write_is_reported_by_drain() {
        let dir = tempfile::tempdir().expect("tempdir");
        let encoder = SnapshotEncoder::new();
        encoder.submit(
            vec![0u8; 4 * 4 * 4],
            4,
            4,
            dir.path().join("no-such-dir").join("x.png"),
        );
        assert!(
            !encoder.drain(),
            "drain must report the failed write, not hide it"
        );
    }

    /// The bytes a worker writes are a real PNG of the requested size —
    /// the deferred path must not quietly produce something only this
    /// process can read back.
    #[test]
    fn encoded_output_is_a_readable_png_of_the_right_size() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("frame.png");
        encode_png(&[0x7Fu8; 6 * 3 * 4], 6, 3, &path).expect("encode");
        let decoder = png::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(&path).expect("open"),
        ));
        let reader = decoder.read_info().expect("png info");
        let info = reader.info();
        assert_eq!((info.width, info.height), (6, 3));
        assert_eq!(info.color_type, png::ColorType::Rgba);
    }
}
