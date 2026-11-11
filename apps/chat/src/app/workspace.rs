//! The workspace this window is in: taking it from the resource, adopting it with
//! the presence context, and settling the participant this device is.

use std::collections::HashMap;

use ui::presence::PresenceCtx;

use crate::model::{self, Id};
use crate::permissions::ChatPermissions;

use super::ChatApp;

/// What the channels show of the workspace and its calls. Kept as one value so
/// a frame can tell whether showing them again would draw anything else.
#[derive(Clone, Default, PartialEq)]
pub(crate) struct ChannelRows {
    pub(crate) voice: HashMap<Id, Vec<Id>>,
    pub(crate) names: HashMap<Id, String>,
    pub(crate) colors: HashMap<Id, (u8, u8, u8)>,
    pub(crate) in_calls: HashMap<Id, Vec<model::VoiceMember>>,
    /// Whether this window is in a call, and which.
    pub(crate) call: (bool, String),
}

impl ChatApp {
    /// Keep the workspace's record of its agents, as the window reads it: the
    /// agent identity record every app of the workspace opens. Chat keeps it,
    /// and names every author it spells `agent:<id>` by it.
    #[must_use]
    pub(crate) fn with_agents_record(
        mut self,
        record: Option<ui::framework::WorkspaceRecord<ui::tilo_workspace_agent::WorkspaceAgents>>,
    ) -> Self {
        self.agents_record = record;
        self
    }

    /// Follow the workspace's agents record: what each agent is called, by its bare id.
    ///
    /// An agent is a collaborator of the workspace like a person is, and its
    /// name is the workspace's answer, kept in its own record ([`WorkspaceAgents`]).
    /// Presence only names an agent while its device works here; the record
    /// names it always, so an author reads by its name whether or not it is
    /// running. Whether any name changed, so the window shows them again.
    pub(super) fn follow_agents(&mut self) -> bool {
        let Some(record) = self.agents_record.clone() else {
            return false;
        };
        let state = record.get();
        let names: HashMap<Id, String> = state
            .active()
            .filter(|agent| !agent.name.trim().is_empty())
            .map(|agent| (agent.id.0.clone(), agent.name.trim().to_owned()))
            .collect();
        let colours: HashMap<Id, u8> = state
            .active()
            .map(|agent| (agent.id.0.clone(), agent.slot()))
            .collect();
        if names == self.workspace_agent_names && colours == self.workspace_agent_colours {
            return false;
        }
        self.workspace_agent_names = names;
        self.workspace_agent_colours = colours;
        true
    }

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
    /// and colours, the rosters of the calls, and the authority the window
    /// acts under. Whether any of it changed, so the channels show it (also
    /// while the conversation is put away and not drawn). In a call they show
    /// who speaks, which moves every frame.
    pub(super) fn take_workspace(&mut self) -> bool {
        // The workspace's agents are named by its own record, not by presence: an
        // author reads by its name after it stopped working here too.
        let agents_changed = self.follow_agents();
        if agents_changed {
            // A conversation's name is what its participants are called: read them
            // again, as this pass does when the record moved.
            self.synced = None;
        }
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
            return perms_changed || agents_changed;
        }
        // Who is in a call is not presence: it arrives from the host and from
        // the call's own service while this frame is being built, and a call
        // this window never joined changes nothing else here — so a channel
        // names everyone the last build saw, not everyone the record says.
        let rows = self.channel_rows();
        let changed = perms_changed || rows != self.shown_rows;
        if changed || self.call.active {
            self.show_sidebar();
            self.shown_rows = rows;
        }
        changed || agents_changed
    }

    /// What the channels would show now.
    fn channel_rows(&self) -> ChannelRows {
        ChannelRows {
            voice: self.workspace_voice.clone(),
            names: self.workspace_names.clone(),
            colors: self.workspace_colors.clone(),
            in_calls: self.voice_members.clone(),
            call: (self.call.active, self.call.channel.clone()),
        }
    }

    pub fn adopt_workspace(&mut self, presence: &PresenceCtx) {
        // Presence answered: from here on it, not the record, says who is
        // here. An answer naming nobody besides this window's own person is
        // still an answer — the workspace is otherwise empty.
        self.workspace_presence_answers = true;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row the channels handed the shell, nested rows too.
    fn rows(app: &ChatApp) -> Vec<String> {
        fn walk(items: &[ui::widget::NavItem], into: &mut Vec<String>) {
            for item in items {
                into.push(item.id.clone());
                walk(&item.children, into);
            }
        }
        let mut ids = Vec::new();
        if let Some((sections, _)) = app.places.shown() {
            for section in &sections {
                walk(&section.items, &mut ids);
            }
        }
        ids
    }

    #[test]
    fn a_call_ending_elsewhere_takes_its_rows_off_the_channels() {
        let mut doc = automerge::AutoCommit::new();
        crate::demo::seed(&mut doc).unwrap();
        let mut app = ChatApp::new_ephemeral();
        app.reload(&doc);
        app.capture_agent_presence = Some(PresenceCtx::empty());
        app.take_workspace();
        let row = "in-call:c_standup/u_dana";
        let mut dana = crate::model::VoiceMember {
            user: "u_dana".into(),
            talking: false,
            muted: false,
            deafened: false,
            audio_only: true,
            sharing: false,
            ticket: None,
            video: None,
        };
        app.voice_members
            .insert("c_standup".into(), vec![dana.clone()]);
        assert!(app.take_workspace(), "someone in a call is a change");
        assert!(rows(&app).iter().any(|id| id == row), "{:?}", rows(&app));
        dana.talking = true;
        app.voice_members.insert("c_standup".into(), vec![dana]);
        assert!(app.take_workspace(), "someone speaking is a change");
        app.voice_members.clear();
        assert!(app.take_workspace(), "the call ending here is a change");
        assert!(!rows(&app).iter().any(|id| id == row), "{:?}", rows(&app));

        // This window's own row goes the same way once it has hung up.
        let mine = format!("in-call:c_standup/{}", app.me.id);
        app.call.active = true;
        app.call.channel = "c_standup".into();
        assert!(app.take_workspace(), "joining here is a change");
        assert!(rows(&app).iter().any(|id| id == &mine), "{:?}", rows(&app));
        app.call.active = false;
        assert!(app.take_workspace(), "the call ending is a change");
        assert!(!rows(&app).iter().any(|id| id == &mine), "{:?}", rows(&app));
    }
}
