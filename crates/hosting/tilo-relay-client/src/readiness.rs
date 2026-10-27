//! What a node publishes about itself once it serves, and the check that the
//! node still answers.
//!
//! A node — a machine running a person's rooms for their web pages
//! (docs/node-machine.md) — can survive an update with its units "active" and
//! answer nothing: a certificate it can no longer load, a listener that bound
//! but is wedged, a key the person's page no longer trusts. The relay being
//! active says none of that, and neither does the session beside it, so the
//! boot that decides whether an update is kept has to ask the node itself
//! (`tilo-node-readiness`, `os/usr/libexec/tilo-boot-health`).
//!
//! The relay publishes a [`Node`] when it starts serving: the boot it started
//! in, the address browsers come in at, and its own device key. The session
//! device key signs that record, so a check holding only the session's
//! *public* key can tell a record this node wrote in this boot from a file left
//! behind by one that is gone. Then it asks the address the record names for
//! the host behind it — the TLS-relay readiness handshake every served host
//! answers ([`host_info`]), which a listener that is up and broken does not —
//! and requires the host that answers to be the key the record names, with a
//! certificate that verifies now.

use std::time::Duration;
#[cfg(feature = "probe")]
use std::{net::SocketAddr, sync::Arc};

#[cfg(feature = "probe")]
use http_body_util::BodyExt as _;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use tilo_relay_protocol::web::HostInfo;
use tilo_user_identity::verify_device_cert_at;

#[cfg(feature = "probe")]
use crate::https::HttpFuture;
use crate::https::{self, HttpClient, LinkError};

/// The domain the session's signature over a record covers, so a signature
/// made here can never be read as a signature over anything else.
const RECORD_DOMAIN: &[u8] = b"tilo-node-readiness-v1\0";

/// The fields a relay has for its own record when it starts serving.
#[derive(Clone, Copy, Debug)]
pub struct Record<'a> {
    /// The boot it started in (`/proc/sys/kernel/random/boot_id`).
    pub boot_id: &'a str,
    /// The `host:port` browsers reach the relay at.
    pub browsers: &'a str,
    /// The name the certificate is for: what a page dials, and what a check
    /// presents in the handshake. `None` for one issued for the address
    /// itself.
    pub name: &'a str,
    /// The relay's device key, hex.
    pub host_key: &'a str,
    /// SHA-256 of its TLS certificate (lowercase hex), for one this machine
    /// minted (`--tls dev`).
    pub certificate_sha256: Option<&'a str>,
}

/// What a serving node publishes about itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// The boot this record was written in, as the kernel names it
    /// (`/proc/sys/kernel/random/boot_id`). A record from another boot says
    /// nothing about the node serving now.
    pub boot_id: String,
    /// The `host:port` the relay serves browsers on, as a page reaches it:
    /// what a check dials.
    pub browsers: String,
    /// The name the relay's certificate is for (its `--public-host`, or the
    /// address): what a page dials and a check presents in the handshake.
    pub name: String,
    /// This relay's device key, hex: the identity `/peer/v1/host` has to
    /// present.
    pub host_key: String,
    /// SHA-256 of the relay's TLS certificate (lowercase hex) when it is one
    /// this machine minted rather than one from a chain a system trust store
    /// knows. The check pins it, the way a page pins WebTransport's
    /// certificate, so judging a node needs no root installed on it.
    pub certificate_sha256: Option<String>,
    /// The session device key's signature over the fields above.
    pub session_signed: Vec<u8>,
}

impl Node {
    /// The record a node that has just started serving publishes, signed by
    /// its session device key.
    #[must_use]
    pub fn new(fields: Record<'_>, session: &SigningKey) -> Self {
        let Record {
            boot_id,
            browsers,
            name,
            host_key,
            certificate_sha256,
        } = fields;
        let signed = signed_bytes(&[
            boot_id,
            browsers,
            name,
            host_key,
            certificate_sha256.unwrap_or(""),
        ]);
        Self {
            boot_id: boot_id.to_owned(),
            browsers: browsers.to_owned(),
            name: name.to_owned(),
            host_key: host_key.to_owned(),
            certificate_sha256: certificate_sha256.map(str::to_owned),
            session_signed: session.sign(&signed).to_bytes().to_vec(),
        }
    }

    /// This record as the one line a node publishes.
    ///
    /// # Errors
    ///
    /// Returns the encoder's error; these are strings, so it never occurs.
    pub fn to_line(&self) -> serde_json::Result<String> {
        Ok(format!("{}\n", serde_json::to_string(self)?))
    }

    /// The record in `line`.
    ///
    /// # Errors
    ///
    /// The line is not a record: a half-written file from a node that stopped
    /// while publishing it.
    pub fn parse(line: &str) -> Result<Self, ReadinessError> {
        serde_json::from_str(line.trim())
            .map_err(|error| ReadinessError::Unreadable(error.to_string()))
    }

    /// Whether `session` is the key that signed this record.
    ///
    /// # Errors
    ///
    /// The record carries no signature that key made: another node's, a
    /// session reinstalled since, or a corrupted file.
    pub fn verify(&self, session: &VerifyingKey) -> Result<(), ReadinessError> {
        let signature: [u8; 64] = self.session_signed.as_slice().try_into().map_err(|_| {
            ReadinessError::NotThisNodes("the record's signature is not 64 bytes".to_owned())
        })?;
        session
            .verify_strict(
                &signed_bytes(&[
                    &self.boot_id,
                    &self.browsers,
                    &self.name,
                    &self.host_key,
                    self.certificate_sha256.as_deref().unwrap_or(""),
                ]),
                &Signature::from_bytes(&signature),
            )
            .map_err(|_| {
                ReadinessError::NotThisNodes(
                    "the record was not signed by this session's device key".to_owned(),
                )
            })
    }

    /// The address browsers reach the relay at.
    ///
    /// # Errors
    ///
    /// The record's `browsers` is not a `host:port`.
    pub fn address(&self) -> Result<SocketAddr, ReadinessError> {
        self.browsers.trim().parse().map_err(|error| {
            ReadinessError::Unreadable(format!("the record names no address: {error}"))
        })
    }

    /// The host key the record names, decoded.
    ///
    /// # Errors
    ///
    /// The record's `host_key` is not 32 bytes of hex.
    pub fn host(&self) -> Result<[u8; 32], ReadinessError> {
        let bytes = hex::decode(self.host_key.trim()).map_err(|error| {
            ReadinessError::Unreadable(format!("the record names no host key: {error}"))
        })?;
        let len = bytes.len();
        bytes.try_into().map_err(|_: Vec<u8>| {
            ReadinessError::Unreadable(format!("the record's host key is {len} bytes, not 32"))
        })
    }

    /// Ask the node this record describes what it is, having checked that the
    /// answer is the node the record names.
    ///
    /// # Errors
    ///
    /// [`ReadinessError::Unanswered`] when nothing answers the readiness
    /// handshake within `patience` — what a listener that is up but wedged
    /// looks like — [`ReadinessError::AnotherHost`] when a different host
    /// answers, and [`ReadinessError::Unreadable`] when the host presents a
    /// certificate that no longer verifies.
    pub async fn probe(
        &self,
        http: &dyn HttpClient,
        patience: Duration,
    ) -> Result<HostInfo, ReadinessError> {
        let host = self.host()?;
        let answered = tokio::time::timeout(patience, https::host_info(http))
            .await
            .map_err(|_| {
                ReadinessError::Unanswered(format!("no answer within {}s", patience.as_secs()))
            })?
            .map_err(|error| match error {
                LinkError::Unavailable(reason) => ReadinessError::Unanswered(reason),
                other => ReadinessError::Unreadable(other.to_string()),
            })?;
        if answered.host_cert.device_public_key != host {
            return Err(ReadinessError::AnotherHost(hex::encode(
                answered.host_cert.device_public_key,
            )));
        }
        verify_device_cert_at(&answered.host_cert, tilo_clock::unix_now_ns())
            .map_err(|error| ReadinessError::Unreadable(format!("host certificate: {error}")))?;
        Ok(answered)
    }
}

/// The bytes a session signs: the domain, then each field with its length, so
/// no two records share a signature.
fn signed_bytes(fields: &[&str]) -> Vec<u8> {
    let mut bytes = RECORD_DOMAIN.to_vec();
    for field in fields {
        bytes.extend_from_slice(&u64::try_from(field.len()).unwrap_or(u64::MAX).to_be_bytes());
        bytes.extend_from_slice(field.as_bytes());
    }
    bytes
}

/// Why a node's readiness could not be read or did not hold.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReadinessError {
    /// The published file is not a record.
    #[error("unreadable readiness: {0}")]
    Unreadable(String),
    /// The file is a record some other node wrote.
    #[error("not this node's readiness: {0}")]
    NotThisNodes(String),
    /// The node published nothing within the wait.
    #[error("the node published no readiness: {0}")]
    NotPublished(String),
    /// The node did not answer.
    #[error("the node does not answer: {0}")]
    Unanswered(String),
    /// Another host answers under the node's address.
    #[error("another host answers there: {0}")]
    AnotherHost(String),
}

/// HTTPS to the node a record names, trusting exactly the certificate it
/// published — the pin a page gives WebTransport — or the system's roots when
/// the relay serves one of them.
#[cfg(feature = "probe")]
pub struct NodeHttps {
    addr: SocketAddr,
    /// The name the certificate is for, as `--public-host` named it.
    name: rustls::pki_types::ServerName<'static>,
    connector: tokio_rustls::TlsConnector,
}

#[cfg(feature = "probe")]
impl NodeHttps {
    /// A client for `record`'s node.
    ///
    /// # Errors
    ///
    /// The record names no address, or its pin is not a SHA-256.
    pub fn trusting(record: &Node) -> Result<Self, ReadinessError> {
        let addr = record.address()?;
        let config = match &record.certificate_sha256 {
            Some(pin) => {
                let sha256 = hex::decode(pin.trim()).map_err(|error| {
                    ReadinessError::Unreadable(format!(
                        "the record's certificate pin is not hex: {error}"
                    ))
                })?;
                tilo_net::pinned_client_config(move |certificate| {
                    use sha2::Digest as _;
                    sha2::Sha256::digest(certificate).as_slice() == sha256.as_slice()
                })
                .map_err(|error| ReadinessError::Unreadable(error.to_string()))?
            }
            None => tilo_net::client_config()
                .map_err(|error| ReadinessError::Unreadable(error.to_string()))?,
        };
        // The certificate is issued for the name a page dials with, which the
        // record carries; a bare address is issued for the address.
        let name =
            match record.name.parse::<std::net::IpAddr>() {
                Ok(ip) => rustls::pki_types::ServerName::IpAddress(ip.into()),
                Err(_) => rustls::pki_types::ServerName::try_from(record.name.clone()).map_err(
                    |error| ReadinessError::Unreadable(format!("the record's name: {error}")),
                )?,
            };
        Ok(Self {
            addr,
            name,
            connector: tokio_rustls::TlsConnector::from(Arc::new(config)),
        })
    }

    async fn request(
        &self,
        path: &str,
        session: Option<&str>,
        body: Vec<u8>,
    ) -> Result<(u16, Vec<u8>), String> {
        let tcp = tokio::net::TcpStream::connect(self.addr)
            .await
            .map_err(|error| error.to_string())?;
        let tls = self
            .connector
            .clone()
            .connect(self.name.clone(), tcp)
            .await
            .map_err(|error| error.to_string())?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(tls))
                .await
                .map_err(|error| error.to_string())?;
        tokio::spawn(connection);
        let mut request = hyper::Request::post(path)
            .header(hyper::header::CONTENT_TYPE, "application/octet-stream");
        if let Some(session) = session {
            request = request.header(tilo_relay_protocol::web::SESSION_HEADER, session);
        }
        let request = request
            .body(http_body_util::Full::new(bytes::Bytes::from(body)))
            .map_err(|error| error.to_string())?;
        let response = sender
            .send_request(request)
            .await
            .map_err(|error| error.to_string())?;
        let status = response.status().as_u16();
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|error| error.to_string())?
            .to_bytes()
            .to_vec();
        Ok((status, body))
    }
}

#[cfg(feature = "probe")]
impl HttpClient for NodeHttps {
    fn post(&self, path: &str, session: Option<&str>, body: Vec<u8>) -> HttpFuture<'_> {
        let path = path.to_owned();
        let session = session.map(str::to_owned);
        Box::pin(async move { self.request(&path, session.as_deref(), body).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    const HOST: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    fn record<'a>(host_key: &'a str, certificate: Option<&'a str>) -> Record<'a> {
        Record {
            boot_id: "b1d2",
            browsers: "192.0.2.10:9443",
            name: "node.example",
            host_key,
            certificate_sha256: certificate,
        }
    }

    #[test]
    fn a_record_survives_the_line_a_node_publishes() {
        let published = Node::new(record(HOST, Some("aabb")), &session());
        let read = Node::parse(&published.to_line().expect("encode")).expect("parse");
        assert_eq!(read, published);
        assert_eq!(read.boot_id, "b1d2");
        assert_eq!(read.name, "node.example");
        assert_eq!(read.certificate_sha256.as_deref(), Some("aabb"));
        assert_eq!(
            read.address().expect("address").to_string(),
            "192.0.2.10:9443"
        );
        assert_eq!(
            read.host().expect("host"),
            [
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb,
                0xcc, 0xdd, 0xee, 0xff,
            ]
        );
    }

    #[test]
    fn the_sessions_key_says_whose_record_it_is() {
        let published = Node::new(record(HOST, None), &session());
        published
            .verify(&session().verifying_key())
            .expect("its own session");
        let other = SigningKey::from_bytes(&[9u8; 32]);
        assert!(matches!(
            published.verify(&other.verifying_key()),
            Err(ReadinessError::NotThisNodes(_))
        ));
    }

    // A node that survives an update and answers nothing is the update this
    // check exists to reject; a record that says one thing and a relay that
    // serves another has to fail, whichever field was swapped.
    #[test]
    fn nothing_about_a_record_can_be_changed_under_its_signature() {
        let published = Node::new(record(HOST, Some("aabb")), &session());
        let session_key = session().verifying_key();
        let mut changed: Vec<Node> = Vec::new();

        let mut other = published.clone();
        other.browsers = "192.0.2.11:9443".to_owned();
        changed.push(other);
        let mut other = published.clone();
        other.boot_id = "b1d3".to_owned();
        changed.push(other);
        let mut other = published.clone();
        other.name = "elsewhere.example".to_owned();
        changed.push(other);
        let mut other = published.clone();
        other.host_key = "ff".repeat(32);
        changed.push(other);
        let mut other = published.clone();
        other.certificate_sha256 = Some("ccdd".to_owned());
        changed.push(other);
        let mut other = published;
        other.certificate_sha256 = None;
        changed.push(other);

        for node in changed {
            assert!(
                matches!(
                    node.verify(&session_key),
                    Err(ReadinessError::NotThisNodes(_))
                ),
                "a record with {node:?} verified under its signature"
            );
        }
    }

    #[test]
    fn a_record_without_its_signature_is_not_one() {
        let mut published = Node::new(record(HOST, None), &session());
        published.session_signed.pop();
        assert!(matches!(
            published.verify(&session().verifying_key()),
            Err(ReadinessError::NotThisNodes(_))
        ));
    }

    #[test]
    fn a_half_written_record_is_unreadable_rather_than_a_node() {
        assert!(matches!(
            Node::parse("{\"boot_id\": \"b1d2\", \"brow"),
            Err(ReadinessError::Unreadable(_))
        ));
        assert!(matches!(
            Node::parse(""),
            Err(ReadinessError::Unreadable(_))
        ));
    }
}
