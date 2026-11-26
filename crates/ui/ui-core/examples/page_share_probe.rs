//! What a watched share asks of a real page, on the real engine.
//!
//! The engine runs as Browser's windows run it: drawn from its pixels, its page captured as a
//! display list only while a session shares the window with someone watching. A web view draws
//! it, as a window does, and the probe plays the session: it says the window is watched (what
//! `FloatingSurfaceHostMessage::PaintKeyframe` does in an app), turns the loop as the app's
//! runner turns it, and reads the share's outbox as the app sends it on. The runner draws a
//! frame for the session's ask and for each wake of the engine, takes the engine's events and
//! syncs its tree before it, and does the same without a frame when the view's own wake time
//! has come; it draws nothing because a widget says it would like a frame. The view is placed and drawn
//! directly, with no window, no layout around it and no fonts: a page drawn from its pixels sets
//! no text, and the probe runs where no font catalogue is prepared. What came out is applied as
//! the device showing the window applies it, and the page it then holds is printed as one line of
//! JSON: whether a whole frame came, after how long, how large, and the text it carries.
//!
//! Three more things are measured where asked. `--changes N`: how long each of the next `N`
//! changes the page makes to itself takes to reach what was handed over (the page stamps each with
//! the clock, as `tick N @MILLISECONDS`). `--still-ms N`: what a page nothing changes costs while
//! it is watched, and again once nobody watches, each over `N` milliseconds: how often the engine
//! woke the loop, how many frames a loop that draws when woken drew, and on how many of its turns
//! the view said it wanted a frame. And always: how large the page is whatever the share carried
//! of it, its whole frame and every image it names, read from the engine.
//!
//! What the page cost its engine is counted too, so that it can be held to numbers no busy
//! machine changes (`captures`, until the page was whole or said to be too large, and
//! `captures_in_all`): how many captures of its layout the engine was asked for and how many it
//! answered, how long each took, how many smaller asks went beside them, and how many pictures
//! the engine painted meanwhile, each of which may ask for one capture more. `--slow-capture-ms
//! N` runs the engine as one whose captures take that long: a capture's answer reaches the app
//! no sooner than `N` milliseconds after it was asked, and what the engine answers behind it
//! waits for it, as an engine answers in the order it is asked. `--order` asks the engine itself
//! for a capture and, straight behind it, for something small, and says which it answered first.
//!
//! What arrived is also printed item by item where asked (`--items`): every run of text with
//! the box it is drawn in, its family list, the class of face a device without those families
//! sets it in, its size, weight and colour, and every fill, border and vector drawing, each
//! with the part of its element it is. The suite compares that with what the page's own script
//! measures of the engine's layout. `--focus` gives the page the keyboard, as a window in front
//! does, so the engine draws its ring around the field being edited; `--zoom LEVEL` zooms the
//! page as the Browser's zoom does; `--settle-ms N` turns the loop that long before the page
//! is read, for what the page still changes after its text arrived. `--snapshot PATH` writes
//! the engine's own layout snapshot of the page there, as the engine hands it over: what the
//! converter's tests keep as their fixtures.
//!
//! `page-share-probe HOST PROFILE URL WIDTH HEIGHT [--watched-before-load] [--within-ms N]
//! [--expect TEXT]... [--has TEXT]... [--changes N] [--still-ms N] [--items] [--focus]
//! [--zoom LEVEL] [--settle-ms N] [--snapshot PATH] [--slow-capture-ms N] [--order]`, run by
//! `crates/engines/chromium-host/chromium/tests/page_share.py` on pages of its own.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "explicit probe executable: a malformed invocation or an engine that does not start ends it at once; it is not a production application"
)]
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cosmic_text::{FontSystem, fontdb};
use tilo_ui_base::Widget;
use tilo_ui_base::presence::PresenceCtx;
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui::time::{Duration, Instant};
use tilo_ui::webview::paint::stream::wire::{Payload, Receiver};
use tilo_ui::webview::paint::stream::{BlobRef, Item};
use tilo_ui::webview::paint::{self, PaintEngine};
use tilo_ui::widget::WebView;
use tilo_ui_base::widget::DrawChild;
use tilo_webview::chromium::ChromiumEngine;
use tilo_webview::{FrameSlot, KeyModifiers, SharedEngine, WebViewCommand, WebViewEvent};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

#[allow(
    clippy::disallowed_methods,
    reason = "standalone probe polls its owned engine in bounded 10 ms waits; it has no UI event loop"
)]
fn pause() {
    std::thread::sleep(Duration::from_millis(10));
}

/// What wakes the app's loop.
type Waker = Arc<dyn Fn() + Send + Sync>;

/// What a page cost its engine so far.
#[derive(Default)]
struct Counts {
    /// When each capture the engine has not answered was asked, by its request.
    asked_at: HashMap<u32, Instant>,
    /// Captures of the page's layout the engine was asked for.
    asked: usize,
    /// Captures whose answer reached the app.
    answered: usize,
    /// How long each answered capture took, in milliseconds, in the order they were answered.
    took: Vec<u128>,
    /// What the engine was asked beside its captures: what fields show and what class of face
    /// it sets a family list in.
    asks: usize,
    /// Pictures the engine painted since the share was watched: each is a change of the page
    /// that may ask for one capture more.
    pictures: usize,
    /// Where the newest picture seen is.
    picture: usize,
    /// The engine's answers that wait, in its order, each with when it is let through.
    held: VecDeque<(Instant, WebViewEvent)>,
}

impl Counts {
    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "asked": self.asked,
            "answered": self.answered,
            "took_ms": self.took,
            "slowest_ms": self.took.iter().max(),
            "asks": self.asks,
            "pictures": self.pictures,
        })
    }
}

/// The engine as the share's capture reaches it, with what the page costs it counted, and
/// where asked answering as an engine does whose captures take `slow`.
struct Counted {
    native: SharedEngine,
    slow: Duration,
    waker: Mutex<Option<Waker>>,
    counts: Mutex<Counts>,
}

impl Counted {
    fn over(native: SharedEngine, slow: Duration) -> Arc<Self> {
        Arc::new(Self {
            native,
            slow,
            waker: Mutex::new(None),
            counts: Mutex::new(Counts::default()),
        })
    }

    fn counts(&self) -> std::sync::MutexGuard<'_, Counts> {
        self.counts.lock().expect("the counts")
    }

    /// The share is watched from here: the pictures painted before it ask for no capture.
    fn watched(&self) {
        let picture = self.picture();
        let mut counts = self.counts();
        counts.pictures = 0;
        counts.picture = picture;
    }

    /// Where the engine's newest picture is.
    fn picture(&self) -> usize {
        self.native
            .frames()
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(|frame| Arc::as_ptr(&frame.rgba) as usize))
            .unwrap_or(0)
    }

    /// Wake the app's loop when an answer held until `due` is let through.
    #[allow(
        clippy::disallowed_methods,
        reason = "standalone probe: a held answer wakes the loop as the engine's own thread would, with no UI event loop to ask for the wake"
    )]
    fn wake_at(&self, due: Instant) {
        let Some(waker) = self.waker.lock().expect("the waker").clone() else {
            return;
        };
        let wait = due.saturating_duration_since(tilo_ui::time::now());
        std::thread::spawn(move || {
            std::thread::sleep(wait);
            waker();
        });
    }
}

impl tilo_webview::WebViewEngine for Counted {
    fn set_visible(&self, visible: bool) {
        self.native.set_visible(visible);
    }
    fn set_modifiers(&self, modifiers: KeyModifiers) {
        self.native.set_modifiers(modifiers);
    }
    fn send(&self, command: WebViewCommand) {
        {
            let mut counts = self.counts();
            match &command {
                WebViewCommand::PaintCapture { request_id } => {
                    counts.asked = counts.asked.saturating_add(1);
                    counts.asked_at.insert(*request_id, tilo_ui::time::now());
                }
                WebViewCommand::PaintShown { .. } => {
                    counts.asks = counts.asks.saturating_add(1);
                }
                _ => {}
            }
        }
        self.native.send(command);
    }
    fn frames(&self) -> FrameSlot {
        // Seen as often as the share's capture looks for a new picture, so none it sees is
        // missed here.
        let picture = self.picture();
        let mut counts = self.counts();
        if picture != counts.picture {
            counts.picture = picture;
            counts.pictures = counts.pictures.saturating_add(1);
        }
        drop(counts);
        self.native.frames()
    }
    fn drain_events(&self) -> Vec<WebViewEvent> {
        let now = tilo_ui::time::now();
        let mut out = Vec::new();
        let mut wake = None;
        let mut counts = self.counts();
        let let_through = |counts: &mut Counts, out: &mut Vec<WebViewEvent>| {
            while counts.held.front().is_some_and(|(due, _)| *due <= now) {
                let Some((_, event)) = counts.held.pop_front() else {
                    break;
                };
                if let WebViewEvent::PaintSnapshot { request_id, .. } = &event
                    && let Some(asked) = counts.asked_at.remove(request_id)
                {
                    counts.answered = counts.answered.saturating_add(1);
                    counts
                        .took
                        .push(now.saturating_duration_since(asked).as_millis());
                }
                out.push(event);
            }
        };
        for event in self.native.drain_events() {
            if !matches!(
                event,
                WebViewEvent::PaintSnapshot { .. }
                    | WebViewEvent::PaintShown { .. }
                    | WebViewEvent::PaintResource { .. }
            ) {
                out.push(event);
                continue;
            }
            // No answer overtakes one the engine gave before it.
            let mut due = counts.held.back().map_or(now, |(due, _)| *due);
            if let WebViewEvent::PaintSnapshot { request_id, .. } = &event
                && let Some(asked) = counts.asked_at.get(request_id)
                && let Some(slow) = asked.checked_add(self.slow)
            {
                due = due.max(slow);
            }
            if due > now {
                wake = Some(due);
            }
            counts.held.push_back((due, event));
            let_through(&mut counts, &mut out);
        }
        let_through(&mut counts, &mut out);
        drop(counts);
        if let Some(due) = wake {
            self.wake_at(due);
        }
        out
    }
    fn request_close(&self) -> bool {
        self.native.request_close()
    }
    fn drain_download_events(&self) -> Vec<WebViewEvent> {
        self.native.drain_download_events()
    }
    fn set_waker(&self, waker: Box<dyn Fn() + Send + Sync>) {
        let waker: Waker = Arc::from(waker);
        *self.waker.lock().expect("the waker") = Some(Arc::clone(&waker));
        self.native.set_waker(Box::new(move || waker()));
    }
    fn wrapped(&self) -> Option<&SharedEngine> {
        Some(&self.native)
    }
}

/// What the share's outbox handed over, held as the device showing the window holds it.
#[derive(Default)]
struct Handed {
    receiver: Receiver,
    payloads: usize,
    wholes: usize,
    /// The size of the newest whole frame, encoded.
    whole_bytes: usize,
    largest: usize,
    /// The images that came, by content hash, with their size.
    images: HashMap<[u8; 32], usize>,
    /// The size of a payload the app said is larger than a share hands over, of which only the
    /// start came.
    too_large: Option<usize>,
}

impl Handed {
    /// Take what the app would send its session now.
    fn take(&mut self) {
        for frame in paint::take_share_frames() {
            if !frame.complete() {
                self.too_large = Some(frame.of);
                continue;
            }
            self.payloads = self.payloads.saturating_add(1);
            self.largest = self.largest.max(frame.payload.len());
            if Payload::is_key(&frame.payload) {
                self.wholes = self.wholes.saturating_add(1);
                self.whole_bytes = frame.payload.len();
            }
            let payload = Payload::decode(&frame.payload).expect("a payload the outbox encoded");
            if let Some((hash, bytes)) = self.receiver.apply(payload) {
                self.images.insert(hash, bytes.len());
            }
        }
    }

    /// The text of the page the receiver holds, line by line.
    fn texts(&self) -> Vec<String> {
        self.receiver
            .frame()
            .map(|frame| {
                frame
                    .layers
                    .values()
                    .flat_map(|layer| &layer.items)
                    .filter_map(|item| match item {
                        Item::Text { text, .. } => Some(text.clone()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The images the held page names and nothing brought.
    fn images_missing(&self) -> usize {
        self.receiver.frame().map_or(0, |frame| {
            frame
                .blobs()
                .filter(|blob| match blob {
                    BlobRef::Hash(hash) => !self.images.contains_key(hash),
                    BlobRef::Url(_) => true,
                })
                .count()
        })
    }

    /// The texts of `expected` the held page does not carry.
    fn missing(&self, expected: &[String]) -> Vec<String> {
        let texts = self.texts();
        expected
            .iter()
            .filter(|want| !texts.iter().any(|text| text.contains(want.as_str())))
            .cloned()
            .collect()
    }
}

/// What the probe was asked: the engine and its profile, the page and the size of its window,
/// whether the share is watched before the page loads, how long the page may take, and the text
/// the page handed over must carry.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is one switch of the probe's command line, on or off"
)]
struct Asked {
    host: String,
    profile: String,
    url: String,
    viewport: (u32, u32),
    watched_before_load: bool,
    within: Duration,
    expected: Vec<String>,
    /// Texts the result says the page carries or not, without waiting for them.
    has: Vec<String>,
    /// How many of the page's own changes to time.
    changes: usize,
    /// How long to measure a page nothing changes, watched and then unwatched.
    still: Duration,
    /// Whether to print what arrived item by item.
    items: bool,
    /// Whether the page has the keyboard.
    focus: bool,
    /// The zoom level the page is set to once loaded.
    zoom: Option<f64>,
    /// How long the loop turns before the page is read.
    settle: Duration,
    /// Where to write the engine's layout snapshot of the page.
    snapshot: Option<String>,
    /// How long the engine takes over a capture at the least.
    slow_capture: Duration,
    /// Whether to ask the engine which of two asks it answers first.
    order: bool,
}

impl Asked {
    fn read() -> Self {
        let args: Vec<String> = std::env::args().skip(1).collect();
        assert!(
            args.len() >= 5,
            "HOST PROFILE URL WIDTH HEIGHT [--watched-before-load] [--within-ms N] [--expect TEXT]..."
        );
        let mut asked = Self {
            host: args[0].clone(),
            profile: args[1].clone(),
            url: args[2].clone(),
            viewport: (
                args[3].parse().expect("a width"),
                args[4].parse().expect("a height"),
            ),
            watched_before_load: false,
            within: Duration::from_secs(5),
            expected: Vec::new(),
            has: Vec::new(),
            changes: 0,
            still: Duration::ZERO,
            items: false,
            focus: false,
            zoom: None,
            settle: Duration::ZERO,
            snapshot: None,
            slow_capture: Duration::ZERO,
            order: false,
        };
        let mut rest = args[5..].iter();
        while let Some(arg) = rest.next() {
            match arg.as_str() {
                "--watched-before-load" => asked.watched_before_load = true,
                "--within-ms" => {
                    asked.within = Duration::from_millis(
                        rest.next()
                            .and_then(|ms| ms.parse().ok())
                            .expect("milliseconds"),
                    );
                }
                "--expect" => asked.expected.push(rest.next().expect("a text").clone()),
                "--has" => asked.has.push(rest.next().expect("a text").clone()),
                "--changes" => {
                    asked.changes = rest
                        .next()
                        .and_then(|count| count.parse().ok())
                        .expect("a number of changes");
                }
                "--still-ms" => {
                    asked.still = Duration::from_millis(
                        rest.next()
                            .and_then(|ms| ms.parse().ok())
                            .expect("milliseconds"),
                    );
                }
                "--items" => asked.items = true,
                "--focus" => asked.focus = true,
                "--zoom" => {
                    asked.zoom = Some(
                        rest.next()
                            .and_then(|level| level.parse().ok())
                            .expect("a zoom level"),
                    );
                }
                "--settle-ms" => {
                    asked.settle = Duration::from_millis(
                        rest.next()
                            .and_then(|ms| ms.parse().ok())
                            .expect("milliseconds"),
                    );
                }
                "--snapshot" => asked.snapshot = Some(rest.next().expect("a path").clone()),
                "--slow-capture-ms" => {
                    asked.slow_capture = Duration::from_millis(
                        rest.next()
                            .and_then(|ms| ms.parse().ok())
                            .expect("milliseconds"),
                    );
                }
                "--order" => asked.order = true,
                other => assert!(other.is_empty(), "unknown argument {other}"),
            }
        }
        asked
    }
}

/// The web view of a window, and what a frame of it is drawn with.
struct Window {
    view: WebView,
    /// The document the window's tree is synced against: the view reads nothing of it.
    state: tilo_ui::State,
    text: TextRenderer,
    theme: Theme,
    presence: PresenceCtx,
}

impl Window {
    /// A window whose page fills `viewport`, in logical pixels at scale one.
    fn showing(engine: &SharedEngine, viewport: (u32, u32)) -> Self {
        let mut view = WebView::new(engine.clone());
        view.set_viewport(
            Rect::new(0.0, 0.0, f64::from(viewport.0), f64::from(viewport.1)),
            1.0,
        );
        Self {
            view,
            state: tilo_ui::State::new(),
            // No face at all: nothing here sets text.
            text: TextRenderer::with_font_system(FontSystem::new_with_locale_and_db(
                "en-US".to_owned(),
                fontdb::Database::new(),
            )),
            theme: Theme::dark(),
            presence: PresenceCtx::empty(),
        }
    }

    /// Draw one frame of the view, as a window's redraw does.
    fn draw(&mut self) {
        let mut scene = Scene::new();
        self.view.draw_child(
            &mut scene,
            Affine::IDENTITY,
            &mut self.text,
            &self.theme,
            &self.presence.draw_env(),
        );
    }

    /// Whether the view says it would like a frame: what a host that asks its widgets draws
    /// for, and the app's runner does not.
    fn due(&self) -> bool {
        self.view.needs_redraw() || self.view.needs_animation_frame()
    }

    /// Sync the view, as the runner's state pass syncs the window's tree.
    fn sync(&mut self) {
        let root = self.state.root.clone();
        self.view.sync(&self.state.doc, &root);
    }

    /// Whether the wake time the view asked for has come.
    fn wake_due(&self) -> bool {
        self.view
            .next_wake_deadline()
            .is_some_and(|due| due <= tilo_ui::time::now())
    }
}

/// Load the page, drawing the view as a window with a loading page does.
fn load(window: &mut Window, engine: &SharedEngine, url: &str, handed: &mut Handed) {
    engine.send(WebViewCommand::Navigate(url.to_owned()));
    let deadline = tilo_ui::time::now()
        .checked_add(Duration::from_secs(30))
        .expect("a bounded load");
    loop {
        window.draw();
        let loaded = engine.drain_events().iter().any(|event| {
            matches!(
                event,
                WebViewEvent::LoadFinished | WebViewEvent::LoadFinishedAt { .. }
            )
        });
        handed.take();
        if loaded {
            return;
        }
        assert!(tilo_ui::time::now() < deadline, "the page did not load");
        pause();
    }
}

/// Wait, for at most `asked.within`, until what was handed over is a whole page carrying the
/// text asked for. The first frame is the one the session's message asks the app's loop for;
/// from there the loop turns as the runner turns it. Returns after how many milliseconds the
/// page was whole, and how many frames were drawn.
fn watch(turning: &mut Loop<'_>, asked: &Asked) -> (Option<u128>, u32) {
    let since = tilo_ui::time::now();
    turning.window.draw();
    let mut frames = 1_u32;
    loop {
        let waited = tilo_ui::time::now().saturating_duration_since(since);
        turning.handed.take();
        if turning.handed.receiver.frame().is_some()
            && turning.handed.missing(&asked.expected).is_empty()
        {
            return (Some(waited.as_millis()), frames);
        }
        if waited >= asked.within || turning.handed.too_large.is_some() {
            return (None, frames);
        }
        frames = frames.saturating_add(u32::from(turning.turn()));
    }
}

/// The app's loop as its runner turns it once the page is handed over: the engine wakes it, it
/// takes the engine's events and draws a frame, and otherwise it sleeps. `wakes` counts the
/// engine's wakes.
struct Loop<'a> {
    window: &'a mut Window,
    engine: &'a SharedEngine,
    handed: &'a mut Handed,
    wakes: &'a AtomicUsize,
    /// The wakes already served.
    served: usize,
}

/// What one stretch of the loop cost.
#[derive(Default)]
struct Cost {
    turns: u32,
    /// How often the engine woke the loop.
    wakes: usize,
    /// Frames drawn: one for each turn the engine woke.
    frames: u32,
    /// Turns on which the view said it wanted a frame, which a host that asks it would draw.
    view_asks: u32,
    /// Payloads handed over meanwhile.
    payloads: usize,
    /// Pictures the engine painted meanwhile.
    pictures: u32,
}

impl Cost {
    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "turns": self.turns,
            "wakes": self.wakes,
            "frames": self.frames,
            "view_asks": self.view_asks,
            "payloads": self.payloads,
            "pictures": self.pictures,
        })
    }
}

impl Loop<'_> {
    /// Where the engine's newest picture is.
    fn picture(&self) -> usize {
        self.engine
            .frames()
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(|frame| Arc::as_ptr(&frame.rgba) as usize))
            .unwrap_or(0)
    }

    /// One turn: the state pass and a frame when the engine woke the loop since the last one,
    /// the state pass alone when the view's own wake time has come. Whether it drew.
    fn turn(&mut self) -> bool {
        pause();
        let wakes = self.wakes.load(Ordering::Relaxed);
        let woken = wakes != self.served;
        if woken || self.window.wake_due() {
            drop(self.engine.drain_events());
            self.window.sync();
        }
        if woken {
            self.served = wakes;
            self.window.draw();
        }
        self.handed.take();
        woken
    }

    /// Turn for `time`, counting what it cost.
    fn run(&mut self, time: Duration) -> Cost {
        let since = tilo_ui::time::now();
        let wakes = self.wakes.load(Ordering::Relaxed);
        let payloads = self.handed.payloads;
        let mut picture = self.picture();
        let mut cost = Cost::default();
        while tilo_ui::time::now().saturating_duration_since(since) < time {
            cost.view_asks = cost.view_asks.saturating_add(u32::from(self.window.due()));
            cost.frames = cost.frames.saturating_add(u32::from(self.turn()));
            cost.turns = cost.turns.saturating_add(1);
            let now = self.picture();
            cost.pictures = cost.pictures.saturating_add(u32::from(now != picture));
            picture = now;
        }
        cost.wakes = self.wakes.load(Ordering::Relaxed).saturating_sub(wakes);
        cost.payloads = self.handed.payloads.saturating_sub(payloads);
        cost
    }

    /// What a page nothing changes costs over `time`, watched and then once nobody watches;
    /// nothing where no time was asked for. What was still loading is let come first.
    fn still(&mut self, time: Duration) -> (Option<Cost>, Option<Cost>) {
        if time.is_zero() {
            return (None, None);
        }
        self.run(Duration::from_millis(2500));
        let watched = self.run(time);
        paint::set_share_watched(false);
        self.run(Duration::from_millis(500));
        (Some(watched), Some(self.run(time)))
    }

    /// How long each of the page's next `count` changes took to reach what was handed over, in
    /// milliseconds: the page stamps a change as `tick N @MILLISECONDS` on the clock, and a change
    /// has arrived when the held page carries its stamp. Gives up after `limit`.
    #[allow(
        clippy::disallowed_methods,
        reason = "the page stamps its changes with the wall clock, which is the only clock it shares with the probe"
    )]
    fn changes(&mut self, count: usize, limit: Duration) -> Vec<u64> {
        let since = tilo_ui::time::now();
        let mut seen: Vec<u64> = Vec::new();
        let mut took = Vec::new();
        while took.len() < count && tilo_ui::time::now().saturating_duration_since(since) < limit {
            self.turn();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| {
                    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
                });
            for text in self.handed.texts() {
                let Some((_, stamp)) = text.split_once(" @") else {
                    continue;
                };
                let Ok(stamp) = stamp.trim().parse::<u64>() else {
                    continue;
                };
                if text.starts_with("tick ") && !seen.contains(&stamp) {
                    // The first stamp was there when the page was handed over: it times nothing.
                    if !seen.is_empty() {
                        took.push(now.saturating_sub(stamp));
                    }
                    seen.push(stamp);
                }
            }
        }
        took
    }
}

/// How large the page is, whatever the share carried of it: its whole frame as the engine holds
/// it, encoded, and the images that frame names.
fn page_size(engine: &SharedEngine) -> serde_json::Value {
    let Some(Payload::Key(frame)) = engine
        .take_paint_stream(true)
        .into_iter()
        .find(|payload| matches!(payload, Payload::Key(_)))
    else {
        return serde_json::Value::Null;
    };
    let whole = paint::stream::encode(&Payload::Key(frame.clone())).map_or(0, |bytes| bytes.len());
    let images: Vec<usize> = frame
        .blobs()
        .filter_map(|blob| match blob {
            BlobRef::Hash(hash) => engine.paint_blob(hash).map(|bytes| bytes.len()),
            BlobRef::Url(_) => None,
        })
        .collect();
    serde_json::json!({
        "whole_bytes": whole,
        "items": frame.layers.values().map(|layer| layer.items.len()).sum::<usize>(),
        "layers": frame.layers.len(),
        "images": images.len(),
        "image_bytes": images.iter().sum::<usize>(),
        "largest_image": images.iter().max().copied().unwrap_or(0),
    })
}

/// Most items of a page the probe prints.
const MAX_ITEMS: usize = 6000;

/// What arrived, item by item, each in the page's viewport: what the suite compares with the
/// engine's own layout.
fn items(handed: &Handed) -> Vec<serde_json::Value> {
    let Some(frame) = handed.receiver.frame() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (id, layer) in &frame.layers {
        let Some((offset, _)) = frame.layer_to_viewport(*id) else {
            continue;
        };
        for item in &layer.items {
            if out.len() >= MAX_ITEMS {
                return out;
            }
            let Some(rect) = item.rect() else {
                continue;
            };
            let rect = rect.translate(offset.x, offset.y);
            let key = item.key();
            let mut said = serde_json::json!({
                "node": key.node,
                "part": key.part,
                "r": [rect.x, rect.y, rect.w, rect.h],
            });
            let more = match item {
                Item::Text {
                    text, font, color, ..
                } => serde_json::json!({
                    "k": "text",
                    "text": text,
                    "family": font.family,
                    "class": paint::font_class(&font.family).name(),
                    "size": font.size,
                    "weight": font.weight,
                    "italic": font.italic,
                    "color": color.0,
                }),
                Item::Fill { color, radius, .. } => {
                    serde_json::json!({"k": "fill", "color": color.0, "radius": radius})
                }
                Item::Border {
                    widths,
                    colors,
                    radius,
                    ..
                } => serde_json::json!({
                    "k": "border",
                    "widths": widths,
                    "color": colors[0].0,
                    "radius": radius,
                }),
                Item::Image { .. } => serde_json::json!({"k": "image"}),
                Item::Vector { shapes, .. } => {
                    serde_json::json!({"k": "vector", "shapes": shapes.len()})
                }
                Item::Layer(_) => continue,
            };
            if let (Some(said), Some(more)) = (said.as_object_mut(), more.as_object()) {
                said.extend(more.clone());
            }
            out.push(said);
        }
    }
    out
}

/// Which of two asks the engine answers first: a capture of the page's layout and, straight
/// behind it, the class of face it sets one family in, which costs it next to nothing. An
/// engine that answers in the order it is asked says the capture first however long the page
/// takes it, which is what tells an engine still at work on a capture from one that lost the
/// request. Asked of the engine itself once nothing else is.
fn order(native: &SharedEngine) -> serde_json::Value {
    const CAPTURE: u32 = 0x7fff_ffe0;
    const SMALL: u32 = 0x7fff_ffe1;
    let since = tilo_ui::time::now();
    native.send(WebViewCommand::PaintCapture {
        request_id: CAPTURE,
    });
    native.send(WebViewCommand::PaintShown {
        request_id: SMALL,
        nodes: Vec::new(),
        families: vec!["serif".to_owned()],
    });
    let deadline = since
        .checked_add(Duration::from_secs(60))
        .expect("a bounded wait");
    let (mut capture_ms, mut small_ms, mut first) = (None, None, None);
    while (capture_ms.is_none() || small_ms.is_none()) && tilo_ui::time::now() < deadline {
        for event in native.drain_events() {
            let waited = tilo_ui::time::now()
                .saturating_duration_since(since)
                .as_millis();
            match event {
                WebViewEvent::PaintSnapshot {
                    request_id: CAPTURE,
                    ..
                } => {
                    capture_ms.get_or_insert(waited);
                    first.get_or_insert("capture");
                }
                WebViewEvent::PaintShown {
                    request_id: SMALL, ..
                }
                | WebViewEvent::PaintSnapshot {
                    request_id: SMALL, ..
                } => {
                    small_ms.get_or_insert(waited);
                    first.get_or_insert("small");
                }
                _ => {}
            }
        }
        pause();
    }
    serde_json::json!({"first": first, "capture_ms": capture_ms, "small_ms": small_ms})
}

/// The engine's own layout snapshot of the page, asked of the engine itself once nothing else
/// is: the share's capture keeps its snapshots to itself. `None` when the engine hands none
/// over within ten seconds.
fn snapshot(native: &SharedEngine) -> Option<String> {
    const REQUEST: u32 = 0x7fff_fff0;
    native.send(WebViewCommand::PaintCapture {
        request_id: REQUEST,
    });
    let deadline = tilo_ui::time::now()
        .checked_add(Duration::from_secs(10))
        .expect("a bounded wait");
    while tilo_ui::time::now() < deadline {
        for event in native.drain_events() {
            if let WebViewEvent::PaintSnapshot {
                request_id: REQUEST,
                snapshot: Some(snapshot),
            } = event
            {
                return Some(snapshot);
            }
        }
        pause();
    }
    None
}

fn main() {
    let asked = Asked::read();
    let native: SharedEngine =
        ChromiumEngine::with_host(Path::new(&asked.host), Path::new(&asked.profile))
            .expect("the engine host");
    let counted = Counted::over(native.clone(), asked.slow_capture);
    // As Browser's windows run it: drawn from its pixels, captured only while watched.
    let engine: SharedEngine = PaintEngine::streaming(counted.clone());
    let wakes = Arc::new(AtomicUsize::new(0));
    engine.set_waker(Box::new({
        let wakes = Arc::clone(&wakes);
        move || {
            wakes.fetch_add(1, Ordering::Relaxed);
        }
    }));
    engine.set_visible(true);
    let mut window = Window::showing(&engine, asked.viewport);
    let mut handed = Handed::default();
    if asked.watched_before_load {
        counted.watched();
        paint::set_share_watched(true);
    }
    load(&mut window, &engine, &asked.url, &mut handed);
    if asked.focus {
        engine.send(WebViewCommand::SetFocused(true));
    }
    if let Some(level) = asked.zoom {
        engine.send(WebViewCommand::SetZoomLevel(level));
    }
    // A shell asks for the window's page: the session tells the app.
    if !asked.watched_before_load {
        counted.watched();
        paint::set_share_watched(true);
    }
    let mut turning = Loop {
        window: &mut window,
        engine: &engine,
        handed: &mut handed,
        wakes: &wakes,
        served: wakes.load(Ordering::Relaxed),
    };
    let (complete_ms, frames) = watch(&mut turning, &asked);
    let captures = counted.counts().json();
    if !asked.settle.is_zero() {
        turning.run(asked.settle);
    }
    let change_ms = if asked.changes > 0 {
        turning.changes(asked.changes, Duration::from_secs(30))
    } else {
        Vec::new()
    };
    let (watched, unwatched) = turning.still(asked.still);
    let images_missing = handed.images_missing();
    paint::set_share_watched(true);
    let size = page_size(&engine);
    paint::set_share_watched(false);
    let texts = handed.texts();
    let items = if asked.items {
        items(&handed)
    } else {
        Vec::new()
    };
    if let Some(path) = &asked.snapshot {
        let said = snapshot(&native).expect("the engine's layout snapshot of the page");
        std::fs::write(path, said).expect("the snapshot's file");
    }
    let captures_in_all = counted.counts().json();
    let order = asked.order.then(|| order(&native));
    println!(
        "{}",
        serde_json::json!({
            "whole": handed.receiver.frame().is_some(),
            "complete_ms": complete_ms,
            "frames_drawn": frames,
            "payloads": handed.payloads,
            "wholes": handed.wholes,
            "whole_bytes": handed.whole_bytes,
            "largest_payload": handed.largest,
            "largest_carried": paint::MAX_SHARE_PAYLOAD,
            "missing": handed.missing(&asked.expected),
            "lacks": handed.missing(&asked.has),
            "text_runs": texts.len(),
            "images": handed.images.len(),
            "image_bytes": handed.images.values().sum::<usize>(),
            "images_missing": images_missing,
            "too_large": handed.too_large,
            "captures": captures,
            "captures_in_all": captures_in_all,
            "order": order,
            "page": size,
            "change_ms": change_ms,
            "still_watched": watched.as_ref().map(Cost::json),
            "still_unwatched": unwatched.as_ref().map(Cost::json),
            "texts": texts.iter().take(64).collect::<Vec<_>>(),
            "items": items,
        })
    );
    drop(window);
    drop(engine);
    drop(counted);
    drop(native);
}
