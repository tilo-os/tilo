//! Fill a public workspace with people, as its door takes them in: each a
//! new person with their own root and device, entering through the
//! workspace's door on a session with the relay, as a page's Join does.
//! Nothing is written around the relay: the members are the host's own, by
//! the path every visitor takes.
//!
//! ```sh
//! seed-lounge RELAY_ORIGIN WORKSPACE COUNT [CA_PEM]
//! ```
//!
//! `CA_PEM` is a certificate the relay's TLS chains to (the local stack's
//! Caddy CA), trusted beside the system's roots. Prints one JSON line:
//! `{"joined": N, "seconds": S}`.

use std::sync::Arc;

use tilo_user_identity::{DeviceCapabilities, DeviceCert, DeviceIdentity, UserIdentity};

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: seed-lounge RELAY_ORIGIN WORKSPACE COUNT [CA_PEM]";
    let relay = args.next().ok_or(usage)?;
    let workspace = args.next().ok_or(usage)?;
    let count: usize = args
        .next()
        .ok_or(usage)?
        .parse()
        .map_err(|_| "COUNT is a number")?;
    let mut builder = tilo_net::Client::builder();
    if let Some(ca) = args.next() {
        use rustls::pki_types::CertificateDer;
        use rustls::pki_types::pem::PemObject as _;
        let pem = std::fs::read(&ca).map_err(|error| format!("{ca}: {error}"))?;
        let trusted = CertificateDer::pem_slice_iter(&pem)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("{ca}: {error}"))?;
        builder = builder.tls(
            tilo_net::client_config_also_trusting(trusted).map_err(|error| error.to_string())?,
        );
    }
    let client = builder.build().map_err(|error| error.to_string())?;
    let host = tilo_relay_client::https::Origin::new(&client, &relay);
    let info = tilo_relay_client::https::host_info(&host)
        .await
        .map_err(|error| format!("the relay: {error}"))?;
    let address = info.address.ok_or("the relay takes no device sessions")?;
    let began = std::time::Instant::now();
    let mut joined: usize = 0;
    for number in 1..=count {
        let name = format!("Seeded member {number}");
        let (root, user) = UserIdentity::generate(&name, 1);
        let (device, identity) = DeviceIdentity::generate(format!("{name}'s browser"), 1);
        let mut cert = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: identity.public_key,
            device_label: identity.label,
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: tilo_clock::unix_now_ns().saturating_sub(1_000_000_000),
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root)
            .map_err(|error| error.to_string())?;
        let held = tilo_session_host::device::Held {
            cert,
            host_cert: Some(info.host_cert.clone()),
        };
        let (cert, entering) = tilo_session_host::device::door_entry(
            &info.host_cert,
            &workspace,
            &device,
            tilo_session_host::device::Entering {
                held: Some(&held),
                root: None,
                label: &name,
                name: &name,
                token: None,
            },
        )
        .await
        .map_err(|refused| format!("{name}: {}", refused.message))?;
        let session = tilo_session_host::PeerHandle::start(
            tilo_session_host::PeerConfig {
                host: tilo_session_host::Host::published(&address),
                cert,
                host_cert: info.host_cert.clone(),
                revocations: tilo_user_identity::RevocationList::new(),
                entering: Some(entering),
            },
            Arc::new(device),
            Arc::new(tilo_session_host::MemoryPeerStore::default()),
        );
        let entered = tilo_session_host::device::entered(&session).await;
        session.close();
        entered.map_err(|refused| format!("{name} was not admitted: {}", refused.message))?;
        joined = number;
        if joined.is_multiple_of(50) {
            eprintln!("seed-lounge: {joined} of {count} joined");
        }
    }
    println!(
        "{}",
        serde_json::json!({"joined": joined, "seconds": began.elapsed().as_secs_f64()})
    );
    Ok(())
}
