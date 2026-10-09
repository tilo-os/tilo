use crate::invocation_scope::{
    PublishedInvocationAuthorizer, PublishedInvocationLease, PublishedInvocationRequest,
    ResourceInvocationScope,
};

impl super::App {
    pub(super) fn check_published_invocation_revision(
        &mut self,
        lease: &PublishedInvocationLease,
        request: &PublishedInvocationRequest,
    ) -> Result<(), tilo_actions::DoorError> {
        lease.check().map_err(tilo_actions::DoorError::Failed)?;
        let spec = lease
            .dispatch_spec()
            .map_err(tilo_actions::DoorError::Failed)?;
        self.check_published_invocation_spec(spec, request)
    }

    pub(super) fn check_published_invocation_spec(
        &mut self,
        spec: &crate::invocation_scope::PublishedInvocationSpec,
        request: &PublishedInvocationRequest,
    ) -> Result<(), tilo_actions::DoorError> {
        let refused = |reason: String| tilo_actions::DoorError::Unavailable {
            code: "published_invocation_revision_changed".into(),
            reason,
        };
        if let Some(binding) = &spec.fresh_binding {
            if spec.directory_revision.is_some() {
                return Err(refused(
                    "fresh and revision-bound modes cannot be mixed".into(),
                ));
            }
            let expected = &binding.descriptor;
            if expected.address != request.address || expected.semantic_identity.is_none() {
                return Err(refused(
                    "fresh operation has no exact semantic binding".into(),
                ));
            }
            let resource = self
                .resolve_publication_resource(&request.address.resource, &request.address.action)
                .ok_or_else(|| refused("fresh operation is no longer published".into()))?;
            let action = resource
                .actions
                .iter()
                .find(|action| action.id == request.address.action)
                .ok_or_else(|| refused("fresh operation is no longer published".into()))?;
            let current = Self::directory_descriptor(&resource, action);
            if !action.admits_fresh_binding()
                || action.operation_type != binding.operation_type
                || &current != expected
                || current.kind != spec.kind
                || current.contract != spec.contract
                || current.params != spec.params
                || current.input != spec.input
            {
                return Err(refused("fresh operation public binding changed".into()));
            }
            // The existing dispatch below binds arguments and builds this
            // current operation exactly once, in this same invoker scope.
            // Comparing a public binding never executes an old callback.
            return Ok(());
        }
        if let Some(original) = &spec.directory_revision {
            let current = self.try_action_directory_revision().map_err(refused)?;
            if original != &current {
                return Err(refused(
                    "current directory differs from the original admitted revision".into(),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn published_invocation_scope(
        &self,
        address: &tilo_actions::ActionAddress,
        lease: Option<PublishedInvocationLease>,
    ) -> Result<Option<ResourceInvocationScope>, tilo_actions::DoorError> {
        let Some(lease) = lease else { return Ok(None) };
        use crate::bound_dispatch::legacy_document::{LocalResourceTarget, select_local_target};
        let scope = if let Some(target) = self
            .actions
            .bound_publication
            .target(address)
            .map_err(crate::bound_dispatch::BoundActionError::into_door)?
        {
            ResourceInvocationScope::for_bound_target(target, lease)
        } else {
            match select_local_target(self.root_widget.as_ref(), &self.state.doc, address)? {
                LocalResourceTarget::Component(target) => {
                    ResourceInvocationScope::for_bound_target(target, lease)
                }
                LocalResourceTarget::Legacy(target) => {
                    ResourceInvocationScope::for_owner(target.owner, lease)
                }
                LocalResourceTarget::Outer => {
                    return Err(tilo_actions::DoorError::Failed(
                        "required invocation has no canonical record owner".into(),
                    ));
                }
            }
        };
        scope.map(Some).map_err(tilo_actions::DoorError::Failed)
    }
    pub(crate) fn require_published_invocation_authorizer(&mut self) {
        self.actions.published_invocations.required = true;
    }

    pub(crate) fn install_published_invocation_authorizer(
        &mut self,
        authorizer: PublishedInvocationAuthorizer,
    ) -> Result<(), String> {
        let runtime = &mut self.actions.published_invocations;
        if !runtime.required || runtime.authorizer.is_some() || runtime.failure.is_some() {
            return Err("published invocation authorizer cannot be replaced".into());
        }
        runtime.authorizer = Some(authorizer);
        Ok(())
    }

    pub(crate) fn published_invocation_health(&self) -> Result<(), String> {
        let runtime = &self.actions.published_invocations;
        if let Some(reason) = &runtime.failure {
            return Err(reason.clone());
        }
        if runtime.required && runtime.authorizer.is_none() {
            return Err("required published invocation authorizer is unavailable".into());
        }
        Ok(())
    }

    pub(super) fn authorize_published_invocation(
        &self,
        request: &PublishedInvocationRequest,
    ) -> Result<Option<PublishedInvocationLease>, tilo_actions::DoorError> {
        let refused = |reason: String| tilo_actions::DoorError::Unavailable {
            code: "published_invocation_unauthorized".into(),
            reason,
        };
        self.published_invocation_health().map_err(refused)?;
        let runtime = &self.actions.published_invocations;
        if !runtime.required {
            return Ok(None);
        }
        let authorizer = runtime.authorizer.as_ref().ok_or_else(|| {
            refused("required published invocation authorizer is unavailable".into())
        })?;
        let lease = authorizer(request).map_err(refused)?;
        if !lease.matches(request) {
            return Err(refused(
                "published invocation lease belongs to different inputs".into(),
            ));
        }
        lease.dispatch_spec().map_err(refused)?;
        lease.check().map_err(refused)?;
        Ok(Some(lease))
    }

    pub(super) fn check_published_continuation(
        &self,
        lease: Option<&ResourceInvocationScope>,
    ) -> Result<(), String> {
        let result = self
            .published_invocation_health()
            .and_then(|()| match lease {
                Some(lease) => lease.check(),
                None if self.actions.published_invocations.required => {
                    Err("accepted invocation has no original caller lease".into())
                }
                None => Ok(()),
            });
        // A denied explicit job scope is permanently retired. Other admitted
        // jobs retain their own custody and continue independently.
        result
    }
}
