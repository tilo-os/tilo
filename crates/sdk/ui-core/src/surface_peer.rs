//! A surface app's actions on the session that places its window.
//!
//! The desktop's own surfaces (the bar, the desk, the panels, a sheet) are
//! apps run as processes of their own. Besides the Wayland socket their
//! window becomes, the session hands each one end of a socket on which the
//! two answer each other's actions: the session serves the actions the
//! desktop publishes, as any host serves a caller
//! ([`tilo_actions::remote::RemoteActionMessage`]),
//! and asks the surface to run one of its own
//! ([`SurfaceMessage::Invocation`]), hearing the outcome
//! ([`SurfaceMessage::UpdateInvocation`]). There is no other channel: what a
//! surface does, and what it is asked to do, is actions.
//!
//! A surface process finds the socket in [`ACTIONS_FD_ENV`]; without it the
//! app is an ordinary app with no host in reach, as before.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, mpsc};

use serde::{Deserialize, Serialize};
use tilo_actions::InvocationUpdate;
use tilo_actions::outcome::InvocationId;
use tilo_actions::remote::RemoteActionFrame;
use tilo_lock::MutexRecover as _;

use crate::collaboration::{CollabEvent, CollabRequest, PeerTransport};

/// The environment variable naming the descriptor on which a surface app
/// talks to the session that places it.
pub const ACTIONS_FD_ENV: &str = "TILO_SURFACE_ACTIONS_FD";

/// Largest message either side sends: a catalogue page of actions, an
/// invocation's outcome. Past it the connection closes rather than the two
/// sides disagreeing about where a message ended.
pub const MAX_SURFACE_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

/// One message between a surface app and its session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SurfaceMessage {
    /// One message of the action protocol, as the session's host or the
    /// app's client speaks it.
    Action(RemoteActionFrame),
    /// The session asks the surface to run the action `resource`/`action`
    /// its own catalogue offers, under `id`, and to answer with
    /// [`Self::UpdateInvocation`].
    Invocation {
        id: u64,
        invocation: InvocationId,
        resource: String,
        action: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        argument: Option<String>,
        actor: String,
    },
    /// The surface says how the invocation `id` it was asked to run is
    /// going, or how it ended.
    UpdateInvocation { id: u64, update: InvocationUpdate },
    /// The actions the surface offers the desktop, whole: the resources
    /// it publishes, each with its actions.
    Published {
        app_id: String,
        actions: Vec<crate::collaboration::ExportedAction>,
    },
}

impl SurfaceMessage {
    /// The message as the bytes one write of the socket carries: its length
    /// first, little endian, then its JSON.
    ///
    /// # Errors
    ///
    /// [`ErrorMessage::TooLarge`] past [`MAX_SURFACE_MESSAGE_BYTES`], or
    /// [`ErrorMessage::Malformed`] if it does not encode.
    pub fn encode(&self) -> Result<Vec<u8>, ErrorMessage> {
        let body =
            serde_json::to_vec(self).map_err(|error| ErrorMessage::Malformed(error.to_string()))?;
        if body.len() > MAX_SURFACE_MESSAGE_BYTES {
            return Err(ErrorMessage::TooLarge { len: body.len() });
        }
        let mut bytes = Vec::with_capacity(body.len() + 4);
        bytes.extend_from_slice(&u32::try_from(body.len()).unwrap_or(u32::MAX).to_le_bytes());
        bytes.extend_from_slice(&body);
        Ok(bytes)
    }

    /// The message `body` holds, its length having been read already.
    ///
    /// # Errors
    ///
    /// [`ErrorMessage::Malformed`] when it is not one.
    pub fn decode(body: &[u8]) -> Result<Self, ErrorMessage> {
        serde_json::from_slice(body).map_err(|error| ErrorMessage::Malformed(error.to_string()))
    }
}

/// Why bytes are not a usable surface message.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ErrorMessage {
    #[error("surface message of {len} bytes exceeds the limit")]
    TooLarge { len: usize },
    #[error("malformed surface message: {0}")]
    Malformed(String),
}

/// The app's end of the socket to its session.
///
/// Cloneable pieces are not: the reader thread owns the receiving half.
pub struct SurfacePeer {
    outgoing: mpsc::Sender<SurfaceMessage>,
    /// What this surface offers, as its publication moves it.
    published: crate::collaboration::SurfaceTable,
    /// The route the session has acknowledged, if the app asked to join
    /// one: a surface's is its own window's, which the session places.
    acknowledged: Option<crate::collaboration::RouteSpec>,
    incoming: mpsc::Receiver<SurfaceMessage>,
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
        Self {
            outgoing,
            published: crate::collaboration::SurfaceTable::default(),
            acknowledged: None,
            incoming,
            received: VecDeque::new(),
            connected: true,
            wake,
        }
    }

    /// Whether the socket to the session is still open.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.connected
    }
}

impl std::fmt::Debug for SurfacePeer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SurfacePeer")
            .field("connected", &self.connected)
            .finish_non_exhaustive()
    }
}

impl PeerTransport for SurfacePeer {
    fn is_connected(&self) -> bool {
        self.connected
    }

    fn send(&mut self, request: CollabRequest) -> Result<(), String> {
        let message = match request {
            // A surface's route is its own window, placed by the session:
            // there is no room to join, and the app is told so.
            CollabRequest::Hello {
                app_id,
                cluster_id,
                record,
                ..
            } => {
                self.acknowledged = Some(crate::collaboration::RouteSpec {
                    app_id,
                    cluster_id,
                    record,
                });
                return Ok(());
            }
            CollabRequest::RemoteAction { message } => {
                SurfaceMessage::Action(RemoteActionFrame::new(message))
            }
            CollabRequest::UpdateInvocation { id, update } => {
                SurfaceMessage::UpdateInvocation { id, update }
            }
            // What the app publishes is kept here as the table the host
            // would hold, and what it offers now is sent whole: a surface
            // exports a handful of actions, and the session then holds the
            // same list a broker's catalogue would show.
            CollabRequest::PublishActions { app_id, actions } => {
                SurfaceMessage::Published { app_id, actions }
            }
            CollabRequest::PublishActionSurface { app_id, delta } => {
                if let Err(error) = self.published.apply(&delta) {
                    return Err(format!("the published actions: {error}"));
                }
                SurfaceMessage::Published {
                    app_id,
                    actions: self.published.actions(),
                }
            }
            // There is no room to sync, no document to publish and no
            // device to reach: what a surface does is actions, and saying
            // so to requests that are not is what keeps the ones that are
            // working. A closed socket is reported where it is sent on.
            _ => return Ok(()),
        };
        self.outgoing
            .send(message)
            .map_err(|_| "the session's socket to this surface is closed".to_owned())
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
            match message {
                SurfaceMessage::Action(frame) => {
                    self.received.push_back(CollabEvent::RemoteAction {
                        message: frame.message,
                    })
                }
                SurfaceMessage::Invocation {
                    id,
                    invocation,
                    resource,
                    action,
                    argument,
                    actor,
                } => self.received.push_back(CollabEvent::Invocation {
                    id,
                    invocation,
                    resource,
                    action,
                    argument,
                    actor,
                    context: None,
                }),
                // The app's own reports, arriving at the app: nothing to do.
                SurfaceMessage::UpdateInvocation { .. } | SurfaceMessage::Published { .. } => {}
            }
        }
        self.received.drain(..).collect()
    }
}

/// Read messages from the session off `stream` until it closes, handing
/// each to `incoming`; on a broken message, stop, closing the connection.
fn read_messages(
    mut stream: UnixStream,
    incoming: &mpsc::Sender<SurfaceMessage>,
    wake: &std::sync::Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
) {
    loop {
        let mut length = [0_u8; 4];
        if stream.read_exact(&mut length).is_err() {
            return;
        }
        let length = u32::from_le_bytes(length) as usize;
        if length > MAX_SURFACE_MESSAGE_BYTES {
            return;
        }
        let mut body = vec![0_u8; length];
        if stream.read_exact(&mut body).is_err() {
            return;
        }
        let Ok(message) = SurfaceMessage::decode(&body) else {
            return;
        };
        if incoming.send(message).is_err() {
            return;
        }
        // The app may be resting: what the session said waits for a frame
        // that would otherwise never run.
        if let Some(wake) = wake.lock_recover().as_ref() {
            wake();
        }
    }
}

/// Write messages for the session to `stream` until `outgoing` closes.
fn write_messages(mut stream: UnixStream, outgoing: &mpsc::Receiver<SurfaceMessage>) {
    while let Ok(message) = outgoing.recv() {
        let Ok(bytes) = message.encode() else {
            return;
        };
        if stream.write_all(&bytes).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::remote::{CatalogueQuery, RemoteActionMessage};

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
        let mut length = [0_u8; 4];
        session.read_exact(&mut length).expect("a message");
        let len = usize::try_from(u32::from_le_bytes(length)).expect("a length");
        let mut body = vec![0_u8; len];
        session.read_exact(&mut body).expect("the message");
        let SurfaceMessage::Action(frame) = SurfaceMessage::decode(&body).expect("decoded") else {
            panic!("not an action");
        };
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
            published: crate::collaboration::SurfaceTable::default(),
            acknowledged: None,
            incoming,
            received: VecDeque::new(),
            connected: true,
            wake: Arc::default(),
        };
        let invocation = InvocationId::new();
        session
            .send(SurfaceMessage::Invocation {
                id: 3,
                invocation,
                resource: "bar".into(),
                action: "open-menu".into(),
                argument: None,
                actor: "person".into(),
            })
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
            (3, invocation, "bar", "open-menu")
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
