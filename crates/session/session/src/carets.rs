//! Each person's caret, in everyone else's windows on the same record: where the person here types
//! is said in their presence, as the selection of the window they work in, and where each other
//! person here types reaches this device's apps as their caret, which the app draws under their
//! name, in their colour, as it draws an agent's. Whether they are writing a message there rides
//! with it, so a conversation on every other device says they are typing.
//!
//! What the person types in goes on the workspace's live session with the rest of their word about
//! themselves ([`collab_types::presence::ShellSelection`]): the editor and the range, as live
//! anchors every member's copy of the app resolves against its own editors, and whether a draft is
//! under way there. Nothing of it is kept: a caret is where someone is now.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use collab_types::presence::{PresenceKey, PresencePayload, ShellSelection};
use runtime_collab::event::CollabEvent;
use tilo_lock::MutexRecover as _;
use tilo_workspace_service::apps::collab_broker::{CrdtRoute, FanoutState, WorkspaceSubscription};

/// Where the person here last put their caret or selection: the app they did it in, the editor,
/// the range, and whether they write a message there.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Typing {
    app: String,
    key: PresenceKey,
    start: usize,
    end: usize,
    /// 0 with no draft, else how many times the app said it under way
    /// ([`ShellSelection::composing`]).
    composing: u32,
}

impl Typing {
    /// What `payload`, which the person's app in `app` published under `key`, makes of where they
    /// type, from `was`: a caret or selection moves it (keeping their draft where it stays), and a
    /// draft started, said again, or stopped says so there. `None` for what says nothing of
    /// either.
    fn after(
        was: Option<&Self>,
        app: &str,
        key: &PresenceKey,
        payload: &PresencePayload,
    ) -> Option<Self> {
        let same = was.filter(|was| was.app == app && was.key == *key);
        match payload {
            PresencePayload::Caret { pos } => Some(Self::at(app, key, *pos, *pos, same)),
            PresencePayload::Selection { start, end } => {
                Some(Self::at(app, key, *start, *end, same))
            }
            PresencePayload::Composing { composing } => Some(Self {
                composing: if *composing {
                    same.map_or(0, |was| was.composing).wrapping_add(1).max(1)
                } else {
                    0
                },
                ..same
                    .cloned()
                    .unwrap_or_else(|| Self::at(app, key, 0, 0, None))
            }),
            _ => None,
        }
    }

    fn at(app: &str, key: &PresenceKey, start: usize, end: usize, same: Option<&Self>) -> Self {
        Self {
            app: app.to_owned(),
            key: key.clone(),
            start,
            end,
            composing: same.map_or(0, |was| was.composing),
        }
    }
}

/// Another person's caret as this device's apps were last handed it: where it is, and whether
/// they write a message there.
#[derive(Clone, Debug, PartialEq)]
struct Caret {
    key: PresenceKey,
    payload: PresencePayload,
    composing: u32,
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
    shown: RefCell<HashMap<(String, String), Caret>>,
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
                        member,
                        key,
                        payload,
                    } = event
                    else {
                        return;
                    };
                    if route.app_id.is_empty()
                        || person.lock_recover().as_deref() != member.person_id()
                    {
                        return;
                    }
                    let mut own = own.lock_recover();
                    let Some(typing) = Typing::after(own.as_ref(), &route.app_id, key, payload)
                    else {
                        return;
                    };
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
            tilo_server_protocol::live::presence_anchors(&typing.key, typing.start, typing.end)?;
        Some(ShellSelection {
            anchor: hex::encode(anchor.cursor),
            head: hex::encode(head.cursor),
            composing: typing.composing,
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
            let mut placed: HashMap<String, Caret> = HashMap::new();
            for member in sync.members(&workspace) {
                // Presence names its person as their device says; one that names no person's
                // root key has no caret to hand on.
                let Some(user) = member.user.filter(|user| {
                    Some(user) != person.as_ref() && collab_types::PersonId::parse(user).is_ok()
                }) else {
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

    /// Hand the apps of `workspace` each other person's caret that moved, and whether they write
    /// a message where it is when that changed, and take away the caret of each one who has none
    /// any more.
    fn show(&self, workspace: &str, placed: HashMap<String, Caret>) {
        let mut shown = self.shown.borrow_mut();
        let gone: Vec<(String, Caret)> = shown
            .iter()
            .filter(|((held, user), _)| held == workspace && !placed.contains_key(user))
            .map(|((_, user), caret)| (user.clone(), caret.clone()))
            .collect();
        for (user, caret) in gone {
            shown.remove(&(workspace.to_owned(), user.clone()));
            if caret.composing != 0 {
                self.hand(workspace, user.clone(), caret.key.clone(), composing(false));
            }
            self.hand(workspace, user, caret.key, PresencePayload::Left);
        }
        for (user, caret) in placed {
            let entry = (workspace.to_owned(), user.clone());
            let was = shown.insert(entry, caret.clone());
            if was.as_ref() == Some(&caret) {
                continue;
            }
            // A draft left behind where the caret was stops there.
            if let Some(was) = &was
                && was.composing != 0
                && was.key != caret.key
            {
                self.hand(workspace, user.clone(), was.key.clone(), composing(false));
            }
            if was
                .as_ref()
                .is_none_or(|was| was.key != caret.key || was.payload != caret.payload)
            {
                self.hand(workspace, user.clone(), caret.key.clone(), caret.payload);
            }
            // A draft said again keeps it shown: the apps drop one they hear nothing of.
            let writes = caret.composing != 0;
            if was.as_ref().map_or(writes, |was| {
                was.composing != caret.composing || was.key != caret.key
            }) {
                self.hand(workspace, user, caret.key, composing(writes));
            }
        }
    }

    /// Hand each agent's narrated work to the apps of its workspace, as any member's presence reaches
    /// them: the window showing the record the agent acts in draws its cursor at the control its
    /// action is bound to. Work naming no app, or no member, shows nowhere.
    pub(crate) fn narrate(&self, narrated: Vec<(String, tilo_actions::SemanticInteraction)>) {
        for (workspace, work) in narrated {
            let Some(app) = work.action.app.as_ref().map(|app| app.id.clone()) else {
                continue;
            };
            let Some(member) = work.actor.members().first().map(|member| (*member).clone()) else {
                continue;
            };
            let key = PresenceKey::resource(&app, &work.resource.id, &work.resource.kind);
            let event = CollabEvent::RemotePresenceUpdate {
                member,
                key,
                payload: PresencePayload::Interaction(Box::new(work)),
            };
            self.fanout
                .fanout_to_route(&event, 0, &CrdtRoute::cluster(&workspace));
        }
    }

    /// Hand one person's presence in `workspace` to the apps there, as any member's reaches them.
    fn hand(&self, workspace: &str, user: String, key: PresenceKey, payload: PresencePayload) {
        let event = CollabEvent::RemotePresenceUpdate {
            member: collab_types::MemberId::person(user),
            key,
            payload,
        };
        self.fanout
            .fanout_to_route(&event, 0, &CrdtRoute::cluster(workspace));
    }
}

/// A draft under way, or stopped, as an app's presence carries it.
const fn composing(composing: bool) -> PresencePayload {
    PresencePayload::Composing { composing }
}

/// The caret or selection `selection` names, as an app's presence carries it, and whether a draft
/// is under way there.
fn caret_of(selection: &ShellSelection) -> Option<Caret> {
    let anchor = tilo_server_protocol::live::LiveAnchor {
        cursor: hex::decode(&selection.anchor).ok()?,
    };
    let head = tilo_server_protocol::live::LiveAnchor {
        cursor: hex::decode(&selection.head).ok()?,
    };
    let (key, start, end) = tilo_server_protocol::live::read_presence_anchors(&anchor, &head)?;
    let payload = if start == end {
        PresencePayload::Caret { pos: start }
    } else {
        PresencePayload::Selection { start, end }
    };
    Some(Caret {
        key,
        payload,
        composing: selection.composing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: &str = "workspace";
    const ADA: &str = "ada";

    /// Where Ada writes: a channel of Chat.
    fn channel() -> PresenceKey {
        PresenceKey::resource("so.tilo.chat", "channel-1", "TextChannel")
    }

    /// What the fanout hands the apps of [`WORKSPACE`], each event, while its subscription lives.
    type Handed = (Arc<WorkspaceSubscription>, Arc<Mutex<Vec<PresencePayload>>>);

    /// Carets on a fanout of their own, for Ada, and what the fanout hands the apps.
    fn carets() -> (Carets, Arc<FanoutState>, Handed) {
        let fanout = Arc::new(FanoutState::new());
        let carets = Carets::new(Arc::clone(&fanout), Arc::new(|| {}));
        *carets.person.lock_recover() = Some(ADA.to_owned());
        carets
            .subscription
            .set_workspaces(vec![WORKSPACE.to_owned()]);
        let handed = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&handed);
        let apps = fanout.subscribe_workspaces(
            vec![WORKSPACE.to_owned()],
            Box::new(move |event, route| {
                if let CollabEvent::RemotePresenceUpdate { payload, .. } = event
                    && route.app_id.is_empty()
                {
                    sink.lock_recover().push(payload.clone());
                }
            }),
        );
        (carets, fanout, (apps, handed))
    }

    /// Ada's app in Chat says `payload` under the channel's key.
    fn ada_says(fanout: &FanoutState, payload: PresencePayload) {
        let origin = CrdtRoute::record("so.tilo.chat", WORKSPACE, "derived:main");
        let event = CollabEvent::RemotePresenceUpdate {
            member: collab_types::MemberId::person(ADA),
            key: channel(),
            payload,
        };
        fanout.fanout_presence(&event, 0, &origin.to_cluster_scope(), &origin);
    }

    tilo_test_env::scenario! {
        fn carets_and_drafts_ride_in_presence_to_the_apps;

        phase a_caret_said_in_presence_reads_back_as_the_editor_and_place_it_was_in;
        {
            let key = PresenceKey::Widget {
                path: vec![collab_types::presence::PathSegment::Field("block-1".into())],
            };
            let (anchor, head) = tilo_server_protocol::live::presence_anchors(&key, 7, 7).unwrap();
            let said = ShellSelection {
                anchor: hex::encode(anchor.cursor),
                head: hex::encode(head.cursor),
                composing: 1,
            };
            assert_eq!(
                caret_of(&said),
                Some(Caret {
                    key: key.clone(),
                    payload: PresencePayload::Caret { pos: 7 },
                    composing: 1,
                })
            );
            let (anchor, head) = tilo_server_protocol::live::presence_anchors(&key, 2, 9).unwrap();
            let said = ShellSelection {
                anchor: hex::encode(anchor.cursor),
                head: hex::encode(head.cursor),
                composing: 0,
            };
            assert_eq!(
                caret_of(&said),
                Some(Caret {
                    key,
                    payload: PresencePayload::Selection { start: 2, end: 9 },
                    composing: 0,
                })
            );
        }

        phase a_draft_the_persons_app_says_rides_with_their_caret_until_it_stops;
        {
            let (carets, fanout, _) = carets();
            ada_says(&fanout, PresencePayload::Selection { start: 0, end: 0 });
            let said = || carets.said_in("so.tilo.chat").expect("her caret");
            assert_eq!(said().composing, 0, "no draft yet");
            ada_says(&fanout, composing(true));
            assert_eq!(said().composing, 1);
            // Moving within the record keeps the draft where it is.
            ada_says(&fanout, PresencePayload::Caret { pos: 0 });
            assert_eq!(said().composing, 1);
            // Said again as she keeps typing: what keeps her typing on other devices.
            ada_says(&fanout, composing(true));
            assert_eq!(said().composing, 2);
            ada_says(&fanout, composing(false));
            assert_eq!(said().composing, 0);
            assert_eq!(
                carets.said_in("so.tilo.notes"),
                None,
                "said only in her app"
            );
        }

        phase someone_elses_draft_reaches_the_apps_as_they_start_and_stop_it;
        {
            let (carets, _fanout, (_apps, handed)) = carets();
            let caret = |composing| Caret {
                key: channel(),
                payload: PresencePayload::Caret { pos: 0 },
                composing,
            };
            let take = || std::mem::take(&mut *handed.lock_recover());
            carets.show(WORKSPACE, HashMap::from([("bo".to_owned(), caret(1))]));
            assert_eq!(take(), [PresencePayload::Caret { pos: 0 }, composing(true)]);
            carets.show(WORKSPACE, HashMap::from([("bo".to_owned(), caret(1))]));
            assert_eq!(take(), [], "nothing moved");
            carets.show(WORKSPACE, HashMap::from([("bo".to_owned(), caret(2))]));
            assert_eq!(take(), [composing(true)], "still typing: said again");
            carets.show(WORKSPACE, HashMap::from([("bo".to_owned(), caret(0))]));
            assert_eq!(take(), [composing(false)], "sent: no one typing");
            carets.show(WORKSPACE, HashMap::from([("bo".to_owned(), caret(1))]));
            assert_eq!(take(), [composing(true)]);
            carets.show(WORKSPACE, HashMap::new());
            assert_eq!(
                take(),
                [composing(false), PresencePayload::Left],
                "someone gone while typing types no more"
            );
        }
    }
}
