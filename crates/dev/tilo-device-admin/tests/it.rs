//! The route end to end on this machine: a device's service and dialers of
//! several kinds, through a relay of the test's own.

use std::path::Path;
use std::time::Duration;

use iroh_relay::server::{RelayConfig, Server, ServerConfig};
use tilo_device_admin::dial::{Dialer, Failure, Options};
use tilo_device_admin::handshake::{Refusal, Untrusted};
use tilo_device_admin::net::Relays;
use tilo_device_admin::{identity, serve};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, IdentityPaths, persist, sign_device_cert,
};
use tokio::io::{AsyncBufReadExt as _, BufReader};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario: each phase reuses the relay, the device and its service"
)]
async fn the_route_admits_the_owners_administrator_alone() {
    fn enrol(
        root: &Path,
        person: &ed25519_dalek::SigningKey,
        label: &str,
        capabilities: DeviceCapabilities,
    ) -> DeviceCert {
        let device = identity::ensure_key(root).unwrap();
        let mut cert = DeviceCert {
            user_public_key: person.verifying_key().to_bytes(),
            device_public_key: device,
            device_label: label.into(),
            capabilities,
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, person).unwrap();
        let paths = IdentityPaths::under(root);
        persist::write_device_cert(&paths.device_cert, &cert).unwrap();
        cert
    }

    let mut config = ServerConfig::default();
    config.relay = Some(RelayConfig::new((std::net::Ipv4Addr::LOCALHOST, 0)));
    let relay = Server::spawn(config).await.unwrap();
    let url = format!("http://{}", relay.http_addr().unwrap());
    let relays = Relays::choose(Some(&url)).unwrap();

    let person = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
    let stranger = ed25519_dalek::SigningKey::from_bytes(&[32; 32]);
    let dirs: Vec<_> = (0..5).map(|_| tempfile::tempdir().unwrap()).collect();
    let device = enrol(
        dirs[0].path(),
        &person,
        "board",
        DeviceCapabilities::member(),
    );
    enrol(
        dirs[1].path(),
        &person,
        "workstation",
        DeviceCapabilities::administrator(),
    );
    enrol(
        dirs[2].path(),
        &person,
        "phone",
        DeviceCapabilities::primary(),
    );
    enrol(
        dirs[3].path(),
        &stranger,
        "theirs",
        DeviceCapabilities::administrator(),
    );
    identity::ensure_key(dirs[4].path()).unwrap();

    // The daemon answers each request line with the line itself.
    let service = serve::start(&serve::Options {
        identity_root: dirs[0].path().to_owned(),
        relays: relays.clone(),
        daemon: [
            "sh",
            "-c",
            "while IFS= read -r line; do printf '%s\\n' \"$line\"; done",
        ]
        .map(String::from)
        .to_vec(),
        state_file: None,
    })
    .await
    .unwrap();
    let endpoint = service.endpoint().id();
    assert_ne!(endpoint.as_bytes(), &device.device_public_key);

    let options = |root: &Path, expected: Option<[u8; 32]>| Options {
        identity_root: root.to_owned(),
        endpoint,
        device: expected,
        relays: relays.clone(),
        home_relay: None,
        wait: Duration::from_secs(20),
    };
    let open = |root: &Path, expected: Option<[u8; 32]>| {
        let options = options(root, expected);
        async move {
            let dialer = Dialer::new(&options).await?;
            dialer
                .open(options.wait)
                .await
                .map(|session| (dialer, session))
        }
    };

    // Phase: the owner's administrator reaches tilo-vmd's protocol.
    {
        let (dialer, session) = open(dirs[1].path(), Some(device.device_public_key))
            .await
            .map_err(|failure| failure.to_string())
            .unwrap();
        assert_eq!(
            session.preamble.device,
            hex::encode(device.device_public_key)
        );
        assert_eq!(session.preamble.label, "board");
        assert!(["relay", "direct"].contains(&session.preamble.route.as_str()));
        let mut send = session.send;
        let mut reader = BufReader::new(session.recv);
        let request = br#"{"jsonrpc":"2.0","id":1,"method":"status","params":{}}"#;
        send.write_all(request).await.unwrap();
        send.write_all(b"\n").await.unwrap();
        let mut answer = Vec::new();
        reader.read_until(b'\n', &mut answer).await.unwrap();
        assert_eq!(answer.strip_suffix(b"\n").unwrap(), request);
        send.finish().unwrap();
        drop(reader);
        dialer.close().await;
    }

    // Phase: the person's devices without the grant, and another identity's
    // administrator, are refused with the reason.
    for (dir, reason) in [
        (dirs[2].path(), Refusal::NotAdministrator),
        (dirs[3].path(), Refusal::OtherIdentity),
    ] {
        let failure = open(dir, None).await.err().unwrap();
        assert!(
            matches!(failure, Failure::Refused(r) if r == reason),
            "{failure}"
        );
    }

    // Phase: a grant written into a certificate after it was signed does
    // not verify, so the device refuses it as uncertified.
    {
        let path = IdentityPaths::under(dirs[2].path()).device_cert;
        let mut cert = persist::read_device_cert(&path).unwrap();
        cert.capabilities.can_administer_devices = true;
        persist::write_device_cert(&path, &cert).unwrap();
        let failure = open(dirs[2].path(), None).await.err().unwrap();
        assert!(
            matches!(failure, Failure::Refused(Refusal::NotCertified)),
            "{failure}"
        );
    }

    // Phase: a machine with no certificate cannot dial at all.
    {
        let failure = open(dirs[4].path(), None).await.err().unwrap();
        assert_eq!(failure.exit_code(), 5, "{failure}");
    }

    // Phase: the dialer refuses an answer from another device than it asked for.
    {
        let failure = open(dirs[1].path(), Some([7; 32])).await.err().unwrap();
        assert!(
            matches!(failure, Failure::Untrusted(Untrusted::OtherDevice { .. })),
            "{failure}"
        );
    }
    service.shutdown().await;
}
