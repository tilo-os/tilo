//! The workspace this window is in: taking it from the resource, adopting it with
//! the presence context, and settling the member this device is.

use std::collections::HashMap;

use ui::presence::PresenceCtx;
use ui::tilo_resource::MemberId;

use crate::model::{self, Id};
use crate::permissions::ChatPermissions;

use super::ChatApp;

/// What the channels show of the workspace and its calls. Kept as one value so
/// a frame can tell whether showing them again would draw anything else.
#[derive(Clone, Default, PartialEq)]
pub(crate) struct ChannelRows {
    pub(crate) voice: HashMap<Id, Vec<MemberId>>,
    pub(crate) names: HashMap<MemberId, String>,
    pub(crate) colors: HashMap<MemberId, (u8, u8, u8)>,
    pub(crate) in_calls: HashMap<Id, Vec<model::VoiceMember>>,
    /// Whether this window is in a call, and which.
    pub(crate) call: (bool, String),
}

impl ChatApp {
    /// Keep the workspace's document of its agents, as the window reads it: the
    /// agent identity record every app of the workspace opens. Chat keeps it,
    /// and names every author it spells `agent:<id>` by it.
    /// Take the workspace's view of the members off the presence store.
    ///
    /// Called once per frame from `draw`. The presence store's member
    /// kinds come from the host's certified People projection; Chat never
    /// infers Agent identity from a payload kind, display name or id prefix.
    /// Names and colours remain presentation metadata and grant no authority.
    ///
    /// The workspace's answer **replaces** what was held rather than merging into
    /// it. Merging is how a peer who has gone keeps a name and a colour on
    /// everyone else's screen for the rest of the session: presence forgets
    /// them on leave, and anything downstream that only ever learned would go
    /// on offering a departed member in the `@` picker and painting their
    /// colour behind a mention.
    /// Take this frame's workspace: who is here and in which call, their names
    /// and colours, the rosters of the calls, and the authority the window
    /// acts under. Whether any of it changed, so the channels show it (also
    /// while the conversation is put away and not drawn). In a call they show
    /// who speaks, which moves every frame.
    pub(super) fn take_workspace(&mut self) -> bool {
        let permissions = ui::presence::frame::frame(PresenceCtx::permission_context).flatten();
        let perms_changed = permissions.is_some_and(|permissions| {
            let changed = self.perms.context() != &permissions;
            self.perms = ChatPermissions::from_context(permissions);
            changed
        });
        let names_before = self.workspace_names.clone();
        let presence = if let Some(capture) = self.capture_agent_presence.clone() {
            self.adopt_workspace(&capture);
            true
        } else if ui::presence::frame::frame(|presence| self.adopt_workspace(presence)).is_none() {
            // No presence this frame: nobody's answer says who is here, so the
            // resource's own words about each person stand (`live_status`).
            self.workspace_presence_answers = false;
            false
        } else {
            true
        };
        // The agents' names come from the workspace's own list, not presence: an agent that
        // works on the workspace's server is no device here, and it is named all the same.
        self.name_agents();
        if self.workspace_names != names_before {
            // A direct conversation is named by what the member list calls its people: read the
            // conversations again.
            self.synced = None;
        }
        if !presence {
            return perms_changed;
        }
        // Who is in a call is not presence: it arrives from the host and from
        // the call's own service while this frame is being built, and a call
        // this window never joined changes nothing else here — so a channel
        // names everyone the last build saw, not everyone the resource says.
        let rows = self.channel_rows();
        let changed = perms_changed || rows != self.shown_rows;
        if changed || self.call.active {
            self.show_sidebar();
            self.shown_rows = rows;
        }
        changed
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
        // Presence answered: from here on it, not the resource, says who is
        // here. An answer naming nobody besides this window's own person is
        // still an answer — the workspace is otherwise empty.
        self.workspace_presence_answers = true;
        self.adopt_own_member(presence);
        self.workspace_voice = self
            .channels
            .iter()
            .filter(|channel| channel.kind == model::ChannelKind::Voice)
            .filter_map(|channel| {
                let key = Self::voice_presence_key(&channel.id);
                let mut people: Vec<MemberId> = Vec::new();
                for (user, _) in presence.carets.get(&key).into_iter().flatten() {
                    if !people.contains(user) {
                        people.push(user.clone());
                    }
                }
                (!people.is_empty()).then(|| (channel.id.clone(), people))
            })
            .collect();
        self.workspace_names
            .retain(|id, _| presence.names.contains_key(id));
        self.workspace_colors
            .retain(|id, _| presence.names.contains_key(id));
        // Every member the workspace lists is kept, named or not: the list is who a message can
        // go to, before they ever open Chat.
        for (id, name) in &presence.names {
            self.workspace_names
                .insert(id.clone(), name.trim().to_owned());
            // A colour is the same kind of answer: assigned per workspace so two
            // people never collide, and shared with the caret pill and cursor.
            self.workspace_colors
                .insert(id.clone(), presence.color_for(id));
        }
    }

    /// Take the workspace's answer to "which of these members am I".
    ///
    /// This window signs what it writes with the id its host gave it, the
    /// author every action of its person carries. A workspace that spells the
    /// same member differently names them too: lines recorded under
    /// that spelling are still this person's, so their name stays beside
    /// them.
    fn adopt_own_member(&mut self, presence: &PresenceCtx) {
        self.own_label = presence.self_label();
        if let Some(self_id) = presence.self_id()
            && *self_id != self.me.id
            && !self.former_ids.contains(self_id)
        {
            self.former_ids.push(self_id.clone());
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
            user: MemberId::person("u_dana"),
            name: None,
            service: None,
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
        let mine = format!("in-call:c_standup/{}", app.me.id.id());
        app.call.active = true;
        app.call.channel = "c_standup".into();
        assert!(app.take_workspace(), "joining here is a change");
        assert!(rows(&app).iter().any(|id| id == &mine), "{:?}", rows(&app));
        app.call.active = false;
        assert!(app.take_workspace(), "the call ending is a change");
        assert!(!rows(&app).iter().any(|id| id == &mine), "{:?}", rows(&app));
    }
}
