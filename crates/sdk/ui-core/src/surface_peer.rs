//! A surface app's actions on the session that places its window.
//!
//! The desktop's own surfaces (the bar, the desk, the panels, a sheet) are
//! ordinary apps run as processes of their own. Besides the Wayland socket
//! their window becomes, the session hands each one end of a connection on
//! which the two speak the action protocol every app and its host already
//! speak ([`tilo_actions::remote::RemoteActionMessage`]): the session serves
//! the actions the desktop publishes, and the surface lists, runs and
//! reports its own. There is no other message and no surface-only kind: what
//! a surface does, and what it is asked to do, is actions.
//!
//! One message is one frame of that protocol ([`RemoteActionFrame`]), framed
//! the way the collaboration wire frames the same frames ([`FRAMING`]), so a
//! surface's connection is its host connection in all but the descriptor it
//! arrives on.
//!
//! A surface process finds the socket in [`ACTIONS_FD_ENV`]; without it the
//! app is an ordinary app with no host in reach, as before.

use std::collections::{HashMap, VecDeque};
use std::os::fd::FromRawFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, mpsc};

use tilo_actions::remote::{CataloguePage, CatalogueRow, RemoteActionFrame, RemoteActionMessage};
use tilo_lock::MutexRecover as _;

use crate::collaboration::{CollabEvent, CollabRequest, PeerTransport};

/// What this process's surface publishes, shared with the peer that answers
/// the session: the app hands its resources over every frame it draws, as a
/// hosted app hands them to its host.
static PUBLISHED: std::sync::LazyLock<
    std::sync::Mutex<Option<Arc<std::sync::Mutex<crate::collaboration::SurfaceTable>>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

/// Publish what this surface offers now, as its frame ends: the resources
/// the app was built from, with the actions they carry. Nothing happens in a
/// process that is not a surface.
pub fn publish(resources: &[tilo_actions::ResourceSpec]) {
    let Some(table) = PUBLISHED.lock_recover().clone() else {
        return;
    };
    let descriptors = tilo_actions::door::survey(resources);
    // What the surface offers is small and changed as a whole: each frame
    // hands over the whole surface rather than tracking what moved.
    let (delta, _) = crate::collaboration::SurfacePublication::next(None, &descriptors);
    let mut table = table.lock_recover();
    if let Err(error) = table.apply(&delta) {
        tracing::warn!(%error, "a surface's actions did not apply");
    }
}

/// The environment variable naming the descriptor on which a surface app
/// talks to the session that places it.
pub const ACTIONS_FD_ENV: &str = "TILO_SURFACE_ACTIONS_FD";

/// The readings of the resources this surface follows, shared with the
/// connection that answers the session: opened on the app's own thread, kept
/// by the reader thread as they arrive, and looked at when the app draws.
static READINGS: std::sync::LazyLock<std::sync::Mutex<Option<Arc<Readings>>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

/// How long opening a followed resource waits for the reading its watch
/// begins at, and how long an action waits for the session's answer: both
/// bound a lost connection, not slow work.
const SESSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);

/// One of the session's published resources as a surface app reaches it: the
/// reading it stands at, each later reading, and the actions that change it.
///
/// This is the session's own side of the action protocol
/// ([`SURFACE_PROVIDER`]) as the connection to the session hands it over: the
/// surface asks to follow a resource, the session sends the reading it stands
/// at and each one that moves, and an action is invoked on the session the
/// way any app invokes one. The readings arrive on the reader thread and only
/// the newest is kept, so a frame that was slow shows the state now, never
/// one that has passed.
///
/// [`Self::open`] blocks for the first reading: call it off the frame thread,
/// as [`Self::invoke`].
#[derive(Clone)]
pub struct SessionResource {
    resource: Arc<str>,
    readings: Arc<Readings>,
}

impl SessionResource {
    /// Follow `resource`, waiting for the reading it stands at now.
    ///
    /// # Errors
    ///
    /// This process is no surface, the session does not answer in time, or no
    /// service publishes `resource`.
    pub fn open(resource: &str) -> Result<Self, String> {
        let readings = READINGS
            .lock_recover()
            .clone()
            .ok_or("This window is no surface of a session.")?;
        let (first, answer) = mpsc::sync_channel(1);
        readings
            .opening
            .lock_recover()
            .insert(resource.to_owned(), first);
        if readings
            .outgoing
            .lock_recover()
            .send(RemoteActionMessage::ResourceWatch {
                resource: resource.to_owned(),
            })
            .is_err()
        {
            readings.opening.lock_recover().remove(resource);
            return Err("The session closed the actions socket.".into());
        }
        match answer.recv_timeout(SESSION_TIMEOUT) {
            Ok(Ok(_reading)) => Ok(Self {
                resource: Arc::from(resource),
                readings,
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => {
                readings.opening.lock_recover().remove(resource);
                Err("The session did not answer.".to_string())
            }
        }
    }

    /// The newest reading since the last call; `None` when nothing changed.
    #[must_use]
    pub fn latest(&self) -> Option<Result<String, String>> {
        self.readings
            .newest
            .lock_recover()
            .remove(self.resource.as_ref())
    }

    /// Wake `wake`'s owner at each reading that arrives, so the view showing
    /// the resource draws the new one instead of looking for it.
    pub fn attach(&self, wake: crate::wake::WakeHandle) {
        *self.readings.wake.lock_recover() = Some(wake);
    }

    /// Invoke `action` of the resource with `arguments` (none for an action
    /// that takes nothing). Blocking, bounded: call it off the frame thread.
    ///
    /// # Errors
    ///
    /// The session does not answer in time, or the door refused the action,
    /// with what it said.
    pub fn invoke(
        &self,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        let invocation = tilo_actions::outcome::InvocationId::new();
        let (reply, answer) = mpsc::sync_channel(1);
        self.readings
            .answers
            .lock_recover()
            .insert(invocation, reply);
        let sent = self
            .readings
            .outgoing
            .lock_recover()
            .send(RemoteActionMessage::Invoke(
                tilo_actions::remote::InvokeRequest {
                    invocation,
                    target: tilo_actions::remote::RemoteActionTarget {
                        provider: SURFACE_PROVIDER.to_string(),
                        address: tilo_actions::ActionAddress::new(self.resource.as_ref(), action),
                    },
                    argument: tilo_actions::remote::InvokeArgument::Json(arguments.clone()),
                    context: None,
                    confirmation: None,
                    revision: None,
                    on_behalf_of: None,
                    wait_ms: None,
                    appearance: None,
                },
            ));
        if sent.is_err() {
            self.readings.answers.lock_recover().remove(&invocation);
            return Err("The session closed the actions socket.".into());
        }
        answer
            .recv_timeout(SESSION_TIMEOUT)
            .map_err(|_| "The session did not answer the change.".to_string())?
    }
}

/// What the session answers its surfaces' actions under.
const SURFACE_PROVIDER: &str = "so.tilo.session";

/// The followed resources and the actions in flight, as the reader thread
/// keeps them.
struct Readings {
    /// The connection's writing end, to ask for a resource or take an action.
    outgoing: std::sync::Mutex<mpsc::Sender<RemoteActionMessage>>,
    /// The newest reading of each followed resource not yet taken.
    newest: std::sync::Mutex<HashMap<String, Result<String, String>>>,
    /// What waits for the first reading of a resource, by resource.
    opening: std::sync::Mutex<HashMap<String, mpsc::SyncSender<Result<String, String>>>>,
    /// What waits for the verdict on an action taken from here, by invocation.
    answers: std::sync::Mutex<
        HashMap<tilo_actions::outcome::InvocationId, mpsc::SyncSender<Result<(), String>>>,
    >,
    /// The app's wake, once its window handed one over.
    wake: std::sync::Mutex<Option<crate::wake::WakeHandle>>,
}

impl Readings {
    /// The readings this surface follows, taken from the connection it asks
    /// the session over.
    fn new(outgoing: mpsc::Sender<RemoteActionMessage>) -> Self {
        Self {
            outgoing: std::sync::Mutex::new(outgoing),
            newest: std::sync::Mutex::default(),
            opening: std::sync::Mutex::default(),
            answers: std::sync::Mutex::default(),
            wake: std::sync::Mutex::default(),
        }
    }

    /// Take in what the session said of a followed resource or a taken
    /// action. Whether it was one of those.
    fn take(&self, message: &RemoteActionMessage) -> bool {
        match message {
            RemoteActionMessage::ResourceReading { resource, reading } => {
                let delivery: Result<String, String> = match reading {
                    Some(reading) => Ok(reading.clone()),
                    None => Err(format!("No service publishes {resource}.")),
                };
                self.newest
                    .lock_recover()
                    .insert(resource.clone(), delivery.clone());
                // The opener is told the resource is served; the reading
                // itself waits in `newest` for the caller to take.
                if let Some(waiting) = self.opening.lock_recover().remove(resource) {
                    let _ = waiting.send(delivery);
                }
                self.wake();
                true
            }
            RemoteActionMessage::Receipt(receipt) => {
                let waiting = self.answers.lock_recover().remove(&receipt.invocation);
                let Some(waiting) = waiting else {
                    return false;
                };
                let _ = waiting.send(match &receipt.status {
                    tilo_actions::remote::ReceiptStatus::Rejected { error } => {
                        Err(error.to_string())
                    }
                    _ => Ok(()),
                });
                true
            }
            _ => false,
        }
    }

    fn wake(&self) {
        if let Some(wake) = self.wake.lock_recover().as_ref() {
            wake.wake();
        }
    }
}

/// Both directions' framing: one length-prefixed frame per message, as the
/// collaboration wire frames the same protocol's frames.
pub const FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(64 << 20);

/// The app's end of the connection to its session.
///
/// Cloneable pieces are not: the reader thread owns the receiving half.
pub struct SurfacePeer {
    outgoing: mpsc::Sender<RemoteActionMessage>,
    /// The app the session placed, as it joined: what its catalogue rows are
    /// published under, and what the desktop calls it.
    app_id: String,
    /// What this surface offers, as its publication moves it: the actions
    /// the session is answered with when it asks. Shared with the app's
    /// frames, which hand their resources over as they end.
    published: Arc<std::sync::Mutex<crate::collaboration::SurfaceTable>>,
    /// The route the session has acknowledged, if the app asked to join
    /// one: a surface's is its own window's, which the session places.
    acknowledged: Option<crate::collaboration::RouteSpec>,
    /// The invocations the session asked this app to run, by the id the app
    /// names them with while it runs them.
    asked: HashMap<u64, tilo_actions::outcome::InvocationId>,
    /// The id the next invocation the session asks is named by.
    asking: u64,
    incoming: mpsc::Receiver<RemoteActionMessage>,
    received: VecDeque<CollabEvent>,
    connected: bool,
    /// What wakes the app when the session speaks: its next frame, handed
    /// over by the window once it has one.
    wake: Arc<std::sync::Mutex<Option<Box<dyn Fn() + Send + Sync>>>>,
}

impl SurfacePeer {
    /// The peer on the already connected socket handed over as
    /// [`ACTIONS_FD_ENV`]'s number; `None` when the number names no socket.
    #[must_use]
    pub fn from_descriptor(number: i32) -> Option<Self> {
        // SAFETY: the session wrote this number into the environment of the
        // process it spawned, having put the socket on it before the exec.
        // Whoever started a process cannot reuse the number afterwards.
        let stream = unsafe { UnixStream::from_raw_fd(number) };
        Some(Self::of(stream))
    }

    /// A peer on `stream`, one end of a socket to the session.
    #[must_use]
    pub fn of(stream: UnixStream) -> Self {
        let (outgoing, writes) = mpsc::channel();
        let (incoming_sender, incoming) = mpsc::channel();
        let wake: Arc<std::sync::Mutex<Option<Box<dyn Fn() + Send + Sync>>>> = Arc::default();
        // The resources this surface follows are kept here, and ask the
        // session over this same connection.
        let readings = Arc::new(Readings::new(outgoing.clone()));
        *READINGS.lock_recover() = Some(Arc::clone(&readings));
        // A long-lived protocol client of its own, on threads of its own:
        // it reads whenever the session speaks, and a message waiting must
        // wake the app even when nothing else asks it for a frame.
        if let Ok(reader) = stream.try_clone() {
            let reading = Arc::clone(&wake);
            let followed = Arc::clone(&readings);
            let _ = tilo_rt::spawn_thread("surface-actions", move || {
                read_messages(reader, &incoming_sender, &reading, &followed);
            });
            let _ = tilo_rt::spawn_thread("surface-actions-writes", move || {
                write_messages(stream, &writes);
            });
        }
        let published: Arc<std::sync::Mutex<crate::collaboration::SurfaceTable>> = Arc::default();
        *PUBLISHED.lock_recover() = Some(Arc::clone(&published));
        Self {
            outgoing,
            app_id: String::new(),
            published,
            acknowledged: None,
            asked: HashMap::new(),
            asking: 0,
            incoming,
            received: VecDeque::new(),
            connected: true,
            wake: Arc::default(),
        }
    }

    /// Answer the session's request from what this app offers.
    fn answer(&mut self, message: RemoteActionMessage) -> Option<RemoteActionMessage> {
        match message {
            RemoteActionMessage::CatalogueRequest { correlation, query } => {
                let rows: Vec<CatalogueRow> = self
                    .published
                    .lock_recover()
                    .actions()
                    .iter()
                    .filter(|_| {
                        query
                            .provider
                            .as_ref()
                            .is_none_or(|provider| *provider == self.app_id)
                    })
                    .map(|action| {
                        CatalogueRow::for_action(
                            self.app_id.clone(),
                            self.app_id.clone(),
                            action.descriptor(tilo_actions::ExecutionLocus::ClientLocal),
                        )
                    })
                    .collect();
                Some(RemoteActionMessage::CatalogueResponse {
                    correlation,
                    result: Ok(CataloguePage::from_rows(
                        format!("v1:{}", self.app_id),
                        rows,
                        &query,
                    )),
                })
            }
            RemoteActionMessage::Invoke(request) => {
                // The app runs what it publishes through its own actions, as
                // it does an ask from a room's host; the id the app names the
                // run with is what its outcome arrives under.
                self.asking = self.asking.wrapping_add(1);
                let id = self.asking;
                let argument = request.argument.to_slot();
                self.asked.insert(id, request.invocation);
                self.received.push_back(CollabEvent::Invocation {
                    id,
                    invocation: request.invocation,
                    resource: request.target.address.resource,
                    action: request.target.address.action,
                    argument,
                    // Asked by the person at the desktop that placed the
                    // surface: their own keys reach it the same way.
                    actor: crate::collaboration::local_user_id().0,
                    context: None,
                });
                None
            }
            other => {
                // What answers this app's own requests: a receipt, an
                // invocation's state, a catalogue page.
                self.received
                    .push_back(CollabEvent::RemoteAction { message: other });
                None
            }
        }
    }
}

impl PeerTransport for SurfacePeer {
    fn is_connected(&self) -> bool {
        self.connected
    }

    fn hosts_surface(&self) -> bool {
        true
    }

    fn send(&mut self, request: CollabRequest) -> Result<(), String> {
        match request {
            CollabRequest::Hello {
                app_id,
                cluster_id,
                record,
                ..
            } => {
                self.app_id = app_id;
                self.acknowledged = Some(crate::collaboration::RouteSpec {
                    app_id: self.app_id.clone(),
                    cluster_id,
                    record,
                });
                Ok(())
            }
            CollabRequest::RemoteAction { message } => self
                .outgoing
                .send(message)
                .map_err(|_| String::from("the session closed the actions socket")),
            CollabRequest::UpdateInvocation { id, update } => {
                let Some(invocation) = self.asked.remove(&id) else {
                    return Ok(());
                };
                let state = tilo_actions::remote::InvocationState::new(invocation, update);
                self.outgoing
                    .send(RemoteActionMessage::Update {
                        invocation,
                        state: Some(state),
                    })
                    .map_err(|_| String::from("the session closed the actions socket"))
            }
            CollabRequest::PublishActionSurface { delta } => {
                // The app this peer is was named when it joined: a surface
                // publishes what it offers, and the session reads it as the
                // app it placed.
                if let Err(error) = self.published.lock_recover().apply(&delta) {
                    tracing::warn!(%error, "a surface's action surface did not apply");
                }
                Ok(())
            }
            // There is no room to sync, no document to publish and no
            // device to reach: a surface's desktop is the process it speaks
            // to here, and everything it is told is an action.
            _ => Ok(()),
        }
    }

    fn take_primary_route_result(
        &mut self,
    ) -> Option<Result<crate::collaboration::RouteSpec, String>> {
        self.acknowledged.take().map(Ok)
    }

    fn set_wake(&mut self, wake: Box<dyn Fn() + Send + Sync>) {
        *self.wake.lock_recover() = Some(wake);
    }

    fn poll(&mut self) -> Vec<CollabEvent> {
        while let Ok(message) = self.incoming.try_recv() {
            // What the session asked and this app answers itself: its
            // catalogue of actions goes back to it right away.
            if let Some(reply) = self.answer(message) {
                let _ = self.outgoing.send(reply);
            }
        }
        self.received.drain(..).collect()
    }
}

/// Read the session's messages off `stream` until it closes, waking the app
/// for each.
fn read_messages(
    mut stream: UnixStream,
    incoming: &mpsc::Sender<RemoteActionMessage>,
    wake: &Arc<std::sync::Mutex<Option<Box<dyn Fn() + Send + Sync>>>>,
    readings: &Arc<Readings>,
) {
    loop {
        let Some(body) = FRAMING.read(&mut stream).ok().flatten() else {
            return;
        };
        let Ok(frame) = RemoteActionFrame::decode(&body) else {
            tracing::warn!("the session sent a message that is not one");
            return;
        };
        // What this surface follows, and how what it asked stands, is the
        // app's own business: it never waits for a frame to be taken.
        if readings.take(&frame.message) {
            continue;
        }
        if incoming.send(frame.message).is_err() {
            return;
        }
        // The app may be resting: what the session said waits for a frame
        // that would otherwise never run.
        if let Some(wake) = wake.lock_recover().as_ref() {
            wake();
        }
    }
}

/// Write the app's messages to the session on `stream` until it closes.
fn write_messages(mut stream: UnixStream, outgoing: &mpsc::Receiver<RemoteActionMessage>) {
    while let Ok(message) = outgoing.recv() {
        let Ok(body) = RemoteActionFrame::new(message).encode() else {
            tracing::warn!("a message to the session did not encode");
            return;
        };
        if FRAMING.write(&mut stream, &body).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::ActionAddress;
    use tilo_actions::outcome::InvocationId;
    use tilo_actions::remote::{CatalogueQuery, InvokeArgument, InvokeRequest, RemoteActionTarget};

    #[test]
    fn a_surface_answers_the_sessions_actions() {
        let (app, session) = UnixStream::pair().expect("a socket");
        let mut peer = SurfacePeer::of(app);
        let mut session = session;
        peer.send(CollabRequest::RemoteAction {
            message: RemoteActionMessage::CatalogueRequest {
                correlation: 7,
                query: CatalogueQuery::default(),
            },
        })
        .expect("sent");
        let body = FRAMING
            .read(&mut session)
            .expect("a read")
            .expect("a message");
        let frame = RemoteActionFrame::decode(&body).expect("decoded");
        assert_eq!(
            frame.message,
            RemoteActionMessage::CatalogueRequest {
                correlation: 7,
                query: CatalogueQuery::default(),
            }
        );
    }

    #[test]
    fn the_sessions_invocation_reaches_the_app_as_one() {
        // The reader thread's queue, filled as the session's message would
        // arrive: what poll answers an app's turn with.
        let (outgoing, _writes) = mpsc::channel();
        let (session, incoming) = mpsc::channel();
        let mut peer = SurfacePeer {
            outgoing,
            app_id: "so.tilo.topbar".into(),
            published: Arc::default(),
            acknowledged: None,
            asked: HashMap::new(),
            asking: 0,
            incoming,
            received: VecDeque::new(),
            connected: true,
            wake: Arc::default(),
        };
        let invocation = InvocationId::new();
        session
            .send(RemoteActionMessage::Invoke(InvokeRequest {
                invocation,
                target: RemoteActionTarget {
                    provider: "so.tilo.topbar".into(),
                    address: ActionAddress::new("bar", "open-menu"),
                },
                argument: InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: None,
                on_behalf_of: None,
                wait_ms: None,
                appearance: None,
            }))
            .expect("the session speaks");
        let event = peer.poll().into_iter().next().expect("an event");
        let CollabEvent::Invocation {
            id,
            invocation: arrived,
            resource,
            action,
            ..
        } = event
        else {
            panic!("not an invocation");
        };
        assert_eq!(
            (id, arrived, resource.as_str(), action.as_str()),
            (1, invocation, "bar", "open-menu")
        );
    }

    #[test]
    fn a_surface_joins_its_own_window_and_carries_no_room() {
        let (app, _session) = UnixStream::pair().expect("a socket");
        let mut peer = SurfacePeer::of(app);
        // The one document a surface has is the window the session placed
        // it as, which the session answers for.
        let cluster = uuid::Uuid::new_v4();
        peer.send(CollabRequest::Hello {
            app_id: "so.tilo.topbar".into(),
            cluster_id: cluster,
            record: None,
            record_kind: None,
            handle: None,
        })
        .expect("a surface joins");
        let joined = peer.take_primary_route_result().expect("acknowledged");
        assert_eq!(joined.expect("no refusal").app_id, "so.tilo.topbar");
        // Anything a room would carry is not this app's business, and is
        // not a reason to stop answering actions.
        peer.send(CollabRequest::RequestDocumentSnapshot)
            .expect("dropped, not fatal");
    }
}
