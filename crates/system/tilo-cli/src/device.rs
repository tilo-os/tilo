//! `tilo device add`, `tilo device ls`, `tilo device rename` and `tilo device revoke`: the
//! person's devices, as this computer's daemon manages them with its `devices` actions.
//!
//! `add` has this computer's daemon serve a ticket ([`tilo_device_identity::login`]) on the
//! endpoint it already runs, while the ticket lasts. A computer that runs `tilo login` with it
//! reaches this one directly and calls the certify action there; the person's root here certifies
//! its key. The person ran `add` themselves, so the daemon approves the computer that comes with
//! the ticket automatically. A device that does not hold the root forwards the same question to
//! one that does through another approver. Listing, renaming and revoking are the daemon's actions
//! too, so every device of the person's, and an agent on it, does them the same way.

use std::process::ExitCode;

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};
use tilo_device_identity::login::{ISSUE, LIST, RENAME, RESOURCE, REVOKE, Ticket};
use tilo_user_identity::DeviceCert;

use crate::api::{Client, Reach};
use crate::login::{self, Device};

/// This computer's daemon, its service started when it does not run yet, and the login it
/// serves: a login that ended names a workspace no daemon here holds, so the daemon serves this
/// computer as the person's own device instead, in the workspace it founded.
fn daemon() -> Result<(tokio::runtime::Runtime, Option<Device>)> {
    login::trust_test_certificates()?;
    crate::identity::convert_earlier()?;
    let login_place = login::Place::here(crate::identity::paths())?;
    let here = login::load_public(&login_place)?.filter(Device::reaches_a_server);
    login::ensure_daemon()?;
    Ok((login::runtime()?, here))
}

/// The daemon's API, and the workspace its device actions are asked in.
async fn reach(here: Option<&Device>) -> Result<(Client, String)> {
    let client = Client::open(Reach {
        api: None,
        key_file: None,
        json: false,
    })?;
    let workspace = match here {
        Some(device) => device.workspace.clone(),
        None => client.founded_workspace().await?,
    };
    Ok((client, workspace))
}

/// The daemon's device action `action` with `args`, titled `title` in History: what it answered.
async fn ask(
    (client, workspace): &(Client, String),
    action: &str,
    args: &Value,
    title: &str,
) -> Result<Value> {
    let answer = client.act(workspace, RESOURCE, action, args, title).await?;
    Ok(answer.get("result").cloned().unwrap_or(Value::Null))
}

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
    let (runtime, here) = daemon()?;
    let cert = runtime.block_on(async {
        let daemon = reach(here.as_ref()).await?;
        // A role the person did not ask for goes as null, which the daemon reads as none.
        let args = json!({ "plan": plan, "server": server.then_some(true) });
        let issued = ask(&daemon, ISSUE, &args, "Add a device").await?;
        let ticket = issued
            .get("ticket")
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
        // The daemon lists the computer among the person's devices once it is certified.
        let done = ask(&daemon, ISSUE, &json!({ "wait": true }), "Add a device").await?;
        let encoded = done
            .get("cert")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("this computer's daemon answered no certificate"))?;
        let bytes = hex::decode(encoded).map_err(|error| anyhow!("the certificate: {error}"))?;
        let cert: DeviceCert = tilo_user_identity::canonical::from_canonical_bytes(&bytes)
            .map_err(|error| anyhow!("the certificate: {error}"))?;
        Ok(Some(cert))
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
        println!("Added {} ({}).", cert.device_label, short(&key_hex));
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
        let client = Client::open(Reach {
            api: None,
            key_file: None,
            json: false,
        })?;
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
        let client = Client::open(Reach {
            api: None,
            key_file: None,
            json: false,
        })?;
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

/// The person's devices, as this computer's daemon lists them ([`LIST`]).
fn devices(runtime: &tokio::runtime::Runtime, here: Option<&Device>) -> Result<Vec<Value>> {
    runtime.block_on(async {
        let daemon = reach(here).await?;
        let listed = ask(&daemon, LIST, &json!({}), "List your devices").await?;
        Ok(listed
            .get("devices")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    })
}

/// The start of a device's key, as lists show it.
fn short(key: &str) -> &str {
    key.get(..12).unwrap_or(key)
}

/// A listed device's key.
fn key_of(device: &Value) -> &str {
    device.get("key_hex").and_then(Value::as_str).unwrap_or("")
}

/// How a listed device is named.
fn named(device: &Value) -> &str {
    device.get("label").and_then(Value::as_str).unwrap_or("")
}

/// Whether the listed device's `flag` is set.
fn is(device: &Value, flag: &str) -> bool {
    device.get(flag).and_then(Value::as_bool) == Some(true)
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
            Client::open(Reach {
                api: None,
                key_file: None,
                json: false,
            })?
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
/// This computer's daemon does not run, or its host did not answer.
pub fn list(json: bool) -> Result<ExitCode> {
    let (runtime, here) = daemon()?;
    let devices = devices(&runtime, here.as_ref())?;
    if json {
        println!("{}", Value::Array(devices));
        return Ok(ExitCode::SUCCESS);
    }
    for device in &devices {
        let mut notes = Vec::new();
        if is(device, "current") {
            notes.push("this computer");
        }
        if is(device, "online") {
            notes.push("online");
        }
        if is(device, "revoked") {
            notes.push("revoked");
        }
        let notes = if notes.is_empty() {
            String::new()
        } else {
            format!("  ({})", notes.join(", "))
        };
        println!("{}  {}{notes}", short(key_of(device)), named(device));
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
    /// As the daemon's `devices#revoke` names it.
    const fn name(self) -> &'static str {
        match self {
            Self::Retired => "retired",
            Self::Lost => "lost",
            Self::Stolen => "stolen",
            Self::Compromised => "compromised",
        }
    }
}

/// The one device of `devices` that `which` names: by a prefix of its key, or by its name.
fn pick<'a>(devices: &'a [Value], which: &str) -> Result<&'a Value> {
    let wanted = which.trim().to_ascii_lowercase();
    if wanted.is_empty() {
        bail!("Name the device by its key or its name, as `tilo device ls` lists it.");
    }
    let by_key: Vec<_> = devices
        .iter()
        .filter(|device| key_of(device).starts_with(&wanted))
        .collect();
    let found = if by_key.is_empty() {
        devices
            .iter()
            .filter(|device| named(device).eq_ignore_ascii_case(which.trim()))
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

/// `tilo device rename DEVICE NAME`: what one of the person's devices is called in every list.
///
/// # Errors
/// The device is none or several of the person's, or the host did not take the name.
pub fn rename(which: &str, name: &str) -> Result<ExitCode> {
    let (runtime, here) = daemon()?;
    let devices = devices(&runtime, here.as_ref())?;
    let target = pick(&devices, which)?;
    let args = json!({ "device": key_of(target), "name": name });
    runtime.block_on(async {
        let daemon = reach(here.as_ref()).await?;
        ask(&daemon, RENAME, &args, "Rename a device").await
    })?;
    println!("{} is called {} now.", named(target), name.trim());
    Ok(ExitCode::SUCCESS)
}

/// `tilo device revoke DEVICE`: end that device's login, as the person's root on this computer
/// says, on every host and device its daemon's session reaches.
///
/// # Errors
/// This computer cannot revoke, the device is none or several of the person's, it is this
/// computer, or the person did not confirm with `--yes`.
pub fn revoke(which: &str, why: Why, yes: bool) -> Result<ExitCode> {
    let (runtime, here) = daemon()?;
    let devices = devices(&runtime, here.as_ref())?;
    let target = pick(&devices, which)?;
    if is(target, "current") {
        bail!("That is this computer: revoke it from another of your devices.");
    }
    if !yes {
        bail!(
            "Revoking {} cannot be undone: pass --yes to revoke it.",
            named(target)
        );
    }
    let args = json!({ "device": key_of(target), "reason": why.name() });
    runtime
        .block_on(async {
            let daemon = reach(here.as_ref()).await?;
            ask(&daemon, REVOKE, &args, "Revoke a device").await
        })
        .context("the device was not revoked")?;
    println!("Revoked {}.", named(target));
    Ok(ExitCode::SUCCESS)
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
