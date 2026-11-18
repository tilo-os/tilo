use super::*;

#[test]
fn a_scroll_says_its_unit_and_only_a_finger_scroll_may_be_empty() {
    let scroll = |horizontal, vertical, unit| FabricInputEvent::Scroll {
        horizontal,
        vertical,
        unit,
    };
    assert!(scroll(0, -30, ScrollUnit::Lines).validate());
    assert!(
        scroll(0, 0, ScrollUnit::Pixels).validate(),
        "a finger scroll's end"
    );
    assert!(!scroll(0, 0, ScrollUnit::Lines).validate());
    assert!(!scroll(2_000_000, 0, ScrollUnit::Pixels).validate());
    let wire = serde_json::to_string(&scroll(0, -60, ScrollUnit::Lines)).unwrap();
    assert_eq!(
        wire,
        r#"{"scroll":{"horizontal":0,"vertical":-60,"unit":"lines"}}"#
    );
    assert!(
        serde_json::from_str::<FabricInputEvent>(r#"{"scroll":{"horizontal":0,"vertical":-2}}"#)
            .is_err(),
        "a scroll without a unit is not guessed at"
    );
}

fn id(value: &str) -> DeviceId {
    DeviceId::new(value).unwrap()
}
fn stamp(device: &str, counter: u64) -> FabricStamp {
    FabricStamp {
        counter,
        device: id(device),
    }
}
fn snapshot(device: &str, counter: u64, x: i32, caps: FabricCapabilities) -> DeviceFabricSnapshot {
    let owner = id(device);
    DeviceFabricSnapshot {
        schema_version: FABRIC_SCHEMA_VERSION,
        owner: owner.clone(),
        presence: DevicePresence {
            device: owner.clone(),
            label: device.into(),
            kind: DeviceKind::Laptop,
            capabilities: caps,
            online: true,
            last_seen_unix_ms: 10,
            wake_routes: Vec::new(),
            session: OwnerSession::default(),
            stamp: stamp(device, counter),
        },
        media: Vec::new(),
        clipboard: Vec::new(),
        settings: Vec::new(),
        wifi: Vec::new(),
        display: Some(DisplayPlacement {
            outputs: Vec::new(),
            device: owner,
            rect: DisplayRect {
                x,
                y: 0,
                width: 1000,
                height: 800,
            },
            scale_milli: 1000,
            stamp: stamp(device, counter),
        }),
        unlock: crate::fabric::unlock::UnlockFabricState::default(),
        speakers: crate::fabric::speaker_group::SpeakerFabricState::default(),
    }
}

fn arranged_setting(device: &str, counter: u64, displays: Value) -> RoamingSetting {
    let mut value = serde_json::Map::new();
    value.insert("displays".into(), displays);
    RoamingSetting {
        key: settings_store::person_displays::PLACEMENTS_KEY.into(),
        value: Value::Object(value),
        stamp: stamp(device, counter),
    }
}

#[test]
fn display_placements_merge_concurrent_edits_in_both_delivery_orders() {
    let entry = |output: &str, x: i32, rev: u64| {
        serde_json::json!({
            "device": "desktop", "output": output, "enabled": true,
            "order": 0, "placed": true, "x": x, "y": 0, "rev": rev
        })
    };
    let mut desktop = snapshot("desktop", 9, 0, FabricCapabilities::default());
    desktop.settings.push(arranged_setting(
        "desktop",
        9,
        serde_json::json!([entry("DP-1", 100, 3), entry("DP-2", 0, 1)]),
    ));
    let mut laptop = snapshot("laptop", 10, 1000, FabricCapabilities::default());
    laptop.settings.push(arranged_setting(
        "laptop",
        10,
        serde_json::json!([entry("DP-1", 0, 1), entry("DP-2", 200, 3)]),
    ));
    let mut first = DeviceFabric::default();
    first.upsert(desktop.clone()).unwrap();
    first.upsert(laptop.clone()).unwrap();
    let mut second = DeviceFabric::default();
    second.upsert(laptop).unwrap();
    second.upsert(desktop).unwrap();
    let records = first.merged_setting_records();
    assert_eq!(records, second.merged_setting_records());
    let record = &records[settings_store::person_displays::PLACEMENTS_KEY];
    assert_eq!(record.stamp, stamp("laptop", 10));
    let placement = settings_store::person_displays::Placements::from_value(&record.value).unwrap();
    assert_eq!(placement.displays.len(), 2);
    assert_eq!(placement.displays[0].x, 100);
    assert_eq!(placement.displays[1].x, 200);
    // A later fabric publication must not beat a newer per-display edit.
    let mut stale = snapshot("tablet", 100, 2000, FabricCapabilities::default());
    stale.settings.push(arranged_setting(
        "tablet",
        100,
        serde_json::json!([entry("DP-1", -300, 2)]),
    ));
    first.upsert(stale).unwrap();
    assert_eq!(
        first.merged_settings()[settings_store::person_displays::PLACEMENTS_KEY],
        record.value
    );
}

#[test]
fn display_placements_reject_malformed_fabric_records() {
    for displays in [
        serde_json::json!(false),
        serde_json::json!([{
            "device": "desktop", "output": "DP-1", "enabled": true,
            "order": 0, "placed": true, "x": i32::MIN, "y": 0, "rev": 1
        }]),
    ] {
        let mut source = snapshot("desktop", 1, 0, FabricCapabilities::default());
        source
            .settings
            .push(arranged_setting("desktop", 1, displays));
        assert_eq!(
            DeviceFabric::default().upsert(source),
            Err(FabricError::InvalidRoamingSetting)
        );
    }
}

#[test]
fn display_connectors_round_trip_as_their_pinned_bytes() {
    let payload = crate::PersonalFabricPayload::DisplayConnectors {
        stamp: stamp("peer", 1),
        outputs: Vec::new(),
    };
    let pinned = hex::decode("0701047065657200").unwrap();
    assert_eq!(postcard::to_allocvec(&payload).unwrap(), pinned);
    let decoded: crate::PersonalFabricPayload = postcard::from_bytes(&pinned).unwrap();
    assert!(matches!(decoded,
        crate::PersonalFabricPayload::DisplayConnectors { outputs, .. }
            if outputs.is_empty()));
}

fn rich_display_fixture() -> (DeviceFabricSnapshot, DisplayOutputObservation) {
    let mut source = snapshot("peer", 2, 960, FabricCapabilities::default());
    let facts = DisplayOutputObservation {
        connector: "DP-1".into(),
        output: settings_store::person_displays::DeviceOutput {
            id: "monitor:panel".into(),
            name: "Panel".into(),
            builtin: false,
            x: 0,
            y: 0,
            width: 1000,
            height: 800,
            mode_width: 2000,
            mode_height: 1600,
            refresh_mhz: 59_940,
            scale_milli: 2000,
            connected: true,
            enabled: true,
        },
    };
    source
        .display
        .as_mut()
        .unwrap()
        .outputs
        .push(DisplayConnectorPlacement {
            connector: facts.connector.clone(),
            label: "DP-1".into(),
            rect: DisplayRect {
                x: 0,
                y: 0,
                width: 1000,
                height: 800,
            },
            scale_milli: 2000,
        });
    (source, facts)
}

#[test]
fn rich_display_observations_bind_owner_revision_geometry_and_checkpoint() {
    let (mut source, facts) = rich_display_fixture();
    let owner = source.owner.clone();
    let revision = source.display.as_ref().unwrap().stamp.clone();
    let mut fabric = DeviceFabric::default();
    fabric.upsert(source.clone()).unwrap();
    let checkpoint = fabric.encode_checkpoint().unwrap();
    for stamp in [stamp("stranger", 2), stamp("peer", 1)] {
        assert_eq!(
            fabric.observe_display_outputs(&owner, &stamp, vec![facts.clone()]),
            Err(FabricError::ForeignAuthorship)
        );
    }
    let mut wrong = facts.clone();
    wrong.output.width += 1;
    assert_eq!(
        fabric.observe_display_outputs(&owner, &revision, vec![wrong]),
        Err(FabricError::InvalidDisplayPlacement)
    );
    assert_eq!(
        fabric.observe_display_outputs(&owner, &revision, Vec::new()),
        Err(FabricError::InvalidDisplayPlacement)
    );
    assert!(
        fabric
            .observe_display_outputs(&owner, &revision, vec![facts.clone()])
            .unwrap()
    );
    assert!(
        !fabric
            .observe_display_outputs(&owner, &revision, vec![facts.clone()])
            .unwrap()
    );
    assert_eq!(fabric.encode_checkpoint().unwrap(), checkpoint);
    assert!(
        DeviceFabric::decode_checkpoint(&checkpoint)
            .unwrap()
            .display_outputs(&owner)
            .is_none()
    );
    let mut conflicting = facts.clone();
    conflicting.output.refresh_mhz = 60_000;
    assert_eq!(
        fabric.observe_display_outputs(&owner, &revision, vec![conflicting]),
        Err(FabricError::ConflictingRevision)
    );
    source.presence.online = false;
    source.presence.stamp.counter = 3;
    fabric.upsert(source.clone()).unwrap();
    let devices = fabric.person_devices(&owner);
    assert_eq!(devices[0].outputs, vec![facts.output]);
    assert!(!devices[0].online);
    source.presence.stamp.counter = 4;
    source.display.as_mut().unwrap().stamp.counter = 4;
    fabric.upsert(source).unwrap();
    assert!(fabric.display_outputs(&owner).is_none());
    assert!(fabric.person_devices(&owner)[0].outputs.is_empty());
    assert!(fabric.remove_device(&owner));
    assert!(fabric.person_devices(&owner).is_empty());
}

#[test]
fn rich_display_observations_reject_unbounded_and_duplicate_metadata() {
    let (source, facts) = rich_display_fixture();
    let owner = source.owner.clone();
    let revision = source.display.as_ref().unwrap().stamp.clone();
    let mut fabric = DeviceFabric::default();
    fabric.upsert(source).unwrap();
    for invalid in [
        {
            let mut item = facts.clone();
            item.output.x = i32::MIN;
            vec![item]
        },
        vec![facts.clone(); settings_store::person_displays::MAX_DEVICE_OUTPUTS + 1],
    ] {
        assert_eq!(
            fabric.observe_display_outputs(&owner, &revision, invalid),
            Err(FabricError::InvalidDisplayPlacement)
        );
    }
    assert_eq!(
        fabric.observe_display_outputs(&owner, &revision, vec![facts.clone(), facts]),
        Err(FabricError::DuplicateItem)
    );
}

#[test]
fn connector_extension_keeps_schema_six_display_bytes() {
    let peer = DeviceId::new("peer").unwrap();
    let display = DisplayPlacement {
        device: peer.clone(),
        rect: DisplayRect {
            x: 960,
            y: 0,
            width: 1280,
            height: 720,
        },
        scale_milli: 1000,
        stamp: FabricStamp {
            counter: 1,
            device: peer,
        },
        outputs: vec![DisplayConnectorPlacement {
            connector: "DP-1".into(),
            label: "Panel".into(),
            rect: DisplayRect {
                x: 0,
                y: 0,
                width: 1280,
                height: 720,
            },
            scale_milli: 1250,
        }],
    };
    // Fixed schema-6 Postcard bytes: owner, aggregate rect, scale, stamp.
    let previous = hex::decode("0470656572800f00800ad005e807010470656572").unwrap();
    assert_eq!(FABRIC_SCHEMA_VERSION, 8);
    assert_eq!(postcard::to_allocvec(&display).unwrap(), previous);
    let decoded: DisplayPlacement = postcard::from_bytes(&previous).unwrap();
    assert!(
        decoded.outputs.is_empty(),
        "legacy peers retain their aggregate-screen behavior"
    );
    assert_eq!(decoded.rect, display.rect);
}

#[test]
fn connector_observations_require_the_exact_owner_display_revision() {
    let caps = FabricCapabilities {
        display: true,
        pointer: true,
        ..Default::default()
    };
    let mut local = snapshot("peer", 2, 960, caps);
    let display = local.display.as_mut().unwrap();
    let owner = display.device.clone();
    let stamp = display.stamp.clone();
    let outputs = vec![DisplayConnectorPlacement {
        connector: "DP-1".into(),
        label: "Panel".into(),
        rect: DisplayRect {
            x: 0,
            y: 0,
            width: 960,
            height: 540,
        },
        scale_milli: 1250,
    }];
    let mut fabric = DeviceFabric::default();
    fabric.upsert(local.clone()).unwrap();
    let mut stale = stamp.clone();
    stale.counter -= 1;
    assert!(
        fabric
            .observe_display_connectors(&owner, &stale, outputs.clone())
            .is_err()
    );
    assert!(
        fabric
            .observe_display_connectors(&DeviceId::new("other").unwrap(), &stamp, outputs.clone())
            .is_err()
    );
    assert!(
        fabric
            .observe_display_connectors(&owner, &stamp, outputs.clone())
            .unwrap()
    );
    assert!(
        !fabric
            .observe_display_connectors(&owner, &stamp, outputs.clone())
            .unwrap()
    );
    let mut conflict = outputs;
    conflict[0].rect.x = 50;
    assert!(
        fabric
            .observe_display_connectors(&owner, &stamp, conflict)
            .is_err()
    );
    assert!(
        !fabric.upsert(local.clone()).unwrap(),
        "duplicate schema-6 checkpoints retain the optional observation"
    );
    local.presence.stamp.counter += 1;
    fabric.upsert(local).unwrap();
    assert_eq!(
        fabric
            .snapshots()
            .get(&owner)
            .unwrap()
            .display
            .as_ref()
            .unwrap()
            .outputs
            .len(),
        1
    );
    fabric.remove_device(&owner);
    assert!(
        fabric
            .observe_display_connectors(&owner, &stamp, Vec::new())
            .is_err(),
        "revoked or forgotten peers cannot retain observations"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn media_handoff_and_media_actions_are_capability_and_locator_checked() {
    fn media_action_locators_reject_host_paths_and_url_credentials() {
        let source = id("source");
        let target = id("target");
        for locator in [
            MediaSourceLocator::Workspace {
                path: "../private/song.flac".into(),
            },
            MediaSourceLocator::RemoteStream {
                url: "https://secret@example.com/song".into(),
            },
            MediaSourceLocator::AppResource {
                uri: "1music://track/foreign".into(),
            },
            MediaSourceLocator::PersonalTransfer {
                transfer_id: [0; 16],
                file_name: "../foreign.mp4".into(),
            },
        ] {
            let envelope = PersonalFabricActionEnvelope {
                request_id: [4; 16],
                source: source.clone(),
                target: target.clone(),
                issued_unix_ms: 10_000,
                action: PersonalFabricAction::ResumeMedia {
                    session_id: "player:one".into(),
                    app_id: "so.tilo.music".into(),
                    workspace_id: "11111111-1111-1111-1111-111111111111".into(),
                    title: "Track".into(),
                    locator,
                    position_ms: 0,
                    resume_playing: true,
                    has_video: false,
                },
            };
            assert_eq!(
                envelope.validate_at(&source, &target, 10_000),
                Err(FabricError::InvalidAction)
            );
        }
    }

    fn media_terminal_actions_require_exact_nonzero_request_and_session() {
        let source = id("source");
        let target = id("target");
        for action in [
            PersonalFabricAction::MediaRejected {
                request_id: [9; 16],
                session_id: "player:one".into(),
                failure: MediaHandoffFailure::OpenFailed,
            },
            PersonalFabricAction::MediaCancelled {
                request_id: [9; 16],
                session_id: "player:one".into(),
            },
        ] {
            let envelope = PersonalFabricActionEnvelope {
                request_id: [4; 16],
                source: source.clone(),
                target: target.clone(),
                issued_unix_ms: 10_000,
                action,
            };
            assert_eq!(envelope.validate_at(&source, &target, 10_000), Ok(()));
        }

        let invalid = PersonalFabricActionEnvelope {
            request_id: [4; 16],
            source: source.clone(),
            target: target.clone(),
            issued_unix_ms: 10_000,
            action: PersonalFabricAction::MediaRejected {
                request_id: [0; 16],
                session_id: "player:one".into(),
                failure: MediaHandoffFailure::OpenFailed,
            },
        };
        assert_eq!(
            invalid.validate_at(&source, &target, 10_000),
            Err(FabricError::InvalidAction)
        );
    }

    media_action_locators_reject_host_paths_and_url_credentials();
    media_terminal_actions_require_exact_nonzero_request_and_session();
}

#[test]
fn window_views_are_bounded_distinct_and_sized() {
    let source = id("viewer");
    let target = id("host");
    let envelope =
        |views: Vec<WindowView>, decode: Vec<HardwareDecode>| PersonalFabricActionEnvelope {
            request_id: [5; 16],
            source: source.clone(),
            target: target.clone(),
            issued_unix_ms: 10_000,
            action: PersonalFabricAction::WindowViews { views, decode },
        };
    let view = |tab, width, height| WindowView { tab, width, height };
    let ok = |views| {
        envelope(views, Vec::new())
            .validate_at(&source, &target, 10_000)
            .is_ok()
    };
    let decodes = |decode| {
        envelope(vec![view(7, 1280, 800)], decode)
            .validate_at(&source, &target, 10_000)
            .is_ok()
    };
    let hw = |codec: &str, max| HardwareDecode {
        codec: codec.into(),
        max,
    };

    assert!(ok(Vec::new()), "an empty list ends every stream");
    assert!(ok(vec![
        view(7, 1280, 800),
        view(8, 1, MAX_WINDOW_VIEW_SIDE)
    ]));
    assert!(!ok(vec![view(0, 1280, 800)]), "a tab id is never zero");
    assert!(!ok(vec![view(7, 0, 800)]));
    assert!(!ok(vec![view(7, 1280, MAX_WINDOW_VIEW_SIDE + 1)]));
    assert!(
        !ok(vec![view(7, 1280, 800), view(7, 640, 480)]),
        "one size per tab"
    );
    assert!(!ok((1..=u64::try_from(MAX_WINDOW_VIEWS).unwrap() + 1)
        .map(|tab| view(tab, 100, 100))
        .collect()));

    // A board's decoder limit travels with the list.
    assert!(decodes(vec![
        hw("h264", Some((1920, 1088))),
        hw("av1", None)
    ]));
    assert!(!decodes(vec![hw("mjpeg", None)]), "a codec streams use");
    assert!(!decodes(vec![hw("h264", Some((0, 1088)))]));
    assert!(
        !decodes(vec![hw("h264", None), hw("h264", Some((1280, 720)))]),
        "one limit per codec"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn clipboard_and_nearby_payloads_are_bounded_and_tamper_checked() {
    fn clipboard_is_deduplicated_and_bounded() {
        let mut history = ClipboardHistory::default();
        for n in 0..=MAX_CLIPBOARD_ITEMS {
            history
                .push(
                    ClipboardEntry::shareable(
                        ClipboardPayload::Text(format!("item-{n}")),
                        id("laptop"),
                        n as u64,
                        stamp("laptop", n as u64),
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        assert_eq!(history.entries().count(), MAX_CLIPBOARD_ITEMS);
    }

    fn clipboard_hash_tampering_is_rejected() {
        let mut entry = ClipboardEntry::shareable(
            ClipboardPayload::Text("safe".into()),
            id("phone"),
            1,
            stamp("phone", 1),
        )
        .unwrap();
        entry.payload = ClipboardPayload::Text("changed".into());
        let mut source = snapshot("phone", 1, 0, FabricCapabilities::default());
        source.clipboard.push(entry);
        assert_eq!(source.validate(), Err(FabricError::ClipboardHashMismatch));
    }

    fn nearby_transfer_actions_bind_nonzero_ids_and_bounded_manifests() {
        let source = id("source");
        let target = id("target");
        let envelope = |action| PersonalFabricActionEnvelope {
            request_id: [3; 16],
            source: source.clone(),
            target: target.clone(),
            issued_unix_ms: 10_000,
            action,
        };
        assert_eq!(
            envelope(PersonalFabricAction::NearbyTransferOffer {
                transfer_id: [4; 16],
                name: "Photos".into(),
                kind: tilo_nearby_service::TransferKind::FolderArchive,
                total_bytes: 100,
                content_hash: [5; 32],
            })
            .validate_at(&source, &target, 10_100),
            Ok(())
        );
        assert_eq!(
            envelope(PersonalFabricAction::NearbyTransferReady {
                request_id: [0; 16],
                transfer_id: [4; 16],
                committed_offset: 0,
                prefix_hash: *blake3::hash(&[]).as_bytes(),
            })
            .validate_at(&source, &target, 10_100),
            Err(FabricError::InvalidAction)
        );
        assert_eq!(
            envelope(PersonalFabricAction::NearbyTransferOffer {
                transfer_id: [4; 16],
                name: "../escape".into(),
                kind: tilo_nearby_service::TransferKind::File,
                total_bytes: 100,
                content_hash: [5; 32],
            })
            .validate_at(&source, &target, 10_100),
            Err(FabricError::InvalidAction)
        );
    }

    fn nearby_drop_rejects_paths_tampering_and_oversize_payloads() {
        let source = id("source");
        let target = id("target");
        for (name, bytes, hash) in [
            ("../secret", b"ok".to_vec(), *blake3::hash(b"ok").as_bytes()),
            ("note.txt", b"changed".to_vec(), [0; 32]),
            ("note.txt", vec![1; MAX_NEARBY_DROP_BYTES + 1], [0; 32]),
        ] {
            let envelope = PersonalFabricActionEnvelope {
                request_id: [1; 16],
                source: source.clone(),
                target: target.clone(),
                issued_unix_ms: 10,
                action: PersonalFabricAction::NearbyDrop {
                    file_name: name.into(),
                    content_hash: hash,
                    bytes,
                },
            };
            assert_eq!(
                envelope.validate_at(&source, &target, 10),
                Err(FabricError::InvalidAction)
            );
        }
    }

    clipboard_is_deduplicated_and_bounded();
    clipboard_hash_tampering_is_rejected();
    nearby_transfer_actions_bind_nonzero_ids_and_bounded_manifests();
    nearby_drop_rejects_paths_tampering_and_oversize_payloads();
}

#[test]
fn roaming_settings_accept_only_audited_leaves_and_resolve_deterministically() {
    fn local_hardware_settings_do_not_roam() {
        for key in [
            "tilo.device.name",
            "tilo.display.scale",
            "tilo.network.interface.mac",
            "tilo.storage.mount.usb",
            "tilo.input_device.tablets",
            "tilo.audio_device.output_device",
            "tilo.agent_providers.config",
        ] {
            let mut source = snapshot("desktop", 1, 0, FabricCapabilities::default());
            source.settings.push(RoamingSetting {
                key: key.into(),
                value: Value::Bool(true),
                stamp: stamp("desktop", 1),
            });
            assert_eq!(source.validate(), Err(FabricError::InvalidRoamingSetting));
        }
    }

    fn only_audited_personal_setting_leaves_are_accepted() {
        for key in [
            "tilo.accessibility.screen_reader",
            "tilo.appearance.studio",
            "tilo.locale.measurement",
            "tilo.notifications.per_app",
            "tilo.input.keyboard_layouts",
            "tilo.audio.mono_output",
            "tilo.display_person.night_light_temperature",
            "tilo.shortcuts.bindings",
            "tilo.desktop_controls.vim_mode",
            "tilo.quick_settings.controls",
            "tilo.text_assistance.auto_correct",
        ] {
            assert!(valid_setting_key(key), "expected {key} to roam");
        }
        for key in [
            "tilo.appearance_device.wallpaper_path",
            "tilo.input_device.keyboard_model",
            "tilo.audio_device.input_device",
            "tilo.display.auto_brightness",
            "tilo.power.auto_suspend",
            "tilo.updates.auto",
            "tilo.security.local_credential",
            "tilo.input",
        ] {
            assert!(!valid_setting_key(key), "expected {key} to stay local");
        }
    }

    fn newest_roaming_setting_wins_deterministically() {
        let mut old = snapshot("desktop", 1, 0, FabricCapabilities::default());
        old.settings.push(RoamingSetting {
            key: "tilo.appearance.mode".into(),
            value: Value::Bool(false),
            stamp: stamp("desktop", 1),
        });
        let mut new = snapshot("laptop", 2, 1000, FabricCapabilities::default());
        new.settings.push(RoamingSetting {
            key: "tilo.appearance.mode".into(),
            value: Value::Bool(true),
            stamp: stamp("laptop", 2),
        });
        let mut fabric = DeviceFabric::default();
        fabric.upsert(old).unwrap();
        fabric.upsert(new).unwrap();
        assert_eq!(
            fabric.merged_settings()["tilo.appearance.mode"],
            Value::Bool(true)
        );
    }

    fn equal_revision_conflicts_fail_instead_of_using_arrival_order() {
        let mut fabric = DeviceFabric::default();
        let first = snapshot("laptop", 1, 0, FabricCapabilities::default());
        let mut conflicting = first.clone();
        conflicting.presence.label = "Different".into();
        fabric.upsert(first).unwrap();
        assert_eq!(
            fabric.upsert(conflicting),
            Err(FabricError::ConflictingRevision)
        );
    }

    fn persisted_checkpoint_round_trips_only_validated_state() {
        let mut fabric = DeviceFabric::default();
        fabric
            .upsert(snapshot("laptop", 4, 0, FabricCapabilities::default()))
            .unwrap();
        let encoded = fabric.encode_checkpoint().unwrap();
        assert_eq!(DeviceFabric::decode_checkpoint(&encoded).unwrap(), fabric);

        let mut trailing = encoded;
        trailing.push(0xff);
        assert_eq!(
            DeviceFabric::decode_checkpoint(&trailing),
            Err(FabricError::InvalidCheckpoint)
        );
    }

    local_hardware_settings_do_not_roam();
    only_audited_personal_setting_leaves_are_accepted();
    newest_roaming_setting_wins_deterministically();
    equal_revision_conflicts_fail_instead_of_using_arrival_order();
    persisted_checkpoint_round_trips_only_validated_state();
}

#[test]
fn wifi_records_are_sealed_typed_and_legacy_safe() {
    fn wifi_secrets_must_be_sealed() {
        let mut source = snapshot("phone", 1, 0, FabricCapabilities::default());
        source.wifi.push(WifiCredential {
            network_id: [1; 32],
            ssid: "Home".into(),
            security: os_actions::WifiSecurity::Wpa3,
            sealed_secret: Vec::new(),
            nonce: [2; 24],
            stamp: stamp("phone", 1),
        });
        assert_eq!(source.validate(), Err(FabricError::InvalidWifiCredential));
    }

    fn typed_wifi_security_decodes_the_legacy_string_postcard_shape() {
        #[derive(Serialize)]
        struct LegacyWifiCredential {
            network_id: [u8; 32],
            ssid: String,
            security: String,
            sealed_secret: Vec<u8>,
            nonce: [u8; 24],
            stamp: FabricStamp,
        }

        let legacy = LegacyWifiCredential {
            network_id: [1; 32],
            ssid: "Home".into(),
            security: "wpa3".into(),
            sealed_secret: vec![1, 2, 3],
            nonce: [2; 24],
            stamp: stamp("phone", 1),
        };
        let bytes = postcard::to_allocvec(&legacy).unwrap();
        let decoded: WifiCredential = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.security, os_actions::WifiSecurity::Wpa3);
        assert_eq!(postcard::to_allocvec(&decoded).unwrap(), bytes);
    }

    fn legacy_enterprise_record_is_retained_but_cannot_supply_a_password() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[41; 32]);
        let owner_text = hex::encode(key.verifying_key().to_bytes());
        let owner = id(&owner_text);
        let mut credential = crate::fabric::secrets::seal_wifi_credential(
            "Corp",
            os_actions::WifiSecurity::Wpa2,
            "legacy-secret",
            &owner,
            [owner.clone()],
            FabricStamp {
                counter: 1,
                device: owner.clone(),
            },
        )
        .unwrap();
        // Version 1 allowed this string even though its secret lacked the EAP
        // identity and CA policy needed to safely recreate the profile.
        credential.security = os_actions::WifiSecurity::Enterprise;

        assert_eq!(
            crate::fabric::secrets::open_wifi_credential(&credential, &key),
            Err(crate::fabric::secrets::WifiSecretError::InvalidCredential)
        );

        let mut source = snapshot(&owner_text, 1, 0, FabricCapabilities::default());
        source.wifi.push(credential);
        source.settings.push(RoamingSetting {
            key: "tilo.appearance.mode".into(),
            value: Value::String("Nord".into()),
            stamp: FabricStamp {
                counter: 1,
                device: owner,
            },
        });
        let mut fabric = DeviceFabric::default();
        fabric.upsert(source).unwrap();

        assert_eq!(
            fabric.merged_settings()["tilo.appearance.mode"],
            Value::String("Nord".into())
        );
    }

    wifi_secrets_must_be_sealed();
    typed_wifi_security_decodes_the_legacy_string_postcard_shape();
    legacy_enterprise_record_is_retained_but_cannot_supply_a_password();
}

#[test]
fn a_device_that_went_away_keeps_its_place_until_back() {
    let caps = FabricCapabilities {
        display: true,
        pointer: true,
        ..FabricCapabilities::default()
    };
    let tablet = snapshot("tablet", 1, 1000, caps);
    let mut fabric = DeviceFabric::default();
    fabric.upsert(snapshot("laptop", 1, 0, caps)).unwrap();
    fabric.upsert(tablet.clone()).unwrap();
    assert!(fabric.mark_departed(&id("tablet")));
    let gone = &fabric.snapshots()[&id("tablet")];
    assert!(!gone.presence.online && gone.display.is_some());
    // Its last revision again, from itself: it is back.
    assert_eq!(fabric.upsert(tablet), Ok(true));
    // A restart keeps only this device online.
    let restored = fabric.restored(Some(&id("laptop")));
    let online: Vec<_> = restored
        .online_devices()
        .map(|p| p.device.clone())
        .collect();
    assert_eq!(online, vec![id("laptop")]);
    assert!(restored.snapshots().contains_key(&id("tablet")));
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn remote_input_and_gamepad_actions_require_exact_bound_leases() {
    fn remote_input_requires_a_short_lived_explicit_lease() {
        let lease = InputLease {
            controller: id("laptop"),
            target: id("tablet"),
            issued_unix_ms: 100,
            expires_unix_ms: 200,
            nonce: [7; 16],
            allow_keyboard: true,
            allow_pointer: true,
        };
        assert_eq!(lease.validate_at(150), Ok(()));
        assert_eq!(lease.validate_at(201), Err(FabricError::ExpiredInputLease));
    }

    fn input_lease_must_be_issued_by_the_target_to_its_controller() {
        let controller = id("controller");
        let target = id("target");
        let lease = InputLease {
            controller: controller.clone(),
            target: target.clone(),
            issued_unix_ms: 10_000,
            expires_unix_ms: 20_000,
            nonce: [8; 16],
            allow_keyboard: true,
            allow_pointer: true,
        };
        let envelope = PersonalFabricActionEnvelope {
            request_id: [9; 16],
            source: target.clone(),
            target: controller.clone(),
            issued_unix_ms: 10_000,
            action: PersonalFabricAction::InputLeaseGranted { lease },
        };
        assert_eq!(envelope.validate_at(&target, &controller, 10_100), Ok(()));
        assert_eq!(
            envelope.validate_at(&id("forged-target"), &controller, 10_100),
            Err(FabricError::InvalidAction)
        );
    }

    fn input_actions_reject_unbound_or_malformed_events() {
        let source = id("controller");
        let target = id("target");
        for (nonce, event) in [
            ([0; 16], FabricInputEvent::PointerMotion { x: 10, y: 20 }),
            (
                [1; 16],
                FabricInputEvent::PointerButton {
                    button: 0,
                    pressed: true,
                },
            ),
            (
                [1; 16],
                FabricInputEvent::Key {
                    keycode: 0x300,
                    pressed: true,
                    modifiers: 0,
                },
            ),
        ] {
            let envelope = PersonalFabricActionEnvelope {
                request_id: [2; 16],
                source: source.clone(),
                target: target.clone(),
                issued_unix_ms: 10_000,
                action: PersonalFabricAction::Input {
                    lease_nonce: nonce,
                    event,
                },
            };
            assert_eq!(
                envelope.validate_at(&source, &target, 10_100),
                Err(FabricError::InvalidAction)
            );
        }
    }

    fn gamepad_actions_are_typed_exact_bound_and_nonzero() {
        let source = id("source");
        let target = id("target");
        let envelope = |action| PersonalFabricActionEnvelope {
            request_id: [8; 16],
            source: source.clone(),
            target: target.clone(),
            issued_unix_ms: 10_000,
            action,
        };
        assert!(
            envelope(PersonalFabricAction::GamepadForwardEvent {
                forwarding_id: [1; 16],
                lease_nonce: [2; 16],
                sequence: 1,
                event: os_actions::ForwardedGamepadEvent::Button {
                    button: os_actions::GamepadButton::South,
                    pressed: true,
                },
            })
            .validate_at(&source, &target, 10_001)
            .is_ok()
        );
        for action in [
            PersonalFabricAction::GamepadForwardGrant {
                forwarding_id: [1; 16],
                accepted: true,
                lease_nonce: [0; 16],
            },
            PersonalFabricAction::GamepadForwardGrant {
                forwarding_id: [1; 16],
                accepted: false,
                lease_nonce: [2; 16],
            },
            PersonalFabricAction::GamepadForwardEvent {
                forwarding_id: [1; 16],
                lease_nonce: [2; 16],
                sequence: 2,
                event: os_actions::ForwardedGamepadEvent::Hat { x: 2, y: 0 },
            },
        ] {
            assert_eq!(
                envelope(action).validate_at(&source, &target, 10_001),
                Err(FabricError::InvalidAction)
            );
        }
    }

    remote_input_requires_a_short_lived_explicit_lease();
    input_lease_must_be_issued_by_the_target_to_its_controller();
    input_actions_reject_unbound_or_malformed_events();
    gamepad_actions_are_typed_exact_bound_and_nonzero();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn wire_actions_are_fresh_source_bound_and_never_renumbered() {
    fn ephemeral_actions_are_transport_bound_and_fresh() {
        let source = id("source");
        let target = id("target");
        let envelope = PersonalFabricActionEnvelope {
            request_id: [3; 16],
            source: source.clone(),
            target: target.clone(),
            issued_unix_ms: 10_000,
            action: PersonalFabricAction::ResumeMedia {
                session_id: "player:one".into(),
                app_id: "so.tilo.music".into(),
                workspace_id: "11111111-1111-1111-1111-111111111111".into(),
                title: "Track".into(),
                locator: MediaSourceLocator::Workspace {
                    path: "Music/track.flac".into(),
                },
                position_ms: 42,
                resume_playing: true,
                has_video: false,
            },
        };
        assert_eq!(envelope.validate_at(&source, &target, 10_100), Ok(()));
        assert_eq!(
            envelope.validate_at(&id("intruder"), &target, 10_100),
            Err(FabricError::InvalidAction)
        );
        assert_eq!(
            envelope.validate_at(&source, &id("other-target"), 10_100),
            Err(FabricError::InvalidAction)
        );
        assert_eq!(
            envelope.validate_at(&source, &target, 10_000 + MAX_FABRIC_ACTION_AGE_MS + 1),
            Err(FabricError::InvalidAction)
        );
    }

    fn new_action_variants_do_not_renumber_existing_wire_actions() {
        let placement =
            postcard::to_allocvec(&PersonalFabricAction::RetiredSetDisplayPlacement).unwrap();
        let input = postcard::to_allocvec(&PersonalFabricAction::Input {
            lease_nonce: [1; 16],
            event: FabricInputEvent::PointerMotion { x: 0, y: 0 },
        })
        .unwrap();
        let bluetooth = postcard::to_allocvec(&PersonalFabricAction::BluetoothHandoffPrepare {
            address: "AA:BB:CC:DD:EE:FF".into(),
            name: "Headphones".into(),
        })
        .unwrap();
        assert_eq!(placement.first(), Some(&2));
        assert_eq!(input.first(), Some(&5));
        assert_eq!(bluetooth.first(), Some(&6));
        let pairing =
            postcard::to_allocvec(&PersonalFabricAction::BluetoothHandoffPairingOffered {
                request_id: [2; 16],
                address: "AA:BB:CC:DD:EE:FF".into(),
                authorization: [4; 16],
            })
            .unwrap();
        assert_eq!(pairing.first(), Some(&11));
        let nearby = postcard::to_allocvec(&PersonalFabricAction::NearbyDrop {
            file_name: "note.txt".into(),
            content_hash: *blake3::hash(b"hello").as_bytes(),
            bytes: b"hello".to_vec(),
        })
        .unwrap();
        assert_eq!(nearby.first(), Some(&13));
        let notification = postcard::to_allocvec(&PersonalFabricAction::NotificationRetire {
            id: crate::fabric::notification::ContinuityNotificationId {
                epoch: [1; 16],
                token: [2; 16],
            },
        })
        .unwrap();
        assert_eq!(notification.first(), Some(&21));
        for (action, index) in [
            (PersonalFabricAction::RetiredRealtimeOffer, 24),
            (PersonalFabricAction::RetiredRealtimeAccept, 25),
            (PersonalFabricAction::RetiredRealtimeMediaReady, 26),
            (PersonalFabricAction::RetiredRealtimeStopSource, 27),
            (PersonalFabricAction::RetiredRealtimeCancel, 28),
            (PersonalFabricAction::RetiredRemoteCaptureSignal, 38),
            (PersonalFabricAction::RetiredRemoteCaptureChunk, 39),
            (PersonalFabricAction::RetiredDeviceHealthProbe, 40),
            (PersonalFabricAction::RetiredDeviceHealthReport, 41),
            (PersonalFabricAction::RetiredTethering, 44),
        ] {
            assert_eq!(
                postcard::to_allocvec(&action).unwrap().first(),
                Some(&index)
            );
        }
        for (action, index) in [
            (
                PersonalFabricAction::GamepadForwardRequest {
                    forwarding_id: [3; 16],
                    app_id: "so.tilo.game".into(),
                    workspace_id: "workspace".into(),
                    session_id: "session".into(),
                    label: "Controller".into(),
                },
                29,
            ),
            (
                PersonalFabricAction::GamepadForwardGrant {
                    forwarding_id: [3; 16],
                    accepted: true,
                    lease_nonce: [4; 16],
                },
                30,
            ),
            (
                PersonalFabricAction::GamepadForwardEvent {
                    forwarding_id: [3; 16],
                    lease_nonce: [4; 16],
                    sequence: 1,
                    event: os_actions::ForwardedGamepadEvent::Hat { x: 0, y: -1 },
                },
                31,
            ),
            (
                PersonalFabricAction::GamepadForwardStop {
                    forwarding_id: [3; 16],
                    lease_nonce: [4; 16],
                },
                32,
            ),
        ] {
            assert_eq!(
                postcard::to_allocvec(&action).unwrap().first(),
                Some(&index)
            );
        }
    }

    fn notification_actions_are_source_bound_bounded_and_fresh() {
        let source = id("source");
        let target = id("target");
        let notification = crate::fabric::notification::ContinuityNotificationProjection {
            id: crate::fabric::notification::ContinuityNotificationId {
                epoch: [1; 16],
                token: [2; 16],
            },
            app_id: "so.tilo.mail".into(),
            app_name: "Mail".into(),
            title: "New message".into(),
            body: "Preview".into(),
            body_redacted: false,
            actions: vec![("reply".into(), "Reply".into())],
            urgency: 1,
            expires_unix_ms: 20_000,
        };
        let envelope = PersonalFabricActionEnvelope {
            request_id: [3; 16],
            source: source.clone(),
            target: target.clone(),
            issued_unix_ms: 10_000,
            action: PersonalFabricAction::NotificationProject { notification },
        };
        assert!(envelope.validate_at(&source, &target, 10_000).is_ok());
        assert_eq!(
            envelope.validate_at(&id("spoof"), &target, 10_000),
            Err(FabricError::InvalidAction)
        );

        let invalid = PersonalFabricActionEnvelope {
            action: PersonalFabricAction::NotificationInvoke {
                id: crate::fabric::notification::ContinuityNotificationId {
                    epoch: [1; 16],
                    token: [2; 16],
                },
                action_key: "bad\naction".into(),
            },
            ..envelope
        };
        assert_eq!(
            invalid.validate_at(&source, &target, 10_000),
            Err(FabricError::InvalidAction)
        );
    }

    ephemeral_actions_are_transport_bound_and_fresh();
    new_action_variants_do_not_renumber_existing_wire_actions();
    notification_actions_are_source_bound_bounded_and_fresh();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn audio_and_bluetooth_wire_is_versioned_bounded_and_canonical() {
    fn remote_audio_wire_is_versioned_bounded_and_capability_bearing() {
        let source = id("source");
        let target = id("target");
        let envelope = |action| PersonalFabricActionEnvelope {
            request_id: [3; 16],
            source: source.clone(),
            target: target.clone(),
            issued_unix_ms: 10_000,
            action,
        };
        let offer = || PersonalFabricAction::RemoteAudioOffer {
            session_id: [4; 16],
            protocol_version: REMOTE_AUDIO_PROTOCOL_VERSION,
            label: "All system audio".into(),
            sample_rate_hz: REMOTE_AUDIO_SAMPLE_RATE_HZ,
            channels: REMOTE_AUDIO_CHANNELS,
            frame_samples: REMOTE_AUDIO_FRAME_SAMPLES,
        };
        assert_eq!(
            envelope(offer()).validate_at(&source, &target, 10_100),
            Ok(())
        );
        let mut unsupported = offer();
        if let PersonalFabricAction::RemoteAudioOffer {
            protocol_version, ..
        } = &mut unsupported
        {
            *protocol_version = REMOTE_AUDIO_PROTOCOL_VERSION + 1;
        }
        assert_eq!(
            envelope(unsupported).validate_at(&source, &target, 10_100),
            Err(FabricError::InvalidAction)
        );
        assert_eq!(
            envelope(PersonalFabricAction::RemoteAudioDecision {
                session_id: [4; 16],
                accepted: true,
                authorization: None,
            })
            .validate_at(&source, &target, 10_100),
            Err(FabricError::InvalidAction)
        );
        assert_eq!(
            envelope(PersonalFabricAction::RemoteAudioFrame {
                session_id: [4; 16],
                authorization: [5; 16],
                sequence: 1,
                media_ts: 48_000,
                presentation_ns: 1_000_000_000 + REMOTE_AUDIO_GROUP_LATENCY_NS,
                pcm_s16le: vec![0; MAX_REMOTE_AUDIO_FRAME_BYTES],
            })
            .validate_at(&source, &target, 10_100),
            Ok(())
        );
        assert_eq!(
            envelope(PersonalFabricAction::RemoteAudioFrame {
                session_id: [4; 16],
                authorization: [5; 16],
                sequence: 2,
                media_ts: 0,
                presentation_ns: REMOTE_AUDIO_GROUP_LATENCY_NS,
                pcm_s16le: vec![0; MAX_REMOTE_AUDIO_FRAME_BYTES + 4],
            })
            .validate_at(&source, &target, 10_100),
            Err(FabricError::InvalidAction)
        );
        assert_eq!(
            envelope(PersonalFabricAction::RemoteAudioCancel {
                session_id: [4; 16],
            })
            .validate_at(&source, &target, 10_100),
            Ok(())
        );
        assert_eq!(
            envelope(PersonalFabricAction::RemoteAudioCancel {
                session_id: [0; 16],
            })
            .validate_at(&source, &target, 10_100),
            Err(FabricError::InvalidAction)
        );
        assert_eq!(
            envelope(PersonalFabricAction::RemoteAudioKeepalive {
                session_id: [4; 16],
                authorization: [5; 16],
                playout: None,
            })
            .validate_at(&source, &target, 10_100),
            Ok(())
        );
        assert_eq!(
            envelope(PersonalFabricAction::RemoteAudioKeepalive {
                session_id: [4; 16],
                authorization: [0; 16],
                playout: None,
            })
            .validate_at(&source, &target, 10_100),
            Err(FabricError::InvalidAction)
        );
    }

    fn bluetooth_handoff_actions_accept_only_canonical_bounded_ephemeral_identity() {
        let source = id("source");
        let target = id("target");
        let envelope = |action| PersonalFabricActionEnvelope {
            request_id: [3; 16],
            source: source.clone(),
            target: target.clone(),
            issued_unix_ms: 10_000,
            action,
        };
        assert_eq!(
            envelope(PersonalFabricAction::BluetoothHandoffPrepare {
                address: "AA:BB:CC:DD:EE:FF".into(),
                name: "Headphones".into(),
            })
            .validate_at(&source, &target, 10_100),
            Ok(())
        );
        for address in [
            "AA:BB:CC:DD:EE",
            "AA-BB-CC-DD-EE-FF",
            "../../org/bluez/hci0",
            "GG:BB:CC:DD:EE:FF",
        ] {
            assert_eq!(
                envelope(PersonalFabricAction::BluetoothHandoffPrepare {
                    address: address.into(),
                    name: "Headphones".into(),
                })
                .validate_at(&source, &target, 10_100),
                Err(FabricError::InvalidAction)
            );
        }
        assert_eq!(
            envelope(PersonalFabricAction::BluetoothHandoffCommit {
                request_id: [0; 16],
                address: "AA:BB:CC:DD:EE:FF".into(),
            })
            .validate_at(&source, &target, 10_100),
            Err(FabricError::InvalidAction)
        );
        assert_eq!(
            envelope(PersonalFabricAction::BluetoothHandoffPairingOffered {
                request_id: [5; 16],
                address: "AA:BB:CC:DD:EE:FF".into(),
                authorization: [0; 16],
            })
            .validate_at(&source, &target, 10_100),
            Err(FabricError::InvalidAction)
        );
    }

    remote_audio_wire_is_versioned_bounded_and_capability_bearing();
    bluetooth_handoff_actions_accept_only_canonical_bounded_ephemeral_identity();
}

#[test]
fn remote_audio_frames_play_after_capture_within_the_latency_bound() {
    let source = id("source");
    let target = id("target");
    let envelope = |action| PersonalFabricActionEnvelope {
        request_id: [3; 16],
        source: source.clone(),
        target: target.clone(),
        issued_unix_ms: 10_000,
        action,
    };
    for presentation_ns in [999_999_999, 2_000_000_001] {
        assert_eq!(
            envelope(PersonalFabricAction::RemoteAudioFrame {
                session_id: [4; 16],
                authorization: [5; 16],
                sequence: 1,
                media_ts: 48_000,
                presentation_ns,
                pcm_s16le: vec![0; MAX_REMOTE_AUDIO_FRAME_BYTES],
            })
            .validate_at(&source, &target, 10_100),
            Err(FabricError::InvalidAction),
            "a frame played before its capture or beyond the latency bound"
        );
    }
}

#[test]
fn speaker_group_wire_rejects_zero_identities_and_bad_names() {
    use crate::fabric::speaker_group::{
        EndpointSelector, RoomPoint, SpeakerGroupId, SpeakerRole, StereoSide,
    };
    let source = id("source");
    let target = id("target");
    let envelope = |action| PersonalFabricActionEnvelope {
        request_id: [3; 16],
        source: source.clone(),
        target: target.clone(),
        issued_unix_ms: 10_000,
        action,
    };
    let check = |action| envelope(action).validate_at(&source, &target, 10_100);
    let invite = |group, name: &str, output, role| PersonalFabricAction::SpeakerGroupInvite {
        group,
        name: name.into(),
        output,
        role,
    };
    let group = SpeakerGroupId([1; 16]);
    let output = EndpointSelector([2; 16]);
    let right = SpeakerRole::Stereo(StereoSide::Right);
    assert_eq!(check(invite(group, "Living room", output, right)), Ok(()));
    for bad in [
        invite(SpeakerGroupId([0; 16]), "Living room", output, right),
        invite(group, "", output, right),
        invite(group, "line\nbreak", output, right),
        invite(group, "Living room", EndpointSelector([0; 16]), right),
        invite(
            group,
            "Living room",
            output,
            SpeakerRole::RoomPoint(RoomPoint {
                x_mm: i32::MAX,
                y_mm: 0,
                z_mm: 0,
            }),
        ),
        PersonalFabricAction::SpeakerGroupSessionOffer {
            session_id: [0; 16],
            group,
        },
        PersonalFabricAction::SpeakerGroupSessionDecision {
            session_id: [4; 16],
            group,
            authorization: Some([0; 16]),
        },
        PersonalFabricAction::SpeakerGroupSessionStop {
            session_id: [4; 16],
            authorization: Some([0; 16]),
        },
        PersonalFabricAction::SpeakerGroupLeave {
            group: SpeakerGroupId([0; 16]),
        },
    ] {
        assert_eq!(
            check(bad.clone()),
            Err(FabricError::InvalidAction),
            "{bad:?}"
        );
    }
    assert_eq!(
        check(PersonalFabricAction::SpeakerGroupSessionDecision {
            session_id: [4; 16],
            group,
            authorization: None,
        }),
        Ok(())
    );
}

#[test]
fn a_snapshot_cannot_carry_a_group_led_by_another_device() {
    use crate::fabric::speaker_group::{
        EndpointSelector, SpeakerGroup, SpeakerGroupId, SpeakerGroupMember, SpeakerRole,
        SpeakerTrim, StereoSide,
    };
    let mut laptop = snapshot("laptop", 1, 0, FabricCapabilities::default());
    let member = |device: &str, side| SpeakerGroupMember {
        device: id(device),
        output: EndpointSelector([9; 16]),
        role: SpeakerRole::Stereo(side),
        trim: SpeakerTrim::default(),
    };
    laptop.speakers.groups.push(SpeakerGroup {
        id: SpeakerGroupId([5; 16]),
        name: "Living room".into(),
        leader: id("tablet"),
        members: vec![
            member("laptop", StereoSide::Left),
            member("tablet", StereoSide::Right),
        ],
        stamp: stamp("laptop", 1),
    });
    assert_eq!(laptop.validate(), Err(FabricError::ForeignAuthorship));
    laptop.speakers.groups[0].leader = id("laptop");
    assert_eq!(laptop.validate(), Ok(()));
    let mut fabric = DeviceFabric::default();
    fabric.upsert(laptop).unwrap();
    let group = fabric.speaker_group(SpeakerGroupId([5; 16])).unwrap();
    assert_eq!(
        fabric.admit_speaker_group(group),
        Err(FabricError::IneligibleSpeakerMember),
        "the laptop does not offer that output"
    );
}
