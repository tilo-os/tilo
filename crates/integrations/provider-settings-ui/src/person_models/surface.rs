//! A person's model providers as a part any window places.

use std::any::Any;
use std::collections::HashMap;

use ui::LocalState;
use ui::framework::{Actions, EffectAnswer, EffectCx, Effected, ViewCx};
use ui::tilo_resource::Resource;
use ui::widget::{AppModel, Composed, Element, ModelPath, TaffyTreeExt, Widget};

use super::{PersonModels, PersonModelsEffect};

/// The first minute, where a window has nothing to show before a model is
/// linked: choose a provider, type its key into the protected field, pick a
/// model. It asks the host whether a model is linked and says so in
/// `linked`, so the window shows it only while that is `false`:
///
/// ```ignore
/// cx.switch(&linked, move |cx, linked| {
///     if *linked { conversation(cx) } else { first_model(cx, &linked_state) }
/// })
/// ```
///
/// The requests are effects of the window's own resource, answered here.
pub fn first_model<R: Resource>(cx: &ViewCx<R>, linked: &LocalState<bool>) -> Element {
    let _answered = cx.window_effects(ModelsSurface::<R>::effect);
    Box::new(ModelsSurface::attached(cx, Some(linked.clone())))
}

/// Everything about the person's model providers: the providers, linking
/// another, and the model each kind of work runs on. Before a model is
/// linked it is the first minute.
pub fn everything<R: Resource>(cx: &ViewCx<R>) -> Element {
    let _answered = cx.window_effects(ModelsSurface::<R>::effect);
    Box::new(ModelsSurface::attached(cx, None))
}

/// [`everything`], with `unserved` in its place until a host says
/// it keeps the person's providers: on a device, or a page alone, none
/// does.
pub fn everything_or<R: Resource>(
    cx: &ViewCx<R>,
    unserved: impl Fn() -> Element + 'static,
) -> Element {
    let _answered = cx.window_effects(ModelsSurface::<R>::effect);
    let mut surface = ModelsSurface::attached(cx, None);
    surface.unserved = Some(Box::new(unserved));
    Box::new(surface)
}

/// The person's model providers in a window whose resource is `R`.
pub struct ModelsSurface<R: Resource> {
    model: PersonModels,
    actions: Actions<R>,
    composed: Composed,
    /// Where the first minute says whether a model is linked; `None` for
    /// the whole page.
    linked: Option<LocalState<bool>>,
    /// What stands in the page's place until a host answers that it keeps
    /// the person's providers.
    unserved: Option<Box<dyn Fn() -> Element>>,
}

impl<R: Resource> ModelsSurface<R> {
    fn attached(cx: &ViewCx<R>, linked: Option<LocalState<bool>>) -> Self {
        let remote = ui::RemoteActions::current();
        Self {
            // A window that only describes itself (a capture, History) asks
            // no host anything.
            model: if cx.mode().is_live() {
                PersonModels::new(remote)
            } else {
                PersonModels::unasked(remote)
            },
            actions: cx.actions(),
            composed: Composed::default(),
            linked,
            unserved: None,
        }
    }

    fn effect(&mut self, effect: PersonModelsEffect, _cx: &mut EffectCx<R>) -> EffectAnswer {
        self.model.apply(effect)?;
        self.composed.invalidate();
        Ok(Effected::done())
    }

    /// What the host answered.
    #[must_use]
    pub fn model(&self) -> &PersonModels {
        &self.model
    }
}

impl<R: Resource> Widget for ModelsSurface<R> {
    ui::forward_to_composed!(composed);

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        // The page first: a key typed into its field is handed over as the
        // field syncs, and taken in the same pass.
        self.composed.sync(state, path);
        if self.model.poll() {
            self.composed.invalidate();
        }
        if let Some(linked) = &self.linked
            && let Some(now) = self.model.linked()
            && linked.get() != now
        {
            linked.set(now);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &ui::theme::Theme,
        node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let model = &self.model;
        let actions = &self.actions;
        let first = self.linked.is_some();
        let unserved = self.unserved.as_ref();
        let child = self.composed.build_layout_kept(
            || -> Box<dyn Widget> {
                if let Some(unserved) = unserved
                    && model.served() != Some(true)
                {
                    return unserved();
                }
                crate::widgets::page(if first {
                    model.first_model_sections(actions)
                } else {
                    model.sections(actions)
                })
            },
            taffy,
            theme,
            node_map,
        );
        let style = taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &[child], node_map, self)
    }

    fn needs_animation_frame(&self) -> bool {
        self.model.busy()
            || self
                .composed
                .element()
                .is_some_and(Widget::needs_animation_frame)
    }

    fn name(&self) -> &'static str {
        "PersonModels"
    }
}
