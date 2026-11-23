//! `tilo device add`, `tilo device ls` and `tilo device revoke`: the person's devices, from a
//! computer that is logged in and holds their root.
//!
//! `ls` and `revoke` ask this computer's daemon, which reads the person's devices from the record
//! of their own workspace and whether each is online from presence, and whose root marks a
//! revoked device there.
//!
//! `add` has this computer's daemon serve a ticket ([`tilo_device_identity::login`]) on the
//! endpoint it already runs, while the ticket lasts. A computer that runs `tilo login` with it
//! reaches this one directly and calls the certify action there; the person's root here certifies
//! its key. The person ran `add` themselves, so the daemon approves the computer that comes with
//! the ticket automatically. A device that does not hold the root forwards the same question to
//! one that does through another approver.

use std::process::ExitCode;

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};
use tilo_actions::remote::device_tickets::FleetDevice;
use tilo_device_identity::login::Ticket;
use tilo_user_identity::DeviceCert;

use crate::api::{Calls, Reach};
use crate::login::{self, Device};

/// `tilo device add`: print a ticket, wait for a computer to log in with it, and say which one
/// it was. With `no_wait`, print only the ticket: this computer's daemon keeps serving it while
/// it lasts. With `server`, the computer is a server, which keeps the person's workspaces; with
/// `plan`, it is the machine that plan describes, and its certification takes the plan's entry
/// into its own.
///
/// A computer with the person's root and no login adds devices too: the workspace its daemon
/// serves is where the action is asked.
///
/// # Errors
/// This computer cannot add devices, or no computer came before the ticket ran out.
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
    login::trust_test_certificates()?;
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
        let issued = client
            .act(
                &workspace,
                "devices",
                tilo_device_identity::login::ISSUE,
                &args,
                "Add a device",
            )
            .await?;
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
                &workspace,
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
/// this computer is a device of, once the person approves it here.
///
/// # Errors
/// The text is no sign-in ticket, this computer is not logged in, the browser is not waiting, or
/// the host refused the approval.
#[allow(
    clippy::too_many_lines,
    reason = "one command: the browser's request, the root's signature, and what the person reads"
)]
fn sign_in(ticket: &str, json: bool) -> Result<ExitCode> {
    use tilo_device_identity::login::{APPROVE, Direction, PENDING};

    login::trust_test_certificates()?;
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
    let person = crate::identity::held()?;
    let (_, here) = login::here()?.ok_or_else(|| anyhow!(login::NOT_LOGGED_IN))?;
    login::ensure_daemon()?;
    let runtime = login::runtime()?;
    let text = parsed.encode();
    let (device, label, capability) = runtime.block_on(async {
        let client = Calls::open_with(Reach::default(), crate::mcp::reach_daemon).await?;
        let workspace = here.workspace.clone();
        let answer = client
            .act(
                &workspace,
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
        Ok::<(String, String, String), anyhow::Error>((device, label, capability))
    })?;
    if !json {
        println!("This browser asks to be one of your devices: \"{label}\" ({capability}).");
    }
    let tab = tilo_device_identity::login::Tab {
        device_public_key: device.clone(),
        label: label.clone(),
    };
    // The person's root signs the tab's certificate, as it signs one for any device of theirs.
    // A computer without the root hands the signing to one that holds it: the tab's request is
    // already queued for the person's approving devices.
    let cert = match &person {
        Some(person) => {
            let mut cert = tab
                .certificate(person.public_key(), tilo_clock::unix_now_ns())
                .map_err(|error| anyhow!("the browser's certificate: {error}"))?;
            tilo_user_identity::sign_device_cert(&mut cert, &person.root)
                .map_err(|error| anyhow!("signing the browser's certificate: {error}"))?;
            Some(cert)
        }
        None => None,
    };
    let answer = runtime.block_on(async {
        let client = Calls::open_with(Reach::default(), crate::mcp::reach_daemon).await?;
        let argument = match &cert {
            Some(cert) => json!({"ticket": text, "cert": cert, "label": here.cert.device_label}),
            None => json!({"ticket": text, "label": here.cert.device_label}),
        };
        client
            .act(
                &here.workspace,
                "devices",
                APPROVE,
                &argument,
                "Approve a browser's sign-in",
            )
            .await
    })?;
    if json {
        println!("{}", json!({"label": label, "device": device}));
    } else if cert.is_some() {
        println!("Signed the browser in as one of your devices.");
    } else {
        let _ = answer;
        println!(
            "This computer does not hold your identity, so it handed the signing on: approve \
             \"{label}\" on a device that does, in your Tilo or in a tab of yours, and the browser \
             is signed in."
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// The workspace this computer's daemon serves, where its `devices` actions are asked: the one
/// its login names, else the one it founded.
async fn served(client: &Client) -> Result<String> {
    match login::home().filter(Device::reaches_a_server) {
        Some(device) => Ok(device.workspace),
        None => client.founded_workspace().await,
    }
}

/// The person's devices as this computer's daemon lists them.
async fn devices(client: &Client) -> Result<Vec<FleetDevice>> {
    let workspace = served(client).await?;
    let answer = client
        .act(
            &workspace,
            "devices",
            tilo_device_identity::login::LIST,
            &json!({}),
            "Your devices",
        )
        .await?;
    let listed = answer
        .get("result")
        .and_then(|result| result.get("devices"))
        .cloned()
        .ok_or_else(|| anyhow!("this computer's daemon answered no list of your devices"))?;
    serde_json::from_value(listed).map_err(|error| {
        anyhow!("this computer's daemon answered no list of your devices: {error}")
    })
}

/// This computer's daemon, as `ls` and `revoke` ask it.
fn daemon() -> Result<Client> {
    Client::open(Reach {
        api: None,
        key_file: None,
        json: false,
    })
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

/// `tilo device ls`: the person's devices.
///
/// # Errors
/// This computer's daemon does not run, or answered no list.
pub fn list(json: bool) -> Result<ExitCode> {
    login::ensure_daemon()?;
    let devices = login::runtime()?.block_on(async { devices(&daemon()?).await })?;
    if json {
        let listed: Vec<_> = devices
            .iter()
            .map(|device| {
                json!({
                    "device": device.key_hex,
                    "label": device.label,
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
        println!("{short}  {}{notes}", device.label);
    }
    Ok(ExitCode::SUCCESS)
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
        let client = daemon()?;
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
        let workspace = served(&client).await?;
        client
            .act(
                &workspace,
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

/// `tilo device local-person`: the account on this device that proves it holds the identity
/// the device is bound to, as `NAME UID:GID`. Exits 2 when several do, for a person to say
/// which, and 1 when none does yet or the accounts cannot be read.
pub fn local_person(identity_root: &std::path::Path, passwd: Option<&std::path::Path>) -> ExitCode {
    use tilo_identity::local_person::{self, Person};
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
    let found = text.and_then(|text| {
        let accounts = local_person::regular_accounts(&text);
        let paths = tilo_device_identity::IdentityPaths::under(identity_root);
        Ok(local_person::find(&paths, &accounts)?)
    });
    match found {
        Ok(Person::One(account)) => {
            println!("{} {}:{}", account.name, account.uid, account.gid);
            ExitCode::SUCCESS
        }
        Ok(Person::Nobody) => {
            eprintln!(
                "no account here proves it holds this device's identity; the person's next \
                 sign-in writes the proof"
            );
            ExitCode::FAILURE
        }
        Ok(Person::Several(accounts)) => {
            let names: Vec<_> = accounts.into_iter().map(|account| account.name).collect();
            eprintln!(
                "several accounts hold this device's identity: {}",
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
