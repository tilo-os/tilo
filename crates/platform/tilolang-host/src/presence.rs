//! Where the people of a record are, and who of them is writing a message: presence, as it arrives
//! on an instance's own connection and as the instance says it of its person there.
//!
//! The broker relays every participant's presence frame on the record's route to the other
//! connections on it. The instance's views read what this device heard as lenses
//! (`lenses(c)`): who else is at each resource, and who is composing a message in the one the
//! detail view shows, so a conversation says "… is typing" without reading a draft. What the
//! instance says of its own person is where they are (the resource the detail view shows) and
//! whether they hold a draft there, said once each time it changes.

use std::collections::HashMap;

use collaboration::{CollabEvent, CollabRequest, PresencePayload};
use tilolang_tilo::instance::Instance;
use tilolang_tilo::runtime::value::ResourceRef;
use ui::permissions::UserId;
use ui::presence::{PresenceCtx, PresenceKey};

/// What an instance heard of the record's participants, and what it last said of its own.
pub struct Presence {
    app: String,
    me: String,
    heard: PresenceCtx,
    /// Where the instance last said its person is.
    said_at: Option<PresenceKey>,
    /// Where the instance last said its person writes a message, while they do.
    said_composing: Option<PresenceKey>,
}

impl Presence {
    /// Presence in the records of the app `app`, for the person `me`.
    #[must_use]
    pub fn new(app: &str, me: &str) -> Self {
        let mut heard = PresenceCtx::default();
        // Frames of the person's own other windows are theirs, never someone else here.
        heard.set_self_participant(UserId(me.to_owned()), me.to_owned());
        Self {
            app: app.to_owned(),
            me: me.to_owned(),
            heard,
            said_at: None,
            said_composing: None,
        }
    }

    /// Take `event` when it is a presence frame the broker relayed. Whether it was one.
    pub fn take(&mut self, event: &CollabEvent) -> bool {
        let CollabEvent::RemotePresenceUpdate {
            user_id,
            key,
            payload,
        } = event
        else {
            return false;
        };
        self.heard
            .apply_remote_presence(user_id.clone(), key.clone(), payload.clone());
        true
    }

    /// Tell `instance` who else is at each resource of the app, and who is composing a message
    /// in the resource its detail view shows.
    pub fn show(&mut self, instance: &mut Instance) {
        let now = ui::time::now();
        self.heard.prune_composing(now);
        let composing: Vec<String> = instance.detail_target().map_or_else(Vec::new, |target| {
            self.heard
                .composing_at(&self.key(target), now)
                .into_iter()
                .map(|user| user.0)
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
                    .find(|(user, _)| user.0 != self.me)
                    .map(|(user, _)| (id.clone(), user.0.clone())),
                _ => None,
            })
            .collect()
    }
}
