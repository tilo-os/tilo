//! The actions a window's views bind, kept while what they are built from is
//! unchanged.

use std::rc::Rc;

use tilo_actions::ResourceSpec;

use super::shared_tree::mark_document_jobs;
use super::{BindingBase, ResourceRoot};

/// What [`ResourceRoot::binding_overlay`] built its actions from, and the
/// actions it built: the edits a view binds change only with the state they
/// read, the words they are labelled in, the views that bind them, and what
/// the surfaces say of their effects.
pub(super) struct OverlayKey {
    base: Rc<BindingBase>,
    version: ((u64, u64), u64),
    bindings: u64,
    availability: Vec<super::super::effect::EffectStatus>,
}

impl PartialEq for OverlayKey {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.base, &other.base)
            && self.version == other.version
            && self.bindings == other.bindings
            && self.availability == other.availability
    }
}

pub(super) struct OverlayCache {
    key: OverlayKey,
    actions: Rc<Vec<tilo_actions::ActionSpec>>,
}

impl ResourceRoot {
    /// What the views add over `base` now, as
    /// [`Self::decorate_publication`] adds it: the edits they bound, their
    /// availability, and the app's effects.
    ///
    /// Reading it builds a declaration and an action for every edit a view
    /// bound, which an app showing a table or a calendar does for every row it
    /// shows, and a frame reads it twice: once before the view syncs and once
    /// after. The second read is answered from the first unless what it reads
    /// moved in between — the state, the locale, the views that bound edits,
    /// and what the surfaces say of their effects.
    pub(super) fn binding_overlay(
        &self,
        base: &Rc<BindingBase>,
    ) -> Rc<Vec<tilo_actions::ActionSpec>> {
        let state = self.typed.state();
        let mut availability = Vec::new();
        self.surfaces(&mut |surface| {
            let _profile = tilo_resource::profiling::Phase::start("framework.surface_publication");
            availability.extend(surface.availability());
        });
        let key = OverlayKey {
            base: Rc::clone(base),
            version: (self.version(), ui_core::i18n::I18nManager::revision()),
            bindings: self.typed.bindings_generation(),
            availability,
        };
        if let Some(cached) = self
            .overlay
            .borrow()
            .as_ref()
            .filter(|cached| cached.key == key)
        {
            return Rc::clone(&cached.actions);
        }
        let actions = Rc::new(self.build_overlay(base, &*state, &key.availability));
        *self.overlay.borrow_mut() = Some(OverlayCache {
            key,
            actions: Rc::clone(&actions),
        });
        actions
    }

    /// [`Self::binding_overlay`]'s actions, built.
    pub(super) fn build_overlay(
        &self,
        base: &BindingBase,
        state: &dyn std::any::Any,
        availability: &[super::super::effect::EffectStatus],
    ) -> Vec<tilo_actions::ActionSpec> {
        let bound = {
            let _profile = tilo_resource::profiling::Phase::start("framework.bound_declarations");
            self.typed.declarations(state)
        };
        let own = base
            .resource
            .actions
            .get(..base.head_len)
            .unwrap_or_default();
        let shadows = base
            .resource
            .actions
            .get(..base.shadow_len)
            .unwrap_or_default();
        // The head's actions the edits bind over, refined where they say so.
        let ids = self.typed.declaration_ids(&*bound);
        let mut overlay = ResourceSpec::new("", "", "");
        overlay.actions = own
            .iter()
            .filter(|spec| ids.contains(&spec.id.as_str()))
            .cloned()
            .collect();
        overlay = self.typed.bind(overlay, bound);
        for status in availability {
            if !overlay.actions.iter().any(|spec| spec.id == status.id())
                && let Some(spec) = own.iter().find(|spec| spec.id == status.id())
            {
                overlay.actions.push(spec.clone());
            }
            for spec in overlay
                .actions
                .iter_mut()
                .filter(|spec| spec.id == status.id())
            {
                status.overlay(spec);
            }
        }
        let effects = {
            let _profile = tilo_resource::profiling::Phase::start("framework.effect_specs");
            self.typed.effect_specs()
        };
        for spec in effects {
            let shadowed = shadows
                .iter()
                .chain(&overlay.actions)
                .any(|existing| existing.id == spec.id);
            debug_assert!(!shadowed, "effect `{}` shadows an action", spec.id);
            if shadowed {
                tracing::warn!(effect = spec.id, "effect shadows an action; not published");
                continue;
            }
            overlay.actions.push(spec);
        }
        if base.key.causal {
            mark_document_jobs(&mut overlay);
        }
        if let Some(unreadable) = &base.key.unreadable {
            tilo_resource::withhold_document_actions(&mut overlay, unreadable);
        }
        overlay.actions
    }
}
