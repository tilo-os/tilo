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
//! whether they hold a draft there, said once each time it changes.

use std::collections::HashMap;

use collaboration::{CollabEvent, CollabRequest, PresencePayload};
use tilolang_tilo::instance::Instance;
use tilolang_tilo::runtime::value::ResourceRef;
use ui::permissions::MemberId;
use ui::presence::{PresenceCtx, PresenceKey};

/// What an instance heard of the record's participants, and what it last said of its own.
pub struct Presence {
    app: String,
    me: MemberId,
    heard: PresenceCtx,
    /// Where the instance last said its person is.
    said_at: Option<PresenceKey>,
    /// Where the instance last said its person writes a message, while they do.
    said_composing: Option<PresenceKey>,
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
            said_composing: None,
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
            let me = self
                .heard
                .self_label()
                .or_else(collaboration::local_identity::own_name)
                .unwrap_or_default();
            let others = self
                .heard
                .names
                .keys()
                .map(|user| (user.key(), self.heard.display_name(user)))
                .collect();
            instance.name_people(me, others);
        }
        let now = ui::time::now();
        self.heard.prune_composing(now);
        let composing: Vec<String> = instance.detail_target().map_or_else(Vec::new, |target| {
            self.heard
                .composing_at(&self.key(target), now)
                .into_iter()
                .map(|user| user.key())
                .collect()
        });
        instance.set_peers_and_composers(self.peers(), &composing);
    }

    /// Say where `instance`'s person is and whether they write a message there, through `send`,
    /// when either changed since it was last said. A draft emptied says so at once, under the
    /// key it was under, so nobody reads them typing after they stopped.
    pub fn say(&mut self, instance: &Instance, mut send: impl FnMut(CollabRequest)) {
        let at = instance.detail_target().map(|target| self.key(target));
        if at != self.said_at {
            if let Some(key) = &at {
                send(CollabRequest::Presence {
                    key: key.clone(),
                    payload: PresencePayload::Selection { start: 0, end: 0 },
                });
            }
            self.said_at = at;
        }
        let composing = instance.composing_in().map(|target| self.key(target));
        if composing != self.said_composing {
            match (&composing, &self.said_composing) {
                (Some(key), _) => send(CollabRequest::Presence {
                    key: key.clone(),
                    payload: PresencePayload::Composing { composing: true },
                }),
                (None, Some(key)) => send(CollabRequest::Presence {
                    key: key.clone(),
                    payload: PresencePayload::Composing { composing: false },
                }),
                (None, None) => {}
            }
            self.said_composing = composing;
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

    /// Someone other than the person at each resource of the app, by the resource's id.
    fn peers(&self) -> HashMap<String, String> {
        self.heard
            .selections
            .iter()
            .filter_map(|(key, at)| match key {
                PresenceKey::Resource { app, id, .. } if *app == self.app => at
                    .iter()
                    .find(|(user, _)| *user != self.me)
                    .map(|(user, _)| (id.clone(), user.key())),
                _ => None,
            })
            .collect()
    }
}
