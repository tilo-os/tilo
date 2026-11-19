//! The one enrollment over iroh: a machine anywhere on the internet reaches
//! one of the person's devices, and a machine that shows a code is reached by
//! one of them.
//!
//! The messages are the ones the local network carries
//! ([`super::protocol`]); only the way they travel differs, so a machine
//! behind a network address translation enrolls exactly as one beside the
//! device does. Each message is one unidirectional stream, in order:
//!
//! * With a token, the machine dials the device the token names in its signed
//!   [`Dial`](tilo_user_identity::Dial) list: request, answer, acknowledgement,
//!   Finalized.
//! * With a code, the machine serves its endpoint and shows its code and that
//!   address; a device of the person dials it, is told the machine's
//!   solicitation, answers with a sealed token, and the exchange continues as
//!   with a token.
//!
//! Neither trusts the connection: every message keeps the sealing and
//! signature checks the local network applies, and the token's own signature
//! covers the addresses it names.

use std::net::Ipv4Addr;
use std::time::Duration;

use anyhow::Context;
use iroh::RelayUrl;
use iroh::{Endpoint, RelayMap, RelayMode, endpoint::presets};
use iroh_base::{EndpointAddr, PublicKey};
use tilo_user_identity::Dial;

use crate::approval::{
    ApprovalIdentity, ApprovalOptions, PreparedApproval, accept_request, commit_approval,
    expected_pair, load_approval_identity, prepare_approval, waiting_of,
};
use crate::protocol::{PairSolicitation, encrypt_pair_offer, verify_ack};
use crate::waiting::{Decision, Person, WaitingMachine};
use crate::{valid_solicitation, validity_start_ns};

/// What both sides of an enrollment speak over iroh.
pub const ALPN: &[u8] = b"tilo/enrollment/1";

/// The most one enrollment message may be: the largest packet the local
/// network carries, enough for a personal device's fleet.
const MAX_MESSAGE: usize = 256 * 1024;

/// The runtime every iroh exchange of an enrollment is driven on.
///
/// # Errors
///
/// The runtime cannot be made.
pub fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?)
}

/// The address a token names, as iroh dials it: the device's endpoint id and
/// the packet relay that reaches it.
///
/// # Errors
///
/// The token's address is not an endpoint id or relay.
pub fn address_of(dial: &Dial) -> anyhow::Result<EndpointAddr> {
    let id = PublicKey::from_bytes(&dial.endpoint)
        .map_err(|error| anyhow::anyhow!("enrollment address is not an endpoint id: {error}"))?;
    let mut address = EndpointAddr::new(id);
    if !dial.relay_url.is_empty() {
        address = address.with_relay_url(parse_relay(&dial.relay_url)?);
    }
    Ok(address)
}

pub fn parse_relay(url: &str) -> anyhow::Result<RelayUrl> {
    url.parse::<RelayUrl>()
        .map_err(|error| anyhow::anyhow!("{url:?} is not a packet relay: {error}"))
}

/// This device's address as a token names it: its endpoint id, and the relay
/// it is reached through.
#[must_use]
pub fn dial_of(endpoint: &Endpoint) -> Dial {
    let relay_url = endpoint
        .addr()
        .relay_urls()
        .next()
        .map(ToString::to_string)
        .unwrap_or_default();
    Dial {
        endpoint: *endpoint.id().as_bytes(),
        relay_url,
    }
}

/// An endpoint of this device's own key, reached through `relay` when one is
/// given, accepting just this enrollment.
pub async fn endpoint_for(
    device: &ed25519_dalek::SigningKey,
    relays: Option<&RelayMap>,
) -> anyhow::Result<Endpoint> {
    let mode = relays.map_or(RelayMode::Disabled, |relays| {
        RelayMode::Custom(relays.clone())
    });
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&device.to_bytes()))
        .alpns(vec![ALPN.to_vec()])
        .relay_mode(mode)
        .bind_addr((Ipv4Addr::UNSPECIFIED, 0))
        .map_err(|error| anyhow::anyhow!("this machine's endpoint address: {error}"))?
        .bind()
        .await
        .map_err(|error| anyhow::anyhow!("bind this machine's endpoint: {error}"))?;
    Ok(endpoint)
}

/// Send one message as one unidirectional stream, so the peer reads it whole
/// and in order.
pub async fn send(connection: &iroh::endpoint::Connection, bytes: &[u8]) -> anyhow::Result<()> {
    let mut stream = connection
        .open_uni()
        .await
        .map_err(|error| anyhow::anyhow!("open an enrollment stream: {error}"))?;
    stream
        .write_all(bytes)
        .await
        .map_err(|error| anyhow::anyhow!("write an enrollment message: {error}"))?;
    stream
        .finish()
        .map_err(|error| anyhow::anyhow!("finish an enrollment message: {error}"))?;
    Ok(())
}

/// The next message the peer sent, as one whole unidirectional stream.
pub async fn receive(connection: &iroh::endpoint::Connection) -> anyhow::Result<Vec<u8>> {
    let mut stream = connection
        .accept_uni()
        .await
        .map_err(|error| anyhow::anyhow!("accept an enrollment stream: {error}"))?;
    stream
        .read_to_end(MAX_MESSAGE)
        .await
        .map_err(|error| anyhow::anyhow!("read an enrollment message: {error}"))
}

/// What became of one machine that dialed this device.
enum Admission {
    Admitted,
    Declined,
}

/// The key the enrollment endpoint answers under: derived from the device's
/// own key, so serving an enrollment never contends with the session's own
/// endpoint for the device's iroh identity. A token names this endpoint, and
/// it exists while this device serves an enrollment.
#[must_use]
pub fn serving_key(device: &ed25519_dalek::SigningKey) -> ed25519_dalek::SigningKey {
    tilo_user_identity::enrollment_serving_key(device)
}

/// Serve the person's devices' enrollments over iroh for `window`: answer the
/// machines that dial this device with a token, or the one this device dials
/// that shows a code. With `ask`, each machine that dials is put to the person
/// first, and serving goes on until the window closes.
///
/// # Errors
///
/// This device cannot open its enrollment endpoint, no machine dialled it
/// before the window closed, or the answer could not be read.
pub fn serve(
    options: &ApprovalOptions<'_>,
    relays: Option<&RelayMap>,
    dial_machine: Option<&EndpointAddr>,
    code: Option<&str>,
    ask: bool,
) -> anyhow::Result<()> {
    let mut person = ask.then_some(crate::waiting::Console);
    serve_with(
        options,
        relays,
        dial_machine,
        code,
        person.as_mut().map(|person| person as &mut dyn Person),
    )
}

/// [`serve`], with `person` answering the machines that dial this device: the
/// console in production, whatever a test or a session hands it.
#[allow(
    clippy::future_not_send,
    reason = "the approving device's root signer stays on this thread; the enrollment is driven on one runtime thread"
)]
pub fn serve_with(
    options: &ApprovalOptions<'_>,
    relays: Option<&RelayMap>,
    dial_machine: Option<&EndpointAddr>,
    code: Option<&str>,
    person: Option<&mut dyn Person>,
) -> anyhow::Result<()> {
    let identity = load_approval_identity(options)?;
    let runtime = runtime()?;
    runtime.block_on(async {
        let mut person = person;
        let endpoint = endpoint_for(&serving_key(&identity.device), relays).await?;
        let _ = tokio::time::timeout(Duration::from_secs(15), endpoint.online()).await;
        // The port an iroh endpoint took is not what callers wait for; they
        // wait for it to be up, which is here.
        if let Some(ready) = options.ready {
            let _ = ready.send(0);
        }
        println!(
            "{}",
            serde_json::json!({
                "endpoint": hex::encode(endpoint.id().as_bytes()),
            })
        );
        let dial = dial_of(&endpoint);
        if let Some(machine) = dial_machine {
            // The machine printed its address before its endpoint finished
            // coming up, so the dial is retried while its window is open.
            let deadline = crate::deadline_after(crate::socket_timeout(options.timeout))?;
            let connection = loop {
                match endpoint.connect(machine.clone(), ALPN).await {
                    Ok(connection) => break connection,
                    Err(error) if std::time::Instant::now() >= deadline => {
                        return Err(anyhow::anyhow!("dial the machine showing a code: {error}"));
                    }
                    Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
                }
            };
            approve_shown_code(&connection, options, &identity, &dial, code).await?;
            connection.close(0u32.into(), b"approved");
        } else {
            let deadline = crate::deadline_after(crate::socket_timeout(options.timeout))?;
            let asking = person.is_some();
            loop {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    return Err(crate::WindowClosed(
                        "no machine dialed this device before the window closed",
                    )
                    .into());
                }
                let Ok(incoming) = tokio::time::timeout(left, endpoint.accept()).await else {
                    return Err(crate::WindowClosed(
                        "no machine dialed this device before the window closed",
                    )
                    .into());
                };
                let connection = incoming
                    .context("this device's endpoint closed")?
                    .await
                    .map_err(|error| anyhow::anyhow!("a machine's connection failed: {error}"))?;
                let answered = approve_connection(
                    &connection,
                    std::slice::from_ref(options),
                    &identity,
                    &dial,
                    &mut person,
                )
                .await;
                connection.close(0u32.into(), b"enrolled");
                match answered {
                    Ok(Admission::Admitted) if !asking => return Ok(()),
                    Ok(_) => {}
                    Err(error) if asking => {
                        // One machine that does not verify is not the window
                        // closing: the person is still there to answer the next.
                        eprintln!("{error:#}");
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        endpoint.close().await;
        Ok(())
    })
}

/// Serve this device's enrollment endpoint for as long as the session is up: every
/// machine that dials it with the person's token is put to `person`, and the
/// endpoint stays up between machines, so a token made on any of the person's
/// devices is answered whenever one of them is on.
///
/// # Errors
///
/// This device cannot open its enrollment endpoint.
#[allow(
    clippy::future_not_send,
    reason = "the approving device's root signer stays on this thread; the enrollment is driven on one runtime thread"
)]
pub fn serve_for_lifetime(
    options: &[ApprovalOptions<'_>],
    relays: Option<&RelayMap>,
    person: &mut dyn Person,
    ready: Option<&std::sync::mpsc::Sender<()>>,
) -> anyhow::Result<()> {
    let mut person: Option<&mut dyn Person> = Some(person);
    let primary = options
        .first()
        .context("this device answers no enrollment kind")?;
    let identity = load_approval_identity(primary)?;
    let runtime = runtime()?;
    runtime.block_on(async {
        let endpoint = endpoint_for(&serving_key(&identity.device), relays).await?;
        let _ = tokio::time::timeout(Duration::from_secs(15), endpoint.online()).await;
        if let Some(ready) = ready {
            let _ = ready.send(());
        }
        let dial = dial_of(&endpoint);
        tracing::info!(
            endpoint = %hex::encode(endpoint.id().as_bytes()),
            "answering machines on this device's enrollment endpoint"
        );
        loop {
            let Some(incoming) = endpoint.accept().await else {
                return Ok(());
            };
            let connection = match incoming.await {
                Ok(connection) => connection,
                Err(error) => {
                    tracing::info!(%error, "a machine's connection failed");
                    continue;
                }
            };
            let answered =
                approve_connection(&connection, options, &identity, &dial, &mut person).await;
            connection.close(0u32.into(), b"enrolled");
            if let Err(error) = answered {
                // One machine that does not verify is not the end of serving:
                // the person is still there to answer the next.
                tracing::info!("{error:#}");
            }
        }
    })
}

/// Dial a machine that shows a code and approve it, in this process: what a
/// session does with the `tilo://enroll?…` line the person pasted.
///
/// # Errors
///
/// The line names no machine, this device cannot dial it, or the machine did
/// not verify.
pub fn approve_line(
    options: &ApprovalOptions<'_>,
    relays: Option<&RelayMap>,
    line: &str,
) -> anyhow::Result<()> {
    let (machine, code) = scan(line)?;
    serve_with(options, relays, Some(&machine), code.as_deref(), None)
}

/// Answer one connection: a machine that dialed this device with its token, or
/// one this device dialed for its code. With `person`, the machine is put to
/// them before anything is admitted.
#[allow(
    clippy::future_not_send,
    reason = "the approving device's root signer stays on this thread; the enrollment is driven on one runtime thread"
)]
async fn approve_connection(
    connection: &iroh::endpoint::Connection,
    options: &[ApprovalOptions<'_>],
    identity: &ApprovalIdentity,
    dial: &Dial,
    person: &mut Option<&mut dyn Person>,
) -> anyhow::Result<Admission> {
    let first = receive(connection).await?;
    if let Ok(solicitation) = postcard::from_bytes::<PairSolicitation>(&first) {
        let options = options
            .iter()
            .find(|options| options.kind == solicitation.kind)
            .or_else(|| options.first())
            .context("this device answers no enrollment of that kind")?;
        if let Some(person) = person.as_deref_mut() {
            let waiting = WaitingMachine::of(
                solicitation.device_public_key,
                solicitation.server_node_id,
                &solicitation.label,
                &solicitation.hardware,
            );
            if person.decide(&waiting)? == Decision::Decline {
                return Ok(Admission::Declined);
            }
        }
        let selected = select(&solicitation, options, identity, dial)?;
        answer(connection, options, identity, Some(&selected)).await?;
        Ok(Admission::Admitted)
    } else {
        let (options, pending) = options
            .iter()
            .find_map(|options| {
                accept_request(&first, options, identity, None, None)
                    .ok()
                    .flatten()
                    .map(|pending| (options, pending))
            })
            .context("the machine's request did not verify")?;
        if let Some(person) = person.as_deref_mut()
            && person.decide(&waiting_of(&pending))? == Decision::Decline
        {
            return Ok(Admission::Declined);
        }
        finish(connection, options, identity, &pending).await?;
        Ok(Admission::Admitted)
    }
}

/// A machine that shows a code: read its solicitation, check the code the
/// person read off its screen, hand it a token, and take its request.
#[allow(
    clippy::future_not_send,
    reason = "the approving device's root signer stays on this thread; the enrollment is driven on one runtime thread"
)]
async fn approve_shown_code(
    connection: &iroh::endpoint::Connection,
    options: &ApprovalOptions<'_>,
    identity: &ApprovalIdentity,
    dial: &Dial,
    code: Option<&str>,
) -> anyhow::Result<()> {
    let solicitation: PairSolicitation = postcard::from_bytes(&receive(connection).await?)
        .context("the machine did not announce itself")?;
    let shown = tilo_identity::certification_ceremony::pairing_code(
        solicitation.device_public_key,
        solicitation.server_node_id,
    );
    if code.is_some_and(|code| code != shown) {
        anyhow::bail!("the code on this machine's screen is not the one the machine shows");
    }
    let selected = select(&solicitation, options, identity, dial)?;
    println!(
        "Approving device '{}' with code {shown}",
        solicitation.label
    );
    answer(connection, options, identity, Some(&selected)).await
}

/// Mint the token a selected machine is offered.
fn select(
    solicitation: &PairSolicitation,
    options: &ApprovalOptions<'_>,
    identity: &ApprovalIdentity,
    dial: &Dial,
) -> anyhow::Result<crate::approval::SelectedNearby> {
    let expected = expected_pair(options)?;
    if !valid_solicitation(solicitation)
        || solicitation.kind != options.kind
        || expected.is_some_and(|(device, service)| {
            solicitation.device_public_key != device || solicitation.server_node_id != service
        })
    {
        anyhow::bail!("the machine's solicitation does not match this approval");
    }
    let now = crate::now_ns()?;
    // Backdated for the joining device's clock, but never to before this
    // device was certified: redemption checks the issuer at that instant.
    let issued_at_ns = validity_start_ns(now).max(identity.issuer_cert.issued_at_ns);
    let expires_at_ns = now.saturating_add(5 * 60 * 1_000_000_000);
    let token_text = tilo_identity::enrollment::mint_token(
        identity.user_public_key,
        tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns,
        expires_at_ns,
        std::slice::from_ref(dial),
        &identity.device,
    )?;
    Ok(crate::approval::SelectedNearby {
        solicitation: solicitation.clone(),
        token_text,
    })
}

/// Send the sealed offer, then take the request and answer it.
#[allow(
    clippy::future_not_send,
    reason = "the approving device's root signer stays on this thread; the enrollment is driven on one runtime thread"
)]
async fn answer(
    connection: &iroh::endpoint::Connection,
    options: &ApprovalOptions<'_>,
    identity: &ApprovalIdentity,
    selected: Option<&crate::approval::SelectedNearby>,
) -> anyhow::Result<()> {
    if let Some(selection) = selected {
        let offer = encrypt_pair_offer(
            &selection.token_text,
            selection.solicitation.device_public_key,
        )?;
        send(connection, &offer).await?;
    }
    let request = receive(connection).await?;
    let pending = accept_request(&request, options, identity, selected, None)?
        .context("the machine's request did not verify")?;
    finish(connection, options, identity, &pending).await
}

/// Redeem the token, send the answer, wait for the acknowledgement, commit,
/// and confirm.
#[allow(
    clippy::future_not_send,
    reason = "the approving device's root signer stays on this thread; the enrollment is driven on one runtime thread"
)]
async fn finish(
    connection: &iroh::endpoint::Connection,
    options: &ApprovalOptions<'_>,
    identity: &ApprovalIdentity,
    pending: &crate::approval::PendingApproval,
) -> anyhow::Result<()> {
    let prepared: PreparedApproval = prepare_approval(options, identity, pending)?;
    send(connection, &prepared.encrypted).await?;
    let ack = tokio::time::timeout(crate::socket_timeout(options.timeout), receive(connection))
        .await
        .map_err(|_| anyhow::anyhow!("the machine did not acknowledge its configuration"))??;
    verify_ack(
        &ack,
        &prepared.encrypted,
        pending.token.nonce,
        pending.request.device_public_key,
    )?;
    let finalized = commit_approval(options, identity, pending, &prepared)?;
    send(connection, &finalized).await?;
    // Closing before the machine has read the Finalized would lose it: wait
    // for the machine to close, which it does once it is installed.
    let _ = tokio::time::timeout(Duration::from_secs(30), connection.closed()).await;
    Ok(())
}

/// The machine a `tilo://enroll?endpoint=…&relay=…&code=…` line names, with
/// the code it shows: what a person scans off another machine's console.
///
/// # Errors
///
/// The line is not one a machine printed.
pub fn scan(text: &str) -> anyhow::Result<(EndpointAddr, Option<String>)> {
    let query = text
        .trim()
        .strip_prefix("tilo://enroll?")
        .context("a machine's enrollment line starts with tilo://enroll?")?;
    let mut endpoint = None;
    let mut relay = None;
    let mut code = None;
    for pair in query.split('&') {
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        match name {
            "endpoint" => endpoint = Some(value.to_owned()),
            "relay" => relay = Some(value.to_owned()),
            "code" => code = Some(value.to_owned()),
            _ => {}
        }
    }
    let endpoint = endpoint.context("the enrollment line names no endpoint")?;
    let bytes: [u8; 32] = hex::decode(endpoint)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .context("the enrollment line's endpoint is not a device key")?;
    let dial = Dial {
        endpoint: bytes,
        relay_url: relay.unwrap_or_default(),
    };
    Ok((address_of(&dial)?, code))
}
