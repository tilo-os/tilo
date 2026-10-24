//! Deterministic in-process engine used by unit tests and `--shots`
//! headless renders. It loads no network content: every "page" is a flat
//! colour derived from a stable hash of the URL, with a darker header
//! band, so different URLs render visibly different frames and pixel
//! assertions stay byte-stable across runs.
//!
//! All received commands are recorded and inspectable via
//! [`TestEngine::commands`], which is what widget tests assert against.

use std::sync::{Arc, Mutex};

use super::{
    Frame, FrameSlot, MediaElementInfo, PageFailure, PageNavigation, PageReply, PageRequest,
    WebViewCommand, WebViewEngine, WebViewEvent,
};
use tilo_lock::MutexRecover as _;

/// Height of the darker "chrome" band painted at the top of every test
/// page, in physical pixels.
const HEADER_BAND_PX: u32 = 24;

#[derive(Default)]
struct Inner {
    history: Vec<String>,
    last_navigation_request: u32,
    history_snapshot: Option<(u32, Vec<usize>)>,
    /// `history.is_empty()` means nothing loaded yet.
    index: usize,
    width: u32,
    height: u32,
    scale_factor: f32,
    events: Vec<WebViewEvent>,
    commands: Vec<WebViewCommand>,
    media_element: Option<MediaElementInfo>,
    page_replies: std::collections::VecDeque<Result<PageReply, PageFailure>>,
    next_download: u64,
    downloads: std::collections::HashMap<String, String>,
    /// Every `set_visible` the engine was told, oldest first.
    visibility: Vec<bool>,
    /// What a paint capture answers with; `None` until a test sets one.
    paint_snapshot: Option<String>,
    paint_resources: std::collections::HashMap<String, Vec<u8>>,
    /// Whether it says it measures its document, as a ready host does.
    measures_document: bool,
    /// What the page has selected, as [`WebViewCommand::CopySelection`]
    /// answers it.
    selection: String,
    /// Links the page asked to open in a new window, not taken yet.
    new_tabs: Vec<(String, bool)>,
    /// Whether it was asked for the page's popups, the popups the page
    /// opened and not yet taken, and the engines of those adopted.
    accepts_popups: bool,
    popups: Vec<super::Popup>,
    /// Whether it was asked for the page's own questions.
    accepts_page_dialogs: bool,
    adopted: std::collections::HashMap<u32, Arc<TestEngine>>,
    /// The tabs it was asked to create, oldest first.
    created: Vec<Arc<TestEngine>>,
    /// It paints nothing new, as a busy renderer has not yet.
    holds_frames: bool,
    /// Its pages carry an end mark, as a mail's document does.
    marks_page_end: bool,
    /// Addresses that fail to load, and the network error each fails with.
    failing: std::collections::HashMap<String, String>,
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

    /// Configure the deterministic response to a media-element probe.
    pub fn set_media_element(&self, media: Option<MediaElementInfo>) {
        self.lock_inner().media_element = media;
    }

    /// Every visibility change the engine was told, oldest first.
    pub fn visibility_changes(&self) -> Vec<bool> {
        self.lock_inner().visibility.clone()
    }

    /// Answer every later paint capture with this layout snapshot (raw
    /// `DOMSnapshot` JSON), as a real engine would for its current page.
    pub fn set_paint_snapshot(&self, snapshot: impl Into<String>) {
        self.lock_inner().paint_snapshot = Some(snapshot.into());
    }

    /// Say the engine measures its document (answers
    /// [`WebViewCommand::ReportDocumentSize`]), as a ready host that can does.
    pub fn set_measures_document(&self, measures: bool) {
        self.lock_inner().measures_document = measures;
    }

    /// Say the page asked to open `url` in a new window, as a link the
    /// person followed does.
    pub fn push_new_tab_request(&self, url: impl Into<String>) {
        self.lock_inner().new_tabs.push((url.into(), false));
    }

    /// Say the page opened a popup, as `window.open` with a size does. An
    /// engine nobody asked for popups ignores it, as Chromium's opens it
    /// as a tab.
    pub fn push_popup(&self, id: u32, url: impl Into<String>, width: u32, height: u32) {
        let mut inner = self.lock_inner();
        if inner.accepts_popups {
            inner.popups.push(super::Popup {
                id,
                url: url.into(),
                width,
                height,
            });
        }
    }

    /// Whether it was asked for the page's popups.
    pub fn accepts_popups(&self) -> bool {
        self.lock_inner().accepts_popups
    }

    /// Whether it was asked for the page's own questions.
    pub fn accepts_page_dialogs(&self) -> bool {
        self.lock_inner().accepts_page_dialogs
    }

    /// The tabs it created, oldest first.
    pub fn created_tabs(&self) -> Vec<Arc<TestEngine>> {
        self.lock_inner().created.clone()
    }

    /// The engine a popup was adopted as.
    pub fn adopted(&self, id: u32) -> Option<Arc<TestEngine>> {
        self.lock_inner().adopted.get(&id).cloned()
    }

    /// Say the page has `text` selected: a later
    /// [`WebViewCommand::CopySelection`] answers with it.
    pub fn set_selection(&self, text: impl Into<String>) {
        self.lock_inner().selection = text.into();
    }

    /// Hold back every frame, as a renderer still laying out a page does,
    /// or paint the page at the view's current size now.
    pub fn hold_frames(&self, hold: bool) {
        let mut inner = self.lock_inner();
        inner.holds_frames = hold;
        if !hold {
            self.render_locked(&inner);
        }
    }

    /// Paint its pages with a document's end mark: the bottom-right 2×2
    /// pixels one level off the page beneath, as a mail's document marks
    /// them (`mix-blend-mode: difference` with `rgb(1,1,1)`).
    pub fn mark_page_end(&self) {
        self.lock_inner().marks_page_end = true;
    }

    /// Paint the view at its current size in the page's background colour
    /// and nothing of the page, as Chromium paints a resized view before
    /// the page has drawn there.
    pub fn paint_before_page(&self) {
        let inner = self.lock_inner();
        let (width, height) = (inner.width.max(1), inner.height.max(1));
        let [r, g, b] = inner
            .history
            .get(inner.index)
            .map_or([0xFF; 3], |uri| Self::page_color(uri));
        let pixels = usize::try_from(width)
            .unwrap_or(usize::MAX)
            .saturating_mul(usize::try_from(height).unwrap_or(usize::MAX));
        *self.lock_frames() = Some(Frame {
            trace_id: None,
            damage: None,
            rgba: Arc::new([r, g, b, 0xFF].repeat(pixels)),
            width,
            height,
        });
    }

    /// Deliver a frame painted for an earlier `width` × `height`, as an
    /// engine does that finishes a paint after the view has moved on.
    pub fn paint_stale(&self, width: u32, height: u32) {
        let bytes = usize::try_from(width)
            .unwrap_or(usize::MAX)
            .saturating_mul(usize::try_from(height).unwrap_or(usize::MAX))
            .saturating_mul(4);
        *self.lock_frames() = Some(Frame {
            trace_id: None,
            damage: None,
            rgba: Arc::new(vec![0x80; bytes]),
            width,
            height,
        });
    }

    /// Make every load of `uri` fail with the network `error`
    /// (`ERR_CONNECTION_REFUSED`), as an unreachable site does: the engine
    /// reports the address, the failure and its history, and an owned
    /// navigation's outcome carries the error.
    pub fn fail_address(&self, uri: impl Into<String>, error: impl Into<String>) {
        self.lock_inner().failing.insert(uri.into(), error.into());
    }

    /// Deliver `event` on the next drain, as the engine would on its own (a
    /// page reporting its document size).
    pub fn push_event(&self, event: WebViewEvent) {
        self.lock_inner().events.push(event);
    }

    /// Queue the answer to the next page request. Without one, an outline
    /// is a single document line and every other request succeeds.
    pub fn push_page_reply(&self, reply: Result<PageReply, PageFailure>) {
        self.lock_inner().page_replies.push_back(reply);
    }

    fn page_reply_locked(
        inner: &mut Inner,
        request: &PageRequest,
    ) -> Result<PageReply, PageFailure> {
        if let Some(reply) = inner.page_replies.pop_front() {
            return reply;
        }
        let url = inner.history.get(inner.index).cloned().unwrap_or_default();
        Ok(PageReply {
            outline: matches!(request, PageRequest::Outline | PageRequest::Changes)
                .then(|| format!("- document \"{url} — test page\"")),
            // Logs and lookups answer with an empty, well-formed finding.
            data: match request {
                PageRequest::Console { .. } | PageRequest::Network { .. } => {
                    Some(serde_json::json!({"entries": [], "next": 0, "dropped": 0}))
                }
                PageRequest::At { x, y } => {
                    Some(serde_json::json!({"x": x, "y": y, "element": null}))
                }
                PageRequest::Settle { .. } => {
                    Some(serde_json::json!({"settled": true, "waited_ms": 0}))
                }
                _ => None,
            },
            message: "Done".into(),
            url,
            navigation: if matches!(
                request,
                PageRequest::Click { .. }
                    | PageRequest::Press { .. }
                    | PageRequest::Fill { .. }
                    | PageRequest::Select { .. }
                    | PageRequest::Submit { .. }
            ) {
                PageNavigation::Possible
            } else {
                PageNavigation::None
            },
            loading: false,
        })
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
        if inner.holds_frames {
            return;
        }
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
            for x in 0..width {
                let mark = inner.marks_page_end
                    && x.saturating_add(2) >= width
                    && y.saturating_add(2) >= height;
                let px = if mark { px.map(|c| c.abs_diff(1)) } else { px };
                rgba.extend_from_slice(&[px[0], px[1], px[2], 0xFF]);
            }
        }
        *self.lock_frames() = Some(Frame {
            trace_id: None,
            damage: None,
            rgba: Arc::new(rgba),
            width,
            height,
        });
    }

    fn emit_navigation_locked(&self, inner: &mut Inner) {
        inner.history_snapshot = None;
        let Some(uri) = inner.history.get(inner.index).cloned() else {
            return;
        };
        inner.events.push(WebViewEvent::UriChanged(uri.clone()));
        if let Some(error) = inner.failing.get(&uri).cloned() {
            inner.events.push(WebViewEvent::LoadFailed { uri, error });
        } else {
            inner
                .events
                .push(WebViewEvent::TitleChanged(format!("{uri} — test page")));
            inner.events.push(WebViewEvent::LoadProgress(1.0));
            inner.events.push(WebViewEvent::LoadFinished);
        }
        inner.events.push(WebViewEvent::HistoryChanged {
            can_go_back: inner.index > 0,
            can_go_forward: inner.index.saturating_add(1) < inner.history.len(),
        });
        self.render_locked(inner);
    }
}

impl TestEngine {
    fn navigate_locked(
        &self,
        inner: &mut Inner,
        command: super::NavigationCommand,
    ) -> Result<(), String> {
        match command {
            super::NavigationCommand::Navigate { url } => {
                if !inner.history.is_empty() {
                    inner.history.truncate(inner.index.saturating_add(1));
                }
                inner.history.push(url);
                inner.index = inner.history.len().saturating_sub(1);
            }
            super::NavigationCommand::Back if inner.index > 0 => inner.index -= 1,
            super::NavigationCommand::Forward
                if inner.index.saturating_add(1) < inner.history.len() =>
            {
                inner.index += 1
            }
            super::NavigationCommand::Reload if !inner.history.is_empty() => {}
            super::NavigationCommand::HistoryEntry {
                history_request_id,
                id,
            } => {
                let index = usize::try_from(id.saturating_sub(1)).unwrap_or(usize::MAX);
                if !inner
                    .history_snapshot
                    .as_ref()
                    .is_some_and(|(request, indices)| {
                        *request == history_request_id && indices.contains(&index)
                    })
                {
                    return Err("The history entry is no longer available".into());
                }
                inner.index = index;
            }
            _ => return Err("Navigation is unavailable".into()),
        }
        self.emit_navigation_locked(inner);
        Ok(())
    }

    fn emit_history_locked(
        inner: &mut Inner,
        request_id: u32,
        direction: super::NavigationDirection,
    ) {
        let indices: Vec<_> = match direction {
            super::NavigationDirection::Back => (0..inner.index).rev().take(12).collect(),
            super::NavigationDirection::Forward => (inner.index.saturating_add(1)
                ..inner.history.len())
                .take(12)
                .collect(),
        };
        let entries = indices
            .iter()
            .filter_map(|&index| {
                inner.history.get(index).map(|url| super::NavigationEntry {
                    id: u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX),
                    url: url.clone(),
                    title: format!("{url} — test page"),
                })
            })
            .collect();
        inner.history_snapshot = Some((request_id, indices));
        inner.events.push(WebViewEvent::NavigationHistory {
            request_id,
            direction,
            entries,
        });
    }

    fn emit_media_locked(inner: &mut Inner, request_id: u64, element_id: Option<String>) {
        let element = inner
            .media_element
            .clone()
            .filter(|element| element_id.is_none_or(|id| element.element_id == id));
        let event = element.map_or(
            WebViewEvent::MediaElementUnavailable { request_id },
            |element| WebViewEvent::MediaElement {
                request_id,
                element,
            },
        );
        inner.events.push(event);
    }

    /// Recovers the mutex if a panicking test callback poisoned it while
    /// holding the lock, rather than making every later call into the
    /// engine panic too.
    fn lock_inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock_recover()
    }

    /// Same recovery as [`Self::lock_inner`], for the frame slot.
    fn lock_frames(&self) -> std::sync::MutexGuard<'_, Option<Frame>> {
        self.frames.lock_recover()
    }
}

impl WebViewEngine for TestEngine {
    fn measures_document(&self) -> bool {
        self.lock_inner().measures_document
    }

    fn take_new_tab_requests(&self) -> Vec<(String, bool)> {
        std::mem::take(&mut self.lock_inner().new_tabs)
    }

    fn create_tab(&self) -> Result<super::SharedEngine, String> {
        let tab = TestEngine::new();
        self.lock_inner().created.push(tab.clone());
        Ok(tab)
    }

    fn accept_popups(&self) {
        self.lock_inner().accepts_popups = true;
    }

    fn accept_page_dialogs(&self) {
        self.lock_inner().accepts_page_dialogs = true;
    }

    fn take_popups(&self) -> Vec<super::Popup> {
        std::mem::take(&mut self.lock_inner().popups)
    }

    fn adopt_popup(&self, popup: u32) -> Result<super::SharedEngine, String> {
        let engine = TestEngine::new();
        self.lock_inner().adopted.insert(popup, engine.clone());
        Ok(engine)
    }

    fn navigate_owned(
        &self,
        request_id: u32,
        command: super::NavigationCommand,
    ) -> Result<(), String> {
        if !super::navigation_request_id(request_id) || !command.valid() {
            return Err("Invalid owned navigation request".into());
        }
        let mut inner = self.lock_inner();
        if request_id <= inner.last_navigation_request {
            return Err("Navigation request IDs must increase without reuse".into());
        }
        if inner
            .events
            .iter()
            .filter(|event| matches!(event, WebViewEvent::NavigationOutcome { .. }))
            .count()
            >= 32
        {
            return Err("Navigation outcome capacity reached".into());
        }
        inner.last_navigation_request = request_id;
        inner.commands.push(WebViewCommand::ScopedNavigation {
            request_id,
            command: command.clone(),
        });
        let error = self.navigate_locked(&mut inner, command).err();
        let uri = inner.history.get(inner.index).cloned().unwrap_or_default();
        let error = error.or_else(|| inner.failing.get(&uri).cloned());
        inner.events.push(WebViewEvent::NavigationOutcome {
            request_id,
            uri,
            error,
        });
        Ok(())
    }
    fn set_visible(&self, visible: bool) {
        self.lock_inner().visibility.push(visible);
    }
    #[allow(
        clippy::too_many_lines,
        reason = "the deterministic command interpreter keeps each WebView command's recorded test effect in one exhaustive match"
    )]
    fn send(&self, cmd: WebViewCommand) {
        if let WebViewCommand::ScopedNavigation {
            request_id,
            command,
        } = cmd
        {
            let _ = self.navigate_owned(request_id, command);
            return;
        }
        let mut inner = self.lock_inner();
        inner.commands.push(cmd.clone());
        match cmd {
            WebViewCommand::CancelScopedNavigation { request_id } => {
                inner.events.retain(|event| !matches!(event, WebViewEvent::NavigationOutcome { request_id: id, .. } if *id == request_id));
            }
            WebViewCommand::SetAudioMuted(muted) => {
                inner.events.push(WebViewEvent::AudioMutedChanged(muted));
            }
            WebViewCommand::Navigate(uri) => {
                if !inner.history.is_empty() {
                    let cut = inner.index.saturating_add(1);
                    inner.history.truncate(cut);
                }
                inner.history.push(uri);
                inner.index = inner.history.len().saturating_sub(1);
                self.emit_navigation_locked(&mut inner);
            }
            WebViewCommand::RequestNavigationHistory {
                request_id,
                direction,
            } => {
                Self::emit_history_locked(&mut inner, request_id, direction);
            }
            WebViewCommand::NavigateToHistoryEntry { request_id, id } => {
                let index = usize::try_from(id.saturating_sub(1)).unwrap_or(usize::MAX);
                if inner
                    .history_snapshot
                    .as_ref()
                    .is_some_and(|(request, indices)| {
                        *request == request_id && indices.contains(&index)
                    })
                {
                    inner.index = index;
                    self.emit_navigation_locked(&mut inner);
                }
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
            // Owned navigation returned before locking; input/filter commands
            // are recorded above but have no observable page effect here.
            WebViewCommand::ImportServiceSession(request) => {
                inner.events.push(WebViewEvent::ServiceSessionResult(
                    super::ServiceSessionReply {
                        request_id: request.request_id,
                        session_id: request.session.session_id.clone(),
                        service: request.session.service,
                        imported: false,
                        cookie_count: 0,
                        origin_storage_count: 0,
                    },
                ));
            }
            WebViewCommand::ScopedNavigation { .. }
            | WebViewCommand::SetBlockingScripts(_)
            | WebViewCommand::SetSiteExceptions(_)
            | WebViewCommand::RequestSiteStatus { .. }
            | WebViewCommand::DevTools { .. }
            | WebViewCommand::Credentials(_)
            | WebViewCommand::SiteAction { .. }
            | WebViewCommand::StopLoading
            | WebViewCommand::ExitFullscreen
            | WebViewCommand::MouseMove { .. }
            | WebViewCommand::MouseButton { .. }
            | WebViewCommand::Scroll { .. }
            | WebViewCommand::Key { .. }
            | WebViewCommand::SetFocused(_)
            | WebViewCommand::SetContentFilter(_)
            | WebViewCommand::SetNativeFilterRules { .. }
            | WebViewCommand::SetContentFilters { .. }
            | WebViewCommand::SetUserScripts(_)
            | WebViewCommand::ClearWebsiteData
            | WebViewCommand::SetClearOnExit(_)
            | WebViewCommand::ShowDeveloperTools
            | WebViewCommand::PermissionDecision { .. }
            | WebViewCommand::PermissionDismissed { .. }
            | WebViewCommand::ExtensionAccess { .. }
            | WebViewCommand::ExtensionGrants { .. }
            | WebViewCommand::ExtensionPermissionDecision { .. }
            | WebViewCommand::ExtensionForget { .. }
            | WebViewCommand::ExtensionIdentity { .. }
            | WebViewCommand::ExtensionPanel { .. }
            | WebViewCommand::ExtensionSignInPanel { .. }
            | WebViewCommand::ExtensionClick { .. }
            | WebViewCommand::ExtensionMenuChoice { .. }
            | WebViewCommand::ExtensionNotificationChosen { .. }
            | WebViewCommand::ExtensionOmnibox { .. }
            | WebViewCommand::ExtensionOpenSidePanel { .. }
            | WebViewCommand::DownloadDecision { .. }
            | WebViewCommand::Find { .. }
            | WebViewCommand::ContextMenuAction { .. }
            | WebViewCommand::AnswerPageDialog { .. }
            | WebViewCommand::StopFinding
            | WebViewCommand::SetZoomLevel(_)
            | WebViewCommand::ReportDocumentSize(_)
            | WebViewCommand::CancelNativeDownload { .. }
            | WebViewCommand::RetryDownload { .. }
            | WebViewCommand::CancelAllDownloads => {}
            WebViewCommand::CopySelection { request_id } => {
                let text = inner.selection.clone();
                inner
                    .events
                    .push(WebViewEvent::SelectionText { request_id, text });
            }
            WebViewCommand::RequestMediaElement { request_id } => {
                Self::emit_media_locked(&mut inner, request_id, None);
            }
            WebViewCommand::RequestMediaElementById {
                request_id,
                element_id,
            } => {
                Self::emit_media_locked(&mut inner, request_id, Some(element_id));
            }
            WebViewCommand::Page {
                request_id,
                request,
            } => {
                let result = Self::page_reply_locked(&mut inner, &request);
                inner
                    .events
                    .push(WebViewEvent::PageResult { request_id, result });
            }
            WebViewCommand::PaintCapture { request_id } => {
                let snapshot = inner.paint_snapshot.clone();
                inner.events.push(WebViewEvent::PaintSnapshot {
                    request_id,
                    snapshot,
                });
            }
            WebViewCommand::PaintResource { request_id, url } => {
                let bytes = inner.paint_resources.get(&url).cloned();
                inner
                    .events
                    .push(WebViewEvent::PaintResource { request_id, bytes });
            }
            WebViewCommand::StartDownload { uri } => {
                if !(uri.starts_with("http://") || uri.starts_with("https://")) {
                    drop(inner);
                    return;
                }
                inner.next_download = inner.next_download.saturating_add(1);
                let id = format!("test-download-{}", inner.next_download);
                inner.downloads.insert(id.clone(), uri.clone());
                inner.events.push(WebViewEvent::DownloadRequested {
                    id,
                    uri,
                    suggested_filename: "download.bin".into(),
                });
            }
            WebViewCommand::SetDownloadDestination { id, path } => {
                if std::path::Path::new(&path).is_absolute()
                    && inner.downloads.remove(&id).is_some()
                {
                    inner.events.push(WebViewEvent::DownloadProgress {
                        id: id.clone(),
                        received_bytes: 1024,
                        total_bytes: Some(1024),
                        progress: 1.0,
                    });
                    inner
                        .events
                        .push(WebViewEvent::DownloadFinished { id, path });
                }
            }
            WebViewCommand::CancelDownload { id } => {
                if inner.downloads.remove(&id).is_some() {
                    inner.events.push(WebViewEvent::DownloadFailed {
                        id,
                        error: "Cancelled by user".into(),
                        cancelled: true,
                    });
                }
            }
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
    fn the_test_engine_navigates_renders_resizes_and_records_input() {
        // phase: navigate_emits_full_event_sequence_and_renders
        {
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
        // phase: different_urls_render_different_colors
        {
            assert_ne!(
                TestEngine::page_color("https://example.org"),
                TestEngine::page_color("https://example.com"),
            );
        }
        // phase: resize_rerenders_at_new_dimensions
        {
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
        // phase: input_commands_are_recorded
        {
            let engine = TestEngine::new();
            engine.send(WebViewCommand::MouseMove { x: 3.0, y: 4.0 });
            engine.send(WebViewCommand::SetFocused(true));
            let cmds = engine.commands();
            assert_eq!(cmds.len(), 2);
            assert_eq!(cmds[0], WebViewCommand::MouseMove { x: 3.0, y: 4.0 });
        }
    }

    #[test]
    fn the_test_engine_probes_media_and_guards_downloads() {
        // phase: media_probe_reports_configured_visible_element
        {
            let engine = TestEngine::new();
            let element = MediaElementInfo {
                element_id: "media:test".into(),
                viewport_rect: [0.2, 0.1, 0.6, 0.5],
                intrinsic_size: [1280, 720],
                label: "Video".into(),
            };
            engine.set_media_element(Some(element.clone()));
            engine.send(WebViewCommand::RequestMediaElement { request_id: 7 });
            assert_eq!(
                engine.drain_events(),
                vec![WebViewEvent::MediaElement {
                    request_id: 7,
                    element: element.clone(),
                }]
            );
            engine.send(WebViewCommand::RequestMediaElementById {
                request_id: 8,
                element_id: "media:test".into(),
            });
            assert_eq!(
                engine.drain_events(),
                vec![WebViewEvent::MediaElement {
                    request_id: 8,
                    element,
                }]
            );
        }
        // phase: download_lifecycle_requires_web_url_and_absolute_portal_destination
        {
            #[cfg(windows)]
            let destination = "C:/Temp/guide.pdf";
            #[cfg(not(windows))]
            let destination = "/tmp/guide.pdf";
            let engine = TestEngine::new();
            engine.send(WebViewCommand::StartDownload {
                uri: "file:///etc/passwd".into(),
            });
            assert!(engine.drain_events().is_empty());

            engine.send(WebViewCommand::StartDownload {
                uri: "https://example.test/archive.zip".into(),
            });
            let requested = engine.drain_events();
            let id = match requested.as_slice() {
                [WebViewEvent::DownloadRequested { id, .. }] => id.clone(),
                _ => panic!("expected one genuine download request"),
            };
            engine.send(WebViewCommand::SetDownloadDestination {
                id: id.clone(),
                path: "relative/archive.zip".into(),
            });
            assert!(engine.drain_events().is_empty());
            engine.send(WebViewCommand::CancelDownload { id: id.clone() });
            assert!(matches!(
                engine.drain_events().as_slice(),
                [WebViewEvent::DownloadFailed {
                    id: failed_id,
                    cancelled: true,
                    ..
                }] if failed_id == &id
            ));

            engine.send(WebViewCommand::StartDownload {
                uri: "https://example.test/guide.pdf".into(),
            });
            let requested = engine.drain_events();
            let id = match requested.as_slice() {
                [WebViewEvent::DownloadRequested { id, .. }] => id.clone(),
                _ => panic!("expected one genuine download request"),
            };
            engine.send(WebViewCommand::SetDownloadDestination {
                id: id.clone(),
                path: destination.into(),
            });
            assert!(matches!(
                engine.drain_events().as_slice(),
                [WebViewEvent::DownloadProgress { id: progress_id, .. }, WebViewEvent::DownloadFinished { id: finished_id, path }]
                    if progress_id == &id && finished_id == &id && path == destination
            ));
        }
    }

    #[test]
    fn the_test_engine_walks_and_truncates_history() {
        // phase: history_back_forward_walks_and_reports_state
        {
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
        // phase: navigate_after_back_truncates_forward_history
        {
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
    }
}
