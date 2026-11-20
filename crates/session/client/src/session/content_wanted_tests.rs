//! A server asks its members for the content their files name, on its presence, since it
//! holds no session with each of them; a member that keeps the content answers.

use super::*;

/// The workspace these tests are of.
fn workspace() -> WorkspaceId {
    WorkspaceId::from_u128(1)
}

/// The server asking for content.
const SERVER: [u8; 32] = [9; 32];

/// The request frame asking device `requester` for a picture.
fn want(requester: u64) -> Vec<u8> {
    postcard::to_allocvec(&neighbour_wire::NeighbourMsg::DocumentContentWant {
        requester,
        app_id: "so.tilo.photos".into(),
        cluster_id: workspace().to_string(),
        hash: "ab".repeat(32),
        have: Vec::new(),
    })
    .unwrap()
}

/// The presence the server publishes, carrying `body`.
fn said(body: Vec<u8>) -> Envelope {
    Envelope::Presence {
        workspace_id: workspace(),
        payload: tilo_server_session::AttributedPresence {
            author: SERVER,
            body,
        }
        .encode()
        .unwrap(),
    }
}

#[test]
fn a_servers_ask_on_its_presence_is_a_want_the_member_answers() {
    let ask = want(tilo_peer_state::peer_handle(&SERVER));
    let (of, from, frame) =
        wanted(&said(ask.clone())).expect("the server's ask is this device's to answer");
    assert_eq!((of, from), (workspace(), SERVER));
    assert_eq!(
        frame, ask,
        "answered as it was asked, from the server's device"
    );
}

#[test]
fn a_members_ask_addressed_to_the_device_is_a_want_too() {
    let ask = want(tilo_peer_state::peer_handle(&SERVER));
    let envelope = Envelope::DocumentContentWant {
        workspace_id: workspace(),
        from_device_public_key: SERVER,
        frame: ask.clone(),
    };
    let (_, from, frame) = wanted(&envelope).expect("a member's ask");
    assert_eq!((from, frame), (SERVER, ask));
}

#[test]
fn presence_that_is_not_an_ask_by_a_device_is_no_want() {
    assert!(
        wanted(&said(want(42))).is_none(),
        "a want its publisher did not make"
    );
    assert!(
        wanted(&said(b"a person's presence".to_vec())).is_none(),
        "a person's presence passes as it always did"
    );
    assert!(
        wanted(&Envelope::Presence {
            workspace_id: workspace(),
            payload: b"not an attributed presence".to_vec(),
        })
        .is_none(),
        "presence from another tier"
    );
}
