//! The device side: serve the route, admit administrators, bridge each
//! admitted session to a fresh `tilo-vmd rpc`, and log every session.
//!
//! The service reads the device's identity again for every session, so a
//! device enrolled or a revocation learned after it started counts at once.
//! Every refused and every admitted session, and each request an admitted
//! one makes, is a line in the service's journal.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::Command;

use crate::handshake::{self, Answer, Binding, Challenge, Hello, Proof, Refusal, Role, Verdict};
use crate::identity::{self, Identity};
use crate::net::{self, Relays};

/// How long a dialer may take for each handshake step.
const STEP: Duration = Duration::from_secs(20);
/// A request line longer than this ends the session (a layer upload chunk
/// is a fraction of it).
const MAX_LINE: usize = 64 << 20;
/// How often the service records its endpoint for `tilo-device-admin id`.
const STATE_EVERY: Duration = Duration::from_secs(30);

/// What `serve` needs.
pub struct Options {
    pub identity_root: PathBuf,
    pub relays: Relays,
    /// The daemon each admitted session talks to, as argv.
    pub daemon: Vec<String>,
    /// Where to record the endpoint's id and addresses; `None` records none.
    pub state_file: Option<PathBuf>,
}

struct Shared {
    identity_root: PathBuf,
    daemon: Vec<String>,
    endpoint_id: [u8; 32],
    next_session: AtomicU64,
}

#[derive(Clone)]
struct AdminProtocol(Arc<Shared>);

impl std::fmt::Debug for AdminProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AdminProtocol")
    }
}

impl ProtocolHandler for AdminProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let shared = Arc::clone(&self.0);
        tokio::spawn(async move { session(&shared, connection).await });
        Ok(())
    }
}

/// A running service: its endpoint and router.
pub struct Service {
    pub router: Router,
}

impl Service {
    /// The endpoint the route is served on.
    pub fn endpoint(&self) -> &iroh::Endpoint {
        self.router.endpoint()
    }

    /// Stop accepting and close the endpoint.
    pub async fn shutdown(self) {
        let _ = self.router.shutdown().await;
    }
}

/// Bind the device's admin endpoint and start accepting the route.
///
/// # Errors
/// No readable device key, or an endpoint that cannot bind.
pub async fn start(options: &Options) -> anyhow::Result<Service> {
    if options.daemon.is_empty() {
        anyhow::bail!("no daemon command to bridge sessions to");
    }
    let identity = Identity::load(&options.identity_root)?;
    let secret = iroh::SecretKey::from_bytes(&identity::transport_secret(&identity.key));
    let endpoint = net::bind(secret, &options.relays, Vec::new()).await?;
    let shared = Arc::new(Shared {
        identity_root: options.identity_root.clone(),
        daemon: options.daemon.clone(),
        endpoint_id: *endpoint.id().as_bytes(),
        next_session: AtomicU64::new(1),
    });
    eprintln!(
        "serving {} as endpoint {} for device {} ({}){}",
        String::from_utf8_lossy(crate::ALPN),
        endpoint.id(),
        hex::encode(identity.device_id()),
        options
            .relays
            .own()
            .map_or_else(|| "public relays".to_owned(), |url| format!("relay {url}")),
        if identity.cert.is_none() {
            "; it belongs to no identity yet, so every session is refused until it does"
        } else {
            ""
        }
    );
    let router = Router::builder(endpoint)
        .accept(crate::ALPN, AdminProtocol(shared))
        .spawn();
    Ok(Service { router })
}

/// Serve until SIGTERM or SIGINT.
///
/// # Errors
/// As [`start`].
pub async fn run(options: Options) -> anyhow::Result<()> {
    let service = start(&options).await?;
    if let Some(path) = options.state_file.clone() {
        let endpoint = service.endpoint().clone();
        let relays = options.relays.clone();
        tokio::spawn(async move {
            loop {
                if let Err(error) = record_state(&path, &endpoint, &relays) {
                    eprintln!(
                        "could not record the endpoint in {}: {error}",
                        path.display()
                    );
                }
                tokio::time::sleep(STATE_EVERY).await;
            }
        });
    }
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("listen for SIGTERM")?;
    tokio::select! {
        _ = terminate.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    service.shutdown().await;
    Ok(())
}

/// What the running service records about its endpoint.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct State {
    pub endpoint: String,
    pub addr: iroh::EndpointAddr,
    /// The relay it was configured with, if not the public ones.
    pub relay: Option<String>,
}

fn record_state(
    path: &std::path::Path,
    endpoint: &iroh::Endpoint,
    relays: &Relays,
) -> anyhow::Result<()> {
    let state = State {
        endpoint: endpoint.id().to_string(),
        addr: endpoint.addr(),
        relay: relays.own().map(ToString::to_string),
    };
    let bytes = serde_json::to_vec_pretty(&state)?;
    let partial = path.with_extension("json.partial");
    std::fs::write(&partial, bytes)?;
    std::fs::rename(&partial, path)?;
    Ok(())
}

fn who(cert: &tilo_user_identity::DeviceCert) -> String {
    format!(
        "{:?} (device {})",
        cert.device_label,
        hex::encode(cert.device_public_key)
    )
}

async fn refuse(send: &mut SendStream, frame: impl serde::Serialize + Send + Sync) {
    let _ = handshake::write_frame(send, &frame).await;
    let _ = send.finish();
    // Dropping the connection before the dialer has the answer would lose it.
    let _ = tokio::time::timeout(STEP, send.stopped()).await;
}

async fn step<T>(future: impl Future<Output = std::io::Result<T>>) -> std::io::Result<T> {
    tokio::time::timeout(STEP, future)
        .await
        .unwrap_or_else(|_| Err(std::io::Error::other("the dialer stopped answering")))
}

async fn session(shared: &Shared, connection: Connection) {
    let peer = connection.remote_id();
    let Ok(Ok((mut send, mut recv))) = tokio::time::timeout(STEP, connection.accept_bi()).await
    else {
        eprintln!("admin route: endpoint {peer} opened no stream");
        return;
    };
    let hello: Hello = match step(handshake::read_frame(&mut recv)).await {
        Ok(hello) => hello,
        Err(error) => {
            eprintln!("admin route: endpoint {peer} sent no greeting: {error}");
            return;
        }
    };
    if !admitted(shared, peer, &mut send, &mut recv, &hello).await {
        return;
    }
    let id = shared.next_session.fetch_add(1, Ordering::Relaxed);
    let Some((program, args)) = shared.daemon.split_first() else {
        return;
    };
    let child = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let child = match child {
        Ok(child) => child,
        Err(error) => {
            eprintln!(
                "admin session {id} from {}: could not start {}: {error}",
                who(&hello.cert),
                shared.daemon.join(" ")
            );
            refuse(&mut send, Verdict::Refused(Refusal::DaemonUnavailable)).await;
            return;
        }
    };
    if step(handshake::write_frame(
        &mut send,
        &Verdict::Admitted { session: id },
    ))
    .await
    .is_err()
    {
        return;
    }
    let route = net::route(&connection);
    eprintln!(
        "admin session {id} opened by {} at endpoint {peer} over {route}",
        who(&hello.cert)
    );
    let started = Instant::now();
    let (requests, status) = bridge(id, child, send, recv).await;
    eprintln!(
        "admin session {id} closed after {}s and {requests} requests ({})",
        started.elapsed().as_secs(),
        status
    );
}

/// Run the handshake after the dialer's greeting; `true` once the dialer
/// is admitted. Every refusal is logged and answered.
async fn admitted(
    shared: &Shared,
    peer: iroh::EndpointId,
    send: &mut SendStream,
    recv: &mut RecvStream,
    hello: &Hello,
) -> bool {
    let refused = |reason: Refusal| {
        eprintln!(
            "refused an admin session from {} at endpoint {peer}: {reason:?}: {}",
            who(&hello.cert),
            reason.explain()
        );
    };
    if hello.version != handshake::VERSION {
        refused(Refusal::UnsupportedVersion);
        refuse(send, Answer::Refused(Refusal::UnsupportedVersion)).await;
        return false;
    }
    let identity = match Identity::load(&shared.identity_root) {
        Ok(identity) => identity,
        Err(error) => {
            eprintln!("admin route: cannot read this device's identity: {error:#}");
            refuse(send, Answer::Refused(Refusal::DeviceNotEnrolled)).await;
            return false;
        }
    };
    let Some(own) = identity.cert.clone() else {
        refused(Refusal::DeviceNotEnrolled);
        refuse(send, Answer::Refused(Refusal::DeviceNotEnrolled)).await;
        return false;
    };
    // A damaged revocation list refuses everyone rather than nobody.
    let revocations = match identity.revocations() {
        Ok(revocations) => revocations,
        Err(error) => {
            eprintln!("admin route: cannot read this device's revocations: {error:#}");
            refused(Refusal::DeviceNotEnrolled);
            refuse(send, Answer::Refused(Refusal::DeviceNotEnrolled)).await;
            return false;
        }
    };
    if let Err(reason) = handshake::admit(
        &hello.cert,
        &own.user_public_key,
        &revocations,
        tilo_clock::unix_now_ns(),
    ) {
        refused(reason);
        refuse(send, Answer::Refused(reason)).await;
        return false;
    }
    let binding = Binding {
        dialer: *peer.as_bytes(),
        device: shared.endpoint_id,
    };
    let device_nonce = match handshake::nonce() {
        Ok(nonce) => nonce,
        Err(error) => {
            eprintln!("admin route: no randomness for a challenge: {error}");
            return false;
        }
    };
    let challenge = Answer::Challenge(Box::new(Challenge {
        cert: own,
        nonce: device_nonce,
        proof: handshake::prove(&identity.key, Role::Device, &hello.nonce, &binding),
    }));
    if step(handshake::write_frame(send, &challenge))
        .await
        .is_err()
    {
        return false;
    }
    let proof: Proof = match step(handshake::read_frame(recv)).await {
        Ok(proof) => proof,
        Err(_) => return false,
    };
    if !handshake::proof_holds(
        &hello.cert.device_public_key,
        &proof.proof,
        Role::Administrator,
        &device_nonce,
        &binding,
    ) {
        refused(Refusal::BadProof);
        refuse(send, Verdict::Refused(Refusal::BadProof)).await;
        return false;
    }
    true
}

/// The `"method"` of a request line, read from its first bytes (tilo-vm
/// writes the envelope's `method` before its `params`).
pub fn method(line: &[u8]) -> Option<&str> {
    let head = line.get(..line.len().min(256))?;
    let key = b"\"method\":\"";
    let at = head.windows(key.len()).position(|window| window == key)?;
    let rest = head.get(at.checked_add(key.len())?..)?;
    let name = rest.get(..rest.iter().position(|&byte| byte == b'"')?)?;
    std::str::from_utf8(name).ok()
}

fn audited_method(method: &str) -> &'static str {
    match method {
        "device.actions" => "device.actions",
        "device.unlock.enrol" => "device.unlock.enrol",
        "hello" => "hello",
        "status" => "status",
        "slot.create" => "slot.create",
        "slot.list" => "slot.list",
        "slot.restart" => "slot.restart",
        "slot.rm" => "slot.rm",
        "slot.show" => "slot.show",
        "layer.upload.begin" => "layer.upload.begin",
        "layer.upload.chunk" => "layer.upload.chunk",
        "layer.import" => "layer.import",
        "layer.list" => "layer.list",
        "swap" => "swap",
        "rollback" => "rollback",
        "logs" => "logs",
        "seat.claim" => "seat.claim",
        "seat.release" => "seat.release",
        "shot" => "shot",
        "control" => "control",
        "gc" => "gc",
        "input" => "input",
        "app.install" => "app.install",
        "boot.confirm" => "boot.confirm",
        "awake" => "awake",
        _ => "unsupported request",
    }
}

async fn bridge(
    id: u64,
    mut child: tokio::process::Child,
    mut send: SendStream,
    recv: RecvStream,
) -> (u64, String) {
    let (Some(mut stdin), Some(mut stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return (0, "the daemon has no pipes".into());
    };
    let requests = async move {
        let mut reader = BufReader::new(recv);
        let mut count = 0_u64;
        let mut line = zeroize::Zeroizing::new(Vec::new());
        loop {
            zeroize::Zeroize::zeroize(&mut *line);
            match (&mut reader)
                .take(MAX_LINE as u64)
                .read_until(b'\n', &mut line)
                .await
            {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if !line.ends_with(b"\n") && line.len() >= MAX_LINE {
                eprintln!("admin session {id}: a request line exceeds {MAX_LINE} bytes");
                break;
            }
            count = count.saturating_add(1);
            match method(&line) {
                // An upload is hundreds of chunks; its begin and import say enough.
                Some("layer.upload.chunk") => {}
                Some(name) => eprintln!("admin session {id}: {}", audited_method(name)),
                None => eprintln!("admin session {id}: a request without a method"),
            }
            if stdin.write_all(&line).await.is_err() || stdin.flush().await.is_err() {
                break;
            }
        }
        // Closing stdin ends the daemon's loop.
        drop(stdin);
        count
    };
    let responses = async move {
        let _ = tokio::io::copy(&mut stdout, &mut send).await;
        let _ = send.finish();
        let _ = send.stopped().await;
    };
    let messages = async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            eprintln!("admin session {id}: tilo-vmd: {line}");
        }
    };
    let (count, (), ()) = tokio::join!(requests, responses, messages);
    let status = match child.wait().await {
        Ok(status) if status.success() => "tilo-vmd exited".to_owned(),
        Ok(status) => format!("tilo-vmd {status}"),
        Err(error) => format!("tilo-vmd: {error}"),
    };
    (count, status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_method_is_read_from_the_head_of_a_request() {
        assert_eq!(
            method(br#"{"jsonrpc":"2.0","id":3,"method":"slot.create","params":{}}"#),
            Some("slot.create")
        );
        assert_eq!(method(br#"{"jsonrpc":"2.0","id":3}"#), None);
        let mut long = br#"{"jsonrpc":"2.0","id":1,"#.to_vec();
        long.extend(std::iter::repeat_n(b' ', 300));
        long.extend(br#""method":"status"}"#);
        assert_eq!(method(&long), None);
    }
}
