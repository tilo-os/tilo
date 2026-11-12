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
    assert_eq!(
        member.status,
        Some(Status {
            text: "Wiring the plane".into(),
            automatic: true,
        }),
        "what the device read off their steps stays told apart from their own words"
    );
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
fn a_pointer_is_said_in_the_pixels_of_its_tile_and_a_rest_draws_nothing() {
    let mut board = board();
    board.hear([1; 32], &pointed(WORKSPACE, 3, 12.0, 34.0), 1_000.0);
    let member = board.member([1; 32]).expect("a pointer says they are here");
    assert!(member.here);
    assert_eq!(member.pointer(1_000.0), Some((3, 12.0, 34.0)));
    let over = Tile::new(3, 100.0, 50.0, 400.0, 300.0);
    assert_eq!(
        board.pointers(3, 1_000.0),
        vec![([1; 32], 12.0, 34.0)],
        "the pointer is said in the pixels of the tile it is over, for the surface to place \
         where that tile stands"
    );
    assert_eq!(over.point((12.0, 34.0)), (112.0, 84.0));
    assert!(
        board.pointers(4, 1_000.0).is_empty(),
        "a pointer is only over the tab it is over"
    );
    let drawn = board.revision();
    board.hear([1; 32], &pointed(WORKSPACE, 3, 12.0, 34.0), 1_000.5);
    assert_eq!(board.revision(), drawn, "a resting pointer draws nothing");
    assert_eq!(
        board
            .member([1; 32])
            .expect("still here")
            .pointer_heard_at(),
        Some(1_000.5),
        "a resting pointer said again is still there"
    );
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
        status: Some(Status {
            text: "Wiring the plane".into(),
            automatic: false,
        }),
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
        on_behalf: None,
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

/// The plane says a member's presence ended with their session (an empty body,
/// `plane::Heard::left`): the board takes them down at once, without waiting
/// out their silence, and a later word of theirs is a new stay.
#[test]
fn a_member_whose_presence_ended_is_taken_down_at_once() {
    let mut board = board();
    let arrived = said(Payload::Arrived).encode().expect("a frame");
    assert!(board.heard([1; 32], &arrived, 0.0));
    assert!(
        board.heard([1; 32], &[], 1.0),
        "the end of their presence is a change"
    );
    assert!(board.member([1; 32]).is_none(), "they are gone at once");
    assert!(!board.heard([1; 32], &[], 2.0), "and gone only once");
    assert!(board.heard([1; 32], &arrived, 3.0));
    assert!(
        board.here().any(|member| member.publisher == [1; 32]),
        "back again"
    );
}

#[test]
fn a_member_is_drawn_from_the_word_the_board_merged() {
    let mut board = board();
    board.hear([1; 32], &said(Payload::Person { name: "Ada".into() }), 0.0);
    board.hear(
        [1; 32],
        &said(Payload::Status {
            text: "Reading".into(),
            automatic: false,
        }),
        0.0,
    );
    board.hear([1; 32], &pointed(WORKSPACE, 7, 12.0, 24.0), 1_000.0);
    board.certify([1; 32], "aa".repeat(32), "Framework");
    let member = board.member([1; 32]).expect("they are here");
    let shown = shown(member, 1_500.0);
    assert_eq!(shown.publisher, hex::encode([1; 32]));
    assert_eq!(shown.label, "Ada");
    assert_eq!(shown.activity.as_deref(), Some("Reading"));
    assert_eq!(shown.device, "Framework");
    assert_eq!(shown.user, Some("aa".repeat(32)));
    assert!(!shown.agent, "a person is not an agent");
    let pointer = shown.pointer.as_ref().expect("their pointer");
    assert_eq!(pointer.tab, "7");
    assert!((pointer.at[0] - 12.0).abs() < 1.0 && (pointer.at[1] - 24.0).abs() < 1.0);
    assert!(
        (pointer.age_ms - 500.0).abs() < f64::EPSILON,
        "how long ago they said it: {}",
        pointer.age_ms
    );
    // A page hands this record to its JavaScript whole: the names it reads
    // are the ones here.
    let json = serde_json::to_value(&shown).expect("a member is JSON");
    assert_eq!(json["pointer"]["ageMs"], 500.0);
    assert_eq!(json["pointer"]["tab"], "7");
    assert_eq!(json["activity"], "Reading");
}

#[test]
fn this_seats_own_row_is_what_this_device_said_here() {
    let mut board = board();
    let own = Own {
        label: "Quinten".into(),
        device: "Framework".into(),
        user: Some("bb".repeat(32)),
        availability: Availability::Focusing,
        ..Own::default()
    };
    board.seed(own.clone());
    assert_eq!(
        board.own(),
        &own,
        "a page says who it is before its first word"
    );
    assert!(
        board.due(0.0).is_none(),
        "knowing who it is says nothing itself"
    );
    let shown = own_shown(board.own(), [0xf0; 32]);
    assert_eq!(shown.publisher, hex::encode([0xf0; 32]));
    assert_eq!(shown.label, "Quinten");
    assert_eq!(shown.device, "Framework");
    assert_eq!(shown.availability, "focusing");
    assert!(shown.pointer.is_none());
}

#[test]
fn a_status_taken_back_is_said_and_its_reader_forgets_it() {
    let mut speaker = board();
    let doing = Own {
        label: "Ada".into(),
        status: Some(Status {
            text: "Editing ‘Plan’ in Notes".into(),
            automatic: true,
        }),
        ..Own::default()
    };
    speaker.say(&doing);
    let mut reader = board();
    for (_, frame) in speaker.flush(0.0) {
        reader.hear([1; 32], &frame, 1.0);
    }
    assert_eq!(
        reader.member([1; 32]).and_then(|member| member.activity()),
        Some(("Editing ‘Plan’ in Notes", true))
    );
    speaker.say(&Own {
        status: None,
        ..doing
    });
    let burst = speaker.flush(2.0);
    assert!(
        burst.iter().any(
            |(_, frame)| matches!(&frame.payload, Payload::Status { text, .. } if text.is_empty())
        ),
        "clearing is a frame of its own"
    );
    for (_, frame) in burst {
        reader.hear([1; 32], &frame, 3.0);
    }
    assert_eq!(
        reader.member([1; 32]).and_then(|member| member.activity()),
        None
    );
}

#[test]
fn a_member_who_goes_quiet_is_forgotten_and_one_who_keeps_talking_is_not() {
    let mut board = board();
    board.say(&Own {
        label: "Me".into(),
        ..Own::default()
    });
    board.due(0.0);
    for member in [[1; 32], [2; 32]] {
        board.hear(member, &said(Payload::Arrived), 0.0);
    }
    // Only the second keeps saying itself, as every device here does on its
    // keepalive; the first closed without a word.
    let mut now = 0.0;
    for _ in 0..4 {
        now += KEEPALIVE_MS;
        board.hear([2; 32], &said(Payload::Arrived), now);
        board.due(now);
    }
    assert!(now >= SILENT_MS, "longer than a member may stay quiet");
    let here: Vec<Device> = board.here().map(|member| member.publisher).collect();
    assert_eq!(
        here,
        vec![[2; 32]],
        "the quiet member is gone, the other stays"
    );
    assert!(board.member([1; 32]).is_none(), "nothing of it is kept");
    let revision = board.revision();
    assert!(
        !board.forget_silent(now),
        "forgetting again changes nothing"
    );
    assert_eq!(board.revision(), revision);
}

/// A page busy with one slow frame hears nobody meanwhile: what the others said waits for it. The
/// time it could not listen is not their silence, so nobody leaves its roster for it; a member
/// that stays quiet while it listens again still does.
#[test]
fn a_device_that_could_not_listen_forgets_nobody_for_it() {
    let mut board = board();
    board.say(&Own {
        label: "Me".into(),
        ..Own::default()
    });
    board.due(0.0);
    for member in [[1; 32], [2; 32]] {
        board.hear(member, &said(Payload::Arrived), 0.0);
    }
    // One frame took longer than a member may stay quiet.
    let mut now = SILENT_MS + 1_000.0;
    board.due(now);
    let here: Vec<Device> = board.here().map(|member| member.publisher).collect();
    assert_eq!(here, vec![[1; 32], [2; 32]], "both are still here");
    // What the second said while the page drew arrives; the first has gone quiet.
    board.hear([2; 32], &said(Payload::Arrived), now);
    for _ in 0..4 {
        now += KEEPALIVE_MS;
        board.hear([2; 32], &said(Payload::Arrived), now);
        board.due(now);
    }
    let here: Vec<Device> = board.here().map(|member| member.publisher).collect();
    assert_eq!(
        here,
        vec![[2; 32]],
        "the one quiet while it listened is gone"
    );
}

#[test]
fn a_member_only_the_roster_named_is_not_forgotten_for_silence() {
    let mut board = board();
    board.certify([1; 32], "ada".into(), "laptop");
    assert!(!board.forget_silent(SILENT_MS * 10.0));
    assert!(board.member([1; 32]).is_some());
}

/// The workspace's host speaks for the runs it executes: each is a member of
/// its own, an agent named as the host says, spoken by the host. The same
/// word from any other device is not heard, and the runs leave with the host
/// or when the governance pins another host.
#[test]
fn the_host_speaks_for_each_run_it_executes_and_nobody_else_does() {
    use collab_types::presence::{ShellPresence, stand_in};
    let (host, member) = ([3; 32], [4; 32]);
    let run = "agent-run:9";
    let face = Presence::desktop(
        WORKSPACE,
        Payload::Shell(Box::new(ShellPresence {
            seq: 1,
            sent_at_ms: 1,
            label: "Visa backend".into(),
            agent: true,
            colour: "#81a8ff".into(),
            activity: Some("New note".into()),
            place: None,
            size: [0, 0],
            windows: Vec::new(),
            selection: None,
            chat: None,
            reaction: None,
        })),
    )
    .spoken_for(run);
    let mut board = board();
    assert!(
        !board.hear(host, &face, 1.0),
        "no host is pinned here yet: nobody speaks for a run"
    );
    board.set_host(Some(host));
    board.certify(host, "aa".repeat(32), "home-server");
    assert!(
        !board.hear(member, &face, 2.0),
        "a member does not speak for a run"
    );
    assert!(board.hear(host, &face, 3.0));
    let shown = board
        .member(stand_in(host, run))
        .expect("the run is a member");
    assert!(shown.here && shown.agent());
    assert_eq!(shown.label(), "Visa backend");
    assert_eq!(shown.spoken_by, Some(host));
    assert_eq!(shown.device, "home-server", "said on the host's device");
    assert!(
        !board.member(host).is_some_and(|host| host.here),
        "the host itself said nothing"
    );
    let row = super::shown(shown, 3.0);
    assert_eq!(row.spoken_by, Some(hex::encode(host)));
    assert_eq!(row.activity.as_deref(), Some("New note"));

    // Another run is another member; both leave with the host.
    assert!(board.hear(host, &face.clone().spoken_for("agent-run:10"), 4.0));
    assert_eq!(board.here().count(), 2);
    board.depart(host);
    assert_eq!(board.here().count(), 0);

    // A host the governance no longer pins speaks for none of them.
    assert!(board.hear(host, &face, 5.0));
    assert!(board.set_host(Some(member)));
    assert!(board.member(stand_in(host, run)).is_none());
}

/// The line beside an agent's face names the step it is on, never the one
/// before: a view sent again, or one that arrives after a newer view, is the
/// one already taken and is not taken again.
#[test]
fn an_agents_line_never_rolls_back_to_the_step_before() {
    use collab_types::presence::{ShellPresence, stand_in};
    let (host, run) = ([3; 32], "agent-run:4");
    let saying = |seq: u32, line: &str| {
        Presence::desktop(
            WORKSPACE,
            Payload::Shell(Box::new(ShellPresence {
                seq,
                sent_at_ms: u64::from(seq),
                label: "Claude Code".into(),
                agent: true,
                colour: "#81a8ff".into(),
                activity: Some(line.into()),
                place: None,
                size: [0, 0],
                windows: Vec::new(),
                selection: None,
                chat: None,
                reaction: None,
            })),
        )
        .spoken_for(run)
    };
    let mut board = board();
    board.set_host(Some(host));
    let line = |board: &Board| {
        super::shown(
            board
                .member(stand_in(host, run))
                .expect("the run is a member"),
            1.0,
        )
        .activity
    };

    // Each step the run takes is said once, with a higher sequence.
    assert!(board.hear(host, &saying(1, "Looking at the apps"), 1.0));
    assert_eq!(line(&board).as_deref(), Some("Looking at the apps"));
    assert!(board.hear(host, &saying(2, "Editing Notes"), 2.0));
    assert_eq!(line(&board).as_deref(), Some("Editing Notes"));

    // The view of the step before, sent again or arrived late, changes
    // nothing: the line stays on the step the run is on.
    assert!(
        !board.hear(host, &saying(1, "Looking at the apps"), 3.0),
        "the step before is not news"
    );
    assert_eq!(line(&board).as_deref(), Some("Editing Notes"));

    // The next step, in turn, is.
    assert!(board.hear(host, &saying(3, "Reading Notes"), 4.0));
    assert_eq!(line(&board).as_deref(), Some("Reading Notes"));
}
