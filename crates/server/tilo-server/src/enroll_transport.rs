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

use iroh::RelayUrl;
use iroh::{Endpoint, RelayMap, RelayMode, endpoint::presets};
use iroh_base::{EndpointAddr, PublicKey};
use tilo_user_identity::Dial;

use super::protocol::{
    decrypt_pair_offer, decrypt_response, encrypt_pair_offer, verify_ack, verify_finalized,
};
use super::{
    ApprovalIdentity, ApprovalOptions, DeviceCapabilities, IdentityPaths, JoinOptions, PAIR_MAGIC,
    PairSolicitation, PathBuf, PendingApproval, PreparedJoin, SelectedNearby, WindowClosed,
    accept_request, commit_approval, deadline_after, expected_pair, install_answer,
    load_approval_identity, now_ns, pairing_code, prepare_approval, prepare_join, socket_timeout,
    valid_solicitation, validity_start_ns, verify_answer, verify_response,
};
use anyhow::{Context, bail};

/// What both sides of an enrollment speak over iroh.
const ALPN: &[u8] = b"tilo/enrollment/1";

/// The most one enrollment message may be: the largest packet the local
/// network carries, enough for a personal device's fleet.
const MAX_MESSAGE: usize = 256 * 1024;

/// The runtime every iroh exchange of this program is driven on.
pub(super) fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?)
}

/// The address a token names, as iroh dials it: the device's endpoint id and
/// the packet relay that reaches it.
pub(super) fn address_of(dial: &Dial) -> anyhow::Result<EndpointAddr> {
    let id = PublicKey::from_bytes(&dial.endpoint)
        .map_err(|error| anyhow::anyhow!("enrollment address is not an endpoint id: {error}"))?;
    let mut address = EndpointAddr::new(id);
    if !dial.relay_url.is_empty() {
        address = address.with_relay_url(parse_relay(&dial.relay_url)?);
    }
    Ok(address)
}

fn parse_relay(url: &str) -> anyhow::Result<RelayUrl> {
    url.parse::<RelayUrl>()
        .map_err(|error| anyhow::anyhow!("{url:?} is not a packet relay: {error}"))
}

/// This device's address as a token names it: its endpoint id, and the relay
/// it is reached through.
pub(super) fn dial_of(endpoint: &Endpoint) -> Dial {
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
async fn endpoint_for(
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
async fn send(connection: &iroh::endpoint::Connection, bytes: &[u8]) -> anyhow::Result<()> {
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
async fn receive(connection: &iroh::endpoint::Connection) -> anyhow::Result<Vec<u8>> {
    let mut stream = connection
        .accept_uni()
        .await
        .map_err(|error| anyhow::anyhow!("accept an enrollment stream: {error}"))?;
    stream
        .read_to_end(MAX_MESSAGE)
        .await
        .map_err(|error| anyhow::anyhow!("read an enrollment message: {error}"))
}

/// Enroll this machine over iroh, using `token_text`'s signed addresses: dial
/// the person's devices in the order the token names them until one answers.
pub(super) fn join(token_text: &str, options: JoinOptions<'_>) -> anyhow::Result<Option<PathBuf>> {
    let prepared = prepare_join(token_text, options)?;
    let dial = prepared.token.dial.clone();
    if dial.is_empty() {
        bail!("the enrollment token names no device to dial");
    }
    let runtime = runtime()?;
    let mut last = None;
    for named in &dial {
        match join_one(&runtime, &prepared, options, named) {
            Ok(installed) => return Ok(installed),
            Err(error) => last = Some(error),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no device of the person answered")))
}

/// One dial: the request, the answer, then the acknowledgement and Finalized
/// through [`install_answer`], all on one connection.
fn join_one(
    runtime: &tokio::runtime::Runtime,
    prepared: &PreparedJoin,
    options: JoinOptions<'_>,
    named: &Dial,
) -> anyhow::Result<Option<PathBuf>> {
    // A machine behind a network translation reaches the relay its token
    // names, so its own endpoint takes that relay as home too.
    let relays = if named.relay_url.is_empty() {
        None
    } else {
        Some(RelayMap::from(parse_relay(&named.relay_url)?))
    };
    let address = address_of(named)?;
    let PreparedJoin {
        token,
        request,
        request_bytes,
        response_secret,
        device,
        ..
    } = prepared;
    let connection = runtime.block_on(async {
        let endpoint = endpoint_for(device, relays.as_ref()).await?;
        let connection = tokio::time::timeout(
            socket_timeout(options.timeout),
            endpoint.connect(address, ALPN),
        )
        .await
        .map_err(|_| anyhow::anyhow!("the enrollment window closed"))?
        .map_err(|error| anyhow::anyhow!("dial the person's device: {error}"))?;
        send(&connection, request_bytes).await?;
        let answered = tokio::time::timeout(socket_timeout(options.timeout), receive(&connection))
            .await
            .map_err(|_| anyhow::anyhow!("the approving device did not answer"))??;
        Ok::<_, anyhow::Error>((endpoint, connection, answered))
    })?;
    let (endpoint, connection, response_bytes) = connection;
    let (payload, approver_public_key) = decrypt_response(
        &response_bytes,
        token.nonce,
        device.verifying_key().to_bytes(),
        response_secret,
    )
    .and_then(|plaintext| verify_response(&plaintext, token))
    .context("the approving device's answer did not verify")?;
    verify_answer(&payload, token, request)?;
    let mut confirm = |ack: &[u8]| -> anyhow::Result<()> {
        let finalized = runtime.block_on(async {
            send(&connection, ack).await?;
            let finalized =
                tokio::time::timeout(socket_timeout(options.timeout), receive(&connection))
                    .await
                    .map_err(|_| {
                        anyhow::anyhow!("the approving device did not confirm enrollment")
                    })??;
            Ok::<_, anyhow::Error>(finalized)
        })?;
        verify_finalized(
            &finalized,
            &response_bytes,
            token.nonce,
            device.verifying_key().to_bytes(),
            approver_public_key,
        )
    };
    let installed = install_answer(prepared, options, &payload, &response_bytes, &mut confirm);
    runtime.block_on(endpoint.close());
    installed
}

/// Wait for one of the person's devices to dial this machine and approve it,
/// showing `code` and this machine's address: the code method over iroh.
pub(super) fn wait_for_approval(
    options: JoinOptions<'_>,
    relays: Option<&RelayMap>,
    ready: Option<&std::sync::mpsc::Sender<()>>,
) -> anyhow::Result<()> {
    let JoinOptions { identity_root, .. } = options;
    let paths = IdentityPaths::under(identity_root);
    paths.ensure_directories()?;
    let (device, _) = tilo_device_identity::persist::load_or_create_device_key(&paths)?;
    let server_node_id = tilo_server::daemon::server_node_id(&device);
    let code = pairing_code(device.verifying_key().to_bytes(), server_node_id);
    let runtime = runtime()?;
    let (endpoint, connection, token_text) = runtime.block_on(async {
        let endpoint = endpoint_for(&device, relays).await?;
        // The address is only worth printing once the relay reaches it, and a
        // caller waiting to dial waits for the same moment.
        let _ = tokio::time::timeout(Duration::from_secs(15), endpoint.online()).await;
        if let Some(ready) = ready {
            let _ = ready.send(());
        }
        let address = dial_of(&endpoint);
        println!("Enrollment code: {code}");
        println!(
            "On one of your devices, approve {}, then start its approval with",
            scan_line(&address, &code)
        );
        let solicitation = PairSolicitation {
            magic: PAIR_MAGIC,
            device_public_key: device.verifying_key().to_bytes(),
            server_node_id,
            label: options.label.chars().take(80).collect(),
            kind: options.kind,
        };
        let accepting = tokio::time::timeout(socket_timeout(options.timeout), endpoint.accept())
            .await
            .map_err(|_| {
                WindowClosed("no device of the person dialed this machine before the window closed")
            })?
            .context("this machine's endpoint closed")?;
        let connection = accepting.await.map_err(|error| {
            anyhow::anyhow!("the approving device's connection failed: {error}")
        })?;
        send(&connection, &postcard::to_stdvec(&solicitation)?).await?;
        let offer = tokio::time::timeout(socket_timeout(options.timeout), receive(&connection))
            .await
            .map_err(|_| anyhow::anyhow!("the approving device did not answer"))??;
        let token_text = decrypt_pair_offer(&offer, &device)?;
        Ok::<_, anyhow::Error>((endpoint, connection, token_text))
    })?;
    let prepared = prepare_join(token_text.trim(), options)?;
    let PreparedJoin {
        token,
        request,
        request_bytes,
        response_secret,
        device,
        ..
    } = &prepared;
    let response_bytes = runtime.block_on(async {
        send(&connection, request_bytes).await?;
        let answered = tokio::time::timeout(socket_timeout(options.timeout), receive(&connection))
            .await
            .map_err(|_| anyhow::anyhow!("the approving device did not answer"))??;
        Ok::<_, anyhow::Error>(answered)
    })?;
    let (payload, approver_public_key) = decrypt_response(
        &response_bytes,
        token.nonce,
        device.verifying_key().to_bytes(),
        response_secret,
    )
    .and_then(|plaintext| verify_response(&plaintext, token))
    .context("the approving device's answer did not verify")?;
    verify_answer(&payload, token, request)?;
    let mut confirm = |ack: &[u8]| -> anyhow::Result<()> {
        let finalized = runtime.block_on(async {
            send(&connection, ack).await?;
            let finalized =
                tokio::time::timeout(socket_timeout(options.timeout), receive(&connection))
                    .await
                    .map_err(|_| {
                        anyhow::anyhow!("the approving device did not confirm enrollment")
                    })??;
            Ok::<_, anyhow::Error>(finalized)
        })?;
        verify_finalized(
            &finalized,
            &response_bytes,
            token.nonce,
            device.verifying_key().to_bytes(),
            approver_public_key,
        )
    };
    let installed = install_answer(&prepared, options, &payload, &response_bytes, &mut confirm);
    connection.close(0u32.into(), b"enrolled");
    runtime.block_on(endpoint.close());
    installed?;
    println!("Approved code {code}");
    Ok(())
}

/// What the machine prints for a device of the person to read or scan: this
/// machine's endpoint, the relay that reaches it, and the code to compare.
fn scan_line(address: &Dial, code: &str) -> String {
    let relay = if address.relay_url.is_empty() {
        String::new()
    } else {
        format!("&relay={}", address.relay_url)
    };
    format!(
        "tilo://enroll?endpoint={}{relay}&code={code}",
        hex::encode(address.endpoint)
    )
}

/// Serve the person's devices' enrollments over iroh for `window`: answer the
/// machines that dial this device with a token, and the ones this device dials
/// that show a code.
pub(super) fn serve(
    options: &ApprovalOptions<'_>,
    relays: Option<&RelayMap>,
    dial_machine: Option<&EndpointAddr>,
    code: Option<&str>,
) -> anyhow::Result<()> {
    let identity = load_approval_identity(options)?;
    let runtime = runtime()?;
    runtime.block_on(async {
        let endpoint = endpoint_for(&identity.device, relays).await?;
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
        if let Some(machine) = dial_machine {
            // The machine printed its address before its endpoint finished
            // coming up, so the dial is retried while its window is open.
            let deadline = deadline_after(socket_timeout(options.timeout))?;
            let connection = loop {
                match endpoint.connect(machine.clone(), ALPN).await {
                    Ok(connection) => break connection,
                    Err(error) if std::time::Instant::now() >= deadline => {
                        return Err(anyhow::anyhow!("dial the machine showing a code: {error}"));
                    }
                    Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
                }
            };
            approve_shown_code(&connection, options, &identity, &dial_of(&endpoint), code).await?;
            connection.close(0u32.into(), b"approved");
        } else {
            let incoming = tokio::time::timeout(socket_timeout(options.timeout), endpoint.accept())
                .await
                .map_err(|_| {
                    WindowClosed("no machine dialed this device before the window closed")
                })?;
            let connection = incoming
                .context("this device's endpoint closed")?
                .await
                .map_err(|error| anyhow::anyhow!("a machine's connection failed: {error}"))?;
            approve_connection(&connection, options, &identity, &dial_of(&endpoint)).await?;
            connection.close(0u32.into(), b"approved");
        }
        endpoint.close().await;
        Ok(())
    })
}

/// Answer one connection: a machine that dialed this device with its token, or
/// one this device dialed for its code.
#[allow(
    clippy::future_not_send,
    reason = "the approving device's root signer stays on this thread; the enrollment is driven on one runtime thread"
)]
async fn approve_connection(
    connection: &iroh::endpoint::Connection,
    options: &ApprovalOptions<'_>,
    identity: &ApprovalIdentity,
    dial: &Dial,
) -> anyhow::Result<()> {
    let first = receive(connection).await?;
    if let Ok(solicitation) = postcard::from_bytes::<PairSolicitation>(&first) {
        let selected = select(&solicitation, options, identity, dial)?;
        answer(connection, options, identity, Some(&selected)).await
    } else {
        let pending = accept_request(&first, options, identity, None, None)?
            .context("the machine's request did not verify")?;
        finish(connection, options, identity, &pending).await
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
    let shown = pairing_code(solicitation.device_public_key, solicitation.server_node_id);
    if code.is_some_and(|code| code != shown) {
        bail!("the code on this machine's screen is not the one the machine shows");
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
) -> anyhow::Result<SelectedNearby> {
    let expected = expected_pair(options)?;
    if !valid_solicitation(solicitation)
        || solicitation.kind != options.kind
        || expected.is_some_and(|(device, service)| {
            solicitation.device_public_key != device || solicitation.server_node_id != service
        })
    {
        bail!("the machine's solicitation does not match this approval");
    }
    let now = now_ns()?;
    // Backdated for the joining device's clock, but never to before this
    // device was certified: redemption checks the issuer at that instant.
    let issued_at_ns = validity_start_ns(now).max(identity.issuer_cert.issued_at_ns);
    let expires_at_ns = now.saturating_add(5 * 60 * 1_000_000_000);
    let token_text = tilo_identity::enrollment::mint_token(
        identity.user_public_key,
        DeviceCapabilities::member(),
        issued_at_ns,
        expires_at_ns,
        std::slice::from_ref(dial),
        &identity.device,
    )?;
    Ok(SelectedNearby {
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
    selected: Option<&SelectedNearby>,
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
    pending: &PendingApproval,
) -> anyhow::Result<()> {
    let prepared = prepare_approval(options, identity, pending)?;
    send(connection, &prepared.encrypted).await?;
    let ack = tokio::time::timeout(socket_timeout(options.timeout), receive(connection))
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
pub(super) fn scan(text: &str) -> anyhow::Result<(EndpointAddr, Option<String>)> {
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

#[cfg(test)]
#[path = "enroll_transport_tests.rs"]
mod tests;
