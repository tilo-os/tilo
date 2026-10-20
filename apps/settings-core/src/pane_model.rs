//! A pane over a model the app keeps: the state and the background work of
//! one area of Settings (a device's updates, its networks) live in the model,
//! which the area's effects drive whether or not its pane is open; the pane
//! only shows it.

use std::any::Any;
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

    /// Start following what the device reports, for a pane that shows the
    /// model: the watches, monitor or thread that tell it of changes run
    /// while a pane holds what this answers. Panes showing the model share
    /// one, so recomposing a pane never restarts them.
    fn follow(&mut self) -> Option<Rc<dyn Any>> {
        None
    }
}

/// A shared model, held by its area and shown by its pane.
pub type Shared<M> = Rc<RefCell<M>>;

/// The pane showing a [`PaneModel`].
///
/// It holds its model as `dyn PaneModel`, not as a type parameter: a generic
/// pane would compile its whole widget implementation once per area, all of
/// it shipped in the Settings tapp.
pub struct ModelPane {
    model: Rc<RefCell<dyn PaneModel>>,
    composed: ui::widget::Composed,
    composition_dirty: bool,
    catalog_revision: u64,
    composition_paint_dirty: bool,
    drawn_rect: Rect,
    /// What keeps the model following the device while this pane shows it.
    _following: Option<Rc<dyn Any>>,
}

impl ModelPane {
    pub fn new<M: PaneModel>(model: Shared<M>) -> Self {
        Self::of(model)
    }

    fn of(model: Rc<RefCell<dyn PaneModel>>) -> Self {
        let following = model.borrow_mut().follow();
        Self {
            model,
            _following: following,
            composed: ui::widget::Composed::default(),
            composition_dirty: true,
            catalog_revision: ui::i18n::I18nManager::revision(),
            composition_paint_dirty: true,
            drawn_rect: Rect::ZERO,
        }
    }
}

impl Widget for ModelPane {
    ui::forward_to_composed!(composed);

    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        if self.model.borrow_mut().poll() {
            self.composition_dirty = true;
            self.composed.invalidate();
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
        crate::pane_model::pane_node(taffy, child, node_map, self)
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
        self.answer(effect, serde_json::Value::String(message.into()));
    }

    /// Tell the caller of `effect` it succeeded, answering `value`.
    pub fn answer(&mut self, effect: &'static str, value: serde_json::Value) {
        if let Some(reply) = self.waiting.remove(effect) {
            reply.finish(Ok(ui::tilo_actions::ActionOutput {
                value: Some(value),
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
/// A pane's own node: a full-width column around its composed `child`,
/// registered for `pane`.
#[allow(
    clippy::implicit_hasher,
    reason = "the node map is the toolkit's layout pass's, with its hasher"
)]
pub fn pane_node(
    taffy: &mut ui::taffy::TaffyTree<()>,
    child: ui::taffy::NodeId,
    node_map: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn ui::widget::Widget>,
    pane: &mut (dyn ui::widget::Widget + 'static),
) -> ui::taffy::NodeId {
    use ui::widget::TaffyTreeExt as _;
    let style = ui::taffy::Style {
        display: ui::taffy::Display::Flex,
        flex_direction: ui::taffy::FlexDirection::Column,
        size: ui::taffy::Size {
            width: ui::taffy::Dimension::percent(1.0),
            height: ui::taffy::Dimension::auto(),
        },
        ..Default::default()
    };
    taffy.widget_node(style, &[child], node_map, pane)
}

/// `build_layout` for a pane whose content is `self.build_composition()`
/// held in `self.composed`, wrapped in [`pane_node`], plus forwarding the
/// rest of the widget to that composition.
///
/// `composed_pane!(kept)` keeps the composition across layouts, so a resize
/// or a theme change leaves its controls (and their hover, press and focus)
/// in place: for a pane whose controls follow their fields
/// ([`crate::row::SettingRow`]) and that invalidates `self.composed` when
/// what it builds the page from directly changes.
#[macro_export]
macro_rules! composed_pane {
    (kept) => {
        fn build_layout(
            &mut self,
            taffy: &mut ui::taffy::TaffyTree<()>,
            theme: &ui::theme::Theme,
            node_map: &mut std::collections::HashMap<
                ui::taffy::NodeId,
                *mut dyn ui::widget::Widget,
            >,
        ) -> ui::taffy::NodeId {
            let mut composed = std::mem::take(&mut self.composed);
            let child =
                composed.build_layout_kept(|| self.build_composition(), taffy, theme, node_map);
            self.composed = composed;
            $crate::pane_model::pane_node(taffy, child, node_map, self)
        }

        ui::forward_to_composed!(composed);
    };
    () => {
        fn build_layout(
            &mut self,
            taffy: &mut ui::taffy::TaffyTree<()>,
            theme: &ui::theme::Theme,
            node_map: &mut std::collections::HashMap<
                ui::taffy::NodeId,
                *mut dyn ui::widget::Widget,
            >,
        ) -> ui::taffy::NodeId {
            let composition = self.build_composition();
            let child = self
                .composed
                .build_layout(composition, taffy, theme, node_map);
            $crate::pane_model::pane_node(taffy, child, node_map, self)
        }

        ui::forward_to_composed!(composed);
    };
}
pub use crate::composed_pane;

/// Why this process cannot tell which programs the device has installed,
/// when it cannot: no directory on `PATH` is visible to it. A lookup would
/// then report every program as missing, which says nothing about the device.
pub fn installed_programs_unreadable() -> Option<String> {
    programs_unreadable_on(std::env::var_os("PATH").as_deref())
}

fn programs_unreadable_on(path: Option<&std::ffi::OsStr>) -> Option<String> {
    let Some(path) = path else {
        return Some("PATH is not set".to_string());
    };
    if std::env::split_paths(path).any(|dir| dir.is_dir()) {
        None
    } else {
        Some(format!(
            "none of {} is visible to Settings",
            path.to_string_lossy()
        ))
    }
}

/// A pane over a model the area keeps, or an empty page when the window
/// holds no such area.
pub fn model_pane<M: PaneModel>(model: Option<&Shared<M>>) -> Box<dyn ui::widget::Widget> {
    model.map_or_else(
        || crate::widgets::page(Vec::new()),
        |model| Box::new(ModelPane::new(std::rc::Rc::clone(model))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `PATH` none of whose directories is visible cannot say a program is
    /// missing; one visible directory can.
    #[test]
    fn programs_are_unreadable_only_when_no_path_directory_is_visible() {
        let dir = tempfile::tempdir().unwrap();
        let hidden = dir.path().join("usr-bin");
        let unreadable = programs_unreadable_on(Some(hidden.as_os_str()));
        assert!(
            unreadable
                .as_deref()
                .is_some_and(|error| error.ends_with("is visible to Settings")),
            "{unreadable:?}"
        );
        let mixed = std::env::join_paths([hidden.as_path(), dir.path()]).unwrap();
        assert_eq!(programs_unreadable_on(Some(&mixed)), None);
        assert_eq!(
            programs_unreadable_on(None).as_deref(),
            Some("PATH is not set")
        );
    }
}
