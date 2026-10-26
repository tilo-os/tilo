use super::*;
use crate::services::{
    NearbyTransferRequestContext, NearbyTransferServices, WifiContinuityServices,
};

#[derive(Default)]
struct RecordingWifi {
    calls: Mutex<Vec<runtime_collab::WifiContinuityCommand>>,
}

impl WifiContinuityServices for RecordingWifi {
    fn command(
        &self,
        command: runtime_collab::WifiContinuityCommand,
    ) -> runtime_collab::WifiContinuityReply {
        self.calls.lock().expect("calls").push(command);
        runtime_collab::WifiContinuityReply::NotFound
    }
}

struct RecordingNearby {
    reply: runtime_collab::NearbyTransferReply,
    calls: Mutex<
        Vec<(
            NearbyTransferRequestContext,
            runtime_collab::NearbyTransferCommand,
        )>,
    >,
}

impl RecordingNearby {
    fn answering(reply: runtime_collab::NearbyTransferReply) -> Self {
        Self {
            reply,
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl NearbyTransferServices for RecordingNearby {
    fn command(
        &self,
        context: &NearbyTransferRequestContext,
        command: runtime_collab::NearbyTransferCommand,
    ) -> Result<runtime_collab::NearbyTransferReply, String> {
        self.calls
            .lock()
            .expect("calls")
            .push((context.clone(), command));
        Ok(self.reply.clone())
    }

    fn disconnect(&self, _peer_id: u64) {}

    fn changed(
        &self,
        context: &NearbyTransferRequestContext,
        after: u64,
    ) -> Option<crate::services::NearbyTransferChange> {
        self.calls.lock().expect("calls").push((
            context.clone(),
            runtime_collab::NearbyTransferCommand::Snapshot,
        ));
        Some(Box::pin(std::future::ready(after.wrapping_add(1))))
    }
}

/// A host that answers commands but cannot say when its state changes.
struct UnannouncedNearby;

impl NearbyTransferServices for UnannouncedNearby {
    fn command(
        &self,
        _context: &NearbyTransferRequestContext,
        _command: runtime_collab::NearbyTransferCommand,
    ) -> Result<runtime_collab::NearbyTransferReply, String> {
        Ok(runtime_collab::NearbyTransferReply::Accepted)
    }

    fn disconnect(&self, _peer_id: u64) {}
}

fn lookup() -> Vec<u8> {
    serde_json::to_vec(&runtime_collab::WifiContinuityCommand::Lookup {
        ssid: "home".into(),
        security: runtime_collab::PortableWifiSecurity::Wpa2,
    })
    .expect("encode lookup")
}

fn snapshot() -> Vec<u8> {
    serde_json::to_vec(&runtime_collab::NearbyTransferCommand::Snapshot).expect("encode snapshot")
}

fn files_context() -> NearbyTransferRequestContext {
    NearbyTransferRequestContext::authenticated(
        41,
        NEARBY_TRANSFER_APP_ID,
        "workspace-7",
        CrdtRoute::app(NEARBY_TRANSFER_APP_ID, "workspace-7").channel_key(),
        Some("local-person".into()),
    )
}

#[test]
fn network_wake_uses_only_the_certified_settings_capability() {
    let service = Arc::new(RecordingWifi::default());
    let services = HostServices {
        wifi_continuity: Some(service.clone()),
        ..Default::default()
    };
    let command = runtime_collab::WifiContinuityCommand::NetworkWake {
        profile_uuid: "11111111-1111-4111-8111-111111111111".into(),
        enabled: Some(true),
    };
    let wire = serde_json::to_vec(&command).unwrap();
    delegate_wifi_continuity_command(
        &services,
        WIFI_CONTINUITY_APP_ID,
        WIFI_CONTINUITY_APP_ID,
        &wire,
    )
    .unwrap();
    assert_eq!(service.calls.lock().unwrap().as_slice(), [command]);
    assert!(
        delegate_wifi_continuity_command(
            &services,
            "so.tilo.browser",
            WIFI_CONTINUITY_APP_ID,
            &wire
        )
        .is_err()
    );
    assert!(
        delegate_wifi_continuity_command(
            &services,
            WIFI_CONTINUITY_APP_ID,
            "so.tilo.browser",
            &wire
        )
        .is_err()
    );
    assert!(
        delegate_wifi_continuity_command(
            &services,
            WIFI_CONTINUITY_APP_ID,
            WIFI_CONTINUITY_APP_ID,
            br#"{"command":"network-wake","profile_uuid":"--injection","enabled":true}"#
        )
        .is_err()
    );
    assert_eq!(service.calls.lock().unwrap().len(), 1);
}

#[test]
fn wifi_continuity_delegates_only_the_certified_settings_route() {
    let service = Arc::new(RecordingWifi::default());
    let services = HostServices {
        wifi_continuity: Some(service.clone()),
        ..Default::default()
    };
    let reply = delegate_wifi_continuity_command(
        &services,
        WIFI_CONTINUITY_APP_ID,
        WIFI_CONTINUITY_APP_ID,
        &lookup(),
    )
    .expect("certified Settings delegates");
    assert_eq!(
        reply,
        serde_json::to_vec(&runtime_collab::WifiContinuityReply::NotFound).expect("reply")
    );
    assert_eq!(service.calls.lock().expect("calls").len(), 1);

    let other_app = delegate_wifi_continuity_command(
        &services,
        "so.tilo.browser",
        "so.tilo.browser",
        &lookup(),
    )
    .expect_err("only Settings carries Wi-Fi credentials");
    assert!(other_app.to_string().contains("certified Settings"));
    let spoofed = delegate_wifi_continuity_command(
        &services,
        WIFI_CONTINUITY_APP_ID,
        "so.tilo.browser",
        &lookup(),
    )
    .expect_err("caller claim cannot override certified route");
    assert!(spoofed.to_string().contains("certified connection route"));
    let oversized = delegate_wifi_continuity_command(
        &services,
        WIFI_CONTINUITY_APP_ID,
        WIFI_CONTINUITY_APP_ID,
        &vec![b' '; runtime_collab::MAX_WIFI_CONTINUITY_WIRE_BYTES + 1],
    )
    .expect_err("oversized command refused");
    assert!(oversized.to_string().contains("protocol bound"));
    assert_eq!(service.calls.lock().expect("calls").len(), 1);
}

#[test]
fn wifi_continuity_is_unavailable_without_a_host_service() {
    let unsupported = delegate_wifi_continuity_command(
        &HostServices::default(),
        WIFI_CONTINUITY_APP_ID,
        WIFI_CONTINUITY_APP_ID,
        &lookup(),
    )
    .expect_err("host without Wi-Fi fails explicitly");
    assert!(unsupported.to_string().contains("unsupported by this host"));
}

#[test]
fn nearby_transfer_delegates_only_the_certified_files_route() {
    let service = Arc::new(RecordingNearby::answering(
        runtime_collab::NearbyTransferReply::Accepted,
    ));
    let services = HostServices {
        nearby_transfer: Some(service.clone()),
        ..Default::default()
    };
    let context = files_context();
    let reply =
        delegate_nearby_transfer_command(&services, &context, NEARBY_TRANSFER_APP_ID, &snapshot())
            .expect("certified Files delegates");
    assert_eq!(
        reply,
        serde_json::to_vec(&runtime_collab::NearbyTransferReply::Accepted).expect("reply")
    );
    assert_eq!(
        service.calls.lock().expect("calls").as_slice(),
        &[(context, runtime_collab::NearbyTransferCommand::Snapshot)]
    );

    let other_app = NearbyTransferRequestContext::authenticated(
        41,
        "so.tilo.browser",
        "workspace-7",
        CrdtRoute::app("so.tilo.browser", "workspace-7").channel_key(),
        Some("local-person".into()),
    );
    let refused =
        delegate_nearby_transfer_command(&services, &other_app, "so.tilo.browser", &snapshot())
            .expect_err("only Files sends to nearby devices");
    assert!(refused.to_string().contains("certified Files"));
    let spoofed = delegate_nearby_transfer_command(
        &services,
        &files_context(),
        "so.tilo.browser",
        &snapshot(),
    )
    .expect_err("caller claim cannot override certified route");
    assert!(spoofed.to_string().contains("certified connection route"));
    let invalid = delegate_nearby_transfer_command(
        &services,
        &files_context(),
        NEARBY_TRANSFER_APP_ID,
        br#"{"command":"send"}"#,
    );
    assert!(invalid.is_err(), "malformed command refused");
    assert_eq!(service.calls.lock().expect("calls").len(), 1);
}

#[test]
fn airdrop_delegation_seals_person_and_rejects_application_trust_claims() {
    use runtime_collab::{NearbyTransferCommand as Command, NearbyTransferReply as Reply};
    let service = Arc::new(RecordingNearby::answering(Reply::Accepted));
    let services = HostServices {
        nearby_transfer: Some(service.clone()),
        ..Default::default()
    };
    let context = files_context();
    let command = Command::SendAirDrop {
        source_path: b"/approved/report".to_vec(),
        address: "127.0.0.1".into(),
        port: 443,
    };
    let json = serde_json::to_vec(&command).unwrap();
    delegate_nearby_transfer_command(&services, &context, NEARBY_TRANSFER_APP_ID, &json).unwrap();
    let calls = service.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0.person(), Some("local-person"));
    assert_eq!(calls[0].1, command);
    drop(calls);
    let mut spoof = serde_json::from_slice::<serde_json::Value>(&json).unwrap();
    for key in ["person", "certificate_sha256", "verified", "remember"] {
        spoof[key] = "app claim".into();
        assert!(
            delegate_nearby_transfer_command(
                &services,
                &context,
                NEARBY_TRANSFER_APP_ID,
                &serde_json::to_vec(&spoof).unwrap()
            )
            .is_err()
        );
        spoof.as_object_mut().unwrap().remove(key);
    }
    assert_eq!(
        service.calls.lock().unwrap().len(),
        1,
        "spoofed authority never reaches the service"
    );
}

#[test]
fn nearby_transfer_refuses_an_unbounded_host_reply() {
    let service = Arc::new(RecordingNearby::answering(
        runtime_collab::NearbyTransferReply::Rejected {
            reason: String::new(),
        },
    ));
    let services = HostServices {
        nearby_transfer: Some(service),
        ..Default::default()
    };
    let error = delegate_nearby_transfer_command(
        &services,
        &files_context(),
        NEARBY_TRANSFER_APP_ID,
        &snapshot(),
    )
    .expect_err("invalid reply never reaches the app");
    assert!(error.to_string().contains("invalid bounded reply"));
}

#[test]
fn only_the_certified_files_route_waits_for_nearby_changes() {
    let service = Arc::new(RecordingNearby::answering(
        runtime_collab::NearbyTransferReply::Accepted,
    ));
    let services = HostServices {
        nearby_transfer: Some(service.clone()),
        ..Default::default()
    };
    let change = nearby_transfer_change(&services, &files_context(), 4)
        .expect("Files waits on its own route");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    assert_eq!(runtime.block_on(change), 5, "the host's next change");
    assert_eq!(
        service.calls.lock().expect("calls")[0].0,
        files_context(),
        "the host learns which connection waits, for its handles"
    );

    for (app, workspace) in [
        ("so.tilo.browser", "workspace-7"),
        (NEARBY_TRANSFER_APP_ID, ""),
    ] {
        let context = NearbyTransferRequestContext::authenticated(
            41,
            app,
            workspace,
            CrdtRoute::app(app, workspace).channel_key(),
            Some("local-person".into()),
        );
        let refused = nearby_transfer_change(&services, &context, 0)
            .err()
            .expect("only the certified Files route in a workspace waits");
        assert!(refused.to_string().contains("certified Files"));
    }
    assert_eq!(
        service.calls.lock().expect("calls").len(),
        1,
        "a refused route never reaches the host"
    );
    for services in [
        HostServices::default(),
        HostServices {
            nearby_transfer: Some(Arc::new(UnannouncedNearby)),
            ..Default::default()
        },
    ] {
        let unannounced = nearby_transfer_change(&services, &files_context(), 0)
            .err()
            .expect("a host that cannot announce changes says so");
        assert_eq!(unannounced.kind, capnp::ErrorKind::Unimplemented);
    }
}

/// Speaker groups that record each wait and answer it with the next
/// revision, or (`announces: false`) cannot say when they change.
struct WaitingSpeakers {
    announces: bool,
    waits: Mutex<Vec<u64>>,
}

impl crate::services::SpeakerServices for WaitingSpeakers {
    fn command(&self, _command: runtime_collab::SpeakerCommand) -> runtime_collab::SpeakerReply {
        runtime_collab::SpeakerReply::Done { group: None }
    }

    fn changed(&self, after: u64) -> Option<crate::services::SpeakerChange> {
        self.waits.lock().expect("waits").push(after);
        self.announces
            .then(|| Box::pin(std::future::ready(after.wrapping_add(1))) as _)
    }
}

#[test]
fn only_the_certified_settings_route_waits_for_speaker_changes() {
    let service = Arc::new(WaitingSpeakers {
        announces: true,
        waits: Mutex::new(Vec::new()),
    });
    let services = HostServices {
        speakers: Some(service.clone()),
        ..Default::default()
    };
    let change =
        speaker_change(&services, "so.tilo.settings", 4).expect("Settings waits on its route");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    assert_eq!(runtime.block_on(change), 5, "the host's next change");

    for app in [NEARBY_TRANSFER_APP_ID, "so.tilo.browser", ""] {
        let refused = speaker_change(&services, app, 0)
            .err()
            .expect("only the certified Settings route waits");
        assert!(refused.to_string().contains("certified Settings"));
    }
    assert_eq!(
        *service.waits.lock().expect("waits"),
        [4],
        "a refused route never reaches the host"
    );

    for services in [
        HostServices::default(),
        HostServices {
            speakers: Some(Arc::new(WaitingSpeakers {
                announces: false,
                waits: Mutex::new(Vec::new()),
            })),
            ..Default::default()
        },
    ] {
        let unannounced = speaker_change(&services, "so.tilo.settings", 0)
            .err()
            .expect("a host that cannot announce changes says so");
        assert_eq!(unannounced.kind, capnp::ErrorKind::Unimplemented);
    }
}

#[test]
fn nearby_transfer_is_unavailable_without_a_host_service() {
    let unsupported = delegate_nearby_transfer_command(
        &HostServices::default(),
        &files_context(),
        NEARBY_TRANSFER_APP_ID,
        &snapshot(),
    )
    .expect_err("host without nearby radios fails explicitly");
    assert!(unsupported.to_string().contains("unsupported by this host"));
}
