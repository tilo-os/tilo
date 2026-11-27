#[allow(
    clippy::wildcard_imports,
    reason = "one part of the broker module: the shared state, its imports and the other parts reach this through the parent glob"
)]
use super::*;

pub(super) struct ContextSub {
    pub(super) peer_id: u64,
    pub(super) tx: mpsc::UnboundedSender<ContextUpdate>,
    /// Authenticated native membership projection and the exact admission it
    /// came from. Incremental label/presence frames never replace it.
    pub(super) native: Option<NativeContext>,
    /// The apps installed here, by what each is called, this receiver was last told: it is sent
    /// the context again when one is installed, removed, or renamed.
    pub(super) apps: Vec<collab_types::history::NamedApp>,
    /// What the workspace's `workspace/members` resource was read from when this receiver was
    /// last told it moved ([`AppBrokerState::members_digest`]): it is told again only when that
    /// changes, not on every re-check of its admission.
    pub(super) members: Option<[u8; 32]>,
}

#[derive(Clone)]
pub(super) struct NativeContext {
    pub(super) admitted: AdmittedJoinIdentity,
}

/// Per-channel subscriber registry, keyed by the record's channel key. A
/// `channelId` carries the whole address ([`runtime_collab::route::channel_key`]
/// encodes `(app, cluster, record)`), decoded via
/// [`CrdtRoute::from_channel_key`]; delivery is [`CrdtRoute::delivers_to`].
#[derive(Default)]
pub(super) struct ChannelSubscribers {
    pub(super) context: Vec<ContextSub>,
}

/// An owner commit being waited for: dropping the wait before the owner
/// answered cannot convert an admitted effect into a refusal or leave the
/// standalone capability apparently healthy, so the owner's outcome is unknown
/// from then on.
pub(super) struct OwnerWait<'a> {
    pub(super) failure: &'a Mutex<Option<String>>,
    pub(super) answered: bool,
}

impl<'a> OwnerWait<'a> {
    pub(super) const fn on(failure: &'a Mutex<Option<String>>) -> Self {
        Self {
            failure,
            answered: false,
        }
    }

    pub(super) fn answered(mut self) {
        self.answered = true;
    }
}

impl Drop for OwnerWait<'_> {
    fn drop(&mut self) {
        if !self.answered
            && let Ok(mut failure) = self.failure.lock()
        {
            failure.get_or_insert_with(|| "owner commit interrupted; outcome unknown".into());
        }
    }
}

// Stream servers

// The queues below are `Rc<AsyncMutex<mpsc::UnboundedReceiver<_>>>` rather
// than `Rc<RefCell<_>>`. Two readers can ask for the next invocation before
// the first one's `recv().await` resolves; a `RefCell` would then panic on the
// second `borrow_mut()` (and this crate is `panic = "abort"`, so that panic
// takes the whole desktop session down). The async mutex instead makes the
// second reader wait its turn — the two calls serialize instead of racing.

impl AppBrokerState {
    /// What a window with no native endpoint on `channel_id` is told, as `peer_id`.
    #[cfg(test)]
    pub(crate) fn watch_context(
        &self,
        channel_id: &str,
        peer_id: u64,
    ) -> mpsc::UnboundedReceiver<ContextUpdate> {
        self.subscribe_context(channel_id, peer_id, None)
    }

    /// Delegates to the shared fanout registry for typed connection ids.
    pub(super) fn next_peer_id(&self) -> u64 {
        // Peer table torn down: racing with shutdown. Hand out a
        // placeholder id; the caller drops it on the next tokio yield.
        self.fanout.upgrade().map_or(u64::MAX, |t| t.next_peer_id())
    }
    /// Drive an event onto the subscribed queues `route` addresses,
    /// skipping `except`. Called from [`FanoutState::fanout_to_route`]
    /// Delegates to
    /// [`CrdtRoute::delivers_to`] rather than reimplementing the filter,
    /// so RPC and in-process delivery cannot drift.
    pub fn fanout_to_route(&self, event: &CollabEvent, except: u64, route: &CrdtRoute) {
        if route.cluster_id.is_empty() {
            return;
        }
        let Ok(mut all_channels) = self.channels.lock() else {
            return;
        };
        let channels = all_channels
            .iter_mut()
            .filter(|(k, _)| route.delivers_to(&CrdtRoute::from_channel_key(k)))
            .map(|(_, subs)| subs);
        let pruned = match event {
            CollabEvent::Context { permissions, apps } => {
                Self::fanout_context(channels, except, permissions, apps)
            }
            CollabEvent::FollowChanged { peer_id } => {
                Self::fanout_follow_changed(channels, except, peer_id.as_ref())
            }
            CollabEvent::ResourcesArrived { records } => subscriptions::context_frame(
                channels,
                except,
                &ContextUpdate::ResourcesArrived(records.clone()),
            ),
            CollabEvent::RecordsMoved { records } => subscriptions::context_frame(
                channels,
                except,
                &ContextUpdate::RecordsMoved(records.clone()),
            ),
            // What a member says reaches the apps as the frame they said it in ([`Self::heard`]).
            CollabEvent::RemoteCursorMoved { .. }
            | CollabEvent::RemotePresenceUpdate { .. }
            | CollabEvent::Document { .. }
            | CollabEvent::KeyAccess { .. }
            | CollabEvent::CommittedDocument { .. }
            | CollabEvent::ResourceUnavailable { .. }
            | CollabEvent::Invocation { .. }
            | CollabEvent::InvocationCancellation { .. }
            | CollabEvent::LocalDocumentActionReported { .. }
            | CollabEvent::WorkspaceControl { .. }
            | CollabEvent::ResourceCommandResult { .. }
            | CollabEvent::ActionVerdicts { .. }
            | CollabEvent::DocumentContent { .. }
            | CollabEvent::DocumentContentState { .. }
            | CollabEvent::RemoteAction { .. }
            | CollabEvent::Handoff { .. } => {
                // Invocation delivery, cooperative cancellation and the save a
                // window is asked ([`Self::ask_save`]) are point-to-point and
                // never enter route fanout.
                false
            }
        };
        // A window that went away is no longer shown by the record its app
        // held: the device's daemon publishes what the windows hold again.
        if pruned {
            self.windows_changed();
        }
    }

    /// Ask the window `route` names to make everything it shows durable, as request `token`; its
    /// app's answer settles `answer` ([`Self::answered_save`]). When no connection shows that
    /// window, `answer` is dropped at once, and when the connection asked goes away before it
    /// answers, then: its waiter hears no answer either way.
    pub fn ask_save(
        &self,
        route: &CrdtRoute,
        token: u64,
        answer: tilo_actions::remote::Completer<HandoffReport>,
    ) {
        let key = route.channel_key();
        let frame = ContextUpdate::Handoff(
            collab_types::handoff::HandoffRequest::Checkpoint { token }.encode(),
        );
        // Noted before the frame goes out, so an answer racing it finds what it answers.
        let mut saves = self
            .saves
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let asked = self.channels.lock().ok().and_then(|mut channels| {
            let subscribers = channels.get_mut(&key)?;
            subscribers.context.retain(|sub| !sub.tx.is_closed());
            subscribers
                .context
                .iter()
                .rev()
                .find(|sub| sub.tx.send(frame.clone()).is_ok())
                .map(|sub| sub.peer_id)
        });
        let unanswered = match asked {
            Some(peer) => saves.insert((key, token), (peer, answer)),
            None => Some((0, answer)),
        };
        drop(saves);
        drop(unanswered);
    }

    /// The app of the window `route` names answered a save it was asked
    /// ([`Self::ask_save`]): `message` is its encoded [`HandoffReport`].
    pub(super) fn answered_save(&self, route: &CrdtRoute, message: &[u8]) -> Result<(), String> {
        let report = HandoffReport::decode(message)
            .ok_or_else(|| "a window's answer to a save does not decode".to_owned())?;
        let token = report.token();
        let asked = self
            .saves
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&(route.channel_key(), token));
        let (_, answer) =
            asked.ok_or_else(|| format!("this window was asked no save numbered {token}"))?;
        answer.complete(report);
        Ok(())
    }

    /// The saves the connection whose route holds are `holds` was asked and did not answer:
    /// their waiters hear no answer now.
    pub(super) fn drop_saves_of(&self, holds: &[u64]) {
        let mut saves = self
            .saves
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (dropped, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut *saves)
            .into_iter()
            .partition(|(_, (peer, _))| holds.contains(peer));
        saves.extend(kept);
        drop(saves);
        drop(dropped);
    }

    /// Hand `frame`, what `member` said in the workspace `scope` addresses, to every app
    /// there but the connection `except`, as the live session carries it.
    pub fn heard(
        &self,
        member: &collab_types::MemberId,
        frame: &collab_types::presence::Presence,
        except: u64,
        scope: &CrdtRoute,
    ) {
        if scope.cluster_id.is_empty() {
            return;
        }
        let Ok(mut all_channels) = self.channels.lock() else {
            return;
        };
        let channels = all_channels
            .iter_mut()
            .filter(|(k, _)| scope.delivers_to(&CrdtRoute::from_channel_key(k)))
            .map(|(_, subs)| subs);
        let heard = ContextUpdate::Heard {
            member: member.clone(),
            frame: Box::new(frame.clone()),
        };
        if subscriptions::context_frame(channels, except, &heard) {
            self.windows_changed();
        }
    }
    /// One arm of [`Self::fanout_to_route`]: the workspace's permission
    /// context and the names of the apps installed here. Who the members are
    /// is its `workspace/members` resource, read through the store.
    pub(super) fn fanout_context<'a>(
        channels: impl Iterator<Item = &'a mut ChannelSubscribers>,
        except: u64,
        permissions: &[String],
        apps: &[collab_types::history::NamedApp],
    ) -> bool {
        let env = ContextUpdate::Snapshot {
            permissions: permissions.to_vec(),
            apps: apps.to_vec(),
        };
        let mut pruned = false;
        for subs in channels {
            let before = subs.context.len();
            subs.context.retain_mut(|s| {
                if s.peer_id == except {
                    return true;
                }
                // App-authored context is not complete membership authority.
                // Native receivers get owner-backed add/remove refreshes from
                // `refresh_workspace_context_services` instead.
                if s.native.is_some() {
                    return true;
                }
                if s.tx.send(env.clone()).is_err() {
                    return false;
                }
                s.apps = apps.to_vec();
                true
            });
            pruned |= subs.context.len() != before;
        }
        pruned
    }
    /// Session-local view binding. It rides the session context stream but
    /// is explicitly tagged so clients do not replace their roster with an
    /// empty control frame.
    pub(super) fn fanout_follow_changed<'a>(
        channels: impl Iterator<Item = &'a mut ChannelSubscribers>,
        except: u64,
        peer_id: Option<&tilo_actions::MemberId>,
    ) -> bool {
        let env = ContextUpdate::FollowChanged {
            peer_id: peer_id.cloned(),
        };
        let mut pruned = false;
        for subs in channels {
            let before = subs.context.len();
            subs.context.retain(|sub| {
                if sub.peer_id == except {
                    return true;
                }
                sub.tx.send(env.clone()).is_ok()
            });
            pruned |= subs.context.len() != before;
        }
        pruned
    }
    /// Session-local handoff control for the instance(s) on the addressed
    /// primary route. Like Follow, it rides the context stream tagged so the
    /// client never mistakes it for a roster.
    pub(crate) fn ensure_channel(&self, channel_id: &str) {
        if let Ok(mut m) = self.channels.lock() {
            m.entry(channel_id.to_owned())
                .or_insert_with(ChannelSubscribers::default);
        }
    }
    pub(super) fn subscribe_context(
        &self,
        channel_id: &str,
        peer_id: u64,
        native: Option<NativeContext>,
    ) -> mpsc::UnboundedReceiver<ContextUpdate> {
        let (tx, rx) = mpsc::unbounded_channel();
        // What each app installed here is called: what this window's history names another
        // app's changes by. The members themselves are the workspace's `workspace/members`
        // resource, which the window reads through the store when this frame reaches it.
        let apps = self.installed_apps();
        let _ = tx.send(ContextUpdate::Snapshot {
            permissions: Vec::new(),
            apps: apps.clone(),
        });
        let certified = native.is_some();
        let held = tx.clone();
        if let Ok(mut m) = self.channels.lock() {
            m.entry(channel_id.to_owned())
                .or_insert_with(ChannelSubscribers::default)
                .context
                .push(ContextSub {
                    peer_id,
                    tx,
                    native,
                    apps,
                    members: None,
                });
        }
        // A window's app now holds this record: the device's daemon publishes it.
        self.windows_changed();
        // The records of its app's trees this device already holds in the
        // workspace ([`BrokerChannels::tree_records_held`]), such as those it was
        // replayed when it joined, before any window of the app ran: a
        // window learns those only from here. Read after the subscription
        // is in place, so a record admitted meanwhile is told either here
        // or live (the shell announces a record once it is admitted).
        subscriptions::held_tree_records(&self.broker_channels, channel_id, &held);
        if certified {
            // A new admitted session brings its own certificate expiry: the
            // policy turn re-checks, and from then on wakes at that expiry.
            tilo_identity::policy_change::announce();
        }
        rx
    }
    /// Who is in `workspace` now, as its membership names them: its people
    /// and the agents delegated in it. Presentation only, as the roster is:
    /// nothing is admitted by it, and a host that keeps no membership names
    /// nobody.
    pub fn here(
        &self,
        caller: &tilo_actions::remote::Caller,
    ) -> Option<Vec<tilo_actions::remote::Here>> {
        let workspace = caller.workspace()?;
        let now = self.policy_now_ns().ok()? / 1_000_000_000;
        let (agents, people) = self
            .membership
            .as_ref()?
            .decide_in_workspace(workspace, |workspace| {
                let workspace = workspace?;
                let agents = workspace.policy.agent_members_at(now);
                let people: Vec<(String, String)> = workspace
                    .policy
                    .people()
                    .into_iter()
                    .map(|(person, _)| person)
                    .filter(|person| !workspace.policy.is_blocked(person))
                    .map(|person| {
                        let wanted = collab_types::MemberId::person(person.clone());
                        let name = workspace
                            .members
                            .iter()
                            .find(|member| member.id == wanted)
                            .map_or_else(|| person.clone(), |member| member.name.clone());
                        (person, name)
                    })
                    .collect();
                Some((agents, people))
            })
            .ok()??;
        let listed = self
            .services
            .agents
            .as_deref()
            .map(|roster| roster.listed(workspace))
            .unwrap_or_default();
        let mut here: Vec<tilo_actions::remote::Here> = people
            .into_iter()
            .map(|(who, name)| tilo_actions::remote::Here {
                who,
                name,
                kind: "person".into(),
                works_in: None,
                devices: Vec::new(),
            })
            .collect();
        here.extend(
            agents
                .into_iter()
                .map(|(agent, _)| tilo_actions::remote::Here {
                    who: String::from_utf8_lossy(&agent.actor_bytes()).into_owned(),
                    name: Self::agent_name(&listed, &agent),
                    kind: "agent".into(),
                    works_in: None,
                    devices: Vec::new(),
                }),
        );
        if let Some(present) = &self.services.present_devices {
            present.attach(workspace, &mut here);
        }
        Some(here)
    }
    /// The next time what a watch of `caller`'s workspace follows may have changed, as this
    /// broker can tell it ([`tilo_actions::remote::ActionProvider::changed`]): the roster it reads
    /// from its membership and its agents' grants, and nothing for the records, which its apps
    /// keep and whose writes it is not told of.
    pub fn changed(
        &self,
        caller: &tilo_actions::remote::Caller,
        scope: tilo_actions::remote::WatchScope,
    ) -> Option<tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>>>
    {
        self.here(caller)?;
        match scope {
            tilo_actions::remote::WatchScope::Records => None,
            tilo_actions::remote::WatchScope::Here => Some(self.policy_changed().map(Ok)),
        }
    }
    /// The name a person reads for `agent`: the one the workspace's record of its agents gives
    /// it, or its id where the record names no agent.
    pub(super) fn agent_name(
        listed: &[crate::services::ListedAgent],
        agent: &collab_types::MemberId,
    ) -> String {
        listed
            .iter()
            .find(|named| Self::lists(named, agent))
            .map_or_else(|| agent.id().to_owned(), |named| named.name.clone())
    }
    /// Whether `named`, as the record of the workspace's agents keeps it, is the agent `agent`:
    /// its id, of the person it acts for.
    fn lists(named: &crate::services::ListedAgent, agent: &collab_types::MemberId) -> bool {
        agent
            .as_agent()
            .is_some_and(|(id, person)| id == named.id && person.as_str() == named.acts_for)
    }
    /// Complete workspace membership for one retained native receiver is the
    /// workspace's `workspace/members` resource, which its windows read through
    /// the store: a native receiver is kept only for the admission it joined
    /// under, and is told when that membership may have moved.
    pub(super) fn native_membership_roster(
        &self,
        route: &CrdtRoute,
        _admitted: &AdmittedJoinIdentity,
    ) -> Result<(), String> {
        let membership = self
            .membership
            .as_ref()
            .ok_or("native context has no membership authority")?;
        let now = self.policy_now_ns()? / 1_000_000_000;
        membership.decide_in_workspace(&route.cluster_id, |workspace| {
            let workspace = workspace.ok_or("native context workspace is unavailable")?;
            // An agent the workspace admits is named by its id and the person it acts for, and
            // an id its terms forbid names nobody.
            for (agent, _) in workspace.policy.agent_members_at(now) {
                tilo_peer_state::workspace_membership::valid_agent_id(agent.id())
                    .map_err(|_| "native context contains a noncanonical agent identity")?;
            }
            Ok::<_, &str>(())
        })??;
        Ok(())
    }
    pub(super) fn native_roster_for_subscriber(
        &self,
        route: &CrdtRoute,
        admitted: &AdmittedJoinIdentity,
    ) -> Result<Option<NativeContext>, String> {
        let Some(owner) = self.services.workspace_memberships.as_ref() else {
            return Ok(None);
        };
        if !owner.requires_workspace(&route.cluster_id) {
            return Ok(None);
        }
        let current = self
            .revalidate_admission(route, admitted)
            .map_err(|error| error.to_string())?;
        self.native_membership_roster(route, &current)?;
        Ok(Some(NativeContext { admitted: current }))
    }
    /// What each app installed on this device is called, as the host's authority knows it: what
    /// a window's history names another app's change by. Presentation only.
    pub(super) fn installed_apps(&self) -> Vec<collab_types::history::NamedApp> {
        self.membership
            .as_ref()
            .map(|membership| membership.installed_apps())
            .unwrap_or_default()
    }
    pub(super) fn refresh_workspace_context_services(&self) -> Result<(), String> {
        for (channel, peer_id, admitted) in self.native_context_targets()? {
            let route = CrdtRoute::from_channel_key(&channel);
            if self.revalidate_admission(&route, &admitted).is_err() {
                self.drop_context_subscriber(&channel, peer_id, &admitted)?;
                continue;
            }
            self.tell_context_subscriber(&channel, peer_id, &admitted)?;
        }
        Ok(())
    }
    /// Every native context subscription as its channel, its subscriber and
    /// the admission it was let in by.
    pub(super) fn native_context_targets(
        &self,
    ) -> Result<Vec<(String, u64, AdmittedJoinIdentity)>, String> {
        Ok(self
            .channels
            .lock()
            .map_err(|_| "native context subscriptions unavailable")?
            .iter()
            .flat_map(|(channel, subscribers)| {
                subscribers
                    .context
                    .iter()
                    .filter_map(|subscriber| {
                        subscriber.native.as_ref().map(|native| {
                            (channel.clone(), subscriber.peer_id, native.admitted.clone())
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .collect())
    }
    /// Drop a subscriber of `channel` whose admission `admitted` no longer
    /// revalidates, and leave any other subscriber that joined under another
    /// admission of the same peer alone.
    pub(super) fn drop_context_subscriber(
        &self,
        channel: &str,
        peer_id: u64,
        admitted: &AdmittedJoinIdentity,
    ) -> Result<(), String> {
        let mut channels = self
            .channels
            .lock()
            .map_err(|_| "native context subscriptions unavailable")?;
        let mut dropped = false;
        if let Some(subscribers) = channels.get_mut(channel) {
            let before = subscribers.context.len();
            subscribers.context.retain(|subscriber| {
                subscriber.peer_id != peer_id
                    || subscriber
                        .native
                        .as_ref()
                        .is_none_or(|native| native.admitted != *admitted)
            });
            dropped = subscribers.context.len() != before;
        }
        drop(channels);
        if dropped {
            self.windows_changed();
        }
        Ok(())
    }
    /// Tell one native context subscriber that its workspace's membership moved: who the
    /// members are is its `workspace/members` resource, which the window reads again through the
    /// store, so it is told only when what that resource is read from changed since it was last
    /// told, and the names of the apps installed here are sent when one was installed or removed
    /// since. Returns whether it heard something.
    pub(super) fn tell_context_subscriber(
        &self,
        channel: &str,
        peer_id: u64,
        admitted: &AdmittedJoinIdentity,
    ) -> Result<bool, String> {
        let apps = self.installed_apps();
        let members = self.members_digest(&CrdtRoute::from_channel_key(channel).cluster_id);
        let members_moved =
            ContextUpdate::RecordsMoved(vec![collab_types::store::MovedRecord::whole(
                tilo_resource_store::WORKSPACE_MEMBERS_ID,
            )]);
        let mut channels = self
            .channels
            .lock()
            .map_err(|_| "native context subscriptions unavailable")?;
        let mut changed = false;
        let mut gone = false;
        if let Some(subscribers) = channels.get_mut(channel) {
            subscribers.context.retain_mut(|subscriber| {
                let Some(native) = subscriber.native.as_mut() else {
                    return true;
                };
                if subscriber.peer_id != peer_id || native.admitted != *admitted {
                    return true;
                }
                if members.is_none() || subscriber.members != members {
                    if subscriber.tx.send(members_moved.clone()).is_err() {
                        gone = true;
                        return false;
                    }
                    subscriber.members = members;
                    changed = true;
                }
                if subscriber.apps == apps {
                    return true;
                }
                if subscriber
                    .tx
                    .send(ContextUpdate::Snapshot {
                        permissions: Vec::new(),
                        apps: apps.clone(),
                    })
                    .is_err()
                {
                    gone = true;
                    return false;
                }
                subscriber.apps.clone_from(&apps);
                changed = true;
                true
            });
        }
        drop(channels);
        // A subscriber that could not be told went away: what the windows here
        // hold is the daemon's to publish again.
        if gone {
            self.windows_changed();
        }
        Ok(changed)
    }
    /// A digest of what `workspace`'s `workspace/members` resource is read from, as this host's
    /// authority holds it now: its policy, the people and members its governance names, and the
    /// agents its record lists. `None` when it cannot be read, which tells every receiver.
    pub(super) fn members_digest(&self, workspace: &str) -> Option<[u8; 32]> {
        let membership = self.membership.as_ref()?;
        let mut hasher = blake3::Hasher::new();
        membership
            .decide_in_workspace(workspace, |held| {
                let held = held?;
                let governance = (&held.policy, &held.people_names, &held.members);
                hasher.update(&postcard::to_stdvec(&governance).ok()?);
                Some(())
            })
            .ok()??;
        for agent in self
            .services
            .agents
            .as_deref()
            .map(|roster| roster.listed(workspace))
            .unwrap_or_default()
        {
            for field in [&agent.id, &agent.name, &agent.acts_for] {
                hasher.update(&(field.len() as u64).to_le_bytes());
                hasher.update(field.as_bytes());
            }
            let avatar = agent.avatar.as_deref().unwrap_or_default();
            hasher.update(&(avatar.len() as u64).to_le_bytes());
            hasher.update(avatar.as_bytes());
        }
        Some(*hasher.finalize().as_bytes())
    }
    /// Re-check every membership surface and native context subscriber,
    /// and register with
    /// [`crate::policy_schedule::announce_at`] the first instant at
    /// which a time-bound input they read (a capability's expiry, a
    /// subscriber certificate's validity) changes the answer. Returns whether
    /// the next re-check must come on the one-second clock instead of on an
    /// announcement: an input nothing announces, or a check that failed.
    pub(super) fn refresh_policy_surfaces(self: &Arc<Self>) -> bool {
        // Read only the workspaces' grants: decided again whatever else this host serves.
        self.refresh_shown_actions();
        if self.services.workspace_memberships.is_none() || self.membership.is_none() {
            // No native context subscriber can be admitted: nothing here changes with policy.
            return false;
        }
        // Sampled before anything is read: an expiry between this sample
        // and a read is registered as well, so none is skipped.
        let before = self.policy_now_ns();
        let context_failed = self
            .refresh_workspace_context_services()
            .inspect_err(|error| {
                tracing::warn!(%error, "native membership context could not be refreshed");
            })
            .is_err();
        let clocked = context_failed;
        let announced = self
            .membership
            .as_ref()
            .is_some_and(|membership| membership.announces_policy_changes())
            && tilo_identity::policy_change::timers_reliable();
        let Ok(before) = before else {
            return true;
        };
        if clocked || !announced {
            return true;
        }
        let Ok((next, reads_fleet)) = self.next_policy_change(before) else {
            return true;
        };
        if reads_fleet && !tilo_identity::policy_change::identity_files_watched() {
            // Another process may revoke a device in the fleet file; without
            // its watch, the fleet is re-read every second as before.
            return true;
        }
        if let Some(at) = next {
            let Ok(policy_now) = self.policy_now_ns() else {
                return true;
            };
            crate::policy_schedule::announce_at(crate::policy_schedule::wall_instant(
                at,
                policy_now,
                tilo_clock::unix_now_ns(),
            ));
        }
        false
    }
    /// The first policy instant (Unix nanoseconds) after `after` at which a
    /// membership surface or a native context subscriber can be answered
    /// differently with nothing written, and whether a subscriber's
    /// revalidation reads the device fleet.
    pub(super) fn next_policy_change(&self, after: u64) -> Result<(Option<u64>, bool), String> {
        let capabilities = match self.membership.as_ref() {
            Some(membership) => membership
                .decide_in_tree(|tree| tree.next_grant_expiry_after(after / 1_000_000_000))?
                .map(|seconds| seconds.saturating_mul(1_000_000_000)),
            None => None,
        };
        let fleet_authority = self.certificate_authority.get().is_some();
        let mut reads_fleet = false;
        let certificates = self
            .channels
            .lock()
            .map_err(|_| "native context subscriptions unavailable")?
            .values()
            .flat_map(|subscribers| subscribers.context.iter())
            .filter_map(|subscriber| subscriber.native.as_ref()?.admitted.certificate.as_ref())
            .filter_map(|certificate| {
                reads_fleet |= fleet_authority;
                // Not yet valid until issued; no longer valid once expired.
                [Some(certificate.issued_at_ns), certificate.expires_at_ns]
                    .into_iter()
                    .flatten()
                    .filter(|at| *at > after)
                    .min()
            })
            .min();
        Ok((
            capabilities.into_iter().chain(certificates).min(),
            reads_fleet,
        ))
    }
}
