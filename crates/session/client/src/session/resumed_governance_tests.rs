//! A page that opens over the store it kept: its replica is held already, so
//! the server's replay carries no governance change the page could follow.
//! What that replica's governance says — its members and their names — must
//! reach the page anyway, or Chat's `@` picker offers only the people
//! presence names.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use super::governance::governance_envelope;
use super::*;
use tilo_peer_state::owner::testing::{LateAdmission, late_admission_at};

tilo_automerge_isolate::test_worker!();

/// A device key the test holds, signing at once.
struct Key(ed25519_dalek::SigningKey);

impl DeviceSigner for Key {
    fn device_public_key(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }

    fn sign(&self, message: Vec<u8>) -> tilo_server_client::SignFuture<'_> {
        let signature = ed25519_dalek::Signer::sign(&self.0, &message).to_bytes();
        Box::pin(std::future::ready(Ok(signature)))
    }
}

#[test]
fn a_page_that_reopens_a_workspace_it_kept_names_its_members() {
    let LateAdmission {
        bootstrap,
        device_key,
        governance,
        admitting,
        governor,
        ..
    } = late_admission_at(web_time::SystemTime::now());
    let workspace_id = bootstrap.workspace_id;
    let workspace = workspace_id.to_string();
    // The device that answered is the one the session dials: its certificate
    // is the roster's for that device.
    let server_cert = bootstrap
        .certified_roster
        .iter()
        .find(|cert| cert.device_public_key == governor)
        .cloned()
        .unwrap();
    let config = PeerConfig::member(
        ServerAddress::through(
            iroh_base::EndpointAddr::new(
                iroh_base::EndpointId::from_bytes(&server_cert.device_public_key).unwrap(),
            ),
            None,
        ),
        bootstrap.device_cert.clone(),
        server_cert,
    );
    let answer = postcard::to_stdvec(
        &tilo_peer_state::join::JoinTransportResponse::MembershipComplete(bootstrap),
    )
    .unwrap();
    let store = Arc::new(crate::MemoryPeerStore::default());
    let bootstrap_envelope = || Envelope::WorkspaceBootstrap {
        workspace_id,
        bootstrap: answer.clone(),
    };

    // The page's first session takes the workspace in, admits its governance
    // and keeps it in the store.
    let (mut first, _, _) = session_task(
        config.clone(),
        Arc::new(Key(device_key.clone())),
        store.clone(),
        None,
        false,
    );
    first.inbound(bootstrap_envelope());
    first.inbound(governance_envelope(governor, workspace_id, &governance).unwrap());
    first.inbound(governance_envelope(governor, workspace_id, &admitting).unwrap());
    first.admit_governance();
    let members = |task: &Task| {
        task.reports
            .status()
            .workspace_members
            .get(&workspace)
            .cloned()
            .unwrap_or_default()
    };
    assert_eq!(
        members(&first).len(),
        2,
        "the page that joined names its members"
    );
    drop(first);

    // Opened again on the store it kept, it is sent the membership answer and
    // nothing else: what its governance already says must reach it as well.
    let (mut again, _, _) = session_task(config, Arc::new(Key(device_key)), store, None, false);
    again.inbound(bootstrap_envelope());
    assert_eq!(
        members(&again).len(),
        2,
        "the reopened page names the members its replica kept"
    );
}
