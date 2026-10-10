//! The two content-panel widgets the [`ui::widget::NavShell`] hosts.
//!
//! Both are deliberately thin: they own only their allocated rect and a
//! shared handle to the [`Core`] (the model + view state). Drawing and input
//! delegate straight into `Core`, so the overview and the detail render from
//! one source of truth — selecting a city in the overview is immediately
//! reflected in the detail because both read the same `Core`.
//!
//! The shell owns the header row, the responsive geometry, the
//! push/side-by-side transition and the Back button; these panes just paint
//! their content into whatever rect the shell hands them, which is how the
//! same code works side-by-side (wide) and full-screen-push (narrow).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use ui::widget::ModelPath;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::a11y::{AccessNode, Action};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Widget, layout_rect};

use crate::core::{Core, OverviewEventCtx};

/// The contentful city-list (master / overview) panel.
pub struct OverviewPane {
    core: Rc<RefCell<Core>>,
    rect: Rect,
}

impl OverviewPane {
    pub fn new(core: Rc<RefCell<Core>>) -> Self {
        Self {
            core,
            rect: Rect::ZERO,
        }
    }
}

/// The forecast (detail) panel.
pub struct DetailPane {
    core: Rc<RefCell<Core>>,
    rect: Rect,
}

impl DetailPane {
    pub fn new(core: Rc<RefCell<Core>>) -> Self {
        Self {
            core,
            rect: Rect::ZERO,
        }
    }
}

// `Widget::build_layout`/`apply_layout` (the callers below) return
// `NodeId`/`()`, not a `Result` — the trait is shared by every widget in
// `ui`, so its signature isn't ours to change here. `TaffyTree::new_leaf`
// and `::layout` both wrap an infallible operation in this taffy version
// (`new_leaf` always inserts; `layout` always finds a node this module
// just registered) — the `Result` is vestigial, not a real failure mode.
#[allow(
    clippy::unwrap_used,
    reason = "TaffyTree::new_leaf never fails in this taffy version, and the Widget trait's build_layout signature isn't ours to change to Result"
)]
fn leaf(
    taffy: &mut TaffyTree<()>,
    node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    me: *mut dyn Widget,
) -> NodeId {
    let node = taffy
        .new_leaf(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        })
        .unwrap();
    node_map.insert(node, me);
    node
}

impl Widget for OverviewPane {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        leaf(taffy, node_map, std::ptr::from_mut::<dyn Widget>(self))
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = layout_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.core
            .borrow_mut()
            .draw_overview(self.rect, scene, transform, text_renderer, theme);
    }

    /// The add-place suggestions and the per-place menu float in the
    /// window's top layer, over the forecast too.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.core.borrow_mut().draw_overview_overlay(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
        );
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.core.borrow().overview_modal_capture()
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if *ctx.handled {
            return;
        }
        if self
            .core
            .borrow_mut()
            .handle_overview_event(OverviewEventCtx {
                event,
                scale_factor: ctx.scale_factor,
                modifiers: *ctx.modifiers,
                state: ctx.state.reborrow(),
                path: ctx.path,
                focus_target: ctx.focus_target,
                node_id: ctx.node_id,
                taffy: ctx.taffy,
            })
        {
            *ctx.handled = true;
        }
    }

    fn name(&self) -> &'static str {
        "WeatherOverview"
    }

    fn test_id(&self) -> Option<&str> {
        Some("weather-overview")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<AccessNode> {
        Some(self.core.borrow().overview_accessibility(self.rect))
    }

    fn perform_action_at(
        &mut self,
        action: &Action,
        key_path: &[u64],
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let (state, path) = ui::doc::document(state, path);
        let search_key = ui::a11y::hash_key(&"weather.city_search");
        if let Some(index) = key_path.iter().position(|key| *key == search_key) {
            #[expect(
                clippy::indexing_slicing,
                reason = "`index` comes from `position` on this slice, so it is in bounds"
            )]
            let search_path = &key_path[index..];
            return self
                .core
                .borrow_mut()
                .perform_search_action(action, search_path, state, path);
        }
        if !matches!(action, Action::Activate) {
            return false;
        }
        let Some(key) = key_path.last().copied() else {
            return false;
        };
        let id = self
            .core
            .borrow()
            .model
            .locations
            .iter()
            .find(|location| ui::a11y::hash_key(&location.id) == key)
            .map(|location| location.id);
        id.is_some_and(|id| self.core.borrow_mut().open_location(id))
    }
}

impl Widget for DetailPane {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        leaf(taffy, node_map, std::ptr::from_mut::<dyn Widget>(self))
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = layout_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.core
            .borrow_mut()
            .draw_detail(self.rect, scene, transform, text_renderer, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if *ctx.handled {
            return;
        }
        if self
            .core
            .borrow_mut()
            .handle_detail_event(event, ctx.scale_factor)
        {
            *ctx.handled = true;
        }
    }

    fn name(&self) -> &'static str {
        "WeatherDetail"
    }

    fn test_id(&self) -> Option<&str> {
        Some("weather-detail")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn tooltip(&self) -> Option<&str> {
        self.core.borrow().detail_tooltip()
    }

    fn accessibility(&self) -> Option<AccessNode> {
        Some(self.core.borrow().detail_accessibility(self.rect))
    }

    /// The forecast's one accessible control: the out-of-date notice's
    /// Retry button.
    fn perform_action_at(
        &mut self,
        action: &Action,
        key_path: &[u64],
        _state: &mut ui::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        let retry = ui::a11y::hash_key(&crate::core::RETRY_KEY);
        if !matches!(action, Action::Activate) || key_path.last() != Some(&retry) {
            return false;
        }
        self.core.borrow_mut().retry();
        true
    }
}
