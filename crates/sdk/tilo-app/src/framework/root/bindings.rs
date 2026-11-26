//! The document-owned half of a root: what a view's controls bind to, the
//! head a refresh publishes, and the publication a retained surface keeps.

use std::rc::Rc;

use tilo_actions::ResourceSpec;
use tilo_ui_base::widget::Element;
use ui_core::automerge::AutoCommit;

use super::shared_tree::mark_document_jobs;
use super::{
    BaseKey, BindingBase, BindingHead, Decoration, ResourceRoot, framework_specs, unreadable_state,
};
use crate::framework::rows::{LiveRow, merge};

impl ResourceRoot {
    /// The document-owned part of what controls bind to: the live
    /// resource's identity and own actions, without its children, published
    /// without building a child, so it costs the same however many items
    /// the resource holds.
    #[inline(never)]
    pub(super) fn binding_head(&self) -> BindingHead {
        let state = self.typed.state();
        let label = self.name_of(&*state);
        let version = (self.version(), tilo_ui_base::i18n::I18nManager::revision());
        let epoch = self.typed.cache_epoch(&*state);
        let resource = if let Some(reason) = &self.typed.unreadable() {
            // The state is the last that read, not what the document holds,
            // so even the fields bound controls were bound to are no longer
            // offered: every document action says why it does not apply.
            let mut head = self
                .typed
                .head(self.dispatch.resource_id(), &label, &*state);
            tilo_resource::withhold_document_actions(&mut head, reason);
            head.actions
                .retain(|spec| spec.id != tilo_resource::FIELD_EDIT);
            Rc::new(head)
        } else if let Some(epoch) = &epoch {
            self.live_head(&*state, epoch, &label)
        } else {
            let _profile = tilo_resource::profiling::Phase::start("framework.resource_head");
            Rc::new(
                self.typed
                    .head(self.dispatch.resource_id(), &label, &*state),
            )
        };
        BindingHead {
            version,
            epoch,
            resource,
        }
    }

    /// The head bound controls were last refreshed from, while it still
    /// holds: the resource may reuse its publication
    /// ([`Resource::publication_cache_epoch`]) and nothing it depends on
    /// moved since. A resource that may not is published afresh each sync.
    pub(super) fn retained_head(&self) -> Option<BindingHead> {
        let head = self.binding_head.borrow_mut().take()?;
        let current = self.typed.cache_epoch(&*self.typed.state());
        (head.epoch.is_some() && head.epoch == current).then_some(head)
    }

    /// What bound controls look up ([`super::controls::Actions`]), as one
    /// resource: the published resource's identity and actions over `head`,
    /// exactly as [`Self::publication`] publishes them, without building or
    /// decorating its children.
    #[cfg(test)]
    pub(super) fn binding_publication(&self, head: &BindingHead) -> ResourceSpec {
        let base = self.binding_base(head);
        let overlay = self.binding_overlay(&base);
        let mut resource = (*base.resource).clone();
        for spec in overlay.iter() {
            match resource.actions.iter_mut().find(|own| own.id == spec.id) {
                Some(own) => *own = spec.clone(),
                None => resource.actions.push(spec.clone()),
            }
        }
        resource
    }

    /// The part of what bound controls look up that only `head` and the
    /// framework decide: the head with the presentation's, the framework's
    /// and History's actions, as [`Self::decorate_publication`] adds them.
    /// Built once per head and kept, so a refresh shares it.
    pub(super) fn binding_base(&self, head: &BindingHead) -> Rc<BindingBase> {
        let key = BaseKey {
            head: Rc::clone(&head.resource),
            locale: head.version.1,
            epoch: head.epoch.clone(),
            causal: self.typed.has_causal_drains(),
            unreadable: self.typed.unreadable(),
        };
        if let Some(base) = self
            .binding_base
            .borrow()
            .as_ref()
            .filter(|base| base.key == key)
        {
            return Rc::clone(base);
        }
        let mut resource = (*head.resource).clone();
        let head_len = resource.actions.len();
        resource = self.with_presentation_specs(resource);
        let shadow_len = resource.actions.len();
        resource = self.with_framework_specs(resource);
        if key.causal {
            mark_document_jobs(&mut resource);
        }
        if let Some(unreadable) = &key.unreadable {
            tilo_resource::withhold_document_actions(&mut resource, unreadable);
        }
        let base = Rc::new(BindingBase {
            key,
            resource: Rc::new(resource),
            head_len,
            shadow_len,
        });
        *self.binding_base.borrow_mut() = Some(Rc::clone(&base));
        base
    }

    /// Refresh the actions bound controls look up, reusing `head` while the
    /// live state and locale are those it was built at. Answers the head
    /// they were refreshed from.
    #[inline(never)]
    pub(super) fn refresh_bindings(&self, head: Option<BindingHead>) -> BindingHead {
        let _profile = tilo_resource::profiling::Phase::start("framework.action_bindings");
        let version = (self.version(), tilo_ui_base::i18n::I18nManager::revision());
        let head = match head {
            Some(head) if head.version == version => head,
            _ => self.binding_head(),
        };
        let base = self.binding_base(&head);
        let overlay = self.binding_overlay(&base);
        // Read while the document did not: the fields these edits were bound
        // to are of the last state that read, not what it holds now.
        let overlay = if self.typed.unreadable().is_some() {
            Rc::new(
                overlay
                    .iter()
                    .filter(|spec| spec.id != tilo_resource::FIELD_EDIT)
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        } else {
            overlay
        };
        self.action_bindings
            .update_shared(Rc::clone(&base.resource), overlay);
        head
    }

    /// [`Self::publication`] of the resource as `doc` holds it now, which a
    /// change from another window or member may have moved past what
    /// this view last synced.
    #[inline(never)]
    pub(super) fn current_publication(&self, doc: &AutoCommit) -> ResourceSpec {
        if !self.follow_document(doc)
            && let Ok(current) = self.typed.read(doc)
        {
            // Publication can run after an edit and before the view syncs:
            // validate its nested views against this same state, not the
            // previous frame's handler table, until the next live revision.
            self.keep_nested_views(self.shared_document_tree(&*current).children());
            let reason = self.typed.unreadable();
            let mut tree = self.publication_of(&*current);
            if let Some(reason) = reason {
                tilo_resource::withhold_document_actions(&mut tree, &reason);
            }
            return tree;
        }
        self.publication()
    }

    /// Whether the live state is `doc`'s, after bringing it up to `doc` when
    /// the document moved on since the view last synced: this window's own
    /// edit before its frame, or another window's or member's change.
    /// The state applies the change, reading only what it touched, and the
    /// view follows it at its next sync ([`Self::refresh`]). Publication runs
    /// on every keystroke between the edit and the frame, and reading the
    /// resource whole there cost each key a read of the whole document. A
    /// document the state's does not lead to (a past version) leaves it as
    /// it is.
    pub(super) fn follow_document(&self, doc: &AutoCommit) -> bool {
        if self.typed.follows(doc) {
            return true;
        }
        if !self.typed.leads_to(doc) {
            return false;
        }
        if let Err(error) = self.typed.sync(doc) {
            tracing::warn!(%error, kind = self.resource.kind, "resource unreadable");
        }
        self.typed.follows(doc)
    }

    #[inline(never)]
    pub(super) fn publication_of(&self, state: &dyn std::any::Any) -> ResourceSpec {
        let _profile = tilo_resource::profiling::Phase::start("framework.publication");
        let tree = self.shared_document_tree(state).to_resource();
        self.decorate_publication(state, tree, Decoration::Tree)
    }

    /// Add to `resource`, the document-owned tree, what the view and the
    /// framework publish on it. [`Decoration::Actions`] decorates only the
    /// resource's own actions; its children stay as they are.
    #[inline(never)]
    pub(super) fn decorate_publication(
        &self,
        state: &dyn std::any::Any,
        mut resource: ResourceSpec,
        decoration: Decoration,
    ) -> ResourceSpec {
        // The name the app gives the state it holds now: what History names a
        // change by, and what the catalogue lists the record as.
        self.dispatch.name_resource(&self.name_of(state));
        let children = decoration == Decoration::Tree;
        let bound = {
            let _profile = tilo_resource::profiling::Phase::start("framework.bound_declarations");
            self.typed.declarations(state)
        };
        let mut rows = Vec::new();
        let mut availability = Vec::new();
        self.surfaces(&mut |surface| {
            let _profile = tilo_resource::profiling::Phase::start("framework.surface_publication");
            availability.extend(surface.availability());
            if children {
                rows.extend(surface.live_rows().into_iter().map(LiveRow::into_resource));
            }
        });
        resource = self.typed.bind(resource, bound);
        for status in availability.iter().filter(|status| status.item().is_none()) {
            for spec in resource
                .actions
                .iter_mut()
                .filter(|spec| spec.id == status.id())
            {
                status.overlay(spec);
            }
        }
        if children {
            merge(&mut resource.children, rows);
            let _profile = tilo_resource::profiling::Phase::start("framework.row_declarations");
            // One walk gathers what the views say of the rows they published
            // beside the document: their edits, an item's presentation from
            // state the document does not hold, and an effect unavailable on
            // one row. A row's status reaches it wherever it is published,
            // nested ones too (a playlist's tracks, the cameras of a
            // disconnected capture row), as its declarations do.
            let mut bindings = self.row_bindings(&resource.children, Some(&resource.kind));
            Self::apply_row_bindings_below(&mut resource.children, &mut bindings);
            self.bind_rows(&mut resource.children, &mut bindings);
            // The nested editors the views opened ([`Dispatcher::child`]):
            // each is published at its own address, with its actions applying
            // as this resource's, so the palette, agents and scripts address
            // it like any resource.
            for (id, nested) in self.dispatch.nested_resources() {
                if !resource
                    .children
                    .iter()
                    .any(|known| known.id.as_str() == id)
                {
                    resource.children.push(nested);
                }
            }
        }
        resource = self.with_presentation_specs(resource);
        // The app's own effects come before the framework's history and
        // render operations: an app answering `diff` its own way is asked.
        let effects = {
            let _profile = tilo_resource::profiling::Phase::start("framework.effect_specs");
            self.typed.effect_specs()
        };
        for spec in effects {
            let shadowed = resource
                .actions
                .iter()
                .any(|existing| existing.id == spec.id);
            debug_assert!(!shadowed, "effect `{}` shadows an action", spec.id);
            if shadowed {
                tracing::warn!(effect = spec.id, "effect shadows an action; not published");
                continue;
            }
            resource = resource.with_action(spec);
        }
        resource = self.with_framework_specs(resource);
        if self.typed.has_causal_drains() {
            mark_document_jobs(&mut resource);
        }
        if let Some(unreadable) = self.typed.unreadable() {
            tilo_resource::withhold_document_actions(&mut resource, &unreadable);
        }
        if children {
            // A record a view asked for and could not open is published
            // with its declared actions unavailable for why: nothing is
            // shown of it, so nothing but those is answered.
            // The record the window holds, kept for the workspace too, is this resource itself:
            // published once, as it.
            let own = resource.id.as_str().to_owned();
            resource.children.extend(
                self.storage
                    .resource_resources()
                    .into_iter()
                    .filter(|beside| beside.id.as_str() != own),
            );
            resource
                .children
                .extend(self.storage.unavailable_resources());
        }
        resource
    }

    /// `resource` with the view and effect operations of the app's
    /// presentation it does not offer itself.
    pub(super) fn with_presentation_specs(&self, mut resource: ResourceSpec) -> ResourceSpec {
        for spec in &self.presentation {
            if spec.kind == tilo_actions::OperationKind::Document {
                continue;
            }
            if resource
                .actions
                .iter()
                .any(|existing| existing.id == spec.id)
            {
                continue;
            }
            resource = resource.with_action(spec.clone());
        }
        resource
    }

    /// `resource` with the framework's render, `read` and History actions it
    /// does not offer itself.
    pub(super) fn with_framework_specs(&self, mut resource: ResourceSpec) -> ResourceSpec {
        let specs = framework_specs();
        let read = self.resource.readable.then_some(&specs.read);
        let facts = self.resource.facts.then_some(&specs.facts);
        let history = specs.history.iter().filter(|_| self.history);
        for spec in specs.native.iter().chain(read).chain(facts).chain(history) {
            if !resource
                .actions
                .iter()
                .any(|existing| existing.id == spec.id)
            {
                resource = resource.with_action(spec.clone());
            }
        }
        // A link to the record opens it in this app, views or not.
        tilo_resource::share::offer_resource(&mut resource);
        self.offer_open_link(resource)
    }

    /// Show why the record does not read as the resource instead of the
    /// view while it does not, never the last state as if it were current,
    /// and refuse submissions with the same reason.
    #[inline(never)]
    pub(super) fn follow_readability(&mut self) {
        let reason = self.typed.unreadable();
        if self.unreadable.as_ref().map(|(shown, _)| shown) == reason.as_ref() {
            return;
        }
        self.dispatch.set_unreadable(reason.clone());
        self.unreadable = reason.map(|reason| {
            let shown: Element = Box::new(unreadable_state(&reason));
            (reason, shown)
        });
        self.swapped = true;
    }

    /// Follow the document when it moved — a local action, a
    /// member's change, an undo or a restore — reading only what the
    /// change touched.
    ///
    /// Answers the document-owned head the bindings were refreshed from, for
    /// the refresh after the view synced to reuse.
    pub(super) fn refresh(&self, doc: &AutoCommit) -> BindingHead {
        if let Err(error) = self.typed.sync(doc) {
            tracing::warn!(%error, kind = self.resource.kind, "resource unreadable");
        }
        // Refresh the same declarations the host publishes before rebuilt
        // controls look up their bindings. Retained surface handles share it.
        let head = self.refresh_bindings(self.retained_head());
        self.typed.follow_change(doc, &self.followed);
        self.feeds.refresh(doc);
        // Work a chain's parts owe is drained as they change too.
        self.typed.drain(
            self.typed
                .revision()
                .wrapping_mul(31)
                .wrapping_add(self.storage.chains_revision()),
        );
        head
    }
}
