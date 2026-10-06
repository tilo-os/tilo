//! The page a headless capture shows.
//!
//! The deterministic test engine paints every page as a flat saturated colour
//! under a dark band, which reads as a broken placeholder next to real
//! chrome. A capture shows a blank light document instead: the engine still
//! drives navigation, history and events, and only the pixels it hands the
//! web view change.

use std::sync::{Arc, Mutex};

use ui::webview::test_engine::TestEngine;
use ui::webview::{Frame, FrameSlot, SharedEngine, WebViewCommand, WebViewEngine, WebViewEvent};

/// A blank document: warm white, the colour of a page with no styling.
const DOCUMENT: [u8; 4] = [0xFA, 0xF9, 0xF7, 0xFF];

/// The test engine with a blank document in place of its painted page.
pub(crate) struct CapturePage {
    inner: Arc<TestEngine>,
    frames: FrameSlot,
}

impl CapturePage {
    /// A shared engine over a fresh test engine.
    pub(crate) fn engine() -> SharedEngine {
        Arc::new(Self {
            inner: TestEngine::new(),
            frames: Arc::new(Mutex::new(None)),
        })
    }

    /// Keep the blank frame the size of the page the test engine painted.
    /// A frame of the right size is reused as it is, so the web view sees one
    /// allocation until the window is resized.
    fn refresh(&self) {
        let painted = self
            .inner
            .frames()
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(|frame| (frame.width, frame.height)));
        let Some((width, height)) = painted else {
            return;
        };
        let Ok(mut slot) = self.frames.lock() else {
            return;
        };
        if slot
            .as_ref()
            .is_some_and(|frame| (frame.width, frame.height) == (width, height))
        {
            return;
        }
        let pixels = usize::try_from(width)
            .unwrap_or(0)
            .saturating_mul(usize::try_from(height).unwrap_or(0));
        let rgba = DOCUMENT
            .iter()
            .copied()
            .cycle()
            .take(pixels.saturating_mul(4))
            .collect();
        *slot = Some(Frame {
            trace_id: None,
            rgba: Arc::new(rgba),
            width,
            height,
        });
    }
}

impl WebViewEngine for CapturePage {
    fn create_tab(&self) -> Result<SharedEngine, String> {
        self.inner.create_tab()
    }

    fn supports_native_filtering(&self) -> bool {
        self.inner.supports_native_filtering()
    }

    fn send(&self, cmd: WebViewCommand) {
        self.inner.send(cmd);
    }

    fn frames(&self) -> FrameSlot {
        self.refresh();
        self.frames.clone()
    }

    fn drain_events(&self) -> Vec<WebViewEvent> {
        self.inner.drain_events()
    }
}
