use tilo_resource::{AppNamed, Replica};

use super::*;

const ASKED: i64 = 1_700_000_000_000;
const APP: (&str, &str, &str) = ("so.tilo.calls", "workspace-1", "Calls");
const NO_FRONT: (&str, &str, &str) = ("", "", "");
const PAD: (&str, &str, &str) = ("", "", "Gamepad");

fn laptop() -> String {
    "22".repeat(32)
}

fn phone() -> String {
    "33".repeat(32)
}

/// A record that keeps the laptop and the phone.
fn record() -> Replica<Devices, AppNamed> {
    let mut record = Replica::<Devices>::new().in_app("so.tilo.model", "Model");
    for key in [laptop(), phone()] {
        record
            .apply(Devices::KEEP_FACT.with((
                key,
                "00".to_owned(),
                "seen".to_owned(),
                "00".to_owned(),
            )))
            .expect("the record keeps the device");
    }
    record
}

fn state(record: &Replica<Devices, AppNamed>) -> Devices {
    record.state().expect("the record reads")
}

fn kept(record: &Replica<Devices, AppNamed>, id: [u8; 16]) -> RemoteUse {
    state(record)
        .uses
        .get(&Id::from_raw(hex::encode(id)))
        .cloned()
        .expect("the use is kept")
}

fn steps(turns: &[Turn]) -> Vec<Step> {
    turns.iter().map(|turn| turn.step).collect()
}

fn ask(
    record: &mut Replica<Devices, AppNamed>,
    id: [u8; 16],
    local: &str,
    other: &str,
    profile: &Profile,
    at: i64,
) -> Result<(), String> {
    let held = state(record);
    let named = if *profile == Profile::Controller {
        PAD
    } else {
        APP
    };
    let write =
        asking(&held, id, local, other, profile, named, at).map_err(|why| why.to_string())?;
    record
        .apply(write)
        .map(drop)
        .map_err(|why| format!("{why:?}"))
}

/// The laptop asks for the phone's camera for the app it has in front; only the phone asks its
/// person. On their yes the phone sends and the laptop stands a virtual camera in, each once; the
/// phone renews the grant while it sends, so it lasts; once it stops renewing the grant runs out
/// five minutes after the last renewal, both devices stop, and nothing is begun again.
#[test]
fn a_camera_is_granted_by_its_device_kept_while_it_sends_and_runs_out_once_it_stops() {
    let (laptop, phone) = (laptop(), phone());
    let mut record = record();
    let id = [1; 16];
    ask(
        &mut record,
        id,
        &laptop,
        &phone,
        &Profile::usual(RemoteKind::Camera),
        ASKED,
    )
    .unwrap();
    let asked = kept(&record, id);
    assert_eq!(
        (asked.from.as_str(), asked.to.as_str()),
        (phone.as_str(), laptop.as_str())
    );
    assert_eq!(asked.decider(), phone);
    assert_eq!(asked.app, "so.tilo.calls");

    let (mut at_laptop, mut at_phone) = (Part::default(), Part::default());
    let both = |_: &str| true;
    let (writes, work) = turns(&state(&record), &laptop, both, ASKED + 1, &mut at_laptop);
    assert!(
        writes.is_empty() && work.is_empty(),
        "the laptop that asked decides nothing"
    );
    let (writes, work) = turns(&state(&record), &phone, both, ASKED + 1, &mut at_phone);
    assert!(writes.is_empty());
    assert_eq!(steps(&work), [Step::Decide]);
    let (_, again) = turns(&state(&record), &phone, both, ASKED + 2, &mut at_phone);
    assert!(again.is_empty(), "the phone's person is asked once");

    let granted = ASKED + 5_000;
    record
        .apply(answering(&hex::encode(id), true, NO_FRONT, granted))
        .unwrap();
    let use_ = kept(&record, id);
    assert_eq!(use_.stage, RemoteStage::Granted);
    assert_eq!(
        use_.until_ms,
        granted + GRANT_MS,
        "a grant lasts five minutes"
    );

    let (_, work) = turns(&state(&record), &phone, both, granted + 1, &mut at_phone);
    assert_eq!(steps(&work), [Step::Send]);
    at_phone.running(&hex::encode(id));
    let (_, work) = turns(&state(&record), &laptop, both, granted + 1, &mut at_laptop);
    assert_eq!(steps(&work), [Step::Receive]);
    at_laptop.running(&hex::encode(id));

    // Past half its grant, the phone that sends renews it, once per grant.
    let half = granted + GRANT_MS / 2 + 1;
    let (writes, work) = turns(&state(&record), &phone, both, half, &mut at_phone);
    assert!(work.is_empty());
    assert_eq!(writes.len(), 1, "the sending phone renews its grant");
    let (writes, _) = turns(&state(&record), &phone, both, half + 1, &mut at_phone);
    assert!(writes.is_empty(), "a grant is renewed once");
    let (writes, _) = turns(&state(&record), &laptop, both, half, &mut at_laptop);
    assert!(writes.is_empty(), "only the device that sends renews");
    record
        .apply(Devices::RENEW_REMOTE_USE.with((hex::encode(id), half)))
        .unwrap();
    assert_eq!(kept(&record, id).until_ms, half + GRANT_MS);

    // The phone stops sending: nobody renews, and five minutes after the last renewal both end.
    let out = half + GRANT_MS;
    let (writes, work) = turns(&state(&record), &laptop, both, out, &mut at_laptop);
    assert_eq!(writes.len(), 1, "a grant that ran out is ended");
    assert!(work.is_empty());
    for write in writes {
        record.apply(write).unwrap();
    }
    assert_eq!(kept(&record, id).stage, RemoteStage::Ended);
    let (writes, work) = turns(&state(&record), &phone, both, out + 1, &mut at_phone);
    assert!(writes.is_empty());
    assert_eq!(steps(&work), [Step::Stop]);
    at_phone.stopped(&hex::encode(id));
    let (_, work) = turns(&state(&record), &laptop, both, out + 1, &mut at_laptop);
    assert_eq!(steps(&work), [Step::Stop]);
    at_laptop.stopped(&hex::encode(id));
    let (writes, work) = turns(&state(&record), &phone, both, out + 2, &mut at_phone);
    assert!(
        writes.is_empty() && work.is_empty(),
        "what ended is begun nowhere again"
    );
}

/// A no, an ask nobody answers in thirty seconds, and a use whose other device leaves all end
/// with nothing sent: the one that left is given up by the device still there, and the device
/// that receives stops its virtual device on the pass that reads it.
#[test]
fn a_declined_unanswered_or_abandoned_use_ends_with_nothing_left_running() {
    let (laptop, phone) = (laptop(), phone());
    let both = |_: &str| true;

    let mut record = record();
    ask(
        &mut record,
        [2; 16],
        &laptop,
        &phone,
        &Profile::usual(RemoteKind::Microphone),
        ASKED,
    )
    .unwrap();
    record
        .apply(answering(&hex::encode([2; 16]), false, NO_FRONT, ASKED + 1))
        .unwrap();
    assert_eq!(kept(&record, [2; 16]).stage, RemoteStage::Declined);
    let (_, work) = turns(
        &state(&record),
        &laptop,
        both,
        ASKED + 2,
        &mut Part::default(),
    );
    assert!(work.is_empty(), "a declined use stands nothing in");

    let mut record = self::record();
    ask(
        &mut record,
        [3; 16],
        &laptop,
        &phone,
        &Profile::usual(RemoteKind::Scanner),
        ASKED,
    )
    .unwrap();
    let mut part = Part::default();
    let (writes, _) = turns(
        &state(&record),
        &laptop,
        both,
        ASKED + ASK_WAIT_MS - 1,
        &mut part,
    );
    assert!(writes.is_empty(), "the person has thirty seconds to answer");
    let (writes, _) = turns(
        &state(&record),
        &laptop,
        both,
        ASKED + ASK_WAIT_MS,
        &mut part,
    );
    for write in writes {
        record.apply(write).unwrap();
    }
    assert_eq!(kept(&record, [3; 16]).stage, RemoteStage::GivenUp);

    let mut record = self::record();
    ask(
        &mut record,
        [4; 16],
        &laptop,
        &phone,
        &Profile::usual(RemoteKind::Camera),
        ASKED,
    )
    .unwrap();
    record
        .apply(answering(&hex::encode([4; 16]), true, NO_FRONT, ASKED + 1))
        .unwrap();
    let mut at_laptop = Part::default();
    let (_, work) = turns(&state(&record), &laptop, both, ASKED + 2, &mut at_laptop);
    assert_eq!(steps(&work), [Step::Receive]);
    at_laptop.running(&hex::encode([4; 16]));
    let phone_gone = |key: &str| key != phone;
    let (writes, work) = turns(
        &state(&record),
        &laptop,
        phone_gone,
        ASKED + 3,
        &mut at_laptop,
    );
    assert!(work.is_empty());
    for write in writes {
        record.apply(write).unwrap();
    }
    assert_eq!(kept(&record, [4; 16]).stage, RemoteStage::GivenUp);
    let (_, work) = turns(
        &state(&record),
        &laptop,
        phone_gone,
        ASKED + 4,
        &mut at_laptop,
    );
    assert_eq!(
        steps(&work),
        [Step::Stop],
        "the virtual camera goes with the phone"
    );
}

/// The record keeps only uses between two of the person's devices, carrying what a remote use
/// carries: a scanner of at most 1024 pages and 64 MiB, and at most eight cameras, microphones
/// and scanners, and eight controllers, on one device at once.
#[test]
fn the_record_keeps_only_bounded_uses_between_two_of_the_persons_devices() {
    let (laptop, phone) = (laptop(), phone());
    let mut record = record();
    let stranger = "44".repeat(32);
    let camera = Profile::usual(RemoteKind::Camera);
    assert!(ask(&mut record, [5; 16], &laptop, &laptop, &camera, ASKED).is_err());
    assert!(ask(&mut record, [5; 16], &laptop, &stranger, &camera, ASKED).is_err());
    let too_long = Profile::Scanner {
        format: DocumentFormat::Pdf,
        max_pages: MAX_PAGES + 1,
        max_document_bytes: MAX_DOCUMENT_BYTES,
    };
    assert!(ask(&mut record, [5; 16], &laptop, &phone, &too_long, ASKED).is_err());
    let too_big = Profile::Scanner {
        format: DocumentFormat::Pdf,
        max_pages: MAX_PAGES,
        max_document_bytes: MAX_DOCUMENT_BYTES + 1,
    };
    assert!(ask(&mut record, [5; 16], &laptop, &phone, &too_big, ASKED).is_err());

    for n in 1..=8_u8 {
        ask(&mut record, [n; 16], &laptop, &phone, &camera, ASKED).unwrap();
    }
    assert!(
        ask(&mut record, [9; 16], &laptop, &phone, &camera, ASKED).is_err(),
        "a ninth capture is refused"
    );
    for n in 10..=17_u8 {
        ask(
            &mut record,
            [n; 16],
            &phone,
            &laptop,
            &Profile::Controller,
            ASKED,
        )
        .unwrap();
    }
    assert!(
        ask(
            &mut record,
            [18; 16],
            &phone,
            &laptop,
            &Profile::Controller,
            ASKED
        )
        .is_err(),
        "a ninth controller is refused"
    );
    // Once the asks ran out, the device takes as many again.
    let later = ASKED + ASK_WAIT_MS;
    ask(&mut record, [19; 16], &laptop, &phone, &camera, later).unwrap();
}

/// A controller goes the other way: the device it is plugged into asks, the device it plays on
/// decides, and its grant reaches the one app session its person had in front; it is a five
/// minute lease nobody renews.
#[test]
fn a_controller_reaches_the_app_session_in_front_where_it_plays_for_five_minutes() {
    let (laptop, phone) = (laptop(), phone());
    let both = |_: &str| true;
    let mut record = record();
    let id = [20; 16];
    ask(
        &mut record,
        id,
        &phone,
        &laptop,
        &Profile::Controller,
        ASKED,
    )
    .unwrap();
    let asked = kept(&record, id);
    assert_eq!(
        (asked.from.as_str(), asked.to.as_str()),
        (phone.as_str(), laptop.as_str())
    );
    assert_eq!(asked.decider(), laptop);
    let (_, work) = turns(
        &state(&record),
        &laptop,
        both,
        ASKED + 1,
        &mut Part::default(),
    );
    assert_eq!(steps(&work), [Step::Decide]);
    assert!(
        record
            .apply(answering(&hex::encode(id), true, NO_FRONT, ASKED + 2))
            .is_err(),
        "a yes names the app session in front"
    );
    record
        .apply(answering(
            &hex::encode(id),
            true,
            ("so.tilo.game", "workspace-2", "window-7"),
            ASKED + 2,
        ))
        .unwrap();
    let granted = kept(&record, id);
    assert_eq!(
        (
            granted.app.as_str(),
            granted.workspace.as_str(),
            granted.session.as_str()
        ),
        ("so.tilo.game", "workspace-2", "window-7")
    );
    assert_eq!(granted.until_ms, ASKED + 2 + GRANT_MS);
    record
        .apply(Devices::RENEW_REMOTE_USE.with((hex::encode(id), ASKED + GRANT_MS / 2 + 10)))
        .unwrap();
    assert_eq!(
        kept(&record, id).until_ms,
        ASKED + 2 + GRANT_MS,
        "a controller's lease is fixed"
    );
    let mut at_phone = Part::default();
    at_phone.running(&hex::encode(id));
    let (writes, _) = turns(
        &state(&record),
        &phone,
        both,
        ASKED + GRANT_MS / 2 + 10,
        &mut at_phone,
    );
    assert!(writes.is_empty(), "nobody renews a controller's lease");
}
