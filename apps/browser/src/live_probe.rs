//! Observe the same engine events consumed by the browser during live captures.
use std::sync::{Arc, Mutex};
use ui::webview::{FrameSlot, SharedEngine, WebViewCommand, WebViewEngine, WebViewEvent};

#[derive(Clone, Default, serde::Serialize)]
pub struct PageState {
    pub uri: String,
    pub title: String,
    /// The engine reported load completion. This is not an HTTP status check;
    /// captures of error documents still require visual inspection.
    pub loaded: bool,
    pub error: Option<String>,
}

pub struct LiveProbe {
    inner: SharedEngine,
    state: Mutex<PageState>,
}
impl LiveProbe {
    pub fn new(inner: SharedEngine) -> Arc<Self> {
        Arc::new(Self {
            inner,
            state: Mutex::new(PageState::default()),
        })
    }
    pub fn state(&self) -> PageState {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}
impl WebViewEngine for LiveProbe {
    fn supports_native_filtering(&self) -> bool {
        self.inner.supports_native_filtering()
    }
    fn send(&self, command: WebViewCommand) {
        self.inner.send(command);
    }
    fn frames(&self) -> FrameSlot {
        self.inner.frames()
    }
    fn take_dmabuf(&self) -> Option<ui::webview::DmabufFrame> {
        self.inner.take_dmabuf()
    }
    fn set_waker(&self, waker: Box<dyn Fn() + Send + Sync>) {
        self.inner.set_waker(waker);
    }
    fn drain_events(&self) -> Vec<WebViewEvent> {
        let events = self.inner.drain_events();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for event in &events {
            match event {
                WebViewEvent::UriChanged(uri) => {
                    state.uri.clone_from(uri);
                    state.loaded = false;
                    state.error = None;
                }
                WebViewEvent::TitleChanged(title) => state.title.clone_from(title),
                WebViewEvent::LoadFinished => state.loaded = true,
                WebViewEvent::LoadFinishedAt { uri } if uri == &state.uri => state.loaded = true,
                WebViewEvent::LoadFailed { error, .. } => {
                    state.error = Some(error.clone());
                    state.loaded = false;
                }
                _ => {}
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observing_events_preserves_visible_failures() {
        let engine = ui::webview::chromium::UnavailableEngine::new("missing runtime".into());
        let probe = LiveProbe::new(engine);
        probe.send(WebViewCommand::Navigate("https://example.org".into()));
        let events = probe.drain_events();
        assert_eq!(events.len(), 1);
        assert_eq!(probe.state().error.as_deref(), Some("missing runtime"));
        assert!(!probe.state().loaded);
        assert!(probe.frames().lock().unwrap().is_none());
    }
}
