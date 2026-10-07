//! Exact mounted-owner preparation for supplemental native Message capture.
use crate::invocation_scope::{
    NativeInvocationSource, NativeMessageCapture, PublishedInvocationRequest,
};

impl super::App {
    pub(super) fn finish_native_message_capture(
        &mut self,
        capture: Option<NativeMessageCapture>,
        result: Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        let Some(capture) = capture else {
            return result;
        };
        let route = capture.route().clone();
        let changed = capture.has_changes();
        if result.is_err() {
            // AppliedLocal can precede a failed action-history/follow-up
            // boundary. Preserve its local truth, but never turn that failed
            // execution into new private Agent input.
            if changed {
                self.block_native_message_publication(&route);
            }
            return result;
        }
        let publication = capture.finish().and_then(|contribution| {
            self.collaboration
                .manager
                .try_send_to_route(
                    route.clone(),
                    crate::collaboration::CollabRequest::NativeExecutedContribution {
                        contribution,
                    },
                )
                .map_err(|_| {
                    "native Message execution could not enter its exact route queue".into()
                })
        });
        if let Err(reason) = publication {
            if changed {
                self.block_native_message_publication(&route);
            }
            return result.and(Err(tilo_actions::DoorError::Unavailable {
                code: "native_message_publication_refused".into(),
                reason,
            }));
        }
        result
    }

    pub(super) fn prepare_native_message_capture(
        &mut self,
        request: &PublishedInvocationRequest,
    ) -> Result<Option<NativeMessageCapture>, tilo_actions::DoorError> {
        let Some(source) = NativeInvocationSource::current_delivery() else {
            return Ok(None);
        };
        let refused = |reason: String| tilo_actions::DoorError::Unavailable {
            code: "native_message_execution_refused".into(),
            reason,
        };
        source.matches(request).map_err(refused)?;
        if let Some(original) = &source.spec().directory_revision {
            let current = self.try_action_directory_revision().map_err(refused)?;
            if original != &current {
                return Err(refused("native Message directory revision changed".into()));
            }
        }
        use crate::bound_dispatch::legacy_document::{LocalResourceTarget, select_local_target};
        let owner = if let Some(target) = self
            .actions
            .bound_publication
            .target(&request.address)
            .map_err(crate::bound_dispatch::BoundActionError::into_door)?
        {
            crate::history_mode::HistoryOwner::component(target.binding)
        } else {
            match select_local_target(self.root_widget.as_ref(), &self.state.doc, &request.address)?
            {
                LocalResourceTarget::Component(target) => {
                    crate::history_mode::HistoryOwner::component(target.binding)
                }
                LocalResourceTarget::Legacy(target) => target.owner,
                LocalResourceTarget::Outer => {
                    return Err(refused(
                        "native Message has no canonical record owner".into(),
                    ));
                }
            }
        };
        NativeMessageCapture::new(owner, source)
            .map(Some)
            .map_err(refused)
    }

    /// Invoke this before ANY document command after a failed claimed capture.
    /// Unknown Person edits never enter this method. A real locally applied
    /// Message cannot be laundered through the ordinary document-send door.
    pub(super) fn block_native_message_publication(
        &mut self,
        route: &crate::collaboration::RouteSpec,
    ) {
        self.refresh_document_routes();
        let reason = "Addressed Message execution could not retain its original input publication. Changes remain on this device; reconciliation is required.";
        if let Some(entry) = self
            .document_routes
            .owners
            .iter_mut()
            .find(|entry| &entry.route == route)
        {
            entry.pending.failure = Some(reason.into());
        } else {
            // A mounted owner that disappears after local apply cannot safely
            // resume as another record owner or an ordinary Person edit.
            self.action_publication_failure = Some(reason.into());
            self.collaboration.manager.stop_after_publication_failure();
        }
        self.layout.layout_dirty = true;
    }
}
