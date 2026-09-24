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

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::PathBuf;

use tilo_relay_protocol::{
    ComputeAccelerator, DeviceMeshProfile, DeviceRole, HomePeerRegistration, MetricsSnapshot,
    PeerRelayConfig, PowerAvailability, ProcessLease, RelayConfig, ReplicationIntent,
    ReplicationResource, ServiceCapabilities, TrustPolicy, WorkspaceBinding,
    personal_continuity_workspace_id, room_workspace_id,
};

fn postcard_hex<T: serde::Serialize>(value: &T) -> String {
    hex::encode(postcard::to_stdvec(value).unwrap())
}

fn from_postcard_hex<T: serde::de::DeserializeOwned>(encoded: &str) -> T {
    postcard::from_bytes(&hex::decode(encoded).unwrap()).unwrap()
}

fn selected_resources() -> ReplicationIntent {
    ReplicationIntent::Resources(BTreeSet::from([
        ReplicationResource::MediaLibrary,
        ReplicationResource::AppRecords("so.tilo.notes".to_string()),
    ]))
}

#[test]
fn device_mesh_profile_json_encoding() {
    let profile = DeviceMeshProfile {
        device: [1; 32],
        role: DeviceRole::Hybrid,
        services: ServiceCapabilities::home_peer(),
        replication: BTreeMap::from([
            (7, ReplicationIntent::FullWorkspace),
            (9, selected_resources()),
        ]),
        filesystem_endpoints: Vec::new(),
    };
    let encoded = concat!(
        r#"{"device":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],"#,
        r#""role":"hybrid","#,
        r#""services":{"workspace_replication":true,"full_content_replication":true,"#,
        r#""inbound_relay":true,"remote_control_relay":true,"app_serving":false,"compute":false},"#,
        r#""replication":{"7":"full_workspace","#,
        r#""9":{"resources":[{"kind":"media_library"},{"kind":"app_records","id":"so.tilo.notes"}]}},"#,
        r#""filesystem_endpoints":[]}"#,
    );
    assert_eq!(serde_json::to_string(&profile).unwrap(), encoded);
    assert_eq!(
        serde_json::from_str::<DeviceMeshProfile>(encoded).unwrap(),
        profile
    );
}

#[test]
fn service_advertisement_wire_encoding() {
    // Management responses and compute worker adverts carry these fields in
    // postcard frames.
    let advert = (
        DeviceRole::AlwaysOnServer,
        ServiceCapabilities::personal_server(),
    );
    let encoded = "01010101010101";
    assert_eq!(postcard_hex(&advert), encoded);
    assert_eq!(
        from_postcard_hex::<(DeviceRole, ServiceCapabilities)>(encoded),
        advert
    );
}

#[test]
fn mesh_enum_wire_encodings() {
    let power = [
        PowerAvailability::Unknown,
        PowerAvailability::External,
        PowerAvailability::Charging(80),
        PowerAvailability::Battery(15),
    ];
    let encoded = "00010250030f";
    assert_eq!(postcard_hex(&power), encoded);
    assert_eq!(from_postcard_hex::<[PowerAvailability; 4]>(encoded), power);

    assert_eq!(postcard_hex(&ComputeAccelerator::Gpu), "00");
    assert_eq!(
        serde_json::to_string(&(ComputeAccelerator::Gpu, PowerAvailability::Battery(15))).unwrap(),
        r#"["gpu",{"battery":15}]"#
    );
}

#[test]
fn replication_intent_json_encoding() {
    let intents = vec![
        ReplicationIntent::WorkspaceOnly,
        ReplicationIntent::OnDemand,
        ReplicationIntent::FullWorkspace,
        selected_resources(),
    ];
    let encoded = concat!(
        r#"["workspace_only","on_demand","full_workspace","#,
        r#"{"resources":[{"kind":"media_library"},{"kind":"app_records","id":"so.tilo.notes"}]}]"#,
    );
    assert_eq!(serde_json::to_string(&intents).unwrap(), encoded);
    assert_eq!(
        serde_json::from_str::<Vec<ReplicationIntent>>(encoded).unwrap(),
        intents
    );
}

#[test]
fn relay_config_toml_encoding() {
    let config = RelayConfig {
        identity_root: PathBuf::from("/var/lib/tilo"),
        state_root: PathBuf::from("/var/lib/tilo-relay"),
        full_replica_status: PathBuf::from("/var/lib/tilo/full-replica/status.json"),
        home_peer_bundle: PathBuf::from("/etc/tilo/home-peer.toml"),
        iroh_listen: SocketAddr::from(([0, 0, 0, 0], 4919)),
        trust_policy: TrustPolicy::AllowList {
            users: vec!["ab".repeat(32)],
        },
        device_role: DeviceRole::AlwaysOnServer,
        services: ServiceCapabilities::home_peer(),
        workspaces: vec![
            WorkspaceBinding {
                id: u64::MAX,
                pin: true,
                replication: ReplicationIntent::FullWorkspace,
                admitted_users: vec!["cd".repeat(32)],
            },
            WorkspaceBinding {
                id: 42,
                pin: false,
                replication: selected_resources(),
                admitted_users: Vec::new(),
            },
        ],
        peer_relays: vec![PeerRelayConfig {
            iroh_node_id: "ef".repeat(32),
            label: "west".to_string(),
            priority: 100,
        }],
    };
    let encoded = r#"identity_root = "/var/lib/tilo"
state_root = "/var/lib/tilo-relay"
full_replica_status = "/var/lib/tilo/full-replica/status.json"
home_peer_bundle = "/etc/tilo/home-peer.toml"
iroh_listen = "0.0.0.0:4919"
device_role = "always_on_server"

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

[[workspace]]
id = "18446744073709551615"
pin = true
replication = "full_workspace"
admitted_users = ["cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"]

[[workspace]]
id = "42"
pin = false
admitted_users = []

[workspace.replication]
resources = [
    { kind = "media_library" },
    { kind = "app_records", id = "so.tilo.notes" },
]

[[peer_relays]]
iroh_node_id = "efefefefefefefefefefefefefefefefefefefefefefefefefefefefefefefef"
label = "west"
priority = 100
"#;
    assert_eq!(toml::to_string_pretty(&config).unwrap(), encoded);
    assert_eq!(toml::from_str::<RelayConfig>(encoded).unwrap(), config);
}

#[test]
fn trust_policy_toml_encodings() {
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
fn legacy_workspace_binding_toml_still_parses() {
    let legacy: WorkspaceBinding = toml::from_str("id = 42").unwrap();
    assert_eq!(
        legacy,
        WorkspaceBinding {
            id: 42,
            pin: true,
            replication: ReplicationIntent::WorkspaceOnly,
            admitted_users: Vec::new(),
        }
    );
}

#[test]
fn home_peer_registration_json_encoding() {
    let registration = HomePeerRegistration {
        schema_version: HomePeerRegistration::CURRENT_SCHEMA,
        device_node_id: "11".repeat(32),
        relay_service_node_id: "22".repeat(32),
        relay_endpoint: None,
        label: "Attic server".to_string(),
        user_public_key: "33".repeat(32),
        device_cert: None,
        relay_workspace_ids: vec![7, u64::MAX],
    };
    let encoded = concat!(
        r#"{"schema_version":3,"#,
        r#""device_node_id":"1111111111111111111111111111111111111111111111111111111111111111","#,
        r#""relay_service_node_id":"2222222222222222222222222222222222222222222222222222222222222222","#,
        r#""relay_endpoint":null,"label":"Attic server","#,
        r#""user_public_key":"3333333333333333333333333333333333333333333333333333333333333333","#,
        r#""device_cert":null,"relay_workspace_ids":[7,18446744073709551615]}"#,
    );
    assert_eq!(serde_json::to_string(&registration).unwrap(), encoded);
    assert_eq!(
        serde_json::from_str::<HomePeerRegistration>(encoded).unwrap(),
        registration
    );

    let legacy = format!(
        r#"{{"device_node_id":"{0}","relay_service_node_id":"{0}","label":"old","user_public_key":"{0}","relay_workspace_ids":[]}}"#,
        "44".repeat(32)
    );
    let legacy: HomePeerRegistration = serde_json::from_str(&legacy).unwrap();
    assert_eq!(legacy.schema_version, 1);
    assert_eq!(legacy.relay_endpoint, None);
    assert_eq!(legacy.device_cert, None);
}

#[test]
fn metrics_snapshot_json_encoding() {
    let snapshot = MetricsSnapshot {
        conns_accepted: 1,
        conns_authed: 2,
        auth_rejected: 3,
        deltas_received: 4,
        bytes_queued: 5,
        deltas_fanned_out: 6,
    };
    let encoded = r#"{
  "conns_accepted": 1,
  "conns_authed": 2,
  "auth_rejected": 3,
  "deltas_received": 4,
  "bytes_queued": 5,
  "deltas_fanned_out": 6
}"#;
    assert_eq!(
        String::from_utf8(serde_json::to_vec_pretty(&snapshot).unwrap()).unwrap(),
        encoded
    );
    assert_eq!(
        serde_json::from_str::<MetricsSnapshot>(encoded).unwrap(),
        snapshot
    );
}

#[test]
fn process_lease_json_encoding() {
    let lease = ProcessLease::from_parts(4242, 987_654_321);
    let encoded = r#"{"pid":4242,"start_time":987654321}"#;
    assert_eq!(serde_json::to_string(&lease).unwrap(), encoded);
    assert_eq!(
        serde_json::from_str::<ProcessLease>(encoded).unwrap(),
        lease
    );
}

#[test]
fn relay_shard_ids_are_stable() {
    assert_eq!(
        room_workspace_id("0b7e7dfa-3ad5-4b43-9d6c-5f0d7c1f6a11").unwrap(),
        3_029_697_355_180_468_570
    );
    // Parsing normalises the textual form before hashing.
    assert_eq!(
        room_workspace_id("0B7E7DFA-3AD5-4B43-9D6C-5F0D7C1F6A11").unwrap(),
        3_029_697_355_180_468_570
    );
    assert!(room_workspace_id("not a room").is_err());
    assert_eq!(
        personal_continuity_workspace_id(&[5; 32]),
        431_687_783_281_078_992
    );
}
