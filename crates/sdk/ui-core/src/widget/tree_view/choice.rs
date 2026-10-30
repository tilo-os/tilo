//! Choosing a node, published as the tree's view action `select`
//! ([`TreeImpl::on_view`]): what the host and agents reach of it.

use super::{NodeId, TreeImpl, TreeModel, Widget};
use crate::widget::ActionEmitter;

/// A tree's published choice and the app's handler.
pub(super) struct TreeChoice {
    pub(super) choice: crate::widget::view_choice::ViewChoice,
    pub(super) handler: Box<dyn FnMut(NodeId)>,
}

impl<T, M: TreeModel> TreeImpl<T, M> {
    /// Called with the node chosen: the tree, as control `id`, publishes
    /// choosing a node as its view action `select`
    /// ([`view_choice`](crate::widget::view_choice)), so a person's click, a
    /// screen reader, a script and an agent choose through the same action.
    /// What is chosen is the window's own state, never an edit.
    #[must_use]
    pub fn on_view(mut self, id: impl Into<String>, f: impl FnMut(NodeId) + 'static) -> Self {
        let id = id.into();
        self.test_id = Some(id.clone());
        self.view = Some(TreeChoice {
            choice: crate::widget::view_choice::ViewChoice::new(id, "Select"),
            handler: Box::new(f),
        });
        self
    }

    /// Report a node the person chose: the published choice's own run (no
    /// host decides a view or records it), else [`Self::on_select`].
    pub(super) fn report_selected(&mut self, id: NodeId) {
        if let Some(view) = self.view.as_mut() {
            (view.handler)(id);
        } else if let Some(callback) = self.on_select.as_mut() {
            callback(id);
        }
    }
}

impl<T, M: TreeModel> TreeImpl<T, M> {
    /// The key a node is chosen by: its durable semantic key, else its id.
    pub(super) fn entry_key(&self, id: NodeId) -> String {
        self.model
            .semantic_key(id)
            .unwrap_or_else(|| id.to_string())
    }
}

impl<T: 'static, M: TreeModel + 'static> TreeImpl<T, M> {
    /// Every resident selectable node under the root, by key. Keys alone:
    /// a renamed node leaves what the tree publishes as it was.
    fn entries(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![self.model.root_id()];
        while let Some(id) = stack.pop() {
            if self.model.is_selectable(id) && self.model.label(id).is_some_and(|l| !l.is_empty()) {
                out.push(self.entry_key(id));
            }
            let count = self.model.child_count(id);
            stack.extend(
                (0..count)
                    .rev()
                    .filter_map(|index| self.model.child_id(id, index)),
            );
        }
        out
    }
}

impl<T: 'static, M: TreeModel + 'static> TreeImpl<T, M> {
    /// Answer a `select` or an `open` addressed to this tree: show the node
    /// addressed and report it, whoever chose it.
    fn start_view(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        let chosen = self
            .view
            .as_ref()
            .map(|view| view.choice.start(op))
            .transpose()?
            .flatten()
            .map(|key| (key, false));
        let opened = self
            .opens
            .as_ref()
            .map(|opens| opens.choice.start(op))
            .transpose()?
            .flatten()
            .map(|key| (key, true));
        let Some((key, opens)) = chosen.or(opened) else {
            return Ok(None);
        };
        let mut stack = vec![self.model.root_id()];
        let mut found = None;
        while let Some(id) = stack.pop() {
            if self.entry_key(id) == key && self.model.is_selectable(id) {
                found = Some(id);
                break;
            }
            let count = self.model.child_count(id);
            stack.extend((0..count).filter_map(|index| self.model.child_id(id, index)));
        }
        let id = found.ok_or_else(|| format!("no entry `{key}`"))?;
        self.selection.select_only(id);
        self.reveal(id);
        let opened = if opens {
            self.opens.as_mut()
        } else {
            self.view.as_mut()
        };
        if let Some(choice) = opened {
            (choice.handler)(id);
        }
        Ok(Some(crate::widget::view_choice::chosen()))
    }
}

impl<T: 'static, M: TreeModel + 'static> crate::widget::ResourceOperations for TreeImpl<T, M> {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        let history = crate::widget::resource_operation::local_history_handlers(self);
        match (
            self.view.is_some() || self.opens.is_some(),
            history.is_empty(),
        ) {
            (false, _) => history,
            (true, true) => &[crate::widget::view_choice::CHOICE_SELECT],
            (true, false) => &[
                crate::widget::view_choice::CHOICE_SELECT,
                crate::undo::EDIT_OPERATION,
            ],
        }
    }

    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        if let Some(started) = self.start_view(op)? {
            return Ok(Some(started));
        }
        crate::widget::resource_operation::start_by_default(self, op, doc, path)
    }
}

impl<T: 'static, M: TreeModel + 'static> crate::resources::PublishesResources for TreeImpl<T, M> {
    fn resources(&self, _doc: &automerge::AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        self.view
            .iter()
            .chain(self.opens.iter())
            .map(|view| view.choice.resource_of_keys(self.entries()))
            .collect()
    }
}

impl<T: 'static, M: TreeModel + 'static> crate::widget::controls::ActionControls
    for TreeImpl<T, M>
{
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        for view in self.view.iter_mut().chain(self.opens.iter_mut()) {
            view.choice.install_custody(domain, attachment);
        }
        if let Some(emitter) = self.rename_action.as_mut() {
            let _ = emitter.install_custody(domain, attachment);
        }
        if let Some(emitter) = self.delete_action.as_mut() {
            let _ = emitter.install_custody(domain, attachment);
        }
        for emitter in self.row_action_actions.iter_mut().flatten() {
            let _ = emitter.install_custody(domain, attachment);
        }
        for child in Widget::children_mut(self) {
            crate::widget::controls::install_control_custody(child, domain, attachment);
        }
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        if let Some(invocation) = self
            .view
            .iter_mut()
            .chain(self.opens.iter_mut())
            .find_map(|view| view.choice.take())
        {
            return Some(invocation);
        }
        if let Some(invocation) = self.rename_action.as_mut().and_then(ActionEmitter::take) {
            return Some(invocation);
        }
        if let Some(invocation) = self.delete_action.as_mut().and_then(ActionEmitter::take) {
            return Some(invocation);
        }
        if let Some(invocation) = self
            .row_action_actions
            .iter_mut()
            .flatten()
            .find_map(ActionEmitter::take)
        {
            return Some(invocation);
        }
        Widget::children_mut(self)
            .into_iter()
            .find_map(crate::widget::controls::take_control_invocation)
    }

    /// The rows the tree shows, bound to its published choice.
    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        let mut bindings = crate::widget::controls::default_bindings(self);
        if let Some(view) = self.view.as_ref().or(self.opens.as_ref()) {
            for binding in &mut bindings {
                if binding.control_id.starts_with("node:")
                    && matches!(
                        binding.kind,
                        crate::widget::ControlBindingKind::SemanticAction(_)
                    )
                {
                    *binding = view.choice.metadata(binding.control_id.clone());
                }
            }
        }
        bindings
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        for view in self.view.iter_mut().chain(self.opens.iter_mut()) {
            view.choice.refresh_availability(allowed);
        }
        if let Some(emitter) = self.rename_action.as_mut() {
            emitter.refresh_availability(allowed);
        }
        if let Some(emitter) = self.delete_action.as_mut() {
            emitter.refresh_availability(allowed);
        }
        for emitter in self.row_action_actions.iter_mut().flatten() {
            emitter.refresh_availability(allowed);
        }
        for child in Widget::children_mut(self) {
            crate::widget::controls::refresh_control_action_availability(child, allowed);
        }
    }
}
