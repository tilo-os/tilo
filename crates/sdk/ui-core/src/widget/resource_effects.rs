//! The typed effect host for a retained widget root outside the app framework.
//!
//! A session surface that runs before any compositor exists — the sign-in
//! screen, the installer — keeps its state in the widget rather than in a
//! resource document, but its actions are ordinary published resources. This
//! host keeps the toolkit's raw operation protocol inside the toolkit: the
//! root names the toolkit's one typed effect vocabulary ([`Effect`]) and
//! answers it in [`TypedEffects::answer`], so an app's own sources neither
//! encode nor decode a host operation, while the app framework's
//! `ResourceSurface` remains the path a resource answers its effects through.
//!
//! [`Effect`]: tilo_resource::Effect

use crate::automerge::AutoCommit;
use crate::tilo_actions::{ActionOutput, OperationKind, Resource};
use crate::tilo_resource::Effect;
use crate::widget::{
    Decorator, ResourceOperation, ResourceOperationStart, ResourceOperations, Widget,
    start_in_children,
};

/// A retained widget root that publishes its resources and answers their
/// effects as one typed vocabulary.
///
/// Host it with [`host`]; the returned widget publishes what
/// [`published_resources`](Self::published_resources) reports and decodes the
/// operations of [`Self::Effect`] before reaching
/// [`answer`](Self::answer).
pub trait TypedEffects: Widget {
    /// The typed vocabulary every effect operation on this root decodes to:
    /// the toolkit's canonical [`Effect`].
    type Effect: Effect;

    /// The resources this root has open now, with the actions each exports.
    fn published_resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        Vec::new()
    }

    /// Answer one effect. Every door — a bound control, the palette, an agent
    /// or script, the session console — ends here, after its own resolution,
    /// availability and argument binding. The handler still re-checks the
    /// root's preconditions.
    ///
    /// # Errors
    ///
    /// Returns the reason the caller is told when the effect cannot run now.
    fn answer(&mut self, effect: Self::Effect) -> Result<ActionOutput, String>;
}

/// [`EffectHost`] around `root`: the production root of a session surface.
pub fn host<W: TypedEffects + 'static>(root: W) -> EffectHost<W> {
    EffectHost { inner: root }
}

/// The toolkit's typed-effect host over a retained root. It stands in for the
/// root's layout node and forwards every hook it does not add.
pub struct EffectHost<W: TypedEffects + 'static> {
    inner: W,
}

impl<W: TypedEffects + 'static> Decorator for EffectHost<W> {
    crate::provides!(
        dyn crate::resources::PublishesResources,
        dyn ResourceOperations;
        inner = inner
    );

    type Inner = W;

    fn inner(&self) -> &W {
        &self.inner
    }

    fn inner_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    /// The host adds no structure of its own: a change the root reports stays
    /// the change it reported.
    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        self.inner.take_structure_change()
    }
}

impl<W: TypedEffects + 'static> crate::resources::PublishesResources for EffectHost<W> {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        self.inner.published_resources(doc)
    }
}

impl<W: TypedEffects + 'static> ResourceOperations for EffectHost<W> {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[<W::Effect as Effect>::OPERATION]
    }

    fn start_operation(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        let request = op
            .unscoped()
            .filter(|request| request.kind != OperationKind::Document)
            .filter(|request| request.type_id == <W::Effect as Effect>::OPERATION);
        if let Some(request) = request {
            let effect = W::Effect::from_payload(request.payload)?;
            return self
                .inner
                .answer(effect)
                .map(|output| Some(ResourceOperationStart::Completed(output)));
        }
        // Everything else is the toolkit's own widgets inside the root (a
        // bound button, a field). A hosted invocation carries its original
        // caller's custody; this root answers only its local vocabulary.
        start_in_children(self, op, doc, path)
    }
}
