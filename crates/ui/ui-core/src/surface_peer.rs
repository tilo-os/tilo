//! A surface app's actions on the session that places its window. The shell's own
//! surfaces (the bar, the shell, the panels, a sheet) are ordinary apps run as processes
//! of their own.
//!
//! The session places a surface app's window itself, so the window has no `shell-ui`
//! connection of a desktop window's: what its content shows by name
//! ([`crate::window_targets`]) and a press the session asks of one of those controls
//! travel here instead, as the same messages a window says and hears there
//! ([`FloatingSurfaceClientMessage::WindowTargets`],
//! [`FloatingSurfaceHostMessage::PressTarget`]). A person's tools, an agent and a test then
//! find and press the keys of the on-screen keyboard as they do a window's buttons.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, mpsc};

use runtime_portal::surface_actions::{Reader, Writer};

use tilo_actions::remote::{
    CataloguePage, CatalogueQuery, CatalogueRow, RemoteActionFrame, RemoteActionMessage,
};
use tilo_lock::MutexRecover as _;

use crate::floating_surface::{FloatingSurfaceClientMessage, FloatingSurfaceHostMessage};
use crate::window_targets::{TargetAction, WindowTarget};
use tilo_ui_base::{
    collaboration::CollabEvent, collaboration::CollabRequest, collaboration::PeerTransport,
};

static FRAME: std::sync::LazyLock<std::sync::Mutex<Option<Arc<std::sync::Mutex<Frame>>>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

/// What the surface offers, and whether the session has yet to hear of it: the last
/// frame's resources and whatever the framework's route offered beside them. They are
/// kept apart because each publishes a surface whole, and one must not withdraw what the
/// other offers.
#[derive(Default)]
struct Frame {
    table: tilo_ui_base::collaboration::SurfaceTable,
    route: tilo_ui_base::collaboration::SurfaceTable,
    offered: Option<Vec<tilo_actions::api::ActionDescriptor>>,
    owed: bool,
    /// The presses the session asked of the window's controls, not taken yet.
    presses: VecDeque<Press>,
    /// The way to the session's end of the socket: what the window shows by name goes
    /// there.
    said: Option<mpsc::Sender<Said>>,
}

/// One message to the session: the action protocol every app speaks to its host, or what
/// the window shows by name.
enum Said {
    Action(RemoteActionMessage),
    Window(FloatingSurfaceClientMessage),
}

impl From<RemoteActionMessage> for Said {
    fn from(message: RemoteActionMessage) -> Self {
        Self::Action(message)
    }
}

/// A control of this surface's window the session pressed, as it named it: the control's
/// name and role as the window listed it, or what it does, and how it is acted on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Press {
    pub name: String,
    pub role: String,
    pub identity: Option<String>,
    pub action: TargetAction,
}

/// Whether this process is a surface app the session reaches over its actions socket.
#[must_use]
pub fn hosted() -> bool {
    FRAME.lock_recover().is_some()
}

/// Tell the session what this surface's window shows by name now: the controls its
/// accessibility tree exports, in the window's own logical coordinates.
pub fn publish_targets(targets: Vec<WindowTarget>) {
    let Some(frame) = FRAME.lock_recover().clone() else {
        return;
    };
    let frame = frame.lock_recover();
    if let Some(said) = &frame.said
        && said
            .send(Said::Window(FloatingSurfaceClientMessage::WindowTargets {
                targets,
                instance: None,
            }))
            .is_err()
    {
        tracing::debug!("the session closed the actions socket");
    }
}

/// The presses the session asked of this surface's window since the last call.
#[must_use]
pub fn take_presses() -> Vec<Press> {
    let Some(frame) = FRAME.lock_recover().clone() else {
        return Vec::new();
    };
    let mut frame = frame.lock_recover();
    frame.presses.drain(..).collect()
}

/// Publish what this surface offers now, as its frame ends: the resources the app draws
/// from, with the actions they carry.
pub fn publish(resources: &[tilo_actions::ResourceSpec]) {
    let Some(frame) = FRAME.lock_recover().clone() else {
        return;
    };
    let offered = tilo_actions::api::survey(resources);
    let mut frame = frame.lock_recover();
    if frame.offered.as_ref() == Some(&offered) {
        return;
    }
    let (delta, _) = tilo_ui_base::collaboration::SurfacePublication::next(None, &offered);
    if let Err(error) = frame.table.apply(&delta) {
        tracing::warn!(%error, "a surface's actions did not apply");
        return;
    }
    frame.offered = Some(offered);
    frame.owed = true;
}

/// Both directions' framing, as [`runtime_portal::surface_actions`] frames a surface's
/// messages.
pub use runtime_portal::surface_actions::FRAMING;

/// The app's end of the connection to its session. Cloneable pieces are not: the reader
/// thread owns the receiving half.
pub struct SurfacePeer {
    outgoing: mpsc::Sender<Said>,
    app_id: String,
    frame: Arc<std::sync::Mutex<Frame>>,
    acknowledged: Option<tilo_ui_base::collaboration::RouteSpec>,
    asked: HashMap<u64, tilo_actions::outcome::InvocationId>,
    asking: u64,
    incoming: mpsc::Receiver<RemoteActionMessage>,
    received: VecDeque<CollabEvent>,
    connected: bool,
    wake: Arc<std::sync::Mutex<Option<Box<dyn Fn() + Send + Sync>>>>,
}

impl SurfacePeer {
    /// The peer on the already connected socket handed over as
    /// [`ACTIONS_FD_ENV`](tilo_ui_base::surface_services::ACTIONS_FD_ENV)'s number; `None` when
    /// the number names no socket.
    #[must_use]
    pub fn from_descriptor(number: i32) -> Option<Self> {
        // SAFETY: the session wrote this number into the environment of the
        // process it spawned, having put the socket on it before the exec.
        // Whoever started a process cannot reuse the number afterwards.
        let (reader, writer) = unsafe { runtime_portal::surface_actions::take(number) }?;
        Some(Self::of(reader, writer))
    }

    /// A peer on the two halves of a socket to the session.
    #[must_use]
    fn of(reader: Reader, writer: Writer) -> Self {
        let (outgoing, writes) = mpsc::channel();
        let (incoming_sender, incoming) = mpsc::channel();
        let wake: Arc<std::sync::Mutex<Option<Box<dyn Fn() + Send + Sync>>>> = Arc::default();
        // A long-lived protocol client of its own, on threads of its own: it reads
        // whenever the session speaks, and a message waiting must wake the app even when
        // nothing else asks it for a frame.
        let frame: Arc<std::sync::Mutex<Frame>> = Arc::new(std::sync::Mutex::new(Frame {
            said: Some(outgoing.clone()),
            ..Frame::default()
        }));
        let reading = Arc::clone(&wake);
        let pressed = Arc::clone(&frame);
        let _ = tilo_rt::spawn_thread("surface-actions", move || {
            read_messages(reader, &incoming_sender, &pressed, &reading);
        });
        let _ = tilo_rt::spawn_thread("surface-actions-writes", move || {
            write_messages(writer, &writes);
        });
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
                    actor: tilo_ui_base::collaboration::local_user_id().into(),
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
                self.acknowledged = Some(tilo_ui_base::collaboration::RouteSpec {
                    app_id: self.app_id.clone(),
                    cluster_id,
                    document: record,
                });
                Ok(())
            }
            CollabRequest::RemoteAction { message, .. } => self
                .outgoing
                .send(message.into())
                .map_err(|_| String::from("the session closed the actions socket")),
            CollabRequest::UpdateInvocation { id, update } => {
                let Some(invocation) = self.asked.remove(&id) else {
                    return Ok(());
                };
                let state = tilo_actions::remote::InvocationState::new(invocation, update);
                self.outgoing
                    .send(
                        RemoteActionMessage::Update {
                            invocation,
                            state: Some(state),
                            correlation: None,
                        }
                        .into(),
                    )
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
    ) -> Option<Result<tilo_ui_base::collaboration::RouteSpec, String>> {
        self.acknowledged.take().map(Ok)
    }

    fn set_wake(&mut self, wake: Box<dyn Fn() + Send + Sync>) {
        *self.wake.lock_recover() = Some(wake);
    }

    fn poll(&mut self) -> Vec<CollabEvent> {
        while let Ok(message) = self.incoming.try_recv() {
            if let Some(reply) = self.answer(message) {
                let _ = self.outgoing.send(reply.into());
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
                .send(RemoteActionMessage::CatalogueChanged { page }.into());
        }
        self.received.drain(..).collect()
    }
}

fn read_messages(
    mut reader: Reader,
    incoming: &mpsc::Sender<RemoteActionMessage>,
    frame: &Arc<std::sync::Mutex<Frame>>,
    wake: &Arc<std::sync::Mutex<Option<Box<dyn Fn() + Send + Sync>>>>,
) {
    loop {
        let Some(body) = reader.read().ok().flatten() else {
            return;
        };
        if let Ok(message) = RemoteActionFrame::decode(&body) {
            if incoming.send(message.message).is_err() {
                return;
            }
        } else if let Ok(FloatingSurfaceHostMessage::PressTarget {
            name,
            role,
            identity,
            action,
            ..
        }) = serde_json::from_slice(&body)
        {
            frame.lock_recover().presses.push_back(Press {
                name,
                role,
                identity,
                action,
            });
        } else {
            tracing::warn!("the session sent a message that is not one");
            return;
        }
        // The app may be resting: what the session said waits for a frame that would
        // otherwise never run.
        if let Some(wake) = wake.lock_recover().as_ref() {
            wake();
        }
    }
}

fn write_messages(mut writer: Writer, outgoing: &mpsc::Receiver<Said>) {
    while let Ok(said) = outgoing.recv() {
        let body = match said {
            Said::Action(message) => RemoteActionFrame::new(message)
                .encode()
                .map_err(|error| error.to_string()),
            Said::Window(message) => {
                serde_json::to_vec(&message).map_err(|error| error.to_string())
            }
        };
        let Ok(body) = body else {
            tracing::warn!("a message to the session did not encode");
            return;
        };
        if writer.write(&body).is_err() {
            return;
        }
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    use tilo_actions::ActionAddress;
    use tilo_actions::outcome::InvocationId;
    use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};

    #[test]
    fn a_surface_answers_the_sessions_actions() {
        let (app, session) = UnixStream::pair().expect("a socket");
        let (app, writing) = runtime_portal::surface_actions::split(app).expect("two halves");
        let mut peer = SurfacePeer::of(app, writing);
        let mut session = session;
        peer.send(CollabRequest::RemoteAction {
            message: RemoteActionMessage::CatalogueRequest {
                correlation: 7,
                query: CatalogueQuery::default(),
            },
            press: None,
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
        let (app, writing) = runtime_portal::surface_actions::split(app).expect("two halves");
        let mut peer = SurfacePeer::of(app, writing);
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
                asks: false,
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
                signed: None,
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
        let (app, writing) = runtime_portal::surface_actions::split(app).expect("two halves");
        let mut peer = SurfacePeer::of(app, writing);
        let cluster = uuid::Uuid::new_v4();
        peer.send(CollabRequest::Hello {
            app_id: "so.tilo.topbar".into(),
            cluster_id: cluster,
            record: None,
            resource_type: None,
            handle: None,
        })
        .expect("a surface joins");
        let joined = peer.take_primary_route_result().expect("acknowledged");
        assert_eq!(joined.expect("no refusal").app_id, "so.tilo.topbar");
    }
}
