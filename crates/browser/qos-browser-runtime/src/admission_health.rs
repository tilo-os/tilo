use qos_peer_state::StatePeer;
use web_time::SystemTime;

pub(crate) fn durable_admission_failure(
    state: &StatePeer,
    device: [u8; 32],
    now: SystemTime,
    admission_failure: Option<String>,
) -> Option<String> {
    state
        .incomplete_reason()
        .or(admission_failure)
        .or_else(|| {
            state
                .has_governance()
                .then(|| state.permissions(device, now).err())
                .flatten()
        })
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;
    use qos_peer_state::{SeedState, StatePeer, seed_room};
    use qos_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
    use web_time::{Duration, SystemTime};

    fn certificate(root: &SigningKey, key: &SigningKey, label: &str) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: label.into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    #[test]
    fn permission_health_waits_for_governance_but_preserves_real_denial() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let authority_key = SigningKey::from_bytes(&[2; 32]);
        let member_key = SigningKey::from_bytes(&[3; 32]);
        let authority = certificate(&root, &authority_key, "authority");
        let member = certificate(&root, &member_key, "member");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000);
        let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

        let mut state = StatePeer::new(
            room,
            authority.clone(),
            vec![authority.clone(), member.clone()],
            RevocationList::new(),
            now,
        )
        .unwrap();

        assert!(!state.has_governance());
        assert!(state.permissions(member.device_public_key, now).is_err());
        assert_eq!(
            super::durable_admission_failure(&state, member.device_public_key, now, None),
            None,
            "missing governance is a bootstrap prerequisite, not a permission failure"
        );
        assert_eq!(
            super::durable_admission_failure(
                &state,
                member.device_public_key,
                now,
                Some("persisted admission failed".into()),
            ),
            Some("persisted admission failed".into()),
            "an actual admission failure remains terminal before governance arrives"
        );

        state
            .install_seed(
                SeedState {
                    room_changes: seed_room(
                        room,
                        &authority,
                        &[authority.clone(), member.clone()],
                    )
                    .unwrap(),
                    documents: vec![],
                },
                &authority_key,
                now,
            )
            .unwrap();
        assert!(state.has_governance());
        assert_eq!(
            super::durable_admission_failure(&state, member.device_public_key, now, None),
            None,
            "an admitted certificate remains healthy"
        );

        let mut denied = StatePeer::new(
            room,
            authority.clone(),
            vec![authority.clone(), member.clone()],
            RevocationList::new(),
            now,
        )
        .unwrap();
        denied
            .install_seed(
                SeedState {
                    room_changes: seed_room(room, &authority, &[authority.clone()]).unwrap(),
                    documents: vec![],
                },
                &authority_key,
                now,
            )
            .unwrap();
        assert_eq!(
            super::durable_admission_failure(
                &denied,
                member.device_public_key,
                now,
                None,
            ),
            Some("device is absent from governed roster".into()),
            "governance denial must remain terminal"
        );
    }
}
