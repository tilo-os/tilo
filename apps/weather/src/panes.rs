//! The two pane widgets the shell hosts: thin holders of a rect and a handle to
//! the shared [`Core`], delegating drawing and input to it. The shell owns the
//! geometry, the push transition and the Back button.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use ui::widget::ModelPath;
use ui::widget::TaffyTreeExt;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::event::WindowEvent;

use ui::a11y::{AccessNode, Action};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use crate::core::{Core, OverviewEventCtx};

/// The city-list panel.
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

/// The forecast panel.
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

/// A leaf node that fills what its parent gives it, registered for `me`.
fn leaf(
    taffy: &mut TaffyTree<()>,
    node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    me: &mut (dyn Widget + 'static),
) -> NodeId {
    taffy.fill_leaf(node_map, me)
}

impl Widget for OverviewPane {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        leaf(taffy, node_map, self)
    }

    ui::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // The core's paint, which other parts of the app move: current only until
        // the next input.
        ui::outside_paint::note();
        self.core
            .borrow_mut()
            .draw_overview(self.rect, scene, transform, text_renderer, theme);
    }

    /// The add-place suggestions and the menu, in the window's top layer.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        ui::outside_paint::note();
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

    fn accessibility(&self) -> Option<AccessNode> {
        Some(self.core.borrow().overview_accessibility(self.rect))
    }

    fn perform_action(
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
        leaf(taffy, node_map, self)
    }

    ui::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        ui::outside_paint::note();
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

    fn tooltip(&self) -> Option<std::borrow::Cow<'_, str>> {
        self.core.borrow().detail_tooltip().map(Into::into)
    }

    fn accessibility(&self) -> Option<AccessNode> {
        Some(self.core.borrow().detail_accessibility(self.rect))
    }

    /// The forecast's one control: the notice's Retry button.
    fn perform_action(
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
