//! Peer pointers, from an app in a room to the compositor drawing it.
//!
//! Outbound: a [`RoomSubscription`] republishes each local app's
//! surface-local pointer as this session's cursor in desktop
//! coordinates — per session, not per connection, so one person stays
//! one cursor across windows. [`should_publish_cursor`] throttles to
//! [`MIN_PUBLISH_GAP`] and restates a still pointer every
//! [`CURSOR_KEEPALIVE`] for late joiners.
//!
//! Inbound: [`PeerPresence::pump`] folds arrivals through the same
//! decode an app uses. A peer's identity (`peer@<handle>`) comes from
//! the frame; its name is a governance fact from
//! [`ShellNetwork::peer_labels`] — a frame's own `user` field is never
//! trusted, since a peer naming itself could name anybody.

use crate::shell::state::WorkspaceAuthority as _;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ui::collaboration::{CollabEvent, PresencePayload};
use ui::presence::{PresenceCtx, PresenceKey};

use crate::shell::collab_broker::{BrokerHandle, RoomSubscription};
use crate::shell::network::ShellNetwork;

/// A room's last-published cursor position and when it went out, keyed
/// by room — what [`should_publish_cursor`] rate-limits against.
type CursorPublishHistory = HashMap<String, ((f64, f64), Instant)>;
type DepartureKey = (u64, String, std::time::SystemTime);

/// Bridges the session's collaboration broker and its peers for
/// pointer presence. Held by the compositor for the life of the session.
pub struct PeerPresence {
    /// This session's pointer in scene space. `None` until it has
    /// actually been somewhere — not `(0, 0)`, since a session that
    /// never moves would otherwise announce the origin as a real position.
    cursor: Arc<Mutex<Option<(f64, f64)>>>,
    /// Inbound presence from other hosts, filled by [`ShellNetwork::tick`].
    inbound: Arc<Mutex<Vec<crate::shell::network::InboundPresence>>>,
    /// Tab location, follow cues, short statuses, and semantic work arrive through the
    /// authenticated typed-presence route rather than the legacy desktop
    /// cursor queue. They are drained into the same render-only
    /// [`PresenceCtx`] projection in [`Self::pump`].
    typed_inbound: Arc<Mutex<Vec<(String, PresenceKey, PresencePayload)>>>,
    departures: Arc<crate::shell::network::PeerDepartureHistory>,
    seen_departures: Mutex<HashSet<DepartureKey>>,
    subscription: Arc<RoomSubscription>,
    /// The rooms this session is in, mirrored from [`Self::set_rooms`]
    /// so a room this session has left stops being remembered below.
    rooms: Mutex<Vec<String>>,
    /// What this session last put on the wire, per room: position and
    /// when. Read by [`should_publish_cursor`].
    last_published: Mutex<CursorPublishHistory>,
    net: Arc<ShellNetwork>,
}

/// Smallest gap between two pointer publishes for the same room — ~30
/// Hz, matching the rate the toolkit throttles an app's pointer to so a
/// desktop cursor and an in-app cursor cost the same on the wire.
pub const MIN_PUBLISH_GAP: Duration = Duration::from_millis(33);

/// How long a stationary pointer goes unmentioned before the session
/// restates it. Presence is gossip: a host that joins after somebody
/// stopped moving has never heard where they are.
pub const CURSOR_KEEPALIVE: Duration = Duration::from_secs(1);

fn typed_presence_authorized(key: &PresenceKey, payload: &PresencePayload, room: &str) -> bool {
    match payload {
        PresencePayload::Following { .. } | PresencePayload::Status { .. } => {
            matches!(key, PresenceKey::Workspace { id } if id == room)
        }
        PresencePayload::Arrived | PresencePayload::Left => {
            !room.is_empty()
                && matches!(
                    key,
                    PresenceKey::Tab { id } if u64::from_str_radix(id, 16).is_ok()
                )
        }
        PresencePayload::Interaction(update) => {
            !room.is_empty()
                && matches!(
                    key,
                    PresenceKey::Resource { app, id, kind }
                        if update.resource.id == *id
                            && update.resource.kind == *kind
                            && update
                                .action
                                .app
                                .as_ref()
                                .is_none_or(|owner| owner.id == *app)
                )
        }
        _ => false,
    }
}

fn fold_typed_presence(
    updates: Vec<(String, PresenceKey, PresencePayload)>,
    presence: &mut PresenceCtx,
) -> bool {
    let changed = !updates.is_empty();
    for (user, key, payload) in updates {
        if let PresencePayload::Interaction(update) = payload {
            let kind = update.actor.kind;
            let sequence = update.sequence;
            let update = (*update)
                .normalized_by_broker(tilo_actions::InteractionActor { id: user, kind }, sequence);
            let _ = presence.observe_interaction(update);
        } else if matches!(payload, PresencePayload::Left) && matches!(key, PresenceKey::Tab { .. })
        {
            // Here Tab/Left is app lifecycle, not the legacy desktop
            // pointer leaving a viewport. Clear only this location.
            presence.remove_presence(&key, &ui::permissions::UserId(user));
        } else {
            presence.apply_remote_presence(user, key, payload);
        }
    }
    changed
}

fn departure_presence_ids(
    session_handle: u64,
    participant_name: Option<&str>,
    active_labels: &std::collections::BTreeMap<u64, String>,
) -> Vec<String> {
    let mut ids = vec![format!("peer@{session_handle}")];
    if let Some(name) = participant_name
        && !active_labels
            .values()
            .any(|active| active.eq_ignore_ascii_case(name))
    {
        ids.push(name.to_string());
    }
    ids
}

/// Should this session put its pointer on the wire again? `last` is
/// what it last published for this room and when. Pure so the decision
/// can be asserted on directly.
pub fn should_publish_cursor(
    last: Option<((f64, f64), Instant)>,
    at: (f64, f64),
    now: Instant,
    min_gap: Duration,
    keepalive: Duration,
) -> bool {
    let Some((was, when)) = last else {
        return true; // Never said anything about this room — say it.
    };
    let elapsed = now.saturating_duration_since(when);
    if was == at {
        return elapsed >= keepalive;
    }
    elapsed >= min_gap
}

/// What to put on the wire for a room right now, if anything. `at` is
/// `None` until the pointer has moved; `None` here means silence, not
/// `Some((0.0, 0.0))`.
fn cursor_to_publish(
    at: Option<(f64, f64)>,
    last: Option<((f64, f64), Instant)>,
    now: Instant,
    min_gap: Duration,
    keepalive: Duration,
) -> Option<(f64, f64)> {
    let at = at?;
    should_publish_cursor(last, at, now, min_gap, keepalive).then_some(at)
}

impl PeerPresence {
    /// Subscribe to `rooms` on `broker` and republish what the local
    /// apps publish in them to `net`.
    pub fn attach(broker: &BrokerHandle, net: Arc<ShellNetwork>, rooms: Vec<String>) -> Self {
        let cursor: Arc<Mutex<Option<(f64, f64)>>> = Arc::new(Mutex::new(None));
        let inbound = net.inbound_presence_handle();
        let rooms_mirror = rooms.clone();
        let net_for_desktop = Arc::clone(&net);
        let net_for_subscription = Arc::clone(&net);

        let sink_cursor = Arc::clone(&cursor);
        let typed_inbound: Arc<Mutex<Vec<(String, PresenceKey, PresencePayload)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink_typed = Arc::clone(&typed_inbound);
        let subscription = broker.subscribe_rooms(
            rooms,
            Box::new(move |event, route| {
                if let CollabEvent::RemotePresenceUpdate {
                    user_id,
                    key,
                    payload,
                } = event
                    && matches!(
                        payload,
                        PresencePayload::Following { .. }
                            | PresencePayload::Status { .. }
                            | PresencePayload::Interaction(_)
                            | PresencePayload::Arrived
                            | PresencePayload::Left
                    )
                {
                    // Room-scoped values must name the delivery room. A
                    // resource-scoped interaction is authorized by the room
                    // route itself, then retained only as roster activity.
                    let authorized = typed_presence_authorized(key, payload, &route.cluster_id);
                    if authorized && let Ok(mut queue) = sink_typed.lock() {
                        queue.push((user_id.clone(), key.clone(), payload.clone()));
                    }
                    return;
                }
                // Only a pointer says where someone is; carets/selections
                // are the document's business, not the desktop's.
                let CollabEvent::RemotePresenceUpdate {
                    payload: PresencePayload::Pointer { .. },
                    ..
                } = event
                else {
                    return;
                };
                // Room comes from the delivery route, not the app's own
                // key — only the route knows which room this app is in.
                if route.cluster_id.is_empty() {
                    return;
                }
                let key = PresenceKey::Workspace {
                    id: route.cluster_id.clone(),
                };
                let Ok(key_str) = serde_json::to_string(&key) else {
                    return;
                };
                // Nothing to say if the pointer has never moved.
                let Some(point) = *sink_cursor
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                else {
                    return;
                };
                net_for_subscription.publish_presence(key_str, point);
            }),
        );

        let departures = net.peer_departure_history();
        let seen_departures = departures
            .snapshot()
            .into_iter()
            .map(|departure| {
                (
                    departure.session_handle,
                    departure.route.cluster_id,
                    departure.observed_at,
                )
            })
            .collect();
        Self {
            cursor,
            inbound,
            typed_inbound,
            departures,
            seen_departures: Mutex::new(seen_departures),
            subscription,
            rooms: Mutex::new(rooms_mirror),
            last_published: Mutex::new(HashMap::new()),
            net: net_for_desktop,
        }
    }

    /// Re-point the subscription at the session's current rooms. Called
    /// whenever the workspace set changes.
    pub fn set_rooms(&self, rooms: Vec<String>) {
        if let Ok(mut g) = self.rooms.lock() {
            // Forget publishes to a left room, so rejoining re-announces
            // rather than staying silent behind a stale keepalive.
            if let Ok(mut last) = self.last_published.lock() {
                last.retain(|room, _| rooms.iter().any(|r| r == room));
            }
            g.clone_from(&rooms);
        }
        self.subscription.set_rooms(rooms);
    }

    /// Put this session's own pointer on the wire, in the room the user
    /// is looking at. Returns whether anything went out. Only the room
    /// on screen, or every room this session is in would paint a cursor
    /// for people not looking at it.
    pub fn publish_local_cursor(&self, room: &str) -> bool {
        self.publish_local_cursor_at(room, Instant::now())
    }

    /// [`Self::publish_local_cursor`] with the clock supplied, so the
    /// throttle and the keepalive can be exercised without sleeping.
    pub fn publish_local_cursor_at(&self, room: &str, now: Instant) -> bool {
        if room.is_empty() {
            return false;
        }
        let at = *self
            .cursor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut last = self
            .last_published
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(at) = cursor_to_publish(
            at,
            last.get(room).copied(),
            now,
            MIN_PUBLISH_GAP,
            CURSOR_KEEPALIVE,
        ) else {
            return false;
        };
        let key = PresenceKey::Workspace {
            id: room.to_string(),
        };
        let Ok(key_str) = serde_json::to_string(&key) else {
            return false;
        };
        self.net.publish_presence(key_str, at);
        last.insert(room.to_string(), (at, now));
        true
    }

    /// Record where this session's pointer is. The `Some` half of
    /// [`Self::set_cursor_position`]; the frame loop uses the other half.
    pub fn set_cursor(&self, x: f64, y: f64) {
        self.set_cursor_position(Some((x, y)));
    }

    /// Record where this session's pointer is, or that it has never been
    /// anywhere — an `Option` because a never-moved pointer must stay
    /// silent rather than publish the origin as a real position.
    pub fn set_cursor_position(&self, at: Option<(f64, f64)>) {
        if let Ok(mut g) = self.cursor.lock() {
            *g = at;
        }
    }

    /// Fold everything that arrived from other hosts into `presence`,
    /// returning whether anything landed. Who is *in* the room is
    /// answered separately by `refresh_roster`.
    pub fn pump(&self, presence: &mut PresenceCtx) -> bool {
        let drained: Vec<_> = {
            let Ok(mut buf) = self.inbound.lock() else {
                return false;
            };
            std::mem::take(&mut *buf)
        };
        let net = Arc::clone(&self.net);
        let mine: Vec<String> = self.rooms.lock().map(|g| g.clone()).unwrap_or_default();
        let cursor_changed =
            fold_inbound(drained, presence, &|room| net.peer_labels(room), &|room| {
                mine.iter().any(|r| r == room)
            });
        let typed = self
            .typed_inbound
            .lock()
            .map(|mut queue| std::mem::take(&mut *queue))
            .unwrap_or_default();
        // Attribution is normalized to the authenticated envelope identity
        // inside the fold; JSON supplied by an app cannot rename its actor.
        let typed_changed = fold_typed_presence(typed, presence);
        let departures = self.departures.snapshot();
        let active_keys = departures
            .iter()
            .map(|departure| {
                (
                    departure.session_handle,
                    departure.route.cluster_id.clone(),
                    departure.observed_at,
                )
            })
            .collect::<HashSet<_>>();
        let departure_changed = self.seen_departures.lock().is_ok_and(|mut seen| {
            seen.retain(|key| active_keys.contains(key));
            let mut changed = false;
            for departure in departures {
                let key = (
                    departure.session_handle,
                    departure.route.cluster_id.clone(),
                    departure.observed_at,
                );
                if !seen.insert(key) {
                    continue;
                }
                let active_labels = self.net.active_session_labels(&departure.route.cluster_id);
                for id in departure_presence_ids(
                    departure.session_handle,
                    departure.participant_name.as_deref(),
                    &active_labels,
                ) {
                    presence.unregister_peer(&id);
                }
                changed = true;
            }
            changed
        });
        cursor_changed || typed_changed || departure_changed
    }

    /// Write the active workspace's room's people into the tree, where
    /// the roster and top bar read them. Separate from [`Self::pump`]:
    /// answers "did who-is-here change", not "did a peer move" — a peer
    /// named while sitting still changes the former, not the latter.
    /// Returns whether the list changed, a reason to repaint.
    pub fn refresh_roster(&self, presence: &PresenceCtx) -> bool {
        let local = self.net.local_peer_id_hash();
        // Written only on change — a lock every frame would contend
        // with the network thread. `replica` is locked once to compute
        // `rows` and dropped before re-locking to write it, rather than
        // held across both — the network thread's own use of the lock
        // is short, and holding it through an unrelated write extends
        // every contending wait by however long that write takes.
        let (active, rows) = {
            let Ok(replica) = self.net.replica.lock() else {
                return false;
            };
            let tree = replica.tree.read();
            let Some(ws) = tree.workspaces.iter().find(|w| w.id == tree.active) else {
                return false;
            };
            if ws.room.is_nil() {
                return false;
            }
            let labels = tree.room_peer_labels(ws.room);
            // Two ways to be present: moving cursor, or CRDT participation.
            let mut arrived = handles_seen_in(presence, &ws.room.to_string());
            arrived.extend(replica.participants_in(ws.room));
            let mut rows = room_roster(ws, &labels, &arrived, local);
            for row in &mut rows {
                let presence_id = presence
                    .names
                    .iter()
                    .find_map(|(id, name)| name.eq_ignore_ascii_case(&row.name).then_some(id))
                    .cloned()
                    .unwrap_or_else(|| ui::permissions::UserId(row.id.clone()));
                row.active_tab = presence.here.iter().find_map(|(key, users)| {
                    let PresenceKey::Tab { id } = key else {
                        return None;
                    };
                    users
                        .contains(&presence_id)
                        .then(|| u64::from_str_radix(id, 16).ok())
                        .flatten()
                });
            }
            let unchanged = ws.collaborators == rows && tree.collaborators == rows;
            let active = ws.id;
            // Drop the read guard as soon as it's no longer needed, rather
            // than holding it to the end of the block below.
            drop(tree);
            if unchanged {
                return false;
            }
            (active, rows)
        };
        let Ok(replica) = self.net.replica.lock() else {
            return false;
        };
        let mut tree = replica.tree.write();
        if let Some(workspace) = tree.workspaces.iter_mut().find(|ws| ws.id == active) {
            workspace.collaborators.clone_from(&rows);
        }
        // Keep the migration-era cache in sync for older readers. New shell
        // chrome always reads the active workspace directly, so this cache
        // cannot leak another workspace's presence into the top bar.
        tree.collaborators = rows;
        true
    }
}

/// The peer handles this session has had presence from in `room`.
///
/// Reads the same [`PresenceCtx`] the overlay paints cursors from, so the
/// roster and the desktop can never disagree about who is present.
fn handles_seen_in(presence: &PresenceCtx, room: &str) -> std::collections::BTreeSet<u64> {
    let mut out = std::collections::BTreeSet::new();
    for (key, peers) in &presence.pointers {
        let PresenceKey::Workspace { id } = key else {
            continue;
        };
        if id.as_str() != room {
            continue;
        }
        for (user, _) in peers {
            if let Some(handle) = user.0.strip_prefix("peer@")
                && let Ok(handle) = handle.parse::<u64>()
            {
                out.insert(handle);
            }
        }
    }
    out
}

/// What a room calls the peers in it, keyed by the handle presence
/// frames identify their sender by.
pub type PeerLabels = std::collections::BTreeMap<u64, String>;

/// Narrows a peer's cursor coordinate (desktop pixel space) to the `f32`
/// [`PresencePayload::Pointer`] takes. Screen coordinates never approach
/// a magnitude where this loses meaningful precision.
#[allow(
    clippy::cast_possible_truncation,
    reason = "desktop pixel coordinates stay well within f32's precision"
)]
fn cursor_coord_to_f32(v: f64) -> f32 {
    v as f32
}

/// Fold inbound presence into `presence`, returning whether anything
/// landed. Split from [`PeerPresence::pump`] so it can be tested without
/// a network; `labels` is a parameter for the same reason (production
/// wires [`ShellNetwork::peer_labels`]).
pub fn fold_inbound(
    entries: Vec<crate::shell::network::InboundPresence>,
    presence: &mut PresenceCtx,
    labels: &dyn Fn(&str) -> PeerLabels,
    member_of: &dyn Fn(&str) -> bool,
) -> bool {
    let mut any = false;
    // Entries in one drain are nearly always the same room.
    let mut resolved: Option<(String, PeerLabels)> = None;
    for entry in entries {
        let Ok(key) = serde_json::from_str::<PresenceKey>(&entry.key) else {
            continue;
        };
        // Only rooms this session is in — an access rule.
        if let PresenceKey::Workspace { id: room } = &key
            && !member_of(room)
        {
            continue;
        }
        // Spelled from the handle, never `entry.user`'s unverified claim.
        let user = format!("peer@{}", entry.peer);

        if let PresenceKey::Workspace { id: room } = &key {
            if !matches!(&resolved, Some((seen, _)) if seen == room) {
                resolved = Some((room.clone(), labels(room)));
            }
            let Some((_, known)) = &resolved else {
                // Unreachable — the branch above always sets `resolved`
                // when it isn't already fresh for `room` — but skipping
                // this one entry costs nothing next tick, unlike a panic.
                continue;
            };
            if let Some(name) = known.get(&entry.peer) {
                presence.set_display_name(ui::permissions::UserId(user.clone()), name.clone());
            }
        }

        presence.apply_remote_presence(
            user,
            key,
            PresencePayload::Pointer {
                x: cursor_coord_to_f32(entry.point.0),
                y: cursor_coord_to_f32(entry.point.1),
            },
        );
        any = true; // Not on loop entry: a `continue` above is a rejected frame.
    }
    any
}

/// The people in `ws`'s room. Two sources, neither sufficient alone:
/// `labels` is the room document's answer (everyone its authority has
/// named, including an invitee who never moved their mouse), `arrived`
/// is who actually turned up (presence plus CRDT participants).
///
/// A named peer that hasn't arrived is marked [`Collaborator::invited`],
/// keyed off the transport-authenticated identity so a peer cannot
/// promote its own row. `local` is dropped — the roster is who *else*
/// is here.
pub fn room_roster(
    ws: &crate::shell::state::Workspace,
    labels: &PeerLabels,
    arrived: &std::collections::BTreeSet<u64>,
    local: u64,
) -> Vec<crate::shell::state::Collaborator> {
    let mut handles: std::collections::BTreeSet<u64> = labels.keys().copied().collect();
    handles.extend(arrived.iter().copied());
    handles.remove(&local);
    handles
        .into_iter()
        .map(|handle| {
            // Same spelling as the presence path, so avatar and cursor share an id.
            let id = format!("peer@{handle}");
            let name = labels.get(&handle).cloned().unwrap_or_else(|| id.clone());
            let role = ws.role_of(&name);
            let who = crate::shell::state::Collaborator::new(id, name).with_session(handle);
            let who = match role {
                Some(role) => who.with_role(crate::shell::state::role_label(role)),
                None => who,
            };
            if arrived.contains(&handle) {
                who
            } else {
                who.as_invited()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::network::InboundPresence;
    use std::time::SystemTime;
    use ui::permissions::UserId;

    fn interaction(actor: &str) -> PresencePayload {
        PresencePayload::Interaction(Box::new(tilo_actions::SemanticInteraction {
            version: tilo_actions::INTERACTION_VERSION,
            invocation: tilo_actions::InvocationId::new(),
            sequence: 1,
            actor: tilo_actions::InteractionActor {
                id: actor.to_string(),
                kind: tilo_actions::InteractionActorKind::Person,
            },
            phase: tilo_actions::InteractionPhase::Applying,
            action: tilo_actions::InteractionAction {
                id: "edit".into(),
                label: "Editing".into(),
                app: None,
            },
            resource: tilo_actions::ActionResource {
                id: "report:q3".into(),
                kind: "document".into(),
                label: "Q3 report".into(),
            },
            anchor: tilo_actions::InteractionAnchor::Resource,
            group: None,
        }))
    }

    #[test]
    fn shell_observes_remote_action_and_intent_without_spoofed_attribution() {
        let room = PresenceKey::Workspace {
            id: "room-7".into(),
        };
        let tab = PresenceKey::Tab { id: "2a".into() };
        let resource = PresenceKey::Resource {
            app: "org.tilo.Writer".into(),
            id: "report:q3".into(),
            kind: "document".into(),
        };
        let status = PresencePayload::Status {
            text: "Preparing the board review".into(),
            automatic: true,
        };
        assert!(typed_presence_authorized(&room, &status, "room-7"));
        assert!(!typed_presence_authorized(&room, &status, "another-room"));
        assert!(typed_presence_authorized(
            &resource,
            &interaction("forged-id"),
            "room-7"
        ));
        assert!(!typed_presence_authorized(
            &PresenceKey::Resource {
                app: "org.tilo.Writer".into(),
                id: "another-report".into(),
                kind: "document".into(),
            },
            &interaction("forged-id"),
            "room-7"
        ));
        assert!(typed_presence_authorized(
            &tab,
            &PresencePayload::Arrived,
            "room-7"
        ));

        let mut presence = PresenceCtx::default();
        assert!(fold_typed_presence(
            vec![
                ("ada".into(), room, status),
                ("ada".into(), resource, interaction("forged-id")),
                ("ada".into(), tab.clone(), PresencePayload::Arrived),
            ],
            &mut presence,
        ));
        assert_eq!(
            presence
                .status_for(&UserId("ada".into()), SystemTime::now())
                .map(|value| value.text.as_str()),
            Some("Preparing the board review")
        );
        let visible = presence.visible_interactions(Instant::now());
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].actor.id, "ada");
        assert_eq!(visible[0].action.label, "Editing");
        assert_eq!(
            presence.here.get(&tab),
            Some(&vec![UserId("ada".into())]),
            "the shell retains the app's active tab for the People location line"
        );
        let next_tab = PresenceKey::Tab { id: "2b".into() };
        assert!(fold_typed_presence(
            vec![("ada".into(), next_tab.clone(), PresencePayload::Arrived,)],
            &mut presence,
        ));
        assert!(presence.here.get(&tab).is_none_or(std::vec::Vec::is_empty));
        assert_eq!(
            presence.here.get(&next_tab),
            Some(&vec![UserId("ada".into())])
        );
        assert!(fold_typed_presence(
            vec![("ada".into(), next_tab.clone(), PresencePayload::Left)],
            &mut presence,
        ));
        assert!(
            presence
                .here
                .get(&next_tab)
                .is_none_or(std::vec::Vec::is_empty),
            "closing the tab clears its roster location without disconnecting Ada"
        );
        assert!(
            presence
                .status_for(&UserId("ada".into()), SystemTime::now())
                .is_some(),
            "tab lifecycle does not erase room-scoped intent"
        );
    }

    #[test]
    fn abrupt_departure_clears_named_presence_unless_another_device_is_live() {
        let mut presence = PresenceCtx::default();
        let ada = UserId("ada".into());
        let old_transport = UserId("peer@41".into());
        let tab = PresenceKey::Tab { id: "2a".into() };
        presence.register_presence(&tab, ada.clone());
        presence.register_presence(&tab, old_transport.clone());
        presence.apply_remote_presence(
            "ada".into(),
            PresenceKey::Workspace {
                id: "room-7".into(),
            },
            PresencePayload::Status {
                text: "Preparing the board review".into(),
                automatic: false,
            },
        );
        let PresencePayload::Interaction(update) = interaction("ada") else {
            unreachable!();
        };
        assert!(presence.observe_interaction(*update).unwrap());

        let ids = departure_presence_ids(41, Some("ada"), &Default::default());
        assert_eq!(ids, vec!["peer@41", "ada"]);
        for id in ids {
            presence.unregister_peer(&id);
        }
        assert!(presence.here.values().all(std::vec::Vec::is_empty));
        assert!(presence.status_for(&ada, SystemTime::now()).is_none());
        assert!(presence.visible_interactions(Instant::now()).is_empty());

        let still_live = std::collections::BTreeMap::from([(73, "ada".to_string())]);
        assert_eq!(
            departure_presence_ids(41, Some("ada"), &still_live),
            vec!["peer@41"],
            "one device leaving must not erase the person's other live session"
        );
    }

    #[test]
    fn a_session_that_has_said_nothing_speaks() {
        assert!(should_publish_cursor(
            None,
            (10.0, 10.0),
            Instant::now(),
            MIN_PUBLISH_GAP,
            CURSOR_KEEPALIVE,
        ));
    }

    #[test]
    fn a_pointer_that_has_never_moved_is_not_announced_at_the_origin() {
        let now = Instant::now();

        assert_eq!(
            cursor_to_publish(None, None, now, MIN_PUBLISH_GAP, CURSOR_KEEPALIVE),
            None,
            "a session whose pointer has never moved must not announce one"
        );

        // The origin is not banned once actually moved to.
        assert_eq!(
            cursor_to_publish(
                Some((0.0, 0.0)),
                None,
                now,
                MIN_PUBLISH_GAP,
                CURSOR_KEEPALIVE
            ),
            Some((0.0, 0.0)),
            "a pointer somebody actually moved to the origin is still a pointer"
        );
    }

    #[test]
    fn a_move_inside_the_gap_is_held() {
        let t0 = Instant::now();
        assert!(
            !should_publish_cursor(
                Some(((10.0, 10.0), t0)),
                (11.0, 10.0),
                t0 + Duration::from_millis(5),
                MIN_PUBLISH_GAP,
                CURSOR_KEEPALIVE,
            ),
            "a pointer moving at frame rate must not put every sample on the wire"
        );
    }

    #[test]
    fn a_move_past_the_gap_goes_out() {
        let t0 = Instant::now();
        assert!(should_publish_cursor(
            Some(((10.0, 10.0), t0)),
            (400.0, 300.0),
            t0 + MIN_PUBLISH_GAP,
            MIN_PUBLISH_GAP,
            CURSOR_KEEPALIVE,
        ));
    }

    #[test]
    fn a_pointer_that_stopped_is_restated_for_whoever_joined_since() {
        let t0 = Instant::now();
        let at = (10.0, 10.0);
        assert!(
            !should_publish_cursor(
                Some((at, t0)),
                at,
                t0 + Duration::from_millis(200),
                MIN_PUBLISH_GAP,
                CURSOR_KEEPALIVE,
            ),
            "a still pointer must not republish at frame rate"
        );
        assert!(
            should_publish_cursor(
                Some((at, t0)),
                at,
                t0 + CURSOR_KEEPALIVE,
                MIN_PUBLISH_GAP,
                CURSOR_KEEPALIVE,
            ),
            "but it must be restated once the keepalive is due"
        );
    }

    /// A frame as it arrives off the wire. `user` is the sender's own
    /// (untrusted) claimed name — tests below pass a wrong one on purpose.
    fn arrived(user: &str, peer: u64, room: &str, at: (f64, f64)) -> InboundPresence {
        InboundPresence {
            peer,
            user: user.into(),
            key: serde_json::to_string(&PresenceKey::Workspace { id: room.into() })
                .expect("a workspace key serialises"),
            point: at,
            ts: 0,
        }
    }

    fn unnamed(_room: &str) -> PeerLabels {
        PeerLabels::new()
    }

    /// This session is in every room a frame might name — default for
    /// tests not about membership.
    fn any_room(_room: &str) -> bool {
        true
    }

    fn named(pairs: &[(u64, &str)]) -> impl Fn(&str) -> PeerLabels + use<> {
        let map: PeerLabels = pairs.iter().map(|(h, n)| (*h, (*n).to_string())).collect();
        move |_room: &str| map.clone()
    }

    #[test]
    fn a_peer_lands_on_the_room_the_overlay_reads() {
        let room = "3f2b1c00-0000-4000-8000-000000000001";
        let mut presence = PresenceCtx::default();
        assert!(fold_inbound(
            vec![arrived("bob", 7, room, (120.0, 240.0))],
            &mut presence,
            &unnamed,
            &any_room,
        ));

        let key = PresenceKey::Workspace { id: room.into() };
        let peers = presence
            .pointers
            .get(&key)
            .expect("the pointer should be filed under the room");
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].0, UserId("peer@7".into()));
        assert_eq!(peers[0].1, (120.0, 240.0));
    }

    /// A folded frame lands in two stores — `pointers` (overlay paint)
    /// and `remote_cursors` (follow-that-peer) — and both being
    /// *painted* would draw one person twice.
    #[test]
    fn a_desktop_pointer_is_painted_by_the_overlay_and_not_again_by_the_window() {
        let room = "3f2b1c00-0000-4000-8000-000000000001";
        let mut presence = PresenceCtx::default();
        fold_inbound(
            vec![arrived("bob", 7, room, (120.0, 240.0))],
            &mut presence,
            &named(&[(7, "Xaver")]),
            &any_room,
        );

        let peers = presence
            .pointers
            .get(&PresenceKey::Workspace { id: room.into() })
            .expect("the overlay's store must hold the peer it is to paint");
        assert_eq!(peers.len(), 1, "one arrival, one peer");

        assert_eq!(
            presence.remote_cursors.len(),
            1,
            "one person must not become two cursors"
        );
        let cursor = &presence.remote_cursors[0];
        assert_eq!(cursor.user_id, peers[0].0.0, "and they are the same person");
        assert!(
            !cursor.painted_by_window(),
            "the compositor paints this pointer; drawing it here too is the duplicate",
        );

        assert_eq!(
            presence.peer_pointer(&cursor.user_id),
            Some([120.0, 240.0]),
            "following a peer must still land where their cursor shows",
        );
    }

    #[test]
    fn one_person_is_one_entry_across_every_room_in_the_store() {
        let mine = "3f2b1c00-0000-4000-8000-000000000001";
        let theirs = "3f2b1c00-0000-4000-8000-0000000000ff";
        let mut presence = PresenceCtx::default();

        fold_inbound(
            vec![
                arrived("bob", 7, mine, (10.0, 20.0)),
                arrived("bob", 7, theirs, (30.0, 40.0)),
            ],
            &mut presence,
            &unnamed,
            &|room| room == mine,
        );

        let rows: Vec<_> = presence
            .pointers
            .iter()
            .flat_map(|(k, v)| v.iter().map(move |(u, p)| (k.clone(), u.clone(), *p)))
            .collect();
        assert_eq!(
            rows.len(),
            1,
            "one person must be one entry in the whole store, got {rows:?}"
        );
        assert_eq!(rows[0].1, UserId("peer@7".into()));
        assert_eq!(
            rows[0].2,
            (10.0, 20.0),
            "and it is the room this session is in"
        );
    }

    #[test]
    fn a_room_this_session_is_not_in_is_refused() {
        let elsewhere = "3f2b1c00-0000-4000-8000-0000000000ff";
        let mut presence = PresenceCtx::default();

        let any = fold_inbound(
            vec![arrived("bob", 7, elsewhere, (10.0, 10.0))],
            &mut presence,
            &unnamed,
            &|_room| false,
        );

        assert!(
            !any,
            "a frame for a room this session is not in changes nothing"
        );
        assert!(
            presence.pointers.is_empty(),
            "and leaves no entry behind: {:?}",
            presence.pointers
        );
    }

    #[test]
    fn a_peer_is_called_what_the_room_calls_it() {
        let room = "3f2b1c00-0000-4000-8000-000000000001";
        let mut presence = PresenceCtx::default();
        fold_inbound(
            vec![arrived("bob", 7, room, (10.0, 10.0))],
            &mut presence,
            &named(&[(7, "Xaver")]),
            &any_room,
        );
        let peers = presence
            .pointers
            .get(&PresenceKey::Workspace { id: room.into() })
            .expect("a pointer");
        assert_eq!(
            presence.display_name(&peers[0].0),
            "Xaver",
            "the pill must read what the room says, not what the sender claimed"
        );
    }

    #[test]
    fn a_sender_cannot_name_itself() {
        let room = "3f2b1c00-0000-4000-8000-000000000001";
        let mut presence = PresenceCtx::default();
        fold_inbound(
            vec![arrived("Quinten", 7, room, (10.0, 10.0))],
            &mut presence,
            &unnamed,
            &any_room,
        );
        let peers = presence
            .pointers
            .get(&PresenceKey::Workspace { id: room.into() })
            .expect("a pointer");
        assert_eq!(
            presence.display_name(&peers[0].0),
            "peer@7",
            "a name the sender chose for itself must not reach the screen"
        );
    }

    #[test]
    fn a_name_does_not_move_a_peer_to_a_new_colour() {
        let room = "3f2b1c00-0000-4000-8000-000000000001";
        let mut before = PresenceCtx::default();
        fold_inbound(
            vec![arrived("", 7, room, (10.0, 10.0))],
            &mut before,
            &unnamed,
            &any_room,
        );
        let mut after = PresenceCtx::default();
        fold_inbound(
            vec![arrived("", 7, room, (10.0, 10.0))],
            &mut after,
            &named(&[(7, "Xaver")]),
            &any_room,
        );
        let key = PresenceKey::Workspace { id: room.into() };
        let a = &before.pointers.get(&key).expect("a pointer")[0].0;
        let b = &after.pointers.get(&key).expect("a pointer")[0].0;
        assert_eq!(
            a, b,
            "the identity must be the same before and after a name"
        );
        assert_eq!(
            before.color_for(a),
            after.color_for(b),
            "and so must the colour"
        );
    }

    #[test]
    fn naming_a_peer_does_not_conjure_a_cursor() {
        let mut presence = PresenceCtx::default();
        assert!(!fold_inbound(
            Vec::new(),
            &mut presence,
            &named(&[(7, "Xaver"), (8, "Quinten")]),
            &any_room,
        ));
        assert!(
            presence.pointers.is_empty(),
            "a name is not a position: {:?}",
            presence.pointers
        );
    }

    #[test]
    fn one_session_stays_one_peer_across_two_apps() {
        let room = "3f2b1c00-0000-4000-8000-000000000001";
        let mut presence = PresenceCtx::default();
        fold_inbound(
            vec![
                arrived("bob", 7, room, (100.0, 100.0)),
                arrived("bob", 7, room, (600.0, 300.0)),
            ],
            &mut presence,
            &unnamed,
            &any_room,
        );

        let key = PresenceKey::Workspace { id: room.into() };
        let peers = presence.pointers.get(&key).expect("a pointer");
        assert_eq!(
            peers.len(),
            1,
            "one person driving two apps must be one peer, got {peers:?}"
        );
        assert_eq!(
            peers[0].1,
            (600.0, 300.0),
            "and their cursor must be where they last pointed"
        );
    }

    #[test]
    fn two_rooms_do_not_share_peers() {
        let here = "3f2b1c00-0000-4000-8000-000000000001";
        let there = "3f2b1c00-0000-4000-8000-0000000000ff";
        let mut presence = PresenceCtx::default();
        fold_inbound(
            vec![
                arrived("bob", 7, here, (10.0, 10.0)),
                arrived("carol", 8, there, (20.0, 20.0)),
            ],
            &mut presence,
            &unnamed,
            &any_room,
        );
        let peers = presence
            .pointers
            .get(&PresenceKey::Workspace { id: here.into() })
            .expect("a pointer in this room");
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].0, UserId("peer@7".into()));
    }

    #[test]
    fn a_folded_frame_files_the_sender_under_the_room_it_named() {
        let room = "3f2b1c00-0000-4000-8000-000000000001";
        let mut presence = PresenceCtx::default();
        let any = fold_inbound(
            vec![arrived("bob", 7, room, (1.0, 2.0))],
            &mut presence,
            &unnamed,
            &any_room,
        );
        assert!(any, "a frame in a room this session is in lands");
        let peers = presence
            .pointers
            .get(&PresenceKey::Workspace { id: room.into() })
            .expect("a pointer in the room it named");
        assert_eq!(
            peers.iter().map(|(u, _)| u.clone()).collect::<Vec<_>>(),
            vec![UserId("peer@7".into())],
            "the sender is the session the transport authenticated, \
             not the name that session announced for itself",
        );
    }

    #[test]
    fn a_frame_that_names_no_room_yields_no_sighting() {
        let mut presence = PresenceCtx::default();
        let entry = InboundPresence {
            peer: 9,
            user: "carol".into(),
            key: serde_json::to_string(&PresenceKey::Tab { id: "2a".into() })
                .expect("a tab key serialises"),
            point: (3.0, 4.0),
            ts: 0,
        };
        fold_inbound(vec![entry], &mut presence, &unnamed, &any_room);
        assert!(
            !presence
                .pointers
                .keys()
                .any(|k| matches!(k, PresenceKey::Workspace { .. })),
            "a frame naming no room put somebody in one: {:?}",
            presence.pointers.keys().collect::<Vec<_>>(),
        );
    }

    #[test]
    fn a_session_that_names_nobody_is_labelled_with_its_handle() {
        let room = "3f2b1c00-0000-4000-8000-000000000001";
        let mut presence = PresenceCtx::default();
        fold_inbound(
            vec![arrived("", 0x2a, room, (5.0, 5.0))],
            &mut presence,
            &unnamed,
            &any_room,
        );
        let peers = presence
            .pointers
            .get(&PresenceKey::Workspace { id: room.into() })
            .expect("a pointer");
        assert_eq!(peers[0].0, UserId("peer@42".into()));
        assert_eq!(presence.display_name(&peers[0].0), "peer@42", "never blank");
    }
}

/// Two sessions, and a pointer that has to get from one to the other.
///
/// Everything above this point is asserted against one session's store,
/// with room set and names handed in as closures — not enough on its
/// own, since what a session publishes and what it accepts agree only
/// by construction. This drives the whole path end to end.
#[cfg(test)]
mod two_sessions {
    use super::*;
    use crate::network::actor::NetCmd;
    use crate::network::protocol::NeighbourMsg;
    use crate::shell::network::shell_network_for_test;
    use crate::shell::state::{PeerOrigin, RoomId, WorkspaceTree};

    const OWNER: &str = "quinten";

    /// A real Iroh endpoint id, minted from a keypair, so the parse is
    /// the production one.
    fn peer_id(seed: u8) -> String {
        iroh::SecretKey::from_bytes(&[seed; 32])
            .public()
            .to_string()
    }

    /// The handle presence frames identify `peer` by.
    fn handle_of(peer: &str) -> u64 {
        crate::network::behaviour::peer_liveness_handle(
            &peer.parse::<iroh::EndpointId>().expect("an endpoint id"),
        )
    }

    /// The rooms a session is in, read off its tree the way the frame
    /// loop does before calling [`PeerPresence::set_rooms`].
    fn rooms_of(tree: &crate::shell::state::WorkspaceTreeHandle) -> Vec<String> {
        tree.read()
            .workspaces
            .iter()
            .map(|w| w.room.to_string())
            .collect()
    }

    /// `spawn` is the removed legacy JSON-socket constructor; the typed
    /// Cap'n Proto broker under test opens its own socket the same way
    /// production does, just with no peer-sync bridge to Iroh.
    fn broker(dir: &std::path::Path, name: &str) -> crate::shell::collab_broker::BrokerHandle {
        crate::shell::collab_broker::spawn_native(
            dir.join(name),
            &None,
            crate::shell::state::WorkspaceTree::default().shared(),
        )
        .expect("a broker on a temp socket")
    }

    /// A session standing in for the host: governs a room and has named
    /// everybody in it.
    fn host_naming(people: &[(&str, &str)]) -> (WorkspaceTree, RoomId) {
        let mut tree = WorkspaceTree::default();
        let ws = tree.append_empty_workspace("Design review");
        let room = tree.room_of(ws).expect("a fresh workspace mints a room");
        tree.adopt_ownerless_workspaces(&OWNER.to_string());
        for (peer, called) in people {
            assert!(
                tree.name_peer(room, &OWNER.to_string(), peer, called),
                "the room's administrator may name a peer in it",
            );
        }
        (tree, room)
    }

    /// A session that has joined `room`, with names replicated out of
    /// the room's document the way production does it.
    fn guest_joining(host: &mut WorkspaceTree, room: RoomId) -> WorkspaceTree {
        let mut tree = WorkspaceTree::default();
        let ws = tree.append_empty_workspace("Mine");
        assert!(
            tree.set_room(ws, room),
            "the guest has to end up in the room it joined",
        );
        let changes = host
            .export_room_changes_since(room, &[])
            .expect("the host is in the room it governs");
        tree.apply_room_changes(room, &changes, &PeerOrigin::hub(1))
            .expect("the host's governance merges into the guest");
        tree
    }

    /// Hand `msg` to `to` the way the gossip reader does.
    fn over_the_wire(msg: NeighbourMsg, to: &ShellNetwork) {
        match msg {
            NeighbourMsg::PresenceMoved {
                peer,
                user,
                key,
                point,
                ts,
            } => to.handle_presence_message(peer, user, key, point, ts),
            other => panic!("expected PresenceMoved, got {other:?}"),
        }
    }

    /// Take the one presence frame `cmds` is holding.
    fn published(cmds: &mut tokio::sync::mpsc::Receiver<NetCmd>) -> NeighbourMsg {
        match cmds
            .try_recv()
            .expect("the session must put its pointer on the wire")
        {
            NetCmd::Publish { msg, .. } => msg,
            other => panic!("expected a publish, got {other:?}"),
        }
    }

    /// Each session's pointer arrives on the other's screen under the
    /// room's name for it. Asserted in both directions — host and guest
    /// are not symmetric, and testing only the easy direction is how a
    /// half-broken path stays green.
    #[test]
    fn a_pointer_crosses_between_two_sessions_both_ways() {
        let guest_peer = peer_id(9);
        let host_peer = peer_id(11);
        let (mut host_tree, room) = host_naming(&[(&guest_peer, "Xaver"), (&host_peer, "Quinten")]);

        let guest_tree = guest_joining(&mut host_tree, room);

        let host_handle = host_tree.shared();
        let guest_handle = guest_tree.shared();

        let (host_net, mut host_cmds) =
            shell_network_for_test(host_handle.clone(), handle_of(&host_peer));
        let (guest_net, mut guest_cmds) =
            shell_network_for_test(guest_handle.clone(), handle_of(&guest_peer));
        let host_net = Arc::new(host_net);
        let guest_net = Arc::new(guest_net);

        let dir = std::env::temp_dir().join(format!("tilo-two-sessions-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let host_broker = broker(&dir, "h.sock");
        let guest_broker = broker(&dir, "g.sock");

        let host = PeerPresence::attach(&host_broker, Arc::clone(&host_net), Vec::new());
        let guest = PeerPresence::attach(&guest_broker, Arc::clone(&guest_net), Vec::new());
        host.set_rooms(rooms_of(&host_handle));
        guest.set_rooms(rooms_of(&guest_handle));

        let room_key = PresenceKey::Workspace {
            id: room.to_string(),
        };

        guest.set_cursor(420.0, 300.0);
        assert!(
            guest.publish_local_cursor(&room.to_string()),
            "a session whose pointer has been somewhere must announce it",
        );
        over_the_wire(published(&mut guest_cmds), &host_net);

        let mut on_host = PresenceCtx::default();
        assert!(
            host.pump(&mut on_host),
            "the host's next frame must fold in what the guest published",
        );
        let seen = on_host
            .pointers
            .get(&room_key)
            .expect("the host must hold the guest's pointer, filed under the room");
        assert_eq!(seen.len(), 1, "one person is one pointer");
        assert_eq!(
            seen[0].1,
            (420.0, 300.0),
            "and it has to be where the guest actually put it",
        );
        assert_eq!(
            on_host.display_name(&seen[0].0),
            "Xaver",
            "the pill over it reads the name the room gave the peer",
        );

        host.set_cursor(880.0, 520.0);
        assert!(host.publish_local_cursor(&room.to_string()));
        over_the_wire(published(&mut host_cmds), &guest_net);

        let mut on_guest = PresenceCtx::default();
        assert!(
            guest.pump(&mut on_guest),
            "and the crossing has to work the other way, which is a \
             separate fact about a separate room set",
        );
        let seen = on_guest
            .pointers
            .get(&room_key)
            .expect("the guest must hold the host's pointer");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].1, (880.0, 520.0));
        assert_eq!(on_guest.display_name(&seen[0].0), "Quinten");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A session that has not moved its pointer says nothing, and one
    /// that then moves it says something, over the real wire.
    #[test]
    fn a_session_is_silent_until_its_pointer_has_been_somewhere() {
        let guest_peer = peer_id(21);
        let (mut host_tree, room) = host_naming(&[(&guest_peer, "Xaver")]);
        let guest_tree = guest_joining(&mut host_tree, room);

        let host_handle = host_tree.shared();
        let guest_handle = guest_tree.shared();
        let (host_net, _host_cmds) = shell_network_for_test(host_handle.clone(), 0xD00Du64);
        let (guest_net, mut guest_cmds) =
            shell_network_for_test(guest_handle.clone(), handle_of(&guest_peer));
        let host_net = Arc::new(host_net);
        let guest_net = Arc::new(guest_net);

        let dir = std::env::temp_dir().join(format!("tilo-silent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let host_broker = broker(&dir, "h.sock");
        let guest_broker = broker(&dir, "g.sock");
        let host = PeerPresence::attach(&host_broker, Arc::clone(&host_net), Vec::new());
        let guest = PeerPresence::attach(&guest_broker, Arc::clone(&guest_net), Vec::new());
        host.set_rooms(rooms_of(&host_handle));
        guest.set_rooms(rooms_of(&guest_handle));

        assert!(
            !guest.publish_local_cursor(&room.to_string()),
            "a pointer that has never been anywhere is not a pointer at the origin",
        );
        assert!(
            guest_cmds.try_recv().is_err(),
            "and nothing may reach the wire for it",
        );
        let mut on_host = PresenceCtx::default();
        assert!(!host.pump(&mut on_host), "so the host has nobody to paint");
        assert!(on_host.pointers.is_empty());

        guest.set_cursor(640.0, 610.0);
        assert!(guest.publish_local_cursor(&room.to_string()));
        over_the_wire(published(&mut guest_cmds), &host_net);
        assert!(host.pump(&mut on_host));
        assert_eq!(
            on_host
                .pointers
                .get(&PresenceKey::Workspace {
                    id: room.to_string()
                })
                .map(|p| p[0].1),
            Some((640.0, 610.0)),
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Where the previous test proves the gate, this proves it's
    /// reachable — takes the position from `ClientInput` exactly as
    /// `native.rs` does, rather than writing `None` by hand.
    #[test]
    fn the_frame_loop_publishes_nothing_for_a_pointer_that_has_never_moved() {
        let guest_peer = peer_id(23);
        let (mut host_tree, room) = host_naming(&[(&guest_peer, "Claude")]);
        let guest_tree = guest_joining(&mut host_tree, room);

        let host_handle = host_tree.shared();
        let guest_handle = guest_tree.shared();
        let (host_net, _host_cmds) = shell_network_for_test(host_handle.clone(), 0xD0Cu64);
        let (guest_net, mut guest_cmds) =
            shell_network_for_test(guest_handle.clone(), handle_of(&guest_peer));
        let host_net = Arc::new(host_net);
        let guest_net = Arc::new(guest_net);

        let dir = std::env::temp_dir().join(format!("tilo-frameloop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let host_broker = broker(&dir, "h.sock");
        let guest_broker = broker(&dir, "g.sock");
        let host = PeerPresence::attach(&host_broker, Arc::clone(&host_net), Vec::new());
        let guest = PeerPresence::attach(&guest_broker, Arc::clone(&guest_net), Vec::new());
        host.set_rooms(rooms_of(&host_handle));
        guest.set_rooms(rooms_of(&guest_handle));

        // No host pointer event has arrived. Optional position is the same
        // shared input datum delivered by either native adapter.
        guest.set_cursor_position(None);
        assert!(
            !guest.publish_local_cursor(&room.to_string()),
            "a session with no pointer has nothing to say about where one is",
        );
        assert!(
            guest_cmds.try_recv().is_err(),
            "and nothing may reach the wire for it",
        );

        let mut on_host = PresenceCtx::default();
        assert!(!host.pump(&mut on_host), "so the host has nobody to paint");
        assert!(
            !on_host.pointers.contains_key(&PresenceKey::Workspace {
                id: room.to_string()
            }),
            "and no pill is drawn for a participant who never pointed at anything",
        );

        guest.set_cursor_position(Some((640.0, 610.0)));
        assert!(guest.publish_local_cursor(&room.to_string()));
        over_the_wire(published(&mut guest_cmds), &host_net);
        assert!(host.pump(&mut on_host));
        assert_eq!(
            on_host
                .pointers
                .get(&PresenceKey::Workspace {
                    id: room.to_string()
                })
                .map(|p| p[0].1),
            Some((640.0, 610.0)),
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Asserted across two real sessions rather than against a closure.
    #[test]
    fn a_pointer_from_a_room_this_session_is_not_in_is_refused() {
        let stranger_peer = peer_id(33);
        let (host_tree, _room) = host_naming(&[(&stranger_peer, "Nobody")]);

        let mut stranger_tree = WorkspaceTree::default();
        let stranger_ws = stranger_tree.append_empty_workspace("Elsewhere");
        let elsewhere = stranger_tree
            .room_of(stranger_ws)
            .expect("a fresh workspace mints a room");

        let host_handle = host_tree.shared();
        let stranger_handle = stranger_tree.shared();
        let (host_net, _host_cmds) = shell_network_for_test(host_handle.clone(), 0xBEEFu64);
        let (stranger_net, mut stranger_cmds) =
            shell_network_for_test(stranger_handle.clone(), handle_of(&stranger_peer));
        let host_net = Arc::new(host_net);
        let stranger_net = Arc::new(stranger_net);

        let dir = std::env::temp_dir().join(format!("tilo-stranger-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let host_broker = broker(&dir, "h.sock");
        let stranger_broker = broker(&dir, "s.sock");
        let host = PeerPresence::attach(&host_broker, Arc::clone(&host_net), Vec::new());
        let stranger =
            PeerPresence::attach(&stranger_broker, Arc::clone(&stranger_net), Vec::new());
        host.set_rooms(rooms_of(&host_handle));
        stranger.set_rooms(rooms_of(&stranger_handle));

        stranger.set_cursor(10.0, 10.0);
        assert!(stranger.publish_local_cursor(&elsewhere.to_string()));
        over_the_wire(published(&mut stranger_cmds), &host_net);

        let mut on_host = PresenceCtx::default();
        assert!(
            !host.pump(&mut on_host),
            "a room this session is not in is not this session's to hold",
        );
        assert!(
            on_host.pointers.is_empty(),
            "and nothing may be filed under it",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Put the fixture's workspace on screen — `append_empty_workspace`
    /// doesn't move `active`, but the roster reads whichever room that is.
    fn looking_at_its_room(tree: &mut WorkspaceTree) {
        tree.active = tree
            .workspaces
            .first()
            .expect("the fixture makes a workspace")
            .id;
    }

    /// A session's own view of who is in the room it is looking at.
    fn roster_of(
        tree: &crate::shell::state::WorkspaceTreeHandle,
    ) -> Vec<(String, Option<String>, bool)> {
        tree.read()
            .active_workspace()
            .map(|workspace| workspace.collaborators.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|c| (c.name.clone(), c.role_id.clone(), c.invited))
            .collect()
    }

    /// Stand a session up around `tree` with `peer` as its identity.
    /// Returns the relay and the command channel, which has to be kept
    /// alive or the actor's sends start failing.
    fn session(
        dir: &std::path::Path,
        sock: &str,
        tree: &crate::shell::state::WorkspaceTreeHandle,
        peer: &str,
    ) -> (
        PeerPresence,
        Arc<ShellNetwork>,
        tokio::sync::mpsc::Receiver<NetCmd>,
    ) {
        let (net, cmds) = shell_network_for_test(tree.clone(), handle_of(peer));
        let net = Arc::new(net);
        let relay = PeerPresence::attach(&broker(dir, sock), Arc::clone(&net), Vec::new());
        relay.set_rooms(rooms_of(tree));
        (relay, net, cmds)
    }

    /// `refresh_roster` must actually write `WorkspaceTree::collaborators`.
    #[test]
    fn a_peer_who_joined_is_in_the_roster() {
        let guest_peer = peer_id(21);
        let host_peer = peer_id(22);
        let (mut host_tree, room) = host_naming(&[(&guest_peer, "Xaver")]);
        let mut guest_tree = guest_joining(&mut host_tree, room);
        looking_at_its_room(&mut host_tree);
        looking_at_its_room(&mut guest_tree);
        let host_handle = host_tree.shared();
        let guest_handle = guest_tree.shared();

        let dir = std::env::temp_dir().join(format!("tilo-roster-join-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let (host, host_net, _host_cmds) = session(&dir, "h.sock", &host_handle, &host_peer);
        let (guest, _guest_net, mut guest_cmds) =
            session(&dir, "g.sock", &guest_handle, &guest_peer);

        assert!(
            roster_of(&host_handle).is_empty(),
            "before anyone arrives the room has nobody in it",
        );

        guest.set_cursor(420.0, 300.0);
        assert!(guest.publish_local_cursor(&room.to_string()));
        over_the_wire(published(&mut guest_cmds), &host_net);

        let mut on_host = PresenceCtx::default();
        assert!(host.pump(&mut on_host), "the frame folds the guest in");
        assert!(
            host.refresh_roster(&on_host),
            "and the room's people changed, which is a reason to repaint",
        );

        assert_eq!(
            roster_of(&host_handle),
            vec![("Xaver".to_string(), None, false)],
            "the host's roster must name the person who joined it, as \
             somebody who is here rather than somebody who was asked",
        );
        assert_eq!(
            host_handle.read().active_workspace().unwrap().collaborators[0].session,
            Some(handle_of(&guest_peer)),
            "trusted screen-sharing disclosure joins viewers through the authenticated session",
        );

        assert!(
            !host.refresh_roster(&on_host),
            "restating the same room's people is not a change",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A plain invite says nothing about who its token is for, so the peer
    /// who redeems one has no name in the room and is listed as its id.
    #[test]
    fn a_peer_the_room_has_not_named_is_listed_as_its_id() {
        let guest_peer = peer_id(31);
        let host_peer = peer_id(32);
        let (mut host_tree, room) = host_naming(&[]);
        let mut guest_tree = guest_joining(&mut host_tree, room);
        looking_at_its_room(&mut host_tree);
        looking_at_its_room(&mut guest_tree);
        let host_handle = host_tree.shared();
        let guest_handle = guest_tree.shared();

        let dir = std::env::temp_dir().join(format!("tilo-roster-unnamed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let (host, host_net, _host_cmds) = session(&dir, "h.sock", &host_handle, &host_peer);
        let (guest, _guest_net, mut guest_cmds) =
            session(&dir, "g.sock", &guest_handle, &guest_peer);

        guest.set_cursor(10.0, 20.0);
        assert!(guest.publish_local_cursor(&room.to_string()));
        over_the_wire(published(&mut guest_cmds), &host_net);

        let mut on_host = PresenceCtx::default();
        host.pump(&mut on_host);
        host.refresh_roster(&on_host);

        let expected = format!("peer@{}", handle_of(&guest_peer));
        assert_eq!(
            roster_of(&host_handle),
            vec![(expected, None, false)],
            "an unnamed peer keeps its id as its label",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A host names itself when it mints an invite; the roster must not
    /// read that back as a peer of its own.
    #[test]
    fn the_room_does_not_list_the_session_reading_it() {
        let guest_peer = peer_id(41);
        let host_peer = peer_id(42);
        let (mut host_tree, room) = host_naming(&[(&guest_peer, "Xaver"), (&host_peer, OWNER)]);
        let mut guest_tree = guest_joining(&mut host_tree, room);
        looking_at_its_room(&mut host_tree);
        looking_at_its_room(&mut guest_tree);
        let host_handle = host_tree.shared();
        let guest_handle = guest_tree.shared();

        let dir = std::env::temp_dir().join(format!("tilo-roster-self-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let (host, host_net, _host_cmds) = session(&dir, "h.sock", &host_handle, &host_peer);
        let (guest, _guest_net, mut guest_cmds) =
            session(&dir, "g.sock", &guest_handle, &guest_peer);

        guest.set_cursor(1.0, 2.0);
        assert!(guest.publish_local_cursor(&room.to_string()));
        over_the_wire(published(&mut guest_cmds), &host_net);
        let mut on_host = PresenceCtx::default();
        host.pump(&mut on_host);
        host.refresh_roster(&on_host);

        let listed: Vec<String> = roster_of(&host_handle)
            .into_iter()
            .map(|(n, ..)| n)
            .collect();
        assert_eq!(
            listed,
            vec!["Xaver".to_string()],
            "the roster is who else is here, and the host is not else",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Nothing crosses a wire here — only the room document's naming.
    #[test]
    fn a_named_peer_who_has_not_arrived_reads_as_invited() {
        let host_peer = peer_id(51);
        let guest_peer = peer_id(52);
        let (mut host_tree, room) = host_naming(&[(&host_peer, OWNER)]);
        let mut guest_tree = guest_joining(&mut host_tree, room);
        looking_at_its_room(&mut guest_tree);
        let guest_handle = guest_tree.shared();

        let dir = std::env::temp_dir().join(format!("tilo-roster-guest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let (guest, _guest_net, _guest_cmds) = session(&dir, "g.sock", &guest_handle, &guest_peer);

        let empty = PresenceCtx::default();
        assert!(
            guest.refresh_roster(&empty),
            "a room whose authority has named somebody has a row to show",
        );
        assert_eq!(
            roster_of(&guest_handle),
            vec![(OWNER.to_string(), Some("owner".to_string()), true)],
            "the row carries the name and the role the room replicated, \
             and says that peer has not arrived",
        );

        assert_eq!(
            crate::shell::state::active_workspace_peer_count(&guest_handle.read()),
            0,
            "an invitation nobody has redeemed is not a peer in the room",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An invitation becomes a participant without a pointer — joining
    /// isn't moving a mouse. The host observes the guest asking the
    /// room's document for its changes, under its authenticated identity.
    #[test]
    fn arriving_turns_an_invitation_into_a_participant() {
        let guest_peer = peer_id(61);
        let host_peer = peer_id(62);
        let (mut host_tree, room) = host_naming(&[(&guest_peer, "Xaver")]);
        looking_at_its_room(&mut host_tree);
        let host_handle = host_tree.shared();

        let dir = std::env::temp_dir().join(format!("tilo-roster-beat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temp dir");
        let (host, host_net, _host_cmds) = session(&dir, "h.sock", &host_handle, &host_peer);

        let quiet = PresenceCtx::default();
        assert!(host.refresh_roster(&quiet));
        assert_eq!(
            roster_of(&host_handle),
            vec![("Xaver".to_string(), None, true)],
            "before he redeems anything, quinten has invited him and no more",
        );
        assert_eq!(
            crate::shell::state::active_workspace_peer_count(&host_handle.read()),
            0,
            "so the top bar still offers to invite the first person",
        );

        // Redeems it: the same request `accept_invite` sends.
        {
            let mut replica = host_net.replica.lock().expect("the replica lock");
            replica.handle_from(
                &crate::shell::state::PeerOrigin::peer_id(&guest_peer),
                &NeighbourMsg::SyncReq {
                    since: serde_json::to_vec(&Vec::<automerge::ChangeHash>::new())
                        .expect("empty heads serialise"),
                    room: room.to_string(),
                    invite_id: None,
                },
            );
        }

        assert!(
            host.refresh_roster(&quiet),
            "the room's people changed, which is what makes the frame repaint",
        );
        assert_eq!(
            roster_of(&host_handle),
            vec![("Xaver".to_string(), None, false)],
            "the same row, now a participant — this is the change a \
             viewer sees at the moment somebody joins",
        );
        assert_eq!(
            crate::shell::state::active_workspace_peer_count(&host_handle.read()),
            1,
            "and the bar shows him instead of offering to invite him",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
