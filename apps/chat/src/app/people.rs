//! Who someone is on screen: their name, initials and colour, the presence the
//! roster shows, and the row the detail drawer lists them as.

use std::collections::HashMap;

use tilo_actions::policy::MemberId;

use crate::model::{self, User};

use super::ChatApp;

impl ChatApp {
    /// What to call `id`: the workspace's answer first, the document's stored name
    /// as an offline fallback, and the raw id last so a label is never blank.
    /// People who share a name both go by it, this window's own person included.
    ///
    /// The precedence is the point. A client that could name itself would be
    /// telling every other peer what to call it, and two peers disagreeing
    /// about a third's name is a difference nobody can settle. So the workspace
    /// answers, and a peer only ever supplies its id.
    pub fn display_name(&self, id: &MemberId) -> String {
        if let Some(name) = self.agent_name(id, None) {
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
            return if self.is_me(id) {
                n.clone()
            } else {
                crate::identity::shared_name_label(id, n)
            };
        }
        // An app's code answers where no window heard the member list: the members the device
        // lends it answer in the same place.
        if let Some(n) = ui::tilo_resource::members::name_of(id) {
            return if self.is_me(id) {
                n
            } else {
                crate::identity::shared_name_label(id, &n)
            };
        }
        if let Some(u) = self.users.iter().find(|u| u.id == *id)
            && !u.name.trim().is_empty()
        {
            return if self.is_me(id) {
                u.name.clone()
            } else {
                crate::identity::shared_name_label(id, &u.name)
            };
        }
        // Your own name, which this had no way to reach until the workspace named
        // it: `me` is not necessarily a row in `users`, which is built from
        // the workspace document, and a workspace this peer has not spoken in has
        // never heard of it.
        if self.is_me(id) && !self.me.name.trim().is_empty() {
            return self.me.name.clone();
        }
        id.id().to_owned()
    }

    /// What the workspace calls the agent `agent` (its bare id): the name its member list gives
    /// it, else the name its rows here keep. `None` for one the workspace names not.
    pub(crate) fn known_agent_name(&self, agent: &str) -> Option<String> {
        let id = MemberId::agent(agent);
        self.workspace_names
            .get(&id)
            .filter(|name| !name.trim().is_empty())
            .cloned()
            .or_else(|| {
                self.users
                    .iter()
                    .find(|user| user.id == id && user.is_agent)
                    .map(|user| user.name.clone())
            })
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
    }

    /// The service a bridged person in a call is reached through (`discord`),
    /// as its label; `None` for a person of the workspace.
    pub(crate) fn voice_service(&self, id: &MemberId) -> Option<String> {
        self.voice_members
            .values()
            .flatten()
            .find(|member| member.user == *id)
            .and_then(|member| member.service.as_deref())
            .map(ui::author::service_label)
            .filter(|label| !label.is_empty())
    }

    /// What to call `id` on screen: this session's own member by their
    /// name like anyone else, and an honest placeholder for someone the workspace
    /// has not named yet — nobody is ever shown their raw routing id.
    pub(crate) fn user_name(&self, id: &MemberId) -> String {
        // `is_me` rather than a bare comparison: a peer that has adopted the
        // workspace's address for itself still wrote its earlier lines under the
        // id it resolved at startup, and those are still its own. Their own
        // name, as everyone else reads it.
        if self.is_me(id) {
            let own = self.display_name(id);
            // A person the workspace has no name for reads their own lines as theirs, as
            // History names them, never as the placeholder others see for them.
            if own == id.id() || own.trim().is_empty() || own == crate::identity::unnamed_label(id)
            {
                return crate::i18n::t("chat-person-you");
            }
            return own;
        }
        if let Some(agent) = self.agent_name(id, None) {
            return agent;
        }
        let named = self.display_name(id);
        // A name that happens to equal the id is still a name when the
        // document stores it (a browser mount's author is both).
        let stored = self
            .users
            .iter()
            .any(|u| u.id == *id && u.name.trim() == named.trim());
        if (named != id.id() || stored) && !named.trim().is_empty() {
            return crate::identity::shared_name_label(id, &named);
        }
        // A bridged person no member is is called what they are called on the
        // service the call reaches them through.
        if let Some(name) = self
            .voice_members
            .values()
            .flatten()
            .find(|member| member.user == *id)
            .and_then(|member| member.name.as_deref())
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            return name.to_owned();
        }
        crate::identity::unnamed_label(id)
    }

    /// What to call the agent or app `id`: the name a line of theirs wrote under (`kept`), which
    /// is the session the agent wrote it in where its client named one ("Release prep"), else what
    /// the workspace calls it; numbered while another member goes by that name too, as every
    /// surface names it. A person's line keeps the name it wrote under. `None` for anyone else.
    pub(crate) fn agent_name(&self, id: &MemberId, kept: Option<&str>) -> Option<String> {
        let kept = kept
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned);
        if id.is_person() {
            return kept;
        }
        kept.or_else(|| self.known_agent_name(id.id()))
    }

    /// How a narrow line names someone: their given name, so a line that
    /// carries faces, three names and a count still says who leads the crowd
    /// instead of eliding a surname. A person named in full once — a
    /// message's author, a call's accessible reading — keeps `user_name`.
    pub(crate) fn given_name(&self, id: &MemberId) -> String {
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
    pub fn initials_for(&self, id: &MemberId) -> String {
        let name = self.display_name(id);
        let s = ui::widget::initials_for_name(&name);
        if s.is_empty() {
            // No word of the name has a letter, so there is no case to set.
            name.chars().next().map(String::from).unwrap_or_default()
        } else {
            s
        }
    }

    /// The presence hue `id` wears: the one the workspace announced for them, else the one rule
    /// every surface draws a member by ([`ui::tokens::PresenceHue::of_member`]) — so a face, a
    /// chip and a byline are one colour, here as in every app of the workspace, for an agent as
    /// for a person.
    pub(crate) fn member_hue(&self, id: &MemberId) -> ui::tokens::PresenceHue {
        self.workspace_colors
            .get(id)
            .and_then(|(red, green, blue)| {
                ui::tokens::PresenceHue::nearest_to_srgb(*red, *green, *blue)
            })
            .unwrap_or_else(|| ui::tokens::PresenceHue::of_member(id))
    }

    /// The member's own colour, in the shared presence vocabulary: the workspace's assignment
    /// when it made one, otherwise [`Self::member_hue`] as the colour that hue wears.
    pub fn member_color(&self, id: &MemberId) -> (u8, u8, u8) {
        self.workspace_colors
            .get(id)
            .copied()
            .unwrap_or_else(|| ui::presence::identity_rgb(self.member_hue(id)))
    }

    /// Everyone the composers' `@` lists offer: the chat's people, so someone who wrote here
    /// stays mentionable after they left, and every member the workspace names, its agents
    /// among them.
    pub(crate) fn mention_people(&self) -> Vec<ui::widget::MentionCandidate> {
        self.member_ids()
            .into_iter()
            .filter(|id| {
                self.users.iter().any(|user| user.id == *id)
                    || self
                        .workspace_names
                        .get(id)
                        .is_some_and(|name| !name.trim().is_empty())
            })
            .map(|id| ui::widget::MentionCandidate {
                name: self.display_name(&id),
                hue: self.member_hue(&id),
                is_self: self.is_me(&id),
                id,
            })
            .collect()
    }

    /// Everyone who can be mentioned, by name: the people of the conversations
    /// shown, and every member the workspace lists.
    pub fn members(&self) -> Vec<(MemberId, String)> {
        let mut out: Vec<(MemberId, String)> = self
            .member_ids()
            .into_iter()
            .map(|id| {
                let name = self.display_name(&id);
                (id, name)
            })
            .collect();
        out.sort_by(|a, b| a.1.cmp(&b.1));
        out
    }

    /// [`Self::members`] without their names: who can be mentioned, each
    /// once. A frame that only needs their colours reads this, so it never
    /// works out a name or builds a row.
    pub(crate) fn member_ids(&self) -> Vec<MemberId> {
        let mut out: Vec<MemberId> = self.users.iter().map(|user| user.id.clone()).collect();
        for id in self.workspace_names.keys() {
            if !out.contains(id) {
                out.push(id.clone());
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
        let mut in_voice: HashMap<MemberId, String> = HashMap::new();
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
                let voice = in_voice.get(&u.id).cloned();
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

    /// Whether `person` is here now. While presence answers who is present,
    /// that answer decides: present people
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

    /// Whether this workspace has somebody besides the local member. An
    /// agent is still a member (and should surface collaboration
    /// controls), but it is never counted or labelled as a person.
    pub(crate) fn has_other_member(&self) -> bool {
        self.users.iter().any(|u| !self.is_me(&u.id))
    }

    /// The roster's public destination name. `People` is precise when every
    /// entry is human; once an agent is present, `Members` accurately
    /// covers both and the row itself keeps the agent visibly typed.
    pub(crate) fn roster_label(&self) -> &'static str {
        if self.users.iter().any(|u| u.is_agent) {
            "Members"
        } else {
            "People"
        }
    }
}
