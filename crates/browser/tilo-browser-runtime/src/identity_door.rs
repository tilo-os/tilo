//! The person's devices, as their page administers them
//! ([`tilo_actions::remote::identity_admin`]).
//!
//! The list is the person's record on the host the page's peer is with
//! ([`tilo_relay_session::workspaces::LIST_PERSONAL`]): every key their root
//! certified, which each device that signs one adds there, marked where a
//! revocation the host keeps refuses it. Taking a device back is the person's
//! root's to sign, and the root is in this tab: a revoke is handed to the page
//! ([`crate::shell::HostRequest::Revoke`]), which signs it and announces it on
//! every session of theirs, and the host's revocations then mark the device in
//! the list it answers.

use serde_json::Value;
use tilo_actions::remote::identity_admin;

#[cfg(target_arch = "wasm32")]
use std::sync::Arc;
#[cfg(target_arch = "wasm32")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_arch = "wasm32")]
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus, RemoteActionTarget,
};
#[cfg(target_arch = "wasm32")]
use tilo_actions::{ActionAddress, ActionOutput, InvocationId, InvocationUpdate};
#[cfg(target_arch = "wasm32")]
use tilo_desktop_surface::session::DesktopSession;
#[cfg(target_arch = "wasm32")]
use tilo_lock::MutexRecover as _;
#[cfg(target_arch = "wasm32")]
use tilo_relay_session::workspaces::{HOST_ACTIONS_PROVIDER, HOST_ROOMS_RESOURCE, LIST_PERSONAL};
#[cfg(target_arch = "wasm32")]
use tilo_session_core::remote::SessionActions;

#[cfg(target_arch = "wasm32")]
use crate::agent_door::{Revoked, Revoking};

#[cfg(target_arch = "wasm32")]
/// Why a device's access ends, as a revocation names it.
const REASONS: [&str; 4] = ["retired", "lost", "stolen", "compromised"];

#[cfg(target_arch = "wasm32")]
/// The person's devices as the page's desktop asks for them.
struct PageIdentity {
    host: SessionActions,
    /// This page's own device, which the list marks current.
    own: [u8; 32],
    revoked: Revoked,
    /// Raised with each revoke, so the desktop takes it on its next frame.
    woken: Arc<AtomicBool>,
}

#[cfg(target_arch = "wasm32")]
/// Serve the desktop `session`'s asks of the person's devices from the
/// person's record on the host `host` reaches; the devices they take back,
/// with `revoked`, for the page to sign. `own` is this page's device.
pub(crate) fn serve(
    session: &DesktopSession,
    host: SessionActions,
    own: [u8; 32],
    revoked: &Revoked,
    woken: &Arc<AtomicBool>,
) {
    session.serve_person_door(
        identity_admin::PROVIDER,
        Arc::new(PageIdentity {
            host,
            own,
            revoked: Arc::clone(revoked),
            woken: Arc::clone(woken),
        }),
    );
}

#[cfg(target_arch = "wasm32")]
fn invalid(reason: &str) -> ActionHostError {
    ActionHostError::InvalidRequest {
        reason: reason.to_owned(),
    }
}

#[cfg(target_arch = "wasm32")]
impl PageIdentity {
    /// Hand the page the device `request` takes back, for the reason it
    /// gives.
    fn revoke(&self, request: &InvokeRequest) -> Result<InvocationReceipt, ActionHostError> {
        let InvokeArgument::Json(argument) = &request.argument else {
            return Err(invalid("a revoke names its device"));
        };
        let key = argument
            .get("device")
            .and_then(Value::as_str)
            .filter(|key| hex::decode(key).is_ok_and(|key| key.len() == 32))
            .ok_or_else(|| invalid("a device is named by its key"))?;
        if hex::decode(key).ok().as_deref() == Some(self.own.as_slice()) {
            return Err(ActionHostError::Refused {
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
        self.woken.store(true, Ordering::Release);
        Ok(InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Completed {
                output: ActionOutput::default(),
            },
        })
    }

    /// The host's answer `state` to the person's record, as the devices it
    /// lists.
    fn devices(&self, mut state: InvocationState) -> InvocationState {
        if let InvocationUpdate::Succeeded { output, .. } = &mut state.update {
            output.value = Some(fleet(output.value.as_ref(), &self.own));
        }
        state
    }
}

/// The person's record `value` (`tilo_relay_session::workspaces::PersonalList`),
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
                "label": cert.get("device_label")?.as_str()?,
                "key_hex": hex::encode(key),
                "fingerprint": fingerprint(&key),
                "current": key == *own,
                "revoked": device.get("revoked").and_then(Value::as_bool).unwrap_or(false),
                "can_host": flag("can_act_as_host"),
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
impl ActionHost for PageIdentity {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        Pending::ready(Ok(CataloguePage {
            exhausted: true,
            ..CataloguePage::default()
        }))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let step = request.target.address.action.as_str();
        if request.target.address.resource != identity_admin::RESOURCE {
            return Pending::ready(Err(ActionHostError::UnknownAction));
        }
        match step {
            identity_admin::REVOKE => Pending::ready(self.revoke(&request)),
            identity_admin::DEVICES => {
                // Asked of the host as the person's record, under the same
                // invocation, so its answer is this one's.
                let record = InvokeRequest {
                    target: RemoteActionTarget {
                        provider: HOST_ACTIONS_PROVIDER.to_owned(),
                        address: ActionAddress::new(HOST_ROOMS_RESOURCE, LIST_PERSONAL),
                    },
                    argument: browser_devices(),
                    ..request
                };
                let own = self.own;
                self.host.invoke(caller, record).map(move |receipt| {
                    receipt.map(|mut receipt| {
                        if let ReceiptStatus::Completed { output } = &mut receipt.status {
                            output.value = Some(fleet(output.value.as_ref(), &own));
                        }
                        receipt
                    })
                })
            }
            _ => Pending::ready(Err(ActionHostError::UnknownAction)),
        }
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.host
            .observe(caller, id)
            .map(|state| self.devices(state))
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.host.next_change(caller, id)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionHostError>> {
        self.host.cancel(caller, id)
    }
}

#[cfg(test)]
#[expect(
    clippy::needless_pass_by_value,
    reason = "a helper that builds the JSON a host answered reads best taking it whole"
)]
mod tests {
    use super::*;
    use serde_json::json;

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

    /// The list says of each key what the person's record kept: the agent a
    /// computer runs, the one workspace an agent's grant reaches (by name where
    /// the record names it), when the key was issued and expires, and when a
    /// session of it was last here and whether one is now.
    #[test]
    fn the_device_list_says_what_the_persons_record_kept() {
        let workspace = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0";
        let record = json!({
            "workspaces": [{"workspace": workspace, "name": "Studio"}],
            "devices": [
                device(
                    cert("Laptop", 2, json!({"can_act_as_host": false}), 5_000_000_000, None),
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

    /// A key the record says nothing more of reads as it was: no time, not
    /// online, and no workspace an agent's grant reaches.
    #[test]
    fn a_key_with_nothing_said_of_it_reads_as_it_was() {
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
