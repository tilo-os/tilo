//! Installed data-route dispatch through real owners and ingress requests.
use super::*;
use tilo_relay::delivery_v3::{cursor::CursorToken, poll_chain::ChainPoll};

fn routes() -> DataRoutes {
    let (ingress, _receiver) = crate::https::ingress_channel();
    DataRoutes::new(crate::store::HostWorkInventory::new(&ingress.host_bytes()).unwrap())
}
fn gate(routes: &DataRoutes, session: [u8; 16], chain: u64, index: u32) -> &'static str {
    match routes.chain_gate(session, Some(ChainPoll { chain, index })) {
        ChainGate::Ready(None) => "ready",
        ChainGate::Ready(Some(_)) => "ready-after-floor",
        ChainGate::Wait => "wait",
        ChainGate::Stale => "stale",
    }
}

/// PIPELINED_POLL_V1 ordering: an index is served only after the one before
/// it was answered, continues after the last page, and other chains are stale.
#[test]
fn chain_gate_serves_in_order_and_advances_only_on_answers() {
    let mut routes = routes();
    let (s, other) = ([1; 16], [2; 16]);
    assert!(
        matches!(routes.chain_gate(s, None), ChainGate::Ready(None)),
        "plain poll"
    );
    assert_eq!(gate(&routes, s, 7, 1), "stale", "no chain started");
    routes.start_chain(s, 7);
    assert_eq!(gate(&routes, s, 7, 0), "ready");
    assert_eq!(gate(&routes, s, 7, 1), "wait");
    assert_eq!(gate(&routes, s, 8, 0), "stale");
    assert_eq!(
        gate(&routes, other, 7, 0),
        "stale",
        "chains are per session"
    );
    // Index 0 answered with a page: index 1 continues after its end.
    let end = CursorToken::from_bytes(&[9; 8]).unwrap();
    routes.chain_answered(s, ChainPoll { chain: 7, index: 0 }, Some(end.clone()));
    assert_eq!(gate(&routes, s, 7, 0), "stale", "answered once");
    assert_eq!(gate(&routes, s, 7, 1), "ready-after-floor");
    assert_eq!(gate(&routes, s, 7, 2), "wait");
    // An empty (idle) answer moves on and keeps the floor.
    routes.chain_answered(s, ChainPoll { chain: 7, index: 1 }, None);
    match routes.chain_gate(s, Some(ChainPoll { chain: 7, index: 2 })) {
        ChainGate::Ready(Some(floor)) => assert_eq!(floor, end),
        _ => panic!("index 2 not ready after its predecessor"),
    }
    // An out-of-turn answer changes nothing.
    routes.chain_answered(s, ChainPoll { chain: 7, index: 5 }, None);
    assert_eq!(gate(&routes, s, 7, 2), "ready-after-floor");
    // A new chain start replaces the old one.
    routes.start_chain(s, 8);
    assert_eq!(gate(&routes, s, 7, 2), "stale");
    assert_eq!(gate(&routes, s, 8, 0), "ready");
}

#[test]
fn chain_states_are_bounded() {
    let mut routes = routes();
    for n in 0..(CHAIN_STATES as u16 + 10) {
        let mut session = [0; 16];
        session[..2].copy_from_slice(&n.to_le_bytes());
        routes.start_chain(session, 1);
    }
    // Sessions without held polls were evicted to admit new chains.
    assert!(routes.chains.len() <= CHAIN_STATES);
    let mut last = [0; 16];
    last[..2].copy_from_slice(&(CHAIN_STATES as u16 + 9).to_le_bytes());
    assert_eq!(gate(&routes, last, 1, 0), "ready");
}
