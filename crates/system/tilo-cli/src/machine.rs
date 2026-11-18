//! `tilo machine`: the machines this person prepared to install, from a USB
//! medium, a rescue system or alongside the system a machine already has.
//!
//! `tilo machine add` makes an install plan ([`install_profile_model::plan`])
//! through [`install_plan::Choices`], the same structure the installer's own
//! window fills in, and mints the single-use, expiring enrollment token it
//! goes with. The plan is kept in this computer's store of the person's; the
//! token is written to a file whose path, never the token, the installer
//! commands name. Every choice given here is made; every one left out is open,
//! and the machine asks one of the person's devices to answer it against the
//! machine's real hardware.
//!
//! The plan is secret-free: it names the token by its nonce and expiry, so the
//! document can travel through the person's devices, a log or an action's
//! answer without exposing the token.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use install_profile_model::plan::{self, DiskRule, Holding, Network, Plan};
use tilo_device_identity::IdentityPaths;

use crate::identity;

/// Where the person's devices serve the installer, unless `--installer-url`
/// names another site.
const INSTALLER_URL: &str = install_plan::INSTALLER_URL;

#[derive(Subcommand)]
pub enum MachineCmd {
    /// Plan a machine, mint its single-use token and print how the plan
    /// reaches it.
    Add(Box<AddArgs>),
    /// The machines this computer planned and has not seen install yet.
    List,
    /// One planned machine: its choices, what is open, and how it reaches the
    /// machine.
    #[command(after_help = "Example:\n  tilo machine show 3f2b1c00d4e5f60718293a4b5c6d7e8f")]
    Show {
        /// The plan's id, as `tilo machine list` prints it.
        plan: String,
        /// Where the installer is served, for the rescue-system command.
        #[arg(long, default_value = INSTALLER_URL)]
        installer_url: String,
    },
}

/// What `tilo machine add` was told; every choice it is not told is open.
#[derive(clap::Args)]
#[command(after_help = "Examples:\n  \
        tilo machine add --name cupboard\n  \
        tilo machine add --name cupboard --disk serial:WD-ABC123 --unlock passphrase\n  \
        tilo machine add --os os --name spare-laptop --disk largest\n\n\
        Not a server: `--os os` installs Tilo OS onto the machine. You have to carry\n\
        the token file to the medium yourself; the plan keeps no token.")]
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
    /// Its gateway (static only).
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
    /// Where the minted token is written, and what the installer commands
    /// name; defaults beside the plan.
    #[arg(long, value_name = "FILE")]
    token_file: Option<PathBuf>,
    /// Where the installer is served, for the rescue-system command.
    #[arg(long, default_value = INSTALLER_URL)]
    installer_url: String,
}

pub fn serve(cmd: MachineCmd) -> Result<std::process::ExitCode> {
    match cmd {
        MachineCmd::Add(add) => plan_and_mint(&add),
        MachineCmd::List => list(),
        MachineCmd::Show {
            plan,
            installer_url,
        } => show(&plan, &installer_url),
    }
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

fn plan_and_mint(add: &AddArgs) -> Result<std::process::ExitCode> {
    let choices = choices(add)?;
    let requested_token_file = add.token_file.clone();
    let dir = identity::dir()?;
    let paths = IdentityPaths::under(&dir);
    let home_peers = tilo_server_protocol::home_peer_registry_path(&dir);
    let (token, named) = install_plan::mint(&paths, &home_peers).map_err(anyhow::Error::msg)?;
    let plan = choices
        .plan(&install_plan::new_id(), named)
        .map_err(anyhow::Error::msg)?;
    let stored = install_plan::store(&dir, &plan, &token, requested_token_file.as_deref())
        .map_err(anyhow::Error::msg)?;
    println!("plan {} ({})", plan.plan, display_name(&plan));
    println!("kept {}", stored.plan_file.display());
    Ok(print_how_it_reaches(
        &plan,
        &stored.token_file,
        &add.installer_url,
    ))
}

/// Print how the plan reaches its machine: the token's path is named, never
/// its bytes.
fn print_how_it_reaches(
    plan: &Plan,
    token_file: &Path,
    installer_url: &str,
) -> std::process::ExitCode {
    println!(
        "token {} (single use, expires in 24 hours)",
        token_file.display()
    );
    let commands = plan::commands(
        plan,
        installer_url.trim_end_matches('/'),
        &token_file.display().to_string(),
        architecture(),
    );
    println!("medium    {}", commands.medium);
    println!("netboot   {}", commands.netboot);
    println!("alongside {}", commands.alongside);
    if !plan.complete() {
        println!(
            "waiting   {} still open; your devices answer them against the machine's hardware",
            plan.open_choices().join(", ")
        );
    }
    std::process::ExitCode::SUCCESS
}

fn list() -> Result<std::process::ExitCode> {
    let dir = identity::dir()?;
    let found = install_plan::plan_files(&dir).context("this computer's plans cannot be read")?;
    if found.is_empty() {
        bail!("this computer knows no planned machine; `tilo machine add --name nas` makes one");
    }
    for path in found {
        match install_plan::read(&path) {
            Ok(plan) => println!(
                "{}\t{}\t{}\t{}",
                plan.plan,
                display_name(&plan),
                plan.destination,
                if plan.complete() { "complete" } else { "open" }
            ),
            Err(error) => println!("{error}"),
        }
    }
    Ok(std::process::ExitCode::SUCCESS)
}

fn show(plan_id: &str, installer_url: &str) -> Result<std::process::ExitCode> {
    let dir = identity::dir()?;
    let plan_id = plan_id.trim();
    if plan_id.len() != 32 || !plan_id.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        bail!("a plan is named by its 32-character id; `tilo machine list` prints them");
    }
    let plan = install_plan::read(&install_plan::plans_dir(&dir).join(format!("{plan_id}.json")))
        .map_err(anyhow::Error::msg)?;
    println!("{}", serde_json::to_string_pretty(&plan)?);
    let token_file = install_plan::plans_dir(&dir).join(format!("{plan_id}.token"));
    let commands = plan::commands(
        &plan,
        installer_url.trim_end_matches('/'),
        &token_file.display().to_string(),
        architecture(),
    );
    println!("medium    {}", commands.medium);
    println!("netboot   {}", commands.netboot);
    println!("alongside {}", commands.alongside);
    Ok(std::process::ExitCode::SUCCESS)
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

fn architecture() -> &'static str {
    std::env::consts::ARCH
}
