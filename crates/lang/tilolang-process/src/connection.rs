//! An instance's connection to its workspace: the broker connection the session opened for the
//! app's launch, and the device's store reached over it.
//!
//! Every resource the app keeps is in the workspace's one store, which the broker answers for:
//! the instance sends each read and write as a resource command on its connection and waits for
//! the answer ([`CallPort`]), as the runtime asks for each read before it goes on. Everything
//! else the connection delivers meanwhile (an invocation, presence, the member list) is kept for
//! the host's next frame ([`Connection::poll`]), in the order it came.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use collaboration::{CollabEvent, CollabRequest, CollaborationManager, RouteSpec};
use tilo_resource_store::Port;
use tilo_resource_store::wire::{Command, Reply};

use crate::wait::Wake;

/// The route an instance joins, as a host that opens a connection of its own names it.
pub use collaboration::RouteSpec as Route;

/// How long a resource command waits for the store's answer before the read or write fails.
const ANSWER_WAIT: Duration = Duration::from_secs(30);

/// The instance's broker connection, shared by its frames and its store.
pub struct Connection {
    manager: Rc<RefCell<CollaborationManager>>,
    /// What arrived while a resource command waited, for the next frame.
    held: Rc<RefCell<VecDeque<CollabEvent>>>,
    route: RouteSpec,
    /// Whether there is no broker: an instance on this device alone.
    local: bool,
}

impl Connection {
    /// The connection `manager`, joined on its primary route.
    ///
    /// # Errors
    /// When the connection joined no route.
    pub fn new(manager: CollaborationManager) -> Result<Self, String> {
        let route = manager
            .primary_route()
            .map(|joined| joined.route().clone())
            .ok_or_else(|| "the connection joined no route".to_owned())?;
        Ok(Self {
            manager: Rc::new(RefCell::new(manager)),
            held: Rc::default(),
            route,
            local: false,
        })
    }

    /// A connection to no broker: an instance whose workspace is on this device alone.
    #[must_use]
    pub fn local(route: RouteSpec) -> Self {
        Self {
            manager: Rc::new(RefCell::new(CollaborationManager::disabled())),
            held: Rc::default(),
            route,
            local: true,
        }
    }

    /// Whether there is no broker.
    #[must_use]
    pub const fn is_local(&self) -> bool {
        self.local
    }

    /// The route the connection joined.
    #[must_use]
    pub fn route(&self) -> &RouteSpec {
        &self.route
    }

    /// The connection's manager, to send on.
    pub fn manager(&self) -> std::cell::RefMut<'_, CollaborationManager> {
        self.manager.borrow_mut()
    }

    /// The shared manager, for a holder that sends later than this frame's borrow: the runtime's
    /// invoker building a child call.
    #[must_use]
    pub fn shared_manager(&self) -> Rc<RefCell<CollaborationManager>> {
        Rc::clone(&self.manager)
    }

    /// What the connection delivered since the last poll: first what arrived while a resource
    /// command waited, then what is ready now.
    pub fn poll(&self) -> Vec<CollabEvent> {
        let mut events: Vec<CollabEvent> = self.held.borrow_mut().drain(..).collect();
        events.extend(self.manager.borrow_mut().poll());
        events
    }

    /// The port the store sends its commands on. `arrived` is written whenever the connection
    /// has something new; without it the port looks again every millisecond.
    #[must_use]
    pub fn port(&self, arrived: Option<Rc<Wake>>) -> CallPort {
        CallPort {
            manager: Rc::clone(&self.manager),
            held: Rc::clone(&self.held),
            arrived,
            next: Cell::new(1),
        }
    }
}

/// The store's commands, sent on the instance's connection; see the module docs.
pub struct CallPort {
    manager: Rc<RefCell<CollaborationManager>>,
    held: Rc<RefCell<VecDeque<CollabEvent>>>,
    /// Written whenever the connection has something new: what the port waits on for its
    /// answer. The host's own loop is woken beside it, so taking it here takes nothing from it.
    arrived: Option<Rc<Wake>>,
    next: Cell<u64>,
}

impl CallPort {
    /// Wait until the connection may have delivered something, or `until`.
    fn wait(&self, until: Instant) {
        let Some(arrived) = &self.arrived else {
            std::thread::park_timeout(Duration::from_millis(1));
            return;
        };
        arrived.wait_for(until.saturating_duration_since(Instant::now()));
        let _ = arrived.consume();
    }
}

impl Port for CallPort {
    fn command(&self, command: Command) -> Result<Reply, String> {
        let request_id = self.next.get();
        self.next.set(request_id.wrapping_add(1));
        self.manager
            .borrow_mut()
            .try_send(CollabRequest::ResourceCommand {
                request_id,
                command: serde_json::to_vec(&command).map_err(|error| error.to_string())?,
            })
            .map_err(|error| format!("the workspace's store cannot be reached: {error:?}"))?;
        let until = Instant::now()
            .checked_add(ANSWER_WAIT)
            .unwrap_or_else(Instant::now);
        loop {
            let mut answer = None;
            for event in self.manager.borrow_mut().poll() {
                match event {
                    CollabEvent::ResourceCommandResult {
                        request_id: answered,
                        result,
                    } if answered == request_id => answer = Some(result),
                    other => self.held.borrow_mut().push_back(other),
                }
            }
            if let Some(answer) = answer {
                return serde_json::from_slice(&answer?).map_err(|error| error.to_string());
            }
            if Instant::now() >= until {
                return Err("the workspace's store did not answer".to_owned());
            }
            self.wait(until);
        }
    }
}
