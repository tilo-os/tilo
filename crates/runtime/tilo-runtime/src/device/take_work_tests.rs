//! What a member that takes work answers, and where it keeps what it runs on.
#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use super::*;

tilo_test_env::scenario! {
    fn a_member_takes_work_from_the_server_its_profile_names;

    phase the_server_is_named_by_its_device_key;
    {
        let device = hex::encode([0x11; 32]);
        let server = Server::new(&device).unwrap();
        assert_eq!(server.device(), [0x11; 32]);
        assert_eq!(server.endpoint, [0x11; 32], "its endpoint is its device's");
    }

    phase a_server_named_by_something_that_is_no_device_key_is_refused;
    {
        assert!(Server::new("not hex").is_err());
        assert!(
            Server::new(&hex::encode([0x11; 16])).is_err(),
            "half a key names no device"
        );
    }

    phase a_tilo_servers_requests_come_from_its_endpoint_and_are_its_devices;
    {
        // A Tilo server keys its endpoint apart from its device: its requests come
        // from the endpoint, signed by the device.
        let server = Server::new(&hex::encode([0x11; 32]))
            .unwrap()
            .with_endpoint(&hex::encode(public(4)))
            .unwrap();
        assert_eq!(server.device(), [0x11; 32]);
        assert_eq!(server.endpoint, public(4));
    }

    phase a_login_joined_with_a_ticket_names_the_server_it_takes_work_for;
    // A profile joined with a ticket serves the server the login was answered with, reached
    // at the endpoint the ticket named.
    {
        let server = SigningKey::from_bytes(&[0x55; 32]);
        let endpoint = public(7);
        let login = tilo_device_identity::login::Device {
            site: "https://tilo.sh".into(),
            host_origin: "https://home.example".into(),
            workspace: "1f8f5b1c-6f2a-4c53-9c1a-2f0b3b0d4e11".into(),
            workspace_name: None,
            agent: "t".into(),
            cert: cert(&server),
            server_cert: cert(&server),
            server_address: Some(tilo_device_identity::login::ServerAddress {
                endpoint_addr: iroh::EndpointAddr::from_parts(
                    iroh::EndpointId::from_bytes(&endpoint).unwrap(),
                    Vec::new(),
                ),
                relay_url: None,
                web: None,
            }),
        };
        let found = Server::from_login(Some(&login)).unwrap();
        assert_eq!(found.device(), server.verifying_key().to_bytes());
        assert_eq!(found.endpoint, endpoint);
        assert!(Server::from_login(None).is_none(), "no login, no server");
    }

    phase each_workspaces_files_are_a_directory_of_their_own_under_the_profile;
    {
        let profile = Path::new("/var/lib/tilo/runtime");
        let workspace = "1f8f5b1c-6f2a-4c53-9c1a-2f0b3b0d4e11";
        let held = files(profile, workspace);
        assert_eq!(held, profile.join("state/take-work/files").join(workspace));
        assert!(
            held.starts_with(profile),
            "a member writes nothing outside its profile"
        );
        assert_eq!(
            files(profile, "../../etc"),
            profile.join("state/take-work/files").join("______etc"),
            "a name that is no workspace stays one directory"
        );
    }

    phase a_members_endpoint_listens_where_its_relay_reaches_it;
    // The endpoint a member's server reaches it at: keyed by the member's device
    // key, listening on the port its entry among its person's servers names.
    {
        let key = ed25519_dalek::SigningKey::from_bytes(&[0x44; 32]);
        let endpoint = crate::device::services::listen_endpoint(&key, Some(0)).unwrap();
        assert_eq!(
            endpoint.id().as_bytes(),
            &key.verifying_key().to_bytes(),
            "the endpoint is this device"
        );
        assert_eq!(
            endpoint
                .bound_sockets()
                .into_iter()
                .filter(std::net::SocketAddr::is_ipv4)
                .count(),
            1,
            "a member listens on one IPv4 socket, on the port it was given"
        );
    }
}

/// An endpoint id: the public half of a key seeded `seed`.
fn public(seed: u8) -> [u8; 32] {
    SigningKey::from_bytes(&[seed; 32])
        .verifying_key()
        .to_bytes()
}

/// A certificate `key`'s person signed for this device.
fn cert(key: &SigningKey) -> tilo_user_identity::DeviceCert {
    tilo_user_identity::DeviceCert {
        user_public_key: key.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "member".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    }
}
