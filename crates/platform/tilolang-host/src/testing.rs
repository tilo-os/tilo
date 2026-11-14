//! The broker's end of an instance's connections, for the host's tests and examples: a fake in
//! place of the session's broker that admits the routes a connection joins, keeps what it sends
//! where the test reads it, and relays a change to the connections joined on its route as the
//! broker does.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use collaboration::commit_log::CommitHistory;
use collaboration::record::RecordKind;
use collaboration::{
    ActionDirectoryRequest, CollabEvent, CollabRequest, DocumentMutation, PeerTransport, RouteSpec,
};

/// What a connection sent on its own route, in the order it sent it.
pub type Sent = Rc<RefCell<Vec<CollabRequest>>>;

/// What the test delivers to a connection.
pub type Inbox = Rc<RefCell<VecDeque<CollabEvent>>>;

/// What a connection sent on a route other than its own, and on which route.
pub type Routed = Rc<RefCell<Vec<(RouteSpec, CollabRequest)>>>;

/// The commits a connection made, as a peer's log keeps them: what one undo takes back across
/// the workspace.
pub type Actions = Rc<RefCell<Vec<CommitHistory>>>;

/// The instance's action directory as the broker holds it: the apps it was offered under, and
/// what the test asks of it.
#[derive(Default)]
pub struct Directory {
    pub offered: Vec<String>,
    pub asked: Vec<ActionDirectoryRequest>,
}

/// The connections of one workspace, as the broker holds them: a change sent on a route reaches
/// the other connections joined on that route.
#[derive(Default)]
pub struct Bus {
    joined: RefCell<Vec<(RouteSpec, String, Inbox)>>,
}

impl Bus {
    fn join(&self, route: RouteSpec, person: String, inbox: Inbox) {
        let mut joined = self.joined.borrow_mut();
        if joined
            .iter()
            .any(|(held, _, there)| *held == route && Rc::ptr_eq(there, &inbox))
        {
            return;
        }
        joined.push((route, person, inbox));
    }

    /// Delivers `mutation`, which `from` sent on `route` as `person`, to the other connections
    /// joined on that route.
    fn relay(
        &self,
        route: &RouteSpec,
        from: &Inbox,
        person: &str,
        mutation: &DocumentMutation,
    ) -> Result<(), String> {
        let delivery = delivered(route, person, mutation)?;
        for (joined, _, inbox) in self.joined.borrow().iter() {
            if joined == route && !Rc::ptr_eq(inbox, from) {
                inbox.borrow_mut().push_back(delivery.clone());
            }
        }
        Ok(())
    }
}

/// `mutation`, sent by `person` on `route`, as the document envelope the broker delivers to the
/// route's other replicas.
///
/// # Errors
///
/// Returns the error a mutation too large to carry encodes to, as the connection reports it.
pub fn delivered(
    route: &RouteSpec,
    person: &str,
    mutation: &DocumentMutation,
) -> Result<CollabEvent, String> {
    let record = route.record.as_deref().unwrap_or("");
    let workspace = route.cluster_id.to_string();
    let mut envelope = collab_types::SignedOp::new_unsigned(
        mutation.encode().map_err(|error| error.to_string())?,
        collab_types::document_artifact_id(&route.app_id, &workspace, record),
        collab_types::Ed25519PublicKey::from_bytes([0; 32]),
        collab_types::document_attestation_id(&route.app_id, &workspace, record),
        collab_types::OpKind::Change,
        std::time::SystemTime::UNIX_EPOCH,
        0,
    );
    envelope.signature = collab_types::Signature::from_bytes([0; 64]);
    Ok(CollabEvent::Document {
        envelope: Box::new(envelope),
        from_user_id: person.to_owned(),
        decrypted_payload: None,
    })
}

/// The broker's end of one connection: the route it joins, what it sends, what the test delivers
/// to it, and the bus its changes travel on.
pub struct Broker {
    route: RouteSpec,
    person: String,
    bus: Rc<Bus>,
    sent: Sent,
    inbox: Inbox,
    routed: Routed,
    actions: Actions,
    directory: Rc<RefCell<Directory>>,
    routes: Rc<RefCell<Vec<RouteSpec>>>,
    joined: Option<RouteSpec>,
}

impl Broker {
    /// A connection to `route`, on a bus of its own: nothing it sends is relayed.
    #[must_use]
    pub fn new(route: RouteSpec) -> Self {
        Self::on(&Rc::default(), route, "")
    }

    /// A connection to `route` on `bus`, whose changes travel as `person`'s: what it sends on a
    /// route reaches the bus's other connections joined on that route.
    #[must_use]
    pub fn on(bus: &Rc<Bus>, route: RouteSpec, person: &str) -> Self {
        Self {
            route,
            person: person.to_owned(),
            bus: Rc::clone(bus),
            sent: Sent::default(),
            inbox: Inbox::default(),
            routed: Routed::default(),
            actions: Actions::default(),
            directory: Rc::default(),
            routes: Rc::default(),
            joined: None,
        }
    }

    /// Offers and takes the action directory's requests through `directory`, as the test holds
    /// it.
    #[must_use]
    pub fn with_directory(mut self, directory: Rc<RefCell<Directory>>) -> Self {
        self.directory = directory;
        self
    }

    /// Keeps the commits it learns in `actions`, as the test holds them.
    #[must_use]
    pub fn with_actions(mut self, actions: Actions) -> Self {
        self.actions = actions;
        self
    }

    /// What the connection sends, as the test reads it.
    #[must_use]
    pub fn sent(&self) -> Sent {
        Rc::clone(&self.sent)
    }

    /// Where the test delivers what the broker sends the connection.
    #[must_use]
    pub fn inbox(&self) -> Inbox {
        Rc::clone(&self.inbox)
    }

    /// What the connection sends on routes other than its own.
    #[must_use]
    pub fn routed(&self) -> Routed {
        Rc::clone(&self.routed)
    }

    /// The records every app of the workspace opens beside its own, as the connection joined
    /// them.
    #[must_use]
    pub fn routes(&self) -> Rc<RefCell<Vec<RouteSpec>>> {
        Rc::clone(&self.routes)
    }
}

impl PeerTransport for Broker {
    fn is_connected(&self) -> bool {
        true
    }

    fn send(&mut self, request: CollabRequest) -> Result<(), String> {
        match &request {
            CollabRequest::Hello { .. } => {
                self.joined = Some(self.route.clone());
                self.bus.join(
                    self.route.clone(),
                    self.person.clone(),
                    Rc::clone(&self.inbox),
                );
            }
            CollabRequest::Document {
                mutation, commit, ..
            } => {
                if let Some(commit) = commit {
                    self.actions.borrow_mut().push(CommitHistory::of(commit));
                }
                self.bus
                    .relay(&self.route, &self.inbox, &self.person, mutation)?;
            }
            _ => {}
        }
        self.sent.borrow_mut().push(request);
        Ok(())
    }

    fn poll(&mut self) -> Vec<CollabEvent> {
        self.inbox.borrow_mut().drain(..).collect()
    }

    fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
        self.joined.take().map(Ok)
    }

    /// Admits every route the connection joins: the records every app of the workspace opens
    /// beside its own (its chunks, each person's own state) travel on routes of their own, and a
    /// broker that refused the join would close the instance's whole connection.
    fn join_route(&mut self, route: RouteSpec, _: RecordKind) -> Result<(), String> {
        self.bus
            .join(route.clone(), self.person.clone(), Rc::clone(&self.inbox));
        self.routes.borrow_mut().push(route);
        Ok(())
    }

    /// Sends on a route of the connection's own, as the broker carries it to the other
    /// connections joined there.
    fn send_to_route(&mut self, route: RouteSpec, request: CollabRequest) -> Result<(), String> {
        if let CollabRequest::Document { mutation, .. } = &request {
            self.bus
                .relay(&route, &self.inbox, &self.person, mutation)?;
        }
        self.routed.borrow_mut().push((route, request));
        Ok(())
    }

    fn publish_action_directory(&mut self, app_id: String) -> Result<(), String> {
        self.directory.borrow_mut().offered.push(app_id);
        Ok(())
    }

    fn take_action_directory_requests(&mut self) -> Vec<ActionDirectoryRequest> {
        std::mem::take(&mut self.directory.borrow_mut().asked)
    }
}
