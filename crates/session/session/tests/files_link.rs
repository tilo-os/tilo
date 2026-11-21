//! A device's files daemon reaches the network through the session's endpoint: the device has one
//! endpoint id, so a server never sees two endpoints under its key replace each other, and a file's
//! bytes still travel both ways between the device and a peer elsewhere.
//!
//! The daemon here is the one `fs serve --session-link` runs, in this test's process: its link to
//! the session, the content store it serves and the hydrator it fetches with. The session is a
//! real device's, started as the login starts it.

#![cfg(target_os = "linux")]

use std::sync::{Arc, Mutex, RwLock};

use crate::common::people::Person;

#[test]
fn the_files_daemon_reaches_peers_through_the_sessions_endpoint() {
    let person = Person::new("Ada");
    let mut laptop = person.device("Ada's laptop");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime for the files daemon");
    // The daemon's stores spawn onto the runtime they are made in.
    let _in_runtime = runtime.enter();

    // The daemon starts first, as the files service does at boot.
    let now_ns = tilo_clock::unix_now_ns();
    let (device, credentials) =
        fs::identity::load_certified_mount_identity(&laptop.identity(), now_ns)
            .expect("this device's certificate is valid")
            .into_parts();
    let link_file = laptop.directory().join("runtime").join(fs::net::LINK_FILE);
    // Serve `store` as a files daemon serves its workspace's content, unenforced.
    let serve = |net: &fs::net::Net, store: &Arc<fs::ContentStore>, name: &str| {
        fs::Workspace::host(
            fs::sync::SyncNode {
                net: net.clone(),
                store: Arc::clone(store),
                hydrator: Arc::new(fs::Hydrator::new(net.clone(), Arc::clone(store))),
                namespace: Arc::new(Mutex::new(fs::Namespace::new(name).expect("a namespace"))),
                identity: fs::FsTransportIdentity::new(
                    credentials.clone(),
                    Arc::new(RwLock::new(tilo_user_identity::RevocationList::new())),
                ),
                policy: None,
            },
            None,
        )
        .expect("the daemon serves the workspace's content")
        .0
    };
    let (net, store, here, _workspace) = runtime.block_on(async {
        let link = fs::net::SessionLink::bind(device.secret.public(), &link_file)
            .await
            .expect("the daemon's link to the session");
        let net = fs::net::Net::Session(link);
        let store = Arc::new(fs::ContentStore::memory());
        let here = store
            .ingest(b"written on the laptop".to_vec())
            .await
            .expect("content kept on the laptop");
        let workspace = serve(&net, &store, "ada-laptop");
        (net, store, here, workspace)
    });
    let fs::net::Net::Session(link) = &net else {
        unreachable!("the daemon was given the session's link");
    };

    laptop.start();
    let mut hellos = link.hellos();
    runtime
        .block_on(tilo_test_env::within(
            "the session to greet its files daemon",
            hellos.wait_for(Option::is_some),
        ))
        .expect("the daemon hears the session");
    assert_eq!(net.id(), device.secret.public());

    // A peer elsewhere, on the relay the laptop uses.
    let peer_key = iroh::SecretKey::generate();
    let relay: iroh::RelayUrl = test_server::loopback_url()
        .parse()
        .expect("the loopback relay's address");
    let theirs = Arc::new(fs::ContentStore::memory());
    let (peer, there, _peer_workspace) = runtime.block_on(async {
        let peer = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(test_server::relay_mode())
            .secret_key(peer_key.clone())
            .bind()
            .await
            .expect("the peer's endpoint");
        tilo_test_env::within("the peer to take the relay as home", peer.online()).await;
        let there = theirs
            .ingest(b"written elsewhere".to_vec())
            .await
            .expect("content kept elsewhere");
        let workspace = serve(&fs::net::Net::from(peer.clone()), &theirs, "elsewhere");
        (peer, there, workspace)
    });

    // The daemon fetches the peer's file through the session, from the server alone.
    let fetched = runtime
        .block_on(
            fs::Hydrator::new(net.clone(), Arc::new(fs::ContentStore::memory())).fetch_from(
                there,
                iroh::EndpointAddr::new(peer_key.public()).with_relay_url(relay.clone()),
            ),
        )
        .expect("the peer's file reaches the laptop");
    assert_eq!(&fetched[..], b"written elsewhere");

    // The peer fetches the laptop's file, reaching the device by its one id on the server.
    let fetched = runtime
        .block_on(
            fs::Hydrator::new(peer, Arc::new(fs::ContentStore::memory())).fetch_from(
                here,
                iroh::EndpointAddr::new(device.secret.public()).with_relay_url(relay),
            ),
        )
        .expect("the laptop's file reaches the peer");
    assert_eq!(&fetched[..], b"written on the laptop");
    drop(store);

    laptop.stop();
    let log = laptop.log();
    assert!(
        log.contains("the files daemon reaches the network through this session"),
        "the session never linked its files daemon:\n{log}"
    );
    assert!(
        !log.contains("Another endpoint connected with the same endpoint id"),
        "the laptop ran two endpoints under its key:\n{log}"
    );
}
