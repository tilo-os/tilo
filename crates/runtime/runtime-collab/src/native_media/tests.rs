use super::*;

#[test]
fn join_cannot_carry_microphone_intent_or_a_caller_route() {
    for bytes in [
        br#"{"version":1,"intent":0,"command":{"op":"join","channel_id":"voice","muted":false}}"#.as_slice(),
        br#"{"version":1,"intent":0,"command":{"op":"join","channel_id":"voice"},"app_id":"other"}"#,
        br#"{"version":1,"intent":0,"command":{"op":"join","channel_id":"voice","call_id":"invented"}}"#,
    ] {
        assert!(Request::decode(bytes).is_err());
    }
    assert_eq!(
        Request::decode(
            br#"{"version":1,"intent":0,"command":{"op":"join","channel_id":"voice"}}"#
        )
        .unwrap()
        .command,
        Command::Join {
            channel_id: "voice".into()
        }
    );
}

#[test]
fn closed_versioned_bounded_commands_round_trip() {
    for command in [
        Command::Join {
            channel_id: "voice".into(),
        },
        Command::SetMuted { muted: false },
        Command::SelectInput {
            input_id: "source-1".into(),
        },
        Command::Leave,
        Command::Status,
    ] {
        let request = Request::new(command);
        assert_eq!(
            Request::decode(&request.encode().unwrap()).unwrap(),
            request
        );
    }
    assert!(Request::decode(br#"{"version":2,"intent":0,"command":{"op":"status"}}"#).is_err());
    assert!(
        Request::decode(br#"{"version":1,"intent":0,"command":{"op":"unmute_remote"}}"#).is_err()
    );
    assert!(Request::decode(&vec![b' '; MAX_WIRE_BYTES + 1]).is_err());
    for channel_id in [String::new(), "x".repeat(257), "voice\nother".into()] {
        assert!(Request::new(Command::Join { channel_id }).encode().is_err());
    }
}

#[test]
fn snapshots_cannot_claim_capture_while_muted_or_unavailable() {
    let mut snapshot = Snapshot::unavailable();
    assert_eq!(
        Snapshot::decode(&snapshot.encode().unwrap()).unwrap(),
        snapshot
    );
    snapshot.capture_active = true;
    assert!(snapshot.encode().is_err());
    snapshot.state = State::Live;
    assert!(snapshot.encode().is_err());
    snapshot.muted = false;
    snapshot.input_id = Some("source-1".into());
    assert!(snapshot.encode().is_ok());
}

#[test]
fn snapshot_inputs_are_bounded_and_unique() {
    let mut snapshot = Snapshot::unavailable();
    let input = Input {
        id: "source".into(),
        label: "Microphone".into(),
    };
    snapshot.inputs = vec![input.clone(), input];
    assert!(snapshot.encode().is_err());
    snapshot.inputs = (0..=MAX_INPUTS)
        .map(|i| Input {
            id: i.to_string(),
            label: "Mic".into(),
        })
        .collect();
    assert!(snapshot.encode().is_err());
}

#[test]
fn zero_intent_only_queries_and_max_only_tears_down() {
    assert!(Request::new(Command::Status).dispatch_valid());
    let mut request = Request::new(Command::SetMuted { muted: false });
    assert!(!request.dispatch_valid());
    request.intent = 1;
    assert!(request.dispatch_valid());
    request.intent = u64::MAX;
    assert!(!request.dispatch_valid());
    request.command = Command::Leave;
    assert!(request.dispatch_valid());
}

#[test]
fn publisher_inventory_is_current_bounded_and_canonical() {
    let call = "a".repeat(64);
    let own = format!("{call}/{}", "1".repeat(64));
    let remote = format!("{call}/{}", "2".repeat(64));
    let mut snapshot = Snapshot::unavailable();
    snapshot.state = State::Live;
    snapshot.inventory_revision = Some(4);
    snapshot.publication = Some(own.clone());
    snapshot.publishers = vec![remote.clone()];
    assert!(snapshot.encode().is_ok());
    snapshot.publication = None; // A Reader may listen without a publication.
    assert!(snapshot.encode().is_ok());
    snapshot.inventory_revision = None;
    assert!(snapshot.encode().is_err());
    snapshot.inventory_revision = Some(4);
    for publishers in [
        vec![remote.clone(), remote.clone()],
        vec![remote.to_uppercase()],
        vec![format!("{}/{}", "b".repeat(64), "2".repeat(64))],
        vec![own.clone()],
    ] {
        snapshot.publication = Some(own.clone());
        snapshot.publishers = publishers;
        assert!(snapshot.encode().is_err());
    }
    snapshot.publication = None;
    // A call has no participant limit.
    snapshot.publishers = (1..=200).map(|n| format!("{call}/{n:064x}")).collect();
    assert!(snapshot.encode().is_ok());
    snapshot.publishers = vec![remote];
    for state in [
        State::Ended,
        State::Unavailable,
        State::Reconnecting,
        State::Joining,
    ] {
        snapshot.state = state;
        assert!(
            snapshot.encode().is_err(),
            "stale carrier inventory must be withdrawn"
        );
    }
}

#[test]
fn rosters_name_people_once_per_channel() {
    let person = |n: u8| hex_person(n);
    let mut snapshot = Snapshot::unavailable();
    snapshot.state = State::Idle;
    snapshot.issue = None;
    snapshot.rosters = vec![
        Roster {
            channel_id: "lounge".into(),
            people: vec![person(1), person(2)],
            ..Default::default()
        },
        Roster {
            channel_id: "voice".into(),
            people: (3..=120).map(person).collect(),
            ..Default::default()
        },
    ];
    let bytes = snapshot.encode().unwrap();
    assert_eq!(Snapshot::decode(&bytes).unwrap(), snapshot);
    for people in [
        vec![person(2), person(1)],
        vec![person(1), person(1)],
        vec![person(10).to_uppercase()],
        vec!["ana".to_owned()],
    ] {
        let mut invalid = snapshot.clone();
        invalid.rosters[0].people = people;
        assert!(invalid.encode().is_err());
    }
    let mut unordered = snapshot;
    unordered.rosters.reverse();
    assert!(unordered.encode().is_err());
}

fn hex_person(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

#[test]
fn a_watch_is_a_query_and_speaking_names_people_once_each() {
    let watch = Request::new(Command::Watch { revision: 7 });
    assert!(watch.dispatch_valid(), "a watch is a query: intent zero");
    let bytes = watch.encode().unwrap();
    assert_eq!(Request::decode(&bytes).unwrap(), watch);
    let mut effect = watch;
    effect.intent = 3;
    assert!(
        !effect.dispatch_valid(),
        "a watch changes nothing, so carries no intent"
    );

    let mut snapshot = Snapshot::unavailable();
    snapshot.speaking = vec![hex_person(1), hex_person(2)];
    snapshot.revision = 42;
    let bytes = snapshot.encode().unwrap();
    assert_eq!(Snapshot::decode(&bytes).unwrap(), snapshot);
    for speaking in [
        vec![hex_person(2), hex_person(1)],
        vec![hex_person(1), hex_person(1)],
        vec!["ana".to_owned()],
    ] {
        let mut invalid = snapshot.clone();
        invalid.speaking = speaking;
        assert!(invalid.encode().is_err());
    }
}

fn person(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

#[test]
fn video_commands_round_trip_and_shown_pictures_are_bounded_sorted_and_layered() {
    let shown = |p: u8, source, layer| Shown {
        person: person(p),
        source,
        layer,
    };
    for command in [
        Command::SetCamera { on: true },
        Command::SetScreen { on: false },
        Command::Pictures { after: 7 },
        Command::Show {
            pictures: vec![
                shown(1, PictureSource::Camera, PictureLayer::Mid),
                shown(1, PictureSource::Screen, PictureLayer::High),
                shown(2, PictureSource::Camera, PictureLayer::Low),
            ],
        },
    ] {
        let request = Request::new(command);
        assert_eq!(
            Request::decode(&request.encode().unwrap()).unwrap(),
            request
        );
    }
    // A screen has no middle layer.
    assert!(
        !Request::new(Command::Show {
            pictures: vec![shown(1, PictureSource::Screen, PictureLayer::Mid)],
        })
        .valid()
    );
    // One entry per person and source, in order.
    assert!(
        !Request::new(Command::Show {
            pictures: vec![
                shown(2, PictureSource::Camera, PictureLayer::Low),
                shown(1, PictureSource::Camera, PictureLayer::Low),
            ],
        })
        .valid()
    );
    assert!(
        !Request::new(Command::Show {
            pictures: vec![
                shown(1, PictureSource::Camera, PictureLayer::Low),
                shown(1, PictureSource::Camera, PictureLayer::High),
            ],
        })
        .valid()
    );
    assert!(
        !Request::new(Command::Show {
            pictures: (0..=MAX_SHOWN)
                .map(|n| shown(
                    u8::try_from(n).unwrap(),
                    PictureSource::Camera,
                    PictureLayer::Low
                ))
                .collect(),
        })
        .valid()
    );
    // Collecting and showing pictures carry no intent; turning a camera on
    // does.
    for command in [
        Command::Pictures { after: 0 },
        Command::Show {
            pictures: Vec::new(),
        },
    ] {
        let mut request = Request::new(command);
        assert!(!request.carries_intent() && request.dispatch_valid());
        request.intent = 1;
        assert!(!request.dispatch_valid());
    }
    let mut camera = Request::new(Command::SetCamera { on: true });
    assert!(camera.carries_intent() && !camera.dispatch_valid());
    camera.intent = 1;
    assert!(camera.dispatch_valid());
}

#[test]
fn a_pictures_answer_carries_whole_objects_in_base64_and_nothing_shows_outside_a_call() {
    let mut snapshot = Snapshot::unavailable();
    snapshot.state = State::Live;
    snapshot.camera = true;
    snapshot.pictures = vec![Picture {
        person: person(3),
        stream: "a".repeat(129),
        source: PictureSource::Camera,
        object: vec![1, 0, 1, 1, 0xff, 0x80],
    }];
    snapshot.pictures_cursor = 4;
    let bytes = snapshot.encode().unwrap();
    assert!(
        std::str::from_utf8(&bytes)
            .unwrap()
            .contains("\"AQABAf+A\""),
        "the object travels as base64"
    );
    assert_eq!(Snapshot::decode(&bytes).unwrap(), snapshot);

    // The largest picture fits one answer.
    snapshot.pictures[0].object = vec![7; MAX_PICTURE_BYTES];
    assert!(snapshot.encode().is_ok());
    snapshot.pictures[0].object = vec![7; MAX_PICTURE_BYTES + 1];
    assert!(snapshot.encode().is_err());
    snapshot.pictures[0].object = Vec::new();
    assert!(snapshot.encode().is_err());
    snapshot.pictures.clear();

    snapshot.state = State::Idle;
    assert!(snapshot.encode().is_err(), "a camera is on only in a call");
    snapshot.camera = false;
    assert!(snapshot.encode().is_ok());
}

#[test]
fn a_roster_names_cameras_and_screens_only_of_its_people() {
    let mut roster = Roster {
        channel_id: "voice".into(),
        people: vec![person(1), person(2)],
        cameras: vec![person(2)],
        screens: vec![person(1)],
    };
    assert!(roster.valid());
    roster.cameras = vec![person(3)];
    assert!(!roster.valid(), "someone not in the call shows no camera");
    roster.cameras = vec![person(2), person(1)];
    assert!(!roster.valid(), "sorted");
}

#[test]
fn a_picture_names_its_coded_unit_and_refuses_one_that_does_not_hold_together() {
    let mut object = vec![1, 1, 1, 2];
    object.extend_from_slice(&7_u64.to_le_bytes());
    object.extend_from_slice(&1_000_u64.to_le_bytes());
    object.extend_from_slice(&640_u16.to_le_bytes());
    object.extend_from_slice(&360_u16.to_le_bytes());
    object.extend_from_slice(&3_u32.to_le_bytes());
    object.extend_from_slice(&[0; 4]);
    object.extend_from_slice(&[0, 0, 1]);
    let mut picture = Picture {
        person: person(1),
        stream: "s".into(),
        source: PictureSource::Camera,
        object,
    };
    assert_eq!(
        picture.unit(),
        Some(PictureUnit {
            keyframe: true,
            codec: PictureCodec::H264,
            width: 640,
            height: 360,
            payload: &[0, 0, 1],
        })
    );
    let unit = picture.unit().unwrap();
    assert_eq!(
        unit.to_object(PictureLayer::High, 7, 1_000),
        picture.object,
        "a unit frames back into the same object"
    );
    picture.object.push(9);
    assert_eq!(picture.unit(), None, "the length names the payload");
    picture.object.pop();
    picture.object[0] = 2;
    assert_eq!(picture.unit(), None, "another version");
}
