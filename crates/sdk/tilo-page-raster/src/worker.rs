//! The render worker for page sources that can render any page from any
//! thread.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use tilo_lock::MutexRecover as _;
use tilo_rt::wake::WakeHandle;
use vello::peniko::ImageData as PenikoImage;

use crate::PageKey;
use crate::queue::BatchQueue;
use crate::tiles::{MAX_TILE_BATCH, TileKey};

/// At most this many page renders are queued for one viewer at a time.
const MAX_PAGE_BATCH: usize = 32;

/// How long one frame may spend serving queued work on the UI thread where
/// no thread can start.
const INLINE_BUDGET: Duration = Duration::from_millis(12);

/// A document whose pages render to RGBA bitmaps.
pub trait PageSource: Send + Sync + 'static {
    /// Page `page` (0-based) rendered `width` device pixels wide.
    fn render_page(&self, page: usize, width: u32) -> Option<PenikoImage>;
    /// One tile of a page at a zoom level (see [`crate::tiles`]).
    fn render_tile(&self, tile: TileKey) -> Option<PenikoImage>;
}

/// A finished render; `None` when the source could not render it.
#[derive(Clone, Debug)]
pub enum Rendered {
    Page {
        key: PageKey,
        image: Option<PenikoImage>,
    },
    Tile {
        key: TileKey,
        image: Option<PenikoImage>,
    },
}

enum Work {
    Page(PageKey),
    Tile(TileKey),
}

#[derive(Default)]
struct State {
    /// Each viewer's source and the version of it the viewer shows.
    sources: HashMap<u64, (u64, Arc<dyn PageSource>)>,
    pages: BatchQueue<PageKey>,
    tiles: BatchQueue<TileKey>,
    results: HashMap<u64, Vec<Rendered>>,
    wakes: HashMap<u64, WakeHandle>,
    rendered: HashMap<u64, u64>,
    /// What viewers let go of, dropped by the worker: tearing down a large
    /// laid-out document costs time no frame should pay.
    retired: Vec<Box<dyn Send>>,
    inline: bool,
}

impl State {
    fn retire_source(&mut self, session: u64) {
        if let Some((_, source)) = self.sources.remove(&session) {
            self.retired.push(Box::new(source));
        }
    }
}

struct Queue {
    state: Mutex<State>,
    ready: Condvar,
}

fn shared_queue() -> &'static Arc<Queue> {
    static QUEUE: OnceLock<Arc<Queue>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        let queue = Arc::new(Queue {
            state: Mutex::new(State::default()),
            ready: Condvar::new(),
        });
        let threads = tilo_rt::available(tilo_rt::Feature::Threads);
        let worker = Arc::clone(&queue);
        let started = threads
            && tilo_rt::spawn_background_thread("page-raster", move || serve(&worker)).is_ok();
        if !started {
            // A browser page: the UI thread serves the queue between frames.
            queue.state.lock_recover().inline = true;
        }
        queue
    })
}

/// A render to run: the viewer's session and document version, its pages
/// and the work.
type Job = (u64, u64, Arc<dyn PageSource>, Work);

/// The next render, whole pages before tiles (a zoomed page's placeholder
/// comes before its tiles, so it is never blank while they arrive), with the
/// source it renders from.
fn next_work(state: &mut State) -> Option<Job> {
    loop {
        let (session, work) = if let Some((session, key)) = state.pages.pop() {
            (session, Work::Page(key))
        } else {
            let (session, key) = state.tiles.pop()?;
            (session, Work::Tile(key))
        };
        if let Some((generation, source)) = state.sources.get(&session) {
            return Some((session, *generation, Arc::clone(source), work));
        }
    }
}

fn render(queue: &Queue, (session, generation, source, work): Job) {
    let result = match work {
        Work::Page(key) => Rendered::Page {
            key,
            image: source.render_page(key.0, key.1),
        },
        Work::Tile(key) => Rendered::Tile {
            key,
            image: source.render_tile(key),
        },
    };
    drop(source);
    let wake = deliver(&mut queue.state.lock_recover(), session, generation, result);
    if let Some(wake) = wake {
        wake.wake();
    }
}

/// Hand `result` to `session` if it still shows the source version it was
/// rendered from, returning the window to wake.
fn deliver(
    state: &mut State,
    session: u64,
    generation: u64,
    result: Rendered,
) -> Option<WakeHandle> {
    let current = state
        .sources
        .get(&session)
        .is_some_and(|(shown, _)| *shown == generation);
    if !current {
        return None;
    }
    state.results.entry(session).or_default().push(result);
    let count = state.rendered.entry(session).or_default();
    *count = count.saturating_add(1);
    state.wakes.get(&session).cloned()
}

fn serve(queue: &Queue) {
    loop {
        let (retired, work) = next_task(queue);
        drop(retired);
        if let Some(work) = work {
            render(queue, work);
        }
    }
}

/// What the worker does next: drop what viewers let go of, or render,
/// waiting until there is either.
fn next_task(queue: &Queue) -> (Vec<Box<dyn Send>>, Option<Job>) {
    let mut state = queue.state.lock_recover();
    loop {
        if !state.retired.is_empty() {
            return (std::mem::take(&mut state.retired), None);
        }
        if let Some(work) = next_work(&mut state) {
            return (Vec::new(), Some(work));
        }
        state = queue
            .ready
            .wait(state)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}

fn next_inline(state: &mut State) -> Option<Job> {
    if state.inline { next_work(state) } else { None }
}

fn retired_inline(state: &mut State) -> Vec<Box<dyn Send>> {
    if state.inline {
        std::mem::take(&mut state.retired)
    } else {
        Vec::new()
    }
}

fn pump_inline(queue: &Queue) {
    let started = tilo_clock::now();
    drop(retired_inline(&mut queue.state.lock_recover()));
    loop {
        let Some(work) = next_inline(&mut queue.state.lock_recover()) else {
            return;
        };
        render(queue, work);
        if tilo_clock::now().duration_since(started) >= INLINE_BUDGET {
            return;
        }
    }
}

static NEXT_SESSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// One viewer's connection to the process's page render worker: it renders
/// the pages and tiles the viewer asks for from the source the viewer shows,
/// below the UI thread, and wakes the viewer's window when one is ready.
/// Every viewer shares the one worker; their batches take turns.
pub struct PageRenderer {
    session: u64,
    queue: Arc<Queue>,
}

impl Default for PageRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl PageRenderer {
    pub fn new() -> Self {
        Self {
            session: NEXT_SESSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            queue: Arc::clone(shared_queue()),
        }
    }

    /// Wake `wake`'s window whenever a render for this viewer is ready.
    pub fn set_wake(&self, wake: WakeHandle) {
        self.queue
            .state
            .lock_recover()
            .wakes
            .insert(self.session, wake);
    }

    /// Show `source`, version `generation`: queued work and undelivered
    /// results for the source shown before are dropped, and a render of it
    /// still under way is discarded when it finishes.
    /// The source shown before is dropped by the worker.
    pub fn set_source(&self, generation: u64, source: Arc<dyn PageSource>) {
        let mut state = self.queue.state.lock_recover();
        state.retire_source(self.session);
        state.sources.insert(self.session, (generation, source));
        state.pages.drop_session(self.session);
        state.tiles.drop_session(self.session);
        state.results.remove(&self.session);
        drop(state);
        self.queue.ready.notify_one();
    }

    /// Show nothing: drop the source, its queued work and its results.
    pub fn clear_source(&self) {
        let mut state = self.queue.state.lock_recover();
        state.retire_source(self.session);
        state.pages.drop_session(self.session);
        state.tiles.drop_session(self.session);
        state.results.remove(&self.session);
        drop(state);
        self.queue.ready.notify_one();
    }

    /// Have the worker drop `value` (the viewer's own handle on a source it
    /// replaced), so a large document is never torn down on the UI thread.
    pub fn retire(&self, value: impl Send + 'static) {
        self.queue
            .state
            .lock_recover()
            .retired
            .push(Box::new(value));
        self.queue.ready.notify_one();
    }

    /// Replace this viewer's queued page renders with `pages`, most
    /// important first; an empty batch cancels them.
    pub fn request_pages(&self, pages: &[PageKey]) {
        let mut batch: VecDeque<PageKey> = VecDeque::new();
        for key in pages {
            if batch.len() < MAX_PAGE_BATCH && !batch.contains(key) {
                batch.push_back(*key);
            }
        }
        self.queue
            .state
            .lock_recover()
            .pages
            .replace(self.session, batch);
        self.queue.ready.notify_one();
    }

    /// Replace this viewer's queued tiles with `tiles`, most important
    /// first; an empty batch cancels them.
    pub fn request_tiles(&self, tiles: &[TileKey]) {
        let mut batch: VecDeque<TileKey> = VecDeque::new();
        for key in tiles {
            if batch.len() < MAX_TILE_BATCH && !batch.contains(key) {
                batch.push_back(*key);
            }
        }
        self.queue
            .state
            .lock_recover()
            .tiles
            .replace(self.session, batch);
        self.queue.ready.notify_one();
    }

    /// The renders of the source shown that finished since the last call, in
    /// the order they finished. Where no thread could start, this first
    /// serves the queue for a bounded slice of the frame.
    pub fn drain(&self) -> Vec<Rendered> {
        pump_inline(&self.queue);
        self.queue
            .state
            .lock_recover()
            .results
            .remove(&self.session)
            .unwrap_or_default()
    }

    /// Whether the UI thread serves the worker (no thread could start) and
    /// has work queued, so the window must keep producing frames.
    pub fn wants_frames(&self) -> bool {
        let state = self.queue.state.lock_recover();
        state.inline && !(state.pages.is_empty() && state.tiles.is_empty())
    }

    /// Renders delivered to this viewer so far.
    pub fn rendered(&self) -> u64 {
        self.queue
            .state
            .lock_recover()
            .rendered
            .get(&self.session)
            .copied()
            .unwrap_or(0)
    }
}

impl Drop for PageRenderer {
    fn drop(&mut self) {
        let mut state = self.queue.state.lock_recover();
        state.retire_source(self.session);
        state.pages.drop_session(self.session);
        state.tiles.drop_session(self.session);
        state.results.remove(&self.session);
        state.wakes.remove(&self.session);
        state.rendered.remove(&self.session);
        drop(state);
        self.queue.ready.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Paper {
        renders: AtomicUsize,
    }

    impl PageSource for Paper {
        fn render_page(&self, _page: usize, width: u32) -> Option<PenikoImage> {
            self.renders.fetch_add(1, Ordering::Relaxed);
            Some(PenikoImage {
                data: vec![255u8; usize::try_from(width.pow(2).saturating_mul(4)).unwrap_or(0)]
                    .into(),
                format: vello::peniko::ImageFormat::Rgba8,
                alpha_type: vello::peniko::ImageAlphaType::Alpha,
                width,
                height: width,
            })
        }
        fn render_tile(&self, _tile: TileKey) -> Option<PenikoImage> {
            None
        }
    }

    fn drain_until(renderer: &PageRenderer, count: usize) -> Vec<Rendered> {
        let deadline = tilo_clock::now()
            .checked_add(Duration::from_secs(30))
            .expect("deadline");
        let mut out = Vec::new();
        while out.len() < count && tilo_clock::now() < deadline {
            out.extend(renderer.drain());
            std::thread::sleep(Duration::from_millis(1));
        }
        out
    }

    /// A source that notes the thread it was dropped on.
    struct Noted(Arc<Mutex<Option<String>>>);

    impl PageSource for Noted {
        fn render_page(&self, _page: usize, _width: u32) -> Option<PenikoImage> {
            None
        }
        fn render_tile(&self, _tile: TileKey) -> Option<PenikoImage> {
            None
        }
    }

    impl Drop for Noted {
        fn drop(&mut self) {
            *self.0.lock_recover() = std::thread::current().name().map(str::to_owned);
        }
    }

    #[test]
    fn the_worker_drops_a_source_the_viewer_replaced() {
        let renderer = PageRenderer::new();
        let dropped_on = Arc::new(Mutex::new(None));
        let shown = Arc::new(Noted(Arc::clone(&dropped_on)));
        renderer.set_source(1, shown.clone());
        renderer.set_source(
            2,
            Arc::new(Paper {
                renders: AtomicUsize::new(0),
            }),
        );
        renderer.retire(shown);
        let deadline = tilo_clock::now()
            .checked_add(Duration::from_secs(30))
            .expect("deadline");
        while dropped_on.lock_recover().is_none() && tilo_clock::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(dropped_on.lock_recover().as_deref(), Some("page-raster"));
    }

    #[test]
    fn a_viewer_gets_its_batch_in_order_and_nothing_of_a_source_it_left() {
        let renderer = PageRenderer::new();
        let paper = Arc::new(Paper {
            renders: AtomicUsize::new(0),
        });
        renderer.set_source(1, paper.clone());
        renderer.request_pages(&[(2, 96), (1, 96), (2, 96)]);
        let done = drain_until(&renderer, 2);
        let keys: Vec<PageKey> = done
            .iter()
            .filter_map(|r| match r {
                Rendered::Page {
                    key,
                    image: Some(_),
                } => Some(*key),
                _ => None,
            })
            .collect();
        assert_eq!(keys, vec![(2, 96), (1, 96)]);
        assert_eq!(renderer.rendered(), 2);

        renderer.set_source(2, paper.clone());
        renderer.request_pages(&[(0, 120)]);
        let done = drain_until(&renderer, 1);
        assert!(matches!(
            done.as_slice(),
            [Rendered::Page { key: (0, 120), .. }]
        ));
        assert_eq!(paper.renders.load(Ordering::Relaxed), 3);
    }
}
