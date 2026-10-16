//! Native app-record checkpoints. Content is retained before a signed room root
//! crosses the owner's ordinary publication barrier. No historical grants survive.
use crate::{DocumentRead, NativeCommittedView, NativeWorkspaceOwner, RoomId, Store};
use automerge::{ROOT, ReadDoc};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::SystemTime};
use tilo_owner_commit::{CommitBinding, CommitTarget};
use tilo_user_identity::DeviceCert;

const MAX_ROUTES: usize = 32;
const MAX_CHECKPOINTS: usize = 128;
const MAX_BLOCK: usize = 256 * 1024;
const MAX_DOCUMENT: usize = 3 * 1024 * 1024;
const MAX_CAPTURE: usize = 16 * 1024 * 1024;

pub use tilo_peer_state::workspace_checkpoint::CheckpointRoute;
use tilo_peer_state::workspace_checkpoint::{CheckpointCommand, CheckpointRequest};

/// The identities actually stored by the record, not an inferred installed schema.
/// None means absent legacy metadata, never a guessed current app version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointSchema {
    pub encoding: String,
    pub kind: Option<String>,
    pub surface: Option<String>,
    pub storage_version: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointEntry {
    pub route: CheckpointRoute,
    pub heads: Vec<[u8; 32]>,
    pub schema: CheckpointSchema,
    pub content: [u8; 32],
    pub bytes: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCheckpoint {
    pub version: u32,
    pub id: uuid::Uuid,
    /// Original capture request name; current names live in the signed index.
    pub original_name: String,
    pub room: RoomId,
    pub captured_generation: u64,
    pub captured_aggregate: [u8; 32],
    /// Person identity from authenticated admission, not an argument or grant copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<[u8; 32]>,
    pub entries: Vec<CheckpointEntry>,
}
/// Durable owner acknowledgement, returned only after metadata publication.
/// The manifest address and id stay fixed when names move or records mutate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointAcknowledgement {
    pub id: uuid::Uuid,
    pub manifest: [u8; 32],
    pub captured_generation: u64,
    pub publication_generation: u64,
}
/// One materialized read; no partial result is returned if any route is denied/damaged.
pub struct MaterializedCheckpoint {
    pub manifest: NativeCheckpoint,
    pub documents: Vec<Vec<u8>>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    checkpoints: BTreeMap<uuid::Uuid, CheckpointAcknowledgement>,
    names: BTreeMap<String, uuid::Uuid>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    invocations: BTreeMap<uuid::Uuid, InvocationResult>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InvocationResult {
    digest: [u8; 32],
    acknowledgement: CheckpointAcknowledgement,
}
fn retry(
    index: &Index,
    request: Option<&CheckpointRequest>,
) -> Result<Option<CheckpointAcknowledgement>, String> {
    let Some(request) = request else {
        return Ok(None);
    };
    let digest = request.digest()?;
    if let Some(result) = index.invocations.get(&request.invocation) {
        if result.digest != digest {
            return Err("checkpoint invocation belongs to another command or caller".into());
        }
        return Ok(Some(result.acknowledgement.clone()));
    }
    if index.invocations.len() >= 256 {
        return Err("checkpoint invocation capacity exhausted".into());
    }
    Ok(None)
}
fn retain_result(
    index: &mut Index,
    request: Option<&CheckpointRequest>,
    ack: &CheckpointAcknowledgement,
) -> Result<(), String> {
    if let Some(request) = request {
        index.invocations.insert(
            request.invocation,
            InvocationResult {
                digest: request.digest()?,
                acknowledgement: ack.clone(),
            },
        );
    }
    Ok(())
}
fn name_valid(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        return Err("invalid native checkpoint name".into());
    }
    Ok(())
}
fn routes_valid(routes: &[CheckpointRoute]) -> Result<(), String> {
    if routes.is_empty() || routes.len() > MAX_ROUTES {
        return Err("native checkpoint route capacity exceeded".into());
    }
    for route in routes {
        route.validate()?;
    }
    if !routes
        .windows(2)
        .all(|pair| matches!(pair, [first, second] if first < second))
    {
        return Err("native checkpoint routes must be sorted and distinct".into());
    }
    Ok(())
}
fn read<T: for<'de> Deserialize<'de> + Serialize>(
    store: &dyn Store,
    hash: [u8; 32],
) -> Result<T, String> {
    let bytes = store.read_history(hash)?;
    if bytes.len() > MAX_BLOCK || *blake3::hash(&bytes).as_bytes() != hash {
        return Err("native checkpoint content integrity failed".into());
    }
    let value: T =
        serde_json::from_slice(&bytes).map_err(|_| "malformed native checkpoint content")?;
    if serde_json::to_vec(&value).map_err(|e| e.to_string())? != bytes {
        return Err("noncanonical native checkpoint content".into());
    }
    Ok(value)
}
fn put(store: &dyn Store, bytes: &[u8]) -> Result<[u8; 32], String> {
    let hash = store.put_history(bytes)?;
    if hash != *blake3::hash(bytes).as_bytes() || store.read_history(hash)? != bytes {
        return Err("native checkpoint retention verification failed".into());
    }
    Ok(hash)
}
fn put_metadata(store: &dyn Store, value: &impl Serialize) -> Result<[u8; 32], String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_BLOCK {
        return Err("native checkpoint metadata capacity exceeded".into());
    }
    put(store, &bytes)
}
fn index(view: &NativeCommittedView, store: &dyn Store) -> Result<Index, String> {
    let index: Index = match view.peer.workspace().workspace_checkpoint_root()? {
        None => Index::default(),
        Some(hash) => read(store, hash)?,
    };
    if index.checkpoints.len() > MAX_CHECKPOINTS
        || index.names.len() > MAX_CHECKPOINTS
        || index.invocations.len() > 256
    {
        return Err("native checkpoint index capacity exceeded".into());
    }
    for (id, ack) in &index.checkpoints {
        if id.is_nil()
            || id != &ack.id
            || ack.captured_generation == 0
            || ack.publication_generation <= ack.captured_generation
            || (view.device() == view.governor() && ack.publication_generation > view.generation())
        {
            return Err("invalid native checkpoint acknowledgement".into());
        }
    }
    for (invocation, result) in &index.invocations {
        if invocation.is_nil()
            || index.checkpoints.get(&result.acknowledgement.id) != Some(&result.acknowledgement)
        {
            return Err("invalid native checkpoint invocation result".into());
        }
    }
    for (name, id) in &index.names {
        name_valid(name)?;
        if !index.checkpoints.contains_key(id) {
            return Err("dangling native checkpoint name".into());
        }
    }
    Ok(index)
}
fn manifest(
    store: &dyn Store,
    room: RoomId,
    ack: &CheckpointAcknowledgement,
) -> Result<NativeCheckpoint, String> {
    let manifest: NativeCheckpoint = read(store, ack.manifest)?;
    if manifest.version != 1
        || manifest.room != room
        || manifest.id != ack.id
        || manifest.captured_generation != ack.captured_generation
    {
        return Err("native checkpoint identity mismatch".into());
    }
    name_valid(&manifest.original_name)?;
    routes_valid(
        &manifest
            .entries
            .iter()
            .map(|e| e.route.clone())
            .collect::<Vec<_>>(),
    )?;
    Ok(manifest)
}
fn unique_identity<'a>(
    document: &'a automerge::AutoCommit,
    object: &automerge::ObjId,
    key: &str,
) -> Result<Option<(automerge::Value<'a>, automerge::ObjId)>, String> {
    let mut values = document.get_all(object, key).map_err(|e| e.to_string())?;
    if values.len() > 1 {
        return Err("conflicting native record schema identity".into());
    }
    Ok(values.pop())
}
fn schema(document: &automerge::AutoCommit) -> Result<CheckpointSchema, String> {
    fn string(
        document: &automerge::AutoCommit,
        object: &automerge::ObjId,
        key: &str,
    ) -> Result<Option<String>, String> {
        match unique_identity(document, object, key)? {
            None => Ok(None),
            Some((value, _)) => value
                .as_str()
                .map(|s| Some(s.to_owned()))
                .ok_or_else(|| "invalid native record schema identity".into()),
        }
    }
    let (kind, surface) = match unique_identity(document, &ROOT, "tilo:record")? {
        None => (None, None),
        Some((automerge::Value::Object(automerge::ObjType::Map), object)) => (
            string(document, &object, "kind")?,
            string(document, &object, "surface")?,
        ),
        _ => return Err("invalid native record identity".into()),
    };
    let storage_version = match unique_identity(document, &ROOT, "tilo:version")? {
        None => None,
        Some((value, _)) => Some(
            value
                .as_u64()
                .ok_or("invalid native record storage version")?,
        ),
    };
    Ok(CheckpointSchema {
        encoding: "automerge-save-v1".into(),
        kind,
        surface,
        storage_version,
    })
}
fn document(store: &dyn Store, entry: &CheckpointEntry) -> Result<Vec<u8>, String> {
    let bytes = store.read_history(entry.content)?;
    if bytes.len() != entry.bytes
        || bytes.len() > MAX_DOCUMENT
        || *blake3::hash(&bytes).as_bytes() != entry.content
    {
        return Err("native checkpoint snapshot integrity failed".into());
    }
    let mut doc =
        automerge::AutoCommit::load(&bytes).map_err(|_| "corrupt native checkpoint snapshot")?;
    let mut heads = doc.get_heads().iter().map(|h| h.0).collect::<Vec<_>>();
    heads.sort_unstable();
    if heads != entry.heads || schema(&doc)? != entry.schema {
        return Err("native checkpoint snapshot heads/schema mismatch".into());
    }
    Ok(bytes)
}
fn documents(store: &dyn Store, checkpoint: &NativeCheckpoint) -> Result<Vec<Vec<u8>>, String> {
    let mut documents = Vec::new();
    let mut total = 0usize;
    for entry in &checkpoint.entries {
        total = total
            .checked_add(entry.bytes)
            .filter(|n| *n <= MAX_CAPTURE)
            .ok_or("native checkpoint read byte capacity")?;
        documents.push(document(store, entry)?);
    }
    Ok(documents)
}
/// A receiving member may hold signed references without the governor's retained
/// blocks. It still cannot read them successfully until verified content arrives.
pub(crate) fn validate_restore(
    view: &NativeCommittedView,
    store: &dyn Store,
) -> Result<(), String> {
    if view.device() != view.governor() {
        view.peer.workspace().workspace_checkpoint_root()?;
        return Ok(());
    }
    for ack in index(view, store)?.checkpoints.values() {
        let checkpoint = manifest(store, view.record.room, ack)?;
        let mut total = 0usize;
        for entry in &checkpoint.entries {
            total = total
                .checked_add(entry.bytes)
                .filter(|n| *n <= MAX_CAPTURE)
                .ok_or("native checkpoint restore byte capacity")?;
            document(store, entry)?;
        }
    }
    Ok(())
}

pub(crate) fn binding(room: RoomId, address: [u8; 32]) -> CommitBinding {
    CommitBinding {
        room: room.to_string(),
        target: CommitTarget::NativeCheckpointRoot { root: address },
        request_digest: *blake3::Hasher::new_derive_key(
            "Tilo native checkpoint root publication v1",
        )
        .update(&address)
        .finalize()
        .as_bytes(),
        basis: address,
    }
}
fn retain_manifest(
    store: &dyn Store,
    candidate: &NativeCommittedView,
    caller: &DeviceCert,
    capture: CheckpointCapture<'_>,
    now: SystemTime,
) -> Result<NativeCheckpoint, String> {
    let mut checkpoint = NativeCheckpoint {
        version: 1,
        id: capture.id,
        original_name: capture.name.into(),
        room: candidate.record.room,
        captured_generation: candidate.generation(),
        captured_aggregate: *blake3::hash(&candidate.record.peer).as_bytes(),
        author: Some(caller.user_public_key),
        entries: Vec::new(),
    };
    let mut total = 0usize;
    for route in capture.routes {
        let captured = match candidate.read_document(
            caller.device_public_key,
            &route.app,
            &route.record,
            None,
            MAX_DOCUMENT,
            now,
        )? {
            DocumentRead::Captured(captured) => captured,
            DocumentRead::CheckpointPending => {
                return Err("native checkpoint capture pending room checkpoint".into());
            }
            DocumentRead::Unchanged(_) => {
                return Err("native checkpoint capture missing snapshot".into());
            }
        };
        total = total
            .checked_add(captured.bytes.len())
            .filter(|n| *n <= MAX_CAPTURE)
            .ok_or("native checkpoint capture byte capacity")?;
        let doc = automerge::AutoCommit::load(&captured.bytes).map_err(|e| e.to_string())?;
        checkpoint.entries.push(CheckpointEntry {
            route: route.clone(),
            heads: captured.heads,
            schema: schema(&doc)?,
            bytes: captured.bytes.len(),
            content: put(store, &captured.bytes)?,
        });
    }
    Ok(checkpoint)
}

impl NativeWorkspaceOwner {
    /// Execute a broker-admitted command with separately supplied governor custody.
    /// Public reads return verified metadata only, never retained document bytes.
    /// # Errors
    /// Refuses absent custody, current denial, conflicting invocation or damaged content.
    pub fn execute_checkpoint(
        &self,
        request: &CheckpointRequest,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<serde_json::Value, String> {
        request.digest()?;
        let room = RoomId::parse(&request.room).ok_or("invalid checkpoint room")?;
        self.with_current(room, |view| {
            if view.device() != view.governor() {
                return Err("checkpoint requires the actual governing owner".into());
            }
            view.peer.workspace().check_checkpoint_writer(key, now)
        })?;
        match &request.command {
            CheckpointCommand::Capture { name, routes } => {
                let ack = self.capture_checkpoint_bound(
                    room,
                    &request.caller,
                    key,
                    CheckpointCapture {
                        id: request.invocation,
                        name,
                        routes,
                    },
                    now,
                    Some(request),
                )?;
                serde_json::to_value(ack).map_err(|e| e.to_string())
            }
            CheckpointCommand::NameBind { name, id } => {
                let ack = self.bind_checkpoint_name_bound(
                    room,
                    &request.caller,
                    key,
                    name,
                    *id,
                    Some(request),
                )?;
                serde_json::to_value(ack).map_err(|e| e.to_string())
            }
            CheckpointCommand::Inspect { id } => {
                let retained = self.read_checkpoint(room, &request.caller, *id, now)?;
                serde_json::to_value(retained.manifest).map_err(|e| e.to_string())
            }
            CheckpointCommand::Find { name } => {
                let ack = self.resolve_checkpoint_name(room, &request.caller, name, now)?;
                serde_json::to_value(ack).map_err(|e| e.to_string())
            }
        }
    }

    /// Capture an explicit native app-record set at ONE committed generation.
    /// Caller evidence comes from host-authenticated admission, never action arguments.
    /// Current governor signing custody is required independently of caller Read.
    /// Reusing an id with the same routes returns its original durable acknowledgement;
    /// changed routes are refused, even after later mutations. Naming is separate.
    ///
    /// # Errors
    /// Current read refusal, absent custody, bounded capacity, missing/encrypted/
    /// incomplete data, pending checkpoint, content I/O or metadata publication failure.
    pub fn capture_checkpoint(
        &self,
        room: RoomId,
        caller: &DeviceCert,
        key: &SigningKey,
        capture: CheckpointCapture<'_>,
        now: SystemTime,
    ) -> Result<CheckpointAcknowledgement, String> {
        self.capture_checkpoint_bound(room, caller, key, capture, now, None)
    }
    fn capture_checkpoint_bound(
        &self,
        room: RoomId,
        caller: &DeviceCert,
        key: &SigningKey,
        capture: CheckpointCapture<'_>,
        now: SystemTime,
        request: Option<&CheckpointRequest>,
    ) -> Result<CheckpointAcknowledgement, String> {
        name_valid(capture.name)?;
        routes_valid(capture.routes)?;
        if capture.id.is_nil() {
            return Err("nil native checkpoint id".into());
        }
        let _turn = self.0.mutation.lock();
        let candidate = self.with_current(room, |v| Ok(v.clone()))?;
        if candidate.device() != candidate.governor()
            || key.verifying_key().to_bytes() != candidate.governor().device_public_key
        {
            return Err("native checkpoint lacks current governor signing custody".into());
        }
        candidate
            .peer
            .workspace()
            .check_checkpoint_writer(key, now)?;
        candidate
            .peer
            .workspace()
            .check_checkpoint_namer(caller, now)?;
        for route in capture.routes {
            candidate.peer.workspace().check_checkpoint_reader(
                caller,
                &route.app,
                &route.record,
                now,
            )?;
        }
        let mut index = index(&candidate, self.0.store.as_ref())?;
        if let Some(ack) = retry(&index, request)? {
            let checkpoint = manifest(self.0.store.as_ref(), room, &ack)?;
            for entry in &checkpoint.entries {
                document(self.0.store.as_ref(), entry)?;
            }
            return Ok(ack);
        }
        if let Some(ack) = index.checkpoints.get(&capture.id) {
            if request.is_some() {
                return Err("checkpoint id already exists without this invocation binding".into());
            }
            let existing = manifest(self.0.store.as_ref(), room, ack)?;
            if existing
                .entries
                .iter()
                .map(|e| &e.route)
                .ne(capture.routes.iter())
            {
                return Err("native checkpoint id is immutable".into());
            }
            if existing.original_name != capture.name {
                return Err("native checkpoint id is already bound to another capture name".into());
            }
            for entry in &existing.entries {
                document(self.0.store.as_ref(), entry)?;
            }
            return Ok(ack.clone());
        }
        if index.checkpoints.len() >= MAX_CHECKPOINTS
            || index.names.len() >= MAX_CHECKPOINTS
            || index.names.contains_key(capture.name)
        {
            return Err("native checkpoint capacity or name collision".into());
        }
        let generation = candidate
            .generation()
            .checked_add(1)
            .ok_or("checkpoint generation exhausted")?;
        let checkpoint = retain_manifest(self.0.store.as_ref(), &candidate, caller, capture, now)?;
        let ack = CheckpointAcknowledgement {
            id: capture.id,
            manifest: put_metadata(self.0.store.as_ref(), &checkpoint)?,
            captured_generation: checkpoint.captured_generation,
            publication_generation: generation,
        };
        index.checkpoints.insert(capture.id, ack.clone());
        index.names.insert(capture.name.into(), capture.id);
        retain_result(&mut index, request, &ack)?;
        self.publish_checkpoint_index(room, candidate, &index, key, now)?;
        Ok(ack)
    }
    fn publish_checkpoint_index(
        &self,
        room: RoomId,
        mut candidate: NativeCommittedView,
        index: &Index,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<(), String> {
        let address = put_metadata(self.0.store.as_ref(), index)?;
        candidate
            .peer
            .workspace_mut()
            .set_workspace_checkpoint_root(address, key, now)?;
        let generation = candidate
            .generation()
            .checked_add(1)
            .ok_or("checkpoint generation exhausted")?;
        self.with_current(room, |previous| {
            candidate
                .peer
                .durable_mutation_messages(&previous.peer, key, now, generation)?;
            Ok(())
        })?;
        candidate.record.peer = candidate.peer.durable()?;
        candidate.record.generation = generation;
        crate::publication::replace_governance_checkpoint(
            &mut candidate,
            binding(room, address),
            now,
        )?;
        self.publish_room(room, candidate, None)
    }
    /// Move a human name to an existing immutable checkpoint. No content/grants copy.
    ///
    /// # Errors
    /// Absent custody, denied current route, missing content or failed publication.
    #[cfg(test)]
    pub fn bind_checkpoint_name(
        &self,
        room: RoomId,
        caller: &DeviceCert,
        key: &SigningKey,
        name: &str,
        id: uuid::Uuid,
    ) -> Result<(), String> {
        self.bind_checkpoint_name_bound(room, caller, key, name, id, None)
            .map(|_| ())
    }
    fn bind_checkpoint_name_bound(
        &self,
        room: RoomId,
        caller: &DeviceCert,
        key: &SigningKey,
        name: &str,
        id: uuid::Uuid,
        request: Option<&CheckpointRequest>,
    ) -> Result<CheckpointAcknowledgement, String> {
        name_valid(name)?;
        let now = SystemTime::now();
        let _turn = self.0.mutation.lock();
        let candidate = self.with_current(room, |v| Ok(v.clone()))?;
        if candidate.device() != candidate.governor()
            || key.verifying_key().to_bytes() != candidate.governor().device_public_key
        {
            return Err("native checkpoint naming lacks governor custody".into());
        }
        candidate
            .peer
            .workspace()
            .check_checkpoint_writer(key, now)?;
        candidate
            .peer
            .workspace()
            .check_checkpoint_namer(caller, now)?;
        let mut index = index(&candidate, self.0.store.as_ref())?;
        let ack = index
            .checkpoints
            .get(&id)
            .ok_or("native checkpoint id absent")?;
        let checkpoint = manifest(self.0.store.as_ref(), room, ack)?;
        for entry in &checkpoint.entries {
            candidate.peer.workspace().check_checkpoint_reader(
                caller,
                &entry.route.app,
                &entry.route.record,
                now,
            )?;
        }
        for entry in &checkpoint.entries {
            document(self.0.store.as_ref(), entry)?;
        }
        let ack = ack.clone();
        if let Some(result) = retry(&index, request)? {
            return Ok(result);
        }
        if request.is_none() && index.names.get(name) == Some(&id) {
            return Ok(ack);
        }
        if index.names.len() >= MAX_CHECKPOINTS && !index.names.contains_key(name) {
            return Err("native checkpoint name capacity".into());
        }
        index.names.insert(name.into(), id);
        retain_result(&mut index, request, &ack)?;
        self.publish_checkpoint_index(room, candidate, &index, key, now)?;
        Ok(ack)
    }
    /// Resolve a movable name only if every retained route is currently readable
    /// and all retained snapshots verify against the authenticated manifest.
    ///
    /// # Errors
    /// Unknown name, current read refusal or absent/corrupt retained content.
    pub fn resolve_checkpoint_name(
        &self,
        room: RoomId,
        caller: &DeviceCert,
        name: &str,
        now: SystemTime,
    ) -> Result<CheckpointAcknowledgement, String> {
        name_valid(name)?;
        self.with_current(room, |view| {
            let index = index(view, self.0.store.as_ref())?;
            let id = index
                .names
                .get(name)
                .ok_or("native checkpoint name absent")?;
            let ack = index
                .checkpoints
                .get(id)
                .ok_or("native checkpoint name is dangling")?;
            let checkpoint = manifest(self.0.store.as_ref(), room, ack)?;
            for entry in &checkpoint.entries {
                view.peer.workspace().check_checkpoint_reader(
                    caller,
                    &entry.route.app,
                    &entry.route.record,
                    now,
                )?;
            }
            documents(self.0.store.as_ref(), &checkpoint)?;
            Ok(ack.clone())
        })
    }

    /// Materialize all snapshots only after checking EVERY route's current permission.
    /// The immutable id, not a movable name, selects the captured generation.
    ///
    /// # Errors
    /// Current caller denial or any absent, corrupt or unsupported retained content.
    pub fn read_checkpoint(
        &self,
        room: RoomId,
        caller: &DeviceCert,
        id: uuid::Uuid,
        now: SystemTime,
    ) -> Result<MaterializedCheckpoint, String> {
        self.with_current(room, |view| {
            let index = index(view, self.0.store.as_ref())?;
            let ack = index
                .checkpoints
                .get(&id)
                .ok_or("native checkpoint id absent")?;
            let checkpoint = manifest(self.0.store.as_ref(), room, ack)?;
            for entry in &checkpoint.entries {
                view.peer.workspace().check_checkpoint_reader(
                    caller,
                    &entry.route.app,
                    &entry.route.record,
                    now,
                )?;
            }
            let documents = documents(self.0.store.as_ref(), &checkpoint)?;
            Ok(MaterializedCheckpoint {
                manifest: checkpoint,
                documents,
            })
        })
    }
}
/// Explicit sorted, distinct native coverage. Does not cover filesystem/media owners.
#[derive(Clone, Copy)]
pub struct CheckpointCapture<'a> {
    pub id: uuid::Uuid,
    pub name: &'a str,
    pub routes: &'a [CheckpointRoute],
}

#[cfg(test)]
#[path = "checkpoint_tests.rs"]
mod tests;
