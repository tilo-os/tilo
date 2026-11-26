//! Clipboard continuity between the person's devices, carried by this device's daemon.
//!
//! What the person copies on one of their devices reaches their other devices: the session that
//! made the copy shares it here ([`SHARE`]), and this daemon hands it to each of the person's
//! other devices as an ordinary invocation of [`RECEIVE`] over the one action service it already
//! serves ([`tilo_actions/1`], [`super::own_device`]). The receiving daemon keeps what it takes,
//! and its session reads it as the copies another device offers ([`RESOURCE`]).
//!
//! Only the person's own devices take part: what this daemon shares is the copy it was made on,
//! every receiving daemon refuses a copy another device made, and the person's record of their
//! devices names whom this daemon shares with. The setting that gates it is the same one that
//! gated it before: clipboard continuity under the person's master switch.
//!
//! Nothing here binds an endpoint: this daemon holds the copies and the route between them.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use iroh::{EndpointAddr, EndpointId, TransportAddr};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tilo_actions::api::{ActionAddress, PlacedAction};
use tilo_actions::remote::admitted::offered;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
};
use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};
use tilo_app_host::clipboard_continuity::{Reading, RemoteCopy};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::actions::FOLLOW_DEADLINE;
use tilo_workspace_service::workspaces::person_workspace::devices::KeyKind;
use tilo_workspace_service::workspaces::providers::{Providers, Reach};

use super::reading::{Changes, Read, Served};
use super::workspaces::{ServiceWorkspaces, block_on_owned};
use tilo_workspace_service::workspaces::person_workspace;

/// The provider the copies are answered under on this device's dispatch and by another of the
/// person's devices over the action service.
pub const PROVIDER: &str = "so.tilo.clipboard";
/// The application whose grants decide the calls: the person's clipboard is their settings'.
pub const APP: &str = tilo_app_id::apps::SETTINGS;
/// The resource the person's shell reads, as its host declares it.
pub const RESOURCE: &str = tilo_app_host::clipboard_continuity::RESOURCE;

/// Share what this device copied with the person's other devices.
pub const SHARE: &str = "share";
/// Take another device's copies: the action its daemon calls here.
pub const RECEIVE: &str = "receive";

/// The copies one device offers at most, as the clipboard's history already bounds it.
const MAX_ITEMS: usize = 50;
/// The most one copy carries, as the clipboard's history already bounds it.
const MAX_ITEM_BYTES: usize = 256 * 1024;

/// The copies the person's devices offer each other, as its actions publish them.
#[derive(tilo_resource::Row)]
#[row(kind = "clipboard-continuity", label = "Clipboard continuity")]
struct ClipboardRow;

/// What a device shares and takes.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.clipboard")]
pub enum ClipboardAction {
    /// Share what this device copied.
    #[effect(kind = "outside")]
    Share,
    /// Take what another device copied.
    #[effect(kind = "outside")]
    Receive,
}

/// The resource and its two actions, as a call declaring one of them reads them.
fn resources() -> Vec<tilo_actions::ResourceSpec> {
    let mut spec = tilo_resource::offer::<ClipboardAction>(tilo_resource::typed_resource::<
        ClipboardRow,
    >(RESOURCE, "Clipboard continuity"));
    spec.app_id = Some(APP.to_owned());
    vec![spec]
}

/// What the person's other devices offered this one, and where a reading follows it.
pub(crate) struct Clipboard {
    inner: Arc<Inner>,
}

struct Inner {
    workspaces: Arc<ServiceWorkspaces>,
    /// The person this device's person is: whose devices a copy goes to.
    person: [u8; 32],
    state: Mutex<State>,
    /// Moves with every change of the state: what a reading follows.
    changed: tokio::sync::watch::Sender<u64>,
    /// The resource and its actions, as this device publishes them.
    resources: Vec<tilo_actions::ResourceSpec>,
}

#[derive(Default)]
struct State {
    /// What each of the person's other devices last offered, by its public key.
    remote: BTreeMap<[u8; 32], Vec<Copy>>,
    /// What this device last shared: what a device that shares here first is answered with, so a
    /// device that comes online takes what was copied before it did.
    shared: Vec<Copy>,
}

/// One copy a device offers.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Copy {
    mime: String,
    bytes: Vec<u8>,
    created_unix_ms: u64,
    pinned: bool,
}

impl Copy {
    /// This copy as a device carries it.
    fn wire(&self) -> WireCopy {
        WireCopy {
            mime: self.mime.clone(),
            bytes: hex::encode(&self.bytes),
            created_unix_ms: self.created_unix_ms,
            pinned: self.pinned,
        }
    }

    /// The copy `wire` names, if it is one the clipboard carries: a format any device reads, its
    /// bytes inside the bound, and never a secret.
    fn of(wire: &WireCopy) -> Option<Self> {
        if wire.bytes.len() > MAX_ITEM_BYTES.saturating_mul(2)
            || !valid_mime(&wire.mime)
            || wire.mime == tilo_clipboard::SECRET_MIME
        {
            return None;
        }
        let bytes = hex::decode(&wire.bytes).ok()?;
        if bytes.len() > MAX_ITEM_BYTES {
            return None;
        }
        Some(Self {
            mime: wire.mime.clone(),
            bytes,
            created_unix_ms: wire.created_unix_ms,
            pinned: wire.pinned,
        })
    }
}

impl Clipboard {
    /// The copies the person's devices share with this one, kept in the person's own record,
    /// which this device's daemon keeps.
    pub(crate) fn new(workspaces: &Arc<ServiceWorkspaces>) -> Arc<Self> {
        let (changed, _) = tokio::sync::watch::channel(0);
        Arc::new(Self {
            inner: Arc::new(Inner {
                workspaces: Arc::clone(workspaces),
                person: workspaces.device_cert().user_public_key,
                state: Mutex::new(State::default()),
                changed,
                resources: resources(),
            }),
        })
    }

    /// Answer the copies on this device's action service from now on, and to another of the
    /// person's devices.
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
        let read: Arc<Read> =
            Arc::new(move |_caller: &tilo_app_host::services::Caller| json!(reading.reading()));
        let changes: Arc<Changes> = Arc::new(move || changing.inner.changed.subscribe());
        Served::new(RESOURCE, read, changes)
    }

    /// What the person's other devices offer: newest first.
    pub(crate) fn reading(&self) -> Reading {
        if permitted().is_err() {
            return Reading::default();
        }
        let mut copies: Vec<([u8; 32], Copy)> = {
            let state = self.inner.state.lock_recover();
            state
                .remote
                .iter()
                .flat_map(|(device, copies)| copies.iter().map(move |copy| (*device, copy.clone())))
                .collect()
        };
        copies.sort_by(|(left_device, left), (right_device, right)| {
            (right.created_unix_ms, *right_device, &right.mime).cmp(&(
                left.created_unix_ms,
                *left_device,
                &left.mime,
            ))
        });
        Reading {
            copies: copies
                .into_iter()
                .map(|(device, copy)| RemoteCopy {
                    origin: hex::encode(device),
                    mime: copy.mime,
                    bytes: hex::encode(&copy.bytes),
                    created_unix_ms: copy.created_unix_ms,
                    pinned: copy.pinned,
                })
                .collect(),
        }
    }

    /// Whether `caller` is the person this device runs for, on this device.
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

    /// The person's own workspace: where the record of their devices is kept.
    fn person_workspace(&self) -> String {
        collab_types::person_workspace(&self.inner.workspaces.device_cert().user_public_key)
            .to_string()
    }

    /// The person's devices as their record lists them, and where each answers: the record is
    /// this device's daemon's, read where it keeps it.
    fn device_endpoints(&self) -> Vec<(tilo_user_identity::DeviceCert, Vec<std::net::SocketAddr>)> {
        let copies = self.inner.workspaces.copies().clone();
        let person = self.inner.person;
        block_on_owned(async move {
            person_workspace::devices::of(&copies, &person)
                .await
                .certified_at_with_endpoints(&person, tilo_clock::unix_now_ns())
        })
    }

    /// Share `copies` with every other device of the person's their record lists, in
    /// `workspace`: the one the person shared them in, where both devices are members.
    fn share(&self, workspace: &str, copies: &[WireCopy]) -> Result<(), String> {
        permitted()?;
        if copies.is_empty() {
            return Err("a share names at least one copy".to_owned());
        }
        if copies.len() > MAX_ITEMS {
            return Err(format!("a share names at most {MAX_ITEMS} copies"));
        }
        let own = self.inner.workspaces.device_cert();
        let mut carried = Vec::with_capacity(copies.len());
        for wire in copies {
            carried.push(Copy::of(wire).ok_or("a shared copy is not one the clipboard keeps")?);
        }
        self.inner.state.lock_recover().shared.clone_from(&carried);
        // The copies are this device's own: the copy made here is the one another device keeps.
        for (cert, addrs) in self.device_endpoints() {
            if KeyKind::of(&cert) != KeyKind::Device
                || cert.device_public_key == own.device_public_key
            {
                continue;
            }
            relay(
                &self.inner,
                workspace,
                cert.device_public_key,
                &addrs,
                copies,
            );
        }
        Ok(())
    }

    /// Take what the device behind `caller` offered, in `workspace`, replacing what it offered
    /// before.
    fn receive(&self, caller: &Caller, workspace: &str, offer: &WireOffer) -> Result<(), String> {
        permitted()?;
        let sender = *caller
            .device_public_key()
            .ok_or("a copy comes from a device of yours")?;
        if hex(&sender) != offer.device {
            return Err("the copies name another device than the one that sent them".to_owned());
        }
        if offer.copies.len() > MAX_ITEMS {
            return Err(format!("a device offers at most {MAX_ITEMS} copies"));
        }
        let mut carried = Vec::with_capacity(offer.copies.len());
        for wire in &offer.copies {
            carried.push(Copy::of(wire).ok_or("an offered copy is not one the clipboard keeps")?);
        }
        let (changed, own) = {
            let mut state = self.inner.state.lock_recover();
            let first = !state.remote.contains_key(&sender);
            let changed = state.remote.get(&sender) != Some(&carried);
            state.remote.insert(sender, carried);
            // A device that shares here first is told what this one copied before it came: the
            // old session said its copies on each sibling's first word, and nothing else would
            // bring them to it.
            let own = if first && !state.shared.is_empty() {
                state.shared.iter().map(Copy::wire).collect()
            } else {
                Vec::new()
            };
            (changed, own)
        };
        if !own.is_empty()
            && let Some((cert, addrs)) = self
                .device_endpoints()
                .into_iter()
                .find(|(cert, _)| cert.device_public_key == sender)
            && KeyKind::of(&cert) == KeyKind::Device
        {
            relay(&self.inner, workspace, sender, &addrs, &own);
        }
        if changed {
            self.inner
                .changed
                .send_modify(|revision| *revision = revision.wrapping_add(1));
        }
        Ok(())
    }
}

/// What one device offers as it shares: the copies it holds now.
#[derive(Clone, Debug, Deserialize)]
struct WireOffer {
    /// The device that sent them, for a device's share; absent when the person shares here.
    #[serde(default)]
    device: String,
    #[serde(default)]
    copies: Vec<WireCopy>,
}

/// One copy on the wire.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct WireCopy {
    mime: String,
    bytes: String,
    #[serde(default)]
    created_unix_ms: u64,
    #[serde(default)]
    pinned: bool,
}

/// Whether the person lets what they copy continue on their other devices: clipboard continuity
/// under the master switch, each on unless they turned it off.
///
/// # Errors
/// Either switch is off, said as Settings words it.
pub fn permitted() -> Result<(), String> {
    use settings_store::continuity_privacy::{ContinuityCapability, ON_UNLESS_SET};
    let document = settings_store::load();
    let on = |name: &str| match document.get("continuity") {
        None => ON_UNLESS_SET,
        Some(Value::Object(section)) => section
            .get(name)
            .map_or(ON_UNLESS_SET, |value| value.as_bool() == Some(true)),
        Some(_) => false,
    };
    (on("enabled") && on(ContinuityCapability::Clipboard.key()))
        .then_some(())
        .ok_or_else(|| "clipboard continuity is turned off in this device's settings".to_owned())
}

/// Send `copies` to `target` at `addrs`, in `workspace`, off the caller's thread.
fn relay(
    inner: &Arc<Inner>,
    workspace: &str,
    target: [u8; 32],
    addrs: &[std::net::SocketAddr],
    copies: &[WireCopy],
) {
    let inner = Arc::clone(inner);
    let workspace = workspace.to_owned();
    let addrs = addrs.to_vec();
    let arguments = json!({
        "device": hex(&inner.workspaces.device_cert().device_public_key),
        "copies": copies,
    });
    tilo_rt::app().spawn(async move {
        let answered = call(&inner, target, &addrs, &workspace, RECEIVE, arguments).await;
        if let Err(reason) = answered {
            tracing::debug!(%reason, "a device did not take a copy");
        }
    });
}

/// Whether `mime` is a format the clipboard carries: a type and a subtype, read by any device.
fn valid_mime(mime: &str) -> bool {
    mime.len() <= 128
        && mime.contains('/')
        && mime.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'/' | b'.' | b'-' | b'+' | b'_' | b';' | b'=')
        })
}

/// Invoke `action` on the device `device` at `addrs`, in `workspace`, waited for until it
/// settles.
async fn call(
    inner: &Arc<Inner>,
    device: [u8; 32],
    addrs: &[std::net::SocketAddr],
    workspace: &str,
    action: &str,
    arguments: Value,
) -> Result<Option<Value>, String> {
    let actions = inner.workspaces.copies().action_service();
    // The other device is dialled where the person's record says its endpoint answers, or by its
    // key alone when no address is kept.
    let id = EndpointId::from_bytes(&device).map_err(|_| "that is not a device key")?;
    let addr = EndpointAddr::from_parts(id, addrs.iter().copied().map(TransportAddr::Ip));
    let remote = Arc::new(actions.remote(&inner.workspaces.endpoint(), &addr, workspace));
    let argument = match arguments {
        Value::Object(fields) => InvokeArgument::Json(fields),
        _ => return Err("a share takes named arguments".to_owned()),
    };
    let request = InvokeRequest {
        invocation: InvocationId::new(),
        target: tilo_actions::remote::RemoteActionTarget {
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

fn hex(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

impl ActionProvider for Clipboard {
    fn published(&self) -> Vec<tilo_actions::ResourceSpec> {
        self.inner.resources.clone()
    }

    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        // The copies are the person's own, shared and taken by their daemons: no catalogue lists
        // a call through which anyone else reaches them.
        let rows: Vec<PlacedAction> = Vec::new();
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

impl Clipboard {
    /// Run one call of the resource: what the person's own dispatch hands here, and what another
    /// of their devices asks over the action service.
    fn start(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Option<Value>, ActionCallError> {
        let address = &request.target.address;
        if address.resource != RESOURCE {
            return Err(ActionCallError::UnknownAction);
        }
        match address.action.as_str() {
            RECEIVE => {
                if !self.is_our_device(caller) {
                    return Err(ActionCallError::NotPermitted);
                }
                let offer: WireOffer = argument(request)?;
                let workspace = caller
                    .workspace()
                    .map_or_else(|| self.person_workspace(), str::to_owned);
                self.receive(caller, &workspace, &offer).map_err(refused)?;
                Ok(None)
            }
            SHARE => {
                // Only the person at this device shares: an app or an agent they run puts what
                // it copies on the clipboard, which the session shares as theirs, and asks for
                // nothing further.
                if !self.is_our_person(caller)
                    || caller.acting().app().is_some()
                    || caller.launched_app().is_some()
                {
                    return Err(ActionCallError::NotPermitted);
                }
                let offer: WireOffer = argument(request)?;
                let workspace = caller
                    .workspace()
                    .map_or_else(|| self.person_workspace(), str::to_owned);
                self.share(&workspace, &offer.copies).map_err(refused)?;
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
            reason: "clipboard continuity takes its arguments as an object".to_owned(),
        });
    };
    serde_json::from_value(Value::Object(fields.clone())).map_err(|error| {
        ActionCallError::InvalidRequest {
            reason: format!("a clipboard continuity argument does not read: {error}"),
        }
    })
}

#[cfg(test)]
#[path = "clipboard_tests.rs"]
pub(super) mod tests;
