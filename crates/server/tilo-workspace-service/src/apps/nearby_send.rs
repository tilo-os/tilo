//! Nearby Send as the shell publishes it: the devices this one can hand a file
//! to, the offers another device made back, and what became of each transfer.
//!
//! One engine carries every file that leaves this device for another of the
//! person's ([`NearbyTransferServices`]); Files reaches it on its certified
//! route, and so does this resource. The resource is one more client of that
//! engine, never a second way around the grant that decides what may be sent:
//! every call is made in the workspace in front, as the certified Files route,
//! and the engine binds each handle to that one connection exactly as it binds
//! Files'. A shell, an agent and a script list what is in reach and act on a
//! transfer through the same published actions a person's press does.
//!
//! Handles are the engine's own capability tokens, carried in the reading as
//! their hex address and handed back by an action. They are renewed whenever
//! the reading is read, so a row the person sees is one they may act on.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_lock::MutexRecover as _;
use tilo_resource::Effect;

use runtime_collab::{
    NearbyTargetHandle, NearbyTransferCommand, NearbyTransferDirection, NearbyTransferHandle,
    NearbyTransferPhase, NearbyTransferReply, NearbyTransferSnapshot, NearbyTransferStatus,
};

use super::services::{NearbyTransferRequestContext, NearbyTransferServices};

/// The resource the shell reads it as.
pub const RESOURCE: &str = "session:nearby-send";

/// The device-facing name the shell's views know it by (`device.nearby_send`),
/// which `device-surface.json` declares.
pub const SURFACE: &str = "nearby-send";

/// The one connection this resource's handles are bound to. It is not a peer
/// id on the wire: the engine binds a handle to a peer and a channel, and this
/// resource keeps one of its own beside the connections Files opens.
const RESOURCE_PEER: u64 = 0x5EED_5EED_5EED_0001;
const CHANNEL: &str = "session:nearby-send";

/// How many files one send may offer; each becomes a transfer of its own.
const MAX_ITEMS: usize = 32;

/// The app whose route the engine admits, the certified Files application.
const FILES_APP_ID: &str = tilo_app_id::apps::FILE_EXPLORER;

/// What a nearby device is.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    /// Another of the person's own devices, reached over the device fabric.
    #[default]
    Device,
    /// A receiver at an address the host names, as a lab's stands: the
    /// AirDrop-style hand-off that needs no account on either end.
    LabReceiver,
}

/// One device a file can be handed to.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct NearbyDevice {
    /// The name the device goes by.
    pub name: String,
    pub kind: DeviceKind,
    /// Whether it is here now.
    pub reachable: bool,
    /// Its address, for a lab receiver; empty for the person's own devices,
    /// which the fabric reaches without one.
    #[serde(default)]
    pub address: String,
    /// Its port, for a lab receiver; 0 otherwise.
    #[serde(default)]
    pub port: u16,
    /// The handle `send` names it by.
    pub handle: String,
}

/// A folder a received file can land in.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct NearbyDestination {
    /// The folder's name as the person reads it.
    pub name: String,
    /// The handle `accept` names it by.
    pub handle: String,
    /// Whether the last accept here remembered it.
    #[serde(default)]
    pub remembered: bool,
}

/// Where a transfer stands, in the words the shell shows.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum TransferState {
    /// Waiting for the person at either end: an offer not yet decided.
    #[default]
    Offered,
    /// Its bytes are moving.
    Sending,
    /// Every byte landed.
    Done,
    /// The person at either end turned it down or stopped it.
    Declined,
    /// It could not finish.
    Failed,
}

/// One transfer, live or in the history the engine keeps.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct NearbyTransferRow {
    pub state: TransferState,
    /// How much of it landed, in percent.
    pub progress: u8,
    /// The other device's name.
    pub peer: String,
    /// Where a received file landed, when this device accepted it; empty
    /// otherwise.
    #[serde(default)]
    pub destination: String,
    /// When this device first saw it, in nanoseconds since the Unix epoch.
    #[serde(default)]
    pub time: u64,
    /// Whether it comes to this device.
    pub incoming: bool,
    /// The file's name.
    pub name: String,
    /// The handle `accept`, `decline`, `cancel` and `retry` name it by.
    pub handle: String,
}

/// What the resource reads: what is in reach, where a received file may land,
/// and what was transferred.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct Reading {
    #[serde(default)]
    pub devices: Vec<NearbyDevice>,
    #[serde(default)]
    pub destinations: Vec<NearbyDestination>,
    #[serde(default)]
    pub transfers: Vec<NearbyTransferRow>,
}

/// The resource's declared type.
#[derive(tilo_resource::Row)]
#[row(
    kind = "session",
    label = "Nearby Send",
    effects = NearbySendAction
)]
pub struct NearbySendRoot;

/// What the resource offers: hand a file to a device, answer an offer, stop or
/// offer again a transfer, and share a resource with people.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.device.nearby-send")]
pub enum NearbySendAction {
    /// Send files to a nearby device
    ///
    /// Offers each file to the device; the person there chooses where it
    /// lands, so nothing arrives unasked.
    #[effect(kind = "outside")]
    #[effect(own_device)]
    Send {
        /// The files, by their path in the workspace's files.
        #[param("The files to send")]
        items: Vec<String>,
        /// The device, as the reading lists it.
        #[param("The device")]
        device: String,
    },
    /// Keep a file a nearby device offered
    ///
    /// The receiving person's choice of where it lands; without one, the
    /// folder the last accept remembered.
    #[effect(kind = "outside")]
    #[effect(own_device)]
    Accept {
        /// The transfer, as the reading lists it.
        #[param("The transfer")]
        transfer: String,
        /// The folder it lands in, as the reading lists them.
        #[param("Where it lands")]
        destination: Option<String>,
    },
    /// Turn down a file a nearby device offered
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    Decline {
        /// The transfer, as the reading lists it.
        #[param("The transfer")]
        transfer: String,
    },
    /// Stop a transfer, sent or received
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    Cancel {
        /// The transfer, as the reading lists it.
        #[param("The transfer")]
        transfer: String,
    },
    /// Offer again a transfer that failed
    #[effect(kind = "outside")]
    #[effect(own_device)]
    Retry {
        /// The transfer, as the reading lists it.
        #[param("The transfer")]
        transfer: String,
    },
    /// Share a resource with people, opening the invitation sheet
    #[effect(kind = "outside")]
    Share {
        /// The resource, by the id the catalogue lists it under.
        #[param("The resource")]
        resource: String,
    },
}

/// The resource as the shell's views read and press it (`device.nearby_send`).
#[must_use]
pub fn surface() -> tilo_actions::surface::Surface {
    tilo_resource::surface::reading_surface::<Reading, NearbySendAction>(SURFACE, "Nearby Send")
}

/// The resource as the session's services publish it.
#[must_use]
pub fn resource(label: impl Into<String>) -> ResourceSpec {
    // The verbs go with the resource: a press, a script and an agent reach the same ones, and
    // what moved is personal to the person the engine answers for.
    tilo_resource::offer::<NearbySendAction>(
        tilo_resource::LiveRow::typed::<NearbySendRoot>(RESOURCE, label)
            .into_resource()
            .with_personal_reading(),
    )
}

/// A receiver at an address this host names, as a lab stands one up: no
/// account and no fabric on either end, only an address, a port and the
/// receiving program's certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabReceiver {
    pub name: String,
    pub address: String,
    pub port: u16,
}

impl LabReceiver {
    /// The handle the reading lists it by.
    #[must_use]
    pub fn handle(&self) -> String {
        "lab_receiver".to_owned()
    }
}

/// What an invocation answered: an action that finished as it ran, or one the
/// engine took and whose progress the reading shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Invoked {
    #[default]
    Accepted,
    Done(Option<Value>),
}

/// How a resource is shared: the session opens its invitation sheet, or says why it cannot.
type ShareHook = Box<dyn Fn(&str) -> Result<(), String>>;

/// The session's Nearby Send engine, published as a resource.
///
/// The workspace is asked for each call, so a send is checked against the
/// Files grants of the workspace the person is in now, whichever was in front
/// when the handle was handed out.
pub struct NearbySend {
    service: Arc<dyn NearbyTransferServices>,
    person: String,
    workspace: Box<dyn Fn() -> String>,
    /// How a resource is shared: the session opens its invitation sheet.
    share: ShareHook,
    /// A lab receiver this host names, when one is set up beside it.
    lab: Option<LabReceiver>,
    /// When each transfer behind a handle was first seen here.
    seen: Mutex<BTreeMap<String, u64>>,
    /// Where each transfer this resource accepted landed, by its handle.
    landed: Mutex<BTreeMap<String, String>>,
}

impl NearbySend {
    /// `service`, acting as `person` in the workspace `workspace` names now,
    /// with `share` opening the invitation sheet for a resource.
    pub fn new(
        service: Arc<dyn NearbyTransferServices>,
        person: impl Into<String>,
        workspace: impl Fn() -> String + 'static,
        share: impl Fn(&str) -> Result<(), String> + 'static,
    ) -> Self {
        Self {
            service,
            person: person.into(),
            workspace: Box::new(workspace),
            share: Box::new(share),
            lab: None,
            seen: Mutex::new(BTreeMap::new()),
            landed: Mutex::new(BTreeMap::new()),
        }
    }

    /// Name the lab receiver at `lab` beside this device, listed as a
    /// [`DeviceKind::LabReceiver`].
    #[must_use]
    pub fn with_lab_receiver(mut self, lab: LabReceiver) -> Self {
        self.lab = Some(lab);
        self
    }

    /// The context this resource's calls arrive in: the certified Files route,
    /// in the workspace in front.
    fn context(&self) -> NearbyTransferRequestContext {
        NearbyTransferRequestContext::authenticated(
            RESOURCE_PEER,
            FILES_APP_ID,
            (self.workspace)(),
            CHANNEL,
            Some(self.person.clone()),
        )
    }

    /// What the engine shows this resource now.
    fn snapshot(&self) -> Result<NearbyTransferSnapshot, ApiError> {
        match self
            .service
            .command(&self.context(), NearbyTransferCommand::Snapshot)
            .map_err(refused)?
        {
            NearbyTransferReply::Snapshot { snapshot } => Ok(snapshot),
            other => Err(refused(format!(
                "the Nearby Send engine answered a snapshot with {other:?}"
            ))),
        }
    }

    /// Whether the host takes a file at an address, for a lab receiver.
    fn air_drop_available(&self) -> bool {
        matches!(
            self.service
                .command(&self.context(), NearbyTransferCommand::AirDropAvailability),
            Ok(NearbyTransferReply::AirDropAvailability { available: true })
        )
    }

    /// Every device a file can be handed to now, then the ones it may land in
    /// here, then the transfers: one snapshot read, its handles renewed.
    ///
    /// # Errors
    /// The engine cannot show what it holds.
    pub fn read(&self) -> Result<Reading, ApiError> {
        let snapshot = self.snapshot()?;
        let now = nanos();
        let mut reading = Reading::default();
        for target in &snapshot.targets {
            reading.devices.push(NearbyDevice {
                name: target.label.clone(),
                kind: DeviceKind::Device,
                reachable: target.online,
                address: String::new(),
                port: 0,
                handle: hex(target.handle.as_bytes()),
            });
        }
        if let Some(lab) = &self.lab {
            reading.devices.push(NearbyDevice {
                name: lab.name.clone(),
                kind: DeviceKind::LabReceiver,
                reachable: self.air_drop_available(),
                address: lab.address.clone(),
                port: lab.port,
                handle: lab.handle(),
            });
        }
        for destination in &snapshot.destinations {
            reading.destinations.push(NearbyDestination {
                name: destination.label.clone(),
                handle: hex(destination.handle.as_bytes()),
                remembered: destination.remembered_for_session,
            });
        }
        let mut seen = self.seen.lock_recover();
        let landed = self.landed.lock_recover();
        for transfer in &snapshot.transfers {
            let handle = hex(transfer.handle.as_bytes());
            let first = *seen.entry(handle.clone()).or_insert(now);
            reading.transfers.push(NearbyTransferRow {
                state: state_of(transfer),
                progress: progress_of(transfer),
                peer: transfer.device_label.clone(),
                destination: landed.get(&handle).cloned().unwrap_or_default(),
                time: first,
                incoming: transfer.direction == NearbyTransferDirection::Receiving,
                name: transfer.name.clone(),
                handle,
            });
        }
        drop(landed);
        drop(seen);
        Ok(reading)
    }

    /// Run one of the resource's actions.
    ///
    /// # Errors
    /// The action is not one the resource publishes, its arguments do not
    /// type, or the engine refused it.
    pub fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Invoked, ApiError> {
        if resource != RESOURCE {
            return Err(ApiError::Unknown(ActionAddress::new(resource, action)));
        }
        let decoded = NearbySendAction::decode(action, arguments)
            .map_err(|error| ApiError::InvalidArguments(format!("{action}: {error}")))?;
        match decoded {
            NearbySendAction::Send { items, device } => self.send(&items, &device),
            NearbySendAction::Accept {
                transfer,
                destination,
            } => self.accept(&transfer, destination.as_deref()),
            NearbySendAction::Decline { transfer } => {
                let handle = transfer_handle(&transfer)?;
                self.answer(NearbyTransferCommand::Decide {
                    handle,
                    accept: false,
                    destination: None,
                    remember_for_session: false,
                })
            }
            NearbySendAction::Cancel { transfer } => {
                let handle = transfer_handle(&transfer)?;
                self.answer(NearbyTransferCommand::Cancel { handle })
            }
            NearbySendAction::Retry { transfer } => {
                let handle = transfer_handle(&transfer)?;
                self.answer(NearbyTransferCommand::Retry { handle })
            }
            NearbySendAction::Share { resource } => {
                (self.share)(&resource).map_err(refused)?;
                Ok(Invoked::Done(None))
            }
        }
    }

    /// Offer each of `items` to `device`, which is either one of the person's
    /// devices, by the handle the reading lists, or the lab receiver.
    fn send(&self, items: &[String], device: &str) -> Result<Invoked, ApiError> {
        if items.is_empty() {
            return Err(ApiError::InvalidArguments(
                "a send names at least one file".to_owned(),
            ));
        }
        if items.len() > MAX_ITEMS {
            return Err(ApiError::InvalidArguments(format!(
                "a send offers at most {MAX_ITEMS} files"
            )));
        }
        if let Some(lab) = &self.lab
            && device == lab.handle()
        {
            if !self.air_drop_available() {
                return Err(refused(
                    "this host hands no file to a lab receiver".to_owned(),
                ));
            }
            for item in items {
                self.answer(NearbyTransferCommand::SendAirDrop {
                    source_path: item.as_bytes().to_vec(),
                    address: lab.address.clone(),
                    port: lab.port,
                })?;
            }
            return Ok(Invoked::Done(None));
        }
        let target = NearbyTargetHandle::from_issued_bytes(bytes_of(device)?);
        let snapshot = self.snapshot()?;
        if !snapshot.targets.iter().any(|one| one.handle == target) {
            return Err(refused(
                "that device is no longer open to a send".to_owned(),
            ));
        }
        for item in items {
            self.answer(NearbyTransferCommand::Send {
                target,
                source_path: item.as_bytes().to_vec(),
            })?;
        }
        Ok(Invoked::Done(None))
    }

    /// Keep the offer `transfer`, landing it in `destination` or the folder
    /// the engine remembered for this connection.
    fn accept(&self, transfer: &str, destination: Option<&str>) -> Result<Invoked, ApiError> {
        let handle = transfer_handle(transfer)?;
        let snapshot = self.snapshot()?;
        let chosen = match destination {
            Some(named) => {
                let wanted = bytes_of(named)?;
                snapshot
                    .destinations
                    .iter()
                    .find_map(|one| (one.handle.as_bytes() == &wanted).then_some(one.handle))
            }
            None => snapshot
                .destinations
                .iter()
                .find(|one| one.remembered_for_session)
                .or_else(|| snapshot.destinations.first())
                .map(|one| one.handle),
        };
        let Some(chosen) = chosen else {
            return Err(refused(
                "no folder here takes the file; Files grants this workspace none".to_owned(),
            ));
        };
        let label = snapshot
            .destinations
            .iter()
            .find(|one| one.handle == chosen)
            .map(|one| one.label.clone())
            .unwrap_or_default();
        self.answer(NearbyTransferCommand::Decide {
            handle,
            accept: true,
            destination: Some(chosen),
            remember_for_session: true,
        })?;
        self.landed
            .lock_recover()
            .insert(transfer.to_owned(), label);
        Ok(Invoked::Accepted)
    }

    /// Run one transfer command, reading a refusal as the reason it gives.
    fn answer(&self, command: NearbyTransferCommand) -> Result<Invoked, ApiError> {
        match self.service.command(&self.context(), command) {
            Ok(NearbyTransferReply::Queued { .. } | NearbyTransferReply::Accepted) => {
                Ok(Invoked::Done(None))
            }
            Ok(NearbyTransferReply::Rejected { reason }) | Err(reason) => Err(refused(reason)),
            Ok(other) => Err(refused(format!(
                "the Nearby Send engine answered with {other:?}"
            ))),
        }
    }
}

/// Why the engine would not do it, as the caller reads it.
fn refused(reason: String) -> ApiError {
    ApiError::Refused {
        code: "nearby-send".to_owned(),
        reason,
    }
}

/// The bytes a handle's hex address names.
fn bytes_of(handle: &str) -> Result<[u8; 16], ApiError> {
    let mut bytes = [0_u8; 16];
    let Ok(decoded) = hex::decode(handle) else {
        return Err(ApiError::InvalidArguments(format!(
            "{handle} is not a handle the reading listed"
        )));
    };
    if decoded.len() != bytes.len() || decoded.iter().all(|byte| *byte == 0) {
        return Err(ApiError::InvalidArguments(format!(
            "{handle} is not a handle the reading listed"
        )));
    }
    bytes.copy_from_slice(&decoded);
    Ok(bytes)
}

fn transfer_handle(transfer: &str) -> Result<NearbyTransferHandle, ApiError> {
    bytes_of(transfer).map(NearbyTransferHandle::from_issued_bytes)
}

fn hex(bytes: &[u8; 16]) -> String {
    hex::encode(bytes)
}

/// The state a row shows for `transfer`.
fn state_of(transfer: &NearbyTransferStatus) -> TransferState {
    match transfer.phase {
        NearbyTransferPhase::AwaitingConsent | NearbyTransferPhase::AwaitingDestination => {
            TransferState::Offered
        }
        NearbyTransferPhase::Queued | NearbyTransferPhase::Transferring => TransferState::Sending,
        NearbyTransferPhase::Completed => TransferState::Done,
        NearbyTransferPhase::Cancelled => TransferState::Declined,
        NearbyTransferPhase::Failed => TransferState::Failed,
    }
}

/// How much of `transfer` landed, in percent.
fn progress_of(transfer: &NearbyTransferStatus) -> u8 {
    if transfer.total_bytes == 0 {
        return 0;
    }
    let landed = u128::from(transfer.transferred_bytes);
    let total = u128::from(transfer.total_bytes);
    let percent = landed
        .checked_mul(100)
        .and_then(|scaled| scaled.checked_div(total))
        .unwrap_or_default()
        .min(100);
    u8::try_from(percent).unwrap_or(100)
}

/// The wall clock, in nanoseconds since the Unix epoch.
fn nanos() -> u64 {
    tilo_clock::wall()
        .duration_since(tilo_clock::time::UNIX_EPOCH)
        .map(|since| u64::try_from(since.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
