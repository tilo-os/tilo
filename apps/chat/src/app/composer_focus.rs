//! Which composer owns the keyboard — the main one, the thread's or an
//! inline message editor — and the presence a draft publishes while it is
//! being written.

use ui::presence::PresenceKey;
use ui::tilo_resource::MemberId;

use super::{ChatApp, Focus};

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

    /// Where a composer on `focus` writes, as Chat publishes it: the conversation, or the message
    /// a thread answers. What a draft there is said under, so the others reading it see who is
    /// typing.
    pub(crate) fn composing_key(&self, focus: Focus) -> Option<PresenceKey> {
        let (id, kind) = match focus {
            Focus::Main => (
                crate::views::channel_address(&self.channel(&self.active_channel)?.id),
                <crate::views::Channel as tilo_view::ViewResource>::KIND.to_owned(),
            ),
            Focus::Thread => {
                let resource =
                    crate::views::Channel::message_resource(self.thread_open.as_deref()?)?;
                (resource.id, resource.ty)
            }
        };
        Some(PresenceKey::resource("so.tilo.chat", id, kind))
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
    /// then everyone the workspace's presence places there.
    /// Whether this person is in the call now. The call panel stays open
    /// after the call ended for a reason, to say why; then they are no
    /// longer in it, for their own count and for everyone else's list.
    pub(crate) fn in_call(&self) -> bool {
        self.call.active && !self.native_voice.ended_for_a_reason()
    }

    pub(crate) fn voice_people(&self, channel: &str) -> Vec<MemberId> {
        let mut people = Vec::new();
        if self.in_call() && self.call.channel == channel {
            people.push(self.me.id.clone());
        }
        // What the workspace's presence says, and who the workspace's call counts in.
        let present = self.workspace_voice.get(channel).into_iter().flatten();
        let counted = self.voice_in(channel).iter().map(|member| &member.user);
        for person in present.chain(counted) {
            if !self.is_me(person) && !people.contains(person) {
                people.push(person.clone());
            }
        }
        people
    }

    pub(crate) fn thread_draft_key(&self) -> Option<String> {
        self.thread_open
            .as_ref()
            .map(|p| format!("{}::t::{}", self.active_channel, p))
    }
}
