//! The machine's side of an enrollment over iroh: it dials the devices its
//! token names, or serves its own endpoint and shows a code for one of the
//! person's devices to dial. The serving side is [`tilo_enrollment::transport`].

use std::path::PathBuf;
use std::time::Duration;

use iroh::RelayMap;
use tilo_user_identity::{Dial, IdentityPaths};

use anyhow::{Context, bail};

use tilo_enrollment::protocol::{
    PAIR_MAGIC, PairSolicitation, decrypt_pair_offer, decrypt_response, verify_finalized,
};
use tilo_enrollment::transport::{
    ALPN, address_of, dial_of, endpoint_for, parse_relay, receive, runtime, send,
};
use tilo_enrollment::{WindowClosed, socket_timeout};

use crate::{
    JoinOptions, PreparedJoin, install_answer, pairing_code, prepare_join, verify_answer,
    verify_response,
};

/// Enroll this machine over iroh, using `token_text`'s signed addresses: dial
/// the person's devices in the order the token names them until one answers.
pub(super) fn join(token_text: &str, options: JoinOptions<'_>) -> anyhow::Result<Option<PathBuf>> {
    let prepared = prepare_join(token_text, options)?;
    // The code this machine shows, which the person's device computes from the
    // same keys: what they compare with what this machine printed.
    println!(
        "Enrollment code: {}",
        pairing_code(
            prepared.request.device_public_key,
            prepared.request.server_node_id,
        )
    );
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
        let line = scan_line(&address, &code);
        println!("Enrollment code: {code}");
        if let Some(code) = qr(&line) {
            println!("Scan this code on one of your devices:");
            println!("{code}");
        }
        println!("Or paste this line there to approve this machine:");
        println!("{line}");
        let solicitation = PairSolicitation {
            magic: PAIR_MAGIC,
            device_public_key: device.verifying_key().to_bytes(),
            server_node_id,
            label: options.label.chars().take(80).collect(),
            hardware: super::hardware_summary(),
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

/// `line` as a QR code a camera reads off the console: dark modules left blank
/// and light ones drawn as blocks, which a dark terminal shows as a white code
/// on black, two module rows to a line. `None` when the line does not fit a code,
/// which leaves the line itself to be read.
pub(super) fn qr(line: &str) -> Option<String> {
    use qrcode::render::unicode::Dense1x2;
    let code = qrcode::QrCode::new(line.as_bytes()).ok()?;
    Some(
        code.render::<Dense1x2>()
            .quiet_zone(true)
            .dark_color(Dense1x2::Light)
            .light_color(Dense1x2::Dark)
            .build(),
    )
}
