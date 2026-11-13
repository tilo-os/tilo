//! A surface app's actions on the session that places its window. The desktop's own
//! surfaces (the bar, the desk, the panels, a sheet) are ordinary apps run as processes
//! of their own.

use std::collections::{HashMap, VecDeque};
use std::os::fd::FromRawFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, mpsc};

use tilo_actions::remote::{
    CataloguePage, CatalogueQuery, CatalogueRow, RemoteActionFrame, RemoteActionMessage,
};
use tilo_lock::MutexRecover as _;

use crate::collaboration::{CollabEvent, CollabRequest, PeerTransport};

static FRAME: std::sync::LazyLock<std::sync::Mutex<Option<Arc<std::sync::Mutex<Frame>>>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

/// What the surface offers, and whether the session has yet to hear of it: the last
/// frame's resources and whatever the framework's route offered beside them. They are
/// kept apart because each publishes a surface whole, and one must not withdraw what the
/// other offers.
#[derive(Default)]
struct Frame {
    table: crate::collaboration::SurfaceTable,
    route: crate::collaboration::SurfaceTable,
    offered: Option<Vec<tilo_actions::door::ActionDescriptor>>,
    owed: bool,
}

/// Publish what this surface offers now, as its frame ends: the resources the app draws
/// from, with the actions they carry.
pub fn publish(resources: &[tilo_actions::ResourceSpec]) {
    let Some(frame) = FRAME.lock_recover().clone() else {
        return;
    };
    let offered = tilo_actions::door::survey(resources);
    let mut frame = frame.lock_recover();
    if frame.offered.as_ref() == Some(&offered) {
        return;
    }
    let (delta, _) = crate::collaboration::SurfacePublication::next(None, &offered);
    if let Err(error) = frame.table.apply(&delta) {
        tracing::warn!(%error, "a surface's actions did not apply");
        return;
    }
    frame.offered = Some(offered);
    frame.owed = true;
}

/// Both directions' framing: one length-prefixed frame per message, as the collaboration
/// wire frames the same protocol's frames.
pub const FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(64 << 20);

/// The app's end of the connection to its session. Cloneable pieces are not: the reader
/// thread owns the receiving half.
pub struct SurfacePeer {
    outgoing: mpsc::Sender<RemoteActionMessage>,
    app_id: String,
    frame: Arc<std::sync::Mutex<Frame>>,
    acknowledged: Option<crate::collaboration::RouteSpec>,
    asked: HashMap<u64, tilo_actions::outcome::InvocationId>,
    asking: u64,
    incoming: mpsc::Receiver<RemoteActionMessage>,
    received: VecDeque<CollabEvent>,
    connected: bool,
    wake: Arc<std::sync::Mutex<Option<Box<dyn Fn() + Send + Sync>>>>,
}

impl SurfacePeer {
    /// The peer on the already connected socket handed over as
    /// [`ACTIONS_FD_ENV`](crate::surface_services::ACTIONS_FD_ENV)'s number; `None` when
    /// the number names no socket.
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
        // A long-lived protocol client of its own, on threads of its own: it reads
        // whenever the session speaks, and a message waiting must wake the app even when
        // nothing else asks it for a frame.
        if let Ok(reader) = stream.try_clone() {
            let reading = Arc::clone(&wake);
            let _ = tilo_rt::spawn_thread("surface-actions", move || {
                read_messages(reader, &incoming_sender, &reading);
            });
            let _ = tilo_rt::spawn_thread("surface-actions-writes", move || {
                write_messages(stream, &writes);
            });
        }
        let frame: Arc<std::sync::Mutex<Frame>> = Arc::default();
        *FRAME.lock_recover() = Some(Arc::clone(&frame));
        Self {
            outgoing,
            app_id: String::new(),
            frame,
            acknowledged: None,
            asked: HashMap::new(),
            asking: 0,
            incoming,
            received: VecDeque::new(),
            connected: true,
            wake,
        }
    }

    fn catalogue(&self, query: &CatalogueQuery) -> CataloguePage {
        let frame = self.frame.lock_recover();
        let mut seen = std::collections::HashSet::new();
        let rows: Vec<CatalogueRow> = frame
            .table
            .actions()
            .into_iter()
            .chain(frame.route.actions())
            .filter(|action| seen.insert((action.resource.clone(), action.action.clone())))
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
        CataloguePage::from_rows(format!("v1:{}", self.app_id), rows, query)
    }

    fn answer(&mut self, message: RemoteActionMessage) -> Option<RemoteActionMessage> {
        match message {
            RemoteActionMessage::CatalogueRequest { correlation, query } => {
                let page = self.catalogue(&query);
                Some(RemoteActionMessage::CatalogueResponse {
                    correlation,
                    result: Ok(page),
                })
            }
            RemoteActionMessage::Invoke(request) => {
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
                    actor: crate::collaboration::local_user_id().0,
                    context: None,
                });
                None
            }
            other => {
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
                let mut frame = self.frame.lock_recover();
                if let Err(error) = frame.route.apply(&delta) {
                    tracing::warn!(%error, "a surface's action surface did not apply");
                    return Ok(());
                }
                frame.owed = true;
                Ok(())
            }
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
            if let Some(reply) = self.answer(message) {
                let _ = self.outgoing.send(reply);
            }
        }
        let owes = {
            let mut frame = self.frame.lock_recover();
            std::mem::replace(&mut frame.owed, false)
        };
        if owes {
            let page = self.catalogue(&CatalogueQuery::default());
            let _ = self
                .outgoing
                .send(RemoteActionMessage::CatalogueChanged { page });
        }
        self.received.drain(..).collect()
    }
}

fn read_messages(
    mut stream: UnixStream,
    incoming: &mpsc::Sender<RemoteActionMessage>,
    wake: &Arc<std::sync::Mutex<Option<Box<dyn Fn() + Send + Sync>>>>,
) {
    loop {
        let Some(body) = FRAMING.read(&mut stream).ok().flatten() else {
            return;
        };
        let Ok(frame) = RemoteActionFrame::decode(&body) else {
            tracing::warn!("the session sent a message that is not one");
            return;
        };
        if incoming.send(frame.message).is_err() {
            return;
        }
        // The app may be resting: what the session said waits for a frame that would
        // otherwise never run.
        if let Some(wake) = wake.lock_recover().as_ref() {
            wake();
        }
    }
}

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
    use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};

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
    fn a_message_from_the_session_wakes_the_app_that_installed_a_wake() {
        let (app, mut session) = UnixStream::pair().expect("a socket");
        session
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .expect("a timeout");
        let mut peer = SurfacePeer::of(app);
        let (woken, wake) = mpsc::channel();
        peer.set_wake(Box::new(move || {
            let _ = woken.send(());
        }));
        let asked = RemoteActionMessage::CatalogueRequest {
            correlation: 11,
            query: CatalogueQuery::default(),
        };
        let body = RemoteActionFrame::new(asked).encode().expect("encodes");
        FRAMING.write(&mut session, &body).expect("written");
        wake.recv_timeout(std::time::Duration::from_secs(10))
            .expect("the app is woken for what the session said");
        peer.poll();
        let body = FRAMING
            .read(&mut session)
            .expect("a read")
            .expect("an answer");
        let frame = RemoteActionFrame::decode(&body).expect("decoded");
        assert!(
            matches!(
                frame.message,
                RemoteActionMessage::CatalogueResponse {
                    correlation: 11,
                    ..
                }
            ),
            "the woken app answers what it was asked: {:?}",
            frame.message
        );
    }

    #[test]
    fn the_sessions_invocation_reaches_the_app_as_one() {
        // The reader thread's queue, filled as the session's message would arrive: what
        // poll answers an app's turn with.
        let (outgoing, _writes) = mpsc::channel();
        let (session, incoming) = mpsc::channel();
        let mut peer = SurfacePeer {
            outgoing,
            app_id: "so.tilo.topbar".into(),
            frame: Arc::default(),
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
    fn a_surface_joins_its_own_window_and_carries_no_workspace() {
        let (app, _session) = UnixStream::pair().expect("a socket");
        let mut peer = SurfacePeer::of(app);
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
        peer.send(CollabRequest::RequestDocumentSnapshot)
            .expect("dropped, not fatal");
    }
}
