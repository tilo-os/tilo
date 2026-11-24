//! Nearby Send between the person's devices, carried by this device's daemon.
//!
//! One device offers a file it holds on the device's content shelf
//! ([`tilo_workspace_service::content::ContentShelf`]) to another of the person's devices: the
//! offer is an ordinary invocation of [`RECEIVE`] over the one action service this daemon already
//! serves (`tilo-actions/1`), the service another of their devices is already reached over
//! ([`super::own_device`]). The receiving daemon keeps the offer as a resource of its own, and its
//! person answers it with the typed [`ACCEPT`] action, which is kept as an asked record until they
//! say yes ([`super::asked`]). On their yes the receiving daemon asks the sending device for the
//! content by its hash, a piece at a time ([`CONTENT`]), keeps what it receives on its own shelf,
//! checks it hashes to what was offered, and installs it in the person's Files at the folder they
//! chose, through the one writer of the workspace's tree.
//!
//! The offer's content is staged on the shelf by the session that shows the share sheet, which is
//! the seat's: the daemon holds the offers, the transfers and the bytes' route, and binds no
//! endpoint of its own.
//!
//! The reading ([`RESOURCE`]) is what the person's shell shows: the devices a file can be handed
//! to, the folders here that take one, and every offer and transfer in order.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use iroh::{EndpointAddr, EndpointId};
use serde::Deserialize;
use serde_json::{Value, json};
use tilo_actions::api::{ActionAddress, ActionDescriptor, PlacedAction};
use tilo_actions::contract::{ActionContract, ActionKind};
use tilo_actions::remote::admitted::{offered, top_level};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus, RemoteActionTarget,
    asked,
};
use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::actions::FOLLOW_DEADLINE;
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::apps::nearby_send::{
    DeviceKind, NearbyDestination, NearbyDevice, NearbyTransferRow, Reading, TransferState,
};
use tilo_workspace_service::apps::services::Caller as ServiceCaller;
use tilo_workspace_service::content::ContentScope;
use tilo_workspace_service::workspaces::PersonResources;
use tilo_workspace_service::workspaces::person_workspace::devices::KeyKind;
use tilo_workspace_service::workspaces::providers::{Providers, Reach};

use super::reading::{Changes, Read, Served};
use super::workspaces::ServiceWorkspaces;

/// The provider the offers and transfers are answered under on this device's dispatch and by
/// another of the person's devices over the action service.
pub const PROVIDER: &str = "so.tilo.nearby";
/// The application whose grants decide the calls: the files being handed over are Files'.
pub const APP: &str = tilo_app_id::apps::FILE_EXPLORER;
/// The resource the shell reads, as the workspace service's Nearby Send host declares it.
pub const RESOURCE: &str = tilo_workspace_service::apps::nearby_send::RESOURCE;

/// Offer files to another of the person's devices.
pub const SEND: &str = "send";
/// Keep a file another device offered, in the folder the person chose.
pub const ACCEPT: &str = "accept";
/// Turn down a file another device offered.
pub const DECLINE: &str = "decline";
/// Stop a transfer either way.
pub const CANCEL: &str = "cancel";
/// Offer a failed transfer again.
pub const RETRY: &str = "retry-transfer";
/// Take another device's offer: the action its daemon calls here.
pub const RECEIVE: &str = "receive";
/// One piece of the content an offer names, asked by the receiving device.
pub const CONTENT: &str = "content";
/// How a transfer ended, told to the other device.
pub const RESULT: &str = "result";

/// The files one offer carries at most, as the shell's host already bounds a send.
const MAX_ITEMS: usize = 32;
/// The most of a content one piece carries.
const PIECE_BYTES: usize = 256 * 1024;
/// The folder a received file lands in unless the person chose another.
const DEFAULT_FOLDER: &str = "Downloads";
/// How many transfers a device remembers before the oldest ended one goes.
const KEPT: usize = 64;

/// The offers and transfers of this device, and the shelf they move over.
pub(crate) struct Nearby {
    inner: Arc<Inner>,
}

struct Inner {
    workspaces: Arc<ServiceWorkspaces>,
    /// The person's devices, as their own workspace lists them: where an offer can go.
    personal: PersonResources,
    state: Mutex<State>,
    /// Moves with every change of the state: what a reading follows.
    changed: tokio::sync::watch::Sender<u64>,
}

#[derive(Default)]
struct State {
    transfers: BTreeMap<[u8; 16], Transfer>,
    /// The folder the person used last in each workspace, so the next accept there offers it.
    remembered: BTreeMap<String, String>,
    /// The folders a workspace's Files offers, read once the tree opens.
    destinations: BTreeMap<String, Vec<String>>,
}

/// One offer or transfer, either end.
struct Transfer {
    id: [u8; 16],
    incoming: bool,
    name: String,
    hash: String,
    total: u64,
    transferred: u64,
    state: TransferState,
    /// The other device, by its key and the name it goes by.
    peer_device: [u8; 32],
    peer: String,
    /// The workspace the content is kept in, on the shelf.
    workspace: String,
    /// Where a received file lands, as the workspace's Files tree names the folder.
    destination: String,
    created_ms: u64,
    /// Set when the person stops it, or the other device does.
    stopped: Arc<AtomicBool>,
}

impl Transfer {
    fn row(&self) -> NearbyTransferRow {
        NearbyTransferRow {
            state: self.state,
            progress: if self.total == 0 {
                0
            } else {
                u8::try_from(
                    self.transferred
                        .min(self.total)
                        .saturating_mul(100)
                        .checked_div(self.total)
                        .unwrap_or_default()
                        .min(100),
                )
                .unwrap_or(100)
            },
            peer: self.peer.clone(),
            destination: self.destination.clone(),
            time: self.created_ms.saturating_mul(1_000_000),
            incoming: self.incoming,
            name: self.name.clone(),
            handle: hex(&self.id),
        }
    }
}

impl Nearby {
    /// The offers and transfers of this device, moved over the content shelf `workspaces` keeps.
    pub(crate) fn new(workspaces: &Arc<ServiceWorkspaces>, personal: PersonResources) -> Arc<Self> {
        let (changed, _) = tokio::sync::watch::channel(0);
        Arc::new(Self {
            inner: Arc::new(Inner {
                workspaces: Arc::clone(workspaces),
                personal,
                state: Mutex::new(State::default()),
                changed,
            }),
        })
    }

    /// Answer the offers and transfers on this device's action service from now on, and to
    /// another of the person's devices.
    pub(crate) fn register(self: &Arc<Self>, providers: &Providers) {
        providers.provide(
            PROVIDER,
            APP,
            Reach::Devices,
            Arc::clone(self) as Arc<dyn ActionProvider + Send + Sync>,
        );
    }

    /// The reading the person's shell follows over the shell link.
    pub(crate) fn published(self: &Arc<Self>) -> Served {
        let reading = Arc::clone(self);
        let changing = Arc::clone(self);
        let read: Arc<Read> = Arc::new(move |caller: &ServiceCaller| {
            json!(reading.reading_for(Some(caller.workspace.as_str())))
        });
        let changes: Arc<Changes> = Arc::new(move || changing.inner.changed.subscribe());
        Served::new(RESOURCE, read, changes)
    }

    /// What the reading shows in `workspace`: every device a file can go to, the folders here,
    /// and each offer and transfer.
    fn reading_for(&self, workspace: Option<&str>) -> Reading {
        let here = self.inner.workspaces.device_cert();
        let mut reading = Reading::default();
        for cert in self.inner.personal.kept_devices() {
            if KeyKind::of(&cert) != KeyKind::Device
                || cert.device_public_key == here.device_public_key
            {
                continue;
            }
            reading.devices.push(NearbyDevice {
                name: cert.device_label.clone(),
                kind: DeviceKind::Device,
                reachable: true,
                address: String::new(),
                port: 0,
                handle: hex(&cert.device_public_key),
            });
        }
        {
            let state = self.inner.state.lock_recover();
            if let Some(workspace) = workspace
                && let Some(folders) = state.destinations.get(workspace)
            {
                let remembered = state.remembered.get(workspace);
                for folder in folders {
                    reading.destinations.push(NearbyDestination {
                        name: folder_name(folder),
                        handle: folder.clone(),
                        remembered: remembered.is_some_and(|chosen| chosen == folder),
                    });
                }
            }
            reading
                .transfers
                .extend(state.transfers.values().map(Transfer::row));
        }
        if let Some(workspace) = workspace
            && !self
                .inner
                .state
                .lock_recover()
                .destinations
                .contains_key(workspace)
        {
            // The folders are read off the workspace's tree, off this thread: the reading shows
            // them, and moves again, once they are read.
            let inner = Arc::clone(&self.inner);
            let workspace = workspace.to_owned();
            tilo_rt::app().spawn(async move {
                Inner::folders(&inner, &workspace).await;
            });
        }
        reading
    }

    /// Tell every follower the reading moved.
    fn moved(&self) {
        let _ = self
            .inner
            .changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }

    /// The scope `workspace`'s Files content lives under on this device's shelf.
    fn scope(workspace: &str) -> ContentScope {
        ContentScope {
            workspace: workspace.to_owned(),
            app: APP.to_owned(),
        }
    }

    /// Whether `caller` is the person this device runs for, on this device or another of theirs.
    fn is_our_person(&self, caller: &Caller) -> bool {
        if caller.is_foreign_device() {
            return false;
        }
        let cert = self.inner.workspaces.device_cert();
        if let Some(user) = caller.user_public_key() {
            return *user == cert.user_public_key;
        }
        let person = hex(&cert.user_public_key);
        caller.device_public_key().is_none() && caller.acting().person_id() == Some(person.as_str())
    }

    /// Whether `caller` is one of the person's devices, other than this one.
    fn is_our_device(&self, caller: &Caller) -> bool {
        if caller.is_foreign_device() {
            return false;
        }
        let cert = self.inner.workspaces.device_cert();
        caller.user_public_key() == Some(&cert.user_public_key)
            && caller
                .device_public_key()
                .is_some_and(|device| *device != cert.device_public_key)
    }

    /// The device `device` names, as the person's own workspace lists it.
    fn other_device(&self, device: [u8; 32]) -> Option<DeviceCert> {
        self.inner
            .personal
            .kept_devices()
            .into_iter()
            .find(|cert| cert.device_public_key == device && KeyKind::of(cert) == KeyKind::Device)
    }

    /// Offer `items`, each a content the shelf already keeps in `workspace`, to the device the
    /// reading names by `device`.
    fn offer(&self, workspace: &str, items: Vec<Item>, device: &str) -> Result<(), String> {
        let target = device_key(device).ok_or("that is not a device the reading listed")?;
        let cert = self
            .other_device(target)
            .ok_or("that device is not one of yours any more")?;
        if !permitted() {
            return Err("this device does not send files to your other devices".to_owned());
        }
        if items.is_empty() {
            return Err("an offer names at least one file".to_owned());
        }
        if items.len() > MAX_ITEMS {
            return Err(format!("an offer names at most {MAX_ITEMS} files"));
        }
        let shelf = self
            .inner
            .workspaces
            .content_shelf()
            .ok_or("this device keeps no content yet")?;
        let scope = Self::scope(workspace);
        let mut carried = Vec::with_capacity(items.len());
        for item in items {
            if !valid_hash(&item.hash) {
                return Err("an offered file's content address is not one".to_owned());
            }
            let Some(bytes) = shelf.read(&scope, &item.hash) else {
                return Err("an offered file is not on this device's shelf".to_owned());
            };
            let size = if item.size == 0 {
                u64::try_from(bytes.len()).unwrap_or(u64::MAX)
            } else {
                item.size
            };
            carried.push((item, size));
        }
        for (item, size) in carried {
            let id = random_id();
            let transfer = Transfer {
                id,
                incoming: false,
                name: item.name.clone(),
                hash: item.hash.clone(),
                total: size,
                transferred: 0,
                state: TransferState::Offered,
                peer_device: target,
                peer: cert.device_label.clone(),
                workspace: workspace.to_owned(),
                destination: String::new(),
                created_ms: tilo_clock::unix_now_ms(),
                stopped: Arc::default(),
            };
            self.keep(transfer);
            let inner = Arc::clone(&self.inner);
            let offer = json!({
                "transfer": hex(&id),
                "name": item.name,
                "hash": item.hash,
                "size": size,
                "device": hex(&self.inner.workspaces.device_cert().device_public_key),
                "label": self.inner.workspaces.device_cert().device_label,
            });
            let workspace = workspace.to_owned();
            tilo_rt::app().spawn(async move {
                let answered = call(&inner, target, &workspace, RECEIVE, offer).await;
                match answered {
                    Ok(_) => Inner::settle(&inner, id, TransferState::Sending),
                    Err(reason) => {
                        tracing::debug!(%reason, "a device did not take a nearby offer");
                        Inner::settle(&inner, id, TransferState::Failed);
                    }
                }
            });
        }
        Ok(())
    }

    /// Keep `transfer`, letting the oldest ended ones go past [`KEPT`].
    fn keep(&self, transfer: Transfer) {
        let mut state = self.inner.state.lock_recover();
        state.transfers.insert(transfer.id, transfer);
        while state.transfers.len() > KEPT {
            let oldest = state
                .transfers
                .values()
                .filter(|transfer| ended(transfer.state))
                .min_by_key(|transfer| transfer.created_ms)
                .map(|transfer| transfer.id);
            match oldest {
                Some(id) => {
                    state.transfers.remove(&id);
                }
                None => break,
            }
        }
        drop(state);
        self.moved();
    }

    /// Take another device's offer: keep it as a transfer of this device for the person to
    /// answer.
    fn receive(&self, workspace: &str, sender: [u8; 32], offer: Received) -> Result<(), String> {
        if device_key(&offer.device) != Some(sender) {
            return Err(
                "the offer names another sending device than the one that asked".to_owned(),
            );
        }
        if !valid_hash(&offer.hash) {
            return Err("the offer's content address is not one".to_owned());
        }
        let peer = self
            .other_device(sender)
            .map_or_else(|| hex(&sender), |cert| cert.device_label);
        let transfer = Transfer {
            id: bytes_of(&offer.transfer).ok_or("the offer names no transfer")?,
            incoming: true,
            name: offer.name,
            hash: offer.hash,
            total: offer.size,
            transferred: 0,
            state: TransferState::Offered,
            peer_device: sender,
            peer,
            workspace: workspace.to_owned(),
            destination: String::new(),
            created_ms: tilo_clock::unix_now_ms(),
            stopped: Arc::default(),
        };
        self.keep(transfer);
        Ok(())
    }

    /// Keep the file `transfer` names, now that the person said yes: ask the sending device for
    /// its content piece by piece, keep it on this device's shelf under the address offered,
    /// and install it in the workspace's Files at the folder they chose.
    fn accept(
        &self,
        workspace: &str,
        transfer: &str,
        destination: Option<&str>,
    ) -> Result<(), String> {
        let id = bytes_of(transfer).ok_or("that is not a transfer the reading listed")?;
        let (peer, hash, name, total) = {
            let state = self.inner.state.lock_recover();
            let found = state
                .transfers
                .get(&id)
                .filter(|transfer| transfer.incoming)
                .ok_or("that offer is not one this device holds")?;
            if found.state != TransferState::Offered {
                return Err("that offer was answered already".to_owned());
            }
            if found.workspace != workspace {
                return Err(format!(
                    "that offer belongs to another workspace, not {workspace}"
                ));
            }
            (
                found.peer_device,
                found.hash.clone(),
                found.name.clone(),
                found.total,
            )
        };
        let chosen = destination
            .map(str::to_owned)
            .or_else(|| {
                self.inner
                    .state
                    .lock_recover()
                    .remembered
                    .get(workspace)
                    .cloned()
            })
            .unwrap_or_else(|| DEFAULT_FOLDER.to_owned());
        {
            let mut state = self.inner.state.lock_recover();
            if let Some(found) = state.transfers.get_mut(&id) {
                found.state = TransferState::Sending;
                found.destination = chosen.clone();
            }
            state
                .remembered
                .insert(workspace.to_owned(), chosen.clone());
        }
        self.moved();
        let nearby = Arc::clone(&self.inner);
        let workspace = workspace.to_owned();
        tilo_rt::app().spawn(async move {
            let stopped = Arc::clone(
                &nearby
                    .state
                    .lock_recover()
                    .transfers
                    .get(&id)
                    .map_or_else(Arc::default, |found| Arc::clone(&found.stopped)),
            );
            let kept = fetch(&nearby, id, peer, &workspace, &hash, total, &stopped).await;
            let settled = match kept {
                Err(reason) => {
                    tracing::warn!(%reason, "a nearby file was not received");
                    Err(())
                }
                Ok(bytes) => match nearby.workspaces.content_shelf() {
                    Some(shelf) => match shelf.keep(&Nearby::scope(&workspace), &hash, &bytes) {
                        Err(reason) => {
                            tracing::warn!(%reason, "a received file was not kept");
                            Err(())
                        }
                        Ok(()) => {
                            Inner::install(&nearby, &workspace, &hash, &name, &chosen, total).await
                        }
                    },
                    None => Err(()),
                },
            };
            let (state, ok) = match settled {
                Ok(()) => (TransferState::Done, true),
                Err(()) if stopped.load(Ordering::Relaxed) => (TransferState::Declined, false),
                Err(()) => (TransferState::Failed, false),
            };
            Inner::settle(&nearby, id, state);
            let result = json!({"transfer": hex(&id), "ok": ok});
            let _ = call(&nearby, peer, &workspace, RESULT, result).await;
        });
        Ok(())
    }

    /// Stop `transfer`: tell the other device, and stop the work here at its next piece.
    fn cancel(&self, workspace: &str, transfer: &str) -> Result<(), String> {
        let id = bytes_of(transfer).ok_or("that is not a transfer the reading listed")?;
        let peer = {
            let mut state = self.inner.state.lock_recover();
            let found = state
                .transfers
                .get_mut(&id)
                .ok_or("that is not a transfer this device holds")?;
            found.stopped.store(true, Ordering::Relaxed);
            found.peer_device
        };
        let result = json!({"transfer": hex(&id), "ok": false});
        let inner = Arc::clone(&self.inner);
        let workspace = workspace.to_owned();
        tilo_rt::app().spawn(async move {
            let _ = call(&inner, peer, &workspace, RESULT, result).await;
        });
        Inner::settle(&self.inner, id, TransferState::Declined);
        Ok(())
    }

    /// Offer a failed transfer again.
    fn retry(&self, workspace: &str, transfer: &str) -> Result<(), String> {
        let id = bytes_of(transfer).ok_or("that is not a transfer the reading listed")?;
        let (incoming, peer, hash, name, total, state) = {
            let state = self.inner.state.lock_recover();
            let found = state
                .transfers
                .get(&id)
                .ok_or("that is not a transfer this device holds")?;
            (
                found.incoming,
                found.peer_device,
                found.hash.clone(),
                found.name.clone(),
                found.total,
                found.state,
            )
        };
        if state != TransferState::Failed {
            return Err("only a transfer that failed is offered again".to_owned());
        }
        let stopped = Arc::new(AtomicBool::new(false));
        {
            let mut held = self.inner.state.lock_recover();
            if let Some(found) = held.transfers.get_mut(&id) {
                found.state = if incoming {
                    TransferState::Sending
                } else {
                    TransferState::Offered
                };
                found.transferred = 0;
                found.stopped = Arc::clone(&stopped);
            }
        }
        self.moved();
        let inner = Arc::clone(&self.inner);
        let workspace = workspace.to_owned();
        let destination = self
            .inner
            .state
            .lock_recover()
            .remembered
            .get(&workspace)
            .cloned()
            .unwrap_or_else(|| DEFAULT_FOLDER.to_owned());
        tilo_rt::app().spawn(async move {
            if incoming {
                let kept = fetch(&inner, id, peer, &workspace, &hash, total, &stopped).await;
                let settled = match kept {
                    Err(reason) => {
                        tracing::warn!(%reason, "a nearby file was not received again");
                        Err(())
                    }
                    Ok(bytes) => match inner.workspaces.content_shelf() {
                        Some(shelf) => {
                            match shelf.keep(&Nearby::scope(&workspace), &hash, &bytes) {
                                Err(reason) => {
                                    tracing::warn!(%reason, "a received file was not kept");
                                    Err(())
                                }
                                Ok(()) => {
                                    Inner::install(
                                        &inner,
                                        &workspace,
                                        &hash,
                                        &name,
                                        &destination,
                                        total,
                                    )
                                    .await
                                }
                            }
                        }
                        None => Err(()),
                    },
                };
                let state = if settled.is_ok() {
                    TransferState::Done
                } else {
                    TransferState::Failed
                };
                Inner::settle(&inner, id, state);
                let result = json!({"transfer": hex(&id), "ok": settled.is_ok()});
                let _ = call(&inner, peer, &workspace, RESULT, result).await;
            } else {
                let offer = json!({
                    "transfer": hex(&id),
                    "name": name,
                    "hash": hash,
                    "size": total,
                    "device": hex(&inner.workspaces.device_cert().device_public_key),
                    "label": inner.workspaces.device_cert().device_label,
                });
                match call(&inner, peer, &workspace, RECEIVE, offer).await {
                    Ok(_) => Inner::settle(&inner, id, TransferState::Sending),
                    Err(_) => Inner::settle(&inner, id, TransferState::Failed),
                }
            }
        });
        Ok(())
    }

    /// One piece of the content `transfer` names, for the device it was offered to.
    fn content(&self, caller: &Caller, request: Piece) -> Result<ActionOutput, String> {
        let id = bytes_of(&request.transfer).ok_or("that is not a transfer")?;
        let (hash, peer, workspace) = {
            let state = self.inner.state.lock_recover();
            let found = state
                .transfers
                .get(&id)
                .filter(|transfer| !transfer.incoming)
                .ok_or("that is not a transfer this device offers")?;
            (
                found.hash.clone(),
                found.peer_device,
                found.workspace.clone(),
            )
        };
        if caller.device_public_key() != Some(&peer) {
            return Err("that transfer was offered to another device".to_owned());
        }
        let shelf = self
            .inner
            .workspaces
            .content_shelf()
            .ok_or("this device keeps no content")?;
        let bytes = shelf
            .read(&Self::scope(&workspace), &hash)
            .ok_or("this device does not keep that content")?;
        let total = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let start = usize::try_from(request.offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let end = start
            .saturating_add(usize::try_from(request.length).unwrap_or(0))
            .min(bytes.len());
        let piece = bytes.get(start..end).unwrap_or_default();
        Ok(ActionOutput {
            value: Some(json!({
                "size": total,
                "offset": request.offset,
                "bytes": hex::encode(piece),
            })),
            resources: Vec::new(),
        })
    }
}

/// One file an offer carries: its content, name and size.
#[derive(Clone, Debug, Deserialize)]
struct Item {
    hash: String,
    name: String,
    #[serde(default)]
    size: u64,
}

/// Another device's offer.
#[derive(Clone, Debug, Deserialize)]
struct Received {
    transfer: String,
    device: String,
    name: String,
    hash: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    #[allow(dead_code, reason = "the sending device's name, kept for the row")]
    label: String,
}

/// A piece of a transfer's content, asked by the device receiving it.
#[derive(Clone, Debug, Deserialize)]
struct Piece {
    transfer: String,
    #[serde(default)]
    offset: u64,
    #[serde(default)]
    length: u64,
}

/// How a transfer ended, as the other device says it.
#[derive(Clone, Debug, Deserialize)]
struct Ended {
    transfer: String,
    #[serde(default)]
    ok: bool,
}

impl Inner {
    /// Have `id` stand at `state` and tell the followers.
    fn settle(inner: &Arc<Self>, id: [u8; 16], state: TransferState) {
        {
            let mut held = inner.state.lock_recover();
            let Some(found) = held.transfers.get_mut(&id) else {
                return;
            };
            found.state = state;
            if state == TransferState::Done {
                found.transferred = found.total;
            }
        }
        let _ = inner
            .changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }

    /// Read the folders `workspace`'s Files offers once, so the reading can list them: the
    /// workspace's tree's own folders, and the folder a received file lands in by default.
    async fn folders(inner: &Arc<Self>, workspace: &str) {
        let Some(id) = tilo_server_protocol::WorkspaceId::parse(workspace) else {
            return;
        };
        let reading = Arc::clone(inner);
        let workspace_owned = workspace.to_owned();
        let read = blocking(move || {
            let puts = reading.workspaces.copies().puts();
            let handle = tilo_rt::app().handle().clone();
            handle.block_on(puts.read(id, &mut |tree| {
                let mut folders = Vec::new();
                if let Some(root) = tree.resolve_path_ino("") {
                    for (_, kind, name) in tree.readdir(root).unwrap_or_default() {
                        if kind == tilo_fs::node::FileType::Directory
                            && let Ok(name) = std::str::from_utf8(name.as_bytes())
                        {
                            folders.push(name.to_owned());
                        }
                    }
                }
                let _ = &workspace_owned;
                Ok(json!(folders))
            }))
        })
        .await;
        let mut folders = read
            .ok()
            .and_then(|value| serde_json::from_value::<Vec<String>>(value).ok())
            .unwrap_or_default();
        if !folders.iter().any(|folder| folder == DEFAULT_FOLDER) {
            folders.insert(0, DEFAULT_FOLDER.to_owned());
        }
        if inner
            .state
            .lock_recover()
            .destinations
            .insert(workspace.to_owned(), folders)
            .is_none()
        {
            let _ = inner
                .changed
                .send_modify(|revision| *revision = revision.wrapping_add(1));
        }
    }

    /// Write `hash`'s content into `workspace`'s Files at `folder`, under `name`.
    async fn install(
        inner: &Arc<Self>,
        workspace: &str,
        hash: &str,
        name: &str,
        folder: &str,
        size: u64,
    ) -> Result<(), ()> {
        let Some(id) = tilo_server_protocol::WorkspaceId::parse(workspace) else {
            return Err(());
        };
        let (hash, name, folder) = (hash.to_owned(), name.to_owned(), folder.to_owned());
        let writing = Arc::clone(inner);
        let written = blocking(move || {
            let puts = writing.workspaces.copies().puts();
            let handle = tilo_rt::app().handle().clone();
            handle.block_on(puts.change(id, None, &mut |tree| {
                let mtime = tilo_clock::unix_now_ms();
                let mut tried = 0_u32;
                loop {
                    let candidate = free_name(&name, tried);
                    match tree.write_file(&folder, &candidate, (&hash, size), tried > 0, mtime) {
                        Ok(written) => {
                            return Ok(json!({
                                "path": format!("/{folder}/{candidate}"),
                                "replaced": written.replaced,
                            }));
                        }
                        Err(reason) if tried < 99 => {
                            tried = tried.saturating_add(1);
                            tracing::debug!(%reason, "a received file's name is taken");
                        }
                        Err(reason) => return Err(reason),
                    }
                }
            }))
        })
        .await;
        match written {
            Ok(value) => {
                tracing::info!(%value, "a file arrived from one of the person's devices");
                Ok(())
            }
            Err(reason) => {
                tracing::warn!(%reason, "a received file is not installed");
                Err(())
            }
        }
    }
}

/// Run `f` on a thread of its own, off the runtime: what reads or changes a workspace's tree is
/// the thread that asks for it, never a task.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f)
        .await
        .expect("a blocking task of this device's")
}

/// `name`, made free among taken names by the `n`th try: "photo.png", "photo (2).png".
fn free_name(name: &str, n: u32) -> String {
    if n == 0 {
        return name.to_owned();
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, format!(".{extension}")),
        _ => (name, String::new()),
    };
    format!("{stem} ({}){extension}", n.saturating_add(1))
}

/// Whether `state` is one a transfer does not leave.
fn ended(state: TransferState) -> bool {
    matches!(
        state,
        TransferState::Done | TransferState::Declined | TransferState::Failed
    )
}

/// Whether this device's person lets files move to their other devices.
fn permitted() -> bool {
    use settings_store::continuity_privacy::{ContinuityCapability, ON_UNLESS_SET};
    let document = settings_store::load();
    let on = |name: &str| match document.get("continuity") {
        None => ON_UNLESS_SET,
        Some(Value::Object(section)) => section
            .get(name)
            .map_or(ON_UNLESS_SET, |value| value.as_bool() == Some(true)),
        Some(_) => false,
    };
    on("enabled") && on(ContinuityCapability::FileTransfer.key())
}

/// The folder `handle` names, as the person reads it.
fn folder_name(handle: &str) -> String {
    handle
        .rsplit('/')
        .find(|part| !part.is_empty())
        .map_or_else(|| "Workspace files".to_owned(), str::to_owned)
}

/// A fresh transfer id, never all zeroes.
fn random_id() -> [u8; 16] {
    loop {
        let mut id = [0_u8; 16];
        if getrandom::fill(&mut id).is_err() {
            return id;
        }
        if id.iter().any(|byte| *byte != 0) {
            return id;
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

fn bytes_of(text: &str) -> Option<[u8; 16]> {
    let decoded = hex::decode(text).ok()?;
    let bytes = <[u8; 16]>::try_from(decoded).ok()?;
    bytes.iter().any(|byte| *byte != 0).then_some(bytes)
}

/// The device key `text` names, as a device's own key is written.
fn device_key(text: &str) -> Option<[u8; 32]> {
    let decoded = hex::decode(text).ok()?;
    let bytes = <[u8; 32]>::try_from(decoded).ok()?;
    bytes.iter().any(|byte| *byte != 0).then_some(bytes)
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Invoke `action` on the device `device` in `workspace`, waited for until it settles, and
/// answer its output's value.
async fn call(
    inner: &Arc<Inner>,
    device: [u8; 32],
    workspace: &str,
    action: &str,
    arguments: Value,
) -> Result<Option<Value>, String> {
    let actions = inner.workspaces.copies().action_service();
    // A device the daemon holds a link to is asked back over it: the one it asked with. One it
    // does not is dialled at the address its key resolves to.
    let remote = match actions.remote_held(device, workspace) {
        Some(held) => Arc::new(held),
        None => {
            let addr = EndpointId::from_bytes(&device)
                .map(EndpointAddr::new)
                .map_err(|_| "that is not a device key".to_owned())?;
            Arc::new(actions.remote(&inner.workspaces.endpoint(), &addr, workspace))
        }
    };
    let argument = match arguments {
        Value::Object(fields) => InvokeArgument::Json(fields),
        _ => return Err("an offer takes named arguments".to_owned()),
    };
    let request = InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: ActionAddress::new(RESOURCE, action),
        },
        argument,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    match remote.invoke_until_settled(request, FOLLOW_DEADLINE).await {
        Some(Ok(InvocationUpdate::Succeeded { output, .. })) => Ok(output.value),
        Some(Ok(InvocationUpdate::Failed { error, .. })) => Err(error.message),
        Some(Ok(InvocationUpdate::Cancelled { message, .. })) => {
            Err(message.unwrap_or_else(|| "the other device stopped it".to_owned()))
        }
        Some(Ok(update)) => Err(format!("the other device answered {update:?}")),
        Some(Err(error)) => Err(error.to_string()),
        None => Err("the other device's answer was dropped".to_owned()),
    }
}

/// Ask `peer` for `hash`'s content a piece at a time, and answer it whole once it hashes to the
/// address offered.
async fn fetch(
    inner: &Arc<Inner>,
    transfer: [u8; 16],
    peer: [u8; 32],
    workspace: &str,
    hash: &str,
    total: u64,
    stopped: &AtomicBool,
) -> Result<Vec<u8>, String> {
    let mut bytes: Vec<u8> = Vec::new();
    while u64::try_from(bytes.len()).unwrap_or(u64::MAX) < total {
        if stopped.load(Ordering::Relaxed) {
            return Err("stopped".to_owned());
        }
        let offset = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let answer = call(
            inner,
            peer,
            workspace,
            CONTENT,
            json!({
                "transfer": hex(&transfer),
                "offset": offset,
                "length": PIECE_BYTES,
            }),
        )
        .await?;
        let Some(value) = answer else {
            return Err("the other device answered no piece".to_owned());
        };
        let piece = value
            .get("bytes")
            .and_then(Value::as_str)
            .ok_or("the other device answered no bytes")?;
        let piece = hex::decode(piece).map_err(|_| "a piece is not hex".to_owned())?;
        if piece.is_empty() {
            return Err("the other device answered an empty piece".to_owned());
        }
        bytes.extend_from_slice(&piece);
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > total {
            return Err("the other device sent more than it offered".to_owned());
        }
        {
            let mut state = inner.state.lock_recover();
            if let Some(found) = state.transfers.get_mut(&transfer) {
                found.transferred = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            }
        }
        let _ = inner
            .changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
    if blake3::hash(&bytes).to_hex().as_str() != hash {
        return Err("the bytes do not hash to the address offered".to_owned());
    }
    Ok(bytes)
}

/// One action's descriptor, as an agent's catalogue lists it.
fn descriptor(action: &str, label: &str, detail: &str) -> ActionDescriptor {
    let t = crate::i18n::t;
    ActionDescriptor {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: ActionAddress::new(RESOURCE, action),
        resource_kind: "nearby-send".into(),
        resource_label: t("nearby-send-resource"),
        action_label: t(label),
        keywords: Vec::new(),
        icon: None,
        description: Some(t(detail)),
        locus: tilo_actions::ExecutionLocus::default(),
        kind: tilo_actions::OperationKind::Effect,
        contract: ActionContract {
            kind: ActionKind::Outside,
            ..ActionContract::default()
        },
        input: tilo_actions::InputSpec::None,
        params: Vec::new(),
        view: None,
    }
}

impl ActionProvider for Nearby {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let rows: Vec<PlacedAction> = top_level(vec![
            descriptor(SEND, "nearby-send-offer", "nearby-send-offer-detail"),
            descriptor(ACCEPT, "nearby-send-keep", "nearby-send-keep-detail"),
            descriptor(DECLINE, "nearby-send-refuse", "nearby-send-refuse-detail"),
            descriptor(CANCEL, "nearby-send-stop", "nearby-send-stop-detail"),
            descriptor(RETRY, "nearby-send-again", "nearby-send-again-detail"),
        ]);
        offered(PROVIDER, APP, rows, &query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let answered = self.start(caller, &request).map(|value| InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Completed {
                output: ActionOutput {
                    value,
                    resources: Vec::new(),
                },
            },
        });
        Pending::ready(answered)
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }
}

impl Nearby {
    /// Run one call of the resource: what the person's own dispatch hands here, and what
    /// another of their devices asks over the action service.
    fn start(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Option<Value>, ActionCallError> {
        let address = &request.target.address;
        if address.resource != RESOURCE {
            return Err(ActionCallError::UnknownAction);
        }
        let workspace = caller
            .workspace()
            .ok_or_else(|| ActionCallError::Refused {
                reason: "Nearby Send is used in a workspace".to_owned(),
            })?
            .to_owned();
        match address.action.as_str() {
            RECEIVE => {
                if !self.is_our_device(caller) {
                    return Err(ActionCallError::NotPermitted);
                }
                let offer: Received = argument(request)?;
                let sender = caller
                    .device_public_key()
                    .copied()
                    .ok_or(ActionCallError::NotPermitted)?;
                self.receive(&workspace, sender, offer).map_err(refused)?;
                Ok(None)
            }
            CONTENT => {
                let piece: Piece = argument(request)?;
                self.content(caller, piece)
                    .map(|output| output.value)
                    .map_err(refused)
            }
            RESULT => {
                if !self.is_our_device(caller) {
                    return Err(ActionCallError::NotPermitted);
                }
                let result: Ended = argument(request)?;
                if let Some(id) = bytes_of(&result.transfer) {
                    Inner::settle(
                        &self.inner,
                        id,
                        if result.ok {
                            TransferState::Done
                        } else {
                            TransferState::Declined
                        },
                    );
                }
                Ok(None)
            }
            _ if !self.is_our_person(caller) => Err(ActionCallError::NotPermitted),
            SEND => {
                #[derive(Deserialize)]
                struct Send {
                    #[serde(default)]
                    items: Vec<Item>,
                    device: String,
                }
                let send: Send = argument(request)?;
                self.offer(&workspace, send.items, &send.device)
                    .map_err(refused)?;
                Ok(None)
            }
            ACCEPT => {
                #[derive(Deserialize)]
                struct Accept {
                    transfer: String,
                    #[serde(default)]
                    destination: Option<String>,
                }
                let accept: Accept = argument(request)?;
                // The person's own yes is kept as an asked record: the dispatch sends the call
                // here again, with their approve, once they answer.
                let (name, peer) = {
                    let state = self.inner.state.lock_recover();
                    bytes_of(&accept.transfer)
                        .and_then(|id| state.transfers.get(&id))
                        .map_or_else(
                            || (String::new(), String::new()),
                            |found| (found.name.clone(), found.peer.clone()),
                        )
                };
                asked::approved_or_asks(request, || asked::AskedFacts {
                    words: crate::i18n::t_args(
                        "nearby-send-keep-question",
                        &[
                            ("name", name.as_str().into()),
                            ("device", peer.as_str().into()),
                        ],
                    ),
                    ..asked::AskedFacts::default()
                })?;
                self.accept(&workspace, &accept.transfer, accept.destination.as_deref())
                    .map_err(refused)?;
                Ok(None)
            }
            DECLINE => {
                #[derive(Deserialize)]
                struct Transfer {
                    transfer: String,
                }
                let transfer: Transfer = argument(request)?;
                self.cancel(&workspace, &transfer.transfer)
                    .map_err(refused)?;
                Ok(None)
            }
            CANCEL => {
                #[derive(Deserialize)]
                struct Transfer {
                    transfer: String,
                }
                let transfer: Transfer = argument(request)?;
                self.cancel(&workspace, &transfer.transfer)
                    .map_err(refused)?;
                Ok(None)
            }
            RETRY => {
                #[derive(Deserialize)]
                struct Transfer {
                    transfer: String,
                }
                let transfer: Transfer = argument(request)?;
                self.retry(&workspace, &transfer.transfer)
                    .map_err(refused)?;
                Ok(None)
            }
            _ => Err(ActionCallError::UnknownAction),
        }
    }
}

fn refused(reason: String) -> ActionCallError {
    ActionCallError::Refused { reason }
}

/// The argument of `request`, read as `T`.
fn argument<T: serde::de::DeserializeOwned>(request: &InvokeRequest) -> Result<T, ActionCallError> {
    let InvokeArgument::Json(fields) = &request.argument else {
        return Err(ActionCallError::InvalidRequest {
            reason: "Nearby Send takes its arguments as an object".to_owned(),
        });
    };
    serde_json::from_value(Value::Object(fields.clone())).map_err(|error| {
        ActionCallError::InvalidRequest {
            reason: format!("a Nearby Send argument does not read: {error}"),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{folder_name, free_name};

    #[test]
    fn a_taken_name_grows_a_number() {
        assert_eq!(free_name("photo.png", 0), "photo.png");
        assert_eq!(free_name("photo.png", 1), "photo (2).png");
        assert_eq!(free_name("holiday", 2), "holiday (3)");
    }

    #[test]
    fn a_folder_is_named_by_its_last_part() {
        assert_eq!(folder_name("Downloads"), "Downloads");
        assert_eq!(folder_name("/a/b/Screenshots"), "Screenshots");
        assert_eq!(folder_name(""), "Workspace files");
    }
}
