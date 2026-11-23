//! Fill a public workspace with people, as its entry takes them in: each a
//! new person with their own root and device, entering through the
//! workspace's entry on a session with the server, as a page's Join does.
//! Nothing is written around the server: the members are the host's own, by
//! the path every visitor takes.
//!
//! ```sh
//! seed-lounge SERVER_ORIGIN WORKSPACE COUNT [CA_PEM]
//! ```
//!
//! `CA_PEM` is a certificate the server's TLS chains to (the local stack's
//! Caddy CA), trusted beside the system's roots. Prints one JSON line:
//! `{"joined": N, "seconds": S}`.

use std::sync::Arc;

use tilo_server_protocol::web::{PageHost, ServerAddress};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, DeviceIdentity, UserIdentity};

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: seed-lounge SERVER_STATE WORKSPACE COUNT";
    let state = args.next().ok_or(usage)?;
    let workspace = args.next().ok_or(usage)?;
    let count: usize = args
        .next()
        .ok_or(usage)?
        .parse()
        .map_err(|_| "COUNT is a number")?;
    // Who the server is and where it is dialled: what it wrote in its own state when its
    // browser listener started (`web-host.json`).
    let kept = std::path::Path::new(&state).join("web-host.json");
    let bytes = std::fs::read(&kept).map_err(|error| format!("{}: {error}", kept.display()))?;
    let info: PageHost =
        serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", kept.display()))?;
    let address: ServerAddress = info
        .address
        .ok_or("the server takes no device sessions")?
        .into();
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
        let held = tilo_session_client::device::Held {
            cert,
            server_cert: Some(info.server_cert.clone()),
        };
        let (cert, entering) = tilo_session_client::device::entry_request(
            &info.server_cert,
            &workspace,
            &device,
            tilo_session_client::device::Entering {
                held: Some(&held),
                root: None,
                label: &name,
                name: &name,
                token: None,
            },
        )
        .await
        .map_err(|refused| format!("{name}: {}", refused.message))?;
        let session = tilo_session_client::start_session(
            tilo_session_client::PeerConfig {
                host: tilo_server_client::ServerAddress::published(&address),
                cert,
                server_cert: info.server_cert.clone(),
                revocations: tilo_user_identity::RevocationList::new(),
                entering: Some(entering),
            },
            Arc::new(device),
            Arc::new(tilo_session_client::MemoryPeerStore::default()),
        );
        let entered = tilo_session_client::device::entered(&session).await;
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
