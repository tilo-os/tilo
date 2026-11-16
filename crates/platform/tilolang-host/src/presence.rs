//! Who the people of a record are, where they are, and who of them is writing a message: the
//! workspace's member list and presence, as they arrive on an instance's own connection, and
//! presence as the instance says it of its person there.
//!
//! The broker hands every connection the workspace's member list (`CollabEvent::Context`): what
//! the instance's views call each person, its own person among them, as a native app's window
//! calls them. It relays every participant's presence frame on the record's route to the other
//! connections on it. The instance's views read what this device heard as lenses
//! (`lenses(c)`): who else is at each resource, and who is composing a message in the one the
//! detail view shows, so a conversation says "… is typing" without reading a draft. What the
//! instance says of its own person is where they are (the resource the detail view shows) and
//! whether they hold a draft there: where, once each time it changes; a draft, again while they
//! type in it ([`ui::presence::DraftSaid`]).

use std::collections::HashMap;

use collaboration::{CollabEvent, CollabRequest, PresencePayload};
use tilolang_tilo::instance::Instance;
use tilolang_tilo::runtime::value::ResourceRef;
use ui::permissions::MemberId;
use ui::presence::{DraftSaid, PresenceCtx, PresenceKey};

/// What an instance heard of the record's participants, and what it last said of its own.
pub struct Presence {
    app: String,
    me: MemberId,
    heard: PresenceCtx,
    /// Where the instance last said its person is.
    said_at: Option<PresenceKey>,
    /// What the instance last said of the draft its person writes.
    said_composing: DraftSaid,
    /// Whether the member list changed since the instance was last told what people are called.
    renamed: bool,
}

impl Presence {
    /// Presence in the records of the app `app`, for the person `me`.
    #[must_use]
    pub fn new(app: &str, me: &str) -> Self {
        let mut heard = PresenceCtx::default();
        // Frames of the person's own other windows are theirs, never someone else here. Their
        // name is the member list's to say.
        heard.set_self_participant(MemberId::person(me), "");
        Self {
            app: app.to_owned(),
            me: MemberId::person(me),
            heard,
            said_at: None,
            said_composing: DraftSaid::default(),
            renamed: true,
        }
    }

    /// Take `event` when it is a presence frame the broker relayed, or the workspace's member
    /// list. Whether it was a presence frame, which nothing else takes; the member list also
    /// names the apps installed here, which the host reads.
    pub fn take(&mut self, event: &CollabEvent) -> bool {
        match event {
            CollabEvent::RemotePresenceUpdate {
                member,
                key,
                payload,
            } => {
                self.heard
                    .apply_remote_presence(member.clone(), key.clone(), payload.clone());
                true
            }
            CollabEvent::Context { members, .. } => {
                self.heard.apply_members(members.clone());
                if self.heard.self_id().is_none() {
                    // A list without the person: their own frames are still theirs.
                    self.heard.set_self_participant(self.me.clone(), "");
                }
                self.renamed = true;
                false
            }
            _ => false,
        }
    }

    /// How long until someone heard composing without a word since drops out: the host wakes
    /// then, so "… is typing" from someone whose window vanished goes with nothing else
    /// happening. `None` while nobody composes.
    #[must_use]
    pub fn wakes_in(&self) -> Option<std::time::Duration> {
        let ends = self.heard.composing_ends()?;
        Some(ends.saturating_duration_since(ui::time::now()))
    }

    /// Tell `instance` what people are called, who else is at each resource of the app, and who
    /// is composing a message in the resource its detail view shows.
    pub fn show(&mut self, instance: &mut Instance) {
        if std::mem::take(&mut self.renamed) {
            let me = self.heard.self_label().unwrap_or_default();
            let others = self
                .heard
                .names
                .keys()
                .map(|user| (user.clone(), self.heard.display_name(user)))
                .collect();
            instance.name_people(me, others);
        }
        let now = ui::time::now();
        self.heard.prune_composing(now);
        let composing: Vec<MemberId> = instance.detail_target().map_or_else(Vec::new, |target| {
            self.heard
                .composing_at(&self.key(target), now)
                .into_iter()
                .collect()
        });
        instance.set_peers_and_composers(self.peers(), &composing);
    }

    /// Say where `instance`'s person is, when that changed since it was last said, and whether
    /// they write a message there, through `send`: again while they type, and a draft emptied at
    /// once, under the key it was under, so nobody reads them typing after they stopped.
    pub fn say(&mut self, instance: &Instance, send: impl FnMut(CollabRequest)) {
        let at = instance.detail_target().map(|target| self.key(target));
        let draft = instance
            .composing_in()
            .map(|(target, revision)| (self.key(target), revision));
        self.say_at(at, draft, ui::time::now(), send);
    }

    /// [`Self::say`] with the person at `at`, writing `draft`, at `now`.
    fn say_at(
        &mut self,
        at: Option<PresenceKey>,
        draft: Option<(PresenceKey, u64)>,
        now: ui::time::Instant,
        mut send: impl FnMut(CollabRequest),
    ) {
        if at != self.said_at {
            if let Some(key) = &at {
                send(CollabRequest::Presence {
                    key: key.clone(),
                    payload: PresencePayload::Selection { start: 0, end: 0 },
                });
            }
            self.said_at = at;
        }
        if let Some((key, payload)) = self.said_composing.look(draft, now) {
            send(CollabRequest::Presence { key, payload });
        }
    }

    /// The key a resource of the app is present under: the same for every participant.
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

    const APP: &str = "so.tilo.lang.chat";

    fn channel() -> PresenceKey {
        PresenceKey::resource(APP, "channel-1", "TextChannel")
    }

    fn heard(presence: &mut Presence, user: &str, payload: PresencePayload) {
        presence.take(&CollabEvent::RemotePresenceUpdate {
            member: MemberId::person(user),
            key: channel(),
            payload,
        });
    }

    #[test]
    fn someone_at_a_resource_by_their_caret_is_there_as_by_their_selection() {
        let mut presence = Presence::new(APP, "bo");
        // Another device hands a selection of nothing over as a caret.
        heard(&mut presence, "ada", PresencePayload::Caret { pos: 0 });
        heard(&mut presence, "bo", PresencePayload::Caret { pos: 0 });
        assert_eq!(
            presence.peers(),
            HashMap::from([("channel-1".to_owned(), MemberId::person("ada"))])
        );
        let mut selected = Presence::new(APP, "bo");
        heard(
            &mut selected,
            "ada",
            PresencePayload::Selection { start: 0, end: 0 },
        );
        assert_eq!(selected.peers(), presence.peers());
    }

    /// Ada's instance says what she does each 100 ms of a test-driven clock for 30 s, typing a
    /// key each step until `typed` ms, then sending her draft or leaving it; Bo's instance hears
    /// it. Whether Bo reads her typing in the channel at each step, by its ms.
    fn read_by_bo(typed: u64, sent: bool) -> Vec<(u64, bool)> {
        let start = ui::time::now();
        let mut ada = Presence::new(APP, "ada");
        let mut bo = Presence::new(APP, "bo");
        let mut read = Vec::new();
        for at in (0..=30_000).step_by(100) {
            let now = start
                .checked_add(std::time::Duration::from_millis(at))
                .expect("a clock 30 s on");
            let draft = (at < typed || !sent).then(|| (channel(), at.min(typed)));
            ada.say_at(Some(channel()), draft, now, |request| {
                if let CollabRequest::Presence { key, payload } = request {
                    bo.heard
                        .apply_remote_presence_at(MemberId::person("ada"), key, payload, now);
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
        let linger = u64::try_from(ui::presence::COMPOSING_LINGER.as_millis()).expect("seconds");
        assert!(
            kept.iter()
                .all(|(at, typing)| !*typing || at.saturating_sub(typed) < linger),
            "a draft left standing stops reading as typing within the linger"
        );
        let sent = read_by_bo(typed, true);
        assert!(
            sent.iter().all(|(at, typing)| (*at < typed) == *typing),
            "typing until sent, and not a moment after"
        );
    }
}
