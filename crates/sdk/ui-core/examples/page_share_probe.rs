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
//! `page-share-probe HOST PROFILE URL WIDTH HEIGHT [--watched-before-load] [--within-ms N]
//! [--expect TEXT]...`, run by `apps/browser/chromium/tests/page_share.py` on a page of its own.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "explicit probe executable: a malformed invocation or an engine that does not start ends it at once; it is not a production application"
)]
use std::path::Path;

use cosmic_text::{FontSystem, fontdb};
use tilo_ui::Widget;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::time::Duration;
use tilo_ui::webview::chromium::ChromiumEngine;
use tilo_ui::webview::paint::stream::Item;
use tilo_ui::webview::paint::stream::wire::{Payload, Receiver};
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
            drop(self.receiver.apply(payload));
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

fn main() {
    let asked = Asked::read();
    let native: SharedEngine =
        ChromiumEngine::with_host(Path::new(&asked.host), Path::new(&asked.profile))
            .expect("the engine host");
    // As Browser's windows run it: drawn from its pixels, captured only while watched.
    let engine: SharedEngine = PaintEngine::streaming(native);
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
            "texts": texts.iter().take(64).collect::<Vec<_>>(),
        })
    );
    drop(window);
    drop(engine);
}
