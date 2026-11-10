//! Fill a public workspace with people, as its door takes them in: each a
//! new person with their own root and device, asking the workspace's door
//! (`<relay>/peer/v1/directory/<workspace>/`) as a page's Join does, then
//! redeeming the invitation it answers with on the relay. Nothing is written
//! around the relay: the members are the host's own, by the path every
//! visitor takes.
//!
//! ```sh
//! seed-lounge RELAY_ORIGIN WORKSPACE COUNT [CA_PEM]
//! ```
//!
//! `CA_PEM` is a certificate the relay's TLS chains to (the local stack's
//! Caddy CA), trusted beside the system's roots. Prints one JSON line:
//! `{"joined": N, "seconds": S}`.

use base64::Engine as _;
use tilo_relay_protocol::workspaces::JoinReply;
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
    let door = tilo_net::Url::parse(&format!(
        "{}/peer/v1/directory/{workspace}/",
        relay.trim_end_matches('/')
    ))
    .map_err(|error| error.to_string())?;
    let began = std::time::Instant::now();
    let mut joined: usize = 0;
    for (index, number) in (0..count).zip(1..) {
        let name = format!("Seeded member {number}");
        let (root, user) = UserIdentity::generate(&name, 1);
        let person = tilo_workspace_directory::Person {
            user: user.public_key,
            name: name.clone(),
        };
        let granted = tilo_workspace_directory::join(&client, &door, &person)
            .await
            .map_err(|error| format!("{name}: the door: {error}"))?;
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
        let invitation = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&granted.invitation)
            .map_err(|error| error.to_string())?;
        let origin = granted.origin.unwrap_or_else(|| relay.clone());
        let host = tilo_relay_client::https::Origin::new(&client, &origin);
        let mut request = [0; 32];
        request[..8].copy_from_slice(&(index as u64).to_le_bytes());
        let reply = tilo_relay_client::invite::ask_to_join_pre_admitted(
            &host,
            &invitation,
            &cert,
            &device,
            request,
            &name,
            granted.admission_token.as_bytes(),
        )
        .await
        .map_err(|error| format!("{name}: joining: {error}"))?;
        if !matches!(reply, JoinReply::Admitted { .. }) {
            return Err(format!("{name} was not admitted: {reply:?}"));
        }
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
