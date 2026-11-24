//! Machines a person prepared to install, planned in their Servers workspace
//! rather than in a file on the device that planned them.
//!
//! The plan is the machine's entry in the workspace's document
//! ([`crate::workspaces::servers`]), so every one of the person's devices reads
//! what it waits for and the installer's window shows the same list the
//! command prints. A machine is planned only on a device that holds the
//! workspace: the device that made the person's root founded it, and every
//! later device was taken in.
//!
//! The machine's token is the one thing that stays on the device that made it,
//! in a file only its account reads: the plan names it by its nonce alone.
//!
//! A machine that installs with nobody at its screen keeps no copy of its disk's recovery key:
//! it hands the key to its host as soon as it is one of the person's devices, and the key is an
//! entry of the vault of their Servers workspace (`crate::workspaces::vault`), under the
//! machine's own device key. Every device of the person reads it there, as Passwords does, and
//! nothing else does.

use super::{Args, answer};
use crate::service_authority::ServiceOperation;
use crate::workspaces::Workspaces;
use crate::workspaces::servers::{self, Servers, WaitingPlan};
use accounts::vault_items::VaultChange;
use accounts::{SecretId, SecretItem, SecretItemKind};
use serde_json::Value;
use tilo_actions::remote::ActionCallError;
use tilo_server_protocol::servers::RecoveryKey;
use tilo_user_identity::DeviceCert;

/// Where the person's devices serve the installer, unless something names
/// another site.
const INSTALLER_URL: &str = "https://tilo.sh";

/// The field of a vault entry that names the machine whose disk recovery key the entry is.
const RECOVERY_MACHINE_FIELD: &str = "tilo.disk-recovery.machine";
/// The tag a disk recovery key's entry carries, which Passwords lists it by.
const RECOVERY_TAG: &str = "disk-recovery-key";
/// The longest recovery key an entry keeps: systemd's is far shorter.
const MAX_RECOVERY_KEY_BYTES: usize = 256;

/// Do the machine operation `operation` with `args`, as the person's own
/// device `device`.
pub(super) async fn invoke(
    workspaces: &Workspaces,
    device: &DeviceCert,
    operation: ServiceOperation,
    args: &Args,
) -> Result<Value, ActionCallError> {
    match operation {
        ServiceOperation::PlanMachine => Box::pin(plan_machine(workspaces, args)).await,
        ServiceOperation::ListMachines => answer(&waiting(workspaces).await?),
        ServiceOperation::ShowMachine => {
            answer(&shown(workspaces, args.required_text("plan")?).await?)
        }
        ServiceOperation::ListServers => answer(&person_servers(workspaces).await?),
        ServiceOperation::AdmitMachine => Box::pin(admit(workspaces, args)).await,
        ServiceOperation::KeepRecoveryKey => {
            Box::pin(keep_recovery_key(
                workspaces,
                device,
                args.required_text("key")?,
            ))
            .await
        }
        ServiceOperation::ListRecoveryKeys => answer(&Box::pin(recovery_keys(workspaces)).await?),
        _ => Err(ActionCallError::UnknownAction),
    }
}

/// Keep `key`, the disk recovery key of the machine `device` is, in the vault of the person's
/// Servers workspace: one entry per machine, which a machine that installs again replaces. What
/// was handed is never echoed in a refusal.
async fn keep_recovery_key(
    workspaces: &Workspaces,
    device: &DeviceCert,
    key: &str,
) -> Result<Value, ActionCallError> {
    let key = key.trim();
    if key.is_empty() || key.len() > MAX_RECOVERY_KEY_BYTES || key.chars().any(char::is_control) {
        return Err(ActionCallError::refused(
            "a disk recovery key is 1 to 256 bytes of text",
        ));
    }
    if !crate::workspaces::person_workspace::holds_person_workspace(workspaces).await {
        return Err(ActionCallError::refused(
            "this device does not hold your Servers workspace, so it keeps no recovery key",
        ));
    }
    let machine = hex::encode(device.device_public_key);
    let now = tilo_clock::unix_now();
    let entry = SecretItem {
        // The machine's key names its one entry, so a key handed again replaces it.
        id: SecretId(hex::encode(&device.device_public_key[..16])),
        kind: SecretItemKind::Other,
        title: device.device_label.clone(),
        username: String::new(),
        password: key.to_owned(),
        origins: std::collections::BTreeSet::new(),
        notes: String::new(),
        fields: std::collections::BTreeMap::from([(
            RECOVERY_MACHINE_FIELD.to_owned(),
            machine.clone(),
        )]),
        tags: std::collections::BTreeSet::from([RECOVERY_TAG.to_owned()]),
        created_unix: now,
        updated_unix: now,
    };
    let workspace = crate::workspaces::person_workspace::workspace_id(workspaces);
    let opened = workspaces
        .open_vault(&workspace)
        .await
        .map_err(ActionCallError::refused)?;
    // The person's own device keeps its own disk's key, in their own Servers workspace.
    workspaces
        .write_vault(&opened, vec![VaultChange::Keep(entry)], None, None)
        .await
        .map_err(|error| {
            ActionCallError::refused(format!("the recovery key was not kept: {error}"))
        })?;
    answer(&machine)
}

/// The disk recovery keys the vault of the person's Servers workspace keeps, one per machine.
async fn recovery_keys(workspaces: &Workspaces) -> Result<Vec<RecoveryKey>, ActionCallError> {
    let workspace = crate::workspaces::person_workspace::workspace_id(workspaces);
    let opened = workspaces
        .open_vault(&workspace)
        .await
        .map_err(ActionCallError::refused)?;
    Ok(opened
        .entries()
        .iter()
        .filter_map(|(entry, _)| {
            Some(RecoveryKey {
                machine: entry.fields.get(RECOVERY_MACHINE_FIELD)?.clone(),
                label: entry.title.clone(),
                key: entry.password.clone(),
            })
        })
        .collect())
}

/// Plan the machine `args` describes in the person's Servers workspace.
/// Answers how the plan
/// reaches its machine: what the command prints.
async fn plan_machine(workspaces: &Workspaces, args: &Args) -> Result<Value, ActionCallError> {
    if !may_admit_devices(workspaces.device_cert()) {
        return Err(ActionCallError::refused(
            "this device may not admit another device; plan it from one of your own devices",
        ));
    }
    let plan = args.required_text("plan")?.to_owned();
    let label = args.required_text("label")?.to_owned();
    let token_file = args.required_text("token_file")?.to_owned();
    let installer_url = args
        .text("installer_url")?
        .unwrap_or(INSTALLER_URL)
        .to_owned();
    if !crate::workspaces::person_workspace::holds_person_workspace(workspaces).await {
        return Err(ActionCallError::refused(
            "your own workspace has not reached this device yet; plan the machine once it has",
        ));
    }
    let document: install_profile_model::plan::Plan = serde_json::from_str(&plan)
        .map_err(|_| ActionCallError::refused("a machine's plan is its own document"))?;
    let id = document.plan.clone();
    servers::apply(workspaces, Servers::PLAN_MACHINE.with((plan, label)))
        .await
        .map_err(ActionCallError::refused)?;
    let waiting = Box::pin(shown(workspaces, &id)).await?;
    Ok(Value::String(described(
        &waiting,
        &token_file,
        &installer_url,
    )))
}

/// Whether the certificate this host serves under is the person's device that
/// may admit another: its own certificate when the host is that device, or the
/// workspace service's, whose issuer is the device that certified it.
fn may_admit_devices(certificate: &DeviceCert) -> bool {
    certificate.capabilities.can_admit_devices
        || certificate
            .issuer
            .as_deref()
            .is_some_and(|device| device.capabilities.can_admit_devices)
}

/// What the person sees of the machine a plan describes: its name and id, the
/// token's path (never its bytes), how the plan reaches the machine, and what
/// the machine still asks.
fn described(waiting: &WaitingPlan, token_file: &str, installer_url: &str) -> String {
    use std::fmt::Write as _;
    let mut said = String::new();
    let plan = &waiting.plan;
    let name = if plan.name.is_empty() {
        plan.destination.as_str()
    } else {
        plan.name.as_str()
    };
    let _ = writeln!(said, "plan {} ({name})", plan.plan);
    let _ = writeln!(said, "token {token_file} (single use, expires in 24 hours)");
    let commands = install_profile_model::plan::commands(
        plan,
        installer_url.trim_end_matches('/'),
        token_file,
        std::env::consts::ARCH,
    );
    let _ = writeln!(said, "medium    {}", commands.medium);
    let _ = writeln!(said, "netboot   {}", commands.netboot);
    let _ = writeln!(said, "alongside {}", commands.alongside);
    if !plan.complete() {
        let _ = writeln!(
            said,
            "waiting   {} still open; your devices answer them against the machine's hardware",
            plan.open_choices().join(", ")
        );
    }
    said
}

/// The machines the person planned and has not installed, each as the
/// installer's window and the machine read it.
async fn waiting(workspaces: &Workspaces) -> Result<Vec<WaitingPlan>, ActionCallError> {
    servers::read(workspaces)
        .await
        .map_err(ActionCallError::refused)?
        .waiting_plans()
        .map_err(ActionCallError::refused)
}

/// The person's servers in service: each one the document names that is
/// installed and not retired, with where it is dialled.
async fn person_servers(
    workspaces: &Workspaces,
) -> Result<Vec<tilo_server_protocol::servers::PersonServer>, ActionCallError> {
    let servers = servers::read(workspaces)
        .await
        .map_err(ActionCallError::refused)?;
    Ok(servers
        .servers
        .iter()
        .filter(|(_, server)| !server.retired && !server.plan.planned())
        .map(
            |(device, server)| tilo_server_protocol::servers::PersonServer {
                device: device.as_str().to_owned(),
                label: server.label.clone(),
                endpoint: tilo_failure::ignored!(
                    "an address this build cannot read is no address",
                    serde_json::from_str(&server.endpoint).ok()
                ),
            },
        )
        .collect())
}

/// The planned machine whose plan is `plan`.
async fn shown(workspaces: &Workspaces, plan: &str) -> Result<WaitingPlan, ActionCallError> {
    servers::read(workspaces)
        .await
        .map_err(ActionCallError::refused)?
        .waiting_plan(plan.trim())
        .map_err(ActionCallError::refused)
}

/// Take the server `args` names in among the person's servers: its entry in their Servers
/// document, under its device key, as the server's own start would have made it
/// ([`servers::admit`]).
pub(super) async fn admit_server(
    workspaces: &Workspaces,
    args: &Args,
) -> Result<Value, ActionCallError> {
    let encoded = args.required_text("cert")?;
    let bytes = hex::decode(encoded)
        .map_err(|_| ActionCallError::refused("a server's certificate is hex"))?;
    let cert: DeviceCert = tilo_user_identity::canonical::from_canonical_bytes(&bytes)
        .map_err(|_| ActionCallError::refused("a server's certificate is its own bytes"))?;
    let endpoint: iroh_base::EndpointAddr =
        serde_json::from_str(args.required_text("endpoint")?)
            .map_err(|_| ActionCallError::refused("a server's endpoint is its own JSON"))?;
    servers::admit(workspaces, &cert, &endpoint)
        .await
        .map_err(ActionCallError::refused)?;
    answer(&"admitted")
}

/// Take the machine `args` names into its entry, under its device key, as the
/// device that approved it does: the drives and questions it reported ride
/// along, and a later report of them from the machine itself updates the same
/// entry ([`Servers::begin_install`]).
async fn admit(workspaces: &Workspaces, args: &Args) -> Result<Value, ActionCallError> {
    let device = args.required_text("device")?.to_owned();
    let nonce = args.required_text("nonce")?.to_owned();
    let drives = args.text("drives")?.unwrap_or("[]").to_owned();
    let ask = args.text("ask")?.map_or_else(String::new, str::to_owned);
    Box::pin(servers::apply(
        workspaces,
        Servers::BEGIN_INSTALL.with((device, nonce, drives, ask)),
    ))
    .await
    .map_err(ActionCallError::refused)?;
    answer(&"admitted")
}
