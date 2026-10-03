//! Isolated loopback protocol proof. These fixture identities authorize no
//! workspace, document, microphone, native app, or production relay.

use std::io::Read as _;

use rustls::pki_types::pem::PemObject as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tilo_moq::relay::{Denied, Direction, Relay, RelayAuthority, SessionAuthorization, VOICE_PATH};

struct Options {
    bind: SocketAddr,
    cert: PathBuf,
    key: PathBuf,
    identities: PathBuf,
}

impl Options {
    fn parse() -> Result<Self, &'static str> {
        let mut bind = "127.0.0.1:0"
            .parse::<SocketAddr>()
            .map_err(|_| "invalid default bind")?;
        let (mut cert, mut key, mut identities) = (None, None, None);
        let mut seen = std::collections::HashSet::new();
        let mut args = std::env::args().skip(1);
        while let Some(flag) = args.next() {
            if !seen.insert(flag.clone()) {
                return Err("duplicate argument");
            }
            let value = args.next().ok_or("missing argument value")?;
            match flag.as_str() {
                "--bind" => bind = value.parse().map_err(|_| "invalid bind address")?,
                "--cert" => cert = Some(PathBuf::from(value)),
                "--key" => key = Some(PathBuf::from(value)),
                "--identities" => identities = Some(PathBuf::from(value)),
                _ => return Err("unsupported argument"),
            }
        }
        if !bind.ip().is_loopback() {
            return Err("proof endpoint must bind loopback");
        }
        Ok(Self {
            bind,
            cert: cert.ok_or("--cert is required")?,
            key: key.ok_or("--key is required")?,
            identities: identities.ok_or("--identities is required")?,
        })
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    identities: Vec<Identity>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Identity {
    proof: [u8; 32],
    publish: Option<String>,
    subscribe: Vec<String>,
    expires_unix_ms: u64,
}

struct ProofAuthority {
    identities: Vec<Arc<Identity>>,
}

fn now_ms() -> Result<u64, &'static str> {
    tilo_clock::try_unix_now_ms().ok_or("invalid system clock")
}

impl ProofAuthority {
    fn load(path: &Path) -> Result<Self, &'static str> {
        let file = std::fs::File::open(path).map_err(|_| "cannot open proof identities")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = file
                .metadata()
                .map_err(|_| "cannot inspect proof identities")?
                .permissions()
                .mode();
            if mode & 0o077 != 0 {
                return Err("proof identities must be private (mode 0600)");
            }
        }
        let mut bytes = Vec::new();
        file.take(65_537)
            .read_to_end(&mut bytes)
            .map_err(|_| "cannot read proof identities")?;
        if bytes.len() > 65_536 {
            return Err("proof identities exceed size bound");
        }
        let fixture: Fixture =
            serde_json::from_slice(&bytes).map_err(|_| "invalid proof identities")?;
        let now = now_ms()?;
        if fixture.identities.is_empty() || fixture.identities.len() > 8 {
            return Err("proof requires one to eight isolated identities");
        }
        for (index, identity) in fixture.identities.iter().enumerate() {
            if identity.expires_unix_ms <= now
                || identity.expires_unix_ms.saturating_sub(now) > 60_000
            {
                return Err("proof identity expiry must be within sixty seconds");
            }
            if identity.subscribe.len() > 8
                || (identity.publish.is_none() && identity.subscribe.is_empty())
            {
                return Err("invalid proof directions");
            }
            if fixture
                .identities
                .get(..index)
                .unwrap_or_default()
                .iter()
                .any(|prior| prior.proof == identity.proof)
            {
                return Err("duplicate proof identity");
            }
        }
        Ok(Self {
            identities: fixture.identities.into_iter().map(Arc::new).collect(),
        })
    }
}

impl RelayAuthority for ProofAuthority {
    fn admit(&self, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        if proof.len() != 32 {
            return Err(Denied);
        }
        self.identities
            .iter()
            .find(|identity| {
                // Even isolated fixtures avoid a matching-prefix timing oracle.
                identity
                    .proof
                    .iter()
                    .zip(proof)
                    .fold(0u8, |diff, (a, b)| diff | (a ^ b))
                    == 0
                    && now_ms().is_ok_and(|now| now < identity.expires_unix_ms)
            })
            .cloned()
            .map(|identity| identity as Arc<dyn SessionAuthorization>)
            .ok_or(Denied)
    }
}

impl SessionAuthorization for Identity {
    fn publication(&self) -> Option<&str> {
        self.publish.as_deref()
    }
    fn subscriptions(&self) -> &[String] {
        &self.subscribe
    }
    fn current(&self, direction: Direction, namespace: &str) -> bool {
        now_ms().is_ok_and(|now| now < self.expires_unix_ms)
            && match direction {
                Direction::Publish => self.publish.as_deref() == Some(namespace),
                Direction::Subscribe => self.subscribe.iter().any(|allowed| allowed == namespace),
            }
    }
}

#[tokio::main]
async fn main() {
    if let Err(message) = run().await {
        eprintln!("MoQ proof: {message}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), &'static str> {
    let options = Options::parse()?;
    let authority = Arc::new(ProofAuthority::load(&options.identities)?);
    let chain = rustls::pki_types::CertificateDer::pem_file_iter(&options.cert)
        .map_err(|_| "cannot read pinned TLS certificate")?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "cannot read pinned TLS certificate")?;
    let key = rustls::pki_types::PrivateKeyDer::from_pem_file(&options.key)
        .map_err(|_| "cannot read pinned TLS key")?;
    let endpoint = tilo_moq::carrier::listener(options.bind, chain, key)
        .map_err(|_| "cannot initialize pinned TLS WebTransport listener")?;
    let address = endpoint
        .local_addr()
        .map_err(|_| "cannot obtain listener address")?;
    let relay = Relay::new(authority);
    println!(
        "READY {}",
        serde_json::json!({
            "url":format!("https://{address}{VOICE_PATH}"),
            "protocol":tilo_moq::carrier::PROTOCOL,
            "identityMode":"isolated-proof-only",
        })
    );
    let mut counters = tokio::time::interval(Duration::from_secs(1));
    counters.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let serving = relay.clone().serve(endpoint);
    tokio::pin!(serving);
    loop {
        tokio::select! {
            () = &mut serving => return Err("WebTransport listener ended"),
            _ = counters.tick() => println!("COUNTERS {}",serde_json::to_string(&relay.counters()).map_err(|_| "counter serialization failed")?),
            result = tokio::signal::ctrl_c() => {
                result.map_err(|_| "cannot observe shutdown signal")?;
                break;
            }
        }
    }
    Ok(())
}
