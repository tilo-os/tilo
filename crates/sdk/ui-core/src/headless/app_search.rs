//! Standalone app search uses the native ActionPalette, including its retained
//! providers, scopes and exact action owner. Embedded apps hand requests out.
use super::FrameHarness;
use crate::action_palette::{ActionPalette, PendingOperation};
use crate::bound_dispatch::legacy_document::LocalResourceTarget;
use crate::bound_dispatch::{CallbackBinding, OrdinaryConfirmation};
use crate::search::{ScopeDecl, SearchRequest};

impl FrameHarness {
    /// Query in the standalone app's search/command presenter.
    pub fn app_search_query(&self) -> Option<&str> {
        self.action_palette
            .as_deref()
            .filter(|p| p.is_presented())
            .map(ActionPalette::query)
    }

    /// None means the whole app; Some names the request's resource scope.
    pub fn app_search_scope(&self) -> Option<&ScopeDecl> {
        self.action_palette
            .as_deref()
            .filter(|p| p.is_presented())
            .and_then(ActionPalette::scope)
    }

    pub(super) fn take_search_invocation(
        &mut self,
        intent: &tilo_actions::ControlInvocation,
        owner: Option<&CallbackBinding>,
    ) -> bool {
        let Some(request) = SearchRequest::from_invocation(intent) else {
            return false;
        };
        // from_component owns one standalone application, unlike a harness
        // embedding tiles. Never find a different owner from the action address.
        if let Some(binding) = owner
            && !self.component_wake.as_ref().map_or_else(
                || self.document_history.has_search(),
                |wake| wake.binding.same_component(binding),
            )
        {
            binding.request_search(request);
        } else if let Err(error) = self.open_app_search(request) {
            tracing::warn!(%error, "headless app search refused");
        }
        true
    }

    /// Open the same scoped resource/provider palette as a native app window.
    pub(super) fn open_app_search(&mut self, request: SearchRequest) -> Result<(), String> {
        if self.action_palette.is_some()
            || self.global_search_query().is_some()
            || self.global_search_pending()
            || self.caller_palette.palette.is_some()
            || self.caller_palette.pending.is_some()
            || self.root.modal_capture().is_some()
            || self.focus_trap_active
            || self.history.as_ref().is_some_and(|h| h.captures_input())
        {
            return Err("Finish the current dialog or command first".into());
        }
        if !crate::search::system::query_input_is_bounded(request.query.as_deref().unwrap_or("")) {
            return Err("Search query is too long".into());
        }
        let mut resources = Vec::new();
        crate::bound_dispatch::collect_unbound_resources(
            self.root.as_ref(),
            &self.state.doc,
            &mut resources,
        );
        let app_id = crate::collaboration::local_app_id();
        let mut resources: Vec<_> = resources
            .into_iter()
            .filter_map(|resource| self.permissions.permitted_resource(&app_id, resource))
            .collect();
        let legacy = crate::bound_dispatch::legacy_document::legacy_resources(
            self.root.as_ref(),
            &self.permissions,
            &self.author,
            false,
        )
        .map_err(|error| error.to_string())?;
        resources.extend(legacy);
        let bound = crate::bound_dispatch::discover_bound_resources(
            self.root.as_ref(),
            &self.state.doc,
            None,
        )
        .map_err(|error| {
            crate::bound_dispatch::BoundActionError::Dispatch(error)
                .into_door()
                .to_string()
        })?;
        let mut providers = crate::bound_dispatch::search_providers(self.root.as_ref());
        providers.push(Box::new(crate::search::OpenResourceSearch::new(&resources)));
        let mut palette = Box::new(ActionPalette::with_host_text(
            self.text.font_system.locale().to_owned(),
        ));
        palette.load_open_resources(&resources);
        palette.load_bound_resources(bound);
        palette.set_providers(providers);
        palette.open();
        palette.set_scope(request.resource_scope().cloned());
        if let Some(query) = &request.query {
            palette.set_query(query);
            palette.select_query();
        }
        self.discard_layout_tree();
        self.focused = None;
        self.action_palette = Some(palette);
        Ok(())
    }

    /// One bounded page/validation step per production frame, matching native.
    pub(super) fn advance_app_search(&mut self) {
        self.app_search_pending = self
            .action_palette
            .as_deref_mut()
            .filter(|palette| palette.is_presented())
            .is_some_and(|palette| palette.advance_search_page().pending);
    }

    pub(super) fn prepare_palette_request(
        &self,
        pending: &PendingOperation,
    ) -> Result<OrdinaryConfirmation, tilo_actions::DoorError> {
        if let Some(request) = &pending.ordinary {
            return Ok(request.clone());
        }
        if pending.history_target.is_some() {
            return Err(tilo_actions::DoorError::InvalidArguments(
                "Missing original History confirmation request".into(),
            ));
        }
        let target = match pending.bound.clone() {
            Some(target) => LocalResourceTarget::Component(target),
            None => crate::bound_dispatch::legacy_document::select_local_target(
                self.root.as_ref(),
                &self.state.doc,
                &pending.address,
            )?,
        };
        let arguments = pending.continuation_arguments().ok_or_else(|| {
            tilo_actions::DoorError::InvalidArguments("Invalid palette arguments".into())
        })?;
        OrdinaryConfirmation::prepare(
            self.root.as_ref(),
            &self.state.doc,
            target,
            &self.control_domain,
            pending.address.clone(),
            arguments,
        )
    }

    pub(super) fn capture_app_search_request(&mut self) {
        let Some(pending) = self
            .action_palette
            .as_deref()
            .and_then(ActionPalette::ordinary_capture_request)
        else {
            return;
        };
        let result = self.prepare_palette_request(&pending);
        if let Some(palette) = self.action_palette.as_deref_mut() {
            match result {
                Ok(request) => palette.retain_ordinary(request),
                Err(_) => palette.close(),
            }
        }
    }

    pub(super) fn finish_app_search_navigation(&mut self) {
        let navigation = self
            .action_palette
            .as_deref_mut()
            .and_then(ActionPalette::take_scoped_navigation);
        if let Some((id, binding)) = navigation {
            if let Some(binding) = binding {
                let _ = crate::bound_dispatch::activate_bound_search(
                    self.root.as_mut(),
                    &mut self.state.doc,
                    &binding,
                    &id,
                );
            } else {
                let before = self.state.doc.get_heads();
                crate::search::capability::activate_search_result(
                    self.root.as_mut(),
                    &id,
                    &mut self.state.doc,
                    &self.state.root,
                );
                if self.state.doc.get_heads() != before {
                    self.state.record_committed_action(before, None, None);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
