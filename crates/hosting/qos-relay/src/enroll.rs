//! LAN/OOB completion for a pre-authorised qOS device enrollment token.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{Context, bail};
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{ChaCha20Poly1305, KeyInit, Nonce};
use clap::{Parser, Subcommand};
use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use qos_relay::HomePeerRegistration;
use qos_user_identity::{
    DeviceCapabilities, DeviceCert, IdentityPaths, decode_enrollment_token,
    encode_enrollment_token, mint_enrollment_token, sign_device_cert, verify_device_cert,
    verify_enrollment_token,
};
use rand::RngCore as _;
use serde::{Deserialize, Serialize};

const PORT: u16 = 45_831;
const MAX_PACKET: usize = 60 * 1024;
const RESPONSE_DOMAIN: &[u8] = b"qos-host-enrollment-response-v1\0";
const PAIR_DOMAIN: &[u8] = b"qos-nearby-home-peer-offer-v1\0";
const PAIR_MAGIC: [u8; 8] = *b"QOSPAIR1";

#[derive(Parser)]
#[command(name = "qos-enroll")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Find one unprovisioned home server broadcasting on this LAN.
    Discover {
        #[arg(long, default_value_t = 15)]
        timeout: u64,
    },
    /// Join the identity named by TOKEN, discovering its approving device on this LAN.
    Join {
        #[arg(long)]
        token_file: PathBuf,
        #[arg(long, default_value = "/var/lib/qos")]
        identity_root: PathBuf,
        #[arg(long, default_value = "Home server")]
        label: String,
        #[arg(long, default_value_t = 270)]
        timeout: u64,
        #[arg(long, default_value = "/etc/qos/home-peer.toml")]
        home_peer_output: PathBuf,
    },
    /// Pair with a nearby trusted qOS device without pre-installing a token.
    Pair {
        #[arg(long, default_value = "/var/lib/qos")]
        identity_root: PathBuf,
        #[arg(long, default_value = "Home server")]
        label: String,
        #[arg(long, default_value_t = 270)]
        timeout: u64,
        #[arg(long, default_value = "/etc/qos/home-peer.toml")]
        home_peer_output: PathBuf,
    },
    /// Approve one nearby host token issued by this device.
    Approve {
        #[arg(long, default_value = "/var/lib/qos")]
        identity_root: PathBuf,
        #[arg(long, default_value_t = 60)]
        timeout: u64,
        #[arg(long, default_value = "/var/cache/quick-fs")]
        workspace_cache: PathBuf,
        #[arg(long, default_value = "/var/lib/qos/fs-capabilities.json")]
        acl: PathBuf,
        #[arg(long = "relay-workspace-id")]
        relay_workspace_ids: Vec<u64>,
        #[arg(long)]
        expected_device: Option<String>,
        #[arg(long)]
        expected_service: Option<String>,
    },
}

#[derive(Serialize, Deserialize)]
struct Request {
    token: String,
    device_public_key: [u8; 32],
    relay_service_node_id: [u8; 32],
    label: String,
}

#[derive(Serialize, Deserialize)]
struct EncryptedRequest {
    ephemeral_public_key: [u8; 32],
    nonce: [u8; 12],
    ciphertext: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct ResponsePayload {
    nonce: [u8; 16],
    recipient_device_public_key: [u8; 32],
    relay_service_node_id: [u8; 32],
    cert: DeviceCert,
    home_peer_toml: String,
}

#[derive(Serialize, Deserialize)]
struct Response {
    payload: ResponsePayload,
    signature: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct EncryptedResponse {
    ephemeral_public_key: [u8; 32],
    nonce: [u8; 12],
    ciphertext: Vec<u8>,
}

#[derive(Clone, Serialize, Deserialize)]
struct PairSolicitation {
    magic: [u8; 8],
    device_public_key: [u8; 32],
    relay_service_node_id: [u8; 32],
    label: String,
}

#[derive(Serialize, Deserialize)]
struct PairCandidate {
    device_node_id: String,
    relay_service_node_id: String,
    label: String,
    verification_code: String,
}

#[derive(Serialize, Deserialize)]
struct PairOffer {
    ephemeral_public_key: [u8; 32],
    nonce: [u8; 12],
    ciphertext: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct AckPayload {
    nonce: [u8; 16],
    device_public_key: [u8; 32],
    response_digest: [u8; 32],
}

#[derive(Serialize, Deserialize)]
struct Ack {
    payload: AckPayload,
    signature: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct FinalizedPayload {
    nonce: [u8; 16],
    device_public_key: [u8; 32],
    response_digest: [u8; 32],
}

#[derive(Serialize, Deserialize)]
struct Finalized {
    payload: FinalizedPayload,
    signature: Vec<u8>,
}

fn register_home_peer(
    identity_root: &std::path::Path,
    cert: &DeviceCert,
    relay_service_node_id: [u8; 32],
    relay_workspace_ids: &[u64],
) -> anyhow::Result<()> {
    let path = identity_root.join("home-peers.json");
    if cert.device_label.is_empty()
        || cert.device_label.len() > 80
        || relay_workspace_ids.len() > 4096
    {
        bail!("home-peer registration exceeds its bounded schema");
    }
    let mut registrations = match std::fs::read(&path) {
        Ok(bytes) if bytes.len() <= 1024 * 1024 => {
            serde_json::from_slice::<Vec<HomePeerRegistration>>(&bytes)
                .context("existing home-peer registry is invalid")?
        }
        Ok(_) => bail!("existing home-peer registry exceeds 1 MiB"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    if registrations.len() > 128
        || registrations.iter().any(|entry| {
            entry.device_node_id.len() != 64
                || hex::decode(&entry.device_node_id).is_err()
                || entry.relay_service_node_id.len() != 64
                || hex::decode(&entry.relay_service_node_id).is_err()
                || entry.user_public_key.len() != 64
                || hex::decode(&entry.user_public_key).is_err()
                || entry.device_cert.as_ref().is_some_and(|cert| {
                    verify_device_cert(cert).is_err()
                        || hex::encode(cert.device_public_key) != entry.device_node_id
                        || hex::encode(cert.user_public_key) != entry.user_public_key
                })
                || entry.label.is_empty()
                || entry.label.len() > 80
                || entry.relay_workspace_ids.len() > 4096
        })
    {
        bail!("existing home-peer registry violates its bounded schema");
    }
    let device_node_id = hex::encode(cert.device_public_key);
    registrations.retain(|entry| entry.device_node_id != device_node_id);
    registrations.push(HomePeerRegistration {
        schema_version: HomePeerRegistration::CURRENT_SCHEMA,
        device_node_id,
        relay_service_node_id: hex::encode(relay_service_node_id),
        label: cert.device_label.clone(),
        user_public_key: hex::encode(cert.user_public_key),
        device_cert: Some(cert.clone()),
        relay_workspace_ids: relay_workspace_ids.to_vec(),
    });
    if registrations.len() > 128 {
        bail!("home-peer registry is full");
    }
    registrations.sort_by(|left, right| left.device_node_id.cmp(&right.device_node_id));
    atomic_file::write_with_mode(&path, &serde_json::to_vec_pretty(&registrations)?, 0o644)?;
    Ok(())
}

fn response_preimage(payload: &ResponsePayload) -> anyhow::Result<Vec<u8>> {
    let mut preimage = RESPONSE_DOMAIN.to_vec();
    preimage.extend(postcard::to_stdvec(payload)?);
    Ok(preimage)
}

fn sign_response(
    payload: ResponsePayload,
    root: &ed25519_dalek::SigningKey,
) -> anyhow::Result<Response> {
    let signature = root.sign(&response_preimage(&payload)?).to_bytes().to_vec();
    Ok(Response { payload, signature })
}

fn verify_response(response: &Response, user_public_key: [u8; 32]) -> anyhow::Result<()> {
    let signature: [u8; 64] = response
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("enrollment response signature has the wrong length"))?;
    VerifyingKey::from_bytes(&user_public_key)?
        .verify(
            &response_preimage(&response.payload)?,
            &Signature::from_bytes(&signature),
        )
        .context("enrollment response was changed in transit")
}

fn pairing_code(device_public_key: [u8; 32], relay_service_node_id: [u8; 32]) -> String {
    let mut input = PAIR_DOMAIN.to_vec();
    input.extend(device_public_key);
    input.extend(relay_service_node_id);
    let digest = blake3::hash(&input);
    let number = u32::from_le_bytes(digest.as_bytes()[..4].try_into().expect("four bytes"));
    format!("{:06}", number % 1_000_000)
}

fn valid_solicitation(solicitation: &PairSolicitation) -> bool {
    solicitation.magic == PAIR_MAGIC
        && VerifyingKey::from_bytes(&solicitation.device_public_key).is_ok()
        && solicitation.relay_service_node_id != [0; 32]
        && !solicitation.label.trim().is_empty()
        && solicitation.label.len() <= 80
}

fn discover(timeout: u64) -> anyhow::Result<()> {
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, PORT))?;
    socket.set_read_timeout(Some(Duration::from_secs(1)))?;
    let deadline = std::time::Instant::now() + socket_timeout(timeout);
    let mut packet = [0_u8; MAX_PACKET];
    while std::time::Instant::now() < deadline {
        match socket.recv_from(&mut packet) {
            Ok((len, _)) => {
                let Ok(solicitation) = postcard::from_bytes::<PairSolicitation>(&packet[..len])
                else {
                    continue;
                };
                if !valid_solicitation(&solicitation) {
                    continue;
                }
                println!(
                    "{}",
                    serde_json::to_string(&PairCandidate {
                        device_node_id: hex::encode(solicitation.device_public_key),
                        relay_service_node_id: hex::encode(solicitation.relay_service_node_id),
                        label: solicitation.label,
                        verification_code: pairing_code(
                            solicitation.device_public_key,
                            solicitation.relay_service_node_id,
                        ),
                    })?
                );
                return Ok(());
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    bail!("no unprovisioned qOS home server was found nearby")
}

fn pair_offer_key(
    shared: &[u8; 32],
    recipient: [u8; 32],
    ephemeral_public_key: [u8; 32],
) -> [u8; 32] {
    let mut input = PAIR_DOMAIN.to_vec();
    input.extend(shared);
    input.extend(recipient);
    input.extend(ephemeral_public_key);
    *blake3::hash(&input).as_bytes()
}

fn encrypt_pair_offer(token: &str, recipient: [u8; 32]) -> anyhow::Result<Vec<u8>> {
    let mut seed = [0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut seed);
    let ephemeral = x25519_dalek::StaticSecret::from(seed);
    let ephemeral_public_key = x25519_dalek::PublicKey::from(&ephemeral).to_bytes();
    let recipient_public = VerifyingKey::from_bytes(&recipient)?
        .to_montgomery()
        .to_bytes();
    let shared = ephemeral.diffie_hellman(&x25519_dalek::PublicKey::from(recipient_public));
    let key = pair_offer_key(shared.as_bytes(), recipient, ephemeral_public_key);
    let mut nonce = [0_u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let ciphertext = ChaCha20Poly1305::new((&key).into())
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: token.as_bytes(),
                aad: &[PAIR_DOMAIN, &recipient].concat(),
            },
        )
        .map_err(|_| anyhow::anyhow!("encrypt nearby pairing offer"))?;
    postcard::to_stdvec(&PairOffer {
        ephemeral_public_key,
        nonce,
        ciphertext,
    })
    .map_err(Into::into)
}

fn decrypt_pair_offer(
    bytes: &[u8],
    recipient: &ed25519_dalek::SigningKey,
) -> anyhow::Result<String> {
    let offer: PairOffer = postcard::from_bytes(bytes)?;
    let recipient_public = recipient.verifying_key().to_bytes();
    let recipient_secret = x25519_dalek::StaticSecret::from(recipient.to_scalar_bytes());
    let shared =
        recipient_secret.diffie_hellman(&x25519_dalek::PublicKey::from(offer.ephemeral_public_key));
    let key = pair_offer_key(
        shared.as_bytes(),
        recipient_public,
        offer.ephemeral_public_key,
    );
    let plaintext = ChaCha20Poly1305::new((&key).into())
        .decrypt(
            Nonce::from_slice(&offer.nonce),
            Payload {
                msg: &offer.ciphertext,
                aad: &[PAIR_DOMAIN, &recipient_public].concat(),
            },
        )
        .map_err(|_| anyhow::anyhow!("nearby pairing offer was not encrypted for this server"))?;
    String::from_utf8(plaintext).context("nearby pairing offer is not text")
}

fn response_aad(token_nonce: [u8; 16], recipient: [u8; 32]) -> Vec<u8> {
    let mut aad = RESPONSE_DOMAIN.to_vec();
    aad.extend(token_nonce);
    aad.extend(recipient);
    aad
}

fn request_key(shared: &[u8; 32], ephemeral_public_key: [u8; 32]) -> [u8; 32] {
    let mut input = b"qos-host-enrollment-request-key-v1\0".to_vec();
    input.extend(shared);
    input.extend(ephemeral_public_key);
    *blake3::hash(&input).as_bytes()
}

fn encrypt_request(
    request: &Request,
    issuer_device_public_key: [u8; 32],
) -> anyhow::Result<(Vec<u8>, x25519_dalek::StaticSecret)> {
    let mut seed = [0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut seed);
    let ephemeral = x25519_dalek::StaticSecret::from(seed);
    let ephemeral_public_key = x25519_dalek::PublicKey::from(&ephemeral).to_bytes();
    let issuer_public = VerifyingKey::from_bytes(&issuer_device_public_key)?
        .to_montgomery()
        .to_bytes();
    let shared = ephemeral.diffie_hellman(&x25519_dalek::PublicKey::from(issuer_public));
    let key = request_key(shared.as_bytes(), ephemeral_public_key);
    let mut nonce = [0_u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let ciphertext = ChaCha20Poly1305::new((&key).into())
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &postcard::to_stdvec(request)?,
                aad: &ephemeral_public_key,
            },
        )
        .map_err(|_| anyhow::anyhow!("encrypt enrollment request"))?;
    Ok((
        postcard::to_stdvec(&EncryptedRequest {
            ephemeral_public_key,
            nonce,
            ciphertext,
        })?,
        ephemeral,
    ))
}

fn decrypt_request(
    bytes: &[u8],
    issuer_device: &ed25519_dalek::SigningKey,
) -> anyhow::Result<(Request, [u8; 32])> {
    let encrypted: EncryptedRequest = postcard::from_bytes(bytes)?;
    let issuer_secret = x25519_dalek::StaticSecret::from(issuer_device.to_scalar_bytes());
    let shared = issuer_secret.diffie_hellman(&x25519_dalek::PublicKey::from(
        encrypted.ephemeral_public_key,
    ));
    let key = request_key(shared.as_bytes(), encrypted.ephemeral_public_key);
    let plaintext = ChaCha20Poly1305::new((&key).into())
        .decrypt(
            Nonce::from_slice(&encrypted.nonce),
            Payload {
                msg: &encrypted.ciphertext,
                aad: &encrypted.ephemeral_public_key,
            },
        )
        .map_err(|_| anyhow::anyhow!("enrollment request was not encrypted for this issuer"))?;
    Ok((
        postcard::from_bytes(&plaintext)?,
        encrypted.ephemeral_public_key,
    ))
}

fn response_key(shared: &[u8; 32], token_nonce: [u8; 16], recipient: [u8; 32]) -> [u8; 32] {
    let mut input = b"qos-host-enrollment-key-v1\0".to_vec();
    input.extend(shared);
    input.extend(token_nonce);
    input.extend(recipient);
    *blake3::hash(&input).as_bytes()
}

fn encrypt_response(
    response: &Response,
    token_nonce: [u8; 16],
    recipient: [u8; 32],
    recipient_ephemeral_public_key: [u8; 32],
) -> anyhow::Result<Vec<u8>> {
    let mut ephemeral_seed = [0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut ephemeral_seed);
    let ephemeral = x25519_dalek::StaticSecret::from(ephemeral_seed);
    let shared = ephemeral.diffie_hellman(&x25519_dalek::PublicKey::from(
        recipient_ephemeral_public_key,
    ));
    let key = response_key(shared.as_bytes(), token_nonce, recipient);
    let mut nonce = [0_u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let ciphertext = ChaCha20Poly1305::new((&key).into())
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &postcard::to_stdvec(response)?,
                aad: &response_aad(token_nonce, recipient),
            },
        )
        .map_err(|_| anyhow::anyhow!("encrypt enrollment response"))?;
    Ok(postcard::to_stdvec(&EncryptedResponse {
        ephemeral_public_key: x25519_dalek::PublicKey::from(&ephemeral).to_bytes(),
        nonce,
        ciphertext,
    })?)
}

fn decrypt_response(
    bytes: &[u8],
    token_nonce: [u8; 16],
    recipient: [u8; 32],
    request_ephemeral: &x25519_dalek::StaticSecret,
) -> anyhow::Result<Response> {
    let encrypted: EncryptedResponse = postcard::from_bytes(bytes)?;
    let shared = request_ephemeral.diffie_hellman(&x25519_dalek::PublicKey::from(
        encrypted.ephemeral_public_key,
    ));
    let key = response_key(shared.as_bytes(), token_nonce, recipient);
    let plaintext = ChaCha20Poly1305::new((&key).into())
        .decrypt(
            Nonce::from_slice(&encrypted.nonce),
            Payload {
                msg: &encrypted.ciphertext,
                aad: &response_aad(token_nonce, recipient),
            },
        )
        .map_err(|_| anyhow::anyhow!("enrollment response was not encrypted for this device"))?;
    postcard::from_bytes(&plaintext).map_err(Into::into)
}

fn response_digest(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

fn make_ack(
    response_bytes: &[u8],
    nonce: [u8; 16],
    device: &ed25519_dalek::SigningKey,
) -> anyhow::Result<Vec<u8>> {
    let payload = AckPayload {
        nonce,
        device_public_key: device.verifying_key().to_bytes(),
        response_digest: response_digest(response_bytes),
    };
    let encoded = postcard::to_stdvec(&payload)?;
    let ack = Ack {
        payload,
        signature: device.sign(&encoded).to_bytes().to_vec(),
    };
    Ok(postcard::to_stdvec(&ack)?)
}

fn verify_ack(
    bytes: &[u8],
    response_bytes: &[u8],
    nonce: [u8; 16],
    device_public_key: [u8; 32],
) -> anyhow::Result<()> {
    let ack: Ack = postcard::from_bytes(bytes)?;
    if ack.payload.nonce != nonce
        || ack.payload.device_public_key != device_public_key
        || ack.payload.response_digest != response_digest(response_bytes)
    {
        bail!("enrollment acknowledgement did not match the response");
    }
    let signature: [u8; 64] = ack
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("acknowledgement signature has the wrong length"))?;
    VerifyingKey::from_bytes(&device_public_key)?.verify(
        &postcard::to_stdvec(&ack.payload)?,
        &Signature::from_bytes(&signature),
    )?;
    Ok(())
}

fn make_finalized(
    response_bytes: &[u8],
    nonce: [u8; 16],
    device_public_key: [u8; 32],
    root: &ed25519_dalek::SigningKey,
) -> anyhow::Result<Vec<u8>> {
    let payload = FinalizedPayload {
        nonce,
        device_public_key,
        response_digest: response_digest(response_bytes),
    };
    let encoded = postcard::to_stdvec(&payload)?;
    Ok(postcard::to_stdvec(&Finalized {
        payload,
        signature: root.sign(&encoded).to_bytes().to_vec(),
    })?)
}

fn verify_finalized(
    bytes: &[u8],
    response_bytes: &[u8],
    nonce: [u8; 16],
    device_public_key: [u8; 32],
    user_public_key: [u8; 32],
) -> anyhow::Result<()> {
    let finalized: Finalized = postcard::from_bytes(bytes)?;
    if finalized.payload.nonce != nonce
        || finalized.payload.device_public_key != device_public_key
        || finalized.payload.response_digest != response_digest(response_bytes)
    {
        bail!("enrollment completion did not match this exchange");
    }
    let signature: [u8; 64] = finalized
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("completion signature has the wrong length"))?;
    VerifyingKey::from_bytes(&user_public_key)?.verify(
        &postcard::to_stdvec(&finalized.payload)?,
        &Signature::from_bytes(&signature),
    )?;
    Ok(())
}

fn consume_nonce(
    identity_root: &std::path::Path,
    nonce: &[u8; 16],
    device_public_key: [u8; 32],
) -> anyhow::Result<()> {
    let dir = identity_root.join("enrollment-nonces");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(hex::encode(nonce));
    let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = std::fs::read_to_string(path)?;
            if existing.trim() == hex::encode(device_public_key) {
                return Ok(());
            }
            bail!("enrollment token has already been consumed by another device");
        }
        Err(error) => return Err(error.into()),
    };
    file.write_all(hex::encode(device_public_key).as_bytes())?;
    file.sync_all()?;
    Ok(())
}

fn now_ns() -> anyhow::Result<u64> {
    Ok(u64::try_from(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos(),
    )
    .unwrap_or(u64::MAX))
}

fn socket_timeout(seconds: u64) -> Duration {
    Duration::from_secs(seconds.clamp(1, 300))
}

fn join(
    token_file: PathBuf,
    identity_root: PathBuf,
    label: String,
    timeout: u64,
    home_peer_output: PathBuf,
) -> anyhow::Result<()> {
    let token_text = std::fs::read_to_string(&token_file)
        .with_context(|| format!("read {}", token_file.display()))?;
    join_with_token(
        token_text.trim(),
        identity_root,
        label,
        timeout,
        home_peer_output,
    )
}

fn join_with_token(
    token_text: &str,
    identity_root: PathBuf,
    label: String,
    timeout: u64,
    home_peer_output: PathBuf,
) -> anyhow::Result<()> {
    let token = decode_enrollment_token(token_text).context("decode enrollment token")?;
    verify_enrollment_token(&token, now_ns()?).context("verify enrollment token")?;
    if !token.capabilities.can_act_as_host || !token.capabilities.can_read_all_workspaces {
        bail!("enrollment token does not grant home-server capabilities");
    }
    let paths = IdentityPaths::under(&identity_root);
    paths.ensure_directories()?;
    let (device, _) = qos_user_identity::persist::load_or_create_device_key(&paths)?;
    qos_user_identity::persist::write_public_key(&paths.user_pubkey, &token.user_public_key)?;
    let request = Request {
        token: token_text.to_owned(),
        device_public_key: device.verifying_key().to_bytes(),
        relay_service_node_id: qos_relay::daemon::relay_service_node_id(&device),
        label: label.chars().take(80).collect(),
    };
    let (bytes, request_ephemeral) = encrypt_request(&request, token.issuer_device_public_key)?;
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    socket.set_read_timeout(Some(Duration::from_secs(2)))?;
    let deadline = std::time::Instant::now() + socket_timeout(timeout);
    let destination = SocketAddrV4::new(Ipv4Addr::BROADCAST, PORT);
    let mut reply = [0_u8; MAX_PACKET];
    while std::time::Instant::now() < deadline {
        socket.send_to(&bytes, destination)?;
        match socket.recv_from(&mut reply) {
            Ok((len, sender)) => {
                let response_bytes = reply[..len].to_vec();
                let Ok(response) = decrypt_response(
                    &response_bytes,
                    token.nonce,
                    device.verifying_key().to_bytes(),
                    &request_ephemeral,
                ) else {
                    continue;
                };
                if verify_response(&response, token.user_public_key).is_err() {
                    continue;
                }
                if response.payload.nonce != token.nonce
                    || response.payload.recipient_device_public_key != request.device_public_key
                    || response.payload.relay_service_node_id != request.relay_service_node_id
                    || response.payload.cert.device_public_key != request.device_public_key
                    || response.payload.cert.user_public_key != token.user_public_key
                    || response.payload.cert.capabilities != token.capabilities
                {
                    continue;
                }
                verify_device_cert(&response.payload.cert)?;
                let _: fs::full_replica::HomePeerSet =
                    toml::from_str(&response.payload.home_peer_toml)
                        .context("approving device returned an invalid home-peer configuration")?;
                atomic_file::write(
                    &home_peer_output,
                    response.payload.home_peer_toml.as_bytes(),
                )?;
                let per_user = paths.device_cert_for_user(&response.payload.cert.user_public_key);
                qos_user_identity::persist::write_device_cert(&per_user, &response.payload.cert)?;
                let ack = make_ack(&response_bytes, token.nonce, &device)?;
                socket.set_read_timeout(Some(Duration::from_millis(300)))?;
                let completion_deadline = std::time::Instant::now() + Duration::from_secs(10);
                while std::time::Instant::now() < completion_deadline {
                    socket.send_to(&ack, sender)?;
                    match socket.recv_from(&mut reply) {
                        Ok((final_len, final_sender)) if final_sender == sender => {
                            if verify_finalized(
                                &reply[..final_len],
                                &response_bytes,
                                token.nonce,
                                device.verifying_key().to_bytes(),
                                token.user_public_key,
                            )
                            .is_ok()
                            {
                                // Publish the default cert as the completion
                                // marker only after the approver durably
                                // registers us and authenticates Finalized.
                                qos_user_identity::persist::write_device_cert(
                                    &paths.device_cert,
                                    &response.payload.cert,
                                )?;
                                consume_nonce(
                                    &identity_root,
                                    &token.nonce,
                                    device.verifying_key().to_bytes(),
                                )?;
                                println!("enrolled {}", response.payload.cert.device_label);
                                return Ok(());
                            }
                        }
                        Ok(_) => {}
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                bail!("approving device did not confirm enrollment; retrying is safe");
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    bail!("no approving qOS device answered before the enrollment window closed")
}

fn pair(
    identity_root: PathBuf,
    label: String,
    timeout: u64,
    home_peer_output: PathBuf,
) -> anyhow::Result<()> {
    let paths = IdentityPaths::under(&identity_root);
    paths.ensure_directories()?;
    let (device, _) = qos_user_identity::persist::load_or_create_device_key(&paths)?;
    let relay_service_node_id = qos_relay::daemon::relay_service_node_id(&device);
    let label: String = label.chars().take(80).collect();
    if label.trim().is_empty() {
        bail!("home-server label must not be empty");
    }
    let solicitation = postcard::to_stdvec(&PairSolicitation {
        magic: PAIR_MAGIC,
        device_public_key: device.verifying_key().to_bytes(),
        relay_service_node_id,
        label: label.clone(),
    })?;
    let code = pairing_code(device.verifying_key().to_bytes(), relay_service_node_id);
    println!("Nearby pairing code: {code}");
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    socket.set_read_timeout(Some(Duration::from_secs(2)))?;
    let destination = SocketAddrV4::new(Ipv4Addr::BROADCAST, PORT);
    let deadline = std::time::Instant::now() + socket_timeout(timeout);
    let mut reply = [0_u8; MAX_PACKET];
    while std::time::Instant::now() < deadline {
        socket.send_to(&solicitation, destination)?;
        match socket.recv_from(&mut reply) {
            Ok((len, _sender)) => {
                let Ok(token_text) = decrypt_pair_offer(&reply[..len], &device) else {
                    continue;
                };
                let Ok(token) = decode_enrollment_token(token_text.trim()) else {
                    continue;
                };
                if verify_enrollment_token(&token, now_ns()?).is_err()
                    || !token.capabilities.can_act_as_host
                    || !token.capabilities.can_read_all_workspaces
                {
                    continue;
                }
                println!("Nearby qOS device approved code {code}");
                return join_with_token(
                    token_text.trim(),
                    identity_root,
                    label,
                    timeout,
                    home_peer_output,
                );
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    bail!("no trusted qOS device approved nearby pairing before the window closed")
}

fn approve(
    identity_root: PathBuf,
    timeout: u64,
    workspace_cache: PathBuf,
    acl: PathBuf,
    mut relay_workspace_ids: Vec<u64>,
    expected_device: Option<String>,
    expected_service: Option<String>,
) -> anyhow::Result<()> {
    let expected: Option<([u8; 32], [u8; 32])> = match (expected_device, expected_service) {
        (Some(device), Some(service)) => Some((
            hex::decode(device)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("expected device ID has the wrong length"))?,
            hex::decode(service)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("expected service ID has the wrong length"))?,
        )),
        (None, None) => None,
        _ => bail!("expected device and service IDs must be supplied together"),
    };
    let paths = IdentityPaths::under(&identity_root);
    let root = qos_user_identity::persist::read_signing_key(&paths.user_privkey)
        .context("this device does not hold the identity root needed to approve enrollment")?;
    let device = qos_user_identity::persist::read_device_key(&paths.device_privkey)?;
    let user_public_key = root.verifying_key().to_bytes();
    relay_workspace_ids.push(qos_relay::personal_continuity_workspace_id(
        &user_public_key,
    ));
    let issuer_cert =
        qos_user_identity::persist::read_device_cert(&paths.device_cert_for_user(&user_public_key))
            .or_else(|_| qos_user_identity::persist::read_device_cert(&paths.device_cert))?;
    verify_device_cert(&issuer_cert)?;
    if issuer_cert.device_public_key != device.verifying_key().to_bytes()
        || !issuer_cert.capabilities.can_admit_devices
    {
        bail!("this device is not authorised to approve enrollment");
    }
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, PORT))?;
    socket.set_read_timeout(Some(Duration::from_secs(1)))?;
    let mut packet = [0_u8; MAX_PACKET];
    let request_deadline = std::time::Instant::now() + socket_timeout(timeout);
    let mut selected_nearby: Option<(PairSolicitation, String, Vec<u8>)> = None;
    let (request, request_ephemeral_public_key, token, sender) = loop {
        if std::time::Instant::now() >= request_deadline {
            bail!("no valid nearby home-server request arrived");
        }
        let (len, sender) = match socket.recv_from(&mut packet) {
            Ok(packet) => packet,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if let Ok(solicitation) = postcard::from_bytes::<PairSolicitation>(&packet[..len]) {
            if !valid_solicitation(&solicitation)
                || expected.is_some_and(|(device, service)| {
                    solicitation.device_public_key != device
                        || solicitation.relay_service_node_id != service
                })
            {
                continue;
            }
            let selection_matches = selected_nearby.as_ref().is_none_or(|(selected, _, _)| {
                selected.device_public_key == solicitation.device_public_key
                    && selected.relay_service_node_id == solicitation.relay_service_node_id
            });
            if !selection_matches {
                continue;
            }
            if selected_nearby.is_none() {
                let mut nonce = [0_u8; 16];
                rand::rngs::OsRng.fill_bytes(&mut nonce);
                let expires_at_ns = now_ns()?.saturating_add(5 * 60 * 1_000_000_000);
                let token = mint_enrollment_token(
                    user_public_key,
                    DeviceCapabilities::host(),
                    expires_at_ns,
                    nonce,
                    &device,
                )?;
                let token_text = encode_enrollment_token(&token)?;
                let offer = encrypt_pair_offer(&token_text, solicitation.device_public_key)?;
                let code = pairing_code(
                    solicitation.device_public_key,
                    solicitation.relay_service_node_id,
                );
                println!(
                    "Approving nearby home server '{}' with code {code}",
                    solicitation.label
                );
                selected_nearby = Some((solicitation, token_text, offer));
            }
            if let Some((_, _, offer)) = &selected_nearby {
                socket.send_to(offer, sender)?;
            }
            continue;
        }
        let Ok((request, request_ephemeral_public_key)) = decrypt_request(&packet[..len], &device)
        else {
            continue;
        };
        let Ok(token) = decode_enrollment_token(request.token.trim()) else {
            continue;
        };
        if verify_enrollment_token(&token, now_ns()?).is_err()
            || token.user_public_key != user_public_key
            || token.issuer_device_public_key != device.verifying_key().to_bytes()
            || !token.capabilities.can_act_as_host
            || !token.capabilities.can_read_all_workspaces
        {
            continue;
        }
        if let Some((selected, selected_token, _)) = &selected_nearby {
            if request.device_public_key != selected.device_public_key
                || request.relay_service_node_id != selected.relay_service_node_id
                || request.label != selected.label
                || request.token != *selected_token
            {
                continue;
            }
        }
        break (request, request_ephemeral_public_key, token, sender);
    };
    let mut cert = DeviceCert {
        user_public_key,
        device_public_key: request.device_public_key,
        device_label: request.label,
        capabilities: token.capabilities,
        issued_at_ns: now_ns()?,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &root)?;
    let ticket_text = std::fs::read_to_string(workspace_cache.join("workspace-ticket"))
        .context("the local Files workspace is not ready to share")?;
    let ticket =
        fs::Ticket::decode(ticket_text.trim()).context("local workspace ticket is invalid")?;
    let capability_json =
        std::fs::read_to_string(&acl).with_context(|| format!("read {}", acl.display()))?;
    let capabilities = serde_json::from_str(&capability_json)
        .context("filesystem capability export is invalid")?;
    let authority = fs::workspace_authority::WorkspaceAuthorityEnvelope::sign(
        ticket.workspace_id(),
        1,
        capabilities,
        false,
        &root,
    )?;
    let home_peer = fs::full_replica::HomePeerSet {
        status_file: PathBuf::from("/var/lib/qos/full-replica/status.json"),
        workspaces: vec![fs::full_replica::HomePeerBundle {
            ticket: ticket.encode()?,
            cache_dir: PathBuf::from("/var/lib/qos/full-replica")
                .join(ticket.workspace_id().to_string()),
            capability_json,
            authority: Some(authority),
            principals: Vec::new(),
            relay_workspace_ids: {
                relay_workspace_ids.sort_unstable();
                relay_workspace_ids.dedup();
                relay_workspace_ids.clone()
            },
        }],
    };
    let signed_response = sign_response(
        ResponsePayload {
            nonce: token.nonce,
            recipient_device_public_key: request.device_public_key,
            relay_service_node_id: request.relay_service_node_id,
            cert,
            home_peer_toml: toml::to_string_pretty(&home_peer)?,
        },
        &root,
    )?;
    let response = encrypt_response(
        &signed_response,
        token.nonce,
        request.device_public_key,
        request_ephemeral_public_key,
    )?;
    socket.set_read_timeout(Some(Duration::from_secs(1)))?;
    let ack_deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut acked = false;
    while std::time::Instant::now() < ack_deadline {
        socket.send_to(&response, sender)?;
        match socket.recv_from(&mut packet) {
            Ok((len, ack_sender)) if ack_sender == sender => {
                if verify_ack(
                    &packet[..len],
                    &response,
                    token.nonce,
                    request.device_public_key,
                )
                .is_ok()
                {
                    acked = true;
                    break;
                }
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    if !acked {
        bail!("home server did not acknowledge its signed configuration");
    }
    register_home_peer(
        &identity_root,
        &signed_response.payload.cert,
        signed_response.payload.relay_service_node_id,
        &relay_workspace_ids,
    )?;
    consume_nonce(&identity_root, &token.nonce, request.device_public_key)?;
    let finalized = make_finalized(&response, token.nonce, request.device_public_key, &root)?;
    for _ in 0..8 {
        socket.send_to(&finalized, sender)?;
        std::thread::sleep(Duration::from_millis(125));
    }
    println!(
        "approved nearby home server {}",
        hex::encode(signed_response.payload.cert.device_public_key)
    );
    Ok(())
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Discover { timeout } => discover(timeout),
        Command::Join {
            token_file,
            identity_root,
            label,
            timeout,
            home_peer_output,
        } => join(token_file, identity_root, label, timeout, home_peer_output),
        Command::Pair {
            identity_root,
            label,
            timeout,
            home_peer_output,
        } => pair(identity_root, label, timeout, home_peer_output),
        Command::Approve {
            identity_root,
            timeout,
            workspace_cache,
            acl,
            relay_workspace_ids,
            expected_device,
            expected_service,
        } => approve(
            identity_root,
            timeout,
            workspace_cache,
            acl,
            relay_workspace_ids,
            expected_device,
            expected_service,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qos_user_identity::DeviceCapabilities;

    fn payload(root: &ed25519_dalek::SigningKey) -> ResponsePayload {
        let user_public_key = root.verifying_key().to_bytes();
        let mut cert = DeviceCert {
            user_public_key,
            device_public_key: [7; 32],
            device_label: "NAS".into(),
            capabilities: DeviceCapabilities::host(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        ResponsePayload {
            nonce: [3; 16],
            recipient_device_public_key: [7; 32],
            relay_service_node_id: [8; 32],
            cert,
            home_peer_toml: "status_file = '/var/lib/qos/full-replica/status.json'".into(),
        }
    }

    #[test]
    fn signed_response_rejects_bundle_tampering() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let mut response = sign_response(payload(&root), &root).unwrap();
        verify_response(&response, root.verifying_key().to_bytes()).unwrap();
        response.payload.home_peer_toml.push_str("\n# changed");
        assert!(verify_response(&response, root.verifying_key().to_bytes()).is_err());
    }

    #[test]
    fn provisioning_bundle_is_encrypted_for_the_recipient_device() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let mut request_seed = [0_u8; 32];
        request_seed[0] = 11;
        let request_ephemeral = x25519_dalek::StaticSecret::from(request_seed);
        let request_public = x25519_dalek::PublicKey::from(&request_ephemeral).to_bytes();
        let mut payload = payload(&root);
        payload.home_peer_toml = "ticket = 'secret-ticket-material'".into();
        let signed = sign_response(payload, &root).unwrap();
        let encrypted = encrypt_response(
            &signed,
            [3; 16],
            device.verifying_key().to_bytes(),
            request_public,
        )
        .unwrap();
        assert!(
            !encrypted
                .windows(b"secret-ticket-material".len())
                .any(|window| window == b"secret-ticket-material")
        );
        let decrypted = decrypt_response(
            &encrypted,
            [3; 16],
            device.verifying_key().to_bytes(),
            &request_ephemeral,
        )
        .unwrap();
        verify_response(&decrypted, root.verifying_key().to_bytes()).unwrap();
        let wrong = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
        assert!(
            decrypt_response(
                &encrypted,
                [3; 16],
                wrong.verifying_key().to_bytes(),
                &request_ephemeral,
            )
            .is_err()
        );
    }

    #[test]
    fn bearer_token_is_encrypted_for_the_issuing_device() {
        let issuer = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
        let request = Request {
            token: "reusable-secret-token".into(),
            device_public_key: [7; 32],
            relay_service_node_id: [8; 32],
            label: "NAS".into(),
        };
        let (encrypted, _) = encrypt_request(&request, issuer.verifying_key().to_bytes()).unwrap();
        assert!(
            !encrypted
                .windows(b"reusable-secret-token".len())
                .any(|window| window == b"reusable-secret-token")
        );
        let (decrypted, _) = decrypt_request(&encrypted, &issuer).unwrap();
        assert_eq!(decrypted.token, request.token);
        let wrong = ed25519_dalek::SigningKey::from_bytes(&[5; 32]);
        assert!(decrypt_request(&encrypted, &wrong).is_err());
    }

    #[test]
    fn automatic_pair_offer_is_private_to_the_soliciting_server() {
        let server = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let token = "signed-host-enrollment-token";
        let encrypted = encrypt_pair_offer(token, server.verifying_key().to_bytes()).unwrap();
        assert!(
            !encrypted
                .windows(token.len())
                .any(|window| window == token.as_bytes())
        );
        assert_eq!(decrypt_pair_offer(&encrypted, &server).unwrap(), token);
        let other = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
        assert!(decrypt_pair_offer(&encrypted, &other).is_err());
    }

    #[test]
    fn nearby_pairing_code_is_stable_and_six_digits() {
        let code = pairing_code([7; 32], [8; 32]);
        assert_eq!(code, pairing_code([7; 32], [8; 32]));
        assert_eq!(code.len(), 6);
        assert!(code.bytes().all(|byte| byte.is_ascii_digit()));
        assert_ne!(code, pairing_code([7; 32], [9; 32]));
    }

    #[test]
    fn unrelated_lan_datagram_is_not_a_pairing_message() {
        let issuer = ed25519_dalek::SigningKey::from_bytes(&[6; 32]);
        assert!(decrypt_request(b"ambient discovery traffic", &issuer).is_err());
        let mut request_seed = [0_u8; 32];
        request_seed[0] = 11;
        let request_ephemeral = x25519_dalek::StaticSecret::from(request_seed);
        assert!(
            decrypt_response(
                b"not an enrollment response",
                [3; 16],
                [7; 32],
                &request_ephemeral,
            )
            .is_err()
        );
    }

    #[test]
    fn enrollment_nonce_is_consumed_once() {
        let dir = tempfile::tempdir().unwrap();
        consume_nonce(dir.path(), &[4; 16], [7; 32]).unwrap();
        consume_nonce(dir.path(), &[4; 16], [7; 32]).unwrap();
        assert!(consume_nonce(dir.path(), &[4; 16], [8; 32]).is_err());
    }

    #[test]
    fn approved_home_peer_registration_exposes_distinct_stable_node_ids() {
        let dir = tempfile::tempdir().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let cert = payload(&root).cert;
        register_home_peer(dir.path(), &cert, [8; 32], &[2, 9]).unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("home-peers.json")).unwrap())
                .unwrap();
        assert_eq!(
            json[0]["device_node_id"],
            hex::encode(cert.device_public_key)
        );
        assert_eq!(json[0]["relay_service_node_id"], hex::encode([8; 32]));
        assert_eq!(json[0]["relay_workspace_ids"], serde_json::json!([2, 9]));
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(dir.path().join("home-peers.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
    }

    #[test]
    fn invalid_existing_home_peer_registry_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("home-peers.json");
        atomic_file::write(&path, b"not json").unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        assert!(register_home_peer(dir.path(), &payload(&root).cert, [8; 32], &[2]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"not json");
    }

    #[test]
    fn dropped_response_can_be_retried_with_the_same_authenticated_ack() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let response = postcard::to_stdvec(&sign_response(payload(&root), &root).unwrap()).unwrap();
        // The first datagram is modeled as dropped. Retransmission uses the
        // exact same signed bytes, and the recipient ACK binds their key,
        // nonce, and that response digest.
        let retransmitted = response.clone();
        assert_eq!(response, retransmitted);
        let ack = make_ack(&retransmitted, [3; 16], &device).unwrap();
        verify_ack(&ack, &response, [3; 16], device.verifying_key().to_bytes()).unwrap();
    }

    #[test]
    fn dropped_all_acks_can_resume_after_process_restart() {
        let dir = tempfile::tempdir().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let response = postcard::to_stdvec(&sign_response(payload(&root), &root).unwrap()).unwrap();

        // No ACK reached the first approver, so it did not consume the
        // admission nonce. A restarted join/approve exchange may authenticate
        // the same device and finish registration idempotently.
        assert!(!dir.path().join("enrollment-nonces").exists());
        let ack = make_ack(&response, [3; 16], &device).unwrap();
        verify_ack(&ack, &response, [3; 16], device.verifying_key().to_bytes()).unwrap();
        consume_nonce(dir.path(), &[3; 16], device.verifying_key().to_bytes()).unwrap();
        let finalized =
            make_finalized(&response, [3; 16], device.verifying_key().to_bytes(), &root).unwrap();
        verify_finalized(
            &finalized,
            &response,
            [3; 16],
            device.verifying_key().to_bytes(),
            root.verifying_key().to_bytes(),
        )
        .unwrap();
        consume_nonce(dir.path(), &[3; 16], device.verifying_key().to_bytes()).unwrap();
        assert!(consume_nonce(dir.path(), &[3; 16], [6; 32]).is_err());
    }
}
