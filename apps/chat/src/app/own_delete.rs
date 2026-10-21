//! Deleting a message of the person's own at their gesture, with no
//! question asked, and the Undo that takes it back.

use super::{ChatApp, Id};

impl ChatApp {
    /// [`Self::try_apply_part`] of the person's own gesture (a menu choice,
    /// a press) that offers its own way back: nothing is asked, as for a
    /// keystroke. A palette, script or agent supplying the arguments is
    /// still asked first.
    pub(crate) fn try_apply_part_gesture(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        submit: impl ui::framework::Submit<crate::parts::ChatPart>,
    ) -> Result<(), String> {
        let chain = self
            .chain
            .as_ref()
            .ok_or_else(|| "this window holds no messages of the chat".to_owned())?;
        let invoke = submit
            .into_invoke()
            .map_err(|_| "the message could not be put together".to_owned())?;
        #[cfg(test)]
        if let Ok(action) = invoke.build(0) {
            self.dispatched_parts.borrow_mut().push(action.operation);
        }
        self.deleted.take();
        chain
            .dispatcher()
            .submit_gesture(doc, invoke)
            .map_err(|error| error.to_string())
    }

    /// Delete a message of the person's own at their gesture, and offer its
    /// Undo under the composer: the deletion is a step of theirs in the part
    /// it was in, which its history takes back.
    pub(crate) fn delete_own_message(&mut self, doc: &mut ui::widget::Edits<'_>, id: &Id) {
        let message = ui::tilo_resource::Id::from_raw(id);
        let part = self.held_messages().part_of(&message);
        let deleted = self.try_apply_part_gesture(
            doc,
            crate::parts::ChatPart::DELETE_MESSAGE
                .with(crate::messages::chat_part::DeleteMessage { message }),
        );
        if deleted.is_ok() {
            *self.deleted.get_mut() = part.map(|part| (part, self.active_channel.clone()));
        }
    }

    /// Undo beside a deleted message: take back the deletion, the person's
    /// last step in the part it was in.
    pub(crate) fn undo_delete(&self) {
        let (Some((part, _)), Some(chain)) = (self.deleted.take(), self.chain.as_ref()) else {
            return;
        };
        // A refusal brings nothing back; the dispatcher reports it.
        let _ = chain.undo_in(part);
    }
}
