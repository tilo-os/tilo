//! What a watched share asks of a real page, on the real engine.
//!
//! The engine runs as Browser's windows run it: drawn from its pixels, its page captured as a
//! display list only while a session shares the window with someone watching. A web view draws
//! it, as a window does, and the probe plays the session: it says the window is watched (what
//! `FloatingSurfaceHostMessage::PaintKeyframe` does in an app), draws a frame when the app's own
//! loop would, and reads the share's outbox as the app sends it on. The view is placed and drawn
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
//! `page-share-probe HOST PROFILE URL WIDTH HEIGHT [--watched-before-load] [--within-ms N]
//! [--expect TEXT]... [--has TEXT]... [--changes N] [--still-ms N]`, run by
//! `apps/browser/chromium/tests/page_share.py` on pages of its own.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "explicit probe executable: a malformed invocation or an engine that does not start ends it at once; it is not a production application"
)]
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use cosmic_text::{FontSystem, fontdb};
use tilo_ui::Widget;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::time::Duration;
use tilo_ui::webview::chromium::ChromiumEngine;
use tilo_ui::webview::paint::stream::wire::{Payload, Receiver};
use tilo_ui::webview::paint::stream::{BlobRef, Item};
use tilo_ui::webview::paint::{self, PaintEngine};
use tilo_ui::webview::{SharedEngine, WebViewCommand, WebViewEvent};
use tilo_ui::widget::{DrawChild, WebView};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

#[allow(
    clippy::disallowed_methods,
    reason = "standalone probe polls its owned engine in bounded 10 ms waits; it has no UI event loop"
)]
fn pause() {
    std::thread::sleep(Duration::from_millis(10));
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
}

impl Handed {
    /// Take what the app would send its session now.
    fn take(&mut self) {
        for frame in paint::take_share_frames() {
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
                other => assert!(other.is_empty(), "unknown argument {other}"),
            }
        }
        asked
    }
}

/// The web view of a window, and what a frame of it is drawn with.
struct Window {
    view: WebView,
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
            &self.presence,
        );
    }

    /// Whether the view asks for a frame, as the app's loop asks it.
    fn due(&self) -> bool {
        self.view.needs_redraw() || self.view.needs_animation_frame()
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
/// from there the view is drawn when it says it needs it, as that loop draws it. Returns after
/// how many milliseconds the page was whole, and how many frames were drawn.
fn watch(
    window: &mut Window,
    engine: &SharedEngine,
    asked: &Asked,
    handed: &mut Handed,
) -> (Option<u128>, u32) {
    let since = tilo_ui::time::now();
    let mut frames = 0_u32;
    let mut due = true;
    loop {
        let waited = tilo_ui::time::now().saturating_duration_since(since);
        if due {
            window.draw();
            frames = frames.saturating_add(1);
        }
        drop(engine.drain_events());
        handed.take();
        if handed.receiver.frame().is_some() && handed.missing(&asked.expected).is_empty() {
            return (Some(waited.as_millis()), frames);
        }
        if waited >= asked.within {
            return (None, frames);
        }
        due = window.due();
        pause();
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

    /// One turn: a frame when the engine woke the loop since the last one. Whether it drew.
    fn turn(&mut self) -> bool {
        pause();
        let wakes = self.wakes.load(Ordering::Relaxed);
        let woken = wakes != self.served;
        if woken {
            self.served = wakes;
            drop(self.engine.drain_events());
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

fn main() {
    let asked = Asked::read();
    let native: SharedEngine =
        ChromiumEngine::with_host(Path::new(&asked.host), Path::new(&asked.profile))
            .expect("the engine host");
    // As Browser's windows run it: drawn from its pixels, captured only while watched.
    let engine: SharedEngine = PaintEngine::streaming(native);
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
        paint::set_share_watched(true);
    }
    load(&mut window, &engine, &asked.url, &mut handed);
    // A desk asks for the window's page: the session tells the app.
    if !asked.watched_before_load {
        paint::set_share_watched(true);
    }
    let (complete_ms, frames) = watch(&mut window, &engine, &asked, &mut handed);
    let mut turning = Loop {
        window: &mut window,
        engine: &engine,
        handed: &mut handed,
        wakes: &wakes,
        served: wakes.load(Ordering::Relaxed),
    };
    let change_ms = if asked.changes > 0 {
        turning.changes(asked.changes, Duration::from_secs(30))
    } else {
        Vec::new()
    };
    // A page nothing changes: first let what was still loading come, then count.
    let (watched, unwatched) = if asked.still.is_zero() {
        (None, None)
    } else {
        drop(turning.run(Duration::from_millis(2500)));
        let watched = turning.run(asked.still);
        paint::set_share_watched(false);
        drop(turning.run(Duration::from_millis(500)));
        (Some(watched), Some(turning.run(asked.still)))
    };
    let images_missing = handed.images_missing();
    paint::set_share_watched(true);
    let size = page_size(&engine);
    paint::set_share_watched(false);
    let texts = handed.texts();
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
            "page": size,
            "change_ms": change_ms,
            "still_watched": watched.as_ref().map(Cost::json),
            "still_unwatched": unwatched.as_ref().map(Cost::json),
            "texts": texts.iter().take(64).collect::<Vec<_>>(),
        })
    );
    drop(window);
    drop(engine);
}
