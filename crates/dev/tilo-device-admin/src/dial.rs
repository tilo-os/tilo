//! The dialer side: a stdio bridge to one device's tilo-vmd over the route.
//!
//! `tilo-vm` runs `tilo-device-admin dial` in place of `ssh <device> tilo-vmd
//! rpc`. Before the request lines, stdout carries one line of JSON that names
//! the device and the route ([`Preamble`]). When the connection drops between
//! requests (a link cut, a roam the path could not follow), the bridge dials
//! again by itself, repeats the session's `hello` and goes on; only a
//! connection lost in the middle of a request ends the session.

use std::io::BufRead as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayUrl};
use tokio::io::{AsyncBufReadExt as _, BufReader};

use crate::handshake::{self, Answer, Binding, Hello, Proof, Refusal, Role, Untrusted, Verdict};
use crate::identity::Identity;
use crate::net::{self, Relays, Route};

/// What `dial` needs.
pub struct Options {
    /// This machine's identity: an enrolled administrator.
    pub identity_root: PathBuf,
    /// The device's admin endpoint.
    pub endpoint: EndpointId,
    /// The device id the answer must prove; `None` accepts any device of
    /// the identity at that endpoint.
    pub device: Option<[u8; 32]>,
    pub relays: Relays,
    /// The relay the device was last seen on, when it uses the public ones.
    pub home_relay: Option<RelayUrl>,
    /// How long to keep dialing a device that does not answer.
    pub wait: Duration,
}

/// Why a dial failed; each maps to its own exit status, which `tilo-vm`
/// turns into its own advice.
#[derive(Debug)]
pub enum Failure {
    /// This machine has no key or no certificate (exit 5). One that does not
    /// grant administration is the device's to refuse.
    NotEnrolled(String),
    /// The device did not answer (exit 2).
    Unreachable(String),
    /// The device refused this machine (exit 3).
    Refused(Refusal),
    /// The answer did not come from the device asked for (exit 4).
    Untrusted(Untrusted),
    /// Anything else (exit 1).
    Other(anyhow::Error),
}

impl Failure {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Other(_) => 1,
            Self::Unreachable(_) => 2,
            Self::Refused(_) => 3,
            Self::Untrusted(_) => 4,
            Self::NotEnrolled(_) => 5,
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotEnrolled(detail) => write!(f, "this machine is not enrolled: {detail}"),
            Self::Unreachable(detail) => write!(f, "no answer over the peer network: {detail}"),
            Self::Refused(reason) => {
                write!(f, "the device refused: {reason:?}: {}", reason.explain())
            }
            Self::Untrusted(why) => write!(f, "refused the answer: {why}"),
            Self::Other(error) => write!(f, "{error:#}"),
        }
    }
}

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

/// The line before the request lines.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Preamble {
    /// Hex device id that answered.
    pub device: String,
    pub label: String,
    /// `relay` or `direct`.
    pub route: String,
    /// The relay's URL or the device's address.
    pub via: String,
    /// The session's number in the device's journal.
    pub session: u64,
}

/// The JSON object the preamble line wraps.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct PreambleLine {
    pub tilo_device_admin: Preamble,
}

/// An admitted session.
pub struct Session {
    pub connection: Connection,
    pub send: SendStream,
    pub recv: RecvStream,
    pub preamble: Preamble,
}

/// The dialer: its endpoint and identity, ready to open sessions.
pub struct Dialer {
    endpoint: Endpoint,
    identity: Identity,
    cert: tilo_user_identity::DeviceCert,
    addr: EndpointAddr,
    device: Option<[u8; 32]>,
}

impl Dialer {
    /// Load this machine's identity and bind its endpoint.
    ///
    /// # Errors
    /// [`Failure::NotEnrolled`] without a key or a certificate; [`Failure::Other`] when the endpoint cannot bind.
    pub async fn new(options: &Options) -> Result<Self, Failure> {
        let identity = Identity::load(&options.identity_root)
            .map_err(|error| Failure::NotEnrolled(format!("{error:#}")))?;
        let Some(cert) = identity.cert.clone() else {
            return Err(Failure::NotEnrolled(format!(
                "{} holds a device key but no certificate",
                options.identity_root.display()
            )));
        };
        // The dialer is a device of its own: its endpoint id is its device key.
        let secret = iroh::SecretKey::from_bytes(&identity.key.to_bytes());
        let endpoint = net::bind(secret, &options.relays, Vec::new()).await?;
        let addr = net::address(
            options.endpoint,
            &options.relays,
            options.home_relay.as_ref(),
        );
        Ok(Self {
            endpoint,
            identity,
            cert,
            addr,
            device: options.device,
        })
    }

    /// Dial the device for up to `wait` and complete the handshake.
    ///
    /// # Errors
    /// Unreachable, refused, or an untrustworthy answer.
    pub async fn open(&self, wait: Duration) -> Result<Session, Failure> {
        let connection = net::dial(&self.endpoint, &self.addr, wait)
            .await
            .map_err(|error| Failure::Unreachable(error.to_string()))?;
        let unreachable = |error: std::io::Error| Failure::Unreachable(error.to_string());
        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .map_err(|error| Failure::Unreachable(error.to_string()))?;
        let nonce = handshake::nonce().map_err(|error| Failure::Other(error.into()))?;
        handshake::write_frame(
            &mut send,
            &Hello {
                version: handshake::VERSION,
                cert: self.cert.clone(),
                nonce,
            },
        )
        .await
        .map_err(unreachable)?;
        let challenge = match handshake::read_frame(&mut recv)
            .await
            .map_err(unreachable)?
        {
            Answer::Challenge(challenge) => challenge,
            Answer::Refused(reason) => return Err(Failure::Refused(reason)),
        };
        let binding = Binding {
            dialer: *self.endpoint.id().as_bytes(),
            device: *connection.remote_id().as_bytes(),
        };
        handshake::trust_device(
            &challenge,
            &self.cert.user_public_key,
            self.device.as_ref(),
            &nonce,
            &binding,
            tilo_clock::unix_now_ns(),
        )
        .map_err(Failure::Untrusted)?;
        let proof = handshake::prove(
            &self.identity.key,
            Role::Administrator,
            &challenge.nonce,
            &binding,
        );
        handshake::write_frame(&mut send, &Proof { proof })
            .await
            .map_err(unreachable)?;
        let session = match handshake::read_frame(&mut recv)
            .await
            .map_err(unreachable)?
        {
            Verdict::Admitted { session } => session,
            Verdict::Refused(reason) => return Err(Failure::Refused(reason)),
        };
        let route = net::route(&connection);
        Ok(Session {
            preamble: Preamble {
                device: hex::encode(challenge.cert.device_public_key),
                label: challenge.cert.device_label,
                route: route.kind().into(),
                via: route.via().into(),
                session,
            },
            connection,
            send,
            recv,
        })
    }

    /// Close the endpoint, telling the device the session is over.
    pub async fn close(self) {
        self.endpoint.close().await;
    }
}

/// Write one whole line to stdout.
fn emit(stdout: &std::io::Stdout, bytes: &[u8]) -> Result<(), Failure> {
    let mut out = stdout.lock();
    out.write_all(bytes)
        .and_then(|()| out.flush())
        .map_err(|error| Failure::Other(error.into()))
}

fn is_hello(line: &[u8]) -> bool {
    crate::serve::method(line) == Some("hello")
}

/// Bridge this process's stdin and stdout to the device until stdin ends.
///
/// # Errors
/// As [`Dialer::new`] and [`Dialer::open`]; a connection lost during a
/// request is [`Failure::Unreachable`].
pub async fn run(options: Options) -> Result<(), Failure> {
    let dialer = Dialer::new(&options).await?;
    let session = dialer.open(options.wait).await?;
    let stdout = std::io::stdout();
    let preamble = serde_json::to_string(&PreambleLine {
        tilo_device_admin: session.preamble.clone(),
    })
    .map_err(|error| Failure::Other(error.into()))?;
    emit(&stdout, format!("{preamble}\n").as_bytes())?;

    // stdin is read on a thread of its own: tokio's stdin cannot be raced.
    let (lines_tx, mut lines) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
    std::thread::spawn(move || {
        let mut input = std::io::stdin().lock();
        loop {
            let mut line = Vec::new();
            match input.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if lines_tx.blocking_send(line).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let label = session.preamble.label.clone();
    let mut reader = BufReader::new(session.recv);
    let mut send = session.send;
    let mut connection = session.connection;
    let mut hello: Option<Vec<u8>> = None;
    let mut in_flight = 0_usize;
    let mut response = Vec::new();
    let mut input_open = true;
    loop {
        if !input_open && in_flight == 0 {
            break;
        }
        tokio::select! {
            line = lines.recv(), if input_open => {
                let Some(line) = line else {
                    input_open = false;
                    let _ = send.finish();
                    continue;
                };
                if hello.is_none() && is_hello(&line) {
                    hello = Some(line.clone());
                }
                if send.write_all(&line).await.is_err() {
                    // Nothing of it was answered: dial again and send it again.
                    let fresh = reconnect(&dialer, &label, hello.as_deref(), &options).await?;
                    (connection, send, reader) = fresh;
                    send.write_all(&line).await.map_err(|error| Failure::Unreachable(error.to_string()))?;
                }
                in_flight = in_flight.saturating_add(1);
            }
            read = reader.read_until(b'\n', &mut response) => {
                match read {
                    Ok(n) if n > 0 && response.ends_with(b"\n") => {
                        emit(&stdout, &response)?;
                        response.clear();
                        in_flight = in_flight.saturating_sub(1);
                    }
                    _ if in_flight == 0 && input_open => {
                        response.clear();
                        let fresh = reconnect(&dialer, &label, hello.as_deref(), &options).await?;
                        (connection, send, reader) = fresh;
                    }
                    _ if !input_open && in_flight == 0 => break,
                    Ok(_) => {
                        return Err(Failure::Unreachable(format!(
                            "{label} closed the session during a request"
                        )));
                    }
                    Err(error) => {
                        return Err(Failure::Unreachable(format!(
                            "the connection to {label} was lost during a request: {error}"
                        )));
                    }
                }
            }
        }
    }
    drop(connection);
    dialer.close().await;
    Ok(())
}

/// Dial again after a lost connection and repeat the session's `hello`.
async fn reconnect(
    dialer: &Dialer,
    label: &str,
    hello: Option<&[u8]>,
    options: &Options,
) -> Result<(Connection, SendStream, BufReader<RecvStream>), Failure> {
    eprintln!("tilo-device-admin: the connection to {label} was lost; dialing it again");
    let session = dialer.open(options.wait).await?;
    let mut send = session.send;
    let mut reader = BufReader::new(session.recv);
    if let Some(hello) = hello {
        send.write_all(hello)
            .await
            .map_err(|error| Failure::Unreachable(error.to_string()))?;
        let mut answer = Vec::new();
        reader
            .read_until(b'\n', &mut answer)
            .await
            .map_err(|error| Failure::Unreachable(error.to_string()))?;
    }
    eprintln!(
        "tilo-device-admin: reconnected to {label} over {} (session {})",
        Route::from_preamble(&session.preamble),
        session.preamble.session
    );
    Ok((session.connection, send, reader))
}

impl Route {
    fn from_preamble(preamble: &Preamble) -> Self {
        match preamble.route.as_str() {
            "relay" => Self::Relay(preamble.via.clone()),
            "direct" => Self::Direct(preamble.via.clone()),
            _ => Self::Unknown,
        }
    }
}
