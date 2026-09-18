use super::*;
// Catches a control request waiting for a held store borrow or an admitted step.
#[test]
fn disconnect_latch_closes_before_the_admitted_step_drains() {
    let gate = Arc::new(AdmissionGate::new());
    let session = TransactionSession::new(ConnectionId([1; 16]), Weak::new(), Arc::clone(&gate));
    gate.state.lock().unwrap().in_flight = 1;
    session.request_disconnect();
    assert!(gate.stopped().load(Ordering::Acquire));
    let state = gate.state.lock().unwrap();
    assert!(state.closed);
    assert_eq!(state.in_flight, 1);
    drop(state);
}
