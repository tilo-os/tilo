//! A pane over a model the app keeps: the state and the background work of
//! one area of Settings (a device's updates, its networks) live in the model,
//! which the area's effects drive whether or not its pane is open; the pane
//! only shows it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use ui::widget::TaffyTreeExt;

use taffy::{NodeId, TaffyTree};
use ui::theme::Theme;
use ui::widget::{AppModel, ModelPath, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

/// The state of one area of Settings, as its pane shows it.
pub trait PaneModel: 'static {
    /// The pane's widgets, built from the model as it is now.
    fn compose(&self) -> Box<dyn Widget>;

    /// Take in what background work finished and what the device reports
    /// now; whether what the pane shows changed.
    fn poll(&mut self) -> bool;

    /// The pane's name, for the accessibility tree and tests.
    fn name(&self) -> &'static str;

    /// Whether background work is still running, so the app keeps polling.
    fn busy(&self) -> bool {
        false
    }

    /// When the model next wants to look again while nothing else happens.
    fn next_wake(&self) -> Option<ui::time::Instant> {
        None
    }
}

/// A shared model, held by its area and shown by its pane.
pub type Shared<M> = Rc<RefCell<M>>;

/// The pane showing a [`PaneModel`].
pub struct ModelPane<M: PaneModel> {
    model: Shared<M>,
    composed: Option<Box<dyn Widget>>,
    composition_dirty: bool,
    catalog_revision: u64,
    structure_dirty: bool,
    composition_paint_dirty: bool,
    drawn_rect: Rect,
    rect: Rect,
}

impl<M: PaneModel> ModelPane<M> {
    pub fn new(model: Shared<M>) -> Self {
        Self {
            model,
            composed: None,
            composition_dirty: true,
            catalog_revision: ui::i18n::I18nManager::revision(),
            structure_dirty: false,
            composition_paint_dirty: true,
            drawn_rect: Rect::ZERO,
            rect: Rect::ZERO,
        }
    }

    /// The model the pane shows.
    pub fn model(&self) -> std::cell::Ref<'_, M> {
        self.model.borrow()
    }
}

impl<M: PaneModel> Widget for ModelPane<M> {
    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        if self.model.borrow_mut().poll() {
            self.composition_dirty = true;
            self.structure_dirty = true;
        }
        if let Some(c) = &mut self.composed {
            c.sync(state, root);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        if self.composition_dirty || self.catalog_revision != ui::i18n::I18nManager::revision() {
            self.composed = None;
            self.composition_paint_dirty = true;
        }
        let composed = self
            .composed
            .get_or_insert_with(|| self.model.borrow().compose());
        self.composition_dirty = false;
        // Composition may initialize the pane's embedded catalogue.
        self.catalog_revision = ui::i18n::I18nManager::revision();
        let child = composed.build_layout(taffy, theme, node_map);
        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &[child], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&cn) = children.first()
        {
            c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(c) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, cn));
        }
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.composition_paint_dirty {
            // Replacement can erase old text between the new child's boxes.
            // Keep both painted extents until the replacement frame is drawn.
            regions.push(if self.drawn_rect.is_zero_area() {
                self.rect
            } else {
                self.rect.union(self.drawn_rect)
            });
        } else if let Some(composed) = &self.composed {
            composed.collect_damage(regions);
        }
    }

    fn needs_redraw(&self) -> bool {
        self.composition_paint_dirty
            || self
                .composed
                .as_ref()
                .is_some_and(|child| child.needs_redraw())
    }

    fn mark_drawn(&mut self) {
        self.composition_paint_dirty = false;
        self.drawn_rect = self.rect;
        if let Some(composed) = &mut self.composed {
            composed.mark_drawn();
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

    fn name(&self) -> &'static str {
        self.model.borrow().name()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self
            .composed
            .as_mut()
            .is_some_and(|c| c.take_structure_dirty());
        own || child
    }

    fn needs_animation_frame(&self) -> bool {
        self.model.borrow().busy()
            || self
                .composed
                .as_ref()
                .is_some_and(|child| child.needs_animation_frame())
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let own = self.model.borrow().next_wake();
        let child = self.composed.as_ref().and_then(|c| c.next_wake_deadline());
        match (own, child) {
            (Some(own), Some(child)) => Some(own.min(child)),
            (own, child) => own.or(child),
        }
    }
}

/// The callers waiting for a model's requests to finish, by effect id.
#[derive(Default)]
pub struct Replies {
    waiting: HashMap<&'static str, ui::framework::EffectReply>,
}

impl Replies {
    /// `reply` waits for the request `effect` started.
    pub fn wait(&mut self, effect: &'static str, reply: ui::framework::EffectReply) {
        self.waiting.insert(effect, reply);
    }

    /// Tell the caller of `effect` it succeeded, saying `message`.
    pub fn succeed(&mut self, effect: &'static str, message: impl Into<String>) {
        if let Some(reply) = self.waiting.remove(effect) {
            reply.finish(Ok(ui::tilo_actions::ActionOutput {
                value: Some(serde_json::Value::String(message.into())),
                resources: Vec::new(),
            }));
        }
    }

    /// Tell the caller of `effect` why it failed.
    pub fn fail(&mut self, effect: &'static str, message: impl Into<String>) {
        if let Some(reply) = self.waiting.remove(effect) {
            reply.finish(Err(message.into()));
        }
    }

    /// Tell the caller of `effect` how it ended.
    pub fn finish(&mut self, effect: &'static str, result: Result<String, String>) {
        match result {
            Ok(message) => self.succeed(effect, message),
            Err(message) => self.fail(effect, message),
        }
    }
}
