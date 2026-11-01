//! Voice calls on the collaboration plane: the channel this device's call is
//! in, and who is in the channels every room this session takes part in
//! shows.
//!
//! Being in a call is the presence every member of the room reads: joining
//! publishes the channel, leaving stops saying it, and a channel's roster is
//! what the members in it say of themselves. The session publishes this from
//! its pass ([`Calls::follow`]), and the calls themselves through the service
//! an app reaches ([`tilo_native_media`]), whose carrier is this plane
//! ([`Factory`]).
//!
//! A channel id is the whole of a call's identity on the wire: the presence
//! names the channel and what it is called, and a roster says who is in a
//! channel, with no room beside it.
//!
//! The call carries no audio or pictures yet: what a call sends is the
//! carrier's own (`tilo_native_media::MediaSession`), and this one has no
//! media lane to send it on.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use runtime_collab::native_media::{Issue, Roster, State};
use tilo_lock::MutexRecover as _;
use tilo_native_media::{CallTarget, MediaFactory, MediaSession, Prepare, Rosters};

use crate::sync::{self, LiveCall};

/// The calls of every room this session takes part in.
pub(crate) struct Calls {
    /// The calls here, as the people in them say.
    live: Mutex<Vec<LiveCall>>,
    /// Who is in each channel, as the members say: what a voice channel
    /// shows.
    rosters: Mutex<Vec<Roster>>,
    /// What the rosters last said, to wake the apps watching them only when
    /// they moved.
    said: Mutex<String>,
    /// Woken whenever a room's calls may have changed.
    changed: Arc<tokio::sync::Notify>,
}

impl Calls {
    #[must_use]
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            live: Mutex::new(Vec::new()),
            rosters: Mutex::new(Vec::new()),
            said: Mutex::new(String::new()),
            changed: Arc::new(tokio::sync::Notify::new()),
        })
    }

    /// Take in the calls everyone here says they are in, and the channel
    /// this device joined (if any), so a voice channel shows a join or a
    /// leave on this pass.
    pub(crate) fn follow(&self, sync: &sync::Client, own: Option<&str>) {
        let live = sync.calls();
        let mut people: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for call in &live {
            let in_call = people.entry(call.channel.clone()).or_default();
            for caller in &call.people {
                if let Some(person) = &caller.user {
                    in_call.insert(person.clone());
                }
            }
        }
        // This device's own call: nobody here hears it from this session, so
        // it is said from what the session itself published.
        if let (Some(channel), Some(person)) = (own, sync.own().user) {
            people.entry(channel.to_owned()).or_default().insert(person);
        }
        let rosters: Vec<Roster> = people
            .into_iter()
            .map(|(channel_id, people)| Roster {
                channel_id,
                people: people.into_iter().collect(),
                ..Roster::default()
            })
            .collect();
        let said = format!("{rosters:?}");
        let moved = {
            let mut last = self.said.lock_recover();
            let moved = *last != said;
            last.clone_from(&said);
            moved
        };
        *self.rosters.lock_recover() = rosters;
        *self.live.lock_recover() = live;
        if moved {
            self.changed.notify_waiters();
        }
    }

    /// What `channel` is called, as the people in it say; empty when nobody
    /// named it.
    pub(crate) fn name(&self, channel: &str) -> String {
        self.live
            .lock_recover()
            .iter()
            .find(|call| call.channel == channel)
            .map(|call| call.name.clone())
            .unwrap_or_default()
    }

    /// Who is in each channel, sorted by channel.
    pub(crate) fn rosters(&self) -> Vec<Roster> {
        self.rosters.lock_recover().clone()
    }
}

/// What voice channel `channel` is called, as the app that holds it
/// published the row its call is joined from: its label, empty when the app
/// publishes no such row.
pub(crate) fn channel_name(
    surfaces: &[tilo_action_broker::collab_app_broker::PublishedSurface],
    (workspace, app, record): (&str, &str, &str),
    channel: &str,
) -> Option<String> {
    let row = format!("{record}/channels/{channel}/call");
    surfaces
        .iter()
        .filter(|surface| surface.app_id == app && surface.route.cluster_id == workspace)
        .flat_map(|surface| &surface.actions)
        .find(|action| action.resource == row)
        .map(|action| action.resource_label.trim().to_owned())
        .filter(|name| !name.is_empty())
}

/// This plane as the carrier of a call: being in the channel is joining it,
/// which the session's presence then says to everyone in the room.
pub(crate) struct Factory(Arc<Calls>);

impl Factory {
    #[must_use]
    pub(crate) fn new(calls: Arc<Calls>) -> Self {
        Self(calls)
    }
}

impl MediaFactory for Factory {
    fn prepare(&self, _target: CallTarget) -> Prepare {
        Box::pin(async move { Ok(Box::new(Call) as Box<dyn MediaSession>) })
    }

    fn rosters(&self, _target: CallTarget) -> Rosters {
        let rosters = self.0.rosters();
        Box::pin(async move { rosters })
    }

    fn rosters_changed(&self) -> Option<Arc<tokio::sync::Notify>> {
        Some(Arc::clone(&self.0.changed))
    }
}

/// One call this device is in: the presence carries it for as long as the
/// app that joined keeps its connection, which is what ends it.
pub(crate) struct Call;

impl MediaSession for Call {
    fn tick(&mut self, _now: Instant) -> Result<State, Issue> {
        Ok(State::Live)
    }
}

#[cfg(test)]
mod tests {
    //! A call on the native desk: an app joins a voice channel through the
    //! service the session hands its apps, the session says the channel in
    //! this person's presence, and the people in the room read this person in
    //! that call until the app leaves.

    use std::rc::Rc;
    use std::sync::Arc;
    use std::time::Duration;

    use collab_types::PeerPresence;
    use collab_types::presence::{Availability, PresenceKey};
    use iroh::EndpointId;
    use runtime_collab::native_media::{Command, Request, State};
    use tilo_compositor::OutputConfig;
    use tilo_desktop_model::{DesktopTree, RoomId, Workspace, WorkspaceId};
    use tilo_network_actor::{NetEvent, room_topic_id};

    use crate::sync::{Client, OwnPresence};
    use crate::{ActionPlane, Options, Session};

    const ROOM: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";
    const CHANNEL: &str = "c_standup";
    const CHAT: &str = "so.tilo.chat";
    /// The app id the desktop of a room says its own presence under.
    const DESKTOP_PRESENCE_APP: &str = "tilo-shell";

    fn device(seed: u8) -> EndpointId {
        iroh::SecretKey::from_bytes(&[seed; 32]).public()
    }

    /// The person a device is a device of, in the hex everyone here knows
    /// them by.
    fn person(seed: u8) -> String {
        hex::encode([seed; 32])
    }

    fn room() -> RoomId {
        RoomId::parse(ROOM).expect("a room id")
    }

    /// A desk showing one workspace, in the room.
    fn desk() -> DesktopTree {
        let room = room();
        let workspace = Workspace::in_room(WorkspaceId::of_room(room), room, "Desk");
        DesktopTree {
            workspaces: vec![workspace.clone()],
            active: workspace.id,
            ..DesktopTree::default()
        }
    }

    /// A directory for the plane's socket that only this user reaches: the
    /// broker refuses to serve one anybody else could open.
    fn private_home() -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().expect("a directory to serve the plane in");
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
            .expect("the plane's directory is this user's");
        root
    }

    /// A session taking part in the room through `sync`, serving its apps on
    /// `socket`, showing the desk.
    fn session(sync: &Rc<Client>, socket: &std::path::Path) -> Session {
        let session = Session::headless(
            Options {
                presentation: ui::headless::fixture_runner_inputs,
                sync: Some(Rc::clone(sync)),
                action_plane: ActionPlane::At(socket.to_path_buf()),
                ..Options::default()
            },
            &[OutputConfig::new("HEADLESS-1", 1280, 800)],
        )
        .expect("session");
        session.handle().replace_tree(desk());
        session
    }

    fn step(session: &mut Session) {
        for _ in 0..4 {
            session.step(Duration::from_millis(1)).expect("step");
        }
    }

    /// One thing `from` said in the room.
    fn said(from: u8, msg: neighbour_wire::NeighbourMsg) -> NetEvent {
        NetEvent::Msg {
            ws: room_topic_id(room()),
            peer: device(from),
            membership_epoch: None,
            replayed: false,
            msg,
        }
    }

    /// A device's identity: the person it is a device of, and its name here.
    fn hello(from: u8, label: &str) -> NetEvent {
        let device_key = device(from);
        let cert = tilo_user_identity::DeviceCert {
            user_public_key: [from; 32],
            device_public_key: *device_key.as_bytes(),
            device_label: label.to_owned(),
            capabilities: tilo_user_identity::DeviceCapabilities {
                can_own_workspaces: false,
                can_admit_devices: false,
                can_act_as_host: false,
                can_read_all_workspaces: false,
                foreign: false,
                can_administer_devices: false,
            },
            issued_at_ns: 0,
            expires_at_ns: None,
            signature: [0; 64],
        };
        said(
            from,
            neighbour_wire::NeighbourMsg::IdentityHello {
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                cert: Box::new(cert),
                revocations: Vec::new(),
            },
        )
    }

    /// One presence `from` said of themselves in the room.
    fn presence(from: u8, payload: collab_types::PeerPresencePayload) -> NetEvent {
        let device_key = device(from);
        said(
            from,
            neighbour_wire::NeighbourMsg::AppPresenceV2 {
                peer: neighbour_wire::peer_handle(device_key.as_bytes()),
                app_id: DESKTOP_PRESENCE_APP.to_owned(),
                cluster_id: ROOM.to_owned(),
                presence: PeerPresence {
                    key: postcard::to_stdvec(&PresenceKey::Workspace {
                        id: ROOM.to_owned(),
                    })
                    .expect("a key"),
                    payload,
                },
                colour: None,
            },
        )
    }

    /// How available `from` says they are, and the call they say they are in.
    fn availability(from: u8, call: Option<String>) -> NetEvent {
        presence(
            from,
            collab_types::PeerPresencePayload::Availability {
                state: Availability::default().id().to_owned(),
                call,
            },
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the session lives until the call is over, and ends in a blocking context"
    )]
    async fn an_app_joins_a_call_and_the_people_here_read_this_person_in_it() {
        let _settled = ui::motion::testing::settled();
        let root = private_home();
        let socket = root.path().join("collab.sock");
        let sync = Rc::new(Client::untethered(
            device(0xf0),
            OwnPresence {
                label: "Quinten".to_owned(),
                user: Some(person(0xf0)),
                ..OwnPresence::default()
            },
        ));
        let mut session = session(&sync, &socket);
        step(&mut session);
        let media = session
            .media_service()
            .expect("the desk hands its apps a media service")
            .clone();

        // Chat joins a voice channel through that service, as its people's
        // call action does.
        let chat = tilo_native_media::Caller::connected(
            7,
            CHAT,
            "the-workspace",
            "chat",
            Arc::new(|| Ok(())),
        );
        let joined = media
            .command_for(
                chat.clone(),
                Request {
                    version: 1,
                    intent: 1,
                    command: Command::Join {
                        channel_id: CHANNEL.to_owned(),
                    },
                },
            )
            .expect("an app joins a voice channel");
        assert_eq!(
            joined.state,
            State::Joining,
            "a joined call waits for its carrier"
        );

        // On its next pass the session says the channel in this person's
        // presence.
        step(&mut session);
        let call = sync
            .own()
            .call
            .expect("this person's presence names the channel they joined");
        assert_eq!(call.channel, CHANNEL);

        // Another person's device hears that presence: their roster shows
        // this person in that call.
        let peer = Client::untethered(
            device(0xb0),
            OwnPresence {
                label: "Framework 16".to_owned(),
                ..OwnPresence::default()
            },
        );
        peer.follow_desktop(&desk());
        peer.hear(hello(0xf0, "Quinten"));
        peer.hear(presence(0xf0, collab_types::PeerPresencePayload::Arrived));
        peer.hear(availability(0xf0, Some(call.encode())));
        let calls = peer.calls();
        let heard = calls
            .iter()
            .find(|call| call.channel == CHANNEL)
            .expect("the peer reads the call this person is in");
        assert_eq!(heard.room, ROOM);
        assert_eq!(
            heard
                .people
                .iter()
                .map(|caller| (caller.user.clone(), caller.label.clone()))
                .collect::<Vec<_>>(),
            [(Some(person(0xf0)), "Quinten".to_owned())],
            "the person in the call is this session's person"
        );

        // Leaving takes it back.
        let left = media
            .command_for(
                chat,
                Request {
                    version: 1,
                    intent: 2,
                    command: Command::Leave,
                },
            )
            .expect("an app leaves the call");
        assert_eq!(left.state, State::Ended);
        step(&mut session);
        assert!(
            sync.own().call.is_none(),
            "leaving clears the presence of this person's call"
        );
        peer.hear(availability(0xf0, None));
        assert!(
            peer.calls().iter().all(|call| call.channel != CHANNEL),
            "the peer no longer reads this person in the call"
        );
        // The session keeps a runtime of its own, which blocking drop ends.
        tokio::task::block_in_place(|| drop(session));
    }
}
