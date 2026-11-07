//! What a board gives a surface that draws people here: everyone's word
//! merged into one row each, their pointer where the surface shows it, and
//! one schedule for what this device says.

use collab_types::presence::{Availability, CallPresence, DESKTOP_APP, Payload, PresenceKey};

use super::*;

const WORKSPACE: &str = "6f1b4a7e-0b2f-4d3a-9a1e-2c3d4e5f6a7b";
const OTHER: &str = "0a2c4e6f-8b1d-4f3a-9c5e-7d9b1f3a5c7e";

fn board() -> Board {
    Board::new(WORKSPACE, [0xf0; 32])
}

fn said(payload: Payload) -> Presence {
    Presence::desktop(WORKSPACE, payload)
}

fn pointed(workspace: &str, tab: u64, x: f32, y: f32) -> Presence {
    Presence::pointing(workspace, tab, x, y)
}

#[test]
fn a_members_parts_merge_into_one_row_and_one_call() {
    let mut board = board();
    board.hear([1; 32], &said(Payload::Person { name: "Ada".into() }), 0.0);
    board.hear([1; 32], &said(Payload::Arrived), 1.0);
    board.hear(
        [1; 32],
        &said(Payload::Availability {
            state: "focusing".into(),
            call: Some(
                CallPresence {
                    channel: "c_voice".into(),
                    name: "voice".into(),
                }
                .encode(),
            ),
            camera: true,
            screen: false,
        }),
        2.0,
    );
    board.hear(
        [1; 32],
        &said(Payload::Status {
            text: "  Wiring the plane  ".into(),
            automatic: true,
        }),
        3.0,
    );
    board.certify([1; 32], "aa".repeat(32), "Framework");
    let member = board.member([1; 32]).expect("they are here");
    assert_eq!(member.label(), "Ada");
    assert_eq!(member.status.as_deref(), Some("Wiring the plane"));
    assert_eq!(member.availability, Availability::Focusing);
    assert_eq!(member.device, "Framework");
    let calls = board.calls();
    assert_eq!(calls.len(), 1, "one channel, one call");
    assert_eq!(calls[0].name, "voice");
    assert_eq!(calls[0].people[0].label, "Ada");
    assert!(calls[0].people[0].camera && !calls[0].people[0].screen);
}

#[test]
fn only_a_change_draws_and_only_this_workspace_counts() {
    let mut board = board();
    let frame = said(Payload::Status {
        text: "Reviewing".into(),
        automatic: false,
    });
    assert!(board.hear([1; 32], &frame, 0.0));
    let drawn = board.revision();
    assert!(!board.hear([1; 32], &frame, 1.0), "the same word again");
    assert_eq!(board.revision(), drawn, "nothing to draw again");
    assert!(
        !board.hear([0xf0; 32], &frame, 2.0),
        "this device is nobody else"
    );
    board.hear([2; 32], &Presence::desktop(OTHER, Payload::Arrived), 3.0);
    assert!(
        board.member([2; 32]).is_none(),
        "another workspace's word is no member here"
    );
}

#[test]
fn a_pointer_is_placed_on_the_tile_it_is_over_and_a_rest_draws_nothing() {
    let mut board = board();
    board.hear([1; 32], &pointed(WORKSPACE, 3, 12.0, 34.0), 1_000.0);
    let member = board.member([1; 32]).expect("a pointer says they are here");
    assert!(member.here);
    assert_eq!(member.pointer(1_000.0), Some((3, 12.0, 34.0)));
    let placed = board.placed(&[Tile::new(3, 100.0, 50.0, 400.0, 300.0)], 1_000.0);
    assert_eq!(
        placed,
        vec![Placed {
            publisher: [1; 32],
            tab: 3,
            x: 112.0,
            y: 84.0,
        }]
    );
    assert!(board.pointers(4, 1_000.0).is_empty());
    let drawn = board.revision();
    board.hear([1; 32], &pointed(WORKSPACE, 3, 12.0, 34.0), 1_000.5);
    assert_eq!(board.revision(), drawn, "a resting pointer draws nothing");
    board.hear([1; 32], &Presence::pointer_left(WORKSPACE, 3), 2_000.0);
    assert!(
        board.pointers(3, 2_000.0).is_empty(),
        "and leaving is drawn"
    );
}

#[test]
fn this_device_says_its_whole_self_at_once_and_nothing_unchanged() {
    let mut board = board();
    let own = Own {
        label: "Quinten".into(),
        status: Some("Wiring the plane".into()),
        ..Own::default()
    };
    board.say(&own);
    let first = board.flush(0.0);
    let payloads: Vec<&Payload> = first.iter().map(|(_, frame)| &frame.payload).collect();
    assert!(
        payloads
            .iter()
            .any(|payload| matches!(payload, Payload::Person { name } if name == "Quinten"))
    );
    assert!(
        payloads
            .iter()
            .any(|payload| matches!(payload, Payload::Arrived))
    );
    assert!(
        payloads
            .iter()
            .any(|payload| matches!(payload, Payload::Availability { .. }))
    );
    for (_, frame) in &first {
        assert_eq!(frame.app, DESKTOP_APP);
        assert_eq!(
            frame.key,
            PresenceKey::Workspace {
                id: WORKSPACE.into()
            }
        );
    }
    assert!(board.due(REPEATS_MS[0]).is_some(), "the loss repairs");
    assert!(board.due(REPEATS_MS[1]).is_some());
    board.say(&own);
    assert!(
        board.due(KEEPALIVE_MS - 1.0).is_none(),
        "an unchanged word leaves nothing"
    );
    assert!(
        board.due(KEEPALIVE_MS + REPEATS_MS[1]).is_some(),
        "a quiet device restates itself"
    );
}

#[test]
fn a_wave_is_a_new_wave_each_time_and_an_apps_word_lives_under_its_key() {
    let mut board = board();
    let wave = |seq| {
        said(Payload::Wave {
            seq,
            target: "ada".into(),
        })
    };
    assert!(board.hear([1; 32], &wave(1), 0.0));
    assert!(!board.hear([1; 32], &wave(1), 1.0), "the same wave again");
    assert!(board.hear([1; 32], &wave(2), 2.0), "a new wave");
    let key = PresenceKey::Tab { id: "draft".into() };
    let draft = Presence {
        app: "notes".into(),
        cluster: WORKSPACE.into(),
        key: key.clone(),
        payload: Payload::Composing { composing: true },
    };
    assert!(board.hear([1; 32], &draft, 3.0));
    assert_eq!(
        board.member([1; 32]).and_then(|member| member.app(&key)),
        Some(&Payload::Composing { composing: true })
    );
    let left = Presence {
        payload: Payload::Left,
        ..draft
    };
    assert!(board.hear([1; 32], &left, 4.0));
    assert_eq!(
        board.member([1; 32]).and_then(|member| member.app(&key)),
        None
    );
    assert!(board.depart([1; 32]));
    assert!(!board.depart([1; 32]), "gone once");
}

#[test]
fn a_shell_says_itself_whole_and_only_when_it_changed() {
    use collab_types::presence::{ShellPresence, ShellWindow};
    let view = |label: &str| {
        Box::new(ShellPresence {
            seq: 0,
            sent_at_ms: 0,
            label: label.into(),
            agent: false,
            colour: "#38b8f6".into(),
            activity: None,
            place: Some("2".into()),
            size: [1440, 852],
            windows: vec![ShellWindow {
                app: "so.tilo.notes".into(),
                record: [7; 8],
                rect: [0, 0, 5000, 5000],
                focused: true,
            }],
            selection: None,
            chat: None,
            reaction: None,
        })
    };
    let mut board = board();
    board.say(&Own {
        label: "Quinten".into(),
        shell: Some(view("Quinten")),
        ..Own::default()
    });
    let first = board.flush(0.0);
    assert!(
        first.iter().any(
            |(_, frame)| matches!(&frame.payload, Payload::Shell(shell) if shell.label == "Quinten")
        ),
        "the shell leaves with the first word"
    );
    // A repeat of the same view says nothing, whatever its clock.
    let mut same = view("Quinten");
    same.sent_at_ms = 99;
    board.say(&Own {
        label: "Quinten".into(),
        shell: Some(same),
        ..Own::default()
    });
    assert!(board.due(MIN_INTERVAL_MS).is_none(), "an unchanged view");
    // A different view is one frame with a sequence of its own.
    board.say(&Own {
        label: "Quinten".into(),
        shell: Some(view("Quinten W.")),
        ..Own::default()
    });
    let said = board.due(1_000.0).expect("the new view leaves");
    let shell = said
        .iter()
        .find_map(|(_, frame)| match &frame.payload {
            Payload::Shell(shell) => Some(shell.clone()),
            _ => None,
        })
        .expect("a shell frame");
    assert_eq!(shell.label, "Quinten W.");
    assert!(shell.seq > 1, "a higher sequence than the first view");
}

#[test]
fn stopping_following_is_said_so_nobody_keeps_showing_it() {
    let mut board = board();
    board.say(&Own {
        following: Some("ada".into()),
        ..Own::default()
    });
    let _ = board.flush(0.0);
    board.say(&Own::default());
    let said = board.due(1_000.0).expect("stopping leaves");
    assert!(
        said.iter().any(|(_, frame)| matches!(
            &frame.payload,
            Payload::Following { target } if target.is_none()
        )),
        "a member who stops following says so"
    );
}

#[test]
fn a_member_who_comes_back_is_a_stay_of_its_own() {
    let mut board = board();
    board.hear([1; 32], &said(Payload::Arrived), 0.0);
    let first = board.member([1; 32]).expect("here").epoch;
    assert!(board.here().any(|member| member.publisher == [1; 32]));
    board.hear([1; 32], &said(Payload::Arrived), 1.0);
    assert_eq!(
        board.member([1; 32]).expect("here").epoch,
        first,
        "a restated arrival is the same stay"
    );
    board.hear([1; 32], &said(Payload::Left), 2.0);
    assert!(!board.here().any(|member| member.publisher == [1; 32]));
    board.hear([1; 32], &said(Payload::Arrived), 3.0);
    assert!(
        board.member([1; 32]).expect("here").epoch > first,
        "coming back begins a new stay, never the one before"
    );
}
