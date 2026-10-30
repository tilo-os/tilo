//! The workspace this window is in: taking it from the resource, adopting it with
//! the presence context, and settling the participant this device is.

use ui::presence::PresenceCtx;

use crate::model::{self, Id};
use crate::permissions::ChatPermissions;

use super::ChatApp;

impl ChatApp {
    /// Take the workspace's view of the participants off the presence store.
    ///
    /// Called once per frame from `draw`. The presence store's participant
    /// kinds come from the host's certified People projection; Chat never
    /// infers Agent identity from a payload kind, display name or id prefix.
    /// Names and colours remain presentation metadata and grant no authority.
    ///
    /// The workspace's answer **replaces** what was held rather than merging into
    /// it. Merging is how a peer who has gone keeps a name and a colour on
    /// everyone else's screen for the rest of the session: presence forgets
    /// them on leave, and anything downstream that only ever learned would go
    /// on offering a departed participant in the `@` picker and painting their
    /// colour behind a mention.
    /// Take this frame's workspace: who is here and in which call, their names
    /// and colours, and the authority the window acts under. Whether any of
    /// it changed, so the channels show it (also while the conversation is
    /// put away and not drawn). In a call they show who speaks, which moves
    /// every frame.
    pub(super) fn take_workspace(&mut self) -> bool {
        let before = (
            self.workspace_voice.clone(),
            self.workspace_names.clone(),
            self.workspace_colors.clone(),
            self.person_contacts.clone(),
        );
        let permissions = ui::presence::frame::frame(PresenceCtx::permission_context).flatten();
        let perms_changed = permissions.is_some_and(|permissions| {
            let changed = self.perms.context() != &permissions;
            self.perms = ChatPermissions::from_context(permissions);
            changed
        });
        if let Some(capture) = self.capture_agent_presence.clone() {
            self.adopt_workspace(&capture);
        } else if ui::presence::frame::frame(|presence| self.adopt_workspace(presence)).is_none() {
            // No presence this frame: nobody's answer says who is here, so the
            // record's own words about each person stand (`live_status`).
            self.workspace_presence_answers = false;
            return perms_changed;
        }
        let after = (
            &self.workspace_voice,
            &self.workspace_names,
            &self.workspace_colors,
            &self.person_contacts,
        );
        let changed = perms_changed || (&before.0, &before.1, &before.2, &before.3) != after;
        if changed || self.call.active {
            self.show_sidebar();
        }
        changed
    }

    pub fn adopt_workspace(&mut self, presence: &PresenceCtx) {
        // Presence answered: from here on it, not the record, says who is
        // here. An answer naming nobody besides this window's own person is
        // still an answer — the workspace is otherwise empty.
        self.workspace_presence_answers = true;
        self.person_contacts = ui::person_chat::contacts();
        self.adopt_own_participant(presence);
        self.workspace_voice = self
            .channels
            .iter()
            .filter(|channel| channel.kind == model::ChannelKind::Voice)
            .filter_map(|channel| {
                let key = Self::voice_presence_key(&channel.id);
                // As people: a call's roster names each person once, and
                // where presence names their devices, each is its person.
                let mut people: Vec<Id> = Vec::new();
                for (user, _) in presence.carets.get(&key).into_iter().flatten() {
                    let person = presence.person_of(user).unwrap_or(user).0.clone();
                    if !people.contains(&person) {
                        people.push(person);
                    }
                }
                (!people.is_empty()).then(|| (channel.id.clone(), people))
            })
            .collect();
        self.workspace_agents = presence
            .names
            .keys()
            .filter(|id| presence.participant_kind(id) == ui::presence::ParticipantKind::Agent)
            .map(|id| {
                id.0.strip_prefix("agent:")
                    .unwrap_or(id.0.as_str())
                    .to_owned()
            })
            .collect();
        self.workspace_agents.sort();
        self.workspace_agents.dedup();
        self.workspace_persons.clear();
        self.workspace_names.retain(|id, _| {
            presence
                .names
                .contains_key(&ui::permissions::UserId(id.clone()))
        });
        self.workspace_colors.retain(|id, _| {
            presence
                .names
                .contains_key(&ui::permissions::UserId(id.clone()))
        });
        for (id, name) in &presence.names {
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            self.workspace_names.insert(id.0.clone(), name.to_string());
            // A colour is the same kind of answer: assigned per workspace so two
            // people never collide, and shared with the caret pill and cursor.
            self.workspace_colors
                .insert(id.0.clone(), presence.color_for(id));
            // Where presence names a person's device, the person (as a
            // call's roster names them) goes by the same name and colour,
            // told apart from a namesake as the device is.
            if let Some(person) = presence.person_of(id) {
                self.workspace_persons
                    .insert(id.0.clone(), person.0.clone());
                self.workspace_names
                    .insert(person.0.clone(), self.workspace_labels.label(&id.0, name));
                self.workspace_colors
                    .insert(person.0.clone(), presence.color_for(id));
            }
        }
    }

    /// Take the workspace's answer to "which of these participants am I".
    ///
    /// This window signs what it writes with the id its host gave it, the
    /// author every action of its person carries. A workspace that spells the
    /// same participant differently names them too: lines recorded under
    /// that spelling are still this person's, so their name stays beside
    /// them.
    fn adopt_own_participant(&mut self, presence: &PresenceCtx) {
        self.workspace_labels.clone_from(presence.labels());
        self.own_label = presence.self_label();
        if let Some(self_id) = presence.self_id()
            && self_id.0 != self.me.id
            && !self.former_ids.contains(&self_id.0)
        {
            self.former_ids.push(self_id.0.clone());
        }
        if let Some(name) = presence.self_name()
            && name != self.me.name
        {
            self.me.name = name.to_string();
            self.me.initials = crate::identity::initials_for(name);
        }
    }
}
