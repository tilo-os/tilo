use super::*;
use crate::apps::BrokerErrorKind;
use crate::apps::services::WifiContinuityServices;

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

fn lookup() -> Vec<u8> {
    serde_json::to_vec(&runtime_collab::WifiContinuityCommand::Lookup {
        ssid: "home".into(),
        security: runtime_collab::PortableWifiSecurity::Wpa2,
    })
    .expect("encode lookup")
}

tilo_test_env::scenario! {
    fn device_continuity_delegates_only_certified_routes;

    phase network_wake_uses_only_the_certified_settings_capability;
    {
        let service = Arc::new(RecordingWifi::default());
        let services = ProcessServices {
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

    phase wifi_continuity_delegates_only_the_certified_settings_route;
    {
        let service = Arc::new(RecordingWifi::default());
        let services = ProcessServices {
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

    phase wifi_continuity_is_unavailable_without_a_host_service;
    {
        let unsupported = delegate_wifi_continuity_command(
            &ProcessServices::default(),
            WIFI_CONTINUITY_APP_ID,
            WIFI_CONTINUITY_APP_ID,
            &lookup(),
        )
        .expect_err("host without Wi-Fi fails explicitly");
        assert!(unsupported.to_string().contains("unsupported by this host"));
    }

    phase only_the_certified_settings_route_waits_for_speaker_changes;
    {
        let service = Arc::new(WaitingSpeakers {
            announces: true,
            waits: Mutex::new(Vec::new()),
        });
        let services = ProcessServices {
            speakers: Some(service.clone()),
            ..Default::default()
        };
        let change =
            speaker_change(&services, "so.tilo.settings", 4).expect("Settings waits on its route");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        assert_eq!(runtime.block_on(change), 5, "the host's next change");

        for app in [tilo_app_id::apps::FILE_EXPLORER, "so.tilo.browser", ""] {
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
            ProcessServices::default(),
            ProcessServices {
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
            assert_eq!(unannounced.kind, BrokerErrorKind::Unimplemented);
        }
    }

}

/// Speaker groups that record each wait and answer it with the next
/// revision, or (`announces: false`) cannot say when they change.
struct WaitingSpeakers {
    announces: bool,
    waits: Mutex<Vec<u64>>,
}

impl crate::apps::services::SpeakerServices for WaitingSpeakers {
    fn command(&self, _command: runtime_collab::SpeakerCommand) -> runtime_collab::SpeakerReply {
        runtime_collab::SpeakerReply::Done { group: None }
    }

    fn changed(&self, after: u64) -> Option<crate::apps::services::SpeakerChange> {
        self.waits.lock().expect("waits").push(after);
        self.announces
            .then(|| Box::pin(std::future::ready(after.wrapping_add(1))) as _)
    }
}
