use crate::invocation_scope::{
    PublishedInvocationAuthorizer, PublishedInvocationLease, PublishedInvocationRequest,
    ResourceInvocationScope,
};

impl super::App {
    pub(super) fn check_published_invocation_revision(
        &mut self,
        lease: &PublishedInvocationLease,
    ) -> Result<(), tilo_actions::DoorError> {
        let refused = |reason: String| tilo_actions::DoorError::Unavailable {
            code: "published_invocation_revision_changed".into(),
            reason,
        };
        if let Some(original) = &lease.dispatch_spec().map_err(refused)?.directory_revision {
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
        let owner = if let Some(target) = self
            .actions
            .bound_publication
            .target(address)
            .map_err(crate::bound_dispatch::BoundActionError::into_door)?
        {
            crate::history_mode::HistoryOwner::component(target.binding)
        } else {
            match select_local_target(self.root_widget.as_ref(), &self.state.doc, address)? {
                LocalResourceTarget::Component(target) => {
                    crate::history_mode::HistoryOwner::component(target.binding)
                }
                LocalResourceTarget::Legacy(target) => target.owner,
                LocalResourceTarget::Outer => {
                    return Err(tilo_actions::DoorError::Failed(
                        "required invocation has no canonical record owner".into(),
                    ));
                }
            }
        };
        ResourceInvocationScope::for_owner(owner, lease)
            .map(Some)
            .map_err(tilo_actions::DoorError::Failed)
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
