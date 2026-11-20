//! Machines a person prepared to install, planned in their Servers workspace
//! rather than in a file on the device that planned them.
//!
//! The plan is the machine's entry in the workspace's document
//! ([`crate::workspaces::servers`]), so every one of the person's devices reads
//! what it waits for and the installer's window shows the same list the
//! command prints. The host makes the workspace when the first machine is
//! planned; no server ever makes it ([`servers::founded`]).
//!
//! The machine's token is the one thing that stays on the device that made it,
//! in a file only its account reads: the plan names it by its nonce alone.

use super::{Args, answer, refused};
use crate::service_authority::ServiceOperation;
use crate::workspaces::Workspaces;
use crate::workspaces::servers::{self, Servers, WaitingPlan};
use serde_json::Value;
use tilo_actions::remote::ActionCallError;
use tilo_user_identity::DeviceCert;

/// Where the person's devices serve the installer, unless something names
/// another site.
const INSTALLER_URL: &str = "https://tilo.sh";

/// Do the machine operation `operation` with `args`, as the person's own
/// device.
pub(super) async fn invoke(
    workspaces: &Workspaces,
    operation: ServiceOperation,
    args: &Args,
) -> Result<Value, ActionCallError> {
    match operation {
        ServiceOperation::PlanMachine => Box::pin(plan_machine(workspaces, args)).await,
        ServiceOperation::ListMachines => answer(&waiting(workspaces).await?),
        ServiceOperation::ShowMachine => {
            answer(&shown(workspaces, args.required_text("plan")?).await?)
        }
        ServiceOperation::AdmitMachine => Box::pin(admit(workspaces, args)).await,
        _ => Err(ActionCallError::UnknownAction),
    }
}

/// Plan the machine `args` describes in the person's Servers workspace,
/// making that workspace when this device holds none. Answers how the plan
/// reaches its machine: what the command prints.
async fn plan_machine(workspaces: &Workspaces, args: &Args) -> Result<Value, ActionCallError> {
    if !may_admit_devices(workspaces.serving_certificate()) {
        return Err(refused(
            &"this device may not admit another device; plan it from one of your own devices",
        ));
    }
    let plan = args.required_text("plan")?.to_owned();
    let label = args.required_text("label")?.to_owned();
    let token_file = args.required_text("token_file")?.to_owned();
    let installer_url = args
        .text("installer_url")?
        .unwrap_or(INSTALLER_URL)
        .to_owned();
    servers::founded(workspaces)
        .await
        .map_err(|error| refused(&error))?;
    let document: install_profile_model::plan::Plan = serde_json::from_str(&plan)
        .map_err(|_| refused(&"a machine's plan is its own document"))?;
    let id = document.plan.clone();
    servers::apply(workspaces, Servers::PLAN_MACHINE.with((plan, label)))
        .await
        .map_err(|error| refused(&error))?;
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
        .map_err(|error| refused(&error))?
        .waiting_plans()
        .map_err(|error| refused(&error))
}

/// The planned machine whose plan is `plan`.
async fn shown(workspaces: &Workspaces, plan: &str) -> Result<WaitingPlan, ActionCallError> {
    servers::read(workspaces)
        .await
        .map_err(|error| refused(&error))?
        .waiting_plan(plan.trim())
        .map_err(|error| refused(&error))
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
    .map_err(|error| refused(&error))?;
    answer(&"admitted")
}
