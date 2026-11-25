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
}

#[derive(Clone)]
pub(super) struct NativeContext {
    pub(super) admitted: AdmittedJoinIdentity,
    pub(super) roster: Vec<collaboration::Member>,
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
    pub(super) fn next_service_peer_id(&self) -> u64 {
        // A fanout allocates unique ids. Without one — before the first
        // connection, and on a broker tests drive directly — the placeholder
        // would be handed out again, and a workspace's second service would
        // replace its first in every registry. Take the next id no surface
        // holds instead.
        let candidate = self.next_peer_id();
        let peer_id = if candidate == u64::MAX {
            match self.publishers.surfaces.lock() {
                Ok(surfaces) if surfaces.contains_key(&candidate) => (1..candidate)
                    .rev()
                    .find(|id| !surfaces.contains_key(id))
                    .unwrap_or(candidate),
                _ => candidate,
            }
        } else {
            candidate
        };
        match self.service_peers.lock() {
            Ok(mut peers) => peers.push(peer_id),
            Err(poisoned) => poisoned.into_inner().push(peer_id),
        }
        peer_id
    }
    /// Withdraw the services a broker thread that unwound had started, as
    /// if their connections had closed, before a new thread starts them.
    pub(super) fn forget_service_peers(&self) {
        let peers = match self.service_peers.lock() {
            Ok(mut peers) => std::mem::take(&mut *peers),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        };
        for peer_id in peers {
            self.remove_connection(peer_id);
        }
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
            CollabEvent::Context {
                members,
                permissions,
                apps,
            } => Self::fanout_context(channels, except, members, permissions, apps),
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
            | CollabEvent::NativeMediaCommandResult { .. }
            | CollabEvent::ResourceCommandResult { .. }
            | CollabEvent::ActionVerdicts { .. }
            | CollabEvent::RetiredNearbyTransferCommandResult { .. }
            | CollabEvent::RetiredNearbyTransferChanged
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
        let dropped: Vec<_> = {
            let mut saves = self
                .saves
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let gone: Vec<_> = saves
                .iter()
                .filter(|(_, (peer, _))| holds.contains(peer))
                .map(|(key, _)| key.clone())
                .collect();
            gone.into_iter()
                .filter_map(|key| saves.remove(&key))
                .collect()
        };
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
    /// One arm of [`Self::fanout_to_route`]: the workspace's member +
    /// permission context.
    pub(super) fn fanout_context<'a>(
        channels: impl Iterator<Item = &'a mut ChannelSubscribers>,
        except: u64,
        members: &[collaboration::Member],
        permissions: &[String],
        apps: &[collab_types::history::NamedApp],
    ) -> bool {
        let env = ContextUpdate::Snapshot {
            members: members.to_vec(),
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
        // What each app installed here is called, with the people's names: what this window's
        // history names another app's changes by.
        let apps = self.installed_apps();
        if let Some(native) = native.as_ref() {
            let _ = tx.send(ContextUpdate::Snapshot {
                members: native.roster.clone(),
                permissions: Vec::new(),
                apps: apps.clone(),
            });
        }
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
                    who: format!("agent:{agent}"),
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
    pub(super) fn agent_name(listed: &[crate::apps::services::ListedAgent], agent: &str) -> String {
        listed
            .iter()
            .find(|named| named.id == agent)
            .map_or_else(|| agent.to_owned(), |named| named.name.clone())
    }
    /// The agents in a workspace, named and pictured as its record of its agents names them:
    /// those its grants admit, and those only the record names.
    pub(super) fn named_agent_members(
        mut agents: Vec<(String, String, Option<String>)>,
        listed: Vec<crate::apps::services::ListedAgent>,
    ) -> Vec<(String, String, Option<String>)> {
        for named in listed {
            match agents.iter_mut().find(|(agent, _, _)| *agent == named.id) {
                Some((_, name, avatar)) => {
                    name.clone_from(&named.name);
                    avatar.clone_from(&named.avatar);
                }
                None => agents.push((named.id, named.name, named.avatar)),
            }
        }
        agents.sort_by(|left, right| left.0.cmp(&right.0));
        agents
    }
    /// The picture `person` (their root, bytes) set in their profile, as the image's own bytes:
    /// what a member row showing them as an author draws. `None` where this host keeps none.
    pub(super) fn person_photo(&self, person: &[u8; 32]) -> Option<Vec<u8>> {
        self.services
            .workspace_memberships
            .as_ref()?
            .person_photo(person)
            .filter(|photo| !photo.is_empty())
    }
    /// The workspace's people as member rows, in id order, then those it removed: presence may
    /// supply a label, but only owner-backed membership supplies the identity. An offline member
    /// remains a member.
    pub(super) fn people_members(
        &self,
        workspace: &tilo_desktop_model::Workspace,
    ) -> Vec<collab_types::Member> {
        let mut people: Vec<(String, tilo_action_policy::Role)> = workspace
            .policy
            .people()
            .into_iter()
            .filter(|(person, _)| !workspace.policy.is_blocked(person))
            .collect();
        people.sort_by(|left, right| left.0.cmp(&right.0));
        let mut listed: Vec<collab_types::Member> = people
            .into_iter()
            .map(|(person, role)| {
                let row = workspace
                    .members
                    .iter()
                    .find(|member| member.id == collab_types::MemberId::person(person.clone()));
                // Someone the workspace's governance names is called that here, whoever is
                // present: the same names the agent worker's members are given name them too.
                // Someone nobody named is unnamed, never named by their key: an app calls them as
                // it calls anyone it has no name for.
                let name = workspace
                    .person_name(&person)
                    .unwrap_or_default()
                    .to_owned();
                // The colour their member row stores: the one they chose, else the one their
                // identity derives.
                let colour = row.map_or_else(
                    || collab_types::presence::PresenceHue::of_identity(&person),
                    |row| row.colour,
                );
                let mut member = collab_types::Member::of_person(
                    &collab_types::Person::new(person.clone(), name).wearing(colour),
                    Some(role),
                );
                // The picture they set in their profile, as the image's own bytes: every
                // surface showing them as an author draws it.
                let root = tilo_failure::ignored!(
                    "a member list row names a person by their root key",
                    hex::decode(&person)
                        .ok()
                        .and_then(|root| <[u8; 32]>::try_from(root).ok())
                );
                member.avatar = root
                    .and_then(|root| self.person_photo(&root))
                    .map(collab_types::AvatarBytes);
                member.joined_at = workspace.policy.joined_at(&person);
                member
            })
            .collect();
        // Whoever the workspace removed keeps their row, so what they wrote still names them.
        let removed: Vec<collab_types::Member> = workspace
            .members
            .iter()
            .filter(|member| member.removed && member.id.is_person())
            .filter(|member| !listed.iter().any(|row| row.id == member.id))
            .cloned()
            .collect();
        listed.extend(removed);
        listed
    }
    /// Complete workspace membership for one retained native receiver. This is
    /// membership metadata, not online presence: it mints no pointer, caret,
    /// arrival frame, or transport permission.
    pub(super) fn native_membership_roster(
        &self,
        route: &CrdtRoute,
        admitted: &AdmittedJoinIdentity,
    ) -> Result<Vec<collaboration::Member>, String> {
        let now = self.policy_now_ns()? / 1_000_000_000;
        let (agents, people) = self
            .membership
            .as_ref()
            .ok_or("native context has no membership authority")?
            .decide_in_workspace(&route.cluster_id, |workspace| {
                let workspace = workspace.ok_or("native context workspace is unavailable")?;
                let agents: Vec<(String, String, Option<String>)> = workspace
                    .policy
                    .agent_members_at(now)
                    .into_iter()
                    .map(|(agent, _)| (agent.clone(), agent, None))
                    .collect();
                let people = self.people_members(workspace);
                Ok::<_, &str>((agents, people))
            })??;
        let listed = self
            .services
            .agents
            .as_deref()
            .map(|roster| roster.listed(&route.cluster_id))
            .unwrap_or_default();
        let agents = Self::named_agent_members(agents, listed);
        let self_id = Self::member_id(admitted);
        let mut mine: Option<collab_types::Member> = None;
        let mut members =
            Vec::with_capacity(agents.len().saturating_add(people.len()).saturating_add(1));
        for row in people {
            if row.id == self_id {
                mine = Some(row);
            } else {
                members.push(row);
            }
        }
        for (agent, name, avatar) in agents {
            tilo_peer_state::workspace_membership::valid_agent_id(&agent)
                .map_err(|_| "native context contains a noncanonical agent identity")?;
            let mut row =
                collab_types::Member::of_software(collab_types::MemberId::agent(agent), name);
            row.avatar = avatar.as_deref().and_then(collab_types::avatar_of);
            if row.id == self_id {
                mine = Some(row);
            } else {
                members.push(row);
            }
        }
        // The person is called what the workspace's member list calls them, as everyone else is;
        // never by their key. Their own app may show the name their session gives them.
        let me = mine.unwrap_or_else(|| {
            collab_types::Member::of_person(
                &collab_types::Person::new(admitted.actor_key.clone(), String::new()),
                None,
            )
        });
        members.push(me.claiming_self());
        if members.iter().filter(|row| row.is_self).count() != 1 {
            return Err("native context has no exact authenticated self member".into());
        }
        Ok(members)
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
        let roster = self.native_membership_roster(route, &current)?;
        Ok(Some(NativeContext {
            admitted: current,
            roster,
        }))
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
            let Ok(current) = self.revalidate_admission(&route, &admitted) else {
                self.drop_context_subscriber(&channel, peer_id, &admitted)?;
                continue;
            };
            self.tell_context_subscriber(&route, &channel, peer_id, &admitted, &current)?;
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
    /// Tell one native context subscriber what its workspace's governance now
    /// admits: the roster, and the name of every app installed here, when one
    /// was installed or removed since it was last told. Returns whether it
    /// heard something new.
    pub(super) fn tell_context_subscriber(
        &self,
        route: &CrdtRoute,
        channel: &str,
        peer_id: u64,
        admitted: &AdmittedJoinIdentity,
        current: &AdmittedJoinIdentity,
    ) -> Result<bool, String> {
        fn same_roster(left: &[collaboration::Member], right: &[collaboration::Member]) -> bool {
            left.len() == right.len()
                && left.iter().zip(right).all(|(left, right)| {
                    left.id == right.id
                        && left.name == right.name
                        && left.avatar == right.avatar
                        && left.packed_colour() == right.packed_colour()
                        && left.is_self == right.is_self
                        && left.removed == right.removed
                })
        }

        let roster = self.native_membership_roster(route, current)?;
        let apps = self.installed_apps();
        let mut channels = self
            .channels
            .lock()
            .map_err(|_| "native context subscriptions unavailable")?;
        let mut changed = false;
        if let Some(subscribers) = channels.get_mut(channel) {
            subscribers.context.retain_mut(|subscriber| {
                let Some(native) = subscriber.native.as_mut() else {
                    return true;
                };
                if subscriber.peer_id != peer_id || native.admitted != *admitted {
                    return true;
                }
                if same_roster(&native.roster, &roster) && subscriber.apps == apps {
                    return true;
                }
                changed = true;
                if subscriber
                    .tx
                    .send(ContextUpdate::Snapshot {
                        members: roster.clone(),
                        permissions: Vec::new(),
                        apps: apps.clone(),
                    })
                    .is_err()
                {
                    return false;
                }
                native.roster.clone_from(&roster);
                subscriber.apps.clone_from(&apps);
                true
            });
        }
        drop(channels);
        // A subscriber that could not be told went away: what the windows here
        // hold is the daemon's to publish again.
        if changed {
            self.windows_changed();
        }
        Ok(changed)
    }
    /// Re-check every membership surface and native context subscriber,
    /// and register with
    /// [`crate::apps::policy_schedule::announce_at`] the first instant at
    /// which a time-bound input they read (a capability's expiry, a
    /// subscriber certificate's validity) changes the answer. Returns whether
    /// the next re-check must come on the one-second clock instead of on an
    /// announcement: an input nothing announces, or a check that failed.
    pub(super) fn refresh_policy_surfaces(self: &Arc<Self>) -> bool {
        // Read only the workspaces' grants: decided again whatever else this host serves.
        self.refresh_shown_actions();
        // Only the opens waiting before this refresh reads the workspaces are
        // told its outcome; a later one waits for the next.
        let waiting = self.take_membership_surface_waiters();
        if self.services.workspace_memberships.is_none() || self.membership.is_none() {
            // No membership surface, and no native context subscriber can
            // be admitted: nothing here changes with policy.
            self.answer_membership_surface_waiters(waiting);
            return false;
        }
        // Sampled before anything is read: an expiry between this sample
        // and a read is registered as well, so none is skipped.
        let before = self.policy_now_ns();
        let surface_failed = self
            .refresh_workspace_membership_services()
            .inspect_err(|error| {
                tracing::warn!(%error, "native membership surface could not be refreshed");
            })
            .is_err();
        self.answer_membership_surface_waiters(waiting);
        let context_failed = self
            .refresh_workspace_context_services()
            .inspect_err(|error| {
                tracing::warn!(%error, "native membership context could not be refreshed");
            })
            .is_err();
        let clocked = surface_failed || context_failed;
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
            crate::apps::policy_schedule::announce_at(crate::apps::policy_schedule::wall_instant(
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
