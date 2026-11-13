//! A record that reaches a page before the governance admitting the page:
//! the page reads its records as the member it is, so its readers hear of
//! the record once the change admitting the page arrived, and then read it.
//!
//! A host replays a workspace's governance in parts, and a newcomer's own
//! admission is its newest change: what else the newcomer was sent can be
//! taken in before it. A reader told of a record then read it as a stranger,
//! was refused, and was not told again, so the record stayed unread.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use super::*;
use tilo_peer_state::owner::testing::{APP, LateAdmission, late_admission_at};

tilo_automerge_isolate::test_worker!();

/// A device key the test holds, signing at once.
struct Key(ed25519_dalek::SigningKey);

impl DeviceSigner for Key {
    fn device_public_key(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }

    fn sign(&self, message: Vec<u8>) -> tilo_relay_client::SignFuture<'_> {
        let signature = ed25519_dalek::Signer::sign(&self.0, &message).to_bytes();
        Box::pin(std::future::ready(Ok(signature)))
    }
}

/// `message`, as the host relays a retained write of `author` to `workspace_id`.
fn relayed(
    workspace_id: u64,
    author: [u8; 32],
    message: &neighbour_wire::NeighbourMsg,
) -> Envelope {
    let body = postcard::to_stdvec(message).unwrap();
    Envelope::Delta {
        workspace_id,
        author_device_public_key: author,
        authored_at_ns: 1,
        delta: tilo_relay_client::coedit::CoeditPayload::RetainedDelta {
            channel: [0; 32],
            body,
        }
        .encode()
        .unwrap(),
        signature: [0; 64],
    }
}

/// Whether `link` heard that `app`'s record `""` landed.
fn heard_landed(link: &Link) -> bool {
    link.drain().into_iter().any(|event| {
        matches!(event, crate::Event::Landed { route, .. } if route.app_id == APP && route.record.is_empty())
    })
}

#[test]
fn a_record_sent_before_the_pages_admission_is_read_once_it_is_admitted() {
    let LateAdmission {
        bootstrap,
        device_key,
        governance,
        record,
        admitting,
        governor,
    } = late_admission_at(web_time::SystemTime::now());
    let workspace_id = bootstrap.workspace_id;
    let workspace = bootstrap.room.clone();
    let host = bootstrap.host.clone();
    let config = PeerConfig {
        host: tilo_relay_client::Host::through(
            iroh_base::EndpointAddr::new(
                iroh_base::EndpointId::from_bytes(&host.device_public_key).unwrap(),
            ),
            None,
        ),
        cert: bootstrap.device_cert.clone(),
        host_cert: host,
        revocations: RevocationList::new(),
        entering: None,
    };
    let (mut task, link, _commands) = session_task(
        config,
        Arc::new(Key(device_key)),
        Arc::new(crate::MemoryPeerStore::default()),
    );
    let answer = postcard::to_stdvec(
        &tilo_peer_state::join::JoinTransportResponse::MembershipComplete(bootstrap),
    )
    .unwrap();
    task.inbound(Envelope::WorkspaceBootstrap {
        workspace_id,
        bootstrap: answer,
    });
    task.inbound(relayed(workspace_id, governor, &governance));
    task.inbound(relayed(workspace_id, governor, &record));
    // What the host sent at once is taken in together.
    task.keep_arrived();
    assert!(
        !task.replicas.admits_this_device(workspace_id),
        "the governance so far does not admit the page"
    );
    let read = || link.engine().record(&workspace, APP, "");
    assert!(
        !heard_landed(&link),
        "a record is not taken to its readers before the page is admitted: they would read it as \
         a stranger's ({:?})",
        read()
    );
    task.inbound(relayed(workspace_id, governor, &admitting));
    task.admit_governance();
    assert!(task.replicas.admits_this_device(workspace_id));
    assert!(
        heard_landed(&link),
        "the record lands once the page is admitted"
    );
    assert!(
        matches!(read(), Ok(Some(_))),
        "the page reads the record: {:?}",
        read()
    );
}
