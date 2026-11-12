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

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;
use tilo_actions::remote::identity_admin::{self, FleetDevice};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, ActionOutput, InvocationId, InvocationUpdate};
use tilo_desktop_surface::session::DesktopSession;
use tilo_lock::MutexRecover as _;
use tilo_relay_session::workspaces::{
    HOST_ACTIONS_PROVIDER, HOST_ROOMS_RESOURCE, LIST_PERSONAL, PersonalList,
};
use tilo_session_core::remote::SessionActions;

use crate::agent_door::{Revoked, Revoking};

/// Why a device's access ends, as a revocation names it.
const REASONS: [&str; 4] = ["retired", "lost", "stolen", "compromised"];

/// The person's devices as the page's desktop asks for them.
struct PageIdentity {
    host: SessionActions,
    /// This page's own device, which the list marks current.
    own: [u8; 32],
    revoked: Revoked,
    /// Raised with each revoke, so the desktop takes it on its next frame.
    woken: Arc<AtomicBool>,
}

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

fn invalid(reason: &str) -> ActionHostError {
    ActionHostError::InvalidRequest {
        reason: reason.to_owned(),
    }
}

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
            output.value = Some(fleet(output.value.take(), &self.own));
        }
        state
    }
}

/// The person's record `value`, as the devices it lists, `own` marked as
/// this page's.
fn fleet(value: Option<Value>, own: &[u8; 32]) -> Value {
    let devices = value
        .and_then(|value| serde_json::from_value::<PersonalList>(value).ok())
        .map(|list| {
            list.devices
                .into_iter()
                .map(|device| FleetDevice {
                    label: device.cert.device_label.clone(),
                    key_hex: hex::encode(device.cert.device_public_key),
                    fingerprint: fingerprint(&device.cert.device_public_key),
                    current: device.cert.device_public_key == *own,
                    revoked: device.revoked,
                    can_host: device.cert.capabilities.can_act_as_host,
                    kind: identity_admin::KeyKind::of(
                        device.cert.capabilities.agent.is_some(),
                        device.cert.capabilities.server_key,
                    ),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    serde_json::to_value(devices).unwrap_or(Value::Array(Vec::new()))
}

/// A key's short form people compare between screens: its first six bytes.
fn fingerprint(key: &[u8; 32]) -> String {
    key.iter()
        .take(6)
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

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
                    argument: InvokeArgument::None,
                    ..request
                };
                let own = self.own;
                self.host.invoke(caller, record).map(move |receipt| {
                    receipt.map(|mut receipt| {
                        if let ReceiptStatus::Completed { output } = &mut receipt.status {
                            output.value = Some(fleet(output.value.take(), &own));
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
