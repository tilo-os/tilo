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
        },
        Roster {
            channel_id: "voice".into(),
            people: (3..=120).map(person).collect(),
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
