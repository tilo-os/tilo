//! Which composer owns the keyboard — the main one, the thread's or an
//! inline message editor — and the presence a draft publishes while it is
//! being written.

use super::{ChatApp, Focus, Id};

impl ChatApp {
    /// The composer for the surface the user is typing in.
    pub(crate) fn focused_composer(&self) -> &crate::composer::DraftComposer {
        match self.focus {
            Focus::Thread if self.thread_open.is_some() => &self.composer_thread,
            _ => &self.composer_main,
        }
    }

    pub(crate) fn focused_composer_mut(&mut self) -> &mut crate::composer::DraftComposer {
        match self.focus {
            Focus::Thread if self.thread_open.is_some() => &mut self.composer_thread,
            _ => &mut self.composer_main,
        }
    }

    /// Point keyboard focus at the composer for the current surface — but only
    /// when no inline editor / rename is capturing input. Idempotent; called
    /// from `reload` and after any focus change.
    pub(crate) fn set_composer_focus(&mut self) {
        let editing = self.editing.is_some() || self.channel_sheet.is_some() || !self.host_focused;
        // Without an open channel the main composer has nowhere to send, so
        // it takes no keys.
        let has_target = self.channel(&self.active_channel).is_some();
        let main = matches!(self.focus, Focus::Main) && !editing && has_target;
        let thread = matches!(self.focus, Focus::Thread) && self.thread_open.is_some() && !editing;
        self.composer_main.set_focus(main);
        self.composer_thread.set_focus(thread);
        self.edit_input
            .set_focus(self.host_focused && self.editing.is_some());
    }

    /// Presence scope of the composer of a channel or thread, so the others
    /// there see who is writing. The draft itself is each writer's own.
    pub(crate) fn draft_presence_key(draft_key: &str) -> ui::presence::PresenceKey {
        ui::presence::PresenceKey::Widget {
            path: vec![
                ui::presence::PathSegment::Field("chat_draft".into()),
                ui::presence::PathSegment::Field(draft_key.to_owned()),
            ],
        }
    }

    /// Presence scope of a voice channel: who is in its call.
    pub(crate) fn voice_presence_key(channel: &str) -> ui::presence::PresenceKey {
        ui::presence::PresenceKey::Widget {
            path: vec![
                ui::presence::PathSegment::Field("chat_voice".into()),
                ui::presence::PathSegment::Field(channel.to_owned()),
            ],
        }
    }

    /// Who is in voice channel `channel`: this person first while they are,
    /// then everyone the room's presence places there.
    /// Whether this person is in the call now. The call panel stays open
    /// after the call ended for a reason, to say why; then they are no
    /// longer in it, for their own count and for everyone else's list.
    pub(crate) fn in_call(&self) -> bool {
        self.call.active && !self.native_voice.ended_for_a_reason()
    }

    pub(crate) fn voice_people(&self, channel: &str) -> Vec<Id> {
        let mut people = Vec::new();
        if self.in_call() && self.call.channel == channel {
            people.push(self.me.id.clone());
        }
        // What the room's presence says, and who the room's call counts in.
        let present = self.room_voice.get(channel).into_iter().flatten();
        let counted = self.voice_in(channel).iter().map(|member| &member.user);
        for person in present.chain(counted) {
            if !self.is_me(person) && !people.contains(person) {
                people.push(person.clone());
            }
        }
        people
    }

    /// The focused composer's scope while the person is writing something
    /// in it (not an inline edit or rename): that they are, never what or
    /// where in it.
    pub(crate) fn composer_presence(
        &self,
    ) -> Option<(ui::presence::PresenceKey, ui::PresencePayload)> {
        if !self.host_focused
            || self.editing.is_some()
            || !self.can_participate()
            || self.direct_person.is_some()
        {
            return None;
        }
        let (composer, key) = match self.focus {
            Focus::Thread => (&self.composer_thread, self.thread_draft_key()?),
            Focus::Main => (&self.composer_main, self.active_channel.clone()),
        };
        if key.is_empty() || composer.value().trim().is_empty() {
            return None;
        }
        Some((
            Self::draft_presence_key(&key),
            ui::PresencePayload::Caret { pos: 0 },
        ))
    }

    pub(crate) fn thread_draft_key(&self) -> Option<String> {
        self.thread_open
            .as_ref()
            .map(|p| format!("{}::t::{}", self.active_channel, p))
    }
}
