//! Answers to broker invocations held until what they caused in the app
//! has landed ([`crate::wake::WakeHandle::owe`]): asked at the end of each
//! sync, never waited for, so the person's own frames go on meanwhile.

use crate::collaboration::CollabRequest;

use super::App;

impl App {
    /// The wake scopes whose owed replies an answer waits for: the
    /// window's, and its component's.
    pub(super) fn owing_scopes(&self) -> Vec<crate::wake::WakeHandle> {
        self.wake_scope
            .as_ref()
            .map(crate::wake::WakeScope::handle)
            .into_iter()
            .chain(
                self.root_widget
                    .callback_binding()
                    .map(|binding| binding.lifetime.clone()),
            )
            .collect()
    }

    /// Answer with the terminal `update` for wire `id` now when nothing is
    /// owed and every reply that landed was taken up by the last sync, else
    /// hold it until that is so ([`Self::answer_held_invocations`]): the
    /// landing reply wakes the window for the sync that answers. Nothing
    /// blocks: the person's own frames go on meanwhile.
    pub(super) fn hold_answer(&mut self, id: u64, update: tilo_actions::InvocationUpdate) {
        let settled = self.actions.held_answers.is_empty()
            && self
                .owing_scopes()
                .iter()
                .all(crate::wake::WakeHandle::settled);
        self.actions.held_answers.push((id, update));
        if settled {
            self.send_held_answers();
        }
    }

    /// Answer with the terminal `update` for wire `id` after the sync its
    /// change asks for has run now, once what that sync caused has landed:
    /// a reply the app asked for in turn is part of the action.
    pub(super) fn answer_after_its_sync(
        &mut self,
        id: u64,
        update: tilo_actions::InvocationUpdate,
    ) {
        self.actions.held_answers.push((id, update));
        self.sync_model(true);
    }

    /// At the end of a sync: send the held answers once nothing is owed and
    /// no reply landed since the previous sync ended, so this one took up
    /// every reply their invocations caused; otherwise the landing reply
    /// wakes the window for a later sync that asks again. Every sync marks
    /// where the next begins, so [`Self::hold_answer`] can ask between them.
    pub(super) fn answer_held_invocations(&mut self) {
        let scopes = self.owing_scopes();
        let settled = scopes.iter().all(crate::wake::WakeHandle::settled);
        for scope in &scopes {
            scope.begin_pass();
        }
        if settled {
            self.send_held_answers();
        }
    }

    /// Send every held answer now, after the document commands they cover:
    /// once settled, or as the window closes and nothing more lands.
    pub(super) fn send_held_answers(&mut self) {
        if self.actions.held_answers.is_empty() {
            return;
        }
        // A terminal must follow the document commands it covers: queue every
        // changed owned record first, or retire the transport as uncertain.
        if !self.queue_documents_before_terminal() {
            self.actions.held_answers.clear();
            return;
        }
        for (id, update) in std::mem::take(&mut self.actions.held_answers) {
            self.collaboration
                .manager
                .send(CollabRequest::UpdateInvocation { id, update });
        }
    }
}
