//! The terminal's decoded images, read from the content store on a worker.
//!
//! A placement names its bytes by content address; reading them can wait on
//! the disk, and a terminal the compositor hosts paints on its frame loop. So
//! painting only looks images up: one not loaded yet is asked of a worker,
//! which reads and decodes it and wakes the window, and the cells it covers
//! keep their outline until it arrives. An image that cannot be shown is
//! remembered as such, so it is read once, not on every frame.

use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use tilo_content::ContentStore;
use ui::lock::MutexRecover as _;
use ui::widget::Image;

use crate::graphics::{TerminalImagePlacement, decode_for_rendering};

/// How many decoded images are kept; past it the cache starts over.
const MAX_ENTRIES: usize = 128;

enum Entry {
    Loading,
    Ready(Image),
    /// Missing, of another length than placed, or not decodable.
    Unavailable,
}

type Finished = Arc<Mutex<Vec<(String, Option<Image>)>>>;

struct Job {
    key: String,
    placement: TerminalImagePlacement,
    waker: Arc<Mutex<ui::WakeHandle>>,
}

pub(crate) struct ImageLoads {
    content: ContentStore,
    entries: HashMap<String, Entry>,
    finished: Finished,
    /// The worker's queue, once one is started.
    jobs: Option<Sender<Job>>,
}

impl ImageLoads {
    pub(crate) fn new(content: ContentStore) -> Self {
        Self {
            content,
            entries: HashMap::new(),
            finished: Arc::default(),
            jobs: None,
        }
    }

    /// Read content from `content` from now on, forgetting what was loaded.
    pub(crate) fn set_content(&mut self, content: ContentStore) {
        self.content = content;
        self.entries.clear();
        self.jobs = None;
    }

    /// The image for `placement` if it is loaded; otherwise ask for it, to
    /// wake the terminal through `waker` once it is read, and return `None`.
    /// Never waits.
    pub(crate) fn image(
        &mut self,
        placement: &TerminalImagePlacement,
        waker: &Arc<Mutex<ui::WakeHandle>>,
    ) -> Option<&mut Image> {
        // The MIME and dimensions form part of the key: raw Kitty payloads
        // have no self-describing header, and a replicated descriptor must
        // not reuse a decoded image under altered metadata.
        let key = format!(
            "{}:{}:{}:{}",
            placement.hash, placement.mime, placement.pixel_width, placement.pixel_height
        );
        if !self.entries.contains_key(&key) {
            if self.entries.len() >= MAX_ENTRIES {
                self.entries.clear();
            }
            let asked = self.ask(Job {
                key: key.clone(),
                placement: placement.clone(),
                waker: Arc::clone(waker),
            });
            let entry = if asked {
                Entry::Loading
            } else {
                Entry::Unavailable
            };
            self.entries.insert(key.clone(), entry);
        }
        match self.entries.get_mut(&key) {
            Some(Entry::Ready(image)) => Some(image),
            _ => None,
        }
    }

    /// Take in what the worker finished; whether any image arrived to paint.
    pub(crate) fn take_finished(&mut self) -> bool {
        let finished = std::mem::take(&mut *self.finished.lock_recover());
        let mut arrived = false;
        for (key, image) in finished {
            // A load the cache forgot meanwhile is still worth keeping.
            let entry = match image {
                Some(image) => {
                    arrived = true;
                    Entry::Ready(image)
                }
                None => Entry::Unavailable,
            };
            self.entries.insert(key, entry);
        }
        arrived
    }

    /// Whether any image is still being loaded.
    #[cfg(test)]
    pub(crate) fn loading(&self) -> bool {
        self.entries
            .values()
            .any(|entry| matches!(entry, Entry::Loading))
    }

    fn ask(&mut self, job: Job) -> bool {
        if let Some(jobs) = &self.jobs {
            match jobs.send(job) {
                Ok(()) => return true,
                Err(mpsc::SendError(job)) => {
                    self.jobs = None;
                    return self.start_worker(job);
                }
            }
        }
        self.start_worker(job)
    }

    /// Start the worker with `first` as its first job. It ends once this
    /// terminal drops its queue.
    fn start_worker(&mut self, first: Job) -> bool {
        let (jobs, queue) = mpsc::channel::<Job>();
        let content = self.content.clone();
        let finished = Arc::clone(&self.finished);
        let spawned = std::thread::Builder::new()
            .name("terminal-images".into())
            .spawn(move || {
                for job in std::iter::once(first).chain(queue) {
                    let image = load(&content, &job.placement);
                    finished.lock_recover().push((job.key, image));
                    crate::inner::request_terminal_wake(&job.waker);
                }
            });
        match spawned {
            Ok(_) => {
                self.jobs = Some(jobs);
                true
            }
            Err(error) => {
                tracing::warn!(%error, "could not start the terminal's image reader");
                false
            }
        }
    }
}

fn load(content: &ContentStore, placement: &TerminalImagePlacement) -> Option<Image> {
    let bytes = content.read(&placement.hash)?;
    (placement.byte_len == u64::try_from(bytes.len()).unwrap_or(u64::MAX))
        .then(|| decode_for_rendering(placement, &bytes))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A content store that counts its reads.
    struct Counted {
        inner: ContentStore,
        opened: Arc<AtomicUsize>,
    }

    impl tilo_content::ContentBackend for Counted {
        fn put(
            &self,
            reader: &mut dyn std::io::Read,
            mime: &str,
        ) -> Result<tilo_content::Stored, tilo_content::ContentError> {
            self.inner.put_reader(reader, mime)
        }

        fn open(
            &self,
            hash: &str,
        ) -> Result<Option<tilo_content::ContentReader>, tilo_content::ContentError> {
            self.opened.fetch_add(1, Ordering::SeqCst);
            Ok(self.inner.open(hash))
        }

        fn has(&self, hash: &str) -> bool {
            self.inner.has(hash)
        }
    }

    fn placement(hash: String, byte_len: u64) -> TerminalImagePlacement {
        TerminalImagePlacement {
            hash,
            mime: "application/x-kitty-rgba".into(),
            byte_len,
            row: 0,
            col: 0,
            columns: 1,
            rows: 1,
            pixel_width: 1,
            pixel_height: 1,
            protocol_id: None,
            alternate_screen: false,
        }
    }

    fn settle(loads: &mut ImageLoads) {
        tilo_test_env::wait_until("the image reads to finish", || {
            loads.take_finished();
            !loads.loading()
        });
    }

    #[test]
    fn an_image_is_read_on_a_worker_and_a_missing_one_only_once() {
        let opened = Arc::new(AtomicUsize::new(0));
        let content = ContentStore::new(Counted {
            inner: ContentStore::memory(),
            opened: Arc::clone(&opened),
        });
        let pixel = content.put(&[255, 0, 0, 255]).unwrap();
        let waker = Arc::new(Mutex::new(ui::WakeHandle::new()));
        let mut loads = ImageLoads::new(content);

        let shown = placement(pixel, 4);
        assert!(
            loads.image(&shown, &waker).is_none(),
            "painting never waits for it"
        );
        settle(&mut loads);
        assert!(
            loads.image(&shown, &waker).is_some(),
            "it is painted once loaded"
        );

        let missing = placement(tilo_content::hash_bytes(b"elsewhere"), 9);
        for _ in 0..5 {
            assert!(loads.image(&missing, &waker).is_none());
            settle(&mut loads);
        }
        assert_eq!(
            opened.load(Ordering::SeqCst),
            2,
            "each image is read once, a missing one included"
        );
    }
}
