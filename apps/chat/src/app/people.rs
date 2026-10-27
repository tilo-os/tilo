//! Who someone is on screen: their name, initials and colour, the presence the
//! roster shows, and the row the detail drawer lists them as.

use std::collections::HashMap;

use crate::model::{self, Id, User};

use super::ChatApp;

impl ChatApp {
    /// What to call `id`: the room's answer first, the document's stored name
    /// as an offline fallback, and the raw id last so a label is never blank.
    /// People who share a name carry the number the room tells them apart
    /// by ("Ada (2)"), this window's own person included.
    ///
    /// The precedence is the point. A client that could name itself would be
    /// telling every other peer what to call it, and two peers disagreeing
    /// about a third's name is a difference nobody can settle. So the room
    /// answers, and a peer only ever supplies its id.
    pub fn display_name(&self, id: &str) -> String {
        if let Some(agent) = id.strip_prefix("agent:")
            && self.room_agents.iter().any(|known| known == agent)
        {
            return self.agent_display_name(agent);
        }
        if self.is_me(id)
            && let Some(own) = &self.own_label
        {
            return own.clone();
        }
        if let Some(n) = self.room_names.get(id)
            && !n.trim().is_empty()
        {
            let n = self.room_labels.label(id, n);
            return if self.is_me(id) {
                n
            } else {
                crate::identity::shared_name_label(id, &n)
            };
        }
        if let Some(agent) = self.agent_authors.get(id) {
            return agent.clone();
        }
        if let Some(u) = self.users.iter().find(|u| u.id == id)
            && !u.name.trim().is_empty()
        {
            let name = self.room_labels.label(id, &u.name);
            return if self.is_me(id) {
                name
            } else {
                crate::identity::shared_name_label(id, &name)
            };
        }
        // Your own name, which this had no way to reach until the room named
        // it: `me` is not necessarily a row in `users`, which is built from
        // the room document, and a room this peer has not spoken in has
        // never heard of it.
        if self.is_me(id) && !self.me.name.trim().is_empty() {
            return self.me.name.clone();
        }
        id.to_string()
    }

    /// What to call `id` on screen: this session's own participant by their
    /// name like anyone else, and an honest placeholder for someone the room
    /// has not named yet — nobody is ever shown their raw routing id.
    pub(crate) fn user_name(&self, id: &str) -> String {
        // `is_me` rather than a bare comparison: a peer that has adopted the
        // room's address for itself still wrote its earlier lines under the
        // id it resolved at startup, and those are still its own. Their own
        // name, as everyone else reads it.
        if self.is_me(id) {
            let own = self.display_name(id);
            if own != id && !own.trim().is_empty() {
                return own;
            }
            return crate::identity::unnamed_label(id);
        }
        if let Some(agent) = id.strip_prefix("agent:")
            && self.room_agents.iter().any(|known| known == agent)
        {
            return crate::i18n::t_args(
                "chat-agent-author",
                &[("name", self.agent_display_name(agent).into())],
            );
        }
        if let Some(agent) = self.agent_authors.get(id) {
            return format!("{agent} (agent)");
        }
        let named = self.display_name(id);
        // A name that happens to equal the id is still a name when the
        // document stores it (a browser mount's author is both).
        let stored = self
            .users
            .iter()
            .any(|u| u.id == id && u.name.trim() == named.trim());
        if (named != id || stored) && !named.trim().is_empty() {
            return crate::identity::shared_name_label(id, &named);
        }
        crate::identity::unnamed_label(id)
    }

    /// The avatar monogram for `id`, derived from whatever the room calls them
    /// so the letter never disagrees with the name beside it.
    pub fn initials_for(&self, id: &str) -> String {
        let name = self.display_name(id);
        let s = ui::widget::initials_for_name(&name);
        if s.is_empty() {
            name.chars()
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_default()
        } else {
            s
        }
    }

    /// The participant's own colour, in the shared presence vocabulary: the
    /// room's assignment when it made one, otherwise the system-wide
    /// per-user colour every other surface derives the same way.
    pub fn participant_color(&self, id: &str) -> (u8, u8, u8) {
        self.room_colors
            .get(id)
            .copied()
            .unwrap_or_else(|| ui::presence::user_color(id))
    }

    /// The chat's people as the composers' `@` lists offer them beside the
    /// room's roster: someone who wrote here stays mentionable after they
    /// left the room.
    pub(crate) fn mention_people(&self) -> Vec<ui::widget::MentionCandidate> {
        self.users
            .iter()
            .map(|user| ui::widget::MentionCandidate {
                id: user.id.clone(),
                name: self.display_name(&user.id),
                kind: ui::presence::ParticipantKind::from(user.is_agent),
                color: self.participant_color(&user.id),
                is_self: self.is_me(&user.id),
            })
            .collect()
    }

    /// Everyone a message's `@name`s bind to when it is sent: the
    /// document's own users, plus anyone the room is talking about who has
    /// not yet written themselves into it.
    pub fn participants(&self) -> Vec<crate::mention::Participant> {
        let mut out: Vec<crate::mention::Participant> = self
            .users
            .iter()
            .map(|u| crate::mention::Participant {
                id: u.id.clone(),
                name: self.display_name(&u.id),
                is_agent: u.is_agent,
            })
            .collect();
        // Each person once: a device stands for its person, so neither the
        // person's alias in `room_names` nor a device whose person already
        // wrote themselves in is a second candidate.
        let aliases: std::collections::HashSet<&Id> = self.room_persons.values().collect();
        for id in self.room_names.keys() {
            let person_listed = self
                .room_persons
                .get(id)
                .is_some_and(|person| out.iter().any(|p| &p.id == person));
            if !aliases.contains(id) && !person_listed && !out.iter().any(|p| &p.id == id) {
                out.push(crate::mention::Participant {
                    id: id.clone(),
                    name: self.display_name(id),
                    is_agent: false,
                });
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// The members roster shown in the right flap when no thread is open: every
    /// user in the room paired with the voice channel they're currently in
    /// (if any). Sorted in-call first, then by presence, then by name. Real
    /// projection of the shared user list + voice membership — no placeholders.
    pub(crate) fn roster_rows(&self) -> Vec<(User, Option<String>)> {
        // user id → the name of the voice channel they're connected to.
        let mut in_voice: HashMap<Id, String> = HashMap::new();
        for channel in self
            .channels
            .iter()
            .filter(|channel| channel.kind == model::ChannelKind::Voice)
        {
            for person in self.voice_people(&channel.id) {
                in_voice.insert(person, channel.name.clone());
            }
        }
        let mut rows: Vec<(User, Option<String>)> = self
            .users
            .iter()
            .cloned()
            .map(|mut u| {
                u.status = self.live_status(&u);
                let voice = in_voice.get(u.id.as_str()).cloned();
                (u, voice)
            })
            .collect();
        let rank = |u: &User, voice: &Option<String>| -> u8 {
            if voice.is_some() {
                0
            } else {
                match u.status {
                    crate::model::Presence::Online => 1,
                    crate::model::Presence::Idle => 2,
                    crate::model::Presence::Dnd => 3,
                    crate::model::Presence::Offline => 4,
                }
            }
        };
        rows.sort_by(|a, b| {
            rank(&a.0, &a.1)
                .cmp(&rank(&b.0, &b.1))
                .then_with(|| a.0.name.cmp(&b.0.name))
        });
        // This session's person once, whichever of its spellings the
        // document recorded them under.
        let mut me_listed = false;
        rows.retain(|(person, _)| {
            if !self.is_me(&person.id) {
                return true;
            }
            !std::mem::replace(&mut me_listed, true)
        });
        rows
    }

    /// Whether `person` is here now. The record keeps the status someone
    /// introduced themselves with, and nobody writes "Offline" when a tab
    /// closes, so a departed visitor would read "Online" forever. While the
    /// room answers who is present, that answer decides: present people keep
    /// the away or busy they set, everyone else is offline. Without a room
    /// (a chat on this device alone) the recorded status stands.
    pub(crate) fn live_status(&self, person: &User) -> model::Presence {
        if self.room_names.is_empty() {
            return person.status;
        }
        let present = self.is_me(&person.id) || self.room_names.contains_key(&person.id);
        match (present, person.status) {
            (false, _) => model::Presence::Offline,
            (true, model::Presence::Offline) => model::Presence::Online,
            (true, status) => status,
        }
    }

    /// Whether this room has somebody besides the local participant. An
    /// agent is still a collaborator (and should surface collaboration
    /// controls), but it is never counted or labelled as a person.
    pub(crate) fn has_other_collaborator(&self) -> bool {
        self.users.iter().any(|u| !self.is_me(&u.id))
    }

    /// The roster's public destination name. `People` is precise when every
    /// entry is human; once an agent is present, `Collaborators` accurately
    /// covers both and the row itself keeps the agent visibly typed.
    pub(crate) fn roster_label(&self) -> &'static str {
        if self.users.iter().any(|u| u.is_agent) {
            "Collaborators"
        } else {
            "People"
        }
    }
}
