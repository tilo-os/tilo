//! Encoded examples of every serialized relay value type.
//!
//! These records are written to disk by one build and read by another, and
//! the mesh types travel on the relay wire, so their encodings may only change
//! through an explicit compatibility decision. Each test pins the exact bytes
//! or text produced today and decodes them back to the same value.

#![allow(
    clippy::unwrap_used,
    reason = "in a test an unwrap is the assertion, not a hazard"
)]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use collab_types::WorkspaceId;
use tilo_relay_protocol::metrics::WorkspaceEgress;
use tilo_relay_protocol::sealed_invitations as SealedInvitations;
use tilo_relay_protocol::{
    ComputeAccelerator, DeviceMeshProfile, HomePeerRegistration, MetricsSnapshot,
    PowerAvailability, ProcessLease, RelayConfig, ServiceCapabilities, TrustPolicy, WorkSetting,
};

fn postcard_hex<T: serde::Serialize>(value: &T) -> String {
    hex::encode(postcard::to_stdvec(value).unwrap())
}

fn from_postcard_hex<T: serde::de::DeserializeOwned>(encoded: &str) -> T {
    postcard::from_bytes(&hex::decode(encoded).unwrap()).unwrap()
}

/// Pins `encoded` as the exact JSON of `value`, and decodes it back.
fn assert_json<T>(value: &T, encoded: &str)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    assert_eq!(serde_json::to_string(value).unwrap(), encoded);
    assert_eq!(&serde_json::from_str::<T>(encoded).unwrap(), value);
}

/// Pins `encoded` as the exact postcard hex of `value`, and decodes it back.
fn assert_postcard<T>(value: &T, encoded: &str)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    assert_eq!(postcard_hex(value), encoded);
    assert_eq!(&from_postcard_hex::<T>(encoded), value);
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn json_encodings_are_pinned() {
    // Device mesh profile.
    let profile = DeviceMeshProfile {
        device: [1; 32],
        services: ServiceCapabilities::home_peer(),
        filesystem_endpoints: Vec::new(),
    };
    assert_json(
        &profile,
        concat!(
            r#"{"device":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],"#,
            r#""services":{"workspace_replication":true,"full_content_replication":true,"#,
            r#""inbound_relay":true,"remote_control_relay":true,"app_serving":false,"compute":false},"#,
            r#""filesystem_endpoints":[]}"#,
        ),
    );

    // Mesh enums.
    assert_eq!(
        serde_json::to_string(&(ComputeAccelerator::Gpu, PowerAvailability::Battery(15))).unwrap(),
        r#"["gpu",{"battery":15}]"#
    );

    // The work a device takes.
    assert_json(
        &WorkSetting::own_person(),
        concat!(
            r#"{"kinds":{"actions":true,"indexing":true,"model_calls":true,"builds":true},"#,
            r#""from":"own_devices","#,
            r#""when":{"charging_only":false,"free_cpu_percent":20,"free_memory_percent":20}}"#,
        ),
    );

    // Home peer registration.
    assert_json(
        &HomePeerRegistration {
            schema_version: HomePeerRegistration::CURRENT_SCHEMA,
            device_node_id: "11".repeat(32),
            relay_service_node_id: "22".repeat(32),
            relay_endpoint: None,
            label: "Attic server".to_string(),
            user_public_key: "33".repeat(32),
            device_cert: None,
            relay_workspace_ids: vec![WorkspaceId::from_u128(7), WorkspaceId::from_u128(8)],
        },
        concat!(
            r#"{"schema_version":3,"#,
            r#""device_node_id":"1111111111111111111111111111111111111111111111111111111111111111","#,
            r#""relay_service_node_id":"2222222222222222222222222222222222222222222222222222222222222222","#,
            r#""relay_endpoint":null,"label":"Attic server","#,
            r#""user_public_key":"3333333333333333333333333333333333333333333333333333333333333333","#,
            r#""device_cert":null,"relay_workspace_ids":["00000000-0000-0000-0000-000000000007","00000000-0000-0000-0000-000000000008"]}"#,
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
  "rooms": {
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
    // A snapshot a relay wrote before the egress counters still reads, with
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
            workspaces: BTreeMap::new(),
            ..snapshot
        }
    );

    assert_json(
        &ProcessLease::from_parts(4242, 987_654_321),
        r#"{"pid":4242,"start_time":987654321}"#,
    );

    // The shelf a site keeps sealed invitations on.
    let stored = SealedInvitations::Stored {
        sealed: "c2VhbGVk".to_string(),
        expires_at_ns: 1_700_000_000_000_000_000,
        delete: "ZGVsZXRl".to_string(),
        host: Some("https://host.example".to_string()),
    };
    assert_json(
        &stored,
        r#"{"sealed":"c2VhbGVk","expiresAtNs":1700000000000000000,"delete":"ZGVsZXRl","host":"https://host.example"}"#,
    );
    // What a build that named no host left, and what it answered with, still
    // reads.
    assert_json(
        &SealedInvitations::Stored {
            host: None,
            ..stored
        },
        r#"{"sealed":"c2VhbGVk","expiresAtNs":1700000000000000000,"delete":"ZGVsZXRl"}"#,
    );
    assert_json(
        &SealedInvitations::Fetched {
            sealed: "c2VhbGVk".to_string(),
            host: Some("https://host.example".to_string()),
        },
        r#"{"sealed":"c2VhbGVk","host":"https://host.example"}"#,
    );
    assert_json(
        &SealedInvitations::Fetched {
            sealed: "c2VhbGVk".to_string(),
            host: None,
        },
        r#"{"sealed":"c2VhbGVk"}"#,
    );
    assert_json(
        &SealedInvitations::TakeBack {
            delete: "ZGVsZXRl".to_string(),
        },
        r#"{"delete":"ZGVsZXRl"}"#,
    );
    assert!(serde_json::from_str::<SealedInvitations::Stored>(r#"{"sealed":"c2VhbGVk","expiresAtNs":1700000000000000000,"delete":"ZGVsZXRl","extra":1}"#).is_err());
}

#[test]
fn postcard_wire_encodings_and_shard_ids_are_pinned() {
    // Management responses and compute worker adverts carry these fields in
    // postcard frames.
    assert_postcard(&ServiceCapabilities::personal_server(), "0101010101");

    // Mesh enums.
    assert_postcard(
        &[
            PowerAvailability::Unknown,
            PowerAvailability::External,
            PowerAvailability::Charging(80),
            PowerAvailability::Battery(15),
        ],
        "00010250030f",
    );
    assert_eq!(postcard_hex(&ComputeAccelerator::Gpu), "00");

    // Mesh profiles may travel in postcard frames as well as JSON.
    let profile = DeviceMeshProfile {
        device: [1; 32],
        services: ServiceCapabilities::home_peer(),
        filesystem_endpoints: Vec::new(),
    };
    let bytes = postcard::to_stdvec(&profile).unwrap();
    assert_eq!(
        postcard::from_bytes::<DeviceMeshProfile>(&bytes).unwrap(),
        profile
    );
}

#[test]
fn toml_encodings_are_pinned() {
    let config = RelayConfig {
        identity_root: PathBuf::from("/var/lib/tilo"),
        state_root: PathBuf::from("/var/lib/tilo-relay"),
        full_replica_status: PathBuf::from("/var/lib/tilo/full-replica/status.json"),
        home_peer_bundle: PathBuf::from("/etc/tilo/home-peer.toml"),
        iroh_listen: SocketAddr::from(([0, 0, 0, 0], 4919)),
        packet_relay: None,
        packet_relay_url: None,
        trust_policy: TrustPolicy::AllowList {
            users: vec!["ab".repeat(32)],
        },
        work: WorkSetting::own_person(),
        services: ServiceCapabilities::home_peer(),
        web_entry: None,
        member_limits: None,
        max_workspace_grants: None,
    };
    let encoded = r#"identity_root = "/var/lib/tilo"
state_root = "/var/lib/tilo-relay"
full_replica_status = "/var/lib/tilo/full-replica/status.json"
home_peer_bundle = "/etc/tilo/home-peer.toml"
iroh_listen = "0.0.0.0:4919"

[work]
from = "own_devices"

[work.kinds]
actions = true
indexing = true
model_calls = true
builds = true

[work.when]
charging_only = false
free_cpu_percent = 20
free_memory_percent = 20

[trust_policy]
kind = "allow_list"
users = ["abababababababababababababababababababababababababababababababab"]

[services]
workspace_replication = true
full_content_replication = true
inbound_relay = true
remote_control_relay = true
app_serving = false
compute = false
"#;
    assert_eq!(toml::to_string_pretty(&config).unwrap(), encoded);
    assert_eq!(toml::from_str::<RelayConfig>(encoded).unwrap(), config);

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

#[test]
fn certification_openers_keep_their_tags() {
    use tilo_relay_protocol::auth::Hello;
    use tilo_relay_protocol::certify::{ApproverRequest, CertifyOpen, MemberApproval};
    use tilo_user_identity::{
        CERTIFICATION_SCHEMA_VERSION, CertificationRequest, DeviceCapabilities, DeviceCert,
    };

    // The certification protocol tells its connections apart by the first
    // frame's variant: a device asking to be certified (0), a certified
    // approver (1), a member's root asking one command (2). Openers are only
    // ever added after these, so a host reads each one's tag as it always
    // has, and a host that predates an opener refuses it.
    let request = CertificationRequest {
        version: CERTIFICATION_SCHEMA_VERSION,
        user_public_key: [1; 32],
        device_public_key: [2; 32],
        device_label: "Phone".into(),
        requested: DeviceCapabilities::member(),
        nonce: [3; 16],
        invite: [0; 16],
        issued_at_ns: 4,
        expires_at_ns: 5,
        signature: [6; 64],
    };
    assert!(postcard_hex(&CertifyOpen::Request(request)).starts_with("00"));
    let cert = DeviceCert {
        user_public_key: [1; 32],
        device_public_key: [2; 32],
        device_label: "Laptop".into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    assert!(postcard_hex(&CertifyOpen::Approver(Hello::new(cert, [7; 32]))).starts_with("01"));

    // A member's approval, whole: the tag, the root, the time, the command
    // and the root's signature as 64 bytes.
    let member = MemberApproval {
        user_public_key: [1; 32],
        issued_at_ns: 2,
        request: ApproverRequest::ListPending,
        signature: [3; 64],
    };
    let encoded = format!("02{}0200{}{}", "01".repeat(32), "40", "03".repeat(64));
    assert_eq!(postcard_hex(&CertifyOpen::Member(member.clone())), encoded);
    let CertifyOpen::Member(decoded) = from_postcard_hex::<CertifyOpen>(&encoded) else {
        panic!("a member's opener decodes as one");
    };
    assert_eq!(decoded, member);
}
