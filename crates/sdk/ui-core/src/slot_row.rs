//! The app's window-bar slots in a row of the window's own, outside a Tilo
//! session.
//!
//! An app declares its toolbar once, as slots in the window's bar
//! ([`crate::window_toolbar`]); in a Tilo session the desktop draws them in
//! the one bar it draws per window. Run anywhere else (another desktop, a
//! window of its own, a browser page) nothing hosts them, so the host draws
//! them itself, with the same [`ToolbarRow`] the desktop's bar uses, in one
//! row across the top of the window under whatever frame the host gives it.
//! The app draws no toolbar of its own either way, and hears what the person
//! did in the row exactly as it hears the desktop's bar
//! ([`crate::window_toolbar::take_events`]).
//!
//! The window's History is one more slot of the row, in its menu, as the
//! desktop's bar carries it beside the app's: the app's History draws no
//! band of its own above an app whose controls are its slots.
//!
//! The row ends with the window's search, the same [`WindowSearch`] the
//! desktop's bar ends with: the one search entry a window has, in a session
//! or not. The row hosts the app's slots ([`crate::window_toolbar::is_hosted`])
//! and presents its search, so the app draws neither a toolbar nor a search
//! field of its own here either.
//!
//! [`SlotRow`] is that row for every host: the native runner's window of its
//! own and the browser's page that mounts one app
//! ([`crate::headless::FrameHarness::host_window_slots`]). Each host lends it
//! the app it serves through [`SlotRowHost`].

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::search::SearchRequest;
use crate::tokens::MotionSource;
use crate::widget::tabs::TOOLBAR_ROW_HEIGHT;
use crate::widget::window_controls::{CONTROL_GAP, CONTROL_PX};
use crate::widget::window_toolbar_row::ToolbarRow;
use crate::widget::{HistoryControl, InPlace, Widget, WindowSearch, WindowSearchRequest};
use crate::window_toolbar::{SearchOpening, ToolbarAction, ToolbarEvent, WindowToolbar};

/// The row's one "tab": the window itself.
const WINDOW: u64 = 0;

/// The window's History slot, the row's own: a press runs the History door
/// and never reaches the app.
pub(crate) const HISTORY: &str = "tilo.window.history";

/// The app's slots with the window's History added to the row's menu.
pub(crate) fn with_history(
    mut toolbar: WindowToolbar,
    history: Option<HistoryControl>,
) -> WindowToolbar {
    if let Some(control) = history {
        let name = if control.is_open() {
            "host-history-hide"
        } else {
            "standalone-history-title"
        };
        toolbar.actions.push(
            ToolbarAction::new(
                HISTORY,
                "history",
                crate::i18n::I18nManager::lookup_plain(name),
            )
            .on(control.is_open())
            .in_menu(),
        );
    }
    toolbar
}

/// The app a [`SlotRow`] serves, as its host lends it.
pub(crate) trait SlotRowHost {
    /// The app's widgets, where the window's search finds.
    fn search_root(&mut self) -> &mut dyn Widget;
    /// What the app offers the window's search in place.
    fn search_offer(&mut self) -> InPlace;
    /// Open the app's own search of everything it publishes, for an app
    /// with nothing to search in place.
    fn open_app_search(&mut self, request: SearchRequest);
    /// The search changed what the app shows: it lays out and is read
    /// again.
    fn search_drove(&mut self);
    /// Whether the window's search, rather than the app, takes the Find (or
    /// Find and Replace) chord now.
    fn takes_search(&mut self, opening: SearchOpening) -> bool;
    /// The window's History control, none while the app has no History.
    fn history_control(&self) -> Option<HistoryControl>;
    /// Run the window's History control on the app's History door.
    fn press_history(&mut self, control: HistoryControl);
    /// Hand the app what the person did in its slots, as the desktop's bar
    /// hands it over.
    fn deliver(&mut self, event: ToolbarEvent);
    /// The slots ran something: the app syncs and lays out again.
    fn slots_acted(&mut self);
}

/// Where the row stands: across the top of a window `window` (logical) in
/// size, inside the host's frame `left`, `top` and `right`, for the app
/// `app_name` (the open search names it).
pub(crate) struct RowPlace<'a> {
    pub(crate) left: f64,
    pub(crate) top: f64,
    pub(crate) right: f64,
    pub(crate) window: (f64, f64),
    pub(crate) app_name: &'a str,
}

/// What an event is read with: the window's scale, the keyboard's
/// modifiers and the platform's conventions.
pub(crate) struct RowInput<'a> {
    pub(crate) scale: f64,
    pub(crate) modifiers: &'a winit::event::Modifiers,
    pub(crate) platform: crate::platform::PlatformConventions,
}

pub(crate) struct SlotRow {
    row: ToolbarRow,
    /// What the row shows (none: hidden), and where it stands.
    shown: Option<(WindowToolbar, Rect)>,
    /// A one-node tree, for the events the row hands its field and menus.
    tree: TaffyTree<()>,
    node: Option<NodeId>,
    /// The document the row's own fields are lent: they keep their text
    /// themselves and write nothing of the app's.
    scratch: automerge::AutoCommit,
    pointer: Point,
    /// The row changed since it was last painted. It reports no widget
    /// damage, so the frame after repaints the whole window.
    repaint: bool,
    /// The window's search, ending the row.
    search: WindowSearch,
    /// The window is narrower than the compact breakpoint: a notice has a
    /// strip of its own under the row.
    compact: bool,
}

impl SlotRow {
    pub(crate) fn new() -> Self {
        let mut tree = TaffyTree::new();
        let node = tree.new_leaf(taffy::Style::default()).ok();
        Self {
            row: ToolbarRow::new(),
            shown: None,
            tree,
            node,
            scratch: automerge::AutoCommit::new(),
            pointer: Point::ORIGIN,
            repaint: false,
            search: WindowSearch::new(),
            compact: false,
        }
    }

    /// The row must be painted again in full.
    pub(crate) fn mark_repaint(&mut self) {
        self.repaint = true;
    }

    /// Whether the row shows.
    pub(crate) fn is_shown(&self) -> bool {
        self.shown.is_some()
    }

    /// Keep the row current: shown at `place` (none: hidden), with the
    /// app's slots, its notice and the window's `history` control. Call
    /// every frame before layout, then [`Self::drive_search`]. Returns the
    /// row's height, with a compact window's notice strip: the app's frame
    /// starts that far below `place.top`.
    pub(crate) fn refresh(
        &mut self,
        theme: &crate::theme::Theme,
        place: Option<RowPlace<'_>>,
        history: Option<HistoryControl>,
    ) -> f64 {
        let Some(place) = place else {
            if self.shown.take().is_some() {
                self.row
                    .show(WINDOW, None, theme.motion(), crate::motion::frame_now());
                // Closed, the search lets go of what it found in the app
                // when it is next driven.
                self.search.close(MotionSource::Pointer);
                self.search.place(Rect::ZERO, 0.0);
                self.repaint = true;
            }
            return 0.0;
        };
        let toolbar = with_history(crate::window_toolbar::shown().unwrap_or_default(), history);
        let (width, window_height) = place.window;
        let inset = f64::from(theme.tokens.spacing.s2);
        let rect = Rect::new(
            place.left + inset,
            place.top,
            (width - place.right - inset).max(place.left + inset),
            place.top + TOOLBAR_ROW_HEIGHT,
        );
        // A compact window says a notice in a strip of its own under the
        // row, as the desktop's bar does.
        let compact = width < f64::from(theme.tokens.metrics.compact_breakpoint);
        let restyled = self.compact != compact;
        self.compact = compact;
        if restyled || self.shown.as_ref() != Some(&(toolbar.clone(), rect)) {
            self.row.set_compact(compact);
            self.row.show(
                WINDOW,
                Some(&toolbar),
                theme.motion(),
                crate::motion::frame_now(),
            );
            self.row
                .set_popup_area(Rect::new(0.0, 0.0, width, window_height));
            let row = search_room(rect);
            self.row.lay_out(row);
            self.place_search(rect, row);
            self.shown = Some((toolbar, rect));
            self.repaint = true;
        }
        self.repaint |= self.row.measure_strip(theme, rect.width());
        let strip = self.row.strip_height();
        self.row.place_strip(if strip > 0.0 {
            Rect::new(rect.x0, rect.y1, rect.x1, rect.y1 + strip)
        } else {
            Rect::ZERO
        });
        self.search.set_app_name(place.app_name);
        self.repaint |= self.row.measure_views(theme);
        TOOLBAR_ROW_HEIGHT + strip
    }

    /// Place the search control at the end of the row: open, it reaches
    /// back over the slots as far as the location, as the desktop's does.
    fn place_search(&mut self, rect: Rect, row: Rect) {
        let x0 = row.x1 + CONTROL_GAP;
        let y0 = rect.center().y - CONTROL_PX / 2.0;
        let location = self.row.slots().location;
        let reach = if location.area() > 0.0 {
            location.x0
        } else {
            row.x0
        };
        self.search.place_trailing(
            Rect::new(x0, y0, x0 + CONTROL_PX, y0 + CONTROL_PX),
            reach.min(x0),
            row.x0.min(x0),
        );
    }

    /// What the app asked of its bar: its Find (open the search), its Ctrl+L
    /// (edit the location).
    pub(crate) fn serve_app_requests(&mut self, host: &mut dyn SlotRowHost) {
        if let Some(opening) = crate::window_toolbar::take_search_request() {
            self.open_search(host, MotionSource::Keyboard, opening);
        }
        if crate::window_toolbar::take_edit_request() && self.row.begin_edit() {
            self.repaint = true;
        }
    }

    /// Open the search in place on what the app offers, or give the open
    /// one the keyboard (for the replace chord, the replacement). An app
    /// with nothing to search in place gets its own search of everything it
    /// publishes, as the desktop's bar hands it to the platform search.
    pub(crate) fn open_search(
        &mut self,
        host: &mut dyn SlotRowHost,
        source: MotionSource,
        opening: SearchOpening,
    ) {
        if self.search.is_open() {
            match opening {
                SearchOpening::Find => self.search.take_keyboard(),
                SearchOpening::Replace => self.search.focus_replacement(),
            }
        } else {
            let in_place = host.search_offer();
            if in_place == InPlace::Nothing {
                host.open_app_search(SearchRequest::this_app());
            } else {
                self.search.open_as(in_place, source, opening);
                // The document says what it offers as the search reaches it.
                self.drive_search(host);
            }
        }
        self.repaint = true;
    }

    /// Carry out what the person asked of the search.
    fn serve_search(&mut self, host: &mut dyn SlotRowHost) -> bool {
        let requests = self.search.take_requests();
        let acted = !requests.is_empty();
        for request in requests {
            match request {
                WindowSearchRequest::Open => {
                    self.open_search(host, MotionSource::Pointer, SearchOpening::Find);
                }
                WindowSearchRequest::Close => self.search.close(crate::motion::input_source()),
                WindowSearchRequest::Widen(_) => {
                    // No platform search outside a session: the app's own
                    // search of everything it publishes takes the query on.
                    let query = self.search.query().to_owned();
                    self.search.close(crate::motion::input_source());
                    host.open_app_search(SearchRequest::this_app().with_query(query));
                }
            }
        }
        acted
    }

    /// Carry the search to the app's widgets, once a frame: what it found
    /// shows in the frame being built.
    pub(crate) fn drive_search(&mut self, host: &mut dyn SlotRowHost) {
        if self.search.wants_drive() && self.search.drive(host.search_root()) {
            host.search_drove();
            self.repaint = true;
        }
    }

    /// The input evidence of the window's search, beside the app's own:
    /// what the field holds and where the control stands, while the row
    /// shows.
    pub(crate) fn with_search_evidence(
        &self,
        snapshot: Option<serde_json::Value>,
    ) -> Option<serde_json::Value> {
        let mut snapshot = snapshot?;
        if self.shown.is_some()
            && let Some(fields) = snapshot.as_object_mut()
        {
            let anchor = self.search.anchor();
            fields.insert(
                "window_search".into(),
                serde_json::json!({
                    "open": self.search.is_open(),
                    "focused": self.search.has_keyboard(),
                    "text": self.search.query(),
                    "caret_byte": self.search.caret_byte(),
                    "rect": [anchor.x0, anchor.y0, anchor.x1, anchor.y1],
                }),
            );
        }
        Some(snapshot)
    }

    /// Whether the row changed since the last frame, which must then
    /// repaint the whole window rather than only the app's own damage.
    pub(crate) fn take_repaint(&mut self) -> bool {
        std::mem::take(&mut self.repaint)
    }

    /// Whether the row needs another frame: a menu arriving, the field's
    /// caret.
    pub(crate) fn needs_frame(&self) -> bool {
        self.shown.is_some()
            && (self.row.needs_animation_frame(crate::time::now())
                || self.search.needs_animation_frame())
    }

    /// The band the row paints, from the host's frame at `left` to the
    /// window's end, with a compact window's notice strip (zero: hidden).
    pub(crate) fn band(&self, left: f64, theme: &crate::theme::Theme) -> Rect {
        let Some((_, rect)) = self.shown.as_ref() else {
            return Rect::ZERO;
        };
        Rect::new(
            left,
            rect.y0,
            rect.x1 + f64::from(theme.tokens.spacing.s2),
            rect.y1 + self.row.strip_height(),
        )
    }

    /// What the row can paint, inside `window`: its band, focus rings
    /// and antialiasing included, while it rests with no menu open, its
    /// location not edited and the search closed; else all of `window`,
    /// which its menus, suggestions and the search's options may cover.
    pub(crate) fn paint_bounds(
        &self,
        left: f64,
        theme: &crate::theme::Theme,
        window: Rect,
    ) -> Rect {
        if self.row.popup_open()
            || self.row.is_editing()
            || self.search.is_open()
            || self.needs_frame()
        {
            return window;
        }
        self.band(left, theme).inflate(4.0, 4.0).intersect(window)
    }

    /// Paint the row from the host's frame at `left`: its band, its slots
    /// and the search, then their menus and the search's options over the
    /// app.
    pub(crate) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut crate::text_render::TextRenderer,
        theme: &crate::theme::Theme,
        presence: &crate::presence::PresenceCtx,
        left: f64,
    ) {
        if self.shown.is_none() {
            return;
        }
        let band = self.band(left, theme);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.content,
            None,
            &band,
        );
        self.row.paint(scene, transform, text, theme, presence);
        self.search.set_ground(theme.tokens.colors.surfaces.content);
        self.search.draw(scene, transform, text, theme, presence);
        self.row
            .paint_popups(scene, transform, text, theme, presence);
        // The search's options panel, over the app.
        self.search
            .draw_overlay(scene, transform, text, theme, presence);
    }

    /// Hand `event` to the row when it is the row's: a pointer over a slot,
    /// over one of its menus, or anything while its location is edited.
    /// Returns whether the row took it, and whether the window must draw
    /// again; what the person did reaches the app as the desktop's bar
    /// delivers it.
    pub(crate) fn route(
        &mut self,
        host: &mut dyn SlotRowHost,
        event: &WindowEvent,
        input: &RowInput<'_>,
    ) -> (bool, bool) {
        let (Some((_, rect)), Some(node)) = (self.shown.as_ref(), self.node) else {
            return (false, false);
        };
        let rect = *rect;
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Point::new(position.x / input.scale, position.y / input.scale);
        }
        let over = rect.contains(self.pointer) || self.row.strip_rect().contains(self.pointer);
        let mut handled = false;
        let mut focus = None;
        // Find (and Find and Replace) opens the window's search, or gives
        // the open one the keyboard, before the app sees the chord, as the
        // desktop's bar does.
        if let Some(opening) = crate::widget::window_search::search_chord(
            event,
            input.modifiers.state(),
            input.platform,
        ) && !self.search.has_keyboard()
            && host.takes_search(opening)
        {
            self.open_search(host, MotionSource::Keyboard, opening);
            return (true, true);
        }
        let root = automerge::ROOT;
        let searched = {
            let mut ctx = crate::widget::EventCtx {
                scale_factor: input.scale,
                modifiers: input.modifiers,
                state: crate::widget::Edits::lend(&mut self.scratch),
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &self.tree,
            };
            self.search.route(event, &mut ctx)
        };
        let mut redraw = false;
        if self.serve_search(host) || searched {
            self.repaint = true;
            redraw = true;
        }
        if searched {
            return (true, redraw);
        }
        let mut ctx = crate::widget::EventCtx {
            scale_factor: input.scale,
            modifiers: input.modifiers,
            state: crate::widget::Edits::lend(&mut self.scratch),
            path: &root,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: node,
            taffy: &self.tree,
        };
        let took = if (self.row.popup_open() && self.row.popup_event(event, &mut ctx))
            || (self.row.is_editing()
                && (!matches!(
                    event,
                    WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. }
                ) || self.row.location_hit(self.pointer))
                && self.row.field_event(event, &mut ctx))
        {
            true
        } else {
            match event {
                WindowEvent::CursorMoved { .. } if over => {
                    self.row.pointer_moved(self.pointer);
                    true
                }
                WindowEvent::CursorMoved { .. } | WindowEvent::CursorLeft { .. } => {
                    self.repaint |= self.row.pointer_left();
                    false
                }
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                } => self.row.press(self.pointer),
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    button: MouseButton::Left,
                    ..
                } => self.row.release(self.pointer),
                _ => false,
            }
        };
        let acted = self.deliver_row_events(host);
        if took || acted {
            self.repaint = true;
            redraw = true;
        }
        (took, redraw)
    }

    /// Hand the app what was done in the row since the last call; the
    /// window's History runs on its door. Returns whether anything was.
    fn deliver_row_events(&mut self, host: &mut dyn SlotRowHost) -> bool {
        let events = self.row.take_events();
        let drafts = self.row.take_drafts();
        let acted = !events.is_empty() || !drafts.is_empty();
        for (_, event) in events.into_iter().chain(drafts) {
            match event {
                ToolbarEvent::Action { id } if id == HISTORY => {
                    if let Some(control) = host.history_control() {
                        host.press_history(control);
                    }
                }
                event => host.deliver(event),
            }
        }
        if acted {
            host.slots_acted();
        }
        acted
    }

    /// The row as assistive technology and automation reach it: the app's
    /// slots, the window's History in the row's menu, and the search.
    pub(crate) fn accessibility(&self) -> Vec<crate::a11y::AccessNode> {
        if self.shown.is_none() {
            return Vec::new();
        }
        let mut nodes = self.row.accessibility();
        nodes.extend(self.search.accessibility());
        nodes
    }

    /// Perform `action` on the part of the row `target` names (a
    /// `window-toolbar-*` slot or a `window-search-*` part), as assistive
    /// technology and automation do; what it runs reaches the app as a
    /// press does. Returns whether the row took it.
    pub(crate) fn perform(
        &mut self,
        host: &mut dyn SlotRowHost,
        target: &str,
        action: &crate::a11y::Action,
    ) -> bool {
        if self.shown.is_none() {
            return false;
        }
        if target.starts_with("window-search-") {
            let root = automerge::ROOT;
            let handled = self.search.perform_semantic_action(
                target,
                action,
                &mut crate::widget::Edits::lend(&mut self.scratch),
                &root,
            );
            let served = self.serve_search(host);
            if handled || served {
                self.drive_search(host);
                self.repaint = true;
            }
            return handled;
        }
        if !matches!(action, crate::a11y::Action::Activate) || !self.row.perform(target) {
            return false;
        }
        self.deliver_row_events(host);
        self.repaint = true;
        true
    }

    /// The row's search, for a test to drive.
    #[cfg(test)]
    pub(crate) fn search_mut(&mut self) -> &mut WindowSearch {
        &mut self.search
    }

    /// Lay the row out across `rect` without a host's frame, for a test.
    #[cfg(test)]
    pub(crate) fn show_for_test(&mut self, rect: Rect) {
        let row = search_room(rect);
        self.row.lay_out(row);
        self.place_search(rect, row);
        self.shown = Some((WindowToolbar::default(), rect));
    }

    /// Type `text` into the open search, key by key, for a test.
    #[cfg(test)]
    pub(crate) fn type_into_search(&mut self, text: &str) {
        let node = self.node.expect("the row's event node");
        let modifiers = winit::event::Modifiers::default();
        let root = automerge::ROOT;
        for character in text.chars() {
            let key = winit::keyboard::Key::Character(character.to_string().into());
            let event = crate::synthetic_input::key_input(&key, ElementState::Pressed);
            let (mut handled, mut focus) = (false, None);
            let mut ctx = crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: crate::widget::Edits::lend(&mut self.scratch),
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &self.tree,
            };
            assert!(
                self.search.route(&event, &mut ctx),
                "the field takes the key"
            );
        }
    }
}

/// The row's room: the window's band less the search control at its end.
fn search_room(rect: Rect) -> Rect {
    Rect::new(
        rect.x0,
        rect.y0,
        (rect.x1 - CONTROL_PX - CONTROL_GAP).max(rect.x0),
        rect.y1,
    )
}
