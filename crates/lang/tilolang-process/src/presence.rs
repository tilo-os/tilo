//! Where the people of a document are, and who of them is writing a message: the presence words
//! that arrive on the connection the launcher handed this process, and presence as the instance
//! says it of its person there.
//!
//! Who the workspace's members are, and what they are called, is its `workspace/members`
//! resource: the instance's host reads it through the store and names the window's views with
//! it. The device's live presence says every word this process says in its workspace, and hands
//! back what the workspace's live session hears from the other devices. The instance's views
//! read what this device heard as lenses
//! (`lenses(c)`): who else is at each resource, and who is composing a message in the one the
//! detail view shows, so a conversation says "… is typing" without reading a draft. What the
//! instance says of its own person is where they are (the resource the detail view shows) and
//! whether they hold a draft there: where, once each time it changes; a draft, again while they
//! type in it ([`tilo_ui_base::presence::DraftSaid`]).

use std::collections::HashMap;

use collab_types::presence::{Heard, Payload, PresencePayload, Said};
use tilo_render_protocol::patch::ToUi;
use tilo_ui_base::permissions::MemberId;
use tilo_ui_base::{presence::DraftSaid, presence::PresenceCtx, presence::PresenceKey};
use tilolang_tilo::instance::Instance;
use tilolang_tilo::runtime::value::ResourceRef;
use ui_core::component_lifecycle::Heard as Shown;

/// What an instance heard of the document's members, and what it last said of its own.
pub struct Presence {
    app: String,
    me: MemberId,
    heard: PresenceCtx,
    /// Where the instance last said its person is.
    said_at: Option<PresenceKey>,
    /// What the instance last said of the draft its person writes.
    said_composing: DraftSaid,
    /// What the window shows of the people here as a window with a connection of its own does,
    /// waiting to be sent to it: each step of an action taken on a resource of the app, which
    /// outlines the control the action stands for.
    for_window: Vec<Shown>,
}

impl Presence {
    /// Presence in the documents of the app `app`, for the person `me`.
    #[must_use]
    pub fn new(app: &str, me: &str) -> Self {
        let mut heard = PresenceCtx::default();
        // Frames of the person's own other windows are theirs, never someone else here. Their
        // name is the member list's to say.
        heard.set_self_member(MemberId::person(me), "");
        Self {
            app: app.to_owned(),
            me: MemberId::person(me),
            heard,
            said_at: None,
            said_composing: DraftSaid::default(),
            for_window: Vec::new(),
        }
    }

    /// Take `heard`, the words the device's live session heard in this app's workspace since the
    /// last look. Whether any was a word these views are shown.
    pub fn heard(&mut self, heard: impl IntoIterator<Item = Heard>) -> bool {
        let mut took = false;
        for word in heard {
            let Ok(Some(payload)) = PresencePayload::from_live(word.payload) else {
                continue;
            };
            if let PresencePayload::Interaction(interaction) = &payload
                && matches!(&word.key, PresenceKey::Resource { app, .. } if *app == self.app)
            {
                self.for_window
                    .push(Shown::Interaction((**interaction).clone()));
            }
            self.heard
                .apply_remote_presence(word.member, word.key, payload);
            took = true;
        }
        took
    }

    /// How long until someone heard composing without a word since drops out: the host wakes
    /// then, so "… is typing" from someone whose window vanished goes with nothing else
    /// happening. `None` while nobody composes.
    #[must_use]
    pub fn wakes_in(&self) -> Option<std::time::Duration> {
        let ends = self.heard.composing_ends()?;
        Some(ends.saturating_duration_since(ui_core::time::now()))
    }

    /// Tell `instance` who else is at each resource of the app and who is composing a message in
    /// the resource its detail view shows. What people are called and how they are drawn is the
    /// workspace's `workspace/members` resource, which the window's host reads through the store.
    pub fn show(&mut self, instance: &mut Instance) {
        let now = ui_core::time::now();
        self.heard.prune_composing(now);
        let composing: Vec<MemberId> = instance.detail_target().map_or_else(Vec::new, |target| {
            self.heard
                .composing_at(&self.key(target), now)
                .into_iter()
                .collect()
        });
        instance.set_peers_and_composers(self.peers(), &composing);
        // The window has no connection of its own: what a presence mark it draws says of a
        // member is named here, from the same read of the workspace's members' resource that
        // names the app's own views.
        if let Some(names) = instance.take_member_names() {
            self.for_window.push(Shown::Names(names));
        }
        for heard in &mut self.for_window {
            if let Shown::Interaction(interaction) = heard {
                instance.heard(interaction);
            }
        }
        for heard in self.take_for_window() {
            instance.say(ToUi::Heard(heard));
        }
    }

    /// What the window is to show of the people here, as it is sent ([`ToUi::Heard`]).
    fn take_for_window(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.for_window)
            .iter()
            .filter_map(|heard| {
                serde_json::to_vec(heard)
                    .inspect_err(|error| tracing::warn!(%error, "what was heard does not encode"))
                    .ok()
            })
            .collect()
    }

    /// Say where `instance`'s person is, when that changed since it was last said, and whether
    /// they write a message there, through `send`: again while they type, and a draft emptied at
    /// once, under the key it was under, so nobody reads them typing after they stopped.
    pub fn say(&mut self, instance: &Instance, send: impl FnMut(Said)) {
        let at = instance.detail_target().map(|target| self.key(target));
        let draft = instance
            .composing_in()
            .map(|(target, revision)| (self.key(target), revision));
        self.say_at(at, draft, ui_core::time::now(), send);
    }

    /// [`Self::say`] with the person at `at`, writing `draft`, at `now`.
    fn say_at(
        &mut self,
        at: Option<PresenceKey>,
        draft: Option<(PresenceKey, u64)>,
        now: ui_core::time::Instant,
        mut send: impl FnMut(Said),
    ) {
        if at != self.said_at {
            if let Some(key) = &at {
                send(Said {
                    key: key.clone(),
                    payload: Payload::Selection { start: 0, end: 0 },
                });
            }
            self.said_at = at;
        }
        if let Some((key, payload)) = self.said_composing.look(draft, now)
            && let Some(payload) = payload.to_live()
        {
            send(Said { key, payload });
        }
    }

    /// The key a resource of the app is present under: the same for every member.
    fn key(&self, target: &ResourceRef) -> PresenceKey {
        PresenceKey::Resource {
            app: self.app.clone(),
            id: target.id_string(),
            kind: target.ty.clone(),
        }
    }

    /// Someone other than the person at each resource of the app, by the resource's id: where
    /// their selection is, or their caret, which is how a selection of nothing reaches a device
    /// from another one.
    fn peers(&self) -> HashMap<String, MemberId> {
        let selected = self
            .heard
            .selections
            .iter()
            .map(|(key, at)| (key, at.iter().map(|(user, _)| user).collect::<Vec<_>>()));
        let caret = self
            .heard
            .carets
            .iter()
            .map(|(key, at)| (key, at.iter().map(|(user, _)| user).collect::<Vec<_>>()));
        let mut peers = HashMap::new();
        for (key, users) in selected.chain(caret) {
            if let PresenceKey::Resource { app, id, .. } = key
                && *app == self.app
                && let Some(user) = users.into_iter().find(|user| **user != self.me)
            {
                peers.entry(id.clone()).or_insert_with(|| user.clone());
            }
        }
        peers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP: &str = "so.tilo.chat";

    fn channel() -> PresenceKey {
        PresenceKey::resource(APP, "channel-1", "TextChannel")
    }

    fn heard(presence: &mut Presence, user: &str, payload: &PresencePayload) {
        presence.heard([Heard {
            member: MemberId::person(user),
            key: channel(),
            payload: payload.to_live().expect("a word the live session carries"),
        }]);
    }

    /// An agent's step of `action` on `resource`, of kind `kind`, of the app `app`.
    fn step(app: &str, resource: &str, kind: &str) -> (PresenceKey, PresencePayload) {
        let interaction = tilo_actions::SemanticInteraction {
            version: tilo_actions::INTERACTION_VERSION,
            invocation: tilo_actions::InvocationId::new(),
            sequence: 1,
            actor: tilo_actions::Acting::agent("scout", "ada"),
            phase: tilo_actions::InteractionPhase::Applying,
            action: tilo_actions::InteractionAction {
                id: "check".into(),
                label: "Check".into(),
                app: None,
                step: None,
                creates: None,
                did: None,
            },
            resource: tilo_actions::ActionResource {
                id: resource.into(),
                kind: kind.into(),
                label: "Plan".into(),
            },
            anchor: tilo_actions::InteractionAnchor::Resource,
            group: None,
        };
        (
            PresenceKey::resource(app, resource, kind),
            PresencePayload::Interaction(Box::new(interaction)),
        )
    }

    /// What `presence` sends its window, as the window reads it.
    fn sent(presence: &mut Presence) -> Vec<Shown> {
        presence
            .take_for_window()
            .iter()
            .map(|heard| serde_json::from_slice(heard).expect("the window reads what is sent"))
            .collect()
    }

    /// The window of a hosted app has no connection of its own: the steps of an agent's action
    /// on the app's resources reach it from the instance's connection, so the control the action
    /// stands for is outlined as in any window. A step in another app is that app's window's.
    /// (What people are called and how they are drawn is the workspace's `workspace/members`
    /// resource, which the window's host reads through the store.)
    #[test]
    fn an_agents_step_on_the_apps_resource_reaches_the_window() {
        let mut presence = Presence::new(APP, "bo");
        for (app, resource) in [(APP, "channel-1"), ("so.tilo.tasks", "task-1")] {
            let (key, payload) = step(app, resource, "TextChannel");
            assert!(presence.heard([Heard {
                member: MemberId::agent("scout", "ada"),
                key,
                payload: payload.to_live().expect("a word the live session carries"),
            }]));
        }
        let heard = sent(&mut presence);
        let [Shown::Interaction(interaction)] = heard.as_slice() else {
            panic!("the step on this app's resource alone: {heard:?}");
        };
        assert_eq!(interaction.resource.id, "channel-1");
        assert!(sent(&mut presence).is_empty(), "each is sent once");
    }

    #[test]
    fn someone_at_a_resource_by_their_caret_is_there_as_by_their_selection() {
        let mut presence = Presence::new(APP, "bo");
        // Another device hands a selection of nothing over as a caret.
        heard(&mut presence, "ada", &PresencePayload::Caret { pos: 0 });
        heard(&mut presence, "bo", &PresencePayload::Caret { pos: 0 });
        assert_eq!(
            presence.peers(),
            HashMap::from([("channel-1".to_owned(), MemberId::person("ada"))])
        );
        let mut selected = Presence::new(APP, "bo");
        heard(
            &mut selected,
            "ada",
            &PresencePayload::Selection { start: 0, end: 0 },
        );
        assert_eq!(selected.peers(), presence.peers());
    }

    /// Ada's instance says what she does each 100 ms of a test-driven clock for 30 s, typing a
    /// key each step until `typed` ms, then sending her draft or leaving it; Bo's instance hears
    /// it. Whether Bo reads her typing in the channel at each step, by its ms.
    fn read_by_bo(typed: u64, sent: bool) -> Vec<(u64, bool)> {
        let start = ui_core::time::now();
        let mut ada = Presence::new(APP, "ada");
        let mut bo = Presence::new(APP, "bo");
        let mut read = Vec::new();
        for at in (0..=30_000).step_by(100) {
            let now = start
                .checked_add(std::time::Duration::from_millis(at))
                .expect("a clock 30 s on");
            let draft = (at < typed || !sent).then(|| (channel(), at.min(typed)));
            ada.say_at(Some(channel()), draft, now, |said| {
                if let Ok(Some(payload)) = PresencePayload::from_live(said.payload) {
                    bo.heard.apply_remote_presence_at(
                        MemberId::person("ada"),
                        said.key,
                        payload,
                        now,
                    );
                }
            });
            bo.heard.prune_composing(now);
            let typing = bo
                .heard
                .composing_at(&channel(), now)
                .contains(&MemberId::person("ada"));
            read.push((at, typing));
        }
        read
    }

    #[test]
    fn someone_typing_in_a_conversation_reads_as_typing_until_they_send_or_stop() {
        let typed = 20_000;
        let kept = read_by_bo(typed, false);
        assert!(
            kept.iter().all(|(at, typing)| *at >= typed || *typing),
            "typing for 20 s reads as typing throughout"
        );
        let linger =
            u64::try_from(tilo_ui_base::presence::COMPOSING_LINGER.as_millis()).expect("seconds");
        assert!(
            kept.iter()
                .all(|(at, typing)| !*typing || at.saturating_sub(typed) < linger),
            "a draft left in place stops reading as typing within the linger"
        );
        let sent = read_by_bo(typed, true);
        assert!(
            sent.iter().all(|(at, typing)| (*at < typed) == *typing),
            "typing until sent, and not a moment after"
        );
    }
}
