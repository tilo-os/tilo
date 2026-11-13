//! Who someone is on screen: their name, initials and colour, the presence the
//! roster shows, and the row the detail drawer lists them as.

use std::collections::HashMap;

use crate::model::{self, Id, User};

use super::ChatApp;

impl ChatApp {
    /// What to call `id`: the workspace's answer first, the document's stored name
    /// as an offline fallback, and the raw id last so a label is never blank.
    /// People who share a name carry the number the workspace tells them apart
    /// by ("Ada (2)"), this window's own person included.
    ///
    /// The precedence is the point. A client that could name itself would be
    /// telling every other peer what to call it, and two peers disagreeing
    /// about a third's name is a difference nobody can settle. So the workspace
    /// answers, and a peer only ever supplies its id.
    pub fn display_name(&self, id: &str) -> String {
        if let Some(agent) = id.strip_prefix("agent:")
            && let Some(name) = self.known_agent_name(agent)
        {
            return name;
        }
        if self.is_me(id)
            && let Some(own) = &self.own_label
        {
            return own.clone();
        }
        if let Some(n) = self.workspace_names.get(id)
            && !n.trim().is_empty()
        {
            let n = self.workspace_labels.label(id, n);
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
            let name = self.workspace_labels.label(id, &u.name);
            return if self.is_me(id) {
                name
            } else {
                crate::identity::shared_name_label(id, &name)
            };
        }
        // Your own name, which this had no way to reach until the workspace named
        // it: `me` is not necessarily a row in `users`, which is built from
        // the workspace document, and a workspace this peer has not spoken in has
        // never heard of it.
        if self.is_me(id) && !self.me.name.trim().is_empty() {
            return self.me.name.clone();
        }
        id.to_string()
    }

    /// What the workspace calls the agent `agent` (its bare id): the name its own record keeps for
    /// it, else the name its device announced while here. `None` for one the workspace names not.
    pub(crate) fn known_agent_name(&self, agent: &str) -> Option<String> {
        self.workspace_agent_names
            .get(agent)
            .cloned()
            .or_else(|| {
                [format!("agent:{agent}"), agent.to_owned()]
                    .into_iter()
                    .find_map(|id| self.workspace_names.get(&id).cloned())
            })
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
    }

    /// What to call `id` on screen: this session's own participant by their
    /// name like anyone else, and an honest placeholder for someone the workspace
    /// has not named yet — nobody is ever shown their raw routing id.
    pub(crate) fn user_name(&self, id: &str) -> String {
        // `is_me` rather than a bare comparison: a peer that has adopted the
        // workspace's address for itself still wrote its earlier lines under the
        // id it resolved at startup, and those are still its own. Their own
        // name, as everyone else reads it.
        if self.is_me(id) {
            let own = self.display_name(id);
            if own != id && !own.trim().is_empty() {
                return own;
            }
            return crate::identity::unnamed_label(id);
        }
        if let Some(agent) = self.acting_name(id, None) {
            return agent;
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

    /// What to call the agent or app `id`, by what `acting` kept of it where it wrote, else by
    /// who is here: the workspace's name for it, else the name it went by, with the person it
    /// acts for ("Helper for Ada"), after it left too. `None` for anyone else.
    pub(crate) fn acting_name(
        &self,
        id: &str,
        acting: Option<&ui::tilo_resource::Acting>,
    ) -> Option<String> {
        let name = id
            .strip_prefix("agent:")
            .and_then(|agent| self.known_agent_name(agent))
            .or_else(|| self.agent_authors.get(id).cloned())
            .or_else(|| acting?.name.as_ref().map(|name| name.0.clone()))?;
        let person = acting
            .and_then(|acting| acting.person.as_ref())
            .map(|person| person.0.as_str())
            .or_else(|| self.agent_people.get(id).map(String::as_str));
        Some(crate::identity::acting_label(
            &name,
            person,
            |person| self.user_name(person),
            |person| self.is_me(person),
        ))
    }

    /// How a narrow line names someone: their given name, so a line that
    /// carries faces, three names and a count still says who leads the crowd
    /// instead of eliding a surname. A person named in full once — a
    /// message's author, a call's accessible reading — keeps `user_name`.
    pub(crate) fn given_name(&self, id: &str) -> String {
        let name = self.user_name(id);
        match name.split_once(' ') {
            Some((given, rest)) if !given.is_empty() && !rest.trim().is_empty() => {
                given.to_string()
            }
            _ => name,
        }
    }

    /// The avatar monogram for `id`, derived from whatever the workspace calls them
    /// so the letter never disagrees with the name beside it.
    pub fn initials_for(&self, id: &str) -> String {
        let name = self.display_name(id);
        let s = ui::widget::initials_for_name(&name);
        if s.is_empty() {
            // No word of the name has a letter, so there is no case to set.
            name.chars().next().map(String::from).unwrap_or_default()
        } else {
            s
        }
    }

    /// The presence hue `id` wears: an agent's is the one its own record gives it (its keeper's
    /// choice, else the one rule's), a person's the one the workspace announced for them, else
    /// the one rule every surface draws a participant by ([`ui::presence::participant_hue`]) —
    /// so a face, a chip and a byline are one colour, here as in every app of the workspace.
    pub(crate) fn participant_hue(&self, id: &str) -> ui::tokens::PresenceHue {
        id.strip_prefix("agent:")
            .and_then(|agent| self.workspace_agent_colours.get(agent))
            .map(|slot| ui::tokens::PresenceHue::from_slot(usize::from(*slot)))
            .or_else(|| {
                self.workspace_colors
                    .get(id)
                    .and_then(|(red, green, blue)| {
                        ui::tokens::PresenceHue::nearest_to_srgb(*red, *green, *blue)
                    })
            })
            .unwrap_or_else(|| ui::presence::participant_hue(id, crate::views::is_agent(id), None))
    }

    /// The participant's own colour, in the shared presence vocabulary: the workspace's assignment
    /// when it made one for a person, otherwise [`Self::participant_hue`] as the colour that hue
    /// wears.
    pub fn participant_color(&self, id: &str) -> (u8, u8, u8) {
        let agent = id
            .strip_prefix("agent:")
            .is_some_and(|agent| self.workspace_agent_colours.contains_key(agent));
        self.workspace_colors
            .get(id)
            .filter(|_| !agent)
            .copied()
            .unwrap_or_else(|| ui::presence::identity_rgb(self.participant_hue(id)))
    }

    /// The chat's people as the composers' `@` lists offer them beside the
    /// workspace's roster: someone who wrote here stays mentionable after they
    /// left the workspace.
    pub(crate) fn mention_people(&self) -> Vec<ui::widget::MentionCandidate> {
        self.users
            .iter()
            .map(|user| ui::widget::MentionCandidate {
                id: user.id.clone(),
                name: self.display_name(&user.id),
                kind: ui::presence::ParticipantKind::from(user.is_agent),
                hue: self.participant_hue(&user.id),
                is_self: self.is_me(&user.id),
            })
            .collect()
    }

    /// Everyone a message's `@name`s bind to when it is sent: the
    /// document's own users, plus anyone the workspace is talking about who has
    /// not yet written themselves into it.
    pub fn participants(&self) -> Vec<crate::mention::Participant> {
        let mut out: Vec<crate::mention::Participant> = self
            .participant_ids()
            .into_iter()
            .map(|(id, is_agent)| crate::mention::Participant {
                id: id.to_owned(),
                name: self.display_name(id),
                is_agent,
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// [`Self::participants`] without their names: who can be mentioned, each
    /// once. A frame that only needs their colours reads this, so it neither
    /// names every participant nor allocates one string each.
    pub(crate) fn participant_ids(&self) -> Vec<(&str, bool)> {
        let mut out: Vec<(&str, bool)> = self
            .users
            .iter()
            .map(|u| (u.id.as_str(), u.is_agent))
            .collect();
        // Each person once: a device stands for its person, so neither the
        // person's alias in `workspace_names` nor a device whose person already
        // wrote themselves in is a second candidate.
        let aliases: std::collections::HashSet<&Id> = self.workspace_persons.values().collect();
        for id in self.workspace_names.keys() {
            let person_listed = self
                .workspace_persons
                .get(id)
                .is_some_and(|person| out.iter().any(|(listed, _)| listed == person));
            if !aliases.contains(id)
                && !person_listed
                && !out.iter().any(|(listed, _)| *listed == id.as_str())
            {
                out.push((id.as_str(), false));
            }
        }
        out
    }

    /// The members roster shown in the right flap when no thread is open: every
    /// user in the workspace paired with the voice channel they're currently in
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
    /// closes, so a departed visitor would read "Online" forever. While
    /// presence answers who is present, that answer decides: present people
    /// keep the away or busy they set, everyone else is offline — including
    /// when the answer names nobody but this window's own person, which is
    /// how a workspace that everybody has left reads. A window whose host has no
    /// presence at all (a chat on this device alone) keeps the recorded
    /// status.
    pub(crate) fn live_status(&self, person: &User) -> model::Presence {
        if !self.workspace_presence_answers {
            return person.status;
        }
        let present = self.is_me(&person.id) || self.workspace_names.contains_key(&person.id);
        match (present, person.status) {
            (false, _) => model::Presence::Offline,
            (true, model::Presence::Offline) => model::Presence::Online,
            (true, status) => status,
        }
    }

    /// Whether this workspace has somebody besides the local participant. An
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
