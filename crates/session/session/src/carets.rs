//! Each person's caret, in everyone else's windows on the same record: where the person here types
//! is said in their presence, as the selection of the window they work in, and where each other
//! person here types reaches this device's apps as their caret, which the app draws under their
//! name, in their colour, as it draws an agent's.
//!
//! What the person types in goes on the workspace's live plane with the rest of their word about
//! themselves ([`collab_types::presence::ShellSelection`]): the editor and the range, as live
//! anchors every member's copy of the app resolves against its own editors. Nothing of it is kept:
//! a caret is where someone is now.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use collab_types::presence::{PresenceKey, PresencePayload, ShellSelection};
use runtime_collab::event::CollabEvent;
use tilo_action_broker::collab_broker::{CrdtRoute, FanoutState, WorkspaceSubscription};
use tilo_lock::MutexRecover as _;

/// Where the person here last put their caret or selection: the app they did it in, the editor,
/// and the range.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Typing {
    app: String,
    key: PresenceKey,
    start: usize,
    end: usize,
}

/// The carets of the people in this session's workspaces, as its apps show them and say them.
pub(crate) struct Carets {
    fanout: Arc<FanoutState>,
    /// The person here's own caret, as their apps last published it.
    own: Arc<Mutex<Option<Typing>>>,
    /// Who this session runs for, as the apps' presence names them: hex of their root.
    person: Arc<Mutex<Option<String>>>,
    /// Held so this device's apps' presence keeps reaching [`Self::own`].
    subscription: Arc<WorkspaceSubscription>,
    /// What each other person's caret was last handed to the apps as, by workspace and person.
    shown: RefCell<HashMap<(String, String), (PresenceKey, PresencePayload)>>,
    /// How far each workspace's people were read.
    read: RefCell<HashMap<String, u64>>,
}

impl Carets {
    /// The carets of the people in this session's workspaces, shown to the apps `fanout` serves.
    /// `wake` runs when the person here moved their caret, so it is said on the next pass.
    pub(crate) fn new(fanout: Arc<FanoutState>, wake: tilo_service::Wake) -> Self {
        let own = Arc::new(Mutex::new(None));
        let person: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let subscription = {
            let (own, person) = (Arc::clone(&own), Arc::clone(&person));
            fanout.subscribe_workspaces(
                Vec::new(),
                Box::new(move |event, route| {
                    // A local app's presence names the app it came from; what this session
                    // hands the apps of someone else's names only the workspace.
                    let CollabEvent::RemotePresenceUpdate {
                        user_id,
                        key,
                        payload,
                    } = event
                    else {
                        return;
                    };
                    if route.app_id.is_empty()
                        || person.lock_recover().as_deref() != Some(user_id.as_str())
                    {
                        return;
                    }
                    let (start, end) = match payload {
                        PresencePayload::Caret { pos } => (*pos, *pos),
                        PresencePayload::Selection { start, end } => (*start, *end),
                        _ => return,
                    };
                    let typing = Typing {
                        app: route.app_id.clone(),
                        key: key.clone(),
                        start,
                        end,
                    };
                    let mut own = own.lock_recover();
                    if own.as_ref() != Some(&typing) {
                        *own = Some(typing);
                        drop(own);
                        wake();
                    }
                }),
            )
        };
        Self {
            fanout,
            own,
            person,
            subscription,
            shown: RefCell::new(HashMap::new()),
            read: RefCell::new(HashMap::new()),
        }
    }

    /// The person here's caret as their presence says it, when it is in `app`, the app of the
    /// window they work in.
    pub(crate) fn said_in(&self, app: &str) -> Option<ShellSelection> {
        let typing = self
            .own
            .lock_recover()
            .clone()
            .filter(|typing| typing.app == app)?;
        let (anchor, head) =
            tilo_relay_protocol::live::presence_anchors(&typing.key, typing.start, typing.end)?;
        Some(ShellSelection {
            anchor: hex::encode(anchor.cursor),
            head: hex::encode(head.cursor),
        })
    }

    /// Follow who is here in `workspaces`: the person here's apps are heard in each of them, and
    /// every other person's caret there is handed to this device's apps as it moves.
    pub(crate) fn follow(&self, sync: &crate::sync::Client, workspaces: Vec<String>) {
        let person = sync.own().user.filter(|user| !user.is_empty());
        person.clone_into(&mut *self.person.lock_recover());
        self.subscription.set_workspaces(workspaces.clone());
        for workspace in workspaces {
            let revision = sync.here_revision(&workspace);
            if self.read.borrow().get(&workspace) == Some(&revision) {
                continue;
            }
            self.read.borrow_mut().insert(workspace.clone(), revision);
            let mut placed: HashMap<String, (PresenceKey, PresencePayload)> = HashMap::new();
            for member in sync.members(&workspace) {
                // Who they are: their certificate's person, else the person the workspace admitted
                // their device as.
                let user = member.user.clone().or_else(|| {
                    let device = <[u8; 32]>::try_from(hex::decode(&member.publisher).ok()?).ok()?;
                    sync.governed_person(&workspace, iroh::EndpointId::from_bytes(&device).ok()?)
                });
                let Some(user) = user.filter(|user| Some(user) != person.as_ref()) else {
                    continue;
                };
                if member.agent {
                    continue;
                }
                if let Some(caret) = member.selection.as_ref().and_then(caret_of) {
                    placed.insert(user, caret);
                }
            }
            self.show(&workspace, placed);
        }
    }

    /// Hand the apps of `workspace` each other person's caret that moved, and take away the caret
    /// of each one who has none any more.
    fn show(&self, workspace: &str, placed: HashMap<String, (PresenceKey, PresencePayload)>) {
        let mut shown = self.shown.borrow_mut();
        let gone: Vec<(String, PresenceKey)> = shown
            .iter()
            .filter(|((held, user), _)| held == workspace && !placed.contains_key(user))
            .map(|((_, user), (key, _))| (user.clone(), key.clone()))
            .collect();
        for (user, key) in gone {
            shown.remove(&(workspace.to_owned(), user.clone()));
            self.hand(workspace, user, key, PresencePayload::Left);
        }
        for (user, caret) in placed {
            let entry = (workspace.to_owned(), user.clone());
            if shown.get(&entry) == Some(&caret) {
                continue;
            }
            shown.insert(entry, caret.clone());
            let (key, payload) = caret;
            self.hand(workspace, user, key, payload);
        }
    }

    /// Hand one person's presence in `workspace` to the apps there, as any member's reaches them.
    fn hand(&self, workspace: &str, user: String, key: PresenceKey, payload: PresencePayload) {
        let event = CollabEvent::RemotePresenceUpdate {
            user_id: user,
            key,
            payload,
        };
        self.fanout
            .fanout_to_route(&event, 0, &CrdtRoute::cluster(workspace));
    }
}

/// The caret or selection `selection` names, as an app's presence carries it.
fn caret_of(selection: &ShellSelection) -> Option<(PresenceKey, PresencePayload)> {
    let anchor = tilo_relay_protocol::live::LiveAnchor {
        cursor: hex::decode(&selection.anchor).ok()?,
    };
    let head = tilo_relay_protocol::live::LiveAnchor {
        cursor: hex::decode(&selection.head).ok()?,
    };
    let (key, start, end) = tilo_relay_protocol::live::read_presence_anchors(&anchor, &head)?;
    let payload = if start == end {
        PresencePayload::Caret { pos: start }
    } else {
        PresencePayload::Selection { start, end }
    };
    Some((key, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_caret_said_in_presence_reads_back_as_the_editor_and_place_it_was_in() {
        let key = PresenceKey::Widget {
            path: vec![collab_types::presence::PathSegment::Field("block-1".into())],
        };
        let (anchor, head) = tilo_relay_protocol::live::presence_anchors(&key, 7, 7).unwrap();
        let said = ShellSelection {
            anchor: hex::encode(anchor.cursor),
            head: hex::encode(head.cursor),
        };
        assert_eq!(
            caret_of(&said),
            Some((key.clone(), PresencePayload::Caret { pos: 7 }))
        );
        let (anchor, head) = tilo_relay_protocol::live::presence_anchors(&key, 2, 9).unwrap();
        let said = ShellSelection {
            anchor: hex::encode(anchor.cursor),
            head: hex::encode(head.cursor),
        };
        assert_eq!(
            caret_of(&said),
            Some((key, PresencePayload::Selection { start: 2, end: 9 }))
        );
    }
}
