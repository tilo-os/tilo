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

thread_local! {
    /// What this process's surface publishes, shared with the peer that
    /// answers the session: the app hands its resources over every frame it
    /// draws, as a hosted app hands them to its host.
    static PUBLISHED: std::cell::RefCell<Option<Arc<std::sync::Mutex<crate::collaboration::SurfaceTable>>>> =
        const { std::cell::RefCell::new(None) };
}

/// Publish what this surface offers now, as its frame ends: the resources
/// the app was built from, with the actions they carry. Nothing happens in a
/// process that is not a surface.
pub fn publish(resources: &[tilo_actions::ResourceSpec]) {
    let table = PUBLISHED.with(|slot| slot.borrow().clone());
    let Some(table) = table else {
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
        // A long-lived protocol client of its own, on threads of its own:
        // it reads whenever the session speaks, and a message waiting must
        // wake the app even when nothing else asks it for a frame.
        if let Ok(reader) = stream.try_clone() {
            let reading = Arc::clone(&wake);
            let _ = tilo_rt::spawn_thread("surface-actions", move || {
                read_messages(reader, &incoming_sender, &reading);
            });
            let _ = tilo_rt::spawn_thread("surface-actions-writes", move || {
                write_messages(stream, &writes);
            });
        }
        let published: Arc<std::sync::Mutex<crate::collaboration::SurfaceTable>> = Arc::default();
        PUBLISHED.with(|slot| *slot.borrow_mut() = Some(Arc::clone(&published)));
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
                    // Asked by the desktop itself, which is what the surface
                    // is a surface of.
                    actor: String::from("the desktop"),
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
            CollabRequest::PublishActionSurface { app_id, delta } => {
                self.app_id = app_id;
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
            self.answer(message);
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
