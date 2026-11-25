//! A device placed for records of one workspace: a workspace's VM, which runs this daemon under a
//! certificate that names the records it reaches there and nothing else
//! ([`tilo_user_identity::DeviceCapabilities::records`]).
//!
//! Such a device holds no workspace's keys, its own workspace's included. It keeps no copy of any
//! workspace, founds none and writes nothing of its person's. Every call made on it (its tenant's,
//! its sessions') is sent to the workspace's host over the link this device dialled, the host its
//! login entered through on this start, which decides each call as this placed device, runs it
//! and seals the change ([`super::hosts`], [`forwarded`]). Nothing is admitted here but its own
//! apps, and the one device whose requests it takes is that host ([`trust_own`]).

use std::sync::{Arc, Weak};

use iroh::EndpointAddr;
use serde_json::Value;
use tilo_actions::contract::ActionKind;
use tilo_actions::remote::environment_window::{self as wire, Renewal};
use tilo_actions::remote::{
    ActionProvider, Admitted, Caller, InvokeArgument, InvokeRequest, Outcome, Pending,
};
use tilo_actions::{ActionFailure, ActionOutput};
use tilo_desktop_model::WorkspaceId;
use tilo_identity::sources::{DevicesSeen, DevicesTrust};
use tilo_user_identity::DeviceCert;

use super::workspaces::ServiceWorkspaces;

/// The one workspace `cert` confines its device to, when it does: the workspace of the records
/// its certificate places it for ([`tilo_user_identity::DeviceCapabilities::records`]).
pub(crate) fn workspace_of(cert: &DeviceCert) -> Option<WorkspaceId> {
    cert.capabilities
        .records
        .as_ref()
        .map(|records| WorkspaceId::from(uuid::Uuid::from_bytes(*records.workspace())))
}

/// Said of a call made on a confined device before its login named its workspace's host.
pub(crate) const NO_HOST: &str =
    "this device reaches its workspace through that workspace's host, which no login named yet";

/// What a call made on the confined device `workspaces` holds, in `workspace` (none: its own),
/// reaches: the workspace's host, over the link this device dialled, asked as this device. The
/// host decides it as a placed caller, by the records the certificate names; nothing is admitted
/// here, and no call of it is ever the person's.
///
/// # Errors
/// `workspace` is not the one the device is placed in, or no login named its host yet.
pub(crate) fn forwarded(
    workspaces: &ServiceWorkspaces,
    workspace: Option<&str>,
) -> Result<Arc<dyn ActionProvider + Send + Sync>, String> {
    let placed = workspaces
        .confined()
        .ok_or("this device is placed in no workspace")?;
    if workspace.is_some_and(|named| WorkspaceId::parse(named) != Some(placed)) {
        return Err("this device reaches the one workspace it is placed in alone".to_owned());
    }
    let own = placed.to_string();
    workspaces
        .copies()
        .providers()
        .host_answering_every(&own)
        .ok_or_else(|| NO_HOST.to_owned())
}

/// The host of a confined device's one workspace, as the login it entered with names it.
#[derive(Clone, Debug)]
pub(crate) struct ConfinedHost {
    /// The host's device key: whose device answers the workspace's actions.
    pub(crate) device: [u8; 32],
    /// Where the host is dialled.
    pub(crate) address: EndpointAddr,
}

impl ConfinedHost {
    /// The host `login` enters through, when `cert` confines its device to the workspace
    /// `login` enters; `None` for any other device or login.
    pub(crate) fn of_login(
        cert: &DeviceCert,
        login: &tilo_device_identity::login::Device,
    ) -> Option<Self> {
        let workspace = workspace_of(cert)?;
        if WorkspaceId::parse(&login.workspace)? != workspace
            || login.cert.device_public_key != cert.device_public_key
        {
            return None;
        }
        Some(Self {
            device: login.server_cert.device_public_key,
            address: login.server_address.as_ref()?.endpoint_addr.clone(),
        })
    }
}

/// The host's renewal of the confined device's certificate ([`wire::RENEW`]): reached by the host
/// that placed the device alone, the one device its action service answers.
pub(crate) struct Renewals(pub(crate) Weak<ServiceWorkspaces>);

impl tilo_actions::remote::Admits for Renewals {
    const PROVIDER: &'static str = wire::PLACEMENT;
    const APP: &'static str = wire::PLACEMENT;

    fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
        tilo_actions::remote::admitted::top_level(vec![descriptor(
            wire::RENEW,
            "Certify the workspace's machine again",
            ActionKind::Outside,
        )])
    }

    fn start(
        &self,
        _caller: &Caller,
        request: &InvokeRequest,
        _admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        if request.target.address != tilo_actions::ActionAddress::new(wire::RESOURCE, wire::RENEW) {
            return Err("this machine takes its certificate again, and nothing else, here".into());
        }
        let Renewal { certificate } = argument(request)?;
        let renewed: DeviceCert =
            serde_json::from_str(&certificate).map_err(|error| error.to_string())?;
        let workspaces = self
            .0
            .upgrade()
            .ok_or("this machine's daemon is stopping")?;
        take_renewal(&workspaces, renewed)?;
        Ok(Pending::ready(output(&serde_json::json!({}))))
    }
}

/// Hold `renewed` as `workspaces`'s device certificate from now on, when it is the same device
/// certified again: its key, its person and its records, valid now under a chain that verifies,
/// and ending later than the one it holds.
///
/// # Errors
/// What differs, in words.
pub(crate) fn take_renewal(
    workspaces: &ServiceWorkspaces,
    renewed: DeviceCert,
) -> Result<(), String> {
    let current = workspaces.device_cert();
    if renewed.device_public_key != current.device_public_key {
        return Err("the certificate is another device's".into());
    }
    if renewed.user_public_key != current.user_public_key {
        return Err("the certificate names another person".into());
    }
    if renewed.capabilities != current.capabilities || renewed.capabilities.records.is_none() {
        return Err("the certificate places the device for other records".into());
    }
    if renewed.expires_at_ns.unwrap_or(u64::MAX) <= current.expires_at_ns.unwrap_or(u64::MAX) {
        return Err("the certificate ends no later than the one this device holds".into());
    }
    tilo_user_identity::verify_device_cert_at(&renewed, tilo_clock::unix_now_ns())
        .map_err(|error| format!("the certificate does not verify: {error}"))?;
    workspaces.renew(renewed);
    Ok(())
}

/// Answer `trust` for the confined device `own`: of its person's devices it knows itself alone,
/// confined to its one workspace. It reads none of its person's records, and needs none: what it
/// admits is its own apps, the one host whose requests it takes is the one its login named
/// ([`super::workspaces::ServiceWorkspaces::confine_to`]), and every call it makes is decided again
/// by that host, which a revocation reaches.
pub(crate) fn trust_own(trust: &DevicesTrust, own: DeviceCert) {
    trust.reading(move || Some(own_alone(&own)));
}

/// What `own` knows of its person's devices: its own key, confined to its one workspace.
fn own_alone(own: &DeviceCert) -> DevicesSeen {
    DevicesSeen::of_host(own.user_public_key, [own.device_public_key], []).confining(
        workspace_of(own).map(|workspace| (own.device_public_key, *workspace.as_uuid().as_bytes())),
    )
}

/// The certificate of `device`, placed for `records` by a host the person whose root is `root`
/// granted placing them: what a workspace's VM is certified with, in a test.
#[cfg(test)]
pub(crate) fn placed_cert(
    root: &dyn tilo_user_identity::StatementSigner,
    device: [u8; 32],
    records: tilo_user_identity::PlacedRecords,
    label: &str,
) -> DeviceCert {
    placed_cert_until(root, device, records, label, 3_600_000_000_000)
}

/// [`placed_cert`], ending `lasts` ns from now.
#[cfg(test)]
#[expect(clippy::expect_used, reason = "a test's fixture")]
pub(crate) fn placed_cert_until(
    root: &dyn tilo_user_identity::StatementSigner,
    device: [u8; 32],
    records: tilo_user_identity::PlacedRecords,
    label: &str,
    lasts: u64,
) -> DeviceCert {
    let now = tilo_clock::unix_now_ns();
    let (issued, until) = (now - 1_000_000_000, now + lasts);
    let issuer = ed25519_dalek::SigningKey::from_bytes(&[0x5a; 32]);
    let mut grant = tilo_user_identity::prepare_placing_grant(
        root.public_key(),
        issuer.verifying_key().to_bytes(),
        records.clone(),
        "The host",
        issued,
        None,
    );
    tilo_user_identity::sign_device_cert(&mut grant, root).expect("the root signs");
    let mut cert = tilo_user_identity::prepare_placed_device_cert(
        &grant, device, records, label, issued, until,
    );
    tilo_user_identity::sign_device_cert(&mut cert, &issuer).expect("its issuer signs");
    cert
}

/// `value` as an action's answer.
pub(super) fn output(value: &impl serde::Serialize) -> Outcome {
    serde_json::to_value(value)
        .map(|value| ActionOutput {
            value: Some(value),
            ..ActionOutput::default()
        })
        .map_err(|error| Box::new(refused(error.to_string())))
}

pub(super) fn refused(message: String) -> ActionFailure {
    ActionFailure {
        code: "refused".to_owned(),
        message,
        details: None,
        retryable: false,
    }
}

/// The argument `request` carries, read as `T`.
pub(super) fn argument<T: serde::de::DeserializeOwned>(
    request: &InvokeRequest,
) -> Result<T, String> {
    let InvokeArgument::Json(named) = &request.argument else {
        return Err("a workspace machine's action takes its arguments as an object".to_owned());
    };
    serde_json::from_value(Value::Object(named.clone())).map_err(|error| error.to_string())
}

/// `action` of the workspace machine, as the device's dispatch lists it: the confined device's
/// renewal and a VM session's actions alike.
pub(super) fn descriptor(
    action: &str,
    label: &str,
    kind: ActionKind,
) -> tilo_actions::api::ActionDescriptor {
    tilo_actions::api::ActionDescriptor {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: tilo_actions::api::ActionAddress::new(wire::RESOURCE, action),
        resource_kind: wire::PROVIDER.to_owned(),
        resource_label: "Workspace machine".to_owned(),
        action_label: label.to_owned(),
        keywords: Vec::new(),
        icon: None,
        description: None,
        locus: tilo_actions::ExecutionLocus::ClientLocal,
        kind: tilo_actions::OperationKind::Effect,
        contract: tilo_actions::contract::ActionContract {
            kind,
            risk: tilo_actions::contract::RiskLevel::Low,
            confirmation: tilo_actions::contract::ConfirmationPolicy::None,
            ..tilo_actions::contract::ActionContract::default()
        },
        input: tilo_actions::api::InputSpec::None,
        params: Vec::new(),
        view: None,
    }
}

#[cfg(test)]
#[path = "confined_tests.rs"]
mod tests;
