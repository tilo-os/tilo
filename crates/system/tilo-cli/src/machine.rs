//! `tilo machine`: the machines this person prepared to install, from a USB
//! medium, a rescue system or alongside the system a machine already has.
//!
//! `tilo machine add` makes an install plan ([`install_profile_model::plan`])
//! through [`install_plan::Choices`], the same structure the installer's own
//! window fills in, and has the single-use, expiring ticket it goes with
//! served. The plan is the machine's entry in the person's Servers workspace,
//! written through the running session's action API (`machine.plan`), so every
//! one of their devices reads it there and the installer's window shows the
//! same list; this computer keeps only the ticket, in a file whose path, never
//! the ticket, the installer commands name. Every choice given here is made;
//! every one left out is open, and the machine asks one of the person's
//! devices to answer it against the machine's real hardware.
//!
//! The plan is secret-free: it names the ticket by its nonce and expiry, so the
//! document can travel through the person's devices, a log or an action's
//! answer without exposing the ticket.

use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Subcommand;
use install_profile_model::plan::{self, DiskRule, Holding, Network, Plan};
use tilo_actions::ActionAddress;
use tilo_actions::remote::{device_tickets, machines};
use tilo_control::front::{self, Spelling};

use crate::identity;

/// Where the person's devices serve the installer, unless `--installer-url`
/// names another site.
const INSTALLER_URL: &str = install_plan::INSTALLER_URL;

#[derive(Subcommand)]
pub enum MachineCmd {
    /// Plan a machine, have its single-use ticket served, and print how the
    /// plan reaches it.
    Add(Box<AddArgs>),
    /// The machines you planned and have not seen install yet.
    List,
    /// One planned machine: its choices, what is open, and how it reaches the
    /// machine.
    #[command(after_help = "Example:\n  tilo machine show 3f2b1c00d4e5f60718293a4b5c6d7e8f")]
    Show {
        /// The plan's id, as `tilo machine list` prints it.
        plan: String,
    },
}

/// What `tilo machine add` was told; every choice it is not told is open.
#[derive(clap::Args)]
#[command(after_help = "Examples:\n  \
        tilo machine add --name cupboard\n  \
        tilo machine add --name cupboard --disk serial:WD-ABC123 --unlock passphrase\n  \
        tilo machine add --os os --name spare-laptop --disk largest\n\n\
        Not a server: `--os os` installs Tilo OS onto the machine. You have to carry\n\
        the ticket file to the medium yourself; the plan keeps no ticket. This computer\n\
        serves the ticket for a day: it has to be on while the machine installs.")]
pub struct AddArgs {
    /// The OS it installs: `server` (Tilo Server) or `os` (Tilo OS).
    #[arg(long, default_value = "server")]
    os: String,
    /// The machine's name; left open when absent.
    #[arg(long)]
    name: Option<String>,
    /// The disk rule: `largest`, `empty`, `only`, `serial:<serial>`,
    /// `wwid:<wwid>`, or `open`.
    #[arg(long, default_value = "largest")]
    disk: String,
    /// How its network is set up: `dhcp`, `static`, or `open`.
    #[arg(long, default_value = "dhcp")]
    network: String,
    /// Its addresses as `address/prefix`, comma separated (static only).
    #[arg(long)]
    addresses: Option<String>,
    /// Its default gateways, one for each address family, comma separated
    /// (static only).
    #[arg(long)]
    gateway: Option<String>,
    /// Its DNS servers, comma separated (static only).
    #[arg(long)]
    dns: Option<String>,
    /// How it unlocks after a boot: `tpm`, `owner-device`, `passphrase`,
    /// or `open`.
    #[arg(long, default_value = "tpm")]
    unlock: String,
    /// The updates it follows: `stable`, `dev`, or `open`.
    #[arg(long, default_value = "stable")]
    updates: String,
    /// A workspace it holds, as `<workspace-id>:store` or
    /// `<workspace-id>:key-holder`; repeat for several.
    #[arg(long = "hold")]
    holds: Vec<String>,
    /// A server holds no workspace: leave hosting answered, not open.
    #[arg(long)]
    no_workspaces: bool,
    /// Where the machine's ticket is written, and what the installer commands
    /// name; defaults under this computer's identity.
    #[arg(long, value_name = "FILE")]
    token_file: Option<PathBuf>,
    /// Where the installer is served, for the rescue-system command.
    #[arg(long, default_value = INSTALLER_URL)]
    installer_url: String,
}

pub fn serve(cmd: MachineCmd) -> Result<std::process::ExitCode> {
    match cmd {
        MachineCmd::Add(add) => plan_and_ticket(&add),
        MachineCmd::List => ask(
            machines::LIST,
            "List the machines you planned",
            &serde_json::json!({}),
        ),
        MachineCmd::Show { plan } => ask(
            machines::SHOW,
            "Show a machine you planned",
            &serde_json::json!({ "plan": plan.trim() }),
        ),
    }
}

/// Ask the local session the machine step `step` with `args`, printing what
/// it answers: the same action the installer's window presses.
fn ask(step: &str, title: &str, args: &serde_json::Value) -> Result<std::process::ExitCode> {
    let sockets = crate::desktop::sockets();
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    let mut io = front::Io {
        out: &mut stdout,
        err: &mut stderr,
    };
    let code = front::run_target(
        &sockets,
        tilo_actions::remote::RemoteActionTarget {
            provider: machines::PROVIDER.to_owned(),
            address: ActionAddress::new(machines::RESOURCE, step),
        },
        title,
        args,
        &Spelling::tilo(),
        &mut io,
    );
    std::io::Write::flush(&mut stdout)?;
    Ok(std::process::ExitCode::from(code))
}

fn choices(add: &AddArgs) -> Result<install_plan::Choices> {
    let destination = match add.os.trim() {
        "server" => "tilo-server",
        "os" | "desktop" => "tilo-os",
        other => bail!("{other:?} is not an OS to install: `server` or `os`"),
    };
    let disk = open_or(&add.disk, |text| {
        DiskRule::decode(text).map_err(|why| anyhow::anyhow!("--disk {text:?}: {why}"))
    })?;
    let network = open_or(&add.network, |text| {
        let addressing = match text {
            "dhcp" | "static" => text.to_owned(),
            other => bail!("--network {other:?} is not `dhcp` or `static`"),
        };
        Ok(Network {
            addressing,
            addresses: add.addresses.clone().unwrap_or_default(),
            gateway: add.gateway.clone().unwrap_or_default(),
            dns: add.dns.clone().unwrap_or_default(),
        })
    })?;
    let unlock = open_or(&add.unlock, |text| {
        plan::UNLOCK
            .contains(&text)
            .then(|| text.to_owned())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "--unlock {text:?} is not one of {}",
                    plan::UNLOCK.join(", ")
                )
            })
    })?;
    let updates = open_or(&add.updates, |text| {
        plan::UPDATES
            .contains(&text)
            .then(|| text.to_owned())
            .ok_or_else(|| anyhow::anyhow!("--updates {text:?} is not `stable` or `dev`"))
    })?;
    let mut hosting = Vec::new();
    for hold in &add.holds {
        hosting.push(holding(hold)?);
    }
    if add.no_workspaces && !hosting.is_empty() {
        bail!("--no-workspaces and --hold answer the same choice twice");
    }
    let hosting = if destination == "tilo-server" && hosting.is_empty() && !add.no_workspaces {
        None
    } else {
        Some(hosting)
    };
    Ok(install_plan::Choices {
        destination: destination.to_owned(),
        name: add.name.clone(),
        network,
        disk,
        unlock,
        updates,
        hosting,
    })
}

fn plan_and_ticket(add: &AddArgs) -> Result<std::process::ExitCode> {
    let choices = choices(add)?;
    let dir = identity::dir()?;
    let named = install_plan::token().map_err(anyhow::Error::msg)?;
    let id = install_plan::new_id();
    let plan = choices.plan(&id, named).map_err(anyhow::Error::msg)?;
    let token_file = add
        .token_file
        .clone()
        .unwrap_or_else(|| install_plan::plans_dir(&dir).join(format!("{id}.token")));
    let args = serde_json::json!({
        "plan": serde_json::to_string(&plan).map_err(|error| anyhow::anyhow!(error.to_string()))?,
        "label": display_name(&plan),
        "token_file": token_file.display().to_string(),
        "installer_url": add.installer_url,
    });
    let code = ask(machines::PLAN, "Plan a machine", &args)?;
    if code != std::process::ExitCode::SUCCESS {
        return Ok(code);
    }
    // The machine logs in with a ticket the session that holds the plan serves for it, and its
    // certification takes the plan's entry, which exists by now, into its own.
    let ticket = planned_ticket(&id)?;
    install_plan::store_token(&dir, &id, &ticket, Some(&token_file)).map_err(anyhow::Error::msg)?;
    Ok(code)
}

/// The ticket the machine `plan` describes logs in with, served by the session that holds the
/// person's Servers workspace for as long as the plan lasts.
fn planned_ticket(plan: &str) -> Result<String> {
    let answer = front::call_target(
        &crate::desktop::sockets(),
        device_tickets::target(device_tickets::ISSUE),
        "Add a planned machine",
        &serde_json::json!({ "plan": plan }),
        &Spelling::tilo(),
    )
    .map_err(anyhow::Error::msg)?;
    answer
        .pointer("/result/ticket")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("the session answered no ticket for the plan"))
}

/// The plan as a person names it.
fn display_name(plan: &Plan) -> &str {
    if plan.name.is_empty() {
        &plan.destination
    } else {
        &plan.name
    }
}

fn holding(text: &str) -> Result<Holding> {
    let (workspace, mode) = text
        .rsplit_once(':')
        .ok_or_else(|| anyhow::anyhow!("--hold {text:?} is <workspace-id>:<mode>"))?;
    if !plan::HOLDING_MODES.contains(&mode) {
        bail!(
            "--hold {text:?}: the mode is one of {}",
            plan::HOLDING_MODES.join(", ")
        );
    }
    Ok(Holding {
        workspace: workspace.trim().to_owned(),
        mode: mode.to_owned(),
    })
}

/// `text` as a choice, or nothing when it is `open`.
fn open_or<T>(text: &str, parse: impl FnOnce(&str) -> Result<T>) -> Result<Option<T>> {
    if text.trim() == "open" {
        return Ok(None);
    }
    parse(text.trim()).map(Some)
}
