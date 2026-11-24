//! The broker's end of an instance's connections, for the host's tests and examples: a fake in
//! place of the session's broker that admits the routes a connection joins, keeps what it sends
//! where the test reads it, answers its resource commands from the workspace's store, relays a
//! presence frame to the connections joined on its route, hands each connection the
//! workspace's member list, and answers reads of a device's records (its engine jobs) as the
//! record API does, a held read once the record moves, as the broker does.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

use collab_types::document::ResourceType;
use collaboration::{
    ActionDirectoryRequest, CollabEvent, CollabRequest, DocumentMutation, PeerTransport, RouteSpec,
};
use tilo_resource_store::Port;
use tilo_resource_store::Store;
use tilo_resource_store::wire::{self, Command, Reply};

/// What a connection sent on its own route, in the order it sent it.
pub type Sent = Rc<RefCell<Vec<CollabRequest>>>;

/// What the test delivers to a connection.
pub type Inbox = Rc<RefCell<VecDeque<CollabEvent>>>;

/// What a connection sent on a route other than its own, and on which route.
pub type Routed = Rc<RefCell<Vec<(RouteSpec, CollabRequest)>>>;

/// A workspace's store, as the broker answers the resource commands of every connection joined
/// there: one of it per workspace, held in memory, with its app's whole documents beside it.
#[derive(Default)]
pub struct Workspace {
    store: RefCell<Store>,
    /// The once-only actions a connection ran and kept done, by address and inputs.
    done: RefCell<Vec<((String, serde_json::Value), serde_json::Value)>>,
    /// The stamps of the writes the store took, oldest first.
    written: RefCell<Vec<tilo_resource_store::Stamp>>,
    /// The app's whole documents: what the connection's `#[resource]` documents open from.
    documents: tilo_resource_store::LocalPort,
}

impl Workspace {
    /// Carries `command` out, as the workspace's store does for a connection of the person
    /// `reader`: a read answers only what that person reads.
    ///
    /// # Errors
    /// Why the store refused the write.
    pub fn command(&self, command: Command, reader: &str) -> Result<Reply, String> {
        if let Command::Turn {
            address,
            inputs,
            answer,
        } = command
        {
            let key = (address, inputs);
            let ran = self
                .done
                .borrow()
                .iter()
                .find(|(done, _)| *done == key)
                .map(|(_, answer)| answer.clone());
            return Ok(Reply::Turn(match (ran, answer) {
                (Some(ran), _) => wire::Turn::Done(ran),
                (None, Some(answer)) => {
                    self.done.borrow_mut().push((key, answer.clone()));
                    wire::Turn::Done(answer)
                }
                (None, None) => wire::Turn::Take,
            }));
        }
        match command {
            // The app's records, as the workspace keeps them.
            command @ (Command::RecordChanges { .. }
            | Command::RecordChange { .. }
            | Command::Records) => self.documents.command(command),
            command if command.writes() => {
                let stamps = stamps_of(&command);
                match wire::write(&mut self.store.borrow_mut(), command) {
                    Ok(_) => {
                        self.written.borrow_mut().extend(stamps);
                        Ok(Reply::Written)
                    }
                    Err(refused) => Err(refused.to_string()),
                }
            }
            command => {
                let reader = reader.to_ascii_lowercase();
                wire::read(&self.store.borrow(), &command, &wire::reads_as(&reader))
                    .ok_or_else(|| "the store answered no read".to_owned())
            }
        }
    }

    /// The stamps of the writes the store took, oldest first: who wrote each and the
    /// invocation it served.
    #[must_use]
    pub fn written(&self) -> Vec<tilo_resource_store::Stamp> {
        self.written.borrow().clone()
    }

    /// The documents the workspace's store holds.
    #[must_use]
    pub fn documents(&self) -> Vec<String> {
        self.store
            .borrow()
            .index()
            .documents()
            .map(str::to_owned)
            .collect()
    }

    /// The records `command` moves: each resource it writes, by the document it lives in. A
    /// write that names no resource (an undo) may move every record the store holds.
    #[must_use]
    pub fn records_written(&self, command: &Command) -> Vec<String> {
        fn collect(command: &Command, records: &mut Vec<String>) -> bool {
            match command {
                Command::Put { id, .. } | Command::Remove { id, .. } | Command::Edit { id, .. } => {
                    let doc = tilo_resource_store::doc_of(id);
                    if !records.iter().any(|known| known == doc) {
                        records.push(doc.to_owned());
                    }
                    true
                }
                Command::Batch(commands) => {
                    commands.iter().all(|command| collect(command, records))
                }
                Command::Undo { .. } => false,
                _ => true,
            }
        }
        let mut records = Vec::new();
        if collect(command, &mut records) {
            records
        } else {
            self.documents()
        }
    }
}

/// The stamps `command` writes under, each write of a batch its own.
fn stamps_of(command: &Command) -> Vec<tilo_resource_store::Stamp> {
    match command {
        Command::Put { stamp, .. }
        | Command::Remove { stamp, .. }
        | Command::Undo { stamp, .. }
        | Command::Edit { stamp, .. } => vec![stamp.clone()],
        Command::Batch(commands) => commands.iter().flat_map(stamps_of).collect(),
        _ => Vec::new(),
    }
}

/// The instance's action directory as the broker holds it: the apps it was offered under, and
/// what the test asks of it.
#[derive(Default)]
pub struct Directory {
    pub offered: Vec<String>,
    pub asked: Vec<ActionDirectoryRequest>,
}

/// A device record: the space it is in (`devices/<device>`), its app and its id.
type DeviceRecord = (String, String, String);

/// A read of a device record held until it moves: where it is answered, its correlation, the
/// record and the revision the reader read.
type HeldRead = (Inbox, u64, DeviceRecord, Option<String>);

/// The connections of one workspace, as the broker holds them: the workspace's store every
/// connection reads and writes, and the workspace's members, people and software, with their
/// names, which every connection is handed when it joins and whenever the list changes.
#[derive(Default)]
pub struct Bus {
    joined: RefCell<Vec<(RouteSpec, String, Inbox)>>,
    /// The workspace's members, people and software.
    members: RefCell<Vec<MemberRow>>,
    /// The workspace's one store.
    workspace: Rc<Workspace>,
    /// The device records the test publishes (`devices/<device>`, its app, its record), each as
    /// it stands and the revision it stands at.
    device_records: RefCell<BTreeMap<DeviceRecord, (serde_json::Value, u64)>>,
    /// The reads of a device record held until it moves past what the reader read: where each is
    /// answered, its correlation, the record and the revision it read.
    held_reads: RefCell<Vec<HeldRead>>,
    /// What a reader's loop is woken by once its read of a device record is answered, as a
    /// connection wakes it when an answer arrives.
    answered: RefCell<Option<tilolang_tilo::background::Waker>>,
}

/// One member of a test's workspace: who, what they are called, and the picture they set.
struct MemberRow {
    who: collaboration::MemberId,
    name: String,
    avatar: Option<collab_types::AvatarBytes>,
}

impl Bus {
    /// The workspace's store.
    #[must_use]
    pub fn workspace(&self) -> Rc<Workspace> {
        Rc::clone(&self.workspace)
    }

    /// The device record `record` of `app` in `space` (`devices/<device>`) is `value` now, as the
    /// device publishes it: each read held for it is answered.
    pub fn publish_device_record(
        &self,
        space: &str,
        app: &str,
        record: &str,
        value: serde_json::Value,
    ) {
        let key = (space.to_owned(), app.to_owned(), record.to_owned());
        {
            let mut records = self.device_records.borrow_mut();
            let revision = records
                .get(&key)
                .map_or(1, |(_, revision)| revision.saturating_add(1));
            records.insert(key.clone(), (value, revision));
        }
        let held: Vec<_> = {
            let mut reads = self.held_reads.borrow_mut();
            let (due, kept) = std::mem::take(&mut *reads)
                .into_iter()
                .partition(|(_, _, read, _)| *read == key);
            *reads = kept;
            due
        };
        for (inbox, correlation, key, _) in held {
            self.answer_device_read(&inbox, correlation, &key);
        }
    }

    /// Wake the readers' loops with `wake` whenever a read of a device record is answered.
    pub fn wake_on_answers(&self, wake: tilolang_tilo::background::Waker) {
        *self.answered.borrow_mut() = Some(wake);
    }

    /// Read the device record `read` asks for, into `inbox`: held while it stands at the
    /// revision the reader read, when the reader waits.
    fn read_device_record(
        &self,
        inbox: &Inbox,
        correlation: u64,
        read: &tilo_actions::remote::ResourceReadRequest,
    ) {
        let key = (
            read.workspace.clone(),
            read.app.clone(),
            read.record.clone(),
        );
        let stands = self
            .device_records
            .borrow()
            .get(&key)
            .map(|(_, revision)| revision.to_string());
        if read.wait_ms.is_some() && (stands.is_none() || stands == read.after) {
            self.held_reads.borrow_mut().push((
                Rc::clone(inbox),
                correlation,
                key,
                read.after.clone(),
            ));
            return;
        }
        self.answer_device_read(inbox, correlation, &key);
    }

    /// Answer the read `correlation` of the device record `key` into `inbox`, as it stands.
    fn answer_device_read(&self, inbox: &Inbox, correlation: u64, key: &DeviceRecord) {
        let reading = match self.device_records.borrow().get(key) {
            Some((value, revision)) => serde_json::json!({
                "revision": revision.to_string(),
                "records": [{ "app": key.1, "record": key.2, "value": value }],
            }),
            None => serde_json::json!({ "revision": "0", "records": [] }),
        };
        inbox.borrow_mut().push_back(CollabEvent::RemoteAction {
            message: tilo_actions::remote::RemoteActionMessage::ResourceReading {
                correlation,
                result: Ok(reading.to_string()),
            },
        });
        if let Some(wake) = &*self.answered.borrow() {
            wake();
        }
    }

    /// The workspace has the member `who`, named `name`: every connection is told.
    pub fn member(&self, who: collaboration::MemberId, name: &str) {
        self.member_with_avatar(who, name, None);
    }

    /// The workspace has the member `who`, named `name` and pictured as `avatar`, the image's
    /// own bytes: every connection is told.
    pub fn member_with_avatar(
        &self,
        who: collaboration::MemberId,
        name: &str,
        avatar: Option<collab_types::AvatarBytes>,
    ) {
        {
            let mut members = self.members.borrow_mut();
            members.retain(|held| held.who != who);
            members.push(MemberRow {
                who,
                name: name.to_owned(),
                avatar,
            });
        }
        let joined: Vec<(String, Inbox)> = self
            .joined
            .borrow()
            .iter()
            .map(|(_, person, inbox)| (person.clone(), Rc::clone(inbox)))
            .collect();
        let mut told: Vec<Inbox> = Vec::new();
        for (person, inbox) in joined {
            if !told.iter().any(|done| Rc::ptr_eq(done, &inbox)) {
                self.tell_members(&person, &inbox);
                told.push(inbox);
            }
        }
    }

    /// Hands the connection of `person` at `inbox` the member list, its own row marked: nothing
    /// while the workspace has no members.
    fn tell_members(&self, person: &str, inbox: &Inbox) {
        let members = self.members.borrow();
        if members.is_empty() {
            return;
        }
        let members = members
            .iter()
            .map(|row| {
                let mut member = collaboration::Member::named(row.who.clone(), row.name.clone());
                member.avatar.clone_from(&row.avatar);
                member.is_self = row.who.person_id() == Some(person);
                member
            })
            .collect();
        inbox.borrow_mut().push_back(CollabEvent::Context {
            members,
            permissions: Vec::new(),
            apps: Vec::new(),
        });
    }

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

    /// Delivers `mutation`, which `from` sent on `route`, to the other connections joined on that
    /// route.
    fn relay(&self, route: &RouteSpec, from: &Inbox, mutation: &DocumentMutation) {
        let delivery = delivered(route, mutation);
        for (joined, _, inbox) in self.joined.borrow().iter() {
            if joined == route && !Rc::ptr_eq(inbox, from) {
                inbox.borrow_mut().push_back(delivery.clone());
            }
        }
    }

    /// The workspace's store moved, here or in another window of it: every other connection reads
    /// again what it read of the records that moved, as the session tells each window when
    /// another host wrote.
    fn resources_moved(&self, from: &Inbox, records: &[String]) {
        let joined = self.joined.borrow();
        let mut told: Vec<&Inbox> = Vec::new();
        for (_, _, inbox) in joined.iter() {
            if Rc::ptr_eq(inbox, from) || told.iter().any(|told| Rc::ptr_eq(told, inbox)) {
                continue;
            }
            inbox.borrow_mut().push_back(CollabEvent::RecordsMoved {
                records: records.to_vec(),
            });
            told.push(inbox);
        }
    }

    /// Delivers the presence frame `key` and `payload`, which `from` said on `route` as
    /// `person`, to the other connections joined on that route.
    fn present(
        &self,
        route: &RouteSpec,
        from: &Inbox,
        person: &str,
        key: &ui_core::presence::PresenceKey,
        payload: &collaboration::PresencePayload,
    ) {
        for (joined, _, inbox) in self.joined.borrow().iter() {
            if joined == route && !Rc::ptr_eq(inbox, from) {
                inbox
                    .borrow_mut()
                    .push_back(CollabEvent::RemotePresenceUpdate {
                        member: ui_core::permissions::MemberId::person(person),
                        key: key.clone(),
                        payload: payload.clone(),
                    });
            }
        }
    }
}

/// `mutation` sent on `route`, as the device's store delivers the record's changes to the
/// route's other windows.
#[must_use]
pub fn delivered(route: &RouteSpec, mutation: &DocumentMutation) -> CollabEvent {
    CollabEvent::Document {
        route: route.clone(),
        mutation: mutation.clone(),
    }
}

/// The broker's end of one connection: the route it joins, what it sends, what the test delivers
/// to it, the workspace's store its resource commands are answered from, and the bus its members
/// and presence travel on.
pub struct Broker {
    route: RouteSpec,
    person: String,
    bus: Rc<Bus>,
    sent: Sent,
    inbox: Inbox,
    routed: Routed,
    directory: Rc<RefCell<Directory>>,
    routes: Rc<RefCell<Vec<RouteSpec>>>,
    joined: Option<RouteSpec>,
}

impl Broker {
    /// A connection to `route`, on a bus of its own: nothing it sends is relayed, and the
    /// workspace's store is that bus's alone.
    #[must_use]
    pub fn new(route: RouteSpec) -> Self {
        Self::on(&Rc::default(), route, "")
    }

    /// A connection to `route` on `bus`, whose changes travel as `person`'s: what it sends on a
    /// route reaches the bus's other connections joined on that route, and its resource
    /// commands are answered from the bus's workspace's store.
    #[must_use]
    pub fn on(bus: &Rc<Bus>, route: RouteSpec, person: &str) -> Self {
        Self {
            route,
            person: person.to_owned(),
            bus: Rc::clone(bus),
            sent: Sent::default(),
            inbox: Inbox::default(),
            routed: Routed::default(),
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

    /// The workspace's store, as the connection's resource commands are answered from.
    #[must_use]
    pub fn workspace(&self) -> Rc<Workspace> {
        self.bus.workspace()
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

    /// The documents every app of the workspace opens beside its own, as the connection joined
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
                self.bus.tell_members(&self.person, &self.inbox);
            }
            CollabRequest::Document { mutation, .. } => {
                self.bus.relay(&self.route, &self.inbox, mutation);
            }
            CollabRequest::ResourceCommand {
                request_id,
                command,
            } => {
                let parsed = serde_json::from_slice(command).map_err(|error| error.to_string());
                let writes = parsed.as_ref().is_ok_and(Command::writes);
                let records = parsed
                    .as_ref()
                    .ok()
                    .map(|command| self.bus.workspace.records_written(command))
                    .unwrap_or_default();
                let reply =
                    parsed.and_then(|command| self.bus.workspace.command(command, &self.person));
                let result = reply.and_then(|reply| {
                    serde_json::to_vec(&reply).map_err(|error| error.to_string())
                });
                // The workspace's store is one, every connection of it: what one wrote, the
                // others read again.
                if writes {
                    self.bus.resources_moved(&self.inbox, &records);
                }
                self.inbox
                    .borrow_mut()
                    .push_back(CollabEvent::ResourceCommandResult {
                        request_id: *request_id,
                        result,
                    });
            }
            CollabRequest::Say { key, payload } => {
                if let Ok(Some(payload)) =
                    collaboration::PresencePayload::from_live(payload.clone())
                {
                    self.bus
                        .present(&self.route, &self.inbox, &self.person, key, &payload);
                }
            }
            CollabRequest::RemoteAction {
                message:
                    tilo_actions::remote::RemoteActionMessage::ResourceRead {
                        correlation, read, ..
                    },
                ..
            } if read.workspace.starts_with("devices/") => {
                self.bus.read_device_record(&self.inbox, *correlation, read);
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

    /// Admits every route the connection joins: the documents every app of the workspace opens
    /// beside its own (its chunks, each person's own state) travel on routes of their own, and a
    /// broker that refused the join would close the instance's whole connection.
    fn join_route(&mut self, route: RouteSpec, _: ResourceType) -> Result<(), String> {
        self.bus
            .join(route.clone(), self.person.clone(), Rc::clone(&self.inbox));
        self.routes.borrow_mut().push(route);
        Ok(())
    }

    /// Sends on a route of the connection's own, as the broker carries it to the other
    /// connections joined there.
    fn send_to_route(&mut self, route: RouteSpec, request: CollabRequest) -> Result<(), String> {
        if let CollabRequest::Document { mutation, .. } = &request {
            self.bus.relay(&route, &self.inbox, mutation);
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
