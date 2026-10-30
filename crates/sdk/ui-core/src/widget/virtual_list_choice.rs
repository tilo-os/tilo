//! Choosing a row, published as the list's view action `select`
//! ([`VirtualList::on_view`]): what the host and agents reach of it.

use super::{ListModel, VirtualList};
use crate::widget::Widget;

impl<M: ListModel> VirtualList<M> {
    /// Called with the row chosen: the list, as control `id`, publishes
    /// choosing a row as its view action `select`
    /// (`view_choice`), each row addressed by
    /// its semantic key ([`Self::with_semantic_key`]), so a person's click,
    /// a screen reader, a script and an agent choose through the same
    /// action. What is chosen is the window's own state, never an edit.
    #[must_use]
    pub fn on_view(mut self, id: impl Into<String>, f: impl FnMut(M::Key) + 'static) -> Self {
        let id = id.into();
        self.test_id = Some(id.clone());
        self.view = Some((
            crate::widget::view_choice::ViewChoice::new(id, "Select"),
            Box::new(f),
        ));
        self
    }

    /// Called with the row opened: the list, as control `id`, publishes
    /// opening a row as its view action `open`, each row addressed by its
    /// semantic key ([`Self::with_semantic_key`]), so a person's second
    /// click or Enter, a screen reader, a script and an agent open through
    /// the same action. What is shown is the window's own state, never an
    /// edit.
    #[must_use]
    pub fn on_activate_view(
        mut self,
        id: impl Into<String>,
        f: impl FnMut(M::Key) + 'static,
    ) -> Self {
        let id = id.into();
        self.opens = Some((
            crate::widget::view_choice::ViewChoice::new(format!("{id}/open"), "Open"),
            Box::new(f),
        ));
        self
    }

    /// The rows an agent may choose, by semantic key alone (a renamed row
    /// leaves what the list publishes as it was); as many as an enumeration
    /// carries.
    fn entries(&self) -> Vec<String> {
        const MAX_ENTRIES: usize = 500;
        let Some(encode) = self.semantic_key.as_ref() else {
            return Vec::new();
        };
        (0..self.model.len().min(MAX_ENTRIES))
            .filter_map(|index| self.model.key_at(index).map(|key| encode(&key)))
            .collect()
    }
}

impl<M: ListModel + 'static> VirtualList<M> {
    /// Answer a `select` or an `open` addressed to this list: show the row
    /// addressed and report it, whoever chose it.
    fn start_view(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        let chosen = self
            .view
            .as_ref()
            .map(|(choice, _)| choice.start(op))
            .transpose()?
            .flatten()
            .map(|entry| (entry, false));
        let opened = self
            .opens
            .as_ref()
            .map(|(choice, _)| choice.start(op))
            .transpose()?
            .flatten()
            .map(|entry| (entry, true));
        let Some((entry, opens)) = chosen.or(opened) else {
            return Ok(None);
        };
        let encode = self
            .semantic_key
            .as_ref()
            .ok_or_else(|| "the list names no rows".to_owned())?;
        let key = (0..self.model.len())
            .filter_map(|index| self.model.key_at(index))
            .find(|key| encode(key) == entry)
            .ok_or_else(|| format!("no entry `{entry}`"))?;
        self.selection.select_only(key.clone());
        self.focused_key = Some(key.clone());
        if opens {
            if let Some((_, handler)) = self.opens.as_mut() {
                handler(key);
            }
        } else if let Some((_, handler)) = self.view.as_mut() {
            handler(key);
        }
        Ok(Some(crate::widget::view_choice::chosen()))
    }
}

impl<M: ListModel + 'static> crate::widget::ResourceOperations for VirtualList<M> {
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

impl<M: ListModel + 'static> crate::resources::PublishesResources for VirtualList<M> {
    fn resources(&self, _doc: &automerge::AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        let rows = || self.entries();
        self.view
            .iter()
            .map(|(choice, _)| choice.resource_of_keys(rows()))
            .chain(
                self.opens
                    .iter()
                    .map(|(choice, _)| choice.resource_of_keys(rows())),
            )
            .collect()
    }
}

impl<M: ListModel + 'static> crate::widget::controls::ActionControls for VirtualList<M> {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        for (choice, _) in self.view.iter_mut().chain(self.opens.iter_mut()) {
            choice.install_custody(domain, attachment);
        }
        for child in Widget::children_mut(self) {
            crate::widget::controls::install_control_custody(child, domain, attachment);
        }
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        if let Some(invocation) = self
            .view
            .as_mut()
            .and_then(|(choice, _)| choice.take())
            .or_else(|| self.opens.as_mut().and_then(|(choice, _)| choice.take()))
        {
            return Some(invocation);
        }
        Widget::children_mut(self)
            .into_iter()
            .find_map(crate::widget::controls::take_control_invocation)
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        for (choice, _) in self.view.iter_mut().chain(self.opens.iter_mut()) {
            choice.refresh_availability(allowed);
        }
        for child in Widget::children_mut(self) {
            crate::widget::controls::refresh_control_action_availability(child, allowed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Theme;
    use crate::widget::{Element, VecListModel};
    use std::cell::RefCell;
    use std::rc::Rc;

    fn plain_rows(count: u32) -> VirtualList<VecListModel<u32, u32>> {
        VirtualList::from_items(
            (0..count).collect(),
            40.0,
            |item| *item,
            |item, _, _| Box::new(crate::widget::Text::new(&format!("row {item}"))) as Element,
        )
        .unwrap()
        .with_viewport_height(200.0)
    }

    /// Choosing a row is the list's published view action, by the rows'
    /// semantic keys: an agent's choice selects the row and reaches the app.
    #[test]
    fn an_agent_chooses_a_row_through_the_published_view_action() {
        let chosen = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::clone(&chosen);
        let list = plain_rows(10)
            .with_semantic_key(ToString::to_string)
            .on_view("numbers", move |key| seen.borrow_mut().push(key));
        let mut h = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            list,
            (300, 300),
            Theme::dark(),
        );
        let entry =
            |key: &str| serde_json::Map::from_iter([("entry".into(), serde_json::json!(key))]);
        h.invoke_published("tilo.control:numbers", "select", &entry("7"))
            .expect("an agent chooses a row");
        assert_eq!(chosen.borrow().as_slice(), [7]);
        assert!(
            h.root::<VirtualList<VecListModel<u32, u32>>>()
                .selection()
                .is_selected(&7)
        );
        assert!(
            h.invoke_published("tilo.control:numbers", "select", &entry("70"))
                .is_err()
        );
    }
}
