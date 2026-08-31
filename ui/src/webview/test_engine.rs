//! Deterministic in-process engine used by unit tests and `--shots`
//! headless renders. It loads no network content: every "page" is a flat
//! colour derived from a stable hash of the URL, with a darker header
//! band, so different URLs render visibly different frames and pixel
//! assertions stay byte-stable across runs.
//!
//! All received commands are recorded and inspectable via
//! [`TestEngine::commands`], which is what widget tests assert against.

use std::sync::{Arc, Mutex};

use super::{Frame, FrameSlot, WebViewCommand, WebViewEngine, WebViewEvent};

/// Height of the darker "chrome" band painted at the top of every test
/// page, in physical pixels.
const HEADER_BAND_PX: u32 = 24;

#[derive(Default)]
struct Inner {
    history: Vec<String>,
    /// `history.is_empty()` means nothing loaded yet.
    index: usize,
    width: u32,
    height: u32,
    scale_factor: f32,
    events: Vec<WebViewEvent>,
    commands: Vec<WebViewCommand>,
}

pub struct TestEngine {
    inner: Mutex<Inner>,
    frames: FrameSlot,
}

impl TestEngine {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                width: 800,
                height: 600,
                scale_factor: 1.0,
                ..Default::default()
            }),
            frames: Arc::new(Mutex::new(None)),
        })
    }

    /// Every command received so far, in order.
    pub fn commands(&self) -> Vec<WebViewCommand> {
        self.lock_inner().commands.clone()
    }

    pub fn current_uri(&self) -> Option<String> {
        let inner = self.lock_inner();
        inner.history.get(inner.index).cloned()
    }

    /// Stable page colour for a URL — exposed so tests can compute the
    /// expected pixels.
    pub fn page_color(uri: &str) -> [u8; 3] {
        // FNV-1a, folded to RGB. Keep in sync with nothing else — this is
        // the definition.
        let mut h: u32 = 2_166_136_261;
        for b in uri.bytes() {
            h ^= u32::from(b);
            h = h.wrapping_mul(16_777_619);
        }
        // The low three bytes of the hash, big-endian — equivalent to the
        // old `(h >> 16) as u8, (h >> 8) as u8, h as u8` but without a
        // truncating cast.
        let [_, r, g, b] = h.to_be_bytes();
        [r, g, b]
    }

    fn render_locked(&self, inner: &Inner) {
        let Some(uri) = inner.history.get(inner.index) else {
            return;
        };
        let (width, height) = (inner.width.max(1), inner.height.max(1));
        let [r, g, b] = Self::page_color(uri);
        let band = [r / 2, g / 2, b / 2];
        let pixel_count = usize::try_from(width)
            .unwrap_or(usize::MAX)
            .saturating_mul(usize::try_from(height).unwrap_or(usize::MAX));
        let mut rgba = Vec::with_capacity(pixel_count.saturating_mul(4));
        for y in 0..height {
            let px: [u8; 3] = if y < HEADER_BAND_PX { band } else { [r, g, b] };
            for _ in 0..width {
                rgba.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
            }
        }
        *self.lock_frames() = Some(Frame {
            rgba: Arc::new(rgba),
            width,
            height,
        });
    }

    fn emit_navigation_locked(&self, inner: &mut Inner) {
        let Some(uri) = inner.history.get(inner.index).cloned() else {
            return;
        };
        inner.events.push(WebViewEvent::UriChanged(uri.clone()));
        inner
            .events
            .push(WebViewEvent::TitleChanged(format!("{uri} — test page")));
        inner.events.push(WebViewEvent::LoadProgress(1.0));
        inner.events.push(WebViewEvent::LoadFinished);
        inner.events.push(WebViewEvent::HistoryChanged {
            can_go_back: inner.index > 0,
            can_go_forward: inner.index.saturating_add(1) < inner.history.len(),
        });
        self.render_locked(inner);
    }
}

impl TestEngine {
    /// Recovers the mutex if a panicking test callback poisoned it while
    /// holding the lock, rather than making every later call into the
    /// engine panic too.
    fn lock_inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Same recovery as [`Self::lock_inner`], for the frame slot.
    fn lock_frames(&self) -> std::sync::MutexGuard<'_, Option<Frame>> {
        self.frames
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl WebViewEngine for TestEngine {
    fn send(&self, cmd: WebViewCommand) {
        let mut inner = self.lock_inner();
        inner.commands.push(cmd.clone());
        match cmd {
            WebViewCommand::Navigate(uri) => {
                if !inner.history.is_empty() {
                    let cut = inner.index.saturating_add(1);
                    inner.history.truncate(cut);
                }
                inner.history.push(uri);
                inner.index = inner.history.len().saturating_sub(1);
                self.emit_navigation_locked(&mut inner);
            }
            WebViewCommand::GoBack => {
                if inner.index > 0 {
                    inner.index = inner.index.saturating_sub(1);
                    self.emit_navigation_locked(&mut inner);
                }
            }
            WebViewCommand::GoForward => {
                if inner.index.saturating_add(1) < inner.history.len() {
                    inner.index = inner.index.saturating_add(1);
                    self.emit_navigation_locked(&mut inner);
                }
            }
            WebViewCommand::Reload => {
                if !inner.history.is_empty() {
                    self.emit_navigation_locked(&mut inner);
                }
            }
            WebViewCommand::Resize {
                width,
                height,
                scale_factor,
            } => {
                inner.width = width;
                inner.height = height;
                inner.scale_factor = scale_factor;
                self.render_locked(&inner);
            }
            // Input and filter commands are recorded (above) but have no
            // observable page effect in the test engine.
            WebViewCommand::StopLoading
            | WebViewCommand::MouseMove { .. }
            | WebViewCommand::MouseButton { .. }
            | WebViewCommand::Scroll { .. }
            | WebViewCommand::Key { .. }
            | WebViewCommand::SetFocused(_)
            | WebViewCommand::SetContentFilter(_)
            | WebViewCommand::SetContentFilters { .. }
            | WebViewCommand::SetUserScripts(_)
            | WebViewCommand::ClearWebsiteData => {}
        }
        // Nothing below uses `inner` — drop the guard here rather than
        // leaving it held until the function's closing brace.
        drop(inner);
    }

    fn frames(&self) -> FrameSlot {
        self.frames.clone()
    }

    fn drain_events(&self) -> Vec<WebViewEvent> {
        std::mem::take(&mut self.lock_inner().events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigate_emits_full_event_sequence_and_renders() {
        let engine = TestEngine::new();
        engine.send(WebViewCommand::Navigate("https://example.org".into()));

        let events = engine.drain_events();
        assert_eq!(
            events[0],
            WebViewEvent::UriChanged("https://example.org".into())
        );
        assert!(
            matches!(events[1], WebViewEvent::TitleChanged(ref t) if t.contains("example.org"))
        );
        assert!(events.contains(&WebViewEvent::LoadFinished));
        assert!(events.contains(&WebViewEvent::HistoryChanged {
            can_go_back: false,
            can_go_forward: false
        }));
        // Draining twice yields nothing new.
        assert!(engine.drain_events().is_empty());

        let frame = engine
            .frames()
            .lock()
            .unwrap()
            .clone()
            .expect("frame rendered");
        assert_eq!((frame.width, frame.height), (800, 600));
        let [r, g, b] = TestEngine::page_color("https://example.org");
        // First pixel is in the header band; below it is the page colour.
        let below = (HEADER_BAND_PX * frame.width * 4) as usize;
        assert_eq!(&frame.rgba[below..below + 4], &[r, g, b, 0xFF]);
        assert_eq!(&frame.rgba[0..4], &[r / 2, g / 2, b / 2, 0xFF]);
    }

    #[test]
    fn different_urls_render_different_colors() {
        assert_ne!(
            TestEngine::page_color("https://example.org"),
            TestEngine::page_color("https://example.com"),
        );
    }

    #[test]
    fn history_back_forward_walks_and_reports_state() {
        let engine = TestEngine::new();
        engine.send(WebViewCommand::Navigate("https://a.test".into()));
        engine.send(WebViewCommand::Navigate("https://b.test".into()));
        engine.drain_events();

        engine.send(WebViewCommand::GoBack);
        assert_eq!(engine.current_uri().as_deref(), Some("https://a.test"));
        let events = engine.drain_events();
        assert!(events.contains(&WebViewEvent::HistoryChanged {
            can_go_back: false,
            can_go_forward: true
        }));

        engine.send(WebViewCommand::GoForward);
        assert_eq!(engine.current_uri().as_deref(), Some("https://b.test"));

        // At the tail: GoForward is a no-op and emits nothing.
        engine.drain_events();
        engine.send(WebViewCommand::GoForward);
        assert!(engine.drain_events().is_empty());
    }

    #[test]
    fn navigate_after_back_truncates_forward_history() {
        let engine = TestEngine::new();
        engine.send(WebViewCommand::Navigate("https://a.test".into()));
        engine.send(WebViewCommand::Navigate("https://b.test".into()));
        engine.send(WebViewCommand::GoBack);
        engine.send(WebViewCommand::Navigate("https://c.test".into()));
        engine.drain_events();

        // b.test is gone: back from c leads to a.
        engine.send(WebViewCommand::GoBack);
        assert_eq!(engine.current_uri().as_deref(), Some("https://a.test"));
    }

    #[test]
    fn resize_rerenders_at_new_dimensions() {
        let engine = TestEngine::new();
        engine.send(WebViewCommand::Navigate("https://example.org".into()));
        engine.send(WebViewCommand::Resize {
            width: 320,
            height: 200,
            scale_factor: 2.0,
        });
        let frame = engine.frames().lock().unwrap().clone().unwrap();
        assert_eq!((frame.width, frame.height), (320, 200));
        assert_eq!(frame.rgba.len(), 320 * 200 * 4);
    }

    #[test]
    fn input_commands_are_recorded() {
        let engine = TestEngine::new();
        engine.send(WebViewCommand::MouseMove { x: 3.0, y: 4.0 });
        engine.send(WebViewCommand::SetFocused(true));
        let cmds = engine.commands();
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0], WebViewCommand::MouseMove { x: 3.0, y: 4.0 });
    }
}
