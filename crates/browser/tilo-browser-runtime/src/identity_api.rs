//! The person's devices, as their page administers them
//! ([`tilo_actions::remote::identity_admin`]).
//!
//! The list is the person's record on the host the page's peer is with
//! ([`tilo_server_session::workspaces::LIST_PERSONAL`]): every key their root
//! certified, which each device that signs one adds there, marked where a
//! revocation the host keeps refuses it. Taking a device back is the person's
//! root's to sign, and the root is in this tab: a revoke is handed to the page
//! ([`crate::shell::HostRequest::Revoke`]), which signs it and announces it on
//! every session of theirs, and the host's revocations then mark the device in
//! the list it answers.
//!
//! A read of the list stays open ([`LiveDevices`]): the host answers it
//! once, and it has the list again whenever the host sends the person's
//! record anew, which it does as the record changes, and whenever this page
//! itself changed it. Whatever shows the list follows that read; nothing
//! asks the host again on a timer.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::Value;
use tilo_actions::remote::{Completer, InvocationState, Pending, identity_admin, pending};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_lock::MutexRecover as _;

#[cfg(target_arch = "wasm32")]
use std::sync::Arc;
#[cfg(target_arch = "wasm32")]
use std::sync::atomic::AtomicBool;
#[cfg(target_arch = "wasm32")]
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget,
};
#[cfg(target_arch = "wasm32")]
use tilo_actions::{ActionAddress, ActionOutput};
#[cfg(target_arch = "wasm32")]
use tilo_desktop_surface::session::DesktopSession;
#[cfg(target_arch = "wasm32")]
use tilo_server_session::workspaces::{
    HOST_ACTIONS_PROVIDER, HOST_WORKSPACES_RESOURCE, LIST_PERSONAL, PERSONAL_DEVICE_NAME,
};
#[cfg(target_arch = "wasm32")]
use tilo_session_actions::SessionActions;

#[cfg(target_arch = "wasm32")]
use crate::mcp_server::{Revoked, Revoking};

#[cfg(target_arch = "wasm32")]
/// Why a device's access ends, as a revocation names it.
const REASONS: [&str; 4] = ["retired", "lost", "stolen", "compromised"];

#[cfg(target_arch = "wasm32")]
/// How long the host is given to answer the person's record when this page
/// reads it once more.
const READ_WAIT_MS: u32 = 20_000;

#[cfg(target_arch = "wasm32")]
/// How long a waiting read sleeps at most before it looks again: whoever
/// waits on it checks its own deadline only when woken.
const WAIT_WAKE_MS: u32 = 10_000;

/// The reads of the person's devices this page keeps current.
///
/// The list is the host's to keep. A read of it is answered by the host
/// once and stays open here. It has the list again when the host sends the
/// person's record anew ([`Self::sent`]), and when this page changed the
/// list itself, by approving a device or announcing that one was taken
/// back ([`Self::moved`]): then the record is read from the host once more
/// ([`Self::wait`] says so, [`Self::read`] takes the answer).
#[derive(Default)]
pub(crate) struct LiveDevices(Mutex<Live>);

#[derive(Default)]
struct Live {
    /// Each open read, and whether the host answered it yet.
    reads: HashMap<InvocationId, bool>,
    /// The list as the host last gave it, and how many lists that makes.
    listed: Option<Value>,
    sequence: u64,
    /// Whether this page changed the list since it was last read.
    owed: bool,
    /// Whether the list is being read from the host again now.
    reading: bool,
    /// Whether the page's timer is set to wake whoever waits.
    ticking: bool,
    /// Whoever waits to hear that a read may have changed.
    waiting: Vec<Completer<()>>,
}

impl Live {
    /// Hold `listed` as the list; whether it differs from the one held.
    fn hold(&mut self, listed: Value) -> bool {
        if self.listed.as_ref() == Some(&listed) {
            return false;
        }
        self.listed = Some(listed);
        self.sequence = self.sequence.saturating_add(1);
        true
    }

    fn answered(&self, id: &InvocationId) -> bool {
        self.reads.get(id).copied().unwrap_or(false)
    }
}

/// Tell everyone who waited that a read may have changed.
fn tell(waiting: Vec<Completer<()>>) {
    for told in waiting {
        told.complete(());
    }
}

impl LiveDevices {
    /// Read `id` was asked of the host, which has not answered it yet.
    pub(crate) fn expect(&self, id: InvocationId) {
        self.0.lock_recover().reads.insert(id, false);
    }

    /// Whether `id` is a read of the list.
    pub(crate) fn reads(&self, id: &InvocationId) -> bool {
        self.0.lock_recover().reads.contains_key(id)
    }

    /// The host answered read `id` with `listed`: the read is open from here
    /// on, and has what the page learns of the list after it.
    pub(crate) fn answered(&self, id: InvocationId, listed: Value) {
        let waiting = {
            let mut live = self.0.lock_recover();
            live.reads.insert(id, true);
            live.hold(listed);
            // Just read: nothing of this page's is missing from it.
            live.owed = false;
            std::mem::take(&mut live.waiting)
        };
        tell(waiting);
    }

    /// The host sent the person's record anew: `listed` is the list now.
    pub(crate) fn sent(&self, listed: Value) {
        let waiting = {
            let mut live = self.0.lock_recover();
            if !live.hold(listed) {
                return;
            }
            std::mem::take(&mut live.waiting)
        };
        tell(waiting);
    }

    /// This page changed the list, by approving a device or announcing that
    /// one was taken back: the record is read from the host once more.
    pub(crate) fn moved(&self) {
        let waiting = {
            let mut live = self.0.lock_recover();
            live.owed = true;
            std::mem::take(&mut live.waiting)
        };
        tell(waiting);
    }

    /// The read this page owed is back: `listed` is the list now, `None`
    /// when the host did not answer.
    pub(crate) fn read(&self, listed: Option<Value>) {
        let waiting = {
            let mut live = self.0.lock_recover();
            live.reading = false;
            let changed = listed.is_some_and(|listed| live.hold(listed));
            // Changed again while it was read: whoever waits asks once more.
            if !changed && !live.owed {
                return;
            }
            std::mem::take(&mut live.waiting)
        };
        tell(waiting);
    }

    /// Read `id` as it is now: the list as its latest item, once the host
    /// answered it.
    pub(crate) fn state(&self, id: &InvocationId) -> Option<InvocationState> {
        let live = self.0.lock_recover();
        if !live.answered(id) {
            return None;
        }
        let value = live.listed.clone()?;
        Some(InvocationState::new(
            *id,
            InvocationUpdate::Item {
                invocation: *id,
                sequence: live.sequence,
                value,
            },
        ))
    }

    /// Wait for read `id` to change: what resolves then, and whether the page
    /// owes its host a read of the record first, which it does once for each
    /// change of its own. `None` for a read the host has not answered.
    pub(crate) fn wait(&self, id: &InvocationId) -> Option<(Pending<()>, bool)> {
        let mut live = self.0.lock_recover();
        if !live.answered(id) {
            return None;
        }
        let (told, change) = pending();
        live.waiting.push(told);
        let owes = live.owed && !live.reading;
        if owes {
            live.owed = false;
            live.reading = true;
        }
        drop(live);
        Some((change, owes))
    }

    /// End read `id`; whether it was one.
    pub(crate) fn end(&self, id: &InvocationId) -> bool {
        let waiting = {
            let mut live = self.0.lock_recover();
            if live.reads.remove(id).is_none() {
                return false;
            }
            std::mem::take(&mut live.waiting)
        };
        tell(waiting);
        true
    }

    /// Whoever waits looks again: something may have changed.
    pub(crate) fn wake(&self) {
        let waiting = std::mem::take(&mut self.0.lock_recover().waiting);
        tell(waiting);
    }

    /// Whether to set the page's timer that wakes whoever waits: one is set
    /// at a time.
    pub(crate) fn tick(&self) -> bool {
        !std::mem::replace(&mut self.0.lock_recover().ticking, true)
    }

    /// The page's timer fired: whoever waits looks again.
    pub(crate) fn ticked(&self) {
        self.0.lock_recover().ticking = false;
        self.wake();
    }
}

#[cfg(target_arch = "wasm32")]
thread_local! {
    /// The reads every started desktop of this page keeps current.
    static LIVE: std::cell::RefCell<Vec<std::sync::Weak<LiveDevices>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(target_arch = "wasm32")]
/// This page changed the person's devices: it approved one that asked, or
/// announced that one was taken back. Whatever reads their list here has it
/// read from the host once more.
pub(crate) fn devices_moved() {
    let pages: Vec<Arc<LiveDevices>> =
        LIVE.with_borrow(|pages| pages.iter().filter_map(std::sync::Weak::upgrade).collect());
    for live in pages {
        live.moved();
    }
}

#[cfg(target_arch = "wasm32")]
/// The person's devices as the page's desktop asks for them.
struct PageIdentity {
    /// The page whose session with the workspace's server carries the asks:
    /// looked up for each, as a page that certifies its device again keeps
    /// its id while the session under it is replaced.
    peer: u32,
    workspace: tilo_server_protocol::WorkspaceId,
    /// This page's own device, which the list marks current.
    own: [u8; 32],
    revoked: Revoked,
    /// Raised with each revoke, so the desktop takes it on its next frame.
    woken: Arc<AtomicBool>,
    /// The reads of the list this page keeps current.
    live: Arc<LiveDevices>,
    /// The person's devices as the server sends them whenever they change,
    /// with the session (`peer_sync::generation`) whose link carries them: a
    /// page that certified its device again hears the new session instead.
    sent: std::sync::Mutex<Option<(u64, tilo_session_client::Events)>>,
}

#[cfg(target_arch = "wasm32")]
/// Serve the desktop `session`'s asks of the person's devices from what the
/// server the page `peer` reaches keeps for them; the devices they take back,
/// with `revoked`, for the page to sign. `own` is this page's device.
pub(crate) fn serve(
    session: &DesktopSession,
    peer: u32,
    workspace: tilo_server_protocol::WorkspaceId,
    own: [u8; 32],
    revoked: &Revoked,
    woken: &Arc<AtomicBool>,
) {
    let live = Arc::new(LiveDevices::default());
    LIVE.with_borrow_mut(|pages| {
        pages.retain(|held| held.strong_count() > 0);
        pages.push(Arc::downgrade(&live));
    });
    let identity = PageIdentity {
        peer,
        workspace,
        own,
        revoked: Arc::clone(revoked),
        woken: Arc::clone(woken),
        live,
        sent: std::sync::Mutex::default(),
    };
    identity.follow_server();
    session.serve_person_api(identity_admin::PROVIDER, Arc::new(identity));
}

#[cfg(target_arch = "wasm32")]
/// The person's record on the host: what lists their devices.
fn personal_record() -> RemoteActionTarget {
    RemoteActionTarget {
        provider: HOST_ACTIONS_PROVIDER.to_owned(),
        address: ActionAddress::new(HOST_WORKSPACES_RESOURCE, LIST_PERSONAL),
    }
}

#[cfg(target_arch = "wasm32")]
fn invalid(reason: &str) -> ActionCallError {
    ActionCallError::InvalidRequest {
        reason: reason.to_owned(),
    }
}

#[cfg(target_arch = "wasm32")]
impl PageIdentity {
    /// The workspace's server as this page reaches it now.
    fn api(&self) -> Option<SessionActions> {
        crate::peer_sync::actions_of(self.peer, self.workspace)
    }

    /// Follow the person's devices on the session the page holds now: one the
    /// page certified its device again into is the one the server sends them
    /// on.
    fn follow_server(&self) {
        let generation = crate::peer_sync::generation(self.peer);
        if generation.is_some()
            && self
                .sent
                .lock_recover()
                .as_ref()
                .is_some_and(|(followed, _)| Some(*followed) == generation)
        {
            return;
        }
        let mut sent = self.sent.lock_recover();
        *sent = None;
        let (Some(generation), Some(link)) = (generation, crate::peer_sync::handle_of(self.peer))
        else {
            return;
        };
        let events =
            link.events_where(|event| matches!(event, tilo_session_client::Event::Personal(_)));
        let told = Arc::clone(&self.live);
        // Each list the server sends wakes the reads, which take it.
        link.on_event(&events, Arc::new(move || told.wake()));
        *sent = Some((generation, events));
    }

    /// Name the device `request` names as the person asked it: the host's own
    /// action on their record, which every list of their devices reads.
    fn rename(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let InvokeArgument::Json(argument) = &request.argument else {
            return Pending::ready(Err(invalid(
                "naming a device says which device, and its name",
            )));
        };
        let asked = InvokeRequest {
            target: RemoteActionTarget {
                provider: HOST_ACTIONS_PROVIDER.to_owned(),
                address: ActionAddress::new(HOST_WORKSPACES_RESOURCE, PERSONAL_DEVICE_NAME),
            },
            argument: InvokeArgument::Json(argument.clone()),
            ..request.clone()
        };
        match self.api() {
            Some(api) => api.invoke(caller, asked),
            None => Pending::ready(Err(crate::peer_sync::no_server())),
        }
    }

    /// Hand the page the device `request` takes back, for the reason it
    /// gives.
    fn revoke(&self, request: &InvokeRequest) -> Result<InvocationReceipt, ActionCallError> {
        let InvokeArgument::Json(argument) = &request.argument else {
            return Err(invalid("a revoke names its device"));
        };
        let key = argument
            .get("device")
            .and_then(Value::as_str)
            .filter(|key| hex::decode(key).is_ok_and(|key| key.len() == 32))
            .ok_or_else(|| invalid("a device is named by its key"))?;
        if hex::decode(key).ok().as_deref() == Some(self.own.as_slice()) {
            return Err(ActionCallError::Refused {
                reason: "This browser can't take itself back; do it from another device".into(),
            });
        }
        let reason = argument
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("retired");
        if !REASONS.contains(&reason) {
            return Err(invalid(
                "a device's access ends as retired, lost, stolen or compromised",
            ));
        }
        self.revoked.lock_recover().push(Revoking {
            key: key.to_owned(),
            reason: reason.to_owned(),
        });
        crate::desktop::wake(&self.woken);
        Ok(InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Completed {
                output: ActionOutput::default(),
            },
        })
    }

    /// Take the records the host sent since the last look: the newest is the
    /// list now.
    fn take_sent(&self) {
        self.follow_server();
        let newest = self.sent.lock_recover().as_ref().and_then(|(_, sent)| {
            sent.drain()
                .into_iter()
                .filter_map(|event| match event {
                    tilo_session_client::Event::Personal(record) => Some(record),
                    _ => None,
                })
                .next_back()
        });
        if let Some(record) = newest
            && let Ok(record) = serde_json::to_value(&*record)
        {
            self.live.sent(fleet(Some(&record), &self.own));
        }
    }

    /// Read the person's record from the host once more, after this page
    /// changed it: its answer is the list every open read has from then on.
    fn read_again(&self, caller: &Caller) {
        let asked = InvokeRequest {
            on_behalf_of: None,
            invocation: InvocationId::new(),
            target: personal_record(),
            argument: browser_devices(),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: Some(READ_WAIT_MS),
            appearance: None,
        };
        let (live, own) = (Arc::clone(&self.live), self.own);
        let Some(api) = self.api() else {
            live.read(None);
            return;
        };
        api.invoke(caller, asked).then(move |receipt| {
            let listed = match receipt {
                Some(Ok(InvocationReceipt {
                    status: ReceiptStatus::Completed { output },
                    ..
                })) => Some(fleet(output.value.as_ref(), &own)),
                _ => None,
            };
            live.read(listed);
        });
    }
}

/// The person's record `value` (`tilo_server_session::workspaces::PersonalList`),
/// as the devices it lists ([`identity_admin::FleetDevice`]), `own` marked
/// as this page's. Read from the answer's JSON, each certificate by the
/// fields its JSON form names, rather than parsed whole: a page only shows
/// them, and parsing every certificate's form would weigh on the code every
/// page loads first.
fn fleet(value: Option<&Value>, own: &[u8; 32]) -> Value {
    // The workspaces the record names, by id: an agent's grant says the one it
    // reaches by id, and the list shows it by its name.
    let names: std::collections::BTreeMap<&str, &str> = value
        .and_then(|value| value.get("workspaces"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|workspace| {
            Some((
                workspace.get("workspace")?.as_str()?,
                workspace.get("name")?.as_str()?,
            ))
        })
        .collect();
    let ms = |ns: Option<&Value>| {
        ns.and_then(Value::as_u64)
            .and_then(|ns| i64::try_from(ns / 1_000_000).ok())
    };
    let devices = value
        .and_then(|value| value.get("devices"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|device| {
            let cert = device.get("cert")?;
            let key: [u8; 32] = cert
                .get("device_public_key")?
                .as_array()?
                .iter()
                .map(|byte| byte.as_u64().and_then(|byte| u8::try_from(byte).ok()))
                .collect::<Option<Vec<u8>>>()?
                .try_into()
                .ok()?;
            let capabilities = cert.get("capabilities")?;
            let flag = |name: &str| {
                capabilities
                    .get(name)
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            };
            // The workspace an agent's grant reaches: `all` reaches every one.
            let scope = capabilities.get("agent").and_then(Value::as_str);
            let runs = device.get("agent").and_then(Value::as_str);
            let kind =
                identity_admin::KeyKind::of(scope.is_some() || runs.is_some(), flag("server_key"));
            Some(serde_json::json!({
                "label": device
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .or_else(|| cert.get("device_label").and_then(Value::as_str))?,
                "key_hex": hex::encode(key),
                "fingerprint": fingerprint(&key),
                "current": key == *own,
                "revoked": device.get("revoked").and_then(Value::as_bool).unwrap_or(false),
                "kind": kind,
                "browser": device.get("browser"),
                "agent": runs,
                "issued_ms": ms(cert.get("issued_at_ns")),
                "expires_ms": ms(cert.get("expires_at_ns")),
                "last_active_ms": device.get("last_active_ms").and_then(Value::as_i64),
                "online": device.get("online").and_then(Value::as_bool).unwrap_or(false),
                "workspace": scope.filter(|scope| *scope != "all")
                    .map(|id| names.get(id).copied().unwrap_or(id)),
            }))
        })
        .collect();
    Value::Array(devices)
}

#[cfg(target_arch = "wasm32")]
// The page keeps its browser profile's tab devices where every tab of it
// reads them; storage a browser blocks throws, which reads as none.
#[wasm_bindgen::prelude::wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = localStorage, js_name = getItem, catch)]
    fn local_item(key: &str) -> Result<Option<String>, wasm_bindgen::JsValue>;
}

#[cfg(target_arch = "wasm32")]
/// The ask of the person's record: naming this browser's tab devices, as the
/// page keeps them (`tilo-browser-devices`: the profile's id and each tab
/// key's certificate), so the host lists them as one browser. Passed as the
/// page wrote it; the host checks every certificate. Without them, asked as
/// any device asks.
fn browser_devices() -> InvokeArgument {
    let kept = local_item("tilo-browser-devices").ok().flatten();
    match kept.and_then(|kept| serde_json::from_str::<Value>(&kept).ok()) {
        Some(browser) if browser.is_object() => {
            let mut argument = serde_json::Map::new();
            argument.insert("browser".to_owned(), browser);
            InvokeArgument::Json(argument)
        }
        _ => InvokeArgument::None,
    }
}

/// A key's short form people compare between screens: its first six bytes.
fn fingerprint(key: &[u8; 32]) -> String {
    key.iter()
        .take(6)
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(target_arch = "wasm32")]
impl ActionProvider for PageIdentity {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage {
            exhausted: true,
            ..CataloguePage::default()
        }))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let step = request.target.address.action.as_str();
        if request.target.address.resource != identity_admin::RESOURCE {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        match step {
            identity_admin::REVOKE => Pending::ready(self.revoke(&request)),
            identity_admin::RENAME => self.rename(caller, &request),
            identity_admin::DEVICES => {
                // Asked of the host as the person's record, under the same
                // invocation. Its answer opens a read this page keeps
                // current: the caller is admitted, and has the list as the
                // read's items from here on.
                let invocation = request.invocation;
                let asked = InvokeRequest {
                    target: personal_record(),
                    argument: browser_devices(),
                    ..request
                };
                self.live.expect(invocation);
                let (live, own) = (Arc::clone(&self.live), self.own);
                let Some(api) = self.api() else {
                    live.end(&invocation);
                    return Pending::ready(Err(crate::peer_sync::no_server()));
                };
                api.invoke(caller, asked).map(move |receipt| {
                    let mut receipt = receipt.inspect_err(|_| {
                        live.end(&invocation);
                    })?;
                    match &receipt.status {
                        ReceiptStatus::Completed { output } => {
                            live.answered(invocation, fleet(output.value.as_ref(), &own));
                            receipt.status = ReceiptStatus::Accepted;
                        }
                        // Still read on the host: its answer opens the read.
                        ReceiptStatus::Accepted => {}
                        ReceiptStatus::Failed { .. }
                        | ReceiptStatus::Cancelled { .. }
                        | ReceiptStatus::Rejected { .. } => {
                            live.end(&invocation);
                        }
                    }
                    Ok(receipt)
                })
            }
            _ => Pending::ready(Err(ActionCallError::UnknownAction)),
        }
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        if !self.live.reads(id) {
            return self.api()?.observe(caller, id);
        }
        self.take_sent();
        if let Some(state) = self.live.state(id) {
            return Some(state);
        }
        // The host had not answered the record at admission: its answer
        // opens the read.
        let state = self.api()?.observe(caller, id)?;
        match &state.update {
            InvocationUpdate::Succeeded { output, .. } => {
                self.live
                    .answered(*id, fleet(output.value.as_ref(), &self.own));
                self.live.state(id)
            }
            update if update.is_terminal() => {
                self.live.end(id);
                Some(state)
            }
            _ => Some(state),
        }
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        if !self.live.reads(id) {
            return self.api()?.next_change(caller, id);
        }
        self.take_sent();
        let Some((change, owes)) = self.live.wait(id) else {
            // Not answered yet: the host says when it is.
            return self.api()?.next_change(caller, id);
        };
        if owes {
            self.read_again(caller);
        }
        if self.live.tick() {
            // Bounded, as a waiter's deadline is only checked when woken.
            let live = Arc::clone(&self.live);
            wasm_bindgen_futures::spawn_local(async move {
                crate::browser::sleep_ms(WAIT_WAKE_MS).await;
                live.ticked();
            });
        }
        Some(change)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        if self.live.end(id) {
            return Pending::ready(Ok(()));
        }
        match self.api() {
            Some(api) => api.cancel(caller, id),
            None => Pending::ready(Err(crate::peer_sync::no_server())),
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::needless_pass_by_value,
    reason = "a helper that builds the JSON a provider answered reads best taking it whole"
)]
mod tests {
    use super::*;
    use serde_json::json;
    use tilo_actions::remote::PendingPoll;

    /// Read `id`'s latest item: how many lists it was sent, and the list.
    fn item(live: &LiveDevices, id: &InvocationId) -> (u64, Value) {
        match live.state(id).expect("the read is open").update {
            InvocationUpdate::Item {
                sequence, value, ..
            } => (sequence, value),
            other => panic!("a read's state is its latest item: {other:?}"),
        }
    }

    /// Whether whoever waits on `change` was told to look again.
    fn told(change: &mut Pending<()>) -> bool {
        matches!(change.poll_ready(), PendingPoll::Ready(()))
    }

    /// A read `id` the host answered with one laptop.
    fn a_read() -> (LiveDevices, InvocationId) {
        let live = LiveDevices::default();
        let id = InvocationId::new();
        live.expect(id);
        live.answered(id, json!([{"label": "Laptop"}]));
        (live, id)
    }

    tilo_test_env::scenario! {
        fn a_read_of_the_persons_devices_is_kept_current;

        phase the_hosts_answer_opens_the_read;
        {
            let live = LiveDevices::default();
            let id = InvocationId::new();
            assert!(!live.reads(&id));
            live.expect(id);
            assert!(live.reads(&id));
            assert!(live.state(&id).is_none(), "the provider has not answered yet");
            assert!(live.wait(&id).is_none(), "the provider says when it has");
            live.answered(id, json!([{"label": "Laptop"}]));
            assert_eq!(item(&live, &id), (1, json!([{"label": "Laptop"}])));
        }

        phase a_record_the_host_sends_anew_is_the_reads_next_item;
        // What the host sends as the record changes: a device paired from
        // another of the person's devices, or one taken back there.
        {
            let (live, id) = a_read();
            let (mut change, owes) = live.wait(&id).unwrap();
            assert!(!owes, "nothing of this page's is missing from the list");
            assert!(!told(&mut change));
            let both = json!([{"label": "Laptop"}, {"label": "Browser on iPhone"}]);
            live.sent(both.clone());
            assert!(told(&mut change));
            assert_eq!(item(&live, &id), (2, both.clone()));
            // The same record again changes nothing, and wakes nobody.
            let (mut change, _) = live.wait(&id).unwrap();
            live.sent(both.clone());
            assert!(!told(&mut change));
            assert_eq!(item(&live, &id), (2, both));
        }

        phase what_this_page_changed_is_read_from_the_host_once_more;
        // The page approved a device, or announced that one was taken back:
        // the list was read before that, and showed it only to a page
        // loaded again.
        {
            let (live, id) = a_read();
            let (mut change, _) = live.wait(&id).unwrap();
            live.moved();
            assert!(told(&mut change), "whoever waits looks again");
            let (mut change, owes) = live.wait(&id).unwrap();
            assert!(owes, "the record is read from the provider");
            let (_, again) = live.wait(&id).unwrap();
            assert!(!again, "once: the read under way answers every waiter");
            assert_eq!(item(&live, &id).0, 1, "the list stands until the provider answers");
            live.read(Some(json!([{"label": "Laptop", "revoked": true}])));
            assert!(told(&mut change));
            assert_eq!(
                item(&live, &id),
                (2, json!([{"label": "Laptop", "revoked": true}]))
            );
            let (_, owes) = live.wait(&id).unwrap();
            assert!(!owes, "read: nothing is owed until the page changes it again");
        }

        phase a_change_made_while_the_record_was_read_is_read_too;
        {
            let (live, id) = a_read();
            live.moved();
            let (_, owes) = live.wait(&id).unwrap();
            assert!(owes);
            // A second device approved before the host answered the first read.
            live.moved();
            let (mut change, again) = live.wait(&id).unwrap();
            assert!(!again, "one read at a time");
            // The first read comes back as it was asked, without the second.
            live.read(Some(json!([{"label": "Laptop"}])));
            assert!(told(&mut change), "whoever waits asks once more");
            let (_, owes) = live.wait(&id).unwrap();
            assert!(owes);
        }

        phase a_host_that_does_not_answer_leaves_the_list_as_it_was;
        {
            let (live, id) = a_read();
            live.moved();
            let (mut change, owes) = live.wait(&id).unwrap();
            assert!(owes);
            live.read(None);
            assert!(!told(&mut change), "nothing new to look at");
            assert_eq!(item(&live, &id), (1, json!([{"label": "Laptop"}])));
            let (_, owes) = live.wait(&id).unwrap();
            assert!(!owes, "and the provider is not asked again for it");
        }

        phase a_read_that_ended_is_gone_and_its_waiters_hear;
        {
            let (live, id) = a_read();
            let (mut change, _) = live.wait(&id).unwrap();
            assert!(live.end(&id));
            assert!(told(&mut change));
            assert!(!live.reads(&id));
            assert!(live.state(&id).is_none());
            assert!(!live.end(&id), "it ends once");
        }

        phase the_pages_timer_is_set_once_at_a_time;
        {
            let (live, id) = a_read();
            let (mut change, _) = live.wait(&id).unwrap();
            assert!(live.tick(), "a waiter is woken within a bound");
            assert!(!live.tick(), "by one timer, however many wait");
            live.ticked();
            assert!(told(&mut change), "whoever waits looks again");
            assert!(live.tick(), "and the next wait sets it again");
            let (mut change, _) = live.wait(&id).unwrap();
            live.wake();
            assert!(told(&mut change));
        }
    }

    fn cert(label: &str, key: u8, capabilities: Value, issued: u64, expires: Option<u64>) -> Value {
        let bytes = |byte: u8| Value::Array((0..32).map(|_| Value::from(byte)).collect());
        json!({
            "user_public_key": bytes(1),
            "device_public_key": bytes(key),
            "device_label": label,
            "capabilities": capabilities,
            "issued_at_ns": issued,
            "expires_at_ns": expires,
        })
    }

    fn device(cert: Value, said: Value) -> Value {
        let mut device = json!({"cert": cert});
        for (name, value) in said.as_object().unwrap() {
            device[name] = value.clone();
        }
        device
    }

    tilo_test_env::scenario! {
        fn the_device_list_reads_the_persons_record;

        phase the_device_list_says_what_the_persons_record_kept;
        // The list says of each key what the person's record kept: the agent a
        // computer runs, the one workspace an agent's grant reaches (by name where
        // the record names it), when the key was issued and expires, and when a
        // session of it was last here and whether one is now.
        {
            let workspace = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0";
            let record = json!({
                "workspaces": [{"workspace": workspace, "name": "Studio"}],
                "devices": [
                    device(
                        cert("Laptop", 2, json!({}), 5_000_000_000, None),
                        json!({"last_active_ms": 7_000, "online": true}),
                    ),
                    device(
                        cert("Claude Code on laptop", 3, json!({}), 6_000_000_000, None),
                        json!({"agent": "Claude Code", "last_active_ms": 9_000}),
                    ),
                    device(
                        cert(
                            "Claude Code",
                            4,
                            json!({"agent": workspace, "server_key": false}),
                            8_000_000_000,
                            Some(9_000_000_000),
                        ),
                        json!({}),
                    ),
                ],
            });
            let listed = fleet(Some(&record), &[2; 32]);
            let listed = listed.as_array().unwrap();
            let of = |key: u8| {
                listed
                    .iter()
                    .find(|device| device["key_hex"] == hex::encode([key; 32]))
                    .unwrap_or_else(|| panic!("the key {key} is listed"))
            };

            let laptop = of(2);
            assert_eq!(laptop["label"], "Laptop");
            assert!(laptop["current"].as_bool().unwrap());
            assert_eq!(laptop["online"], true);
            assert_eq!(laptop["last_active_ms"], 7_000);
            assert_eq!(laptop["issued_ms"], 5_000);
            assert_eq!(laptop["kind"], "device");
            assert!(
                laptop["workspace"].is_null(),
                "a device of theirs reaches every workspace of theirs"
            );

            let computer = of(3);
            assert_eq!(computer["agent"], "Claude Code");
            assert_eq!(
                computer["kind"], "agent",
                "a computer an agent runs on is a key acting for the person"
            );
            assert_eq!(computer["last_active_ms"], 9_000);
            assert!(!computer["online"].as_bool().unwrap());

            let grant = of(4);
            assert_eq!(grant["kind"], "agent");
            assert_eq!(
                grant["workspace"], "Studio",
                "the workspace is named, not its id"
            );
            assert_eq!(grant["issued_ms"], 8_000);
            assert_eq!(grant["expires_ms"], 9_000);
        }

        phase a_device_the_person_named_reads_by_that_name;
        // A device the person renamed reads by the name they gave it, not the
        // one it enrolled with; one they named nothing by reads as it enrolled.
        {
            let record = json!({
                "devices": [
                    device(
                        cert("Claude Code on laptop", 6, json!({}), 2_000_000_000, None),
                        json!({"name": "Claude Code on our laptop 2"}),
                    ),
                    device(cert("Phone", 7, json!({}), 3_000_000_000, None), json!({})),
                ],
            });
            let listed = fleet(Some(&record), &[9; 32]);
            let listed = listed.as_array().unwrap();
            assert_eq!(listed[0]["label"], "Claude Code on our laptop 2");
            assert_eq!(listed[1]["label"], "Phone");
        }

        phase a_key_with_nothing_said_of_it_reads_as_it_was;
        // A key the record says nothing more of reads as it was: no time, not
        // online, and no workspace an agent's grant reaches.
        {
            let record = json!({
                "devices": [device(cert("Phone", 5, json!({}), 1_000_000, None), json!({}))],
            });
            let listed = fleet(Some(&record), &[9; 32]);
            let phone = &listed.as_array().unwrap()[0];
            assert!(phone["agent"].is_null());
            assert!(phone["last_active_ms"].is_null());
            assert!(phone["expires_ms"].is_null());
            assert!(phone["workspace"].is_null());
            assert!(!phone["online"].as_bool().unwrap());
            assert!(!phone["current"].as_bool().unwrap());
            assert_eq!(phone["fingerprint"], "05 05 05 05 05 05");
        }
    }
}
