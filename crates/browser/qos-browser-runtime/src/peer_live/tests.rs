use super::*;
use ed25519_dalek::SigningKey;
use tilo_relay::delivery_v3::live_wire::{
    GRANT_LIFETIME_NS, LIVE_WEBTRANSPORT_PATH, LiveGrantMetadata, LiveNamespace,
};

const ORIGIN: &str = "https://127.0.0.1:19343";
const ROOM: &str = "4f1e5c3a-0000-4000-8000-000000000001";
const NOW: u64 = 1_700_000_000_000_000_000;

fn both() -> LiveDirections {
    LiveDirections {
        publish: true,
        subscribe: true,
    }
}
fn binding(host: &SigningKey, participation: [u8; 32], directions: LiveDirections) -> GrantBinding {
    GrantBinding {
        origin: ORIGIN.into(),
        root: [3; 32],
        host: host.verifying_key().to_bytes(),
        request: grant_request([7; 32], ROOM, participation, directions).unwrap(),
    }
}
fn issue(host: &SigningKey, binding: &GrantBinding, issued: u64, lifetime: u64) -> Vec<u8> {
    issue_boot(host, binding, issued, lifetime, [5; 32])
}
fn issue_boot(
    host: &SigningKey,
    binding: &GrantBinding,
    issued: u64,
    lifetime: u64,
    boot: [u8; 32],
) -> Vec<u8> {
    let metadata = LiveGrantMetadata {
        root: binding.root,
        namespace: LiveNamespace::new(&binding.request.room, binding.request.device),
        request: binding.request.clone(),
        membership_generation: 4,
        audience: format!("{ORIGIN}{LIVE_WEBTRANSPORT_PATH}"),
        host: host.verifying_key().to_bytes(),
        host_boot: boot,
        issued_at_ns: issued,
        expires_at_ns: issued + lifetime,
    };
    SignedLiveGrant::sign(metadata, host)
        .unwrap()
        .encode()
        .unwrap()
}

#[test]
fn grant_request_is_bounded_and_needs_a_direction() {
    assert!(grant_request([7; 32], ROOM, [9; 32], both()).is_ok());
    assert!(
        grant_request(
            [7; 32],
            ROOM,
            [9; 32],
            LiveDirections {
                publish: false,
                subscribe: false
            }
        )
        .is_err()
    );
    assert!(grant_request([0; 32], ROOM, [9; 32], both()).is_err());
    assert!(grant_request([7; 32], "", [9; 32], both()).is_err());
    assert!(grant_request([7; 32], &"r".repeat(129), [9; 32], both()).is_err());
    let encoded = grant_request([7; 32], ROOM, [9; 32], both())
        .unwrap()
        .encode()
        .unwrap();
    assert_eq!(
        LiveGrantRequest::decode(&encoded).unwrap().participation_id,
        [9; 32]
    );
}

#[test]
fn verified_grant_projects_endpoint_expiry_and_renewal() {
    let host = SigningKey::from_bytes(&[1; 32]);
    let b = binding(&host, [9; 32], both());
    let bytes = issue(&host, &b, NOW, GRANT_LIFETIME_NS);
    let verified = verify_grant(&b, &bytes, NOW + 1_000_000_000).unwrap();
    assert_eq!(verified.endpoint, format!("{ORIGIN}/moq/v21/live"));
    assert_eq!(verified.bytes, bytes);
    assert_eq!(verified.expires_in_ns, GRANT_LIFETIME_NS - 1_000_000_000);
    // Renews with a third of the remaining lifetime (>= 10 s) left.
    assert_eq!(
        verified.renew_in_ns,
        verified.expires_in_ns - verified.expires_in_ns / 3
    );
    // Nearly expired: renew immediately.
    let late = verify_grant(&b, &bytes, NOW + GRANT_LIFETIME_NS - 2_000_000_000).unwrap();
    assert_eq!(late.renew_in_ns, 0);
}

#[test]
fn grant_verification_tolerates_small_clock_skew_only() {
    let host = SigningKey::from_bytes(&[1; 32]);
    let b = binding(&host, [9; 32], both());
    let bytes = issue(&host, &b, NOW, GRANT_LIFETIME_NS);
    assert!(
        verify_grant(&b, &bytes, NOW - 2_000_000_000).is_ok(),
        "browser 2 s behind host"
    );
    assert!(
        verify_grant(&b, &bytes, NOW - 6_000_000_000).is_err(),
        "browser far behind host"
    );
    assert!(
        verify_grant(&b, &bytes, NOW + GRANT_LIFETIME_NS).is_err(),
        "expired"
    );
}

#[test]
fn grant_verification_rejects_foreign_or_widened_bindings() {
    let host = SigningKey::from_bytes(&[1; 32]);
    let other = SigningKey::from_bytes(&[2; 32]);
    let b = binding(&host, [9; 32], both());
    // Foreign signer.
    let forged = issue(
        &other,
        &binding(&other, [9; 32], both()),
        NOW,
        GRANT_LIFETIME_NS,
    );
    assert!(verify_grant(&b, &forged, NOW).is_err());
    // Different participation.
    let replay = issue(
        &host,
        &binding(&host, [8; 32], both()),
        NOW,
        GRANT_LIFETIME_NS,
    );
    assert!(verify_grant(&b, &replay, NOW).is_err());
    // Widened directions: asked subscribe-only, grant says publish.
    let viewer = binding(
        &host,
        [9; 32],
        LiveDirections {
            publish: false,
            subscribe: true,
        },
    );
    let widened = issue(&host, &b, NOW, GRANT_LIFETIME_NS);
    assert!(verify_grant(&viewer, &widened, NOW).is_err());
    // Narrowed directions are accepted.
    let narrowed = issue(&host, &viewer, NOW, GRANT_LIFETIME_NS);
    assert!(verify_grant(&b, &narrowed, NOW).is_ok());
    // Wrong root or origin.
    let mut wrong_root = b.clone();
    wrong_root.root = [4; 32];
    assert!(verify_grant(&wrong_root, &issue(&host, &b, NOW, GRANT_LIFETIME_NS), NOW).is_err());
    let mut wrong_origin = b.clone();
    wrong_origin.origin = "https://127.0.0.1:1".into();
    assert!(
        verify_grant(
            &wrong_origin,
            &issue(&host, &b, NOW, GRANT_LIFETIME_NS),
            NOW
        )
        .is_err()
    );
    // Tampered signature bytes.
    let mut tampered = issue(&host, &b, NOW, GRANT_LIFETIME_NS);
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    assert!(verify_grant(&b, &tampered, NOW).is_err());
}

#[test]
fn custody_renews_within_participation_and_drops_on_revocation() {
    let host = SigningKey::from_bytes(&[1; 32]);
    let mut custody = LiveCustody::default();
    assert!(
        custody
            .begin(|p| Ok(binding(&host, p, both())), false, || Ok([9; 32]))
            .is_err(),
        "renew needs a participation"
    );
    let (first, b1) = custody
        .begin(|p| Ok(binding(&host, p, both())), true, || Ok([9; 32]))
        .unwrap();
    let grant = custody
        .complete(first, &issue(&host, &b1, NOW, GRANT_LIFETIME_NS), NOW)
        .unwrap();
    assert!(custody.current(NOW).is_some());
    assert!(
        custody
            .complete(first, &issue(&host, &b1, NOW, GRANT_LIFETIME_NS), NOW)
            .is_err(),
        "consumed"
    );
    // Renewal reuses the participation id and may not regress expiry.
    let (renew, b2) = custody
        .begin(|p| Ok(binding(&host, p, both())), false, || unreachable!())
        .unwrap();
    assert_eq!(b2.request.participation_id, [9; 32]);
    let later = custody
        .complete(
            renew,
            &issue(&host, &b2, NOW + 20_000_000_000, GRANT_LIFETIME_NS),
            NOW + 20_000_000_000,
        )
        .unwrap();
    assert!(later.grant.metadata.expires_at_ns > grant.grant.metadata.expires_at_ns);
    let (stale, b3) = custody
        .begin(|p| Ok(binding(&host, p, both())), false, || unreachable!())
        .unwrap();
    assert!(
        custody
            .complete(
                stale,
                &issue(&host, &b3, NOW, GRANT_LIFETIME_NS),
                NOW + 21_000_000_000
            )
            .is_err(),
        "regressed expiry"
    );
    // A fresh participation supersedes pending renewals.
    let (old, b4) = custody
        .begin(|p| Ok(binding(&host, p, both())), false, || unreachable!())
        .unwrap();
    let (fresh, b5) = custody
        .begin(|p| Ok(binding(&host, p, both())), true, || Ok([10; 32]))
        .unwrap();
    assert!(
        custody
            .complete(old, &issue(&host, &b4, NOW, GRANT_LIFETIME_NS), NOW)
            .is_err()
    );
    custody
        .complete(fresh, &issue(&host, &b5, NOW, GRANT_LIFETIME_NS), NOW)
        .unwrap();
    custody.revoke();
    assert!(custody.current(NOW).is_none());
    assert!(
        custody
            .begin(|p| Ok(binding(&host, p, both())), false, || unreachable!())
            .is_err()
    );
}

#[test]
fn custody_bounds_outstanding_requests() {
    let host = SigningKey::from_bytes(&[1; 32]);
    let mut custody = LiveCustody::default();
    custody
        .begin(|p| Ok(binding(&host, p, both())), true, || Ok([9; 32]))
        .unwrap();
    custody
        .begin(|p| Ok(binding(&host, p, both())), false, || unreachable!())
        .unwrap();
    assert!(
        custody
            .begin(|p| Ok(binding(&host, p, both())), false, || unreachable!())
            .is_err()
    );
}

#[test]
fn digest_window_deduplicates_and_is_bounded() {
    let mut window = DigestWindow::default();
    assert!(window.insert([1; 32]));
    assert!(!window.insert([1; 32]));
    for i in 0..ADMITTED_WINDOW as u32 {
        let mut d = [0; 32];
        d[..4].copy_from_slice(&(i + 10).to_le_bytes());
        window.insert(d);
    }
    assert!(!window.contains(&[1; 32]), "oldest evicted");
    assert_eq!(window.order.len(), ADMITTED_WINDOW);
}

#[test]
fn presence_json_round_trips_canonically() {
    let json = r#"{"seq":42,"sentAtMs":1700000000123,"focus":{"app":"so.tilo.notes","record":"n1"},"pointer":[10.5,-3.25],"selection":{"anchor":"0a0b","head":"0c"},"drag":{"object":{"record":null,"id":"77696e"},"phase":"move","position":[1.0,2.0],"size":[300.0,200.0]}}"#;
    let bytes = encode_presence(json).unwrap();
    assert!(bytes.len() <= live_wire::MAX_DATAGRAM_FRAME_BYTES);
    let back = decode_presence(&bytes).unwrap();
    let a: serde_json::Value = serde_json::from_str(json).unwrap();
    let b: serde_json::Value = serde_json::from_str(&back).unwrap();
    assert_eq!(a, b);
    assert_eq!(encode_presence(&back).unwrap(), bytes);
    // Minimal presence: absent optional fields.
    let minimal = encode_presence(r#"{"seq":1,"sentAtMs":2}"#).unwrap();
    assert!(
        decode_presence(&minimal)
            .unwrap()
            .contains("\"pointer\":null")
    );
}

#[test]
fn presence_json_rejects_invalid_fields() {
    assert!(encode_presence(r#"{"seq":1,"sentAtMs":2,"extra":1}"#).is_err());
    assert!(
        encode_presence(r#"{"seq":1,"sentAtMs":2,"selection":{"anchor":"","head":"00"}}"#).is_err()
    );
    assert!(
        encode_presence(r#"{"seq":1,"sentAtMs":2,"selection":{"anchor":"0g","head":"00"}}"#)
            .is_err()
    );
    let long = "00".repeat(65);
    assert!(
        encode_presence(&format!(
            r#"{{"seq":1,"sentAtMs":2,"selection":{{"anchor":"{long}","head":"00"}}}}"#
        ))
        .is_err()
    );
    assert!(encode_presence(r#"{"seq":1,"sentAtMs":2,"drag":{"object":{"record":null,"id":"01"},"phase":"move","position":[0,0],"size":[-1,2]}}"#).is_err());
    assert!(encode_presence(r#"{"seq":1,"sentAtMs":2,"focus":{"app":"","record":"r"}}"#).is_err());
    assert!(decode_presence(&[0xff; 4]).is_err());
}

#[test]
fn preview_and_ops_projection() {
    let json = r#"{"seq":3,"record":{"app":"so.tilo.word-processor","record":"doc-1"},"ops":[{"kind":"insert","at":"01","text":"hi"},{"kind":"delete","at":"02","len":2}]}"#;
    let bytes = encode_preview(json).unwrap();
    let decoded: serde_json::Value = serde_json::from_str(&decode_ops(&bytes).unwrap()).unwrap();
    assert_eq!(decoded["kind"], "preview");
    assert_eq!(decoded["seq"], 3);
    assert_eq!(decoded["ops"][0]["text"], "hi");
    assert_eq!(decoded["ops"][1]["len"], 2);
    assert_eq!(
        decoded["track"],
        ops_track("so.tilo.word-processor", "doc-1").unwrap()
    );
    assert!(signed_change(&bytes).is_err());
    assert!(encode_preview(r#"{"seq":3,"record":{"app":"a","record":"r"},"ops":[]}"#).is_err());
    assert!(encode_preview(r#"{"seq":3,"record":{"app":"a","record":"r"},"ops":[{"kind":"delete","at":"01","len":0}]}"#).is_err());
}

#[test]
fn signed_change_projection_hides_canonical_bytes() {
    let record = LiveRecord {
        app: "so.tilo.notes".into(),
        record: "n1".into(),
    };
    let canonical = vec![1, 2, 3, 4];
    let digest = tilo_relay::delivery_v3::state::canonical_item_digest(&canonical);
    let bytes = OpsFrame::SignedChange {
        record: record.clone(),
        digest,
        canonical: canonical.clone(),
    }
    .encode()
    .unwrap();
    let json = decode_ops(&bytes).unwrap();
    assert!(json.contains("signed-change") && json.contains(&hex(&digest)));
    assert!(!json.contains("canonical"));
    let (d, r, c) = signed_change(&bytes).unwrap();
    assert_eq!((d, r, c), (digest, record, canonical));
}

#[test]
fn ops_track_matches_wire_contract() {
    let name = ops_track("so.tilo.notes", "n1").unwrap();
    assert!(name.starts_with("ops/") && name.len() == 4 + 64);
    assert!(ops_track("", "n1").is_err());
}

#[test]
fn authorized_directions_come_from_the_grant() {
    let host = SigningKey::from_bytes(&[1; 32]);
    let mut custody = LiveCustody::default();
    let (id, asked) = custody
        .begin(|p| Ok(binding(&host, p, both())), true, || Ok([9; 32]))
        .unwrap();
    // The host authorizes a viewer: subscribe only.
    let mut authorized = asked.clone();
    authorized.request.directions = LiveDirections {
        publish: false,
        subscribe: true,
    };
    let grant = custody
        .complete(id, &issue(&host, &authorized, NOW, GRANT_LIFETIME_NS), NOW)
        .unwrap();
    assert!(!grant.grant.metadata.request.directions.publish);
    assert!(grant.grant.metadata.request.directions.subscribe);
}

#[test]
fn renewal_from_a_restarted_host_forces_a_fresh_participation() {
    let host = SigningKey::from_bytes(&[1; 32]);
    let mut custody = LiveCustody::default();
    let (id, b) = custody
        .begin(|p| Ok(binding(&host, p, both())), true, || Ok([9; 32]))
        .unwrap();
    custody
        .complete(id, &issue(&host, &b, NOW, GRANT_LIFETIME_NS), NOW)
        .unwrap();
    let (renew, b2) = custody
        .begin(|p| Ok(binding(&host, p, both())), false, || unreachable!())
        .unwrap();
    let error = custody
        .complete(
            renew,
            &issue_boot(&host, &b2, NOW + 1, GRANT_LIFETIME_NS, [6; 32]),
            NOW + 1,
        )
        .unwrap_err();
    assert!(error.contains("host restarted"));
    assert!(custody.current(NOW + 1).is_none());
    assert!(
        custody
            .begin(|p| Ok(binding(&host, p, both())), false, || unreachable!())
            .is_err(),
        "renewal needs a fresh participation"
    );
}
