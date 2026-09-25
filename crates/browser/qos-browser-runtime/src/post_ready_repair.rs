use qos_peer_state::StatePeer;
use web_time::SystemTime;

#[derive(Default)]
pub(crate) struct PostReadyRepair {
    assessed: bool,
}

impl PostReadyRepair {
    pub(crate) fn on_ready(
        &mut self,
        current_generation: bool,
        state: &StatePeer,
        device: [u8; 32],
        now: SystemTime,
    ) -> bool {
        if !current_generation || self.assessed {
            return false;
        }
        self.assessed = true;
        needs_repair(state, device, now)
    }
}

fn needs_repair(state: &StatePeer, device: [u8; 32], now: SystemTime) -> bool {
    let authority = state.authority().device_public_key;
    if state.revocations().iter().any(|entry| {
        entry.revoked_device_public_key == device || entry.revoked_device_public_key == authority
    }) {
        return false;
    }
    if state.has_governance()
        && (state.permissions(device, now).is_err() || state.permissions(authority, now).is_err())
    {
        return false;
    }
    let admission = match crate::desktop::admission(state) {
        Ok(admission) => admission,
        Err(_) => return true,
    };
    if admission.required.iter().any(|record| {
        state
            .check_document_read(device, &record.app_id, &record.record_id, now)
            .is_err()
    }) {
        return false;
    }
    if state.incomplete_reason().is_some() || !admission.complete() {
        return true;
    }
    admission.required.iter().any(|record| {
        !matches!(
            state.durable_coverage_with_current(&record.app_id, &record.record_id),
            Ok(Some((_, true)))
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use qos_desktop::{
        AppKind, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
        projection::RoomProjection,
    };
    use qos_peer_state::{NeighbourMsg, StatePeer, TransportOrigin};
    use tilo_user_identity::{
        DeviceCapabilities, DeviceCert, RevocationEntry, RevocationList, RevocationReason,
        sign_device_cert, sign_revocation_entry,
    };
    use web_time::Duration;

    const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    struct Fixture {
        root: SigningKey,
        authority_key: SigningKey,
        authority: DeviceCert,
        member_key: SigningKey,
        member: DeviceCert,
        now: SystemTime,
        native: StatePeer,
        generation: u64,
    }

    impl Fixture {
        fn new() -> Self {
            let root = SigningKey::from_bytes(&[81; 32]);
            let authority_key = SigningKey::from_bytes(&[82; 32]);
            let member_key = SigningKey::from_bytes(&[83; 32]);
            let certificate = |key: &SigningKey, label: &str, primary| {
                let mut cert = DeviceCert {
                    user_public_key: root.verifying_key().to_bytes(),
                    device_public_key: key.verifying_key().to_bytes(),
                    device_label: label.into(),
                    capabilities: if primary {
                        DeviceCapabilities::primary()
                    } else {
                        DeviceCapabilities::member()
                    },
                    issued_at_ns: 1,
                    expires_at_ns: None,
                    signature: [0; 64],
                };
                sign_device_cert(&mut cert, &root).unwrap();
                cert
            };
            let authority = certificate(&authority_key, "authority", true);
            let member = certificate(&member_key, "member", false);
            let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000);
            let mut native = StatePeer::new(
                ROOM,
                authority.clone(),
                vec![authority.clone(), member.clone()],
                RevocationList::new(),
                now,
            )
            .unwrap();
            native
                .install_seed(
                    crate::fixture_seed(ROOM, &authority, &[authority.clone(), member.clone()])
                        .unwrap(),
                    &authority_key,
                    now,
                )
                .unwrap();
            let strip = StripId((5 << 48) | 1);
            let group = (5 << 48) | 2;
            let tab = (5 << 48) | 3;
            let mut room = native.admitted_room().unwrap().clone();
            RoomProjection::project_owned_contents(
                &mut room,
                &[Strip {
                    id: strip,
                    name: "Documents".into(),
                    window_groups: vec![WindowGroup {
                        id: group,
                        active_tab: tab,
                        width: WindowWidth::Normal,
                        maximized: false,
                        tabs: vec![Tab {
                            id: tab,
                            title: "Writer".into(),
                            tile: TileContent::AppPlaceholder {
                                kind: AppKind::Other(word_processor::definition().app_id.into()),
                                label: "Writer".into(),
                            },
                        }],
                    }],
                }],
                5,
                None,
            );
            RoomProjection::bind_record(
                &mut room,
                strip.0,
                group,
                tab,
                ui::record::RecordId::for_tab(1).as_str(),
            )
            .unwrap();
            native
                .install_seed(
                    qos_peer_state::SeedState {
                        room_changes: room
                            .get_changes(&[])
                            .iter()
                            .map(|change| change.raw_bytes().to_vec())
                            .collect(),
                        documents: vec![],
                    },
                    &authority_key,
                    now,
                )
                .unwrap();
            Self {
                root,
                authority_key,
                authority,
                member_key,
                member,
                now,
                native,
                generation: 0,
            }
        }

        fn browser(&self, revocations: RevocationList) -> StatePeer {
            StatePeer::new(
                ROOM,
                self.authority.clone(),
                vec![self.authority.clone(), self.member.clone()],
                revocations,
                self.now,
            )
            .unwrap()
        }

        fn messages(&mut self) -> Vec<NeighbourMsg> {
            self.generation += 1;
            self.native
                .durable_bootstrap_messages(&self.authority_key, self.now, self.generation)
                .unwrap()
        }

        fn complete_browser(&mut self) -> StatePeer {
            let messages = self.messages();
            let mut browser = self.browser(RevocationList::new());
            for message in messages {
                browser
                    .receive_from(
                        TransportOrigin::RelayDelta(self.authority_key.verifying_key().to_bytes()),
                        message,
                        self.now,
                    )
                    .unwrap();
            }
            browser
        }
    }

    #[test]
    fn hydrated_current_durable_admission_does_not_request_a_second_bootstrap() {
        let mut fixture = Fixture::new();
        let browser = fixture.complete_browser();
        let admission = crate::desktop::admission(&browser).unwrap();
        assert!(admission.complete());
        assert!(!admission.required.is_empty());
        assert!(admission.required.iter().all(|record| {
            browser
                .durable_covers_current(&record.app_id, &record.record_id)
                .unwrap()
        }));

        let mut ready = PostReadyRepair::default();
        assert!(!ready.on_ready(
            true,
            &browser,
            fixture.member.device_public_key,
            fixture.now
        ));
    }

    #[test]
    fn ready_requests_repair_for_missing_pending_corrupt_or_noncurrent_required_state() {
        let mut fixture = Fixture::new();
        let messages = fixture.messages();
        let required = crate::desktop::admission(&fixture.native).unwrap().required;

        let mut fresh = fixture.browser(RevocationList::new());
        assert!(PostReadyRepair::default().on_ready(
            true,
            &fresh,
            fixture.member.device_public_key,
            fixture.now
        ));

        let app_index = messages
            .iter()
            .position(|message| matches!(message, NeighbourMsg::AppDocument { .. }))
            .unwrap();
        let app = messages[app_index].clone();
        fresh
            .receive_from(
                TransportOrigin::RelayDelta(fixture.authority_key.verifying_key().to_bytes()),
                app.clone(),
                fixture.now,
            )
            .unwrap();
        assert!(fresh.pending_count() > 0);
        assert!(PostReadyRepair::default().on_ready(
            true,
            &fresh,
            fixture.member.device_public_key,
            fixture.now
        ));

        let mut out_of_order = fixture.browser(RevocationList::new());
        out_of_order
            .receive_from(
                TransportOrigin::RelayDelta(fixture.authority_key.verifying_key().to_bytes()),
                app.clone(),
                fixture.now,
            )
            .unwrap();
        assert!(out_of_order.pending_count() > 0);
        for message in messages
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != app_index)
            .map(|(_, message)| message)
            .cloned()
        {
            out_of_order
                .receive_from(
                    TransportOrigin::RelayDelta(fixture.authority_key.verifying_key().to_bytes()),
                    message,
                    fixture.now,
                )
                .unwrap();
        }
        assert_eq!(out_of_order.pending_count(), 0);
        assert!(!PostReadyRepair::default().on_ready(
            true,
            &out_of_order,
            fixture.member.device_public_key,
            fixture.now
        ));

        let missing_route = &required[0];
        let mut missing = fixture.browser(RevocationList::new());
        for message in messages.clone().into_iter().filter(|message| {
            !matches!(message, NeighbourMsg::AppDocument { app_id, record_id, .. }
                if app_id == &missing_route.app_id && record_id == &missing_route.record_id)
        }) {
            missing
                .receive_from(
                    TransportOrigin::RelayDelta(fixture.authority_key.verifying_key().to_bytes()),
                    message,
                    fixture.now,
                )
                .unwrap();
        }
        assert!(PostReadyRepair::default().on_ready(
            true,
            &missing,
            fixture.member.device_public_key,
            fixture.now
        ));

        let mut corrupt = fixture.browser(RevocationList::new());
        for mut message in messages.clone() {
            if let NeighbourMsg::AppDocument { app_id, .. } = &mut message
                && app_id == &missing_route.app_id
            {
                *app_id = "invalid.route".into();
                assert!(
                    corrupt
                        .receive_from(
                            TransportOrigin::RelayDelta(
                                fixture.authority_key.verifying_key().to_bytes(),
                            ),
                            message,
                            fixture.now,
                        )
                        .is_err()
                );
                continue;
            }
            corrupt
                .receive_from(
                    TransportOrigin::RelayDelta(fixture.authority_key.verifying_key().to_bytes()),
                    message,
                    fixture.now,
                )
                .unwrap();
        }
        assert!(PostReadyRepair::default().on_ready(
            true,
            &corrupt,
            fixture.member.device_public_key,
            fixture.now
        ));

        let mut noncurrent = fixture.complete_browser();
        crate::fixture_writer_insert(
            &mut noncurrent,
            &fixture.member_key,
            "local edit",
            fixture.now,
        )
        .unwrap();
        assert!(PostReadyRepair::default().on_ready(
            true,
            &noncurrent,
            fixture.member.device_public_key,
            fixture.now
        ));
    }

    #[test]
    fn ready_does_not_request_for_empty_admission_or_revoked_and_denied_devices() {
        let mut fixture = Fixture::new();
        let mut room = fixture.native.admitted_room().unwrap().clone();
        qos_desktop::projection::RoomProjection::project_owned_contents(&mut room, &[], 5, None);
        fixture
            .native
            .install_seed(
                qos_peer_state::SeedState {
                    room_changes: room
                        .get_changes(&[])
                        .iter()
                        .map(|change| change.raw_bytes().to_vec())
                        .collect(),
                    documents: vec![],
                },
                &fixture.authority_key,
                fixture.now,
            )
            .unwrap();
        let empty = fixture.complete_browser();
        assert!(
            crate::desktop::admission(&empty)
                .unwrap()
                .required
                .is_empty()
        );
        assert!(!PostReadyRepair::default().on_ready(
            true,
            &empty,
            fixture.member.device_public_key,
            fixture.now
        ));

        let mut fixture = Fixture::new();
        let mut revocation = RevocationEntry {
            user_public_key: fixture.member.user_public_key,
            revoked_device_public_key: fixture.member.device_public_key,
            revoked_at_ns: 1,
            reason: RevocationReason::Compromised,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut revocation, &fixture.root).unwrap();
        let mut revoked = fixture.complete_browser();
        revoked
            .receive_from(
                TransportOrigin::UntrustedPresence(
                    fixture.authority_key.verifying_key().to_bytes(),
                ),
                NeighbourMsg::IdentityHello {
                    cert: Box::new(fixture.authority.clone()),
                    revocations: vec![revocation],
                },
                fixture.now,
            )
            .unwrap();
        assert!(!PostReadyRepair::default().on_ready(
            true,
            &revoked,
            fixture.member.device_public_key,
            fixture.now
        ));
        assert!(!PostReadyRepair::default().on_ready(true, &revoked, [99; 32], fixture.now));
    }

    #[test]
    fn only_the_first_ready_for_the_current_transport_can_request_repair() {
        let fixture = Fixture::new();
        let fresh = fixture.browser(RevocationList::new());
        let mut ready = PostReadyRepair::default();
        assert!(!ready.on_ready(false, &fresh, fixture.member.device_public_key, fixture.now));
        assert!(ready.on_ready(true, &fresh, fixture.member.device_public_key, fixture.now));
        assert!(!ready.on_ready(true, &fresh, fixture.member.device_public_key, fixture.now));
    }
}
