//! Chapter layout on a background thread. The reader says which chapters it
//! shows or will show next ([`LayoutWorker::want`]); the worker sets them in
//! that order, one block at a time, and hands each page-sized stretch over
//! as soon as it is set ([`LayoutWorker::take`]), waking the window.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};

use ui::lock::MutexRecover as _;
use ui::text_render::{FontChoice, FontEpoch, MeasuringSeed, TextRenderer};

use crate::model::{Book, Settings};
use crate::paginate::{ChapterLayoutRequest, ChapterLayouter, LaidBlock, LayoutKey};

/// One chapter to lay out, with everything its layout depends on.
#[derive(Clone)]
pub(crate) struct LayoutJob {
    pub key: LayoutKey,
    /// Which request of the reader's this answers: asked again after it let
    /// one go, the chapter is laid out afresh.
    pub ticket: u64,
    pub book: Arc<Book>,
    pub settings: Settings,
    pub content_width: f32,
    pub content_height: f32,
    pub text_scale: f32,
    pub base_family: FontChoice,
    pub font_multiplier: f32,
}

/// A job's identity: its book, key and ticket.
type JobId = (String, LayoutKey, u64);

impl LayoutJob {
    fn id(&self) -> JobId {
        (self.book.id.clone(), self.key, self.ticket)
    }

    fn is(&self, (book, key, ticket): &JobId) -> bool {
        self.key == *key && self.ticket == *ticket && self.book.id == *book
    }
}

/// A stretch of a chapter's layout, following the one handed over before it.
pub(crate) struct LayoutChunk {
    pub book: String,
    pub key: LayoutKey,
    pub ticket: u64,
    pub blocks: Vec<LaidBlock>,
    /// Page starts decided in this stretch.
    pub pages: Vec<f32>,
    /// The flow's height so far.
    pub height: f32,
    pub complete: bool,
}

struct Wanted {
    jobs: Vec<LayoutJob>,
    /// Jobs of `jobs` finished since the list last changed.
    done: Vec<JobId>,
    seed: Option<MeasuringSeed>,
    closed: bool,
}

struct Shared {
    wanted: Mutex<Wanted>,
    signal: Condvar,
    /// Bumped whenever the wanted list changes, so a job in progress can
    /// learn cheaply that it may no longer be wanted.
    generation: AtomicU64,
    arrived: AtomicBool,
}

pub(crate) struct LayoutWorker {
    shared: Arc<Shared>,
    chunks: mpsc::Receiver<LayoutChunk>,
    /// The fonts the worker measures with.
    epoch: Option<FontEpoch>,
    asked: Vec<JobId>,
}

impl LayoutWorker {
    /// Start the worker, below the UI thread. Call it from a frame: results
    /// wake the window whose frame it is. `None` where there are no threads
    /// (the web), where the reader lays out in its frame instead.
    pub(crate) fn spawn() -> Option<Self> {
        let shared = Arc::new(Shared {
            wanted: Mutex::new(Wanted {
                jobs: Vec::new(),
                done: Vec::new(),
                seed: None,
                closed: false,
            }),
            signal: Condvar::new(),
            generation: AtomicU64::new(0),
            arrived: AtomicBool::new(false),
        });
        let (hand, chunks) = mpsc::channel();
        let wake = ui::wake::WakeHandle::new();
        let worker = Arc::clone(&shared);
        ui::rt::spawn_background_thread("ebook-layout", move || run(&worker, &hand, &wake)).ok()?;
        Some(Self {
            shared,
            chunks,
            epoch: None,
            asked: Vec::new(),
        })
    }

    /// Lay out `jobs`, first to last, instead of whatever was asked before;
    /// a job no longer asked for stops at its next block. `text` is the
    /// renderer the reader draws with: the worker measures as it does.
    pub(crate) fn want(&mut self, text: &TextRenderer, jobs: Vec<LayoutJob>) {
        let asked: Vec<JobId> = jobs.iter().map(LayoutJob::id).collect();
        if asked == self.asked {
            return;
        }
        let seed = (!jobs.is_empty()
            && !self.epoch.as_ref().is_some_and(|e| text.font_epoch() == *e))
        .then(|| text.measuring_seed());
        if let Some(seed) = &seed {
            self.epoch = Some(seed.epoch().clone());
        }
        {
            let mut wanted = self.shared.wanted.lock_recover();
            wanted.done.retain(|done| asked.contains(done));
            wanted.jobs = jobs;
            if seed.is_some() {
                wanted.seed = seed;
            }
        }
        self.asked = asked;
        self.shared.generation.fetch_add(1, Ordering::Release);
        self.shared.signal.notify_one();
    }

    /// Whether stretches arrived since the last [`Self::take`].
    pub(crate) fn has_arrivals(&self) -> bool {
        self.shared.arrived.load(Ordering::Acquire)
    }

    /// Every stretch handed over since the last call, in order.
    pub(crate) fn take(&self) -> Vec<LayoutChunk> {
        self.shared.arrived.store(false, Ordering::Release);
        self.chunks.try_iter().collect()
    }
}

impl Drop for LayoutWorker {
    fn drop(&mut self) {
        self.shared.wanted.lock_recover().closed = true;
        self.shared.generation.fetch_add(1, Ordering::Release);
        self.shared.signal.notify_one();
    }
}

/// The next job not yet done, with the seed to measure it with, or `None`
/// once the worker is closed.
fn next_job(shared: &Shared) -> Option<(LayoutJob, Option<MeasuringSeed>, u64)> {
    let mut wanted = shared.wanted.lock_recover();
    loop {
        if wanted.closed {
            return None;
        }
        let generation = shared.generation.load(Ordering::Acquire);
        if let Some(job) = wanted
            .jobs
            .iter()
            .find(|job| !wanted.done.iter().any(|done| job.is(done)))
            .cloned()
        {
            return Some((job, wanted.seed.take(), generation));
        }
        wanted = shared
            .signal
            .wait(wanted)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}

fn run(shared: &Shared, chunks: &mpsc::Sender<LayoutChunk>, wake: &ui::wake::WakeHandle) {
    let mut renderer: Option<TextRenderer> = None;
    while let Some((job, seed, generation)) = next_job(shared) {
        if let Some(seed) = seed {
            renderer = Some(seed.into_renderer());
        }
        let text = renderer.get_or_insert_with(TextRenderer::new);
        let Some(finished) = lay_out(shared, &job, generation, text, |chunk| {
            let sent = chunks.send(chunk).is_ok();
            shared.arrived.store(true, Ordering::Release);
            wake.wake();
            sent
        }) else {
            return;
        };
        if finished {
            shared.wanted.lock_recover().done.push(job.id());
        }
    }
}

/// Lay `job` out, handing each page-sized stretch to `hand`. `Some(true)`
/// once it is done, `Some(false)` when it stopped because nobody wants it
/// any more, `None` when nobody takes stretches.
fn lay_out(
    shared: &Shared,
    job: &LayoutJob,
    mut generation: u64,
    text: &mut TextRenderer,
    mut hand: impl FnMut(LayoutChunk) -> bool,
) -> Option<bool> {
    text.set_text_scale(job.text_scale);
    let blocks = job
        .book
        .chapters
        .get(job.key.chapter)
        .map_or(&[][..], |chapter| &chapter.blocks);
    let request = ChapterLayoutRequest {
        blocks,
        content_width: job.content_width,
        content_height: job.content_height,
        settings: &job.settings,
        chapter: job.key.chapter,
        language: &job.book.language,
        base_family: &job.base_family,
        font_multiplier: job.font_multiplier,
        font_signature: job.key.font_sig,
    };
    let mut layouter = ChapterLayouter::new(&request, text.text_scale());
    let chunk = |blocks, pages, height, complete| LayoutChunk {
        book: job.book.id.clone(),
        key: job.key,
        ticket: job.ticket,
        blocks,
        pages,
        height,
        complete,
    };
    let mut laid = Vec::new();
    let mut handed_to = 0.0;
    while let Some(block) = layouter.next_block(text) {
        laid.push(block);
        let now = shared.generation.load(Ordering::Acquire);
        if now != generation {
            generation = now;
            let wanted = shared.wanted.lock_recover();
            if wanted.closed || !wanted.jobs.iter().any(|other| other.is(&job.id())) {
                return Some(false);
            }
        }
        if layouter.height() - handed_to >= job.content_height {
            handed_to = layouter.height();
            let pages = layouter.take_pages();
            if !hand(chunk(std::mem::take(&mut laid), pages, handed_to, false)) {
                return None;
            }
        }
    }
    let pages = layouter.finish_pages();
    hand(chunk(laid, pages, layouter.height(), true)).then_some(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The worker hands a long chapter over in page-sized stretches that,
    /// put together, are the layout the reader would set in one go.
    #[test]
    fn stretches_add_up_to_the_whole_chapter_layout() {
        let bytes = crate::test_epub::long_classic(2, 400);
        let book = Arc::new(crate::epub::parse_epub(&bytes, 0, "").expect("parses"));
        let settings = Settings::default();
        let (width, height) = (560.0, 720.0);
        let mut text = TextRenderer::new();
        let key = LayoutKey::new(0, width, height, &settings, 0, text.text_scale());
        let mut worker = LayoutWorker::spawn().expect("the worker starts");
        worker.want(
            &text,
            vec![LayoutJob {
                key,
                ticket: 1,
                book: Arc::clone(&book),
                settings: settings.clone(),
                content_width: width,
                content_height: height,
                text_scale: text.text_scale(),
                base_family: FontChoice::Serif,
                font_multiplier: 1.0,
            }],
        );
        let deadline = tilo_test_env::hang_deadline();
        let mut chunks = Vec::new();
        while !chunks.last().is_some_and(|c: &LayoutChunk| c.complete) {
            assert!(ui::time::now() < deadline, "the chapter was laid out");
            chunks.extend(worker.take());
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(chunks.len() > 2, "the chapter came in stretches");
        let whole = crate::paginate::layout_chapter(
            &mut text,
            &ChapterLayoutRequest {
                blocks: &book.chapters[0].blocks,
                content_width: width,
                content_height: height,
                settings: &settings,
                chapter: 0,
                language: &book.language,
                base_family: &FontChoice::Serif,
                font_multiplier: 1.0,
                font_signature: 0,
            },
        );
        let blocks: Vec<&LaidBlock> = chunks.iter().flat_map(|c| &c.blocks).collect();
        let pages: Vec<f32> = chunks
            .iter()
            .flat_map(|c| c.pages.iter().copied())
            .collect();
        assert_eq!(blocks.len(), whole.blocks.len());
        for (got, want) in blocks.iter().zip(&whole.blocks) {
            assert_eq!(got.index, want.index);
            assert_eq!(got.top.to_bits(), want.top.to_bits());
            assert_eq!(got.bottom.to_bits(), want.bottom.to_bits());
            assert_eq!(got.lines.len(), want.lines.len());
        }
        assert_eq!(pages, whole.pages);
        assert!(whole.pages.len() > 10, "the chapter spans many pages");
        assert_eq!(
            chunks.last().map(|c| c.height.to_bits()),
            Some(whole.total_height.to_bits())
        );
    }
}
