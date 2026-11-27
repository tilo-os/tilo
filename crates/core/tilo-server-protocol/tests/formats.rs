//! Encoded examples of every serialized server value type.
//!
//! These records are written to disk by one build and read by another, so
//! their encodings may only change through an explicit compatibility decision.
//! Each test pins the exact text produced today and decodes it back to the same
//! value.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use collab_types::WorkspaceId;
use tilo_server_protocol::config::{ApplePushKey, WebTls};
use tilo_server_protocol::metrics::WorkspaceEgress;
use tilo_server_protocol::{MetricsSnapshot, ServerConfig, TrustPolicy, WorkSetting};

/// Pins `encoded` as the exact JSON of `value`, and decodes it back.
fn assert_json<T>(value: &T, encoded: &str)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    assert_eq!(serde_json::to_string(value).unwrap(), encoded);
    assert_eq!(&serde_json::from_str::<T>(encoded).unwrap(), value);
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn json_encodings_are_pinned() {
    // The work a device takes.
    assert_json(
        &WorkSetting::own_person(),
        concat!(
            r#"{"kinds":{"actions":true,"indexing":true,"model_calls":true,"builds":true},"#,
            r#""from":"own_devices","#,
            r#""when":{"charging_only":false,"free_cpu_percent":0,"free_memory_percent":0}}"#,
        ),
    );

    // Metrics snapshot, written pretty-printed.
    let snapshot = MetricsSnapshot {
        conns_accepted: 1,
        conns_authed: 2,
        auth_rejected: 3,
        deltas_received: 4,
        bytes_queued: 5,
        deltas_fanned_out: 6,
        subscribes: 7,
        replayed_retained: 8,
        replayed_pending: 9,
        subscriber_dropped: 10,
        subscriber_dropped_backpressure: 11,
        bytes_sent: 12,
        bytes_sent_live: 13,
        documents_read: 17,
        sessions_open: 0,
        workspaces: BTreeMap::from([(
            WorkspaceId::from_u128(14),
            WorkspaceEgress {
                bytes_sent: 15,
                bytes_sent_live: 16,
            },
        )]),
    };
    let encoded = r#"{
  "conns_accepted": 1,
  "conns_authed": 2,
  "auth_rejected": 3,
  "deltas_received": 4,
  "bytes_queued": 5,
  "deltas_fanned_out": 6,
  "subscribes": 7,
  "replayed_retained": 8,
  "replayed_pending": 9,
  "subscriber_dropped": 10,
  "subscriber_dropped_backpressure": 11,
  "bytes_sent": 12,
  "bytes_sent_live": 13,
  "documents_read": 17,
  "workspaces": {
    "00000000-0000-0000-0000-00000000000e": {
      "bytes_sent": 15,
      "bytes_sent_live": 16
    }
  }
}"#;
    assert_eq!(
        String::from_utf8(serde_json::to_vec_pretty(&snapshot).unwrap()).unwrap(),
        encoded
    );
    assert_eq!(
        serde_json::from_str::<MetricsSnapshot>(encoded).unwrap(),
        snapshot
    );
    // A snapshot a server wrote before the egress counters still reads, with
    // no egress.
    let before_egress = r#"{
  "conns_accepted": 1,
  "conns_authed": 2,
  "auth_rejected": 3,
  "deltas_received": 4,
  "bytes_queued": 5,
  "deltas_fanned_out": 6,
  "subscribes": 7,
  "replayed_retained": 8,
  "replayed_pending": 9,
  "subscriber_dropped": 10,
  "subscriber_dropped_backpressure": 11
}"#;
    assert_eq!(
        serde_json::from_str::<MetricsSnapshot>(before_egress).unwrap(),
        MetricsSnapshot {
            bytes_sent: 0,
            bytes_sent_live: 0,
            documents_read: 0,
            workspaces: BTreeMap::new(),
            ..snapshot
        }
    );

    // An invitation a workspace keeps, asked for by the secret its link carries.
    let asked = tilo_server_protocol::web::KeptInvitation {
        workspace: "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71".to_string(),
        secret: "c2VjcmV0".to_string(),
    };
    assert_json(
        &asked,
        r#"{"workspace":"6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71","secret":"c2VjcmV0"}"#,
    );
    assert_json(
        &tilo_server_protocol::web::OpenedInvitation {
            invitation: "aW52aXRhdGlvbg".to_string(),
        },
        r#"{"invitation":"aW52aXRhdGlvbg"}"#,
    );
    assert!(
        serde_json::from_str::<tilo_server_protocol::web::KeptInvitation>(
            r#"{"workspace":"w","secret":"s","extra":1}"#
        )
        .is_err()
    );
}

tilo_test_env::scenario! {
    fn toml_encodings_are_pinned;

    phase toml_encodings_are_pinned;
    {
        let config = ServerConfig {
            identity_root: PathBuf::from("/var/lib/tilo"),
            state_root: PathBuf::from("/var/lib/tilo-server"),
            iroh_listen: SocketAddr::from(([0, 0, 0, 0], 4919)),
            packet_relay: None,
            packet_relay_url: None,
            kept: Vec::new(),
            trust_policy: TrustPolicy::AllowList {
                users: vec!["ab".repeat(32)],
            },
            work: WorkSetting::own_person(),
            rented: false,
            app_serving: false,
            web_entry: None,
            web_listen: Some(SocketAddr::from(([0, 0, 0, 0], 8443))),
            web_tls: WebTls::Local,
            max_workspace_grants: None,
            relay: None,
            apple_push: Some(ApplePushKey {
                key_file: PathBuf::from("/var/lib/tilo/apple-push.p8"),
                key_id: "ABC123DEFG".to_owned(),
                team_id: "TEAM123456".to_owned(),
                topics: vec!["so.tilo.phone".to_owned()],
            }),
        };
        let encoded = r#"identity_root = "/var/lib/tilo"
state_root = "/var/lib/tilo-server"
iroh_listen = "0.0.0.0:4919"
web_listen = "0.0.0.0:8443"
web_tls = "local"

[trust_policy]
kind = "allow_list"
users = ["abababababababababababababababababababababababababababababababab"]

[work]
from = "own_devices"

[work.kinds]
actions = true
indexing = true
model_calls = true
builds = true

[work.when]
charging_only = false
free_cpu_percent = 0
free_memory_percent = 0

[apple_push]
key_file = "/var/lib/tilo/apple-push.p8"
key_id = "ABC123DEFG"
team_id = "TEAM123456"
topics = ["so.tilo.phone"]
"#;
        assert_eq!(toml::to_string_pretty(&config).unwrap(), encoded);
        assert_eq!(toml::from_str::<ServerConfig>(encoded).unwrap(), config);

        // Trust policies.
        for (policy, encoded) in [
            (
                TrustPolicy::AllowMatchingUser,
                "kind = \"allow_matching_user\"\n",
            ),
            (TrustPolicy::AllowAny, "kind = \"allow_any\"\n"),
        ] {
            assert_eq!(toml::to_string(&policy).unwrap(), encoded);
            assert_eq!(toml::from_str::<TrustPolicy>(encoded).unwrap(), policy);
        }
    }
}
