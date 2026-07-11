//! `BrowserWidget` — the quick-ui root widget for the browser app.
//!
//! Chrome around the toolkit's [`ui::widget::WebView`]: a navigation
//! toolbar (back / forward / reload + address input), a thin progress
//! bar while a page loads, and the live page surface. There is no tab
//! strip — tabbing is the compositor's WindowGroup frame primitive, so
//! one browser window shows exactly one page.
//!
//! State model:
//!  * Address text lives in the CRDT under `browser.url` (Automerge
//!    `Text`) — the [`TextInput`] reads/writes it every frame, and the
//!    engine's `UriChanged` events are folded back into it (unless the
//!    user is currently editing).
//!  * Button presses surface as boolean flags (`browser.back_clicked`
//!    etc.); `sync` polls them, translates them into
//!    [`WebViewCommand`]s and resets them.
//!  * Everything engine-side (history, page pixels, load progress)
//!    stays in the engine; the widget only mirrors what the chrome
//!    displays.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, Value};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key as WKey, NamedKey as WNamedKey};

use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::webview::test_engine::TestEngine;
use ui::webview::{SharedEngine, WebViewCommand, WebViewEvent, normalise_url};
use ui::widget::{
    AppKind, AppWidget, Button, Column, Container, IconDescriptor, ProgressBar, Row, TextInput,
    WebView, Widget,
};

use crate::address_overlay::AddressSuggestions;
use crate::history::History;
use crate::settings::{Settings, SettingsPane};

/// CRDT path for the address-bar text.
const URL_PATH: &str = "browser.url";
/// CRDT flags the toolbar buttons write `true` to on click.
const BACK_FLAG: &str = "browser.back_clicked";
const FORWARD_FLAG: &str = "browser.forward_clicked";
const RELOAD_FLAG: &str = "browser.reload_clicked";
/// Gear button → toggle the settings pane.
const SETTINGS_FLAG: &str = "browser.settings_clicked";
/// How many address-bar suggestions to show.
const SUGGESTION_LIMIT: usize = 8;

/// e2e id of the address input (also used internally to detect whether
/// the user is editing it).
const URL_INPUT_ID: &str = "browser.url_input";

/// First page loaded in a fresh live window.
const DEFAULT_URL: &str = "https://duckduckgo.com";
/// Page loaded in `--shots` mode — the test engine renders it as a
/// deterministic flat colour, so snapshots stay byte-stable.
const SHOTS_URL: &str = "https://example.org";

/// Root widget for the browser app. See module docs.
pub struct BrowserWidget {
    rect: Rect,
    engine: SharedEngine,
    composed: Option<Box<dyn Widget>>,
    bootstrap_done: bool,
    /// `--shots` mode: no network, no filter loader, deterministic page.
    headless: bool,
    initial_url: &'static str,

    // Chrome state mirrored from engine events.
    can_go_back: bool,
    can_go_forward: bool,
    /// `1.0` when idle; anything below shows the progress bar.
    progress: f64,
    title: String,
    /// Most recent URI the engine reported — recorded into history on
    /// load-finished.
    current_uri: String,

    // User-facing browser state.
    settings: Settings,
    history: History,
    /// Fuzzy suggestion dropdown under the address bar (a field, not a
    /// tree child: driven here and painted in `draw_overlay`).
    suggestions: AddressSuggestions,
    /// Whether the settings pane replaces the page surface.
    show_settings: bool,
    /// Set when `show_settings` flips, so the host rebuilds the tree.
    structure_dirty: bool,
    /// Last address-bar text a suggestion query ran for, to avoid
    /// re-querying every frame.
    last_query: Option<String>,
    /// Debug/`--shots-overlay`: keep forced suggestions visible without a
    /// real focus + query, so the overlay can be rendered headlessly.
    force_suggestions: bool,
    /// The WebView content rect as window fractions `[x0,y0,x1,y1]`,
    /// published each layout for the zero-copy compositor to blit into.
    content_rect: std::sync::Arc<std::sync::Mutex<Option<[f32; 4]>>>,
}

impl BrowserWidget {
    /// Live-mode widget with the default engine (WPE WebKit where the
    /// backend is available, the deterministic test engine otherwise).
    pub fn new() -> Self {
        Self::with_engine(default_engine(), false)
    }

    /// Headless widget for `--shots`: test engine, pre-navigated to a
    /// deterministic page, never touches the network.
    pub fn new_headless() -> Self {
        let engine: SharedEngine = TestEngine::new();
        engine.send(WebViewCommand::Navigate(SHOTS_URL.to_string()));
        let mut w = Self::with_engine(engine, true);
        w.initial_url = SHOTS_URL;
        w
    }

    /// Handle to the engine this widget drives — the binary uses it to
    /// wire the engine's waker to the winit event-loop proxy.
    pub fn engine_handle(&self) -> SharedEngine {
        self.engine.clone()
    }

    /// Widget over an explicit engine — used by tests.
    pub fn with_engine(engine: SharedEngine, headless: bool) -> Self {
        // Persisted state; `load()` returns defaults/empty when absent, so
        // `--shots` runs are unaffected by (and don't write) real files.
        let settings = if headless { Settings::default() } else { Settings::load() };
        let history = if headless { History::default() } else { History::load() };
        Self {
            rect: Rect::ZERO,
            engine,
            composed: None,
            bootstrap_done: false,
            headless,
            initial_url: DEFAULT_URL,
            can_go_back: false,
            can_go_forward: false,
            progress: 1.0,
            title: String::new(),
            current_uri: String::new(),
            settings,
            history,
            suggestions: AddressSuggestions::new(),
            show_settings: false,
            structure_dirty: false,
            last_query: None,
            force_suggestions: false,
            content_rect: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Test hook: pin a suggestion list into view (bypasses focus/query)
    /// so the overlay can be rendered headlessly for inspection.
    pub fn debug_force_suggestions(&mut self, items: Vec<crate::history::HistoryEntry>) {
        self.suggestions.set_suggestions(items);
        self.force_suggestions = true;
    }

    /// Handle to the live WebView content rect (window fractions), for the
    /// zero-copy compositor. Updated every layout.
    pub fn content_rect_handle(&self) -> std::sync::Arc<std::sync::Mutex<Option<[f32; 4]>>> {
        self.content_rect.clone()
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        // Compact, square-ish icon buttons (44×40 — a comfortable touch
        // target) instead of the toolkit's 120×40 label-button default,
        // so three of them leave the address bar the rest of the width
        // even on a narrow / mobile-width window.
        const NAV_BTN: (f32, f32) = (44.0, 40.0);
        let back = Button::new("browser.back", "", Some(BACK_FLAG))
            .with_icon(Icon::ArrowLeft)
            .with_size(NAV_BTN.0, NAV_BTN.1)
            .with_enabled(self.can_go_back);
        let forward = Button::new("browser.forward", "", Some(FORWARD_FLAG))
            .with_icon(Icon::ArrowRight)
            .with_size(NAV_BTN.0, NAV_BTN.1)
            .with_enabled(self.can_go_forward);
        let reload = Button::new("browser.reload", "", Some(RELOAD_FLAG))
            .with_icon(Icon::Refresh)
            .with_size(NAV_BTN.0, NAV_BTN.1);
        let url_input = TextInput::new(vec![])
            .with_id(URL_INPUT_ID)
            .with_on_change_path(URL_PATH)
            .with_placeholder("Search or enter address");
        let settings_btn = Button::new("browser.settings", "", Some(SETTINGS_FLAG))
            .with_icon(Icon::Settings)
            .with_size(NAV_BTN.0, NAV_BTN.1);

        // `no_wrap` keeps the address input on the same line as the
        // buttons (a wrapping row would push the 100%-width input onto
        // its own line instead of shrinking it to the remaining space).
        let toolbar = Row::new()
            .no_wrap()
            .push(Box::new(back))
            .push(Box::new(forward))
            .push(Box::new(reload))
            .push(Box::new(url_input))
            .push(Box::new(settings_btn));

        // The web view must reach the window edges — no border, no gap.
        // Only the toolbar keeps an inset (its wrapping Container's
        // default padding); the outer column drops all padding and the
        // inter-child gap so the page sits flush under the toolbar and
        // fills the rest of the window.
        let mut column = Column::new()
            .with_no_padding()
            .with_gap(0.0)
            .push(Box::new(Container::new().with_child(Box::new(toolbar))));
        if !self.show_settings && self.progress < 1.0 {
            column = column.push(Box::new(ProgressBar::new(self.progress as f32)));
        }
        // The content region is either the settings pane or the live page.
        let content: Box<dyn Widget> = if self.show_settings {
            Box::new(SettingsPane::with_settings(self.settings.clone()))
        } else {
            Box::new(
                WebView::new(self.engine.clone())
                    .with_ad_blocking(self.settings.adblock_enabled)
                    .with_test_id("browser.surface"),
            )
        };
        column = column.push(content);
        Box::new(column)
    }

    /// One-shot CRDT bootstrap so children can read/write their paths
    /// on the first frame.
    fn bootstrap_crdt(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if state.get(root, URL_PATH).ok().flatten().is_none() {
            if let Ok(obj_id) = state.put_object(root, URL_PATH, ObjType::Text) {
                let _ = state.splice_text(&obj_id, 0, 0, self.initial_url);
            }
        }
        for flag in [BACK_FLAG, FORWARD_FLAG, RELOAD_FLAG, SETTINGS_FLAG] {
            if state.get(root, flag).ok().flatten().is_none() {
                let _ = state.put(root, flag, false);
            }
        }
    }

    fn read_url(state: &AutoCommit, root: &ObjId) -> Option<String> {
        if let Ok(Some((Value::Object(ObjType::Text), obj_id))) = state.get(root, URL_PATH) {
            state.text(&obj_id).ok()
        } else {
            None
        }
    }

    fn read_flag(state: &AutoCommit, root: &ObjId, path: &str) -> bool {
        matches!(
            state.get(root, path),
            Ok(Some((Value::Scalar(s), _)))
                if matches!(s.as_ref(), automerge::ScalarValue::Boolean(true))
        )
    }

    /// Take a raised flag, returning whether it was set (and resetting
    /// it so one click fires exactly once).
    fn take_flag(state: &mut AutoCommit, root: &ObjId, path: &str) -> bool {
        if Self::read_flag(state, root, path) {
            let _ = state.put(root, path, false);
            true
        } else {
            false
        }
    }

    /// Whether the address input currently holds keyboard focus (the
    /// user is editing — engine URI updates must not stomp their text).
    fn url_input_focused(&self) -> bool {
        fn walk(w: &dyn Widget) -> Option<bool> {
            if w.test_id() == Some(URL_INPUT_ID) {
                return Some(w.is_focused());
            }
            for c in w.children() {
                if let Some(f) = walk(c) {
                    return Some(f);
                }
            }
            None
        }
        self.composed
            .as_ref()
            .and_then(|c| walk(c.as_ref()))
            .unwrap_or(false)
    }

    /// Fold engine events into chrome state + the CRDT address text.
    fn apply_engine_events(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let editing = self.url_input_focused();
        for event in self.engine.drain_events() {
            match event {
                WebViewEvent::UriChanged(uri) => {
                    if !editing {
                        ui::put_text_diffed(state, root, URL_PATH, &uri);
                    }
                    self.current_uri = uri;
                }
                WebViewEvent::TitleChanged(title) => self.title = title,
                WebViewEvent::LoadProgress(p) => self.progress = p,
                WebViewEvent::LoadFinished => {
                    self.progress = 1.0;
                    // Record the finished page in persistent history
                    // (real pages only — skip about:/data: and blanks).
                    if !self.headless
                        && self.current_uri.starts_with("http")
                    {
                        self.history.record(&self.current_uri, &self.title);
                        let _ = self.history.save();
                    }
                }
                WebViewEvent::LoadFailed { uri, error } => {
                    self.progress = 1.0;
                    tracing::warn!("load failed for {uri}: {error}");
                    self.title = format!("Failed to load {uri}");
                }
                WebViewEvent::HistoryChanged {
                    can_go_back,
                    can_go_forward,
                } => {
                    self.can_go_back = can_go_back;
                    self.can_go_forward = can_go_forward;
                }
            }
        }
    }

    /// Navigate to whatever the address bar currently holds.
    fn navigate_to_input(&self, state: &AutoCommit, root: &ObjId) {
        if let Some(url) = Self::read_url(state, root) {
            let trimmed = url.trim();
            if !trimmed.is_empty() {
                self.engine
                    .send(WebViewCommand::Navigate(normalise_url(trimmed)));
            }
        }
    }

    /// Enable or disable ad blocking. The WebView widget re-asserts this
    /// each frame from `settings.adblock_enabled`; this direct send makes a
    /// settings-pane toggle take effect immediately, before the pane closes
    /// and the WebView re-appears in the tree.
    fn set_adblock(&self, enabled: bool) {
        self.engine.send(WebViewCommand::SetAdBlocking(enabled));
    }

    /// Poll the settings pane's one-shot + level intents and act on them.
    fn apply_settings_intents(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let intents = SettingsPane::take_pending(state, root);
        if intents.close && self.show_settings {
            self.show_settings = false;
            self.structure_dirty = true;
        }
        if intents.clear_cookies {
            self.engine.send(WebViewCommand::ClearWebsiteData);
        }
        if intents.clear_history {
            self.history.clear();
            let _ = self.history.save();
            self.suggestions.clear();
            self.last_query = None;
        }
        let mut changed = false;
        if let Some(v) = intents.adblock_enabled {
            if v != self.settings.adblock_enabled {
                self.settings.adblock_enabled = v;
                self.set_adblock(v);
                changed = true;
            }
        }
        if let Some(v) = intents.clear_on_exit {
            if v != self.settings.clear_on_exit {
                self.settings.clear_on_exit = v;
                changed = true;
            }
        }
        if changed && !self.headless {
            self.settings.save();
        }
    }

    /// Refresh the address-bar suggestion list from history while the
    /// address input is focused; hide it otherwise.
    fn update_suggestions(&mut self, state: &AutoCommit, root: &ObjId) {
        if self.force_suggestions {
            return; // headless overlay render — keep the pinned list
        }
        let text = if self.show_settings || !self.url_input_focused() {
            String::new()
        } else {
            Self::read_url(state, root).unwrap_or_default()
        };
        let query = text.trim();
        // Only suggest while the user is editing a *query* — not when the
        // bar merely shows the loaded page's URL, and not for an empty bar.
        // (Echoing the current page back at the user is what made the old
        // overlay pointless.)
        let editing = !query.is_empty() && query != self.current_uri.trim();
        if !editing {
            if self.last_query.is_some() {
                self.suggestions.clear();
                self.last_query = None;
            }
            return;
        }
        if self.last_query.as_deref() == Some(query) {
            return;
        }
        self.last_query = Some(query.to_string());
        // Fuzzy history matches, minus the page we're already on.
        let items: Vec<_> = self
            .history
            .suggest(query, SUGGESTION_LIMIT)
            .into_iter()
            .filter(|e| e.url != self.current_uri)
            .collect();
        self.suggestions.set_suggestions(items);
    }

    /// Rect of the address `TextInput` in the composed tree, for anchoring
    /// the suggestion overlay under it.
    fn address_input_rect(&self) -> Option<Rect> {
        self.find_rect(URL_INPUT_ID)
    }

    /// Rect of the composed-tree widget with `test_id == id`, if present.
    fn find_rect(&self, id: &str) -> Option<Rect> {
        fn walk(w: &dyn Widget, id: &str) -> Option<Rect> {
            if w.test_id() == Some(id) {
                return Some(w.get_rect());
            }
            w.children().iter().find_map(|c| walk(*c, id))
        }
        self.composed.as_ref().and_then(|c| walk(c.as_ref(), id))
    }

    /// Publish the WebView surface rect as window fractions for the
    /// zero-copy compositor.
    fn publish_content_rect(&self) {
        let full = self.rect;
        if full.width() <= 0.0 || full.height() <= 0.0 {
            return;
        }
        let frac = self.find_rect("browser.surface").map(|r| {
            [
                ((r.x0 - full.x0) / full.width()) as f32,
                ((r.y0 - full.y0) / full.height()) as f32,
                ((r.x1 - full.x0) / full.width()) as f32,
                ((r.y1 - full.y0) / full.height()) as f32,
            ]
        });
        *self.content_rect.lock().unwrap() = frac;
    }
}

impl Default for BrowserWidget {
    fn default() -> Self {
        Self::new()
    }
}

/// The engine a live browser window runs on: WPE WebKit when the `wpe`
/// feature is compiled in (and its libraries boot), the deterministic
/// test engine otherwise — so the chrome stays fully drivable in
/// hermetic builds.
fn default_engine() -> SharedEngine {
    #[cfg(feature = "wpe")]
    {
        match ui::webview::wpe::WpeEngine::new() {
            Ok(engine) => return engine,
            Err(err) => tracing::warn!("WPE engine unavailable, using test engine: {err}"),
        }
    }
    TestEngine::new()
}

impl Widget for BrowserWidget {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if !self.bootstrap_done {
            self.bootstrap_crdt(state, path);
            if !self.headless {
                // Privacy: "clear on exit" is honoured by clearing the
                // previous session's data at startup — reliable, unlike a
                // teardown hook the windowing layer doesn't expose.
                if self.settings.clear_on_exit {
                    self.engine.send(WebViewCommand::ClearWebsiteData);
                    self.history.clear();
                    let _ = self.history.save();
                }
                // Ad blocking is driven by the WebView widget
                // (`with_ad_blocking`), but send it once here too so it's
                // active from the very first load, before the first sync.
                self.set_adblock(self.settings.adblock_enabled);
                self.engine
                    .send(WebViewCommand::Navigate(self.initial_url.to_string()));
            }
            self.bootstrap_done = true;
        }

        self.apply_engine_events(state, path);

        if Self::take_flag(state, path, BACK_FLAG) {
            self.engine.send(WebViewCommand::GoBack);
        }
        if Self::take_flag(state, path, FORWARD_FLAG) {
            self.engine.send(WebViewCommand::GoForward);
        }
        if Self::take_flag(state, path, RELOAD_FLAG) {
            self.engine.send(WebViewCommand::Reload);
        }
        if Self::take_flag(state, path, SETTINGS_FLAG) {
            self.show_settings = !self.show_settings;
            self.structure_dirty = true;
            self.suggestions.clear();
        }

        self.apply_settings_intents(state, path);
        self.update_suggestions(state, path);

        if let Some(c) = &mut self.composed {
            c.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child_node = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);

        let style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        let node = taffy.new_with_children(style, &[child_node]).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node) {
                if let Some(&child_node) = children.first() {
                    c.apply_layout(taffy, child_node, Point::new(self.rect.x0, self.rect.y0));
                }
            }
        }
        // Anchor the suggestion dropdown flush under the address input.
        if let Some(r) = self.address_input_rect() {
            self.suggestions
                .set_anchor(Point::new(r.x0, r.y1 + 2.0), r.width() as f32);
        }
        self.publish_content_rect();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    /// The suggestion dropdown is a field, not a tree child, so paint it
    /// in the overlay pass here (above the page) after the composed tree's
    /// own overlays.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        // Re-anchor under the address bar every draw (layout may not have
        // re-run since the suggestions changed).
        if let Some(r) = self.address_input_rect() {
            self.suggestions
                .set_anchor(Point::new(r.x0, r.y1 + 2.0), r.width() as f32);
        }
        self.suggestions
            .draw_overlay(scene, transform, text_renderer, theme, presence);
    }

    fn take_structure_dirty(&mut self) -> bool {
        // Toggling the settings pane swaps the content widget → rebuild.
        std::mem::take(&mut self.structure_dirty)
            || self.composed.as_mut().is_some_and(|c| c.take_structure_dirty())
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        // The suggestion dropdown gets first crack at input while it's up
        // (address bar focused): arrow keys move the selection, Enter/click
        // pick a suggestion, Escape dismisses — ahead of the text field.
        if self.suggestions.is_visible() && !self.show_settings {
            let mut ov_handled = false;
            self.suggestions.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                &mut ov_handled,
                focus_target,
                node_id,
                taffy,
            );
            if let Some(entry) = self.suggestions.take_chosen() {
                self.suggestions.clear();
                self.last_query = None;
                ui::put_text_diffed(state, path, URL_PATH, &entry.url);
                self.engine
                    .send(WebViewCommand::Navigate(normalise_url(&entry.url)));
                *handled = true;
                return;
            }
            if ov_handled {
                *handled = true;
                return;
            }
        }

        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node_id) {
                if let Some(&child_node) = children.first() {
                    c.handle_event(
                        event,
                        scale_factor,
                        modifiers,
                        state,
                        path,
                        handled,
                        focus_target,
                        child_node,
                        taffy,
                    );
                }
            }
        }

        // Enter in the focused address bar submits it (no suggestion picked).
        if let WindowEvent::KeyboardInput { event: key, .. } = event {
            if key.state == ElementState::Pressed
                && key.logical_key == WKey::Named(WNamedKey::Enter)
                && self.url_input_focused()
            {
                self.navigate_to_input(state, path);
                self.suggestions.clear();
                self.last_query = None;
                *handled = true;
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &str {
        "BrowserWidget"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn test_id(&self) -> Option<&str> {
        Some("browser.root")
    }
}

impl AppWidget for BrowserWidget {
    fn app_kind(&self) -> AppKind {
        AppKind::Browser
    }
    fn app_title(&self) -> &str {
        if self.title.is_empty() {
            "Browser"
        } else {
            &self.title
        }
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("\u{F02AC}") // mdi-web (browser globe glyph)
    }
    fn permission_hint(&self) -> Option<AppKind> {
        Some(AppKind::Browser)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use ui::webview::WebViewEngine;

    fn synced(widget: &mut BrowserWidget) -> (AutoCommit, ObjId) {
        let mut state = AutoCommit::new();
        let root = automerge::ROOT;
        widget.sync(&mut state, &root);
        (state, root)
    }

    #[test]
    fn nav_gesture_drives_engine_history() {
        use ui::gesture::NavDirection;
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        let (mut state, root) = synced(&mut w);
        // Build the composed tree so the gesture forwards down to the
        // WebView — this exercises the real trait-forwarding path
        // (BrowserWidget → Column → WebView), not a shortcut.
        let mut tree = TaffyTree::<()>::new();
        let mut node_map = std::collections::HashMap::new();
        let theme = Theme::dark();
        w.build_layout(&mut tree, &theme, &mut node_map);

        engine.send(WebViewCommand::Navigate("https://a.test".into()));
        engine.send(WebViewCommand::Navigate("https://b.test".into()));

        assert!(
            w.on_nav_gesture(NavDirection::Back, &mut state, &root),
            "a back gesture is consumed by the embedded WebView",
        );
        assert_eq!(engine.current_uri().as_deref(), Some("https://a.test"));
        w.on_nav_gesture(NavDirection::Forward, &mut state, &root);
        assert_eq!(engine.current_uri().as_deref(), Some("https://b.test"));
    }

    #[test]
    fn bootstrap_seeds_address_bar() {
        let engine = TestEngine::new();
        // Headless so the test never spawns the network filter loader.
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        let (state, root) = synced(&mut w);
        assert_eq!(
            BrowserWidget::read_url(&state, &root).as_deref(),
            Some(DEFAULT_URL)
        );
    }

    #[test]
    fn button_flags_translate_to_engine_commands() {
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        let (mut state, root) = synced(&mut w);

        // History so back/forward are meaningful.
        engine.send(WebViewCommand::Navigate("https://a.test".into()));
        engine.send(WebViewCommand::Navigate("https://b.test".into()));

        let _ = state.put(&root, BACK_FLAG, true);
        w.sync(&mut state, &root);
        assert_eq!(engine.current_uri().as_deref(), Some("https://a.test"));
        assert!(
            !BrowserWidget::read_flag(&state, &root, BACK_FLAG),
            "flag resets after firing",
        );

        let _ = state.put(&root, FORWARD_FLAG, true);
        w.sync(&mut state, &root);
        assert_eq!(engine.current_uri().as_deref(), Some("https://b.test"));

        let _ = state.put(&root, RELOAD_FLAG, true);
        w.sync(&mut state, &root);
        assert!(
            engine.commands().contains(&WebViewCommand::Reload),
            "reload flag reaches the engine",
        );
    }

    #[test]
    fn engine_uri_updates_address_bar_text() {
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        let (mut state, root) = synced(&mut w);

        engine.send(WebViewCommand::Navigate("https://qos.example".into()));
        w.sync(&mut state, &root);

        assert_eq!(
            BrowserWidget::read_url(&state, &root).as_deref(),
            Some("https://qos.example")
        );
        assert_eq!(w.app_title(), "https://qos.example — test page");
    }

    #[test]
    fn history_events_drive_button_enablement() {
        let engine = TestEngine::new();
        let mut w = BrowserWidget::with_engine(engine.clone(), true);
        let (mut state, root) = synced(&mut w);

        engine.send(WebViewCommand::Navigate("https://a.test".into()));
        engine.send(WebViewCommand::Navigate("https://b.test".into()));
        w.sync(&mut state, &root);

        assert!(w.can_go_back);
        assert!(!w.can_go_forward);
    }

    #[test]
    fn headless_engine_has_deterministic_page() {
        let w = BrowserWidget::new_headless();
        let slot = w.engine.frames();
        let frame = slot.lock().unwrap().clone().expect("pre-rendered frame");
        assert!(frame.width > 0 && frame.height > 0);
    }

    #[test]
    fn default_engine_is_shareable() {
        // The engine handle must be cloneable/shareable between widget,
        // app, and blocker thread.
        let engine: SharedEngine = default_engine();
        let _clone: SharedEngine = Arc::clone(&engine);
    }
}
