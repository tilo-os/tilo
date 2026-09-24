#![cfg(feature = "media")]

use std::io::Cursor;
use std::net::Ipv4Addr;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use anyhow::{Context, Result};
use qos_moq::relay::{Denied, Direction, Relay, RelayAuthority, SessionAuthorization, VOICE_PATH};
use rcgen::{CertifiedKey, KeyPair};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use web_transport_quinn::proto::{
    ConnectError, ConnectRequest, ConnectResponse, Frame, VarInt, http::StatusCode,
};

const MOQ_PROTOCOL: &str = "moqt-21";
const MOQ_VERSION: &str = "moq-transport-21";
const DEADLINE: Duration = Duration::from_secs(3);

struct H3Pair {
    server: web_transport_quinn::Server,
    client: web_transport_quinn::Client,
    url: url::Url,
    authority: String,
}

fn h3_pair() -> Result<H3Pair> {
    let CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])?;
    let chain = vec![cert.der().clone()];
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KeyPair::serialize_der(
        &signing_key,
    )));
    let server = web_transport_quinn::ServerBuilder::new()
        .with_addr((Ipv4Addr::LOCALHOST, 0).into())
        .with_certificate(chain.clone(), key)?;
    let port = server.local_addr()?.port();
    let authority = format!("127.0.0.1:{port}");
    let url = format!("https://{authority}{VOICE_PATH}").parse()?;
    let client = web_transport_quinn::ClientBuilder::new().with_server_certificates(chain)?;
    Ok(H3Pair {
        server,
        client,
        url,
        authority,
    })
}

fn request(url: url::Url, authority: &str, target: &str) -> ConnectRequest {
    ConnectRequest::new(url)
        .with_protocol(MOQ_PROTOCOL)
        .with_raw_authority(authority)
        .with_raw_target(target)
}

fn encode_qpack_prefix(buf: &mut Vec<u8>, size: u8, flags: u8, value: usize) {
    let mask = !(0xff << size) as u8;
    let flags = ((flags as usize) << size) as u8;
    if value < mask as usize {
        buf.push(flags | value as u8);
        return;
    }

    buf.push(mask | flags);
    let mut remaining = value - mask as usize;
    while remaining >= 128 {
        buf.push((remaining % 128) as u8 + 128);
        remaining /= 128;
    }
    buf.push(remaining as u8);
}

fn encode_qpack_literal(buf: &mut Vec<u8>, name: &str, value: &str) {
    encode_qpack_prefix(buf, 3, 0b00100, name.len());
    buf.extend_from_slice(name.as_bytes());
    encode_qpack_prefix(buf, 7, 0, value.len());
    buf.extend_from_slice(value.as_bytes());
}

fn connect_wire(extra: &[(&str, &str)]) -> Vec<u8> {
    let mut qpack = vec![0, 0];
    for (name, value) in [
        (":method", "CONNECT"),
        (":scheme", "https"),
        (":authority", "media.example.test:443"),
        (":path", VOICE_PATH),
        (":protocol", "webtransport"),
    ]
    .into_iter()
    .chain(extra.iter().copied())
    {
        encode_qpack_literal(&mut qpack, name, value);
    }

    let mut wire = Vec::new();
    Frame::HEADERS.encode(&mut wire);
    VarInt::from_u32(qpack.len() as u32).encode(&mut wire);
    wire.extend_from_slice(&qpack);
    wire
}

#[test]
fn activated_proto_rejects_duplicate_pseudoheaders_from_qpack_wire() -> Result<()> {
    for (name, value) in [
        (":path", VOICE_PATH),
        (":path", "/peer/v3/connect"),
        (":authority", "media.example.test:443"),
        (":authority", "other.example.test:443"),
    ] {
        let error = ConnectRequest::decode(&mut connect_wire(&[(name, value)]).as_slice())
            .expect_err("duplicate pseudo-header was accepted");
        assert!(
            matches!(error, ConnectError::QpackError(_)),
            "duplicate {name} returned {error:?}"
        );
    }

    let decoded = ConnectRequest::decode(
        &mut connect_wire(&[("x-test", "first"), ("x-test", "second")]).as_slice(),
    )?;
    assert_eq!(decoded.headers.get("x-test").unwrap(), "second");
    Ok(())
}

#[tokio::test]
async fn activated_proto_preserves_raw_connect_fields_before_url_normalization() -> Result<()> {
    for (authority, target) in [
        ("media.example.test:443", VOICE_PATH),
        ("media.example.test:443", "/moq/v21/./voice"),
        ("media.example.test:443", "/moq/v21/%2e/voice"),
        ("media.example.test:443", "/moq/other/../v21/voice"),
        ("media.example.test:443", "/moq/v21/voice?alias=1"),
        ("media.example.test:443", "/moq/v21/voice#fragment"),
        ("MEDIA.example.test:443", VOICE_PATH),
    ] {
        let outgoing = request(
            "https://media.example.test/moq/v21/voice".parse()?,
            authority,
            target,
        );
        let mut wire = Vec::new();
        outgoing.encode(&mut wire)?;
        let decoded = ConnectRequest::read(&mut Cursor::new(wire)).await?;
        eprintln!(
            "decoded raw CONNECT authority={:?} target={:?} normalized={}",
            decoded.raw_authority(),
            decoded.raw_target(),
            decoded.url
        );
        assert_eq!(decoded.raw_authority(), Some(authority));
        assert_eq!(decoded.raw_target(), Some(target));
    }
    Ok(())
}

#[tokio::test]
async fn actual_h3_rejects_raw_aliases_before_moq_setup_parsing() -> Result<()> {
    enum Malformed {
        Target(&'static str),
        AuthorityPrefix(&'static str),
        LocalhostAuthority,
    }
    let cases = [
        Malformed::Target("/moq/v21/voice/"),
        Malformed::Target("/moq/v21/./voice"),
        Malformed::Target("/moq/v21/%2e/voice"),
        Malformed::Target("/moq/other/../v21/voice"),
        Malformed::Target("/moq/v21/voice?alias=1"),
        Malformed::Target("/moq/v21/voice#fragment"),
        Malformed::Target("/peer/v3/connect"),
        Malformed::AuthorityPrefix("user@"),
        Malformed::LocalhostAuthority,
    ];

    for malformed in cases {
        let H3Pair {
            mut server,
            client,
            url,
            authority: expected_authority,
        } = h3_pair()?;
        let (raw_authority, raw_target) = match malformed {
            Malformed::Target(target) => (expected_authority.clone(), target.to_owned()),
            Malformed::AuthorityPrefix(prefix) => (
                format!("{prefix}{expected_authority}"),
                VOICE_PATH.to_owned(),
            ),
            Malformed::LocalhostAuthority => (
                format!(
                    "localhost:{}",
                    expected_authority
                        .rsplit_once(':')
                        .expect("test authority includes a port")
                        .1
                ),
                VOICE_PATH.to_owned(),
            ),
        };
        let sent_authority = raw_authority.clone();
        let sent_target = raw_target.clone();
        let server_authority = expected_authority.clone();
        let server_task = tokio::spawn(async move {
            let incoming = tokio::time::timeout(DEADLINE, server.accept())
                .await
                .context("H3 accept deadline")?
                .context("H3 listener closed")?;
            let observed_authority = incoming.raw_authority().map(str::to_owned);
            let observed_target = incoming.raw_target().map(str::to_owned);
            eprintln!(
                "rejected raw CONNECT authority={observed_authority:?} target={observed_target:?} normalized={}",
                incoming.url
            );
            assert!(
                observed_authority
                    .as_deref()
                    .zip(observed_target.as_deref())
                    .and_then(|(authority, target)| {
                        Relay::validate_route(authority, &server_authority, target)
                    })
                    .is_none(),
                "an alias must not mint a route marker"
            );
            incoming.reject(StatusCode::NOT_FOUND).await?;
            anyhow::Ok((observed_authority, observed_target))
        });

        let result = client
            .connect(request(url, &raw_authority, &raw_target))
            .await;
        assert!(result.is_err(), "malformed raw CONNECT was accepted");
        let (observed_authority, observed_target) = server_task.await??;
        assert_eq!(observed_authority.as_deref(), Some(sent_authority.as_str()));
        assert_eq!(observed_target.as_deref(), Some(sent_target.as_str()));
        // The rejection branch has no MoQ server and therefore cannot consume a
        // SETUP stream. Reaching this assertion proves H3 routing completed first.
    }
    Ok(())
}

struct Permit;

impl SessionAuthorization for Permit {
    fn publication(&self) -> Option<&str> {
        Some("call/a")
    }

    fn subscriptions(&self) -> &[String] {
        &[]
    }

    fn current(&self, _direction: Direction, namespace: &str) -> bool {
        namespace == "call/a"
    }
}

struct Authority {
    admissions: AtomicUsize,
}

impl RelayAuthority for Authority {
    fn admit(&self, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        if proof != b"canonical-proof" {
            return Err(Denied);
        }
        self.admissions.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(Permit))
    }
}

fn draft21() -> moq_net::Version {
    MOQ_VERSION.parse().expect("known Draft 21 version")
}

#[tokio::test]
async fn canonical_raw_connect_reaches_generic_relay_and_cancellation_owns_driver() -> Result<()> {
    let H3Pair {
        mut server,
        client,
        url,
        authority: expected_authority,
    } = h3_pair()?;
    let authority = Arc::new(Authority {
        admissions: AtomicUsize::new(0),
    });
    let relay = Relay::new(authority.clone());
    let serving = relay.clone();
    let route_authority = expected_authority.clone();
    let server_task = tokio::spawn(async move {
        let incoming = tokio::time::timeout(DEADLINE, server.accept())
            .await
            .context("H3 accept deadline")?
            .context("H3 listener closed")?;
        eprintln!(
            "accepted raw CONNECT authority={:?} target={:?} normalized={}",
            incoming.raw_authority(),
            incoming.raw_target(),
            incoming.url
        );
        let route = incoming
            .raw_authority()
            .zip(incoming.raw_target())
            .and_then(|(authority, target)| {
                Relay::validate_route(authority, &route_authority, target)
            })
            .context("canonical raw route rejected")?;
        let transport = incoming
            .respond(ConnectResponse::OK.with_protocol(MOQ_PROTOCOL))
            .await?;
        let pending = moq_net::Server::new()
            .with_versions(draft21().into())
            .accept_request(transport)
            .await?;
        serving.serve_accepted(route, pending).await;
        anyhow::Ok(())
    });

    let transport = client
        .connect(request(url, &expected_authority, VOICE_PATH))
        .await?;
    let moq = moq_net::Client::new()
        .with_versions(draft21().into())
        .with_authorization(b"canonical-proof".to_vec())?;
    let (session, driver) = moq.connect(transport).await?;
    let client_driver = tokio::spawn(driver);

    tokio::time::timeout(DEADLINE, async {
        while relay.counters().active != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("generic relay never became active")?;
    assert_eq!(authority.admissions.load(Ordering::SeqCst), 1);
    assert_eq!(relay.counters().accepted, 1);

    // Cancelling the host-owned task drops both the session and its retained
    // protocol driver. The client must observe closure without a detached task.
    server_task.abort();
    assert!(server_task.await.is_err());
    tokio::time::timeout(DEADLINE, session.closed())
        .await
        .context("client stayed open after generic relay task cancellation")?;
    assert_eq!(relay.counters().active, 0);
    let _ = tokio::time::timeout(DEADLINE, client_driver).await;
    Ok(())
}

/// Product ingress order: CONNECT is answered before MoQ SETUP and host
/// admission, so an open transport is not an admitted session. The client's
/// setup-received signal (relay SETUP) resolves only after admission.
async fn setup_signal_after(proof: &'static [u8]) -> Result<(bool, bool)> {
    let H3Pair {
        mut server,
        client,
        url,
        authority: expected_authority,
    } = h3_pair()?;
    let relay = Relay::new(Arc::new(Authority {
        admissions: AtomicUsize::new(0),
    }));
    let route_authority = expected_authority.clone();
    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.context("H3 listener closed")?;
        let route = incoming
            .raw_authority()
            .zip(incoming.raw_target())
            .and_then(|(authority, target)| {
                Relay::validate_route(authority, &route_authority, target)
            })
            .context("canonical raw route rejected")?;
        let transport = incoming
            .respond(ConnectResponse::OK.with_protocol(MOQ_PROTOCOL))
            .await?;
        let pending = moq_net::Server::new()
            .with_versions(draft21().into())
            .accept_request(transport)
            .await?;
        relay.serve_accepted(route, pending).await;
        anyhow::Ok(())
    });
    let transport = client
        .connect(request(url, &expected_authority, VOICE_PATH))
        .await?;
    let moq = moq_net::Client::new()
        .with_versions(draft21().into())
        .with_authorization(proof.to_vec())?;
    let (session, driver) = moq.connect(transport).await?;
    let client_driver = tokio::spawn(driver);
    let outcome = tokio::time::timeout(DEADLINE, async {
        tokio::select! {
            biased;
            _ = session.setup_received() => (true, false),
            _ = session.closed() => (false, true),
        }
    })
    .await
    .context("neither admission nor closure observed")?;
    server_task.abort();
    client_driver.abort();
    Ok(outcome)
}

#[tokio::test]
async fn setup_received_resolves_only_after_relay_admission() -> Result<()> {
    assert_eq!(setup_signal_after(b"canonical-proof").await?, (true, false));
    assert_eq!(setup_signal_after(b"refused-proof").await?, (false, true));
    Ok(())
}
