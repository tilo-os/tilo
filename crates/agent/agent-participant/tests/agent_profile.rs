//! An agent's thread on a host sees a profile of its own: the apps it
//! mounts keep their files below its root and their secrets in its own
//! store, never the host user's.

#![allow(clippy::unwrap_used, reason = "in a test these are the assertion")]

mod common;

use tilo_agent_participant::{HostedApp, Seed};

#[test]
fn an_agents_apps_keep_to_its_profile() {
    common::isolate_device();
    let host_vault = totp::storage::data_dir();
    let root = tempfile::tempdir().unwrap();
    let agent_root = root.path().to_path_buf();
    std::thread::spawn(move || {
        let _profile = tilo_paths::scope_profile(&agent_root);
        let _secrets = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
        assert!(totp::storage::data_dir().unwrap().starts_with(&agent_root));
        assert_eq!(
            tilo_secrets::SecretStore::for_app("so.tilo.totp").protection(),
            tilo_secrets::Protection::Memory
        );
        let app = HostedApp::open(
            &common::catalogue(),
            "so.tilo.totp",
            common::ROOM,
            "writer",
            true,
            Seed::New,
        )
        .unwrap();
        app.close();
    })
    .join()
    .unwrap();
    assert_eq!(
        totp::storage::data_dir(),
        host_vault,
        "the host's own thread is unchanged"
    );
}
