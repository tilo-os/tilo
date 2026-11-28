//! The broker's end of an instance's connections, for the host's tests and examples: a fake in
//! place of the session's broker that admits the routes a connection joins, keeps what it sends
//! where the test reads it, answers its resource commands from the workspace's store, plays the
//! device's live presence for it (a word one connection's person says is heard by the others,
//! as the device hands the workspace's words to its other apps), hands each connection the
//! workspace's member list, and answers reads of a device's records (its engine jobs) as the
//! record API does, a waiting read once the record moves, as the device does. It answers what each
//! person's window asks of the actions it shows as their device does: as the workspace's owner
//! unless the test gave them another access ([`Bus::set_access`]), an ask waiting until that moves.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

use tilolang_tilo::bundle::Bundle;

use crate::hosted::Spec;

use collab_types::document::ResourceType;
use collab_types::store::wire::{Command, Reply, Turn};
use collaboration::{
    ActionDirectoryRequest, CollabEvent, CollabRequest, DocumentMutation, PeerTransport, RouteSpec,
};
use tilo_resource_store::{Port, Store, wire};

/// What a connection sent on its own route, in the order it sent it.
pub type Sent = Rc<RefCell<Vec<CollabRequest>>>;

/// What the test delivers to a connection.
pub type Inbox = Rc<RefCell<VecDeque<CollabEvent>>>;

/// A workspace's store, as the broker answers the resource commands of every connection joined
/// there: one of it per workspace, held in memory, with its app's whole documents beside it.
pub struct Workspace {
    store: RefCell<Store>,
    /// The once-only actions a connection ran and kept done, by address and inputs.
    done: RefCell<Vec<((String, serde_json::Value), serde_json::Value)>>,
    /// The stamps of the writes the store took, oldest first.
    written: RefCell<Vec<tilo_resource_store::Stamp>>,
    /// The app's whole documents: what the connection's `#[resource]` documents open from.
    documents: tilo_resource_store::LocalPort,
    /// The workspace's members' resource, as its governance would answer it.
    members: RefCell<Option<serde_json::Value>>,
    /// Whether the members' record has yet to arrive: the workspace's governance publishes it,
    /// and a test that has it come late answers no members until it is released.
    members_held: std::cell::Cell<bool>,
    /// How many surfaces this process had named when the store last declared them.
    declared: std::cell::Cell<Option<u64>>,
}

impl Default for Workspace {
    fn default() -> Self {
        let mut store = Store::new(Self::ID);
        // Who reads a record whose type names its readers by a field is the workspace's one
        // decision, as a device's workspace makes it: the test's holds no grants that narrow it.
        store.set_record_reader(std::sync::Arc::new(MembersReadAsThemselves));
        Self {
            store: RefCell::new(store),
            done: RefCell::default(),
            written: RefCell::default(),
            documents: tilo_resource_store::LocalPort::default(),
            members: RefCell::default(),
            members_held: std::cell::Cell::default(),
            declared: std::cell::Cell::default(),
        }
    }
}

/// Who reads a record whose type names its readers by a field, in the test's workspace: a person
/// as themselves, an agent as the person it acts for, and software the workspace made no member
/// of as nobody, as a workspace with no grant that narrows it decides.
struct MembersReadAsThemselves;

impl tilo_resource_store::RecordReader for MembersReadAsThemselves {
    fn reader_of(
        &self,
        actor: &tilo_actions::Actor,
        _kind: &str,
        _id: &str,
    ) -> Option<tilo_actions::PersonId> {
        match actor.member_id()? {
            tilo_actions::MemberId::Person(person)
            | tilo_actions::MemberId::Agent { person, .. } => Some(person.clone()),
            tilo_actions::MemberId::Bot(_) => None,
        }
    }
}

/// Who a write of the test's store is made by, as the workspace takes a write from a device that
/// named no author of its own: one author for every write the test's workspace takes.
const AUTHOR: [u8; 32] = [7; 32];

impl Workspace {
    /// The workspace the test's store keeps the resources of, which their foundings are derived
    /// within.
    pub const ID: &str = "workspace";

    /// Carries `command` out, as the workspace's store does for a connection of the person
    /// `reader`: a read answers only what that person reads.
    ///
    /// # Errors
    /// Why the store refused the write.
    pub fn command(&self, command: Command, reader: &str) -> Result<Reply, String> {
        self.declare_surfaces();
        // Who is in the workspace is its governance's to say, as the workspace service answers
        // it: the members' resource is this test's own list, never a record in the store.
        if let Command::Content { kind, id } = &command
            && kind == tilo_resource_store::WORKSPACE_MEMBERS_KIND
            && id == tilo_resource_store::WORKSPACE_MEMBERS_ID
        {
            if self.members_held.get() {
                return Ok(Reply::Content(None));
            }
            return Ok(Reply::Content(self.members.borrow().clone()));
        }
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
                (Some(ran), _) => Turn::Done(ran),
                (None, Some(answer)) => {
                    self.done.borrow_mut().push((key, answer.clone()));
                    Turn::Done(answer)
                }
                (None, None) => Turn::Take,
            }));
        }
        match command {
            // The app's records, as the workspace keeps them.
            command @ (Command::RecordChanges { .. }
            | Command::RecordChange { .. }
            | Command::Records) => self.documents.command(command),
            // The caller's last steps still in effect, from the documents' change graph: a read
            // that needs the store itself, so it does not go through `wire::read`, as the
            // workspace service answers it too.
            Command::Steps {
                steps,
                invoker,
                kinds,
                redo,
            } => Ok(wire::steps(
                &mut self.store.borrow_mut(),
                steps,
                &invoker,
                &kinds,
                redo,
            )),
            command if command.writes() => {
                let stamps = stamps_of(&command);
                let mut store = self.store.borrow_mut();
                // Every write is judged as the workspace's store judges one: the types and verbs
                // the apps named, and the readers their types give.
                let draft = tilo_resource_store::wire::draft(&mut store, command.clone())
                    .map_err(|refused| refused.to_string())?;
                store
                    .check_batch(&draft, AUTHOR)
                    .map_err(|refusal| format!("{refusal:?}"))?;
                match wire::write(&mut store, command) {
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

    /// Declares, in the workspace's store, every app surface this process names, as a device's
    /// workspace service declares those it knows before it places a write: the types, verbs and
    /// reader rules a write is placed and judged by. A loaded package names its surface before
    /// its first write.
    fn declare_surfaces(&self) {
        let named = collab_types::app_surface::registered();
        if self.declared.get() == Some(named) {
            return;
        }
        self.declared.set(Some(named));
        let mut store = self.store.borrow_mut();
        if store
            .schema()
            .get(tilo_resource_store::reference::KIND)
            .is_none()
        {
            let _ = store.declare(tilo_resource_store::reference::schema());
        }
        for (app, surface) in collab_types::app_surface::Surfaces::process().resources() {
            let _ = store.declare(tilo_resource_store::Schema::of_app(&app, &surface));
        }
        // The host package's types, as a session's workspace declares them: what an app's actions
        // write when one runs a block or waits for its person (`tl.tilo.host.Invocation`).
        let host = tilolang_tilo::tilolang::host_meta();
        for surface in
            tilolang_tilo::surface::package_surfaces(&host, &tilolang_tilo::runtime::SourceWords)
        {
            let _ = store.declare(tilo_resource_store::Schema::of_app(&host.app, &surface));
        }
    }

    /// The workspace's members' resource, as its governance has them now: what the store answers
    /// a read of it with.
    pub fn set_members(&self, members: serde_json::Value) {
        *self.members.borrow_mut() = Some(members);
    }

    /// Hold the workspace's members' record back: until it is released, a read of it answers no
    /// record, as one whose governance has not published it yet.
    pub fn hold_members(&self) {
        self.members_held.set(true);
    }

    /// Let the workspace's members' record come: its connections are told it moved.
    pub fn release_members(&self) {
        self.members_held.set(false);
    }

    /// Whether the workspace's members' record is held back.
    #[must_use]
    pub fn members_held(&self) -> bool {
        self.members_held.get()
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

    /// The document the record `id` of `kind` is held in, as the store keeps it: what a window's
    /// history lists the record's changes from, before they are filtered to its reader.
    #[must_use]
    pub fn document_of(&self, kind: &str, id: &str) -> Option<automerge::AutoCommit> {
        let store = self.store.borrow();
        let doc = store.index().get(kind, id)?.doc.clone();
        automerge::AutoCommit::load(&store.document(&doc)?.save()).ok()
    }

    /// The records `command` moves, as the device's broker tells the other windows: the
    /// resources it moves ([`Command::moves`]), by the document each lives in. A write that names
    /// no resource (an undo) may move every record the store holds.
    #[must_use]
    pub fn records_written(&self, command: &Command) -> Vec<collab_types::store::MovedRecord> {
        let Some(items) = command.moves() else {
            return self
                .documents()
                .into_iter()
                .map(collab_types::store::MovedRecord::whole)
                .collect();
        };
        let mut records: BTreeMap<String, Vec<collab_types::store::MovedItem>> = BTreeMap::new();
        for item in items {
            records
                .entry(tilo_resource_store::doc_of(&item.id).to_owned())
                .or_default()
                .push(item);
        }
        records
            .into_iter()
            .map(|(record, items)| collab_types::store::MovedRecord {
                record,
                items: Some(items),
            })
            .collect()
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
/// names, which the store answers as the workspace's members' resource and every connection is
/// told moved when the list changes.
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
    /// What the device's live presence heard for the connections joined here: the words the
    /// other connections' people said, each with the connection it waits for.
    presence: RefCell<Vec<(Inbox, collab_types::presence::Heard)>>,
    /// Each person's device, by person: what it says of what their windows show.
    devices: RefCell<BTreeMap<String, tilo_ui_model::harness_device::HarnessDevice>>,
    /// The asks of what a window shows, waiting until its device says something else: the person,
    /// where it is answered, its correlation and what it asked.
    waiting_shown: RefCell<Vec<WaitingShown>>,
    /// The workspace's policy, once a test set one ([`Self::set_access`]): the members' reading
    /// then names only the people it gives a role and the software it admitted, as a device's does.
    policy: RefCell<Option<tilo_action_policy::WorkspacePolicy>>,
}

/// An ask of what a window shows, waiting for its device to say something else: the person,
/// where it is answered, its correlation and what it asked.
type WaitingShown = (String, Inbox, u64, Vec<tilo_actions::shown::ShownAction>);

/// Whether `policy` makes `who` a member: a person it gives a role and has not blocked, an agent
/// a grant admitted (`agent-member:…`), or other software, which acts on the grants it was given.
fn member_of(policy: &tilo_action_policy::WorkspacePolicy, who: &collaboration::MemberId) -> bool {
    if let Some(person) = who.as_person() {
        return policy.role_of(person.as_str()).is_some() && !policy.is_blocked(person.as_str());
    }
    match who.agent_id() {
        Some(agent) => policy.grants.iter().any(|grant| {
            grant.id.0.starts_with("agent-member:") && grant.grantee.agent_id() == Some(agent)
        }),
        None => true,
    }
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

    /// Hold the workspace's members' record back until [`Self::release_members`]: a window
    /// opened meanwhile reads no members, as one whose governance has not published them yet.
    pub fn hold_members(&self) {
        self.workspace.hold_members();
    }

    /// Let the workspace's members' record come: every connection is told it moved.
    pub fn release_members(&self) {
        self.workspace.release_members();
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

    /// `person`'s device decides what their windows show by `access` from now on, as when the
    /// workspace's grants change: each ask waiting on them is answered.
    pub fn set_access(
        &self,
        route: &RouteSpec,
        person: &str,
        access: tilo_action_policy::MemberAccess,
    ) {
        *self.policy.borrow_mut() = Some((*access.policy).clone());
        self.device(route, person).set_access(access);
        let waiting: Vec<_> = {
            let mut asks = self.waiting_shown.borrow_mut();
            let (theirs, others) = asks.drain(..).partition(|(of, ..)| of == person);
            *asks = others;
            theirs
        };
        for (_, inbox, correlation, actions) in waiting {
            let decisions = self.device(route, person).decide(&actions);
            inbox.borrow_mut().push_back(CollabEvent::RemoteAction {
                message: tilo_actions::remote::RemoteActionMessage::ShownActionsResponse {
                    correlation,
                    result: Ok(decisions),
                },
            });
        }
        if let Some(wake) = &*self.answered.borrow() {
            wake();
        }
    }

    /// `person`'s device in the workspace of `route`: its owner until the test says otherwise.
    fn device(
        &self,
        route: &RouteSpec,
        person: &str,
    ) -> tilo_ui_model::harness_device::HarnessDevice {
        self.devices
            .borrow_mut()
            .entry(person.to_owned())
            .or_insert_with(|| {
                tilo_ui_model::harness_device::HarnessDevice::member_of(
                    &route.cluster_id.to_string(),
                    &collaboration::MemberId::person(person),
                    tilo_action_policy::Role::Owner,
                )
            })
            .clone()
    }

    /// `spec` as `spec`'s person's device hands it to a window of `bundle`'s app of `route`: with
    /// what it says of the actions the app declares, and of anything of each app it uses, so the
    /// window's first frame acts on them.
    #[must_use]
    pub fn launched(&self, route: &RouteSpec, mut spec: Spec, bundle: &Bundle) -> Spec {
        let resources = tilolang_tilo::surface::package_surfaces(
            &bundle.meta,
            &tilolang_tilo::runtime::SourceWords,
        );
        let mut actions = tilo_actions::shown::declared_by(&bundle.meta.app, &resources);
        for import in &bundle.imports {
            actions.extend(tilo_actions::shown::declared_by(&import.meta.app, &[]));
        }
        spec.verdicts = self.device(route, &spec.person).decide(&actions);
        spec
    }

    /// Answer what `person`'s window asked of the actions it shows, as their device does: at
    /// once, unless it says what the window was last told and the window waits for a change.
    fn shown(
        &self,
        route: &RouteSpec,
        person: &str,
        inbox: &Inbox,
        asked: (u64, &[tilo_actions::shown::ShownAction], Option<&str>, bool),
    ) {
        let (correlation, actions, after, waits) = asked;
        let decisions = self.device(route, person).decide(actions);
        if waits && after == Some(tilo_actions::shown::decisions_revision(&decisions).as_str()) {
            self.waiting_shown.borrow_mut().push((
                person.to_owned(),
                Rc::clone(inbox),
                correlation,
                actions.to_vec(),
            ));
            return;
        }
        inbox.borrow_mut().push_back(CollabEvent::RemoteAction {
            message: tilo_actions::remote::RemoteActionMessage::ShownActionsResponse {
                correlation,
                result: Ok(decisions),
            },
        });
    }

    /// Hands the connection of `person` at `inbox` the workspace's members, as its store answers
    /// them now, and tells it the members' resource moved: a workspace with no members is one
    /// whose record publishes an empty list, as its governance answers it.
    fn tell_members(&self, _person: &str, inbox: &Inbox) {
        if self.workspace.members_held() {
            return;
        }
        let members = self.members.borrow();
        let policy = self.policy.borrow();
        let rows: Vec<serde_json::Value> = members
            .iter()
            .filter(|row| {
                policy
                    .as_ref()
                    .is_none_or(|policy| member_of(policy, &row.who))
            })
            .map(|row| {
                let actor = tilo_actions::Actor::Member(row.who.clone());
                let mut named = serde_json::json!({
                    "actor": serde_json::to_value(actor).unwrap_or_default(),
                    "name": row.name,
                });
                if let Some(photo) = row
                    .avatar
                    .as_ref()
                    .and_then(|avatar| collab_types::photo_url(&avatar.0))
                    && let Some(fields) = named.as_object_mut()
                {
                    fields.insert("photo".to_owned(), serde_json::json!(photo));
                }
                named
            })
            .collect();
        self.workspace
            .set_members(serde_json::json!({ "people": rows }));
        inbox.borrow_mut().push_back(CollabEvent::RecordsMoved {
            records: vec![collab_types::store::MovedRecord::whole(
                tilo_resource_store::WORKSPACE_MEMBERS_ID,
            )],
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
    fn resources_moved(&self, from: &Inbox, records: &[collab_types::store::MovedRecord]) {
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
            directory: Rc::default(),
            routes: Rc::default(),
            joined: None,
        }
    }

    /// `spec` as the person's device hands it to a window of `bundle`'s app on this connection's
    /// route ([`Bus::launched`]).
    #[must_use]
    pub fn launched(&self, spec: Spec, bundle: &Bundle) -> Spec {
        self.bus.launched(&self.route, spec, bundle)
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

    /// The device's live presence as this connection says and hears it, when its embedder hands
    /// the instance a lane instead of the process's daemon line: a word this connection's person
    /// says is heard by the bus's other connections, as the device hands the workspace's words to
    /// its other apps, and what they said waits for this connection's next look.
    #[must_use]
    pub fn presence_line(&self) -> Rc<dyn crate::presence::Line> {
        Rc::new(PresenceLine {
            bus: Rc::clone(&self.bus),
            inbox: Rc::clone(&self.inbox),
            person: self.person.clone(),
            app: self.route.app_id.clone(),
        })
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

    /// The documents every app of the workspace opens beside its own, as the connection joined
    /// them.
    #[must_use]
    pub fn routes(&self) -> Rc<RefCell<Vec<RouteSpec>>> {
        Rc::clone(&self.routes)
    }
}

/// One connection's end of the device's live presence, as the fake broker plays it: what its
/// person says is heard by the workspace's other connections, stamped with their person, and what
/// they said waits to be handed back here.
struct PresenceLine {
    bus: Rc<Bus>,
    inbox: Inbox,
    person: String,
    app: String,
}

impl crate::presence::Line for PresenceLine {
    fn say(&self, said: &collab_types::presence::Said) -> Result<(), String> {
        // A persona's word is the app's persona's, as the device's presence attributes it.
        let member = said.persona.as_ref().map_or_else(
            || collaboration::MemberId::person(self.person.clone()),
            |persona| persona.member(&self.app),
        );
        let heard = collab_types::presence::Heard {
            member,
            key: said.key.clone(),
            payload: said.payload.clone(),
            persona: said.persona.clone(),
        };
        let joined: Vec<Inbox> = self
            .bus
            .joined
            .borrow()
            .iter()
            .map(|(_, _, inbox)| Rc::clone(inbox))
            .collect();
        let mut waiting = self.bus.presence.borrow_mut();
        let mut told: Vec<Inbox> = Vec::new();
        for inbox in joined {
            if Rc::ptr_eq(&inbox, &self.inbox) || told.iter().any(|told| Rc::ptr_eq(told, &inbox)) {
                continue;
            }
            waiting.push((Rc::clone(&inbox), heard.clone()));
            told.push(inbox);
        }
        Ok(())
    }

    fn heard(&self) -> Vec<collab_types::presence::Heard> {
        let (mine, others) = {
            let mut waiting = self.bus.presence.borrow_mut();
            std::mem::take(&mut *waiting)
                .into_iter()
                .partition(|(inbox, _)| Rc::ptr_eq(inbox, &self.inbox))
        };
        *self.bus.presence.borrow_mut() = others;
        mine.into_iter().map(|(_, heard)| heard).collect()
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
            CollabRequest::RemoteAction {
                message:
                    tilo_actions::remote::RemoteActionMessage::ResourceRead {
                        correlation, read, ..
                    },
                ..
            } if read.workspace.starts_with("devices/") => {
                self.bus.read_device_record(&self.inbox, *correlation, read);
            }
            CollabRequest::RemoteAction {
                message:
                    tilo_actions::remote::RemoteActionMessage::ShownActionsRequest {
                        correlation,
                        actions,
                        after,
                        wait_ms,
                    },
                ..
            } => self.bus.shown(
                &self.route,
                &self.person,
                &self.inbox,
                (*correlation, actions, after.as_deref(), wait_ms.is_some()),
            ),
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
