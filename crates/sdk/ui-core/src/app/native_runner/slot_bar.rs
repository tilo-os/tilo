//! The app's window-bar slots in its own window, outside a Tilo session.
//!
//! An app declares its toolbar once, as slots in the window's bar
//! ([`crate::window_toolbar`]); in a Tilo session the desktop draws them in
//! the one bar it draws per window. Run anywhere else (another desktop, a
//! window of its own) nothing hosts them, so the runner draws them itself,
//! with the same [`ToolbarRow`] the desktop's bar uses, in one row across
//! the top of the window under whatever frame the host gives it. The app
//! draws no toolbar of its own either way, and hears what the person did in
//! the row exactly as it hears the desktop's bar
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

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::window::Window;

use super::super::App;
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
const HISTORY: &str = "tilo.window.history";

/// The app's slots with the window's History added to the row's menu.
fn with_history(mut toolbar: WindowToolbar, history: Option<HistoryControl>) -> WindowToolbar {
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

pub(super) struct SlotBar {
    row: ToolbarRow,
    /// What the row shows (none: the bar hosts the slots, or the app
    /// declared none), and where it stands.
    shown: Option<(WindowToolbar, Rect)>,
    /// A one-node tree, for the events the row hands its field and menus.
    tree: TaffyTree<()>,
    node: Option<NodeId>,
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

impl SlotBar {
    pub(super) fn new() -> Self {
        let mut tree = TaffyTree::new();
        let node = tree.new_leaf(taffy::Style::default()).ok();
        Self {
            row: ToolbarRow::new(),
            shown: None,
            tree,
            node,
            pointer: Point::ORIGIN,
            repaint: false,
            search: WindowSearch::new(),
            compact: false,
        }
    }

    /// Keep the row current: shown while nothing hosts the app's slots and
    /// it declared some, under the host's frame (`app.layout.safe_area`).
    /// Call every frame before layout: the row's height is part of the app's
    /// top inset.
    pub(super) fn refresh(&mut self, app: &mut App, window: &Window) {
        // Outside a session the row is the window's bar: it hosts the app's
        // slots and presents its search and History, as the desktop's bar
        // does in one. A window the shell hears is hosted by the shell from
        // its first frame: the row never shows there, not even until the
        // shell says so.
        let standalone = app.floating.client.is_none();
        let shown = standalone && window.fullscreen().is_none();
        if standalone {
            if crate::window_toolbar::set_hosted(shown) {
                app.needs_sync = true;
                app.layout.layout_dirty = true;
            }
            if app.floating.window_bar.presents() != shown {
                app.set_window_presents_history(shown);
            }
            self.serve_app_requests(app);
        }
        let toolbar = shown.then(|| {
            with_history(
                crate::window_toolbar::shown().unwrap_or_default(),
                app.window_history_control(),
            )
        });
        let Some(toolbar) = toolbar else {
            if app.layout.slot_bar_inset > 0.0 {
                app.layout.slot_bar_inset = 0.0;
                app.layout.layout_dirty = true;
            }
            if self.shown.take().is_some() {
                self.row
                    .show(WINDOW, None, app.theme.motion(), crate::motion::frame_now());
                self.search.close(MotionSource::Pointer);
                self.drive_search(app);
                self.search.place(Rect::ZERO, 0.0);
                self.repaint = true;
            }
            return;
        };
        let physical = window.inner_size();
        let (width, window_height) =
            crate::scale::logical_size((physical.width, physical.height), window.scale_factor());
        let area = app.layout.safe_area;
        let inset = f64::from(app.theme.tokens.spacing.s2);
        let rect = Rect::new(
            area.left + inset,
            area.top,
            (width - area.right - inset).max(area.left + inset),
            area.top + TOOLBAR_ROW_HEIGHT,
        );
        // A compact window says a notice in a strip of its own under the
        // row, as the desktop's bar does.
        let compact = width < f64::from(app.theme.tokens.metrics.compact_breakpoint);
        let restyled = self.compact != compact;
        self.compact = compact;
        if restyled || self.shown.as_ref() != Some(&(toolbar.clone(), rect)) {
            self.row.set_compact(compact);
            self.row.show(
                WINDOW,
                Some(&toolbar),
                app.theme.motion(),
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
        self.repaint |= self.row.measure_strip(&app.theme, rect.width());
        let strip = self.row.strip_height();
        self.row.place_strip(if strip > 0.0 {
            Rect::new(rect.x0, rect.y1, rect.x1, rect.y1 + strip)
        } else {
            Rect::ZERO
        });
        let height = TOOLBAR_ROW_HEIGHT + strip;
        if (app.layout.slot_bar_inset - height).abs() > f64::EPSILON {
            app.layout.slot_bar_inset = height;
            app.layout.layout_dirty = true;
            self.repaint = true;
        }
        self.search.set_app_name(&window.title());
        self.repaint |= self.row.measure_views(&app.theme);
        self.drive_search(app);
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
        );
    }

    /// What the app asked of its bar: its Find (open the search), its Ctrl+L
    /// (edit the location).
    fn serve_app_requests(&mut self, app: &mut App) {
        if let Some(opening) = crate::window_toolbar::take_search_request() {
            self.open_search(app, MotionSource::Keyboard, opening);
        }
        if crate::window_toolbar::take_edit_request() && self.row.begin_edit() {
            self.repaint = true;
        }
    }

    /// Open the search in place on what the app offers, or give the open
    /// one the keyboard (for the replace chord, the replacement). An app
    /// with nothing to search in place gets its own search of everything it
    /// publishes, as the desktop's bar hands it to the platform search.
    fn open_search(&mut self, app: &mut App, source: MotionSource, opening: SearchOpening) {
        if self.search.is_open() {
            match opening {
                SearchOpening::Find => self.search.take_keyboard(),
                SearchOpening::Replace => self.search.focus_replacement(),
            }
        } else {
            let in_place: InPlace = app.window_search_offer().into();
            if in_place == InPlace::Nothing {
                app.open_search(SearchRequest::this_app());
            } else {
                self.search.open_as(in_place, source, opening);
                // The document says what it offers as the search reaches it.
                self.drive_search(app);
            }
        }
        self.repaint = true;
    }

    /// Carry out what the person asked of the search.
    fn serve_search(&mut self, app: &mut App) -> bool {
        let requests = self.search.take_requests();
        let acted = !requests.is_empty();
        for request in requests {
            match request {
                WindowSearchRequest::Open => {
                    self.open_search(app, MotionSource::Pointer, SearchOpening::Find);
                }
                WindowSearchRequest::Close => self.search.close(crate::motion::input_source()),
                WindowSearchRequest::Widen(_) => {
                    // No platform search outside a session: the app's own
                    // search of everything it publishes takes the query on.
                    let query = self.search.query().to_owned();
                    self.search.close(crate::motion::input_source());
                    app.open_search(SearchRequest::this_app().with_query(query));
                }
            }
        }
        acted
    }

    /// Carry the search to the app's widgets, once a frame: what it found
    /// shows in the frame being built.
    fn drive_search(&mut self, app: &mut App) {
        if self.search.wants_drive() && self.search.drive(app.root_widget.as_mut()) {
            app.mark_semantic_tree_stale();
            app.layout.layout_dirty = true;
            self.repaint = true;
        }
    }

    /// The input evidence of the window's search, beside the app's own
    /// ([`App::native_input_snapshot`]): what the field holds and where the
    /// control stands, while the row shows.
    pub(super) fn with_search_evidence(
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
    pub(super) fn take_repaint(&mut self) -> bool {
        std::mem::take(&mut self.repaint)
    }

    /// Whether the row needs another frame: a menu arriving, the field's
    /// caret.
    pub(super) fn needs_frame(&self) -> bool {
        self.shown.is_some()
            && (self.row.needs_animation_frame(crate::time::now())
                || self.search.needs_animation_frame())
    }

    pub(super) fn draw(&mut self, app: &mut App, scene: &mut Scene, transform: Affine) {
        let Some((_, rect)) = self.shown.as_ref() else {
            return;
        };
        let (text, theme, presence) = (&mut app.text_renderer, &app.theme, &app.presence_ctx);
        let band = Rect::new(
            app.layout.safe_area.left,
            rect.y0,
            rect.x1 + f64::from(theme.tokens.spacing.s2),
            rect.y1 + self.row.strip_height(),
        );
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
    /// Returns whether the row took it; what the person did reaches the app
    /// as the desktop's bar delivers it.
    pub(super) fn route(&mut self, app: &mut App, window: &Window, event: &WindowEvent) -> bool {
        let (Some((_, rect)), Some(node)) = (self.shown.as_ref(), self.node) else {
            return false;
        };
        let rect = *rect;
        if let WindowEvent::CursorMoved { position, .. } = event {
            let scale = window.scale_factor();
            self.pointer = Point::new(position.x / scale, position.y / scale);
        }
        let over = rect.contains(self.pointer) || self.row.strip_rect().contains(self.pointer);
        let mut handled = false;
        let mut focus = None;
        // Find (and Find and Replace) opens the window's search, or gives
        // the open one the keyboard, before the app sees the chord, as the
        // desktop's bar does.
        if let Some(opening) = crate::widget::window_search::search_chord(
            event,
            app.input.modifiers.state(),
            app.input.platform,
        ) && !self.search.has_keyboard()
            && app.window_bar_takes(opening)
        {
            self.open_search(app, MotionSource::Keyboard, opening);
            window.request_redraw();
            return true;
        }
        let searched = {
            let mut ctx = crate::widget::EventCtx {
                scale_factor: window.scale_factor(),
                modifiers: &app.input.modifiers,
                state: crate::widget::Edits::lend(&mut app.state.doc),
                path: &app.state.root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &self.tree,
            };
            self.search.route(event, &mut ctx)
        };
        if self.serve_search(app) || searched {
            self.repaint = true;
            window.request_redraw();
        }
        if searched {
            return true;
        }
        let mut ctx = crate::widget::EventCtx {
            scale_factor: window.scale_factor(),
            modifiers: &app.input.modifiers,
            state: crate::widget::Edits::lend(&mut app.state.doc),
            path: &app.state.root,
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
        let events = self.row.take_events();
        let drafts = self.row.take_drafts();
        let acted = !events.is_empty() || !drafts.is_empty();
        for (_, event) in events.into_iter().chain(drafts) {
            match event {
                ToolbarEvent::Action { id } if id == HISTORY => {
                    if let Some(control) = app.window_history_control() {
                        let _ = app.invoke_window_history_control(control);
                    }
                }
                event => app.deliver_app_slot_event(event),
            }
        }
        if acted {
            app.needs_sync = true;
            app.layout.layout_dirty = true;
        }
        if took || acted {
            self.repaint = true;
            window.request_redraw();
        }
        took
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{NavItem, NavList, NavSection};
    use winit::keyboard::Key;

    fn settings_like_list() -> NavList {
        NavList::new(vec![
            NavSection::new(
                "personal",
                "Personal",
                vec![
                    NavItem::new("appearance", "Appearance"),
                    NavItem::new("sound", "Sound"),
                    NavItem::new("time", "Time & date"),
                ],
            )
            .filterable(),
        ])
    }

    fn filter(app: &App) -> String {
        app.root_widget
            .as_any()
            .and_then(|root| root.downcast_ref::<NavList>())
            .expect("the app is its list")
            .filter()
            .to_owned()
    }

    /// The row as shown in a window 800 wide, without a platform window.
    fn shown(bar: &mut SlotBar) {
        let rect = Rect::new(8.0, 0.0, 792.0, TOOLBAR_ROW_HEIGHT);
        let row = search_room(rect);
        bar.row.lay_out(row);
        bar.place_search(rect, row);
        bar.shown = Some((WindowToolbar::default(), rect));
    }

    fn type_into_search(bar: &mut SlotBar, app: &mut App, text: &str) {
        let Some(node) = bar.node else {
            panic!("the row's event node");
        };
        for character in text.chars() {
            let key = Key::Character(character.to_string().into());
            let event = crate::synthetic_input::key_input(&key, ElementState::Pressed);
            let mut handled = false;
            let mut focus = None;
            let mut ctx = crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &app.input.modifiers,
                state: crate::widget::Edits::lend(&mut app.state.doc),
                path: &app.state.root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &bar.tree,
            };
            assert!(
                bar.search.route(&event, &mut ctx),
                "the field takes the key"
            );
        }
    }

    // Outside a session the runner's row is the window's bar, and its search is
    // the window's one search entry: Find opens it on the app's list, what is
    // typed narrows the list in the next frame, and the evidence says so.
    #[test]
    fn the_standalone_bar_searches_the_apps_list_in_place() {
        let mut app = App::new(crate::State::new(), Box::new(settings_like_list()));
        let mut bar = SlotBar::new();
        shown(&mut bar);
        bar.open_search(&mut app, MotionSource::Keyboard, SearchOpening::Find);
        assert!(bar.search.is_open() && bar.search.has_keyboard());
        assert_eq!(
            bar.search.in_place(),
            InPlace::List,
            "a list narrows in place"
        );

        type_into_search(&mut bar, &mut app, "s");
        let typed = bar
            .with_search_evidence(Some(serde_json::json!({})))
            .expect("evidence");
        assert_eq!(typed["window_search"]["text"], "s");
        assert_eq!(typed["window_search"]["focused"], true);
        assert_eq!(typed["window_search"]["caret_byte"], 1);
        assert_eq!(
            filter(&app),
            "",
            "the list narrows in the frame, not the keystroke"
        );

        bar.drive_search(&mut app);
        assert_eq!(filter(&app), "s", "the frame narrows the list");

        bar.search.close(MotionSource::Keyboard);
        bar.drive_search(&mut app);
        assert_eq!(filter(&app), "", "closing the search shows the whole list");
    }

    #[test]
    fn evidence_is_only_the_apps_while_the_row_is_hidden() {
        let bar = SlotBar::new();
        let snapshot = bar
            .with_search_evidence(Some(serde_json::json!({ "app": "x" })))
            .expect("evidence");
        assert!(snapshot.get("window_search").is_none());
    }
}
