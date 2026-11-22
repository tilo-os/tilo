//! `tilo device add`, `tilo device ls` and `tilo device revoke`: the person's devices, from a
//! computer that is logged in and holds their root.
//!
//! `add` serves a ticket ([`tilo_device_identity::login`]) on an endpoint of its own while the
//! ticket lasts. A computer that runs `tilo login` with it reaches this one directly and calls the
//! certify action there; the person's root here certifies its key. The person ran `add`
//! themselves, so this tilo approves the computer that comes with the ticket ([`RootApprover`]).
//! A device that does not hold the root forwards the same question to one that does through
//! another [`Approver`].

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use ed25519_dalek::{Signer as _, SigningKey};
use iroh::protocol::Router;
use serde_json::json;
use tilo_device_identity::login::{
    Approval, Approved, Approver, Asked, Declined, Entry, SCOPE, TICKET_LIFETIME_NS, Ticket,
};
use tilo_peer_state::actions::{ACTIONS_ALPN, ActionService, NoActions};
use tilo_peer_state::new_device::ServedTicket;
use tilo_server_session::workspaces::{PersonalDevice, WorkspaceLabels};
use tilo_session_client::device::Device as Entered;
use tilo_user_identity::{DeviceCert, RevocationList, RevocationReason, StatementSigner};

use crate::login::{self, Device};

/// How long the issuer keeps serving once the computer is certified, for it to read its answer.
const TELL_WAIT: Duration = Duration::from_secs(30);

/// How long the issuer keeps serving after it told the computer how its call ended: an answer
/// that ended with its connection is asked for again, and answered where the call is.
const TELL_GRACE: Duration = Duration::from_secs(5);

/// How long a revocation is waited for to show in the person's list.
const REVOKED_WAIT: Duration = Duration::from_secs(20);

/// The person's root on this computer approving the computer that came with a ticket: it signs
/// the computer's certificate with the root, and the approval with this device's own key, which
/// the root certified as [`Self::cert`].
pub(crate) struct RootApprover<R> {
    pub root: R,
    pub device: SigningKey,
    pub cert: DeviceCert,
}

impl<R: StatementSigner + Send + Sync + 'static> Approver for RootApprover<R> {
    fn approve(&self, asked: Asked) -> Approval {
        let mut cert = asked.cert.clone();
        let approved = tilo_user_identity::sign_device_cert(&mut cert, &self.root)
            .map(|()| Approved {
                cert,
                approver: self.cert.clone(),
                signature: self.device.sign(&asked.digest).to_bytes(),
            })
            .map_err(|error| Declined(format!("this device's root could not sign: {error}")));
        Box::pin(std::future::ready(approved))
    }
}

/// A ticket served from this computer, and what serves it.
pub(crate) struct Issued {
    pub ticket: Ticket,
    pub served: ServedTicket,
    router: Router,
}

impl Issued {
    /// Stop serving the ticket.
    pub async fn end(self) {
        let _ = self.router.shutdown().await;
    }
}

/// Serve a new ticket for a computer to enter where the login `here` enters, approved by
/// `approver`, on an endpoint of its own bound to the server's relay.
///
/// # Errors
/// The login names no server, or no endpoint binds.
pub(crate) async fn issue(here: &Device, approver: Arc<dyn Approver>) -> Result<Issued> {
    let published = here
        .server_address
        .as_ref()
        .ok_or_else(|| anyhow!(login::LOGIN_ENDED))?;
    let dialed = login::server_of(here).await?;
    // A key of its own: the endpoint lives as long as the ticket, beside any other this
    // computer runs with its device key.
    let key = SigningKey::from_bytes(&rand::random::<[u8; 32]>());
    let endpoint = dialed
        .bind(Some(key.to_bytes()))
        .await
        .map_err(|error| anyhow!("this computer could not listen for the new one: {error}"))?;
    if dialed.relay().is_some() {
        // The ticket names the relay only once the endpoint reached it.
        let _ = tokio::time::timeout(Duration::from_secs(15), endpoint.online()).await;
    }
    let ticket = Ticket::new(login::reached_of(&endpoint.addr()));
    let mut reached = login::reached_of(&published.endpoint_addr);
    if reached.relay.is_none() {
        reached.relay.clone_from(&published.relay_url);
    }
    let enter = Entry {
        workspace: here.workspace.clone(),
        workspace_name: here.workspace_name.clone(),
        server_cert: here.server_cert.clone(),
        server: reached,
        site: here.site.clone(),
        host_origin: here.host_origin.clone(),
    };
    let served = ServedTicket::new(
        ticket.clone(),
        here.cert.user_public_key,
        enter,
        approver,
        RevocationList::new(),
    );
    let service = ActionService::new(key, Arc::new(NoActions));
    service.admit_uncertified(SCOPE, Arc::new(served.clone()));
    let router = Router::builder(endpoint)
        .accept(ACTIONS_ALPN, service)
        .spawn();
    Ok(Issued {
        ticket,
        served,
        router,
    })
}

/// Wait for `issued` to certify its computer, while its ticket lasts, then for the computer to
/// read its answer: the certificate it was given.
///
/// # Errors
/// The ticket ran out with no computer certified.
pub(crate) async fn certified(issued: &Issued) -> Result<DeviceCert> {
    let mut certified = issued.served.certified();
    let given = tokio::time::timeout(
        Duration::from_nanos(TICKET_LIFETIME_NS),
        certified.wait_for(Option::is_some),
    )
    .await
    .map(|found| found.map(|cert| cert.clone()));
    let Ok(Ok(cert)) = given else {
        bail!(
            "No computer logged in with the ticket before it ran out: run `tilo device add` again."
        );
    };
    let mut told = issued.served.told();
    let _ = tokio::time::timeout(TELL_WAIT, told.wait_for(|told| *told)).await;
    tokio::time::sleep(TELL_GRACE).await;
    cert.context("the certificate given is there")
}

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
/// it was.
///
/// # Errors
/// This computer cannot add devices, or no computer came before the ticket ran out.
pub fn add(json: bool) -> Result<ExitCode> {
    login::trust_test_certificates()?;
    let (key, here, person) = person_here()?;
    let runtime = login::runtime()?;
    runtime.block_on(async {
        let approver = Arc::new(RootApprover {
            root: person.root,
            device: person.device,
            cert: person.cert,
        });
        let issued = issue(&here, approver).await?;
        let ticket = issued.ticket.encode();
        if json {
            println!("{}", json!({ "ticket": ticket }));
        } else {
            eprintln!("On the computer to add, run:");
            println!("tilo login {ticket}");
            eprintln!("The ticket works once, for 10 minutes. Waiting for it…");
        }
        let cert = certified(&issued).await;
        issued.end().await;
        let cert = cert?;
        // The person's other devices list it as soon as the host keeps it there.
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
    })
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
