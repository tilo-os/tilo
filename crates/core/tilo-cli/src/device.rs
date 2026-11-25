//! `tilo device add`, `tilo device ls` and `tilo device revoke`: the person's devices, from a
//! computer that is logged in and holds their root.
//!
//! `ls` reads the person's devices from the record of their own workspace through this computer's
//! daemon, and whether each is online from who is here; `rename` and `revoke` ask the daemon,
//! whose record takes the name and whose root marks a revoked device there.
//!
//! `add` has this computer's daemon serve a ticket ([`tilo_device_identity::login`]) on the
//! endpoint it already runs, while the ticket lasts. A computer that runs `tilo login` with it
//! reaches this one directly and calls the certify action there; the person's root here certifies
//! its key. A ticket is the person's own to issue, and a program of theirs is an agent of theirs:
//! `add` asks them, and serves the ticket once they say yes on the asking card of whichever of
//! their devices is at hand. A device that does not hold the root hands the same question on to
//! one that does.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};
use tilo_actions::remote::device_tickets::FleetDevice;
use tilo_device_identity::login::Ticket;
use tilo_user_identity::DeviceCert;

use crate::api::{Calls, Reach};
use crate::login::{self, Device};

/// How long `tilo device add` waits for the person's yes on the asking card: a ticket is asked
/// of them, and they may answer later than one wait of the call lasts.
const ASKING_DEADLINE: Duration = Duration::from_secs(15 * 60);

/// `tilo device add`: print a ticket, wait for a computer to log in with it, and say which one
/// it was. With `no_wait`, print only the ticket: this computer's daemon keeps serving it while
/// it lasts. With `server`, the computer is a server, which keeps the person's workspaces; with
/// `plan`, it is the machine that plan describes, and its certification takes the plan's entry
/// into its own.
///
/// A computer with the person's root and no login adds devices too: the workspace its daemon
/// serves is where the action is asked. Either way the person confirms it on their asking card:
/// no program of theirs issues a ticket as them.
///
/// # Errors
/// This computer cannot add devices, the person did not confirm it, or no computer came before
/// the ticket ran out.
#[allow(
    clippy::too_many_lines,
    reason = "one command: the ticket a browser showed, or printing one and waiting for it"
)]
pub fn add(
    ticket: Option<&str>,
    json: bool,
    no_wait: bool,
    plan: Option<&str>,
    server: bool,
) -> Result<ExitCode> {
    if let Some(ticket) = ticket {
        return sign_in(ticket, json);
    }
    // A login that ended names a workspace no daemon here holds: the daemon serves this
    // computer as the person's own device instead, in the workspace it founded.
    let here = login::home().filter(Device::reaches_a_server);
    let daemon = login::ensure_daemon()?;
    let runtime = login::runtime()?;
    let cert = runtime.block_on(async {
        let client = Calls::open_with(Reach::default(), || Ok(daemon)).await?;
        let workspace = match &here {
            Some(device) => device.workspace.clone(),
            None => client.founded_workspace().await?,
        };
        // A role the person did not ask for goes as null, which the daemon reads as none.
        let args = json!({ "plan": plan, "server": server.then_some(true) });
        let what = match (server, plan) {
            (_, Some(plan)) => format!("the machine {plan} describes"),
            (true, _) => "a server".to_owned(),
            _ => "a computer".to_owned(),
        };
        // A ticket is the person's own to issue: a program of theirs asks, and they say yes on
        // the asking card, on whichever of their devices is at hand.
        if !json {
            eprintln!(
                "tilo: adding {what}: confirm on the asking card on any of your devices. \
                 Nothing is added until you do."
            );
        }
        let mut issued = client
            .act(
                Some(&workspace),
                "devices",
                tilo_device_identity::login::ISSUE,
                &args,
                "Add a device",
            )
            .await?;
        // Their yes may come after one wait of the call lasts: follow the ask until it ends, or
        // until asking takes longer than a ticket would last.
        let deadline = Instant::now().checked_add(ASKING_DEADLINE);
        while issued.get("status").and_then(Value::as_str) == Some("pending")
            && deadline.is_none_or(|deadline| Instant::now() < deadline)
        {
            issued = client.follow(Some(&workspace), issued).await?;
        }
        match issued.get("status").and_then(Value::as_str) {
            // A no, or a withdrawal, ends the call with no ticket.
            Some("failed" | "cancelled") => {
                let why = issued
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let why = if why.is_empty() {
                    String::new()
                } else {
                    format!(": {why}")
                };
                bail!("Adding {what} was not confirmed, so nothing was added{why}");
            }
            Some("pending") => bail!(
                "Adding {what} still waits for your yes on the asking card on any of your \
                 devices: approve it there, then run `tilo device add` again."
            ),
            _ => {}
        }
        let ticket = issued
            .get("result")
            .and_then(|answer| answer.get("ticket"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("this computer's daemon answered no ticket"))?;
        if json {
            println!("{}", json!({ "ticket": ticket }));
        } else {
            eprintln!("On the computer to add, run:");
            println!("tilo login {ticket}");
            eprintln!(
                "The ticket works once, for 10 minutes. It adds the computer as one of your devices."
            );
        }
        if no_wait {
            return Ok::<Option<DeviceCert>, anyhow::Error>(None);
        }
        let done = client
            .act(
                Some(&workspace),
                "devices",
                tilo_device_identity::login::ISSUE,
                &json!({ "wait": true }),
                "Add a device",
            )
            .await?;
        let encoded = done
            .get("result")
            .and_then(|answer| answer.get("cert"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("this computer's daemon answered no certificate"))?;
        let bytes = hex::decode(encoded).map_err(|error| anyhow!("the certificate: {error}"))?;
        // The daemon that certified it kept it in the record of the person's devices.
        let cert: DeviceCert = tilo_user_identity::canonical::from_canonical_bytes(&bytes)
            .map_err(|error| anyhow!("the certificate: {error}"))?;
        Ok::<Option<DeviceCert>, anyhow::Error>(Some(cert))
    })?;
    let Some(cert) = cert else {
        return Ok(ExitCode::SUCCESS);
    };
    let key_hex = hex::encode(cert.device_public_key);
    if json {
        println!(
            "{}",
            json!({ "device": key_hex, "label": cert.device_label })
        );
    } else {
        println!("Added {} ({}).", cert.device_label, short(&cert));
    }
    Ok(ExitCode::SUCCESS)
}

/// `tilo device add <ticket>`: the browser that showed the ticket is signed in as the person
/// this computer is a device of, once the person approves it here. A ticket an agent shows while
/// it waits for approval is redeemed the same way: the device that asked runs the agent under
/// the certificate the person's root signs here, for that agent in that workspace alone.
///
/// # Errors
/// The text is no sign-in ticket, this computer is not logged in, the browser is not waiting, or
/// the host refused the approval.
#[allow(
    clippy::too_many_lines,
    reason = "one command: the browser's request, the approval, and what the person reads"
)]
fn sign_in(ticket: &str, json: bool) -> Result<ExitCode> {
    use tilo_device_identity::login::{APPROVE, Direction, PENDING};

    let parsed = Ticket::parse(ticket).map_err(|_| {
        anyhow!(
            "{} is not a Tilo ticket; a browser's sign-in prompt copies one that starts with \
             tilo1.",
            ticket.trim().chars().take(24).collect::<String>()
        )
    })?;
    if parsed.direction == Direction::Login {
        bail!(
            "That ticket logs a computer in: run `tilo login {}` on the computer it was made \
             for.",
            ticket.trim()
        );
    }
    // Read without the device key: a process that reads it makes itself undumpable, and the
    // daemon then cannot tell it from a sandboxed program and refuses its socket.
    let here = login::home().ok_or_else(|| anyhow!(login::NOT_LOGGED_IN))?;
    login::ensure_daemon()?;
    let runtime = login::runtime()?;
    let text = parsed.encode();
    let (device, label, capability, key_for) = runtime.block_on(async {
        let client = Calls::open_with(Reach::default(), crate::mcp::reach_daemon).await?;
        let workspace = here.workspace.clone();
        let answer = client
            .act(
                Some(&workspace),
                "devices",
                PENDING,
                &json!({"ticket": text}),
                "Sign a browser in",
            )
            .await?;
        let result = answer.get("result").cloned().unwrap_or(Value::Null);
        let device = result
            .get("device")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("this computer's daemon answered no browser"))?
            .to_owned();
        let label = result
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or("a browser")
            .to_owned();
        let capability = result
            .get("capability")
            .and_then(Value::as_str)
            .unwrap_or("a browser of yours")
            .to_owned();
        let key_for = result
            .get("for")
            .cloned()
            .map(serde_json::from_value::<tilo_device_identity::login::KeyFor>)
            .transpose()
            .map_err(|error| anyhow!("what the ticket asks the key for: {error}"))?;
        Ok::<_, anyhow::Error>((device, label, capability, key_for))
    })?;
    if !json {
        match &key_for {
            Some(tilo_device_identity::login::KeyFor::Agent(_)) => {
                println!("A device asks to run your agent: \"{label}\" ({capability}).");
            }
            Some(tilo_device_identity::login::KeyFor::Workspace(_)) => {
                bail!(tilo_device_identity::login::TabCertError::PlacedByItsHost)
            }
            None => {
                println!(
                    "This browser asks to be one of your devices: \"{label}\" ({capability})."
                );
            }
        }
    }
    let tab = tilo_device_identity::login::Tab {
        device_public_key: device.clone(),
        label: label.clone(),
        key_for: key_for.clone(),
    };
    let answer = runtime.block_on(async {
        let client = Calls::open_with(Reach::default(), crate::mcp::reach_daemon).await?;
        // The daemon signs the tab with the root it keeps, or hands the signing on.
        let argument = json!({"ticket": text, "tab": tab, "label": here.cert.device_label});
        client
            .act(
                Some(&here.workspace),
                "devices",
                APPROVE,
                &argument,
                "Approve a browser's sign-in",
            )
            .await
    })?;
    if json {
        println!("{}", json!({"label": label, "device": device}));
    } else if let Some(tilo_device_identity::login::KeyFor::Agent(_)) = &key_for {
        println!("Approved: \"{label}\" runs your agent from now on.");
    } else if answer
        .get("signed")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        println!("Signed the browser in as one of your devices.");
    } else {
        println!(
            "This computer does not hold your identity, so it handed the signing on: approve \
             \"{label}\" on a device that does, in your Tilo or in a tab of yours, and the browser \
             is signed in."
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// The workspace this computer's daemon serves, where its `devices` actions are asked: the one
/// its login names, when that login reaches a server.
fn named_workspace() -> Option<String> {
    login::home()
        .filter(Device::reaches_a_server)
        .map(|device| device.workspace)
}

/// This computer's certificate: its login's, else the one its person's root here signed for it.
fn this_computer() -> Result<DeviceCert> {
    if let Some(device) = login::home() {
        return Ok(device.cert);
    }
    let person = crate::identity::held()?.ok_or_else(|| {
        anyhow!("This computer is no device of yours yet: log it in with `tilo login`.")
    })?;
    tilo_device_identity::persist::read_device_cert(
        &crate::identity::paths().device_cert_for_user(&person.public_key()),
    )
    .map_err(|error| anyhow!("this computer's certificate does not read: {error}"))
}

/// The person's devices: the record of their own workspace, read through the record API of this
/// computer's daemon, joined with who is here now for which are online.
async fn devices(client: &Calls) -> Result<Vec<FleetDevice>> {
    use tilo_server_protocol::devices::{DEVICES, FleetRecords, TAB_GROUPS, fleet, record_value};
    let cert = this_computer()?;
    let person = cert.user_public_key;
    let own = tilo_server_protocol::servers::person_workspace_id(&person);
    let read = |(app, record): (&'static str, &'static str)| {
        let own = own.clone();
        async move {
            client
                .typed(
                    Some(&own),
                    "tilo_read",
                    json!({ "path": format!("record:{app}:{record}") }),
                    None,
                )
                .await
                .map(|read| record_value(&read))
        }
    };
    let records = FleetRecords {
        devices: read(DEVICES)
            .await
            .context("this computer's daemon reads no record of your devices")?,
        // A person with no browser tab, or no list of workspaces yet, has no such record.
        tab_groups: read(TAB_GROUPS).await.unwrap_or_default(),
        workspaces: read(tilo_server_protocol::workspaces::IDENTITY_LIST)
            .await
            .unwrap_or_default(),
    };
    // One look at who is here: a watch with no cursor answers who is here at once.
    let watched = client
        .typed(Some(&own), "tilo_watch", json!({ "wait_ms": 0 }), None)
        .await
        .context("this computer's daemon says nothing of who is here")?;
    let here: Vec<tilo_actions::remote::Here> = watched
        .get("events")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|event| event.get("kind").and_then(Value::as_str) == Some("presence"))
        .filter_map(|event| event.get("here").cloned())
        .filter_map(|here| serde_json::from_value::<Vec<_>>(here).ok())
        .flatten()
        .collect();
    Ok(fleet(&person, &cert.device_public_key, &records, &here))
}

/// This computer's daemon, as `ls`, `rename` and `revoke` ask it.
async fn daemon() -> Result<Calls> {
    Calls::open_with(Reach::default(), crate::mcp::reach_daemon).await
}

/// The start of a device's key, as lists show it.
fn short(cert: &DeviceCert) -> String {
    hex::encode(cert.device_public_key.get(..6).unwrap_or_default())
}

/// `tilo device key`: this computer's device key, its person and its label, from its login; with
/// no login, the workspace its daemon founded for its person, as the daemon answers it.
///
/// # Errors
/// This computer's login is damaged, or with none its daemon serves no workspace.
pub fn key() -> Result<ExitCode> {
    let Some(here) = login::home() else {
        login::ensure_daemon()?;
        let workspace = login::runtime()?.block_on(async {
            Calls::open_with(Reach::default(), crate::mcp::reach_daemon)
                .await?
                .founded_workspace()
                .await
        })?;
        println!("{}", json!({ "workspace": workspace }));
        return Ok(ExitCode::SUCCESS);
    };
    println!(
        "{}",
        json!({
            "device": hex::encode(here.cert.device_public_key),
            "identity": hex::encode(here.cert.user_public_key),
            "label": here.cert.device_label,
        })
    );
    Ok(ExitCode::SUCCESS)
}

/// `tilo device ls`: every key the person's root certified, each with what it is (`device`,
/// `agent` or `script`); `tilo device revoke` takes any of them back.
///
/// # Errors
/// This computer's daemon does not run, or answered no list.
pub fn list(json: bool) -> Result<ExitCode> {
    login::ensure_daemon()?;
    let devices = login::runtime()?.block_on(async { devices(&daemon().await?).await })?;
    if json {
        let listed: Vec<_> = devices
            .iter()
            .map(|device| {
                json!({
                    "device": device.key_hex,
                    "label": device.label,
                    "kind": device.kind.as_str(),
                    "agent": device.agent,
                    "online": device.online,
                    "revoked": device.revoked,
                    "this": device.current,
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(listed));
        return Ok(ExitCode::SUCCESS);
    }
    for device in &devices {
        let mut notes = Vec::new();
        if device.current {
            notes.push("this computer");
        }
        if device.online {
            notes.push("online");
        }
        if device.revoked {
            notes.push("revoked");
        }
        let notes = if notes.is_empty() {
            String::new()
        } else {
            format!("  ({})", notes.join(", "))
        };
        let short = device.key_hex.get(..12).unwrap_or(&device.key_hex);
        println!(
            "{short}  {:<6}  {}{notes}",
            device.kind.as_str(),
            device.label
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// `tilo device rename DEVICE NAME`: call that device `name` in every list of the person's
/// devices: the record of their own workspace takes the name.
///
/// # Errors
/// The device is none or several of the person's, or the record refused the name.
pub fn rename(which: &str, name: &str) -> Result<ExitCode> {
    login::ensure_daemon()?;
    login::runtime()?.block_on(async {
        let client = daemon().await?;
        let devices = devices(&client).await?;
        let target = pick(&devices, which)?;
        let workspace = match named_workspace() {
            Some(workspace) => workspace,
            None => client.founded_workspace().await?,
        };
        client
            .act(
                Some(&workspace),
                "devices",
                tilo_device_identity::login::RENAME,
                &json!({ "device": target.key_hex, "name": name }),
                "Rename a device",
            )
            .await?;
        println!("{} is called {} now.", target.label, name.trim());
        Ok(ExitCode::SUCCESS)
    })
}

/// Why a device's login is ended, as `tilo device revoke --reason` names it.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Why {
    /// It was sold, given away or replaced.
    Retired,
    /// It went missing.
    Lost,
    /// Someone else has it.
    Stolen,
    /// Its key may have leaked.
    Compromised,
}

impl Why {
    /// The reason as the `devices#revoke` action takes it.
    fn as_str(self) -> &'static str {
        match self {
            Self::Retired => "retired",
            Self::Lost => "lost",
            Self::Stolen => "stolen",
            Self::Compromised => "compromised",
        }
    }
}

/// The one device of `devices` that `which` names: by a prefix of its key, or by its name.
fn pick<'a>(devices: &'a [FleetDevice], which: &str) -> Result<&'a FleetDevice> {
    let wanted = which.trim().to_ascii_lowercase();
    if wanted.is_empty() {
        bail!("Name the device by its key or its name, as `tilo device ls` lists it.");
    }
    let by_key: Vec<_> = devices
        .iter()
        .filter(|device| device.key_hex.starts_with(&wanted))
        .collect();
    let found = if by_key.is_empty() {
        devices
            .iter()
            .filter(|device| device.label.eq_ignore_ascii_case(which.trim()))
            .collect()
    } else {
        by_key
    };
    match found.as_slice() {
        [device] => Ok(device),
        [] => bail!("None of your devices is {which}: `tilo device ls` lists them."),
        _ => bail!("{which} names several of your devices: give more of its key."),
    }
}

/// `tilo device revoke DEVICE`: end that device's login, as the person's root this computer's
/// daemon holds says: the record of their devices marks it, and every host and device of theirs
/// refuses it.
///
/// # Errors
/// This computer cannot revoke, the device is none or several of the person's, it is this
/// computer, or the person did not confirm with `--yes`.
pub fn revoke(which: &str, why: Why, yes: bool) -> Result<ExitCode> {
    login::ensure_daemon()?;
    login::runtime()?.block_on(async {
        let client = daemon().await?;
        let devices = devices(&client).await?;
        let target = pick(&devices, which)?;
        if target.current {
            bail!("That is this computer: revoke it from another of your devices.");
        }
        if !yes {
            bail!(
                "Revoking {} cannot be undone: pass --yes to revoke it.",
                target.label
            );
        }
        let workspace = match named_workspace() {
            Some(workspace) => workspace,
            None => client.founded_workspace().await?,
        };
        client
            .act(
                Some(&workspace),
                "devices",
                tilo_device_identity::login::REVOKE,
                &json!({ "device": target.key_hex, "reason": why.as_str() }),
                "Revoke a device",
            )
            .await?;
        println!("Revoked {}.", target.label);
        Ok(ExitCode::SUCCESS)
    })
}

/// `tilo device local-person`: the account on this device that is its person, as the daemon
/// recorded when that account made or restored them, as `NAME UID:GID`. Exits 2 when several
/// are, for a person to say which, and 1 when none is yet or the accounts cannot be read.
pub fn local_person(identity_root: &std::path::Path, passwd: Option<&std::path::Path>) -> ExitCode {
    let text = match passwd {
        Some(path) => {
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))
        }
        None => std::process::Command::new("getent")
            .arg("passwd")
            .output()
            .context("run getent passwd")
            .and_then(|out| {
                anyhow::ensure!(out.status.success(), "getent passwd failed");
                Ok(String::from_utf8_lossy(&out.stdout).into_owned())
            }),
    };
    // The system daemon's profile is `state` in the device's identity tree.
    let profile = identity_root.join("state");
    let found = text.and_then(|text| {
        let people = tilo_api::daemon::admission::recorded_accounts(&profile)?;
        Ok(regular_accounts(&text)
            .into_iter()
            .filter(|account| people.get(&account.uid).is_some_and(|person| person.device))
            .collect::<Vec<_>>())
    });
    match found.as_deref() {
        Ok([account]) => {
            println!("{} {}:{}", account.name, account.uid, account.gid);
            ExitCode::SUCCESS
        }
        Ok([]) => {
            eprintln!(
                "no account here is this device's person yet; the account that sets the computer \
                 up becomes it"
            );
            ExitCode::FAILURE
        }
        Ok(accounts) => {
            let names: Vec<_> = accounts
                .iter()
                .map(|account| account.name.as_str())
                .collect();
            eprintln!(
                "several accounts are this device's person: {}",
                names.join(", ")
            );
            ExitCode::from(2)
        }
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

/// A login account, as the system's account database lists it.
struct Account {
    name: String,
    uid: u32,
    gid: u32,
}

/// The regular accounts (uid 1000 to 59999) in `passwd`-format `text`.
fn regular_accounts(text: &str) -> Vec<Account> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            let [name, _, uid, gid, ..] = fields.as_slice() else {
                return None;
            };
            let uid: u32 = uid.parse().ok()?;
            let gid: u32 = gid.parse().ok()?;
            ((1000..60000).contains(&uid) && !name.is_empty()).then(|| Account {
                name: (*name).to_owned(),
                uid,
                gid,
            })
        })
        .collect()
}

/// `tilo device rollout`: this computer's daemon as one of its person's servers,
/// taking its turn in an update. The automatic update path runs these before it
/// restarts the machine, so two of the person's servers never restart at once.
#[derive(Clone, Debug, clap::Subcommand)]
pub enum RolloutCmd {
    /// Take this server's turn among the person's servers: whether it may restart
    /// into the staged image now, and why it may not.
    Prepare {
        /// The staged image it would restart into.
        target: String,
    },
    /// Say which image this server booted and that it passed its health verdict,
    /// so the update lease goes to the person's next server.
    Booted {
        /// The image it booted.
        image: String,
    },
    /// Ask the person's other devices for this server's rollback floor to move
    /// past the generation that booted, and keep what they signed.
    Floor {
        /// The generation that booted.
        generation: u64,
        /// The image it booted.
        #[arg(long)]
        image: String,
        /// The updates this server follows.
        #[arg(long)]
        updates: String,
        /// The public half (PEM) of the person's floor key.
        #[arg(long, value_name = "PATH")]
        authority: PathBuf,
        /// What its TPM states, as `tilo-rollback floor-request` wrote it.
        #[arg(long, value_name = "PATH")]
        request: PathBuf,
    },
}

/// Ask this computer's daemon for what `cmd` names, and print its answer.
///
/// # Errors
/// The daemon is not running here, or refused the turn.
pub fn rollout(cmd: RolloutCmd) -> Result<ExitCode> {
    let here = login::home().filter(Device::reaches_a_server);
    let daemon = login::ensure_daemon()?;
    let (action, args) = match cmd {
        RolloutCmd::Prepare { target } => ("prepare", json!({ "target": target })),
        RolloutCmd::Booted { image } => ("booted", json!({ "image": image })),
        RolloutCmd::Floor {
            generation,
            image,
            updates,
            authority,
            request,
        } => {
            let authority = std::fs::read_to_string(&authority)
                .with_context(|| format!("read {}", authority.display()))?;
            let request = std::fs::read_to_string(&request)
                .with_context(|| format!("read {}", request.display()))?;
            (
                "floor",
                json!({
                    "generation": generation,
                    "image": image,
                    "updates": updates,
                    "authority": authority,
                    "request": request,
                }),
            )
        }
    };
    let runtime = login::runtime()?;
    let answer = runtime.block_on(async {
        let client = Calls::open_with(Reach::default(), || Ok(daemon)).await?;
        let workspace = match &here {
            Some(device) => device.workspace.clone(),
            None => client.founded_workspace().await?,
        };
        client
            .act(Some(&workspace), "updates", action, &args, "Update")
            .await
    })?;
    println!("{}", serde_json::to_string(&answer)?);
    Ok(ExitCode::SUCCESS)
}
