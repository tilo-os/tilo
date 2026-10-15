//! A bounded byte ring + a background prefetcher.
//!
//! [`RingBuffer`] is a fixed-capacity FIFO of bytes: pushing past capacity
//! drops the oldest bytes, modelling a streaming buffer that keeps only the
//! most recent window in memory. [`Prefetcher`] drives a reader on a background
//! thread through such a ring, tracking how much of the source has been pulled
//! so a consumer can render a buffering percentage.
//!
//! For a [`crate::Source::Workspace`] the reader is the FUSE mount, so
//! prefetching pulls content down on demand and reports its progress.

use std::collections::VecDeque;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tilo_lock::MutexRecover as _;

/// A fixed-capacity FIFO byte buffer. Writing past `capacity` overwrites the
/// oldest bytes still in the ring.
pub struct RingBuffer {
    buf: VecDeque<u8>,
    capacity: usize,
}

impl RingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(capacity.min(1 << 20)),
            capacity: capacity.max(1),
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Push `bytes`, dropping the oldest bytes if this would exceed capacity.
    /// Only the trailing `capacity` bytes of a longer-than-capacity slice are
    /// retained (the window slides).
    pub fn push_slice(&mut self, bytes: &[u8]) {
        if bytes.len() >= self.capacity {
            self.buf.clear();
            let start = bytes.len().saturating_sub(self.capacity);
            self.buf.extend(bytes.get(start..).unwrap_or(&[]));
            return;
        }
        let overflow = self
            .buf
            .len()
            .saturating_add(bytes.len())
            .saturating_sub(self.capacity);
        for _ in 0..overflow {
            self.buf.pop_front();
        }
        self.buf.extend(bytes);
    }

    /// Pop up to `max` of the oldest bytes into a fresh vector (consumer read).
    pub fn drain(&mut self, max: usize) -> Vec<u8> {
        let n = max.min(self.buf.len());
        self.buf.drain(..n).collect()
    }
}

/// Prefetch progress, cheaply pollable from the UI thread.
#[derive(Clone)]
pub struct PrefetchProgress {
    bytes_read: Arc<AtomicU64>,
    total: Option<u64>,
    done: Arc<AtomicBool>,
    ring: Arc<Mutex<RingBuffer>>,
}

impl PrefetchProgress {
    /// Bytes pulled from the source so far.
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read.load(Ordering::Relaxed)
    }

    /// The source's total byte length, if it was known at spawn.
    pub fn total(&self) -> Option<u64> {
        self.total
    }

    /// Whether the reader has drained the source (or hit its end / an error).
    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }

    /// Buffering percentage 0..=100. With a known total it is read progress;
    /// without one it reports 100 as soon as the read completes, else 0.
    pub fn percent(&self) -> u8 {
        match self.total {
            Some(total) if total > 0 => {
                let read = self.bytes_read().min(total);
                let permille = read.saturating_mul(100).checked_div(total).unwrap_or(0);
                u8::try_from(permille).unwrap_or(100)
            }
            _ => {
                if self.is_done() {
                    100
                } else {
                    0
                }
            }
        }
    }

    /// Drain up to `max` buffered bytes from the ring (the most-recent window).
    pub fn drain(&self, max: usize) -> Vec<u8> {
        self.ring.lock_recover().drain(max)
    }
}

/// A background reader pulling a source through a bounded ring.
pub struct Prefetcher {
    progress: PrefetchProgress,
    _handle: std::thread::JoinHandle<()>,
}

impl Prefetcher {
    /// Spawn a thread that reads `reader` in `chunk`-sized reads into a ring of
    /// `ring_capacity` bytes, counting total bytes pulled. `total` is the
    /// source's byte length when known (drives the percentage).
    ///
    /// # Errors
    ///
    /// Returns the OS error if the prefetch thread could not be spawned (e.g.
    /// the process is out of resources).
    pub fn spawn(
        mut reader: Box<dyn Read + Send>,
        total: Option<u64>,
        ring_capacity: usize,
        chunk: usize,
    ) -> std::io::Result<Self> {
        let bytes_read = Arc::new(AtomicU64::new(0));
        let done = Arc::new(AtomicBool::new(false));
        let ring = Arc::new(Mutex::new(RingBuffer::new(ring_capacity)));
        let progress = PrefetchProgress {
            bytes_read: bytes_read.clone(),
            total,
            done: done.clone(),
            ring: ring.clone(),
        };

        let handle = std::thread::Builder::new()
            .name("media-prefetch".into())
            .spawn(move || {
                let mut scratch = vec![0u8; chunk.max(1)];
                loop {
                    match reader.read(&mut scratch) {
                        Ok(0) => break, // EOF
                        Ok(n) => {
                            ring.lock_recover()
                                .push_slice(scratch.get(..n).unwrap_or(&[]));
                            bytes_read.fetch_add(n as u64, Ordering::Relaxed);
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(_) => break, // treat a read error as end-of-prefetch
                    }
                }
                done.store(true, Ordering::Relaxed);
            })?;

        Ok(Self {
            progress,
            _handle: handle,
        })
    }

    /// A cheap, cloneable handle to this prefetch's progress.
    pub fn progress(&self) -> PrefetchProgress {
        self.progress.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_and_prefetcher_keep_the_trailing_window_and_report_progress() {
        // Phase `ring_slides_and_keeps_the_trailing_window`.
        {
            let mut r = RingBuffer::new(4);
            r.push_slice(&[1, 2, 3]);
            assert_eq!(r.len(), 3);
            r.push_slice(&[4, 5]); // total 5 > cap 4 → drop the oldest one
            assert_eq!(r.len(), 4);
            assert_eq!(r.drain(4), vec![2, 3, 4, 5]);
            assert!(r.is_empty());
        }
        // Phase `ring_push_larger_than_capacity_keeps_only_the_tail`.
        {
            let mut r = RingBuffer::new(3);
            r.push_slice(&[1, 2, 3, 4, 5, 6]);
            assert_eq!(r.len(), 3);
            assert_eq!(r.drain(10), vec![4, 5, 6]);
        }
        // Phase `prefetcher_reads_whole_source_and_reports_full`.
        {
            // A 10 KiB in-memory source with a known total drives percent to 100.
            let data = vec![7u8; 10 * 1024];
            let total = data.len() as u64;
            let reader: Box<dyn Read + Send> = Box::new(std::io::Cursor::new(data));
            let pf = Prefetcher::spawn(reader, Some(total), 4096, 1024).unwrap();
            let progress = pf.progress();

            tilo_test_env::wait_until("the background read", || progress.is_done());
            assert_eq!(progress.bytes_read(), total);
            assert_eq!(progress.percent(), 100);
        }
        // Phase `percent_without_total_is_binary_on_completion`.
        {
            let reader: Box<dyn Read + Send> = Box::new(std::io::Cursor::new(vec![0u8; 256]));
            let pf = Prefetcher::spawn(reader, None, 128, 64).unwrap();
            let progress = pf.progress();
            tilo_test_env::wait_until("the background read", || progress.is_done());
            assert_eq!(progress.percent(), 100, "no total → 100 once done");
        }
    }
}
