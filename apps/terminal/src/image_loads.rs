//! The terminal's decoded images, read from the content store on a worker.

use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use tilo_content::ContentStore;
use ui::lock::MutexRecover as _;
use ui::widget::Image;

use crate::graphics::TerminalImagePlacement;
use tilo_terminal::placement_is_sound;

const MAX_ENTRIES: usize = 128;

enum Entry {
    Loading,
    Ready(Box<Image>),
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

    pub(crate) fn set_content(&mut self, content: ContentStore) {
        self.content = content;
        self.entries.clear();
        self.jobs = None;
    }

    /// The image for `placement`, or `None` after asking `waker` to load it; never waits.
    pub(crate) fn image(
        &mut self,
        placement: &TerminalImagePlacement,
        waker: &Arc<Mutex<ui::WakeHandle>>,
    ) -> Option<&mut Image> {
        // The MIME and dimensions are part of the key: a raw Kitty payload has no header of its own, so a
        // descriptor with altered metadata must not reuse a decoded image.
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
            Some(Entry::Ready(image)) => Some(&mut **image),
            _ => None,
        }
    }

    pub(crate) fn take_finished(&mut self) -> bool {
        let finished = std::mem::take(&mut *self.finished.lock_recover());
        let mut arrived = false;
        for (key, image) in finished {
            let entry = match image {
                Some(image) => {
                    arrived = true;
                    Entry::Ready(Box::new(image))
                }
                None => Entry::Unavailable,
            };
            self.entries.insert(key, entry);
        }
        arrived
    }

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
    if placement.byte_len != u64::try_from(bytes.len()).ok()?
        || !placement_is_sound(placement, &bytes)
    {
        return None;
    }
    decode(placement, &bytes)
}

/// Decode a placement the engine validated: a PNG, or the raw pixels a Kitty transmission carried,
/// widened to RGBA because the picture type draws one.
fn decode(placement: &TerminalImagePlacement, bytes: &[u8]) -> Option<Image> {
    match placement.mime.as_str() {
        "image/png" => Image::from_encoded(bytes).ok(),
        "application/x-kitty-rgb" | "application/x-kitty-rgba" => {
            let channels = if placement.mime.ends_with("rgba") {
                4
            } else {
                3
            };
            let pixels = usize::try_from(
                u64::from(placement.pixel_width) * u64::from(placement.pixel_height),
            )
            .ok()?;
            if bytes.len() != pixels.checked_mul(channels)? {
                return None;
            }
            let rgba = if channels == 4 {
                bytes.to_vec()
            } else {
                let mut rgba = Vec::with_capacity(pixels.checked_mul(4)?);
                for rgb in bytes.chunks_exact(3) {
                    let [red, green, blue] = <&[u8; 3]>::try_from(rgb).ok()?;
                    rgba.extend_from_slice(&[*red, *green, *blue, 255]);
                }
                rgba
            };
            Some(Image::from_rgba(
                Arc::from(rgba),
                placement.pixel_width,
                placement.pixel_height,
            ))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
