//! This desktop's half of the peer network: the live session its device's daemon gives it, the
//! workspaces this session shows and their channels, and the roster the bar draws.
//!
//! The member itself is `tilo_session_client::Client`, which every host links. [`Client`] here
//! is that client plus this desktop's host. The session dials nothing and binds no endpoint: the
//! device's daemon holds the key and the one endpoint, and the session is its client.

use std::cell::RefCell;
use std::rc::Rc;
#[cfg(target_os = "linux")]
use std::sync::Arc;

#[cfg(target_os = "linux")]
use iroh::{EndpointAddr, EndpointId};
#[cfg(target_os = "linux")]
use tilo_desktop_model::Layout;
#[cfg(target_os = "linux")]
use tilo_identity::identity_store::DeviceIdentity;
#[cfg(target_os = "linux")]
use tilo_live::live::Track;
#[cfg(target_os = "linux")]
use tilo_live::live_session::Heard;
#[cfg(target_os = "linux")]
use tilo_network_actor::WorkspaceId;
#[cfg(target_os = "linux")]
use tilo_server_protocol::live::{LiveFrame, OpsFrame};
use tilo_session_client::client::Host;
#[cfg(target_os = "linux")]
use tilo_session_client::client::Shell as PolicyShell;
#[cfg(target_os = "linux")]
use tilo_session_client::message_policy::WorkspaceAction;
use tilo_session_client::{Client as SessionClient, Device, OwnPresence, Transport};
#[cfg(target_os = "linux")]
use tilo_session_client::{Cmd, Event, Outbox};
#[cfg(target_os = "linux")]
use tilo_session_core::desktop::here::{Here, LiveMember, Person};
#[cfg(target_os = "linux")]
use tilo_session_core::desktop::state::DesktopHandle;
#[cfg(target_os = "linux")]
/// Everything this device keeps of the peer network beside the client's boards: who is here as the bar
/// lists them, the workspaces' state and channels, the live session, the fabric and the transfers.
#[cfg(target_os = "linux")]
pub(crate) struct Local {
    /// The session this session shows, whose workspaces' policies every message from another member is
    /// held to.
    session: RefCell<Option<DesktopHandle>>,
    /// The call audio the workspaces' live sessions heard since the last call, each with the
    /// workspace it was heard in: what the calls playing it take ([`Self::take_call_audio`]).
    call_audio: RefCell<Vec<(String, Heard)>>,
    /// The call pictures the workspaces' live sessions heard since the last call, each with the
    /// workspace it was heard in: what the calls playing there draw
    /// ([`Self::take_call_pictures`]).
    call_pictures: RefCell<Vec<(String, Heard)>>,
    /// The shared windows' pictures and pages the workspaces' live sessions heard since the last
    /// call, each with the workspace it was heard in: what the sessions showing those windows draw
    /// ([`Self::take_window_objects`]).
    window_objects: RefCell<Vec<(String, Heard)>>,
    /// What shared windows' viewers and hosts said to this device on the workspaces' live sessions
    /// since the last call, each with the workspace it was heard in: what the shares take
    /// ([`Self::take_share_objects`]).
    share_objects: RefCell<Vec<(String, Heard)>>,
    /// This device's workspaces as its daemon says them: `None` until the session reaches the
    /// daemon ([`Client::attach_view`]), and on a session that keeps none.
    view: RefCell<Option<Arc<crate::workspace_view::WorkspaceView>>>,
    /// The devices each workspace's topic carries to this session: those heard on it, and its
    /// hosts, which the members they reach are behind.
    topologies: RefCell<std::collections::BTreeMap<WorkspaceId, Topology>>,
    /// The record of the person's devices in their own workspace, as the session follows it at
    /// the device's daemon, and this device's certificate there.
    person: Option<(PersonDevicesRecord, tilo_user_identity::DeviceCert)>,
    /// The workspaces' live resources, told which devices this session reaches in each: a row
    /// live on a device that went away goes with it.
    live_rows: tilo_workspace_service::live::LiveRows,
    /// The workspaces' live session: their relays and this device's sessions in them, shared with
    /// the session that runs them through the daemon.
    live: Rc<RefCell<Option<crate::live::Live>>>,
    /// Live sessions a test joined this session to, by workspace, in place of the live session's, and the
    /// runtime they run on.
    #[cfg(test)]
    test_live_sessions: RefCell<std::collections::BTreeMap<String, crate::live::Sessions>>,
    #[cfg(test)]
    test_runtime: RefCell<Option<tokio::runtime::Handle>>,
    /// Everything this device said on the share tracks, in order: what a test counts.
    #[cfg(test)]
    test_said: RefCell<Vec<tilo_live::share::Said>>,
}

/// The devices a workspace's topic carries to this session, as what the workspace says here
/// reports them: those that published there, and the workspace's hosts, which the members they
/// reach are behind. A record's content is not routed here: the device's daemon keeps it and
/// asks the workspace's devices itself; these are the devices live rows follow.
#[derive(Debug, Default)]
struct Topology {
    peers: std::collections::BTreeSet<EndpointId>,
    hosts: std::collections::BTreeSet<EndpointId>,
}

impl Topology {
    /// `device` said something on the workspace's topic.
    fn heard(&mut self, device: EndpointId) -> bool {
        self.peers.insert(device)
    }

    /// `host` serves the workspace: what its topic publishes, and the members it reports reaching.
    fn host(&mut self, host: EndpointId) -> bool {
        self.hosts.insert(host)
    }

    /// `device` left the workspace's topic, or the host that reached it did.
    fn left(&mut self, device: EndpointId) -> bool {
        let peer = self.peers.remove(&device);
        let host = self.hosts.remove(&device);
        peer || host
    }

    /// The devices of the workspace this session reaches now: those heard on its topic and the
    /// hosts that serve it, never `own` , this device.
    fn devices(&self, own: EndpointId) -> std::collections::BTreeSet<EndpointId> {
        self.peers
            .iter()
            .chain(self.hosts.iter())
            .filter(|device| **device != own)
            .copied()
            .collect()
    }
}

#[cfg(target_os = "linux")]
impl std::fmt::Debug for Local {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Local").finish_non_exhaustive()
    }
}

#[cfg(target_os = "linux")]
impl Local {
    /// What this desktop's peer network does with one message of the client's own.
    fn heard(&self, event: &Event, client: &SessionClient) -> bool {
        // The devices a workspace's topic carries are the ones live rows follow; a record's
        // content is the device's daemon's, which asks them itself.
        self.follow_present_devices(event, client);
        false
    }

    /// Keep what reached here of the devices a workspace's topic carries: those that said
    /// anything on it, the hosts that serve it and the members they report, and those that left.
    /// What live rows follow of the workspace follows them; a record's content is the device's
    /// daemon's to route, and it hears the workspace's devices itself.
    fn follow_present_devices(&self, event: &Event, client: &SessionClient) {
        let (ws, changed) = match event {
            Event::Msg { ws, peer, .. } => (
                *ws,
                self.present_devices(*ws, |devices| devices.heard(*peer)),
            ),
            Event::HostReady { ws, peer, .. } => (
                *ws,
                self.present_devices(*ws, |devices| devices.host(*peer)),
            ),
            Event::HostMember {
                ws,
                host,
                device,
                active,
            } => {
                let (host, device, active) = (*host, *device, *active);
                (
                    *ws,
                    self.present_devices(*ws, |devices| {
                        let served = devices.host(host);
                        let member = if active {
                            devices.heard(device)
                        } else {
                            devices.left(device)
                        };
                        served || member
                    }),
                )
            }
            Event::PeerLeft { ws, peer } => (
                *ws,
                self.present_devices(*ws, |devices| devices.left(*peer)),
            ),
            _ => return,
        };
        if !changed {
            return;
        }
        let connected = {
            let workspace = client.workspace_id(ws);
            let topologies = self.topologies.borrow();
            match (workspace, topologies.get(&ws)) {
                (Some(id), Some(topology)) => Some((id, topology.devices(client.publisher()))),
                _ => None,
            }
        };
        // The devices the workspace reaches are the ones live rows follow: a row live on a device
        // that went away goes with it.
        if let Some((workspace, devices)) = connected {
            let devices: std::collections::BTreeSet<[u8; 32]> =
                devices.iter().map(|device| *device.as_bytes()).collect();
            self.live_rows.reach(&workspace, devices.iter().copied());
        }
    }

    /// Apply `change` to the devices this session reaches in the workspace on `ws`; whether that
    /// was news.
    fn present_devices(&self, ws: WorkspaceId, change: impl FnOnce(&mut Topology) -> bool) -> bool {
        let mut topologies = self.topologies.borrow_mut();
        let Some(topology) = topologies.get_mut(&ws) else {
            return false;
        };
        change(topology)
    }

    /// Take the call audio the workspaces' live sessions heard since the last call, each with the
    /// workspace it was heard in and already opened. A call plays the frames of the devices its
    /// channel says are in it.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_call_audio(&self) -> Vec<(String, Heard)> {
        std::mem::take(&mut *self.call_audio.borrow_mut())
    }

    /// Take the call pictures the workspaces' live sessions heard since the last call, each with the
    /// workspace it was heard in and already opened. A call draws the pictures of the devices its
    /// channel says are in it.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_call_pictures(&self) -> Vec<(String, Heard)> {
        std::mem::take(&mut *self.call_pictures.borrow_mut())
    }

    /// Take the shared windows' pictures and pages the workspaces' live sessions heard since the last
    /// call, each with the workspace it was heard in and already opened: what the session showing that
    /// window draws.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_window_objects(&self) -> Vec<(String, Heard)> {
        #[cfg(test)]
        self.hear_test_live_sessions();
        std::mem::take(&mut *self.window_objects.borrow_mut())
    }

    /// Take what shared windows' viewers and hosts said to this device on the workspaces' live
    /// sessions since the last call, each with the workspace it was heard in and already opened
    /// ([`tilo_live::share`]).
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_share_objects(&self) -> Vec<(String, Heard)> {
        #[cfg(test)]
        self.hear_test_live_sessions();
        std::mem::take(&mut *self.share_objects.borrow_mut())
    }

    /// Sort what a test's live sessions heard into what the shares take.
    #[cfg(all(test, target_os = "linux"))]
    fn hear_test_live_sessions(&self) {
        for (workspace, live_session) in self.test_live_sessions.borrow().iter() {
            while let Some(heard) = live_session.try_recv() {
                let kept = match heard.track {
                    Track::Page(_) | Track::Window(..) => &self.window_objects,
                    Track::Share => &self.share_objects,
                    _ => continue,
                };
                kept.borrow_mut().push((workspace.clone(), heard));
            }
        }
    }

    /// The workspaces' live sessions this session is in now, by workspace.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn live_sessions(
        &self,
    ) -> std::collections::BTreeMap<String, crate::live::Sessions> {
        #[cfg(test)]
        if !self.test_live_sessions.borrow().is_empty() {
            return self.test_live_sessions.borrow().clone();
        }
        self.live()
            .map(|live| {
                live.live_sessions()
                    .into_iter()
                    .map(|(workspace, live_session)| (workspace.to_string(), live_session))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Moves whenever a workspace's live session stands otherwise or opened again
    /// ([`crate::live::Live::states_revision`]): what [`Self::live_states`] and
    /// [`Self::live_sessions`] are read again on. It never moves for a session that runs no live session.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn live_revision(&self) -> u64 {
        #[cfg(test)]
        if !self.test_live_sessions.borrow().is_empty() {
            // A test's live sessions are joined before its session runs, and stay.
            return 1;
        }
        self.live().map_or(0, |live| live.states_revision())
    }

    /// The endpoints the device's relay turned away from each workspace's live session and has
    /// not let in since, by workspace ([`crate::live::Live::turned_away`]).
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn live_turned_away(&self) -> crate::live::TurnedAwayLists {
        self.live()
            .map(|live| live.turned_away())
            .unwrap_or_default()
    }

    /// How far each workspace's live session came on this device, by workspace
    /// ([`crate::live::LiveState`]): nothing for a session that runs no live session.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn live_states(&self) -> std::collections::BTreeMap<String, crate::live::LiveState> {
        #[cfg(test)]
        if !self.test_live_sessions.borrow().is_empty() {
            // A test's live sessions are joined on a relay of the test's own: open, as this device's.
            return self
                .test_live_sessions
                .borrow()
                .iter()
                .map(|(workspace, live_session)| {
                    let relay = crate::live::Relayer {
                        device: live_session.device(),
                        name: String::new(),
                        here: true,
                    };
                    let open = crate::live::LiveState::Open {
                        relay,
                        epoch: 0,
                        lent: false,
                    };
                    (workspace.clone(), open)
                })
                .collect();
        }
        self.live()
            .map(|live| {
                live.states()
                    .into_iter()
                    .map(|(workspace, state)| (workspace.to_string(), state))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Show `tracks` of `sender` on `live_session`, and no other of its tracks: the live session's readers start
    /// on the runtime the live sessions run on, not on the session's own thread.
    #[cfg(target_os = "linux")]
    pub(crate) fn show_tracks(
        &self,
        live_session: &dyn tilo_live::capture::Session,
        sender: [u8; 32],
        tracks: &[Track],
    ) {
        #[cfg(test)]
        let runtime = self.test_runtime.borrow().clone();
        #[cfg(not(test))]
        let runtime = None;
        let runtime = runtime.or_else(|| self.live().and_then(|live| live.runtime()));
        let _entered = runtime.as_ref().map(tokio::runtime::Handle::enter);
        live_session.show(sender, tracks);
    }

    /// Send one message of `surface`'s stream on the topic of the workspace it is shared in, to
    /// `viewers` alone: over the authenticated route to each of them, never fanned out to the
    /// workspace.
    #[cfg(target_os = "linux")]
    /// The person's own workspace's resources, read and written through this device's daemon,
    /// which keeps the workspace: the one place what is theirs alone is read and written.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn person_devices(
        &self,
    ) -> Option<(PersonDevicesRecord, tilo_user_identity::DeviceCert)> {
        self.person.clone()
    }

    /// The workspaces' live session: what this session says into and hears from each of them, and the
    /// relay each one is carried by; `None` when this session holds no workspaces.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn live(&self) -> Option<std::cell::Ref<'_, crate::live::Live>> {
        std::cell::Ref::filter_map(self.live.borrow(), Option::as_ref).ok()
    }

    /// This device's workspaces as its daemon says them, once the session reached it.
    #[must_use]
    pub(crate) fn view(&self) -> Option<Arc<crate::workspace_view::WorkspaceView>> {
        self.view.borrow().clone()
    }

    /// Take every object the workspaces' live sessions heard since the last call: a member's presence
    /// frame, or a call's voice or pictures. Whether any did.
    fn hear_live(&self, client: &SessionClient) -> bool {
        let own = client.publisher();
        let topics: Vec<(tilo_desktop_model::WorkspaceId, String)> = client
            .live_workspaces()
            .into_iter()
            .filter_map(|workspace| {
                let id = tilo_desktop_model::WorkspaceId::parse(&workspace)?;
                Some((id, workspace))
            })
            .collect();
        self.hear_live_in(client, own, topics)
    }

    /// [`Self::hear_live`] over `topics`, the workspaces whose live links this client reads, as
    /// this device `own`'s session.
    fn hear_live_in(
        &self,
        client: &SessionClient,
        own: EndpointId,
        topics: Vec<(tilo_desktop_model::WorkspaceId, String)>,
    ) -> bool {
        let held = self.live.borrow();
        let Some(live) = held.as_ref() else {
            return false;
        };
        let mut heard = false;
        for (ws, workspace) in topics {
            while let Some(object) = live.try_recv(ws) {
                if object.track == Track::Audio {
                    // A call's voice, already opened: the call of that workspace plays it.
                    self.call_audio
                        .borrow_mut()
                        .push((workspace.clone(), object));
                    continue;
                }
                if matches!(object.track, Track::Page(_) | Track::Window(..)) {
                    // A shared window's pictures or page, already opened: the session showing that
                    // window draws them.
                    self.window_objects
                        .borrow_mut()
                        .push((workspace.clone(), object));
                    continue;
                }
                if object.track == Track::Share {
                    // What a shared window's viewer or host said to this device, already opened:
                    // the shares take it.
                    self.share_objects
                        .borrow_mut()
                        .push((workspace.clone(), object));
                    continue;
                }
                if matches!(object.track, Track::Camera(_) | Track::Screen(_)) {
                    // A call's pictures, already opened: the call of that workspace draws them.
                    self.call_pictures
                        .borrow_mut()
                        .push((workspace.clone(), object));
                    continue;
                }
                if object.track != Track::Presence {
                    continue;
                }
                let Ok(from) = EndpointId::from_bytes(&object.sender) else {
                    continue;
                };
                if from == own {
                    continue;
                }
                client.heard_body(&workspace, from, &object.body);
                heard = true;
            }
        }
        heard
    }
}

/// The person's presence as this desktop's surfaces read it: who is here, their pointer, and the
/// bar's word of a wave or an arrival.
pub(crate) struct Roster {
    /// The roster the bar lists, and the pointer over the tile it is on.
    here: Here,
    /// The last wave at this person, for the bar to say.
    wave_notice: RefCell<Option<tilo_session_core::desktop::host::chrome::WaveNotice>>,
    /// The workspace whose arrival the bar is saying, so a person who comes to a workspace where
    /// people are is shown arriving once, not on every roster change.
    arrived_in: RefCell<Option<String>>,
    /// The arrival the bar shows.
    arrival: RefCell<Option<tilo_session_core::desktop::host::chrome::ArrivalNotice>>,
    /// The semantic work the workspace's agents narrated since its surface last took it, by
    /// workspace, admitted and bound to the member that published it
    /// ([`tilo_session_client::narration::Held`]).
    #[cfg(target_os = "linux")]
    narrated: RefCell<tilo_session_client::narration::Held>,
    /// The same work, by workspace, for the apps here: each window showing the record an agent
    /// acts in draws its cursor there ([`Client::take_narrated`]).
    #[cfg(target_os = "linux")]
    for_apps: RefCell<Vec<(String, tilo_actions::SemanticInteraction)>>,
}

/// What this session keeps of the peer network beside the client's boards: the roster every reader
/// draws, and, on a desktop, what the live link and the workspaces' state tell it.
pub(crate) struct Hosting {
    roster: Rc<Roster>,
    #[cfg(target_os = "linux")]
    local: Rc<Local>,
}

impl Roster {
    /// A roster nobody is on yet.
    fn new() -> Self {
        Self {
            here: Here::default(),
            wave_notice: RefCell::new(None),
            arrived_in: RefCell::new(None),
            arrival: RefCell::new(None),
            #[cfg(target_os = "linux")]
            narrated: RefCell::new(tilo_session_client::narration::Held::default()),
            #[cfg(target_os = "linux")]
            for_apps: RefCell::new(Vec::new()),
        }
    }

    #[cfg(target_os = "linux")]
    /// Who is here, as the desktop lists them: `tree` places them where their focused record is
    /// shown — `records` (what each window shows, by tab) names the window each person works in —
    /// and `app_name` names an app.
    #[must_use]
    pub(crate) fn people(
        &self,
        tree: &Layout,
        notes: &crate::continuations::Continuations,
        app_name: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<Person> {
        self.here.borrow().people(
            tree,
            &crate::sync::layout::bindings_of(tree, &notes.windows()),
            app_name,
        )
    }

    #[cfg(target_os = "linux")]
    /// Who is here, written into `tree`'s active workspace as its members and their presence: the roster every
    /// surface that lists people reads.
    pub(crate) fn show(
        &self,
        tree: &mut Layout,
        notes: &crate::continuations::Continuations,
        app_name: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<Person> {
        let people = self.people(tree, notes, app_name);
        // Whose view this person follows, so the bar keeps that face when it has room for only one.
        let following = self.here.borrow().following().map(str::to_owned);
        tilo_session_core::desktop::here::place_here(tree, &people, following.as_deref());
        // Who follows this person: their faces in the bar say so.
        let followers = self.here.borrow().followers(&people);
        tilo_session_core::desktop::here::mark_followers(tree, &followers);
        people
    }

    /// The last wave at this person, for the bar to say.
    #[must_use]
    pub(crate) fn wave_notice(
        &self,
    ) -> Option<tilo_session_core::desktop::host::chrome::WaveNotice> {
        self.wave_notice.borrow().clone()
    }

    /// The arrival the bar shows, while its moment lasts.
    #[must_use]
    pub(crate) fn arrival_notice(
        &self,
    ) -> Option<tilo_session_core::desktop::host::chrome::ArrivalNotice> {
        self.arrival.borrow().clone()
    }

    /// Hand the desktop this person and whoever is here with them in the workspace in front.
    fn show_here(&self, client: &SessionClient) {
        let own = Self::own_member(client);
        let (workspace, members) = client
            .front_members()
            .map(|(workspace, members)| {
                (
                    workspace,
                    members
                        .iter()
                        .map(LiveMember::of)
                        .collect::<Vec<LiveMember>>(),
                )
            })
            .unwrap_or_default();
        self.here
            .borrow_mut()
            .set_members(Some(own), Some(members.clone()));
        self.show_arrival(&workspace, &members);
    }

    /// The person came to a workspace where people already are: the bar's faces pulse and a short
    /// name slides out of them, once for the workspace they came to. A workspace nobody else is in
    /// says nothing, and someone nobody has named is said as every surface says them ("Someone"),
    /// never left out of the news of who came.
    fn show_arrival(&self, workspace: &str, members: &[LiveMember]) {
        let people: Vec<(&LiveMember, String)> = members
            .iter()
            .filter(|member| member.kind != "agent")
            .map(|member| (member, tilo_ui_base::presence::person_label(&member.label)))
            .collect();
        if people.is_empty() {
            return;
        }
        if self.arrived_in.borrow().as_deref() != Some(workspace) {
            tracing::debug!(
                %workspace,
                names = ?people.iter().map(|(_, name)| name.as_str()).collect::<Vec<_>>(),
                "the bar says who is here"
            );
            *self.arrived_in.borrow_mut() = Some(workspace.to_owned());
            self.say_arrival(&people);
            return;
        }
        // The arrival was said already. A person whose name reaches this session after they came
        // arrived unnamed a moment before ("Someone is here"); their name is what everyone calls
        // them, so the bar says who arrived for the same moment again rather than leave the
        // person unnamed on it. A name that changes after it was known is the person's own and
        // is not announced.
        let Some(notice) = self.arrival.borrow().clone() else {
            return;
        };
        let names: Vec<String> = notice
            .people
            .iter()
            .zip(&notice.names)
            .map(|(publisher, was)| {
                people
                    .iter()
                    .find(|(member, _)| &member.publisher == publisher)
                    .map_or_else(|| was.clone(), |(_, name)| name.clone())
            })
            .collect();
        let unnamed = tilo_ui_base::presence::is_unnamed_person;
        let learned = notice
            .names
            .iter()
            .zip(&names)
            .any(|(was, now)| unnamed(was) && !unnamed(now));
        if !learned {
            return;
        }
        tracing::debug!(was = ?notice.names, now = ?names, "the arrival learns its name");
        *self.arrival.borrow_mut() =
            Some(tilo_session_core::desktop::host::chrome::ArrivalNotice {
                people: notice.people,
                names,
                seen: notice.seen,
                at: ui::time::now(),
            });
    }

    /// Say who arrived in the workspace in front, `people`, from now: the bar's moment for it.
    fn say_arrival(&self, people: &[(&LiveMember, String)]) {
        *self.arrival.borrow_mut() =
            Some(tilo_session_core::desktop::host::chrome::ArrivalNotice {
                people: people
                    .iter()
                    .map(|(member, _)| member.publisher.clone())
                    .collect(),
                names: people.iter().map(|(_, name)| name.clone()).collect(),
                seen: None,
                at: ui::time::now(),
            });
    }

    /// This session's own person, as everyone here reads them.
    fn own_member(client: &SessionClient) -> LiveMember {
        LiveMember::of(&client.own_here())
    }

    #[cfg(target_os = "linux")]
    /// Take one semantic frame the peer network heard: an agent's narrated action in a workspace's record.
    /// The work is admitted only from a member the workspace's board knows as an agent
    /// ([`tilo_session_client::narration::Held`]) and bound to that member. Each app here keeps its
    /// own presence, so the work is kept for the apps too ([`Client::take_narrated`]), which draw
    /// the agent's cursor in the window showing its record. Whether `event` was one.
    fn hear_narrated(&self, event: &Event, client: &SessionClient) -> bool {
        let Event::Live { ws, from, frame } = event else {
            return false;
        };
        let Ok(LiveFrame::Ops(OpsFrame::Semantic { event, .. })) = LiveFrame::decode(frame) else {
            return false;
        };
        let Some(workspace) = client.workspace_id(*ws) else {
            return false;
        };
        let here = client.members(&workspace);
        let mut narrated = self.narrated.borrow_mut();
        let admitted = narrated.hear(&workspace, &here, *from, &event);
        let work = narrated.take(&workspace);
        drop(narrated);
        if admitted {
            self.for_apps
                .borrow_mut()
                .extend(work.into_iter().map(|work| (workspace.clone(), work)));
        }
        true
    }

    /// Take the waves at this person heard since the last call: the bar says who waved, for a
    /// moment. A shell that draws its own faces reads them by this too.
    pub(crate) fn show_waves(&self) {
        let heard = self.here.borrow_mut().take_waves();
        if let Some(last) = heard.last() {
            *self.wave_notice.borrow_mut() =
                Some(tilo_session_core::desktop::host::chrome::WaveNotice {
                    from: last.name.clone(),
                    at: ui::time::now(),
                });
        }
    }
}

#[cfg(target_os = "linux")]
impl Host for Hosting {
    fn admitted_person(&self, workspace: &str, device: EndpointId) -> Option<String> {
        // The call gate names whose each of the workspace's member devices is.
        let (_, persons) = self.local.view()?.call_gate(workspace);
        persons.get(device.as_bytes()).cloned()
    }

    fn heard(&self, event: &Event, client: &SessionClient) -> bool {
        // An agent's narrated action is the roster's to take: it moves nothing inside a window here,
        // and says where the person's own agent works.
        self.roster.hear_narrated(event, client) || self.local.heard(event, client)
    }

    fn touched(&self, client: &SessionClient) {
        self.roster.show_here(client);
        self.roster.show_waves();
    }

    fn workspace_host(&self, workspace: &str) -> Option<[u8; 32]> {
        self.local
            .view()?
            .holder_of(tilo_desktop_model::WorkspaceId::parse(workspace)?)
    }

    fn pass(&self, client: &SessionClient) {
        self.local.hear_live(client);
    }
}

/// The session a session shows, as the workspace check reads it.
/// A page's host: the roster, over the transport the page brings.
#[cfg(not(target_os = "linux"))]
impl Host for Hosting {
    fn touched(&self, client: &SessionClient) {
        self.roster.show_here(client);
        self.roster.show_waves();
    }
}

#[cfg(target_os = "linux")]
struct MessagePolicy {
    session: DesktopHandle,
}

#[cfg(target_os = "linux")]
impl PolicyShell for MessagePolicy {
    fn allows(&self, workspace: &str, person: &str, required: &WorkspaceAction, now: u64) -> bool {
        self.session.read(|tree| {
            tilo_session_client::message_policy::allows(tree, workspace, person, required, now)
        })
    }
}

/// The session's client of the peer network: the member's own client, and the desktop host that
/// carries it — the live session its device's daemon gives it, and the workspaces and channels the
/// session shows.
pub struct Client {
    inner: SessionClient,
    hosting: Rc<Hosting>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.inner, f)
    }
}

impl std::ops::Deref for Client {
    type Target = SessionClient;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        self.hosting.local.live.borrow_mut().take();
    }
}

/// The record of the person's devices, as the session follows it.
#[cfg(target_os = "linux")]
type PersonDevicesRecord = crate::followed_resource::SharedFollow<
    tilo_workspace_service::workspaces::person_workspace::devices::Devices,
>;

impl Client {
    /// A client on a transport another host brings, with this desktop's host beside it: a session
    /// whose device daemon holds the endpoint takes part this way, its daemon's live sessions
    /// (`live`) carrying who is here, and a test hands it what it wants the session to show. The
    /// transport's own commands stay the client's, so a host that reads them off its end reads
    /// what this session said.
    #[must_use]
    pub fn over(
        transport: Transport,
        device: Device,
        own: OwnPresence,
        live: Rc<RefCell<Option<crate::live::Live>>>,
    ) -> Self {
        #[cfg(target_os = "linux")]
        let host = Rc::new(Local::bare(live));
        let inner = SessionClient::over(transport, device, own);
        let hosting = Rc::new(Hosting {
            roster: Rc::new(Roster::new()),
            #[cfg(target_os = "linux")]
            local: host,
        });
        inner.attach_host(Rc::clone(&hosting) as Rc<dyn Host>);
        Self { inner, hosting }
    }

    /// A session taking part in a peer network with no transport, for tests that hand it what it would
    /// have heard and read what it would have said.
    #[cfg(test)]
    #[cfg(target_os = "linux")]
    pub(crate) fn offline(
        publisher: EndpointId,
        own: OwnPresence,
    ) -> (Self, tokio::sync::mpsc::UnboundedReceiver<Cmd>) {
        Self::without_network(publisher, own)
    }

    /// A client taking part in no transport: it publishes nowhere and hears only what it is handed.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub fn untethered(publisher: EndpointId, own: OwnPresence) -> Self {
        Self::without_network(publisher, own).0
    }

    /// A client of a peer network with no transport: what both the real client and [`Self::untethered`]
    /// are, at their middle. A test that reads what one peer network would have sent to another takes the
    /// receiver.
    #[cfg(target_os = "linux")]
    pub(crate) fn without_network(
        publisher: EndpointId,
        own: OwnPresence,
    ) -> (Self, tokio::sync::mpsc::UnboundedReceiver<Cmd>) {
        let (outgoing, said) = tokio::sync::mpsc::unbounded_channel();
        let (_unheard, arrived) = std::sync::mpsc::channel();
        let outbox: Arc<dyn Outbox> = Arc::new(outgoing);
        let inner = SessionClient::over(
            Transport {
                outgoing: Arc::clone(&outbox),
                arrived,
                saying: None,
            },
            Device {
                publisher,
                endpoint: EndpointAddr::new(publisher),
                person: None,
                name: String::new(),
            },
            own,
        );
        let hosting = Rc::new(Hosting {
            roster: Rc::new(Roster::new()),
            local: Rc::new(Local::bare(Rc::default())),
        });
        inner.attach_host(Rc::clone(&hosting) as Rc<dyn Host>);
        (Self { inner, hosting }, said)
    }

    /// The same peer network, taken part in as `person` (their user root, hex): what a device's
    /// certificate says of it on a peer network that has one, and what the workspaces' policies hold its
    /// members' messages to.
    #[cfg(test)]
    #[cfg(target_os = "linux")]
    pub(crate) fn taking_part_as(mut self, person: &str) -> Self {
        self.inner.as_person(person);
        self
    }

    /// Take the work the workspaces' agents narrated since the last call, by workspace, admitted
    /// and bound to the agent that did it: what this device's apps draw as that agent's cursor.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_narrated(&self) -> Vec<(String, tilo_actions::SemanticInteraction)> {
        std::mem::take(&mut *self.hosting.roster.for_apps.borrow_mut())
    }

    /// Take the call audio the workspaces' live sessions heard since the last call, already opened.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_call_audio(&self) -> Vec<(String, Heard)> {
        self.hosting.local.take_call_audio()
    }

    /// Take the call pictures the workspaces' live sessions heard since the last call, already
    /// opened.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_call_pictures(&self) -> Vec<(String, Heard)> {
        self.hosting.local.take_call_pictures()
    }

    /// Take the shared windows' pictures and pages the workspaces' live sessions heard since the last
    /// call, already opened.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_window_objects(&self) -> Vec<(String, Heard)> {
        self.hosting.local.take_window_objects()
    }

    /// Take what shared windows' viewers and hosts said to this device since the last call,
    /// already opened.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn take_share_objects(&self) -> Vec<(String, Heard)> {
        self.hosting.local.take_share_objects()
    }

    /// The workspaces' live sessions this session is in now, by workspace: what a call's voice and
    /// pictures and a shared window's pictures and pages travel.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn live_sessions(
        &self,
    ) -> std::collections::BTreeMap<String, crate::live::Sessions> {
        self.hosting.local.live_sessions()
    }

    /// Moves whenever a workspace's live session stands otherwise or opened again: what
    /// [`Self::live_states`] and [`Self::live_sessions`] are read again on.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn live_revision(&self) -> u64 {
        self.hosting.local.live_revision()
    }

    /// How far each workspace's live session came on this device, by workspace: open, or what it
    /// waits for ([`crate::live::LiveState`]).
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn live_states(&self) -> std::collections::BTreeMap<String, crate::live::LiveState> {
        self.hosting.local.live_states()
    }

    /// The endpoints the device's relay turned away from each workspace's live session and has
    /// not let in since, by workspace ([`crate::live::Live::turned_away`]).
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn live_turned_away(&self) -> crate::live::TurnedAwayLists {
        self.hosting.local.live_turned_away()
    }

    /// Join this session to `live_sessions`, by workspace, running on `runtime`, in place of a live session
    /// of its own: what a test's sessions meet on.
    #[cfg(test)]
    #[cfg(target_os = "linux")]
    pub(crate) fn on_live_sessions(
        self,
        live_sessions: std::collections::BTreeMap<String, crate::live::Sessions>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        *self.hosting.local.test_live_sessions.borrow_mut() = live_sessions;
        *self.hosting.local.test_runtime.borrow_mut() = Some(runtime);
        self
    }

    /// Show `tracks` of `sender` on `live_session`, and no other of its tracks ([`Local::show_tracks`]).
    #[cfg(target_os = "linux")]
    pub(crate) fn show_tracks(
        &self,
        live_session: &dyn tilo_live::capture::Session,
        sender: [u8; 32],
        tracks: &[Track],
    ) {
        self.hosting.local.show_tracks(live_session, sender, tracks);
    }

    /// Say `said` on `workspace`'s live session's share track: what a shared window's viewer says to
    /// its host and a host to its viewers, each object to the device it names alone
    /// ([`tilo_live::share`]). Nothing is said while the workspace's live session is not open: a
    /// viewer says its view again, and input lands within its frame or not at all.
    #[cfg(target_os = "linux")]
    pub(crate) fn say(&self, workspace: &str, said: &[tilo_live::share::Said]) {
        if said.is_empty() {
            return;
        }
        #[cfg(test)]
        self.hosting
            .local
            .test_said
            .borrow_mut()
            .extend_from_slice(said);
        let Some(live_session) = self.live_sessions().remove(workspace) else {
            tracing::debug!(%workspace, "a shared window's word waits for no live session");
            return;
        };
        if let Err(error) = live_session.say(said) {
            // The share it belongs to ended with the session: nothing of it is left to say it to.
            tilo_failure::ignored!(
                "the share the word belongs to has ended",
                tracing::debug!(%workspace, %error, "a shared window's word was not said")
            );
        }
    }

    /// Everything this device said on the share tracks so far, in order.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn said_on_shares(&self) -> Vec<tilo_live::share::Said> {
        self.hosting.local.test_said.borrow().clone()
    }

    /// The workspaces' live resources, as far as the devices this session reaches go: a row live
    /// on a device that went away goes with it.
    #[cfg(target_os = "linux")]
    pub(crate) fn live_rows(&self) -> tilo_workspace_service::live::LiveRows {
        self.hosting.local.live_rows.clone()
    }

    /// This device came to hold another workspace: its live session opens that workspace's live session
    /// now ([`crate::live::Live::workspace_arrived`]). Nothing to tell on a session with no live session.
    #[cfg(target_os = "linux")]
    pub(crate) fn workspace_arrived(&self) {
        if let Some(live) = self.hosting.local.live() {
            live.workspace_arrived();
        }
    }

    /// Take what the workspaces' live sessions heard since the last call, on a session whose peer
    /// network is its daemon's: the live links the daemon gave it stand for the workspaces, and
    /// what they carry is taken as the client's own board reads it. Nothing on a client whose own
    /// thread already hears them. Whether any did.
    #[cfg(target_os = "linux")]
    pub(crate) fn follow_live(&self) -> bool {
        let topics: Vec<(tilo_desktop_model::WorkspaceId, String)> = {
            let held = self.hosting.local.live.borrow();
            let Some(live) = held.as_ref() else {
                return false;
            };
            live.live_sessions()
                .into_keys()
                .map(|workspace| (workspace, workspace.to_string()))
                .collect()
        };
        self.hosting
            .local
            .hear_live_in(&self.inner, self.inner.publisher(), topics)
    }

    /// The workspaces the device's daemon keeps, as this client reads them: what the live rows
    /// follow and what tells whether a workspace here has someone else in it.
    #[cfg(target_os = "linux")]
    pub(crate) fn attach_view(&self, view: Option<Arc<crate::workspace_view::WorkspaceView>>) {
        self.hosting.local.view.replace(view);
    }

    /// The person's own workspace's resources, which this session reads and writes what is theirs
    /// alone through once its service holds the workspace.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn person_devices(
        &self,
    ) -> Option<(PersonDevicesRecord, tilo_user_identity::DeviceCert)> {
        self.hosting.local.person_devices()
    }

    /// The way to the person's other devices over the live link, and from them to this one;
    /// `None` when this session holds no workspaces.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn own_devices(&self) -> Option<crate::live::OwnDevices> {
        self.hosting.local.live().map(|live| live.own_devices())
    }

    /// The person's presence as the surfaces here read it.
    pub fn here(&self) -> &Here {
        &self.hosting.roster.here
    }

    /// Hand the workspace check the session this session shows: whose workspaces' policies hold every
    /// member's messages to what the workspace grants them.
    #[cfg(target_os = "linux")]
    pub fn attach_shell(&self, session: DesktopHandle) {
        self.inner.attach_shell(Rc::new(MessagePolicy {
            session: session.clone(),
        }));
        *self.hosting.local.session.borrow_mut() = Some(session);
    }

    /// This device's workspaces as its daemon says them, once the session reached it.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn view(&self) -> Option<Arc<crate::workspace_view::WorkspaceView>> {
        self.hosting.local.view()
    }

    /// This device's handle in a workspace, as a window's stream names it.
    #[must_use]
    #[cfg(target_os = "linux")]
    pub(crate) fn peer(&self) -> neighbour_wire::PeerId {
        tilo_network_actor::behaviour::peer_liveness_handle(&self.publisher())
    }

    #[cfg(target_os = "linux")]
    /// Who is here, as the desktop lists them.
    #[must_use]
    pub(crate) fn people(
        &self,
        tree: &Layout,
        notes: &crate::continuations::Continuations,
        app_name: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<Person> {
        self.hosting.roster.people(tree, notes, app_name)
    }

    #[cfg(target_os = "linux")]
    /// Who is here, written into `tree`'s active workspace as its members.
    pub(crate) fn show(
        &self,
        tree: &mut Layout,
        notes: &crate::continuations::Continuations,
        app_name: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<Person> {
        self.hosting.roster.show(tree, notes, app_name)
    }

    /// Take the waves heard since the last look, so a shell that draws its own faces says them.
    pub(crate) fn show_waves(&self) {
        self.hosting.roster.show_waves();
    }

    /// The waves at this person since the last call, as the bar says them.
    #[must_use]
    pub fn wave_notice(&self) -> Option<tilo_session_core::desktop::host::chrome::WaveNotice> {
        self.hosting.roster.wave_notice()
    }

    /// The arrival the bar shows, while its moment lasts.
    #[must_use]
    pub fn arrival_notice(
        &self,
    ) -> Option<tilo_session_core::desktop::host::chrome::ArrivalNotice> {
        self.hosting.roster.arrival_notice()
    }
}

#[cfg(target_os = "linux")]
impl Local {
    /// A local with no peer network of its own: what a client takes part in over a transport another host
    /// brings.
    fn bare(live: Rc<std::cell::RefCell<Option<crate::live::Live>>>) -> Self {
        Self {
            session: RefCell::new(None),
            call_audio: RefCell::new(Vec::new()),
            call_pictures: RefCell::new(Vec::new()),
            window_objects: RefCell::new(Vec::new()),
            share_objects: RefCell::new(Vec::new()),
            view: RefCell::new(None),
            topologies: RefCell::new(std::collections::BTreeMap::new()),
            person: None,
            live_rows: tilo_workspace_service::live::LiveRows::default(),
            live,
            #[cfg(test)]
            test_live_sessions: RefCell::default(),
            #[cfg(test)]
            test_said: RefCell::default(),
            #[cfg(test)]
            test_runtime: RefCell::default(),
        }
    }
}

/// This device's collaboration identity: the key and certificate it signs and presents with, when
/// this host holds one. A session on a host without one takes part in no workspace as anybody.
#[must_use]
#[cfg(target_os = "linux")]
pub fn device_identity() -> Option<DeviceIdentity> {
    tilo_session_client::identity::device_identity()
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod membership_check_tests;

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests;
