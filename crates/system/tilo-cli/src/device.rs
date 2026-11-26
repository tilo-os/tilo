//! `tilo device add`, `tilo device ls` and `tilo device revoke`: the person's devices, from a
//! computer that is logged in and holds their root.
//!
//! `add` has this computer's daemon serve a ticket ([`tilo_device_identity::login`]) on the
//! endpoint it already runs, while the ticket lasts. A computer that runs `tilo login` with it
//! reaches this one directly and calls the certify action there; the person's root here certifies
//! its key. The person ran `add` themselves, so the daemon approves the computer that comes with
//! the ticket automatically. A device that does not hold the root forwards the same question to
//! one that does through another approver.

use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_api::daemon::DaemonPlace;
use tilo_device_identity::login::Ticket;
use tilo_server_session::workspaces::{PersonalDevice, WorkspaceLabels};
use tilo_session_client::device::Device as Entered;
use tilo_user_identity::{DeviceCert, RevocationReason};

use crate::api::{Client, Reach};
use crate::login::{self, Device};

/// What a computer added with a ticket becomes.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Adding {
    /// An ordinary member device.
    Member,
    /// A device that administers the person's other devices.
    Admin,
}

/// How long a revocation is waited for to show in the person's list.
const REVOKED_WAIT: Duration = Duration::from_secs(20);

/// Keep `cert` in the person's list of devices, named for the agent it runs: what every device
/// of theirs reads back.
async fn keep_in_list(entered: &Entered, cert: &DeviceCert) -> Result<()> {
    let mut list = entered
        .personal_list(&WorkspaceLabels::new(), None)
        .await
        .map_err(|refusal| anyhow!("{refusal}"))?;
    list.devices
        .retain(|kept| kept.cert.device_public_key != cert.device_public_key);
    list.devices.push(PersonalDevice {
        cert: cert.clone(),
        name: None,
        revoked: false,
        browser: None,
        agent: None,
        last_active_ms: None,
        online: false,
    });
    entered
        .keep_personal(&list)
        .await
        .map_err(|refusal| anyhow!("{refusal}"))
}

/// This computer's login and the person whose root it holds, checked to be one person.
fn person_here() -> Result<(SigningKey, Device, crate::identity::Person)> {
    crate::identity::convert_earlier()?;
    let (key, here) = login::load(&login::Place::here(crate::identity::paths())?)?
        .ok_or_else(|| anyhow!(login::NOT_LOGGED_IN))?;
    let person = crate::identity::held()?.ok_or_else(|| {
        anyhow!(
            "This computer does not hold your identity, so it cannot add a device: run \
             `tilo device add` on the one that does."
        )
    })?;
    if person.public_key() != here.cert.user_public_key {
        bail!("This computer is logged in as another person than the one whose identity it holds.");
    }
    Ok((key, here, person))
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
#[allow(
    clippy::too_many_lines,
    reason = "one command: the ticket a browser showed, or printing one and waiting for it"
)]
pub fn add(
    ticket: Option<&str>,
    json: bool,
    adding: Adding,
    no_wait: bool,
    plan: Option<&str>,
    server: bool,
) -> Result<ExitCode> {
    if let Some(ticket) = ticket {
        return sign_in(ticket, json);
    }
    login::trust_test_certificates()?;
    crate::identity::convert_earlier()?;
    let login_place = login::Place::here(crate::identity::paths())?;
    // A login that ended names a workspace no daemon here holds: the daemon serves this
    // computer as the person's own device instead, in the workspace it founded.
    let here = login::load_public(&login_place)?.filter(Device::reaches_a_server);
    let place = DaemonPlace::of(login_place.dir(), &login::this_device_public_key()?)
        .map_err(anyhow::Error::msg)?;
    login::ensure_daemon(&place)?;
    let runtime = login::runtime()?;
    let cert = runtime.block_on(async {
        let client = Client::open(Reach {
            api: None,
            key_file: None,
            json: false,
        })?;
        let workspace = match &here {
            Some(device) => device.workspace.clone(),
            None => client.founded_workspace().await?,
        };
        let capability = match adding {
            Adding::Member => "member",
            Adding::Admin => "admin",
        };
        // A role the person did not ask for goes as null, which the daemon reads as none.
        let args =
            json!({ "capability": capability, "plan": plan, "server": server.then_some(true) });
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
                "The ticket works once, for 10 minutes. It adds the computer as {}.",
                match adding {
                    Adding::Member => "an ordinary device",
                    Adding::Admin => "an administrator of your other devices",
                }
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
        let cert: DeviceCert = tilo_user_identity::canonical::from_canonical_bytes(&bytes)
            .map_err(|error| anyhow!("the certificate: {error}"))?;
        // The person's other devices list it as soon as the host keeps it there. A computer with
        // no login has no session of its own to enter with: the host lists it as it enters.
        // The daemon calls are done, so this process may read the private key now.
        if here.is_some()
            && let Some((key, here)) = login::load(&login_place)?
        {
            match login::connect(&key, &here, None).await {
                Ok(entered) => {
                    let kept = keep_in_list(&entered, &cert).await;
                    entered.close();
                    if let Err(error) = kept {
                        eprintln!("Your other devices list it once it enters: {error}");
                    }
                }
                Err(error) => eprintln!("Your other devices list it once it enters: {error}"),
            }
        }
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
    use crate::ticket_action;
    use tilo_device_identity::login::{CertifiedAction, Direction};

    login::trust_test_certificates()?;
    crate::identity::convert_earlier()?;
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
    let login_place = login::Place::here(crate::identity::paths())?;
    let here = login::load_public(&login_place)?.ok_or_else(|| anyhow!(login::NOT_LOGGED_IN))?;
    let place = DaemonPlace::of(login_place.dir(), &here.cert.device_public_key)
        .map_err(anyhow::Error::msg)?;
    login::ensure_daemon(&place)?;
    let text = parsed.encode();
    let waiting = ticket_action::call(CertifiedAction::Pending, &json!({"ticket": text}))?;
    let device = waiting["device"]
        .as_str()
        .context("this computer's daemon answered no browser")?;
    let label = waiting["label"].as_str().unwrap_or("a browser");
    let capability = waiting["capability"]
        .as_str()
        .unwrap_or("a browser of yours");
    if !json {
        println!("This browser asks to be one of your devices: \"{label}\" ({capability}).");
    }
    let tab = tilo_device_identity::login::Tab {
        device_public_key: device.to_owned(),
        label: label.to_owned(),
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
    ticket_action::call(
        CertifiedAction::Approve,
        &json!({"ticket": text, "cert": cert, "label": here.cert.device_label}),
    )?;
    if json {
        println!("{}", json!({"label": label, "device": device}));
    } else if cert.is_some() {
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

/// The person's devices as the host this computer's login reaches lists them.
async fn devices(key: &SigningKey, here: &Device) -> Result<Vec<PersonalDevice>> {
    let entered = login::connect(key, here, None)
        .await
        .map_err(|error| anyhow!(login::enter_said(&error)))?;
    let listed = entered
        .personal_list(&WorkspaceLabels::new(), None)
        .await
        .map_err(|refusal| anyhow!("{refusal}"));
    entered.close();
    Ok(listed?.devices)
}

/// The start of a device's key, as lists show it.
fn short(cert: &DeviceCert) -> String {
    hex::encode(cert.device_public_key.get(..6).unwrap_or_default())
}

/// How a device is named in a list.
fn named(device: &PersonalDevice) -> &str {
    device.name.as_deref().unwrap_or(&device.cert.device_label)
}

/// `tilo device ls`: the person's devices.
///
/// # Errors
/// This computer is not logged in, or its host did not answer.
pub fn list(json: bool) -> Result<ExitCode> {
    login::trust_test_certificates()?;
    let (key, here) = login::load(&login::Place::here(crate::identity::paths())?)?
        .ok_or_else(|| anyhow!(login::NOT_LOGGED_IN))?;
    let runtime = login::runtime()?;
    let devices = runtime.block_on(devices(&key, &here))?;
    let this = here.cert.device_public_key;
    if json {
        let listed: Vec<_> = devices
            .iter()
            .map(|device| {
                json!({
                    "device": hex::encode(device.cert.device_public_key),
                    "label": named(device),
                    "agent": device.agent,
                    "online": device.online,
                    "revoked": device.revoked,
                    "this": device.cert.device_public_key == this,
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(listed));
        return Ok(ExitCode::SUCCESS);
    }
    for device in &devices {
        let mut notes = Vec::new();
        if device.cert.device_public_key == this {
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
        println!("{}  {}{notes}", short(&device.cert), named(device));
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

impl From<Why> for RevocationReason {
    fn from(why: Why) -> Self {
        match why {
            Why::Retired => Self::Retired,
            Why::Lost => Self::Lost,
            Why::Stolen => Self::Stolen,
            Why::Compromised => Self::Compromised,
        }
    }
}

/// The one device of `devices` that `which` names: by a prefix of its key, or by its name.
fn pick<'a>(devices: &'a [PersonalDevice], which: &str) -> Result<&'a PersonalDevice> {
    let wanted = which.trim().to_ascii_lowercase();
    if wanted.is_empty() {
        bail!("Name the device by its key or its name, as `tilo device ls` lists it.");
    }
    let by_key: Vec<_> = devices
        .iter()
        .filter(|device| hex::encode(device.cert.device_public_key).starts_with(&wanted))
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

/// `tilo device revoke DEVICE`: end that device's login, as the person's root here says, on
/// every host and device this computer's session reaches.
///
/// # Errors
/// This computer cannot revoke, the device is none or several of the person's, it is this
/// computer, or the person did not confirm with `--yes`.
pub fn revoke(which: &str, why: Why, yes: bool) -> Result<ExitCode> {
    login::trust_test_certificates()?;
    let (key, here, person) = person_here()?;
    let runtime = login::runtime()?;
    runtime.block_on(async {
        let devices = devices(&key, &here).await?;
        let target = pick(&devices, which)?;
        let device = target.cert.device_public_key;
        if device == here.cert.device_public_key || device == person.cert.device_public_key {
            bail!("That is this computer: revoke it from another of your devices.");
        }
        if !yes {
            bail!(
                "Revoking {} cannot be undone: pass --yes to revoke it.",
                named(target)
            );
        }
        let entry = tilo_server_client::revoke_device(
            &person.root,
            device,
            tilo_clock::unix_now_ns(),
            why.into(),
        )
        .await
        .map_err(|error| anyhow!("{error}"))?;
        let entered = login::connect(&key, &here, None)
            .await
            .map_err(|error| anyhow!(login::enter_said(&error)))?;
        let revoked = async {
            login::announce_revocation(&entered, entry)?;
            // The host answers the list from the revocations it keeps: once it lists the device
            // as revoked, every device it serves refuses it.
            let started = std::time::Instant::now();
            loop {
                let listed = entered
                    .personal_list(&WorkspaceLabels::new(), None)
                    .await
                    .map_err(|refusal| anyhow!("{refusal}"))?;
                if listed
                    .devices
                    .iter()
                    .any(|kept| kept.cert.device_public_key == device && kept.revoked)
                {
                    return Ok(());
                }
                if started.elapsed() > REVOKED_WAIT {
                    bail!("The host did not take the revocation: run it again.");
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
        .await;
        entered.close();
        revoked?;
        println!("Revoked {}.", named(target));
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
