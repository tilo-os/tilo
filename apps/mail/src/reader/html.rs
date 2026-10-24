//! The open message's HTML body: a sealed document ([`crate::html_mail`]) in
//! an isolated engine, laid out as tall as the document and as wide as the
//! reader, sharp at the display's scale.
//!
//! The engine runs in its own sandboxed process over a throwaway profile, in
//! the mode for sealed documents: its page never navigates, a link the
//! person follows comes back here to open in the Browser, and the address
//! under the pointer is reported so the reader can say where a link goes.
//! Before a document loads the engine's request filter refuses every web
//! request unless the person allowed this message's remote content; the
//! document's own policy and the sanitizer refuse them again.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::webview::{SharedEngine, WebViewCommand, WebViewEvent};
use ui::widget::{WebView, Widget};

use crate::html_mail::{InlinePart, Policy, Prepared};
use crate::model::MessageId;

/// The smallest the reader zooms a wide mail out to fit: the engine's
/// furthest zoom out (1.2⁻⁵).
const MIN_FIT: f64 = 0.401_877_572;

/// The engine for HTML mail: the packaged Chromium runtime when the image has
/// it, over a throwaway profile (no cookies or storage survive the process),
/// sealed; otherwise the deterministic test engine, whose frames are never
/// shown.
fn new_engine() -> (SharedEngine, bool) {
    #[cfg(feature = "live")]
    if let Some(engine) = native_engine() {
        return (engine, true);
    }
    let engine: SharedEngine = ui::webview::test_engine::TestEngine::new();
    (engine, false)
}

#[cfg(feature = "live")]
fn native_engine() -> Option<SharedEngine> {
    use std::sync::OnceLock;
    static PROFILE: OnceLock<Option<tempfile::TempDir>> = OnceLock::new();
    ui::webview::chromium::runtime::locate().ok()?;
    let profile = PROFILE
        .get_or_init(|| {
            tempfile::Builder::new()
                .prefix("tilo-mail-view")
                .tempdir()
                .ok()
        })
        .as_ref()?;
    match ui::webview::chromium::ChromiumEngine::sealed_documents(profile.path()) {
        Ok(engine) => Some(engine as SharedEngine),
        Err(error) => {
            tracing::warn!(%error, "mail: the HTML engine did not start");
            None
        }
    }
}

/// How a message's HTML is set: which message, with its remote content or
/// not, in a dark reader or a light one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Setting {
    pub message: MessageId,
    pub remote: bool,
    pub dark: bool,
}

/// The loaded document as its engine measured it, in the reader's logical
/// pixels, and the width of the view it was laid out in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Measured {
    pub height: f64,
    pub width: f64,
}

/// What the engine said since the last frame that the reader acts on.
#[derive(Debug, Default)]
pub(super) struct Said {
    /// Links the person followed, to open in the Browser.
    pub open: Vec<String>,
    /// The layout or the view changed: draw again.
    pub dirty: bool,
    /// Where the active find match is, top and bottom in the view's
    /// logical pixels: the reader scrolls it into sight.
    pub found_at: Option<(f64, f64)>,
}

/// How far a loaded document is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    /// The page before it is still shown.
    Loading,
    /// It replaced the page before it.
    Committed,
    /// It finished loading.
    Finished,
}

pub(super) struct HtmlBody {
    engine: Option<SharedEngine>,
    /// True only for a real isolated renderer; the deterministic test
    /// engine is never presented as message content.
    live: bool,
    view: Option<WebView>,
    /// The document last prepared, and how.
    prepared: Option<(Setting, Prepared)>,
    loaded: Option<Setting>,
    /// How far the loaded document is: the sizes the engine reports are
    /// of it once it replaced the one before, and of its content once it
    /// finished loading.
    stage: Stage,
    /// The last size the engine reported for the loaded document before it
    /// finished loading, in CSS pixels (width, height).
    early_size: Option<(f64, f64)>,
    measured: Option<Measured>,
    /// The view width the body is being measured at: it is laid out a
    /// sliver tall, and not drawn, until its engine says how tall it is.
    measuring: Option<f64>,
    /// The page zoom that fits the document to the view's width (1 when it
    /// fits as it is), and the width it was found for.
    zoom: f64,
    zoom_width: Option<f64>,
    /// The view's rect as last drawn.
    rect: Option<Rect>,
    /// The address of the link under the pointer.
    hovered: String,
    /// The page has the keyboard: the person pressed in it.
    focused: bool,
    next_request: u32,
    /// The copy waiting for the page's selection.
    copying: Option<u32>,
    /// The find the engine is answering, and what it last said: `(total,
    /// current)` with `current` 0-based.
    finding: Option<u32>,
    found: Option<(usize, Option<usize>)>,
}

/// Whether `frame` is the page's own: its document's end mark
/// ([`crate::html_mail::END_MARK`]) is in its bottom-right corner, one level
/// off what lies beside it on the same row. A frame the engine painted at a
/// new size before the page drew there has one colour across that row (the
/// page's background, which Chromium fills the new part with) or nothing.
fn is_page(frame: &ui::webview::Frame) -> bool {
    /// How far left of the corner the colour beside the mark is read, in
    /// pixels: past the mark at any device scale up to 8.
    const BESIDE: usize = 16;
    let (width, height) = (frame.width as usize, frame.height as usize);
    let row = height.saturating_sub(2) * width;
    let corner = width.saturating_sub(2);
    let pixel = |x: usize| frame.rgba.get((row + x) * 4..(row + x) * 4 + 4);
    match (pixel(corner), pixel(corner.saturating_sub(BESIDE))) {
        (Some(mark), Some(beside)) => {
            corner > BESIDE && mark.get(3).is_some_and(|alpha| *alpha != 0) && mark != beside
        }
        _ => false,
    }
}

/// Whether the engine's latest frame is the page's own at the size `view`
/// is shown at.
fn shows_page(view: &WebView) -> bool {
    if !view.has_fitting_frame() {
        return false;
    }
    let slot = view.engine().frames();
    let Ok(guard) = slot.lock() else {
        return false;
    };
    guard.as_ref().is_some_and(is_page)
}

impl HtmlBody {
    pub fn new() -> Self {
        Self {
            engine: None,
            live: false,
            view: None,
            prepared: None,
            loaded: None,
            stage: Stage::Loading,
            early_size: None,
            measured: None,
            measuring: None,
            zoom: 1.0,
            zoom_width: None,
            rect: None,
            hovered: String::new(),
            focused: false,
            next_request: 0,
            copying: None,
            finding: None,
            found: None,
        }
    }

    /// Start the engine, so the first HTML mail opens without waiting for
    /// it. Text-only use starts none.
    pub fn warm(&mut self) {
        if self.engine.is_none() {
            let (engine, live) = new_engine();
            self.engine = Some(engine);
            self.live = live;
        }
    }

    #[cfg(test)]
    pub fn set_engine(&mut self, engine: SharedEngine) {
        self.engine = Some(engine);
        self.live = true;
    }

    pub fn started(&self) -> bool {
        self.engine.is_some()
    }

    /// Whether HTML is shown in the engine (an isolated renderer runs).
    pub fn live(&self) -> bool {
        self.live
    }

    /// Whether the engine says how tall its document is: the body is then
    /// sized to it, and otherwise fills the reader.
    pub fn measures(&self) -> bool {
        self.engine
            .as_ref()
            .is_some_and(|engine| engine.measures_document())
    }

    pub fn loaded(&self) -> Option<Setting> {
        self.loaded
    }

    pub fn rect(&self) -> Option<Rect> {
        self.rect
    }

    pub fn hovered(&self) -> Option<&str> {
        (!self.hovered.is_empty()).then_some(self.hovered.as_str())
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// The document for `setting`, prepared once for it.
    pub fn prepare(
        &mut self,
        setting: Setting,
        html: &str,
        theme: &Theme,
        inline: &[InlinePart],
    ) -> &Prepared {
        let fresh = self
            .prepared
            .as_ref()
            .is_some_and(|(prepared, _)| *prepared == setting);
        if !fresh {
            let colors = &theme.tokens.colors;
            let ground = ui::color::to_hex(colors.surfaces.content);
            let ink = ui::color::to_hex(colors.text_primary);
            let prepared = crate::html_mail::prepare(
                html,
                &Policy {
                    remote: setting.remote,
                    dark: setting.dark,
                    ground: &ground,
                    ink: &ink,
                    inline,
                },
            );
            self.prepared = Some((setting, prepared));
        }
        let (_, prepared) = self
            .prepared
            .get_or_insert_with(|| (setting, Prepared::default()));
        prepared
    }

    /// The document prepared last, if it is for `setting`.
    pub fn prepared(&self, setting: Setting) -> Option<&Prepared> {
        self.prepared
            .as_ref()
            .filter(|(prepared, _)| *prepared == setting)
            .map(|(_, prepared)| prepared)
    }

    fn view(&mut self) -> Option<&mut WebView> {
        let engine = self.engine.clone()?;
        Some(self.view.get_or_insert_with(|| {
            // The body is sized to its document, so the reader scrolls as
            // one page.
            engine.send(WebViewCommand::ReportDocumentSize(true));
            let mut view = WebView::new(engine).with_test_id("mail_html_body");
            // It draws the page's own frames only: never the blank one the
            // engine paints a grown view with first.
            view.accept_frames(is_page);
            view
        }))
    }

    /// Load the prepared document for `setting`, unless it is loaded.
    /// The engine's request rules come first, so nothing the document
    /// names is fetched under the rules of the one before it. Returns
    /// whether the document can be shown: one too large for the engine is
    /// not.
    pub fn load(&mut self, setting: Setting) -> bool {
        if self.loaded == Some(setting) {
            return true;
        }
        let Some(url) = self
            .prepared(setting)
            .and_then(|prepared| crate::html_mail::data_url(&prepared.document))
        else {
            return false;
        };
        let same_message = self
            .loaded
            .is_some_and(|loaded| loaded.message == setting.message);
        let Some(view) = self.view() else {
            return false;
        };
        // What the view shows until this mail paints is never the mail
        // before it; the same mail, loaded again with its images, stays up
        // until it paints again.
        if !same_message {
            view.forget_frame();
        }
        let engine = view.engine().clone();
        engine.send(WebViewCommand::SetNativeFilterRules {
            rules: std::sync::Arc::new(crate::html_mail::filter_rules(setting.remote)),
            enabled: Some(true),
        });
        engine.send(WebViewCommand::Navigate(url));
        // Each mail is set at its own size.
        engine.send(WebViewCommand::SetZoomLevel(0.0));
        self.zoom = 1.0;
        self.zoom_width = None;
        if !same_message {
            self.measured = None;
        }
        self.stage = Stage::Loading;
        self.early_size = None;
        self.loaded = Some(setting);
        self.hovered.clear();
        self.finding = None;
        true
    }

    /// Nothing HTML is shown: forget what was loaded, so returning to an
    /// HTML mail loads it again, blocked again.
    pub fn unload(&mut self) {
        self.loaded = None;
        self.rect = None;
        self.focused = false;
        self.hovered.clear();
    }

    /// The height to lay the body out at in a view `width` wide, and the
    /// room to keep for it: its document's whole height, so the reader
    /// scrolls the conversation and the mail as one page; a sliver until
    /// that is known, keeping the room it had at another width.
    pub fn layout(&mut self, width: f64, sliver: f64) -> (f64, f64) {
        if let Some(doc) = self
            .measured
            .filter(|doc| (doc.width - width).abs() < 0.5 && self.measuring.is_none())
        {
            let height = doc.height.ceil().max(1.0);
            return (height, height);
        }
        if self
            .zoom_width
            .is_some_and(|fitted| (fitted - width).abs() >= 0.5)
        {
            // A new width: measure the mail at its own size again.
            self.zoom = 1.0;
            self.zoom_width = None;
            if let Some(engine) = &self.engine {
                engine.send(WebViewCommand::SetZoomLevel(0.0));
            }
        }
        self.measuring = Some(width);
        let kept = self
            .measured
            .map_or(sliver, |doc| doc.height.ceil().max(1.0));
        (sliver, kept)
    }

    /// Place the view at `rect`, for the next [`Self::draw`].
    pub fn place(&mut self, rect: Rect) {
        if let Some(view) = self.view() {
            view.place(rect);
        }
    }

    /// Once measured, draw the view where it was placed, inside `clip`: a
    /// light document on a sheet of its own with rounded corners.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        clip: Rect,
    ) {
        let measuring = self.measuring.is_some();
        let sheet = self
            .loaded
            .is_some_and(|setting| !self.prepared(setting).is_some_and(|prepared| prepared.dark));
        self.rect = None;
        let Some(view) = self.view.as_mut() else {
            return;
        };
        let rect = view.get_rect();
        // Hidden until it has painted, it still paints at the scale it is
        // shown at: a frame at another scale is not the page on screen.
        view.lands_at(t);
        if measuring {
            return;
        }
        // Grown to its document's height, the view has nothing of the page
        // until the engine paints at that size: the engine paints the new
        // size first with the page's old frame and a blank rest. The
        // message's body appears with the page's own frame at its size;
        // later resizes keep showing the last one until the next arrives.
        if !view.refresh_frame() {
            return;
        }
        // Scrolled partly out of the reader, it is cut at its edges like
        // the rest of the conversation.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &clip);
        if sheet {
            ui::widget::primitives::paint_document_sheet(scene, t, rect, theme, |scene| {
                view.draw(scene, t, tr, theme, presence);
            });
        } else {
            view.draw(scene, t, tr, theme, presence);
        }
        scene.pop_layer();
        self.rect = Some(rect);
    }

    /// Act on what the engine said: a followed link, the document's size, the
    /// link under the pointer, a selection to copy, where find landed. A
    /// mail never navigates its own view.
    pub fn drain(&mut self) -> Said {
        let mut said = Said::default();
        let Some(engine) = self.view.as_ref().map(|view| view.engine().clone()) else {
            return said;
        };
        for (url, _) in engine.take_new_tab_requests() {
            if is_openable(&url) && said.open.len() < 8 {
                said.open.push(url);
            }
        }
        for event in engine.drain_events() {
            match event {
                // Before its content is laid out a page has no size; that is
                // not a measurement.
                WebViewEvent::DocumentSize { width, height, .. }
                    if width <= 0.0 || height <= 0.0 => {}
                WebViewEvent::UriChanged(uri) if uri.starts_with("data:") => {
                    self.stage = self.stage.max(Stage::Committed);
                }
                WebViewEvent::UriChanged(uri) if !uri.is_empty() && uri != "about:blank" => {
                    // An engine that cannot seal its page left the mail:
                    // show the mail again. Nothing opens without a click.
                    self.loaded = None;
                    said.dirty = true;
                }
                WebViewEvent::LoadFinishedAt { .. } | WebViewEvent::LoadFinished
                    if self.stage == Stage::Committed =>
                {
                    self.stage = Stage::Finished;
                    if let Some((width, height)) = self.early_size.take() {
                        said.dirty |= self.document_size(width, height);
                    }
                }
                WebViewEvent::DocumentSize { width, height, .. }
                    if self.stage == Stage::Finished =>
                {
                    said.dirty |= self.document_size(width, height);
                }
                WebViewEvent::DocumentSize { width, height, .. }
                    if self.stage == Stage::Committed =>
                {
                    // Measured before its content is laid out, a page is as
                    // tall as the sliver it is shown in.
                    self.early_size = Some((width, height));
                }
                WebViewEvent::FindSelection { id, y, height, .. } if self.finding == Some(id) => {
                    said.found_at = Some((y, y + height));
                }
                WebViewEvent::LinkHovered(url) => {
                    if url != self.hovered {
                        self.hovered = url;
                        said.dirty = true;
                    }
                }
                WebViewEvent::SelectionText { request_id, text }
                    if self.copying == Some(request_id) =>
                {
                    self.copying = None;
                    if !text.is_empty() {
                        ui::clipboard_copy(&text);
                    }
                }
                WebViewEvent::FindResult {
                    id, count, active, ..
                } if self.finding == Some(id) => {
                    let total = usize::try_from(count).unwrap_or(0);
                    let current = usize::try_from(active)
                        .ok()
                        .and_then(|active| active.checked_sub(1));
                    self.found = Some((total, current));
                    said.dirty = true;
                }
                _ => {}
            }
        }
        said
    }

    /// The engine measured its document, in CSS pixels. A document wider
    /// than the view is zoomed out to fit it, so a mail laid out for a
    /// wider window reads without scrolling sideways. Returns whether the
    /// reader's layout changes.
    fn document_size(&mut self, css_width: f64, css_height: f64) -> bool {
        let Some(width) = self
            .measuring
            .or_else(|| self.measured.map(|doc| doc.width))
        else {
            return false;
        };
        let view_css = width / self.zoom;
        if css_width > view_css + 0.5 {
            let fit = (width / css_width).clamp(MIN_FIT, 1.0);
            if (fit - self.zoom).abs() > 0.001 {
                self.zoom = fit;
                if let Some(engine) = &self.engine {
                    engine.send(WebViewCommand::SetZoomLevel(fit.ln() / 1.2_f64.ln()));
                }
            }
        }
        self.zoom_width = Some(width);
        let next = Measured {
            height: css_height * self.zoom,
            width,
        };
        let changed = self.measuring.is_some()
            || self.measured.is_none_or(|doc| {
                (doc.height - next.height).abs() > 0.5 || (doc.width - next.width).abs() > 0.5
            });
        self.measured = Some(next);
        self.measuring = None;
        changed
    }

    /// Give the page a pointer or key event: moving over it, pressing and
    /// dragging to select, the wheel when `wheel` says the body scrolls,
    /// and keys while it has them. A right press is the reader's (its
    /// menu); Ctrl+C copies the page's selection to the system clipboard.
    pub fn forward(
        &mut self,
        event: &WindowEvent,
        pointer: Point,
        wheel: bool,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        let over = self.rect.is_some_and(|rect| rect.contains(pointer));
        match event {
            WindowEvent::CursorMoved { .. } => {
                if !over && !self.hovered.is_empty() {
                    self.hovered.clear();
                }
                if over || self.focused {
                    self.forward_raw(event, ctx);
                }
            }
            WindowEvent::MouseInput { button, state, .. } => {
                let pressed = *state == ElementState::Pressed;
                if pressed && *button == MouseButton::Left {
                    self.set_focus(over);
                }
                if matches!(button, MouseButton::Left | MouseButton::Middle)
                    && (over || (!pressed && self.focused))
                {
                    self.forward_raw(event, ctx);
                }
            }
            WindowEvent::MouseWheel { .. } if wheel && over => self.forward_raw(event, ctx),
            WindowEvent::KeyboardInput { event: key, .. } if self.focused => {
                let command = {
                    let state = ctx.modifiers.state();
                    state.control_key() || state.super_key()
                };
                let copy = command
                    && matches!(&key.logical_key, winit::keyboard::Key::Character(c) if c.eq_ignore_ascii_case("c"));
                if copy {
                    if key.state == ElementState::Pressed {
                        self.copy();
                    }
                    *ctx.handled = true;
                } else if command
                    && matches!(&key.logical_key, winit::keyboard::Key::Character(c) if c.eq_ignore_ascii_case("f"))
                {
                    // Find is the window's.
                } else {
                    self.forward_raw(event, ctx);
                }
            }
            _ => {}
        }
    }

    fn forward_raw(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(view) = self.view.as_mut() {
            view.handle_event(event, ctx);
        }
    }

    pub fn set_focus(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            if let Some(view) = self.view.as_mut() {
                view.set_focus(focused);
            }
        }
    }

    /// Copy what the page has selected to the system clipboard, once the
    /// engine says what it is.
    pub fn copy(&mut self) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let request = self.request();
        self.copying = Some(request);
        engine.send(WebViewCommand::CopySelection {
            request_id: request,
        });
    }

    /// Select the whole document.
    pub fn select_all(&self) {
        if let Some(engine) = &self.engine {
            for down in [true, false] {
                engine.send(WebViewCommand::Key {
                    key: ui::webview::KeyInput::Character("a".into()),
                    down,
                    modifiers: ui::webview::KeyModifiers::command(
                        ui::platform::PlatformConventions::default(),
                    ),
                });
            }
        }
    }

    fn request(&mut self) -> u32 {
        self.next_request = self.next_request % (i32::MAX as u32 - 1) + 1;
        self.next_request
    }

    /// Find `query` in the page; the engine says where.
    pub fn find(&mut self, query: &str) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        self.found = None;
        if query.is_empty() {
            self.finding = None;
            engine.send(WebViewCommand::StopFinding);
            return;
        }
        let id = self.request();
        self.finding = Some(id);
        engine.send(WebViewCommand::Find {
            id,
            text: query.to_owned(),
            forward: true,
            next: false,
        });
    }

    /// Step to the next or previous match of the find running.
    pub fn step(&self, query: &str, forward: bool) {
        if let (Some(engine), Some(id)) = (&self.engine, self.finding) {
            engine.send(WebViewCommand::Find {
                id,
                text: query.to_owned(),
                forward,
                next: true,
            });
        }
    }

    pub fn take_found(&mut self) -> Option<(usize, Option<usize>)> {
        self.found.take()
    }

    pub fn needs_animation_frame(&self) -> bool {
        self.view
            .as_ref()
            .is_some_and(Widget::needs_animation_frame)
    }

    /// The view draws a frame of the page's own: what it shows beyond its
    /// placeholder.
    #[cfg(test)]
    pub fn draws_page(&mut self) -> bool {
        self.view.as_mut().is_some_and(WebView::refresh_frame)
    }

    /// The loaded document is not on screen yet.
    pub fn pending(&self) -> bool {
        self.live
            && self.loaded.is_some()
            && (self.measuring.is_some()
                || self.stage != Stage::Finished
                || !self.view.as_ref().is_some_and(|view| {
                    // Placed at the height it was measured at, and painted there.
                    let placed = view.get_rect().height();
                    self.measured
                        .is_none_or(|doc| (placed - doc.height.ceil().max(1.0)).abs() < 0.5)
                        && shows_page(view)
                }))
    }

    pub fn mark_drawn(&mut self) {
        if let Some(view) = self.view.as_mut() {
            view.mark_drawn();
        }
    }
}

/// A link a mail may open: a web or a mail address.
pub(crate) fn is_openable(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    ["https://", "http://", "mailto:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
        && url.len() <= 8192
}

#[cfg(test)]
mod tests {
    use super::is_page;
    use std::sync::Arc;
    use ui::webview::Frame;

    fn frame(width: u32, height: u32, fill: [u8; 4], mark: Option<[u8; 4]>) -> Frame {
        let mut rgba = fill.repeat(width as usize * height as usize);
        if let Some(mark) = mark {
            for y in height - 2..height {
                for x in width - 2..width {
                    let at = (y * width + x) as usize * 4;
                    rgba[at..at + 4].copy_from_slice(&mark);
                }
            }
        }
        Frame {
            trace_id: None,
            damage: None,
            rgba: Arc::new(rgba),
            width,
            height,
        }
    }

    /// Chromium paints a grown view before the page has drawn there in the
    /// page's own background colour: on a dark mail that is the reader's
    /// dark ground, not white. Only the end mark, one level off its row,
    /// tells the page's frame from it, on a light page and a dark one.
    #[test]
    fn the_page_is_told_by_its_end_mark_on_any_background() {
        for ground in [[0xff_u8, 0xff, 0xff, 0xff], [18, 20, 23, 0xff]] {
            let mark = ground.map(|c| c.abs_diff(1));
            let mark = [mark[0], mark[1], mark[2], 0xff];
            assert!(is_page(&frame(64, 32, ground, Some(mark))), "{ground:?}");
            assert!(!is_page(&frame(64, 32, ground, None)), "{ground:?}");
        }
        assert!(!is_page(&frame(64, 32, [0; 4], None)), "nothing painted");
    }
}
