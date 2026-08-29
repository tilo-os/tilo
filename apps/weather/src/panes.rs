//! The two content-panel widgets the [`ui::widget::NavShell`] hosts.
//!
//! Both are deliberately thin: they own only their allocated rect and a
//! shared handle to the [`Core`] (the model + view state). Drawing and input
//! delegate straight into `Core`, so the overview and the detail render from
//! one source of truth — selecting a city in the overview is immediately
//! reflected in the detail because both read the same `Core`.
//!
//! The shell owns the responsive geometry, the push/side-by-side transition,
//! the Back affordance and the divider; these panes just paint their content
//! into whatever rect the shell hands them, which is how the same code works
//! side-by-side (wide) and full-screen-push (narrow).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use crate::core::Core;

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

fn store_rect(taffy: &TaffyTree<()>, node: NodeId, origin: Point) -> Rect {
    let l = taffy.layout(node).unwrap();
    Rect::new(
        origin.x + f64::from(l.location.x),
        origin.y + f64::from(l.location.y),
        origin.x + f64::from(l.location.x) + f64::from(l.size.width),
        origin.y + f64::from(l.location.y) + f64::from(l.size.height),
    )
}

impl Widget for OverviewPane {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        leaf(taffy, node_map, std::ptr::from_mut::<dyn Widget>(self))
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = store_rect(taffy, node, origin);
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
        if *handled {
            return;
        }
        if self.core.borrow_mut().handle_overview_event(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            focus_target,
            node_id,
            taffy,
        ) {
            *handled = true;
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
}

impl Widget for DetailPane {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        leaf(taffy, node_map, std::ptr::from_mut::<dyn Widget>(self))
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = store_rect(taffy, node, origin);
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

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        if *handled {
            return;
        }
        if self
            .core
            .borrow_mut()
            .handle_detail_event(event, scale_factor)
        {
            *handled = true;
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
}
