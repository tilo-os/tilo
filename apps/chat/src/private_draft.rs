//! Adoption of a private editor, never a message send or a public room mutation.
use crate::app::ChatApp;
use ui::framework::{EffectAnswer, Effected};

impl ChatApp {
    pub(crate) fn adopt_private_draft(
        &mut self,
        token: &str,
        invocation: tilo_actions::InvocationId,
    ) -> EffectAnswer {
        let token = uuid::Uuid::parse_str(token).map_err(|_| "invalid private draft token")?;
        let draft = ui::private_draft::fetch(token, invocation)?;
        // A draft is taken only by the Chat of the room it was written in.
        let room = ui::collaboration::local_cluster_id();
        if room.is_nil()
            || uuid::Uuid::parse_str(&draft.workspace).ok() != Some(room)
            || self.rect.area() <= 0.0
        {
            return Err("private draft room or Chat view unavailable".into());
        }
        let ui::private_draft::Target::Person { public_key } = &draft.target;
        self.open_person_conversation(public_key)?;
        // The native host opens at once; a draft never waits for a page's room.
        if self.direct_person.as_deref() != Some(public_key.as_str()) {
            return Err("private conversation is not open yet".into());
        }
        let conversation = Self::person_conversation_key(public_key);
        // A desktop-hosted conversation already owns its retained editor.
        // Adopt there, not into a hidden second parent editor sharing its store.
        if let Some(surface) = self.agent_surfaces.get_mut(&self.active_channel)
            && !matches!(
                surface.placement.attachment(),
                ui::floating_surface::Attachment::App { .. }
            )
        {
            if surface.is_closed()
                || surface.conversation != conversation
                || !surface.view.main_composer_can_edit()
            {
                return Err("retained private conversation is unavailable".into());
            }
            surface
                .view
                .composer_main
                .adopt_private_editor(&conversation, &draft.editor)?;
            surface.view.draft_main.clone_from(&draft.editor.text);
            surface.view.request_relayout();
            self.leave_floated_agent_route();
            ui::private_draft::acknowledge(token, invocation)?;
            self.request_relayout();
            return Ok(Effected::value(
                serde_json::json!({"adopted":token.to_string()}),
            ));
        }
        if !self.main_composer_can_edit() {
            return Err("private conversation is no longer editable".into());
        }
        self.composer_main
            .adopt_private_editor(&conversation, &draft.editor)?;
        self.draft_main.clone_from(&draft.editor.text);
        // The standard surface moves this exact editor, selection and preedit.
        // Never fabricate a pending transfer or create a second editor/window.
        if !self.float_agent_conversation() {
            return Err("private editor retained in Chat; floating surface unavailable".into());
        }
        // An uncertain reply preserves the Shell draft even though Chat also
        // retains its private copy. No failure path sends or discards either.
        ui::private_draft::acknowledge(token, invocation)?;
        self.request_relayout();
        Ok(Effected::value(
            serde_json::json!({"adopted": token.to_string()}),
        ))
    }
}
