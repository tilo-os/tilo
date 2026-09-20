use tilo_install_artifacts::{Availability, Client, Error, ErrorKind, ReleaseSelection, Token};

#[test]
fn normal_facade_has_semantic_controls_only() {
    let _: fn(&Client, ReleaseSelection, Vec<u8>) -> Result<Token, Error> = Client::start;
    let _: fn(&Client, ReleaseSelection) -> Result<Token, Error> = Client::resume;
    let _: fn(&Client) -> tilo_install_artifacts::PreferenceState = Client::preference;
    let _: fn(&Client, &Token) -> Result<(), Error> = Client::pause;
    let _: fn(&Client, &Token) -> Result<(), Error> = Client::release;
}

#[test]
fn normal_constructor_does_not_select_fixture() {
    let availability = tilo_install_artifacts::availability();
    #[cfg(any(target_os = "linux", windows))]
    {
        assert_eq!(availability, Availability::Available);
        let mut client = Client::open_legacy_imager().unwrap();
        client.close().unwrap();
        client.close().unwrap();
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        assert_eq!(availability, Availability::UnsupportedHost);
        assert_eq!(
            Client::open_legacy_imager().err().unwrap().kind(),
            ErrorKind::Unsupported
        );
    }
}

#[cfg(all(unix, feature = "test-support"))]
#[test]
fn explicit_fixture_keeps_finals_and_close_refuses_new_operations() {
    use tilo_install_artifacts::{
        Phase,
        test_support::{Server, manifest_bytes, manifest_value, response},
    };
    let manifest = manifest_bytes(&manifest_value());
    let server = Server::new(vec![
        response(200, "", &manifest),
        response(200, "", &vec![b'E'; 4096]),
        response(200, "", &vec![b'R'; 8192]),
    ]);
    let root = tempfile::tempdir().unwrap();
    let mut client = server.client(root.path(), &root.path().join("selection.json"));
    let token = client
        .start(server.selection(&manifest), b"key".to_vec())
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if client.status().is_some_and(|s| s.phase == Phase::Ready) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{:?}",
            client.status()
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    client.release(&token).unwrap();
    loop {
        if client.status().is_some_and(|s| s.phase == Phase::Released) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{:?}",
            client.status()
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(
        std::fs::read(root.path().join("0/final")).unwrap(),
        vec![b'E'; 4096]
    );
    assert_eq!(
        std::fs::read(root.path().join("1/final")).unwrap(),
        vec![b'R'; 8192]
    );
    assert_eq!(server.consumptions(), 0);
    assert_eq!(
        std::fs::read(root.path().join("selection.json")).unwrap(),
        b"key"
    );
    client.close().unwrap();
    client.close().unwrap();
    assert_eq!(
        client
            .start(server.selection(&manifest), b"key".to_vec())
            .unwrap_err()
            .kind(),
        ErrorKind::Closed
    );
    assert_eq!(
        client.release(&token).unwrap_err().kind(),
        ErrorKind::Closed
    );
}
