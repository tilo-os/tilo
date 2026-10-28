//! The parts the shell hands to the window's bar publish and run their
//! actions through it.

use super::NavShell;

/// A header button the shell hands to the window's bar is not among the
/// shell's laid-out children, yet a press on it in the bar runs it: it
/// publishes its action, and the operation the press starts reaches it.
impl crate::resources::PublishesResources for NavShell {
    fn resources(&self, doc: &automerge::AutoCommit) -> Vec<tilo_actions::Resource> {
        let mut resources = Vec::new();
        for part in self.bar_parts() {
            if let Some(widget) = self.part(part) {
                crate::widget::collect_widget_resources(widget, doc, &mut resources);
            }
        }
        resources
    }
}

impl crate::widget::ResourceOperations for NavShell {
    fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
        let mut handlers = crate::widget::resource_operation::subtree_handlers(
            self,
            crate::widget::resource_operation::local_resource_operation_handlers(self),
        );
        for part in self.bar_parts() {
            if let Some(widget) = self.part(part) {
                handlers
                    .extend(crate::widget::resource_operation::resource_operation_handlers(widget));
            }
        }
        handlers
    }

    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        if let Some(started) =
            crate::widget::resource_operation::start_by_default(self, op, doc, path)?
        {
            return Ok(Some(started));
        }
        let parts = self.bar_parts();
        crate::widget::resource_operation::start_in(self.parts_mut(&parts), op, doc, path)
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        doc: &automerge::AutoCommit,
        path: &automerge::ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if let Some(update) = crate::widget::resource_operation::poll_by_default(
            self,
            invocation,
            cancellation_requested,
            doc,
            path,
        ) {
            return Some(update);
        }
        let parts = self.bar_parts();
        self.parts_mut(&parts).into_iter().find_map(|widget| {
            crate::widget::resource_operation::poll_resource_operation(
                widget,
                invocation,
                cancellation_requested,
                doc,
                path,
            )
        })
    }
}
