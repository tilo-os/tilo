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

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::window::Window;

use super::super::App;
use crate::widget::HistoryControl;
use crate::widget::tabs::TOOLBAR_ROW_HEIGHT;
use crate::widget::window_toolbar_row::ToolbarRow;
use crate::window_toolbar::{ToolbarAction, ToolbarEvent, WindowToolbar};

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
        }
    }

    /// Keep the row current: shown while nothing hosts the app's slots and
    /// it declared some, under the host's frame (`app.layout.safe_area`).
    /// Call every frame before layout: the row's height is part of the app's
    /// top inset.
    pub(super) fn refresh(&mut self, app: &mut App, window: &Window) {
        // A window the shell hears is hosted from its first frame: its
        // slots never show here, not even until the shell says so.
        let toolbar = (app.floating.client.is_none() && !crate::window_toolbar::is_hosted())
            .then(crate::window_toolbar::declared)
            .flatten()
            .filter(|_| window.fullscreen().is_none())
            .map(|toolbar| with_history(toolbar, app.window_history_control()));
        let height = if toolbar.is_some() {
            TOOLBAR_ROW_HEIGHT
        } else {
            0.0
        };
        if (app.layout.slot_bar_inset - height).abs() > f64::EPSILON {
            app.layout.slot_bar_inset = height;
            app.layout.layout_dirty = true;
        }
        let Some(toolbar) = toolbar else {
            if self.shown.take().is_some() {
                self.row
                    .show(WINDOW, None, app.theme.motion(), crate::motion::frame_now());
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
            area.top + height,
        );
        if self.shown.as_ref() != Some(&(toolbar.clone(), rect)) {
            self.row.show(
                WINDOW,
                Some(&toolbar),
                app.theme.motion(),
                crate::motion::frame_now(),
            );
            self.row
                .set_popup_area(Rect::new(0.0, 0.0, width, window_height));
            self.row.lay_out(rect);
            self.shown = Some((toolbar, rect));
            self.repaint = true;
        }
        self.repaint |= self.row.measure_views(&app.theme);
    }

    /// Whether the row changed since the last frame, which must then
    /// repaint the whole window rather than only the app's own damage.
    pub(super) fn take_repaint(&mut self) -> bool {
        std::mem::take(&mut self.repaint)
    }

    /// Whether the row needs another frame: a menu arriving, the field's
    /// caret.
    pub(super) fn needs_frame(&self) -> bool {
        self.shown.is_some() && self.row.needs_animation_frame(crate::time::now())
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
            rect.y1,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.content,
            None,
            &band,
        );
        self.row.paint(scene, transform, text, theme, presence);
        self.row
            .paint_popups(scene, transform, text, theme, presence);
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
        let over = rect.contains(self.pointer);
        let mut handled = false;
        let mut focus = None;
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
