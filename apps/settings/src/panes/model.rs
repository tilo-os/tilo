//! A pane over a model the app keeps: the state and the background work of
//! one area of Settings (a device's updates, its networks) live in the model,
//! which the area's effects drive whether or not its pane is open; the pane
//! only shows it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use ui::theme::Theme;
use ui::widget::{AppModel, ModelPath, Widget};
use vello::kurbo::Rect;

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
    composed: ui::widget::Composed,
    composition_dirty: bool,
    catalog_revision: u64,
    structure_dirty: bool,
    composition_paint_dirty: bool,
    drawn_rect: Rect,
}

impl<M: PaneModel> ModelPane<M> {
    pub fn new(model: Shared<M>) -> Self {
        Self {
            model,
            composed: ui::widget::Composed::default(),
            composition_dirty: true,
            catalog_revision: ui::i18n::I18nManager::revision(),
            structure_dirty: false,
            composition_paint_dirty: true,
            drawn_rect: Rect::ZERO,
        }
    }

    /// The model the pane shows.
    pub fn model(&self) -> std::cell::Ref<'_, M> {
        self.model.borrow()
    }
}

impl<M: PaneModel> Widget for ModelPane<M> {
    ui::forward_to_composed!(composed);

    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        if self.model.borrow_mut().poll() {
            self.composition_dirty = true;
            self.structure_dirty = true;
        }
        self.composed.sync(state, root);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        if self.composition_dirty || self.catalog_revision != ui::i18n::I18nManager::revision() {
            self.composed.clear();
            self.composition_paint_dirty = true;
        }
        let composition = self
            .composed
            .take()
            .unwrap_or_else(|| self.model.borrow().compose());
        self.composition_dirty = false;
        // Composition may initialize the pane's embedded catalogue.
        self.catalog_revision = ui::i18n::I18nManager::revision();
        let child = self
            .composed
            .build_layout(composition, taffy, theme, node_map);
        crate::panes::pane_node(taffy, child, node_map, self)
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.composition_paint_dirty {
            // Replacement can erase old text between the new child's boxes.
            // Keep both painted extents until the replacement frame is drawn.
            let rect = self.composed.rect();
            regions.push(if self.drawn_rect.is_zero_area() {
                rect
            } else {
                rect.union(self.drawn_rect)
            });
        } else if let Some(composed) = self.composed.element() {
            composed.collect_damage(regions);
        }
    }

    fn needs_redraw(&self) -> bool {
        self.composition_paint_dirty || self.composed.element().is_some_and(Widget::needs_redraw)
    }

    fn mark_drawn(&mut self) {
        self.composition_paint_dirty = false;
        self.drawn_rect = self.composed.rect();
        if let Some(composed) = self.composed.element_mut() {
            composed.mark_drawn();
        }
    }

    fn name(&self) -> &'static str {
        self.model.borrow().name()
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self.composed.take_structure_dirty();
        own || child
    }

    fn needs_animation_frame(&self) -> bool {
        self.model.borrow().busy()
            || self
                .composed
                .element()
                .is_some_and(Widget::needs_animation_frame)
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let own = self.model.borrow().next_wake();
        let child = self.composed.element().and_then(Widget::next_wake_deadline);
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
