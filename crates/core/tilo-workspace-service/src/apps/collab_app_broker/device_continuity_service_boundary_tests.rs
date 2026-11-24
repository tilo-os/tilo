use super::*;
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
}
