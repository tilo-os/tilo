//! Realtime commands are delegated with the identity the broker authenticated, never one the caller claims.

use super::*;
use crate::services::{RealtimeRequestContext, RealtimeServices};

#[derive(Default)]
struct RecordingRealtime {
    calls: Mutex<Vec<(RealtimeRequestContext, Vec<u8>)>>,
}

impl RealtimeServices for RecordingRealtime {
    fn command(
        &self,
        context: &RealtimeRequestContext,
        command_json: &[u8],
    ) -> Result<Vec<u8>, String> {
        self.calls
            .lock()
            .map_err(|_| "recording service unavailable".to_string())?
            .push((context.clone(), command_json.to_vec()));
        Ok(br#"{"accepted":true}"#.to_vec())
    }

    fn subscribe(
        &self,
        _context: &RealtimeRequestContext,
    ) -> Result<mpsc::UnboundedReceiver<Vec<u8>>, String> {
        let (_tx, rx) = mpsc::unbounded_channel();
        Ok(rx)
    }

    fn disconnect(&self, _peer_id: u64) {}
}

fn context() -> RealtimeRequestContext {
    RealtimeRequestContext::authenticated(41, "so.tilo.video", "workspace-7")
}

#[test]
fn realtime_delegation_uses_only_broker_authenticated_identity() {
    let service = Arc::new(RecordingRealtime::default());
    let services = HostServices {
        realtime: Some(service.clone()),
        ..Default::default()
    };
    let authenticated = context();
    let reply = delegate_realtime_command(
        &services,
        &authenticated,
        "so.tilo.video",
        br#"{"register":{}}"#,
    )
    .expect("matching certified route delegates");
    assert_eq!(reply, br#"{"accepted":true}"#);
    assert_eq!(
        service.calls.lock().expect("calls").as_slice(),
        &[(authenticated, br#"{"register":{}}"#.to_vec())]
    );
}

#[test]
fn realtime_claim_mismatch_and_absent_service_fail_without_delegation() {
    let service = Arc::new(RecordingRealtime::default());
    let services = HostServices {
        realtime: Some(service.clone()),
        ..Default::default()
    };
    let mismatch = delegate_realtime_command(&services, &context(), "so.tilo.impostor", b"{}")
        .expect_err("caller claim cannot override certified route");
    assert!(mismatch.to_string().contains("certified connection route"));
    assert!(service.calls.lock().expect("calls").is_empty());

    let unsupported =
        delegate_realtime_command(&HostServices::default(), &context(), "so.tilo.video", b"{}")
            .expect_err("host without native service fails explicitly");
    assert!(unsupported.to_string().contains("unsupported by this host"));
}
