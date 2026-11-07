//! Helpers shared by the test modules of this binary.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use registry_server::{AppState, Storage};

/// Run the HTTP server on its own runtime for the rest of the test process.
pub fn serve(storage: &Storage) -> SocketAddr {
    let storage = storage.clone();
    let identity = storage.load_identity().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
                let bound = registry_server::bind_and_run(addr, AppState::new(storage, identity))
                    .await
                    .unwrap();
                sender.send(bound).unwrap();
                std::future::pending::<()>().await;
            });
    });
    receiver.recv().unwrap()
}
