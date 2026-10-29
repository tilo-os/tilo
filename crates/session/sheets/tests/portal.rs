//! The questions the shell asks on an app's behalf, as a person, a screen
//! reader and an agent meet them: every kind drawn as one modal dialog,
//! the keys each kind takes, typed text kept out of every effect, and
//! answering left to the person.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_sheets::portal::{
    Body, Device, Dialog, DraftRow, DraftValue, Field, PortalApp, PortalEffect, PortalSeat, Prompt,
    PromptField, Remember, Shortcut,
};
use ui::framework::testing::HeadlessApp;

const VIEWPORT: (u32, u32) = (1280, 800);

fn seated(dialog: Dialog) -> (HeadlessApp<PortalApp>, PortalSeat) {
    use tilo_sheets::Seat as _;
    let seat = PortalSeat::default();
    seat.presentation().open(true);
    seat.show(Some(dialog));
    let mut app = HeadlessApp::<PortalApp>::for_person_with_lent("person", seat.clone()).unwrap();
    app.harness().resize(VIEWPORT).unwrap();
    app.settle();
    (app, seat)
}

fn key(app: &mut HeadlessApp<PortalApp>, spec: &str) {
    app.harness().key(spec);
    app.settle();
}

fn with_modifiers(app: &mut HeadlessApp<PortalApp>, held: &[&str], spec: &str) {
    let held: Vec<String> = held.iter().map(|name| (*name).to_owned()).collect();
    app.harness().set_modifiers(&held);
    app.harness().key(spec);
    app.harness().set_modifiers(&[]);
    app.settle();
}

fn dialog(body: Body) -> Dialog {
    Dialog {
        handle: "request-1".to_owned(),
        title: "Share your screen?".to_owned(),
        heading: "Meet".to_owned(),
        detail: "Meet asks to record your screen".to_owned(),
        body,
        confirm_label: "Allow".to_owned(),
        cancel_label: "Cancel".to_owned(),
        ..Dialog::default()
    }
}

fn choices(items: &[&str]) -> Body {
    Body::Choices(items.iter().map(|item| (*item).to_owned()).collect())
}

/// Whatever the sheet asked, written out: nothing typed may be in it.
fn asked_text(asked: &[PortalEffect]) -> String {
    format!("{asked:?}")
}

#[test]
fn every_scenario_is_one_modal_dialog_named_by_its_question() {
    use ui::framework::ResourceApp as _;
    for scenario in PortalApp::scenarios() {
        let mut app = HeadlessApp::<PortalApp>::capture_at(Some(scenario), VIEWPORT).unwrap();
        let nodes = app.harness().a11y_nodes();
        let dialog = nodes
            .iter()
            .map(|(_, node)| node)
            .find(|node| node.role() == ui::accesskit::Role::Dialog)
            .unwrap_or_else(|| panic!("{scenario}: the question is a dialog"));
        assert!(dialog.is_modal(), "{scenario}");
        assert!(
            dialog.label().is_some_and(|label| !label.is_empty()),
            "{scenario}: named by its title"
        );
        let pixels = app.harness().render_rgba_transparent();
        assert!(
            pixels.chunks(4).any(|pixel| pixel[3] > 0),
            "{scenario} draws"
        );
    }
}

#[test]
fn up_and_down_move_the_choice_enter_answers_and_escape_declines() {
    let (mut app, seat) = seated(dialog(choices(&["Writer", "Editor", "Browser"])));
    // The shell keeps the highlight: until it moves it, each key moves
    // from the first choice.
    key(&mut app, "ArrowDown");
    key(&mut app, "ArrowUp");
    key(&mut app, "Tab");
    key(&mut app, "Enter");
    key(&mut app, "Escape");
    assert_eq!(
        seat.take_asked(),
        [
            PortalEffect::Select { index: 1 },
            PortalEffect::Select { index: 2 },
            PortalEffect::Select { index: 1 },
            PortalEffect::Confirm,
            PortalEffect::Cancel,
        ]
    );
}

#[test]
fn the_shell_moves_the_highlight_and_a_reader_hears_every_choice() {
    let (mut app, seat) = seated(dialog(choices(&["Writer", "Editor"])));
    seat.show(Some(Dialog {
        selected: 1,
        ..dialog(choices(&["Writer", "Editor"]))
    }));
    app.settle();
    let nodes = app.harness().a11y_nodes();
    for name in ["Writer", "Editor", "Allow", "Cancel", "Meet"] {
        assert!(
            nodes
                .iter()
                .any(|(_, node)| node.label() == Some(name) || node.value() == Some(name)),
            "{name} is read"
        );
    }
    key(&mut app, "ArrowDown");
    assert_eq!(seat.take_asked(), [PortalEffect::Select { index: 0 }]);
}

#[test]
fn a_paged_review_turns_its_pages_before_it_can_be_allowed() {
    let paged = Dialog {
        page: Some((0, 2)),
        ..dialog(choices(&["Allow once"]))
    };
    let (mut app, seat) = seated(paged.clone());
    key(&mut app, "Enter");
    key(&mut app, "ArrowRight");
    assert_eq!(seat.take_asked(), [PortalEffect::Next, PortalEffect::Next]);
    assert!(
        app.harness().a11y_node_named("Continue").is_some(),
        "the primary button says the review goes on"
    );
    seat.show(Some(Dialog {
        page: Some((1, 2)),
        ..paged
    }));
    app.settle();
    key(&mut app, "ArrowLeft");
    key(&mut app, "Enter");
    assert_eq!(
        seat.take_asked(),
        [PortalEffect::Back, PortalEffect::Confirm]
    );
    assert!(app.harness().a11y_node_named("Back").is_some());
}

#[test]
fn space_ticks_only_the_device_in_focus() {
    let devices = Body::Devices(vec![
        Device {
            label: "YubiKey".to_owned(),
            writable: true,
            selected: true,
        },
        Device {
            label: "Card reader".to_owned(),
            writable: false,
            selected: true,
        },
    ]);
    let (mut app, seat) = seated(Dialog {
        selected: 1,
        ..dialog(devices)
    });
    key(&mut app, "Space");
    key(&mut app, "Enter");
    assert_eq!(
        seat.take_asked(),
        [PortalEffect::Toggle { index: 1 }, PortalEffect::Confirm]
    );
}

#[test]
fn backspace_clears_the_shortcut_in_focus() {
    let (mut app, seat) = seated(dialog(Body::Shortcuts(vec![Shortcut {
        name: "Push to talk".to_owned(),
        keys: None,
        status: Some("Needs a modifier".to_owned()),
    }])));
    assert!(app.harness().a11y_node_named("Push to talk").is_some());
    key(&mut app, "Backspace");
    assert_eq!(seat.take_asked(), [PortalEffect::Unbind]);
}

#[test]
fn a_shortcut_takes_the_keys_pressed_by_their_place_on_the_keyboard() {
    use ui::winit::event::ElementState;
    use ui::winit::keyboard::{Key, KeyCode, PhysicalKey};
    let (mut app, seat) = seated(dialog(Body::Shortcuts(vec![Shortcut {
        name: "Push to talk".to_owned(),
        keys: None,
        status: None,
    }])));
    app.harness()
        .set_modifiers(&["Control".to_owned(), "Shift".to_owned()]);
    let mut press = ui::synthetic_input::key_event(
        Key::Character("K".into()),
        Some("K".into()),
        ElementState::Pressed,
    );
    press.physical_key = PhysicalKey::Code(KeyCode::KeyK);
    app.harness()
        .send(&ui::synthetic_input::keyboard_input(press));
    app.settle();
    assert_eq!(
        seat.take_asked(),
        [PortalEffect::Bind {
            keys: "CTRL+SHIFT+KEYK".to_owned()
        }]
    );
}

#[test]
fn each_way_to_open_a_drive_is_its_own_answer() {
    let drive = Body::Answers {
        choices: vec!["Read only".to_owned(), "Allow changes".to_owned()],
        suggested: 0,
        remember: Some(Remember {
            label: "Remember this drive".to_owned(),
            on: false,
        }),
    };
    let (mut app, seat) = seated(dialog(drive));
    key(&mut app, "Space");
    key(&mut app, "ArrowDown");
    key(&mut app, "Enter");
    assert_eq!(
        seat.take_asked(),
        [
            PortalEffect::Remember,
            PortalEffect::Select { index: 1 },
            PortalEffect::Answer { index: 0 },
        ]
    );
    app.harness().click_named("portal-answer-1").unwrap();
    app.settle();
    assert_eq!(seat.take_asked(), [PortalEffect::Answer { index: 1 }]);
}

#[test]
fn a_typed_answer_reaches_the_shell_through_the_seat_alone() {
    let (mut app, seat) = seated(dialog(Body::Entry {
        masked: true,
        required: true,
        choices: Vec::new(),
    }));
    app.harness().type_text("4711");
    app.settle();
    assert_eq!(seat.text(Field::Answer).as_str(), "4711");
    key(&mut app, "Enter");
    let asked = seat.take_asked();
    assert_eq!(asked, [PortalEffect::Confirm]);
    assert!(!asked_text(&asked).contains("4711"));
    // A refusal is said in the sheet; what was typed stays.
    seat.show(Some(Dialog {
        error: Some("Those digits do not match".to_owned()),
        ..dialog(Body::Entry {
            masked: true,
            required: true,
            choices: Vec::new(),
        })
    }));
    app.settle();
    assert!(
        app.harness()
            .a11y_node_named("Those digits do not match")
            .is_some()
    );
    assert_eq!(seat.text(Field::Answer).as_str(), "4711");
    // Another request starts afresh, with nothing of the last one.
    seat.show(Some(Dialog {
        handle: "request-2".to_owned(),
        ..dialog(choices(&["Yes"]))
    }));
    assert!(seat.text(Field::Answer).is_empty());
}

fn draft() -> Body {
    Body::Draft {
        rows: vec![
            DraftRow {
                index: 0,
                label: "To".to_owned(),
                value: DraftValue::Text {
                    text: "ada@example.org".to_owned(),
                    multiline: false,
                    number: false,
                    withheld: false,
                },
                editable: true,
            },
            DraftRow {
                index: 1,
                label: "Message".to_owned(),
                value: DraftValue::Text {
                    text: "Hi".to_owned(),
                    multiline: true,
                    number: false,
                    withheld: false,
                },
                editable: true,
            },
            DraftRow {
                index: 2,
                label: "Urgent".to_owned(),
                value: DraftValue::Flag(false),
                editable: true,
            },
        ],
        choices: vec!["Allow once".to_owned()],
    }
}

#[test]
fn the_person_changes_the_draft_on_the_sheet() {
    let (mut app, seat) = seated(dialog(draft()));
    assert_eq!(seat.text(Field::Draft(0)).as_str(), "ada@example.org");
    app.harness().type_text("x");
    app.settle();
    assert_eq!(seat.text(Field::Draft(0)).as_str(), "ada@example.orgx");
    // Into the prose field: Enter is a new line there.
    key(&mut app, "Tab");
    key(&mut app, "Enter");
    assert!(seat.take_asked().is_empty(), "a new line, not an answer");
    assert!(seat.text(Field::Draft(1)).contains('\n'));
    // The check row: Space ticks it; then the answers.
    key(&mut app, "Tab");
    key(&mut app, "Space");
    assert_eq!(seat.take_asked(), [PortalEffect::Toggle { index: 2 }]);
    key(&mut app, "Tab");
    assert_eq!(seat.take_asked(), [PortalEffect::Select { index: 0 }]);
    // A press on the check row ticks it.
    app.harness()
        .perform_named("portal-draft-2", &ui::a11y::Action::Activate)
        .unwrap();
    app.settle();
    assert_eq!(seat.take_asked(), [PortalEffect::Toggle { index: 2 }]);
    with_modifiers(&mut app, &["Control"], "Enter");
    let asked = seat.take_asked();
    assert_eq!(asked, [PortalEffect::Confirm]);
    assert!(!asked_text(&asked).contains("example"));
}

fn network() -> Dialog {
    Dialog {
        title: "Wi-Fi password needed".to_owned(),
        body: Body::Secret(Prompt {
            fields: vec![
                PromptField {
                    label: "Identity".to_owned(),
                    masked: false,
                    required: true,
                    initial: "ada".to_owned(),
                },
                PromptField {
                    label: "Password".to_owned(),
                    masked: true,
                    required: true,
                    initial: String::new(),
                },
            ],
            primary_label: "Connect".to_owned(),
            ..Prompt::default()
        }),
        ..dialog(Body::Review)
    }
}

#[test]
fn a_secret_goes_to_the_shell_through_the_seat_and_never_into_an_effect() {
    // The keyboard starts in the first field still empty, the password;
    // Tab walks the prompt's own fields.
    let (mut app, seat) = seated(network());
    with_modifiers(&mut app, &["Shift"], "Tab");
    app.harness().type_text("x");
    key(&mut app, "Tab");
    app.harness().type_text("hunter2");
    app.settle();
    assert_eq!(
        seat.secrets()[1].lock().unwrap().as_str(),
        "hunter2",
        "the shell's buffer holds it"
    );
    // The shell refuses one field: the sheet is built again around what
    // was typed, and says why.
    let Body::Secret(prompt) = network().body else {
        panic!("the network fixture asks with a secret prompt")
    };
    seat.show(Some(Dialog {
        body: Body::Secret(Prompt {
            error: Some((1, "Too short".to_owned())),
            ..prompt
        }),
        ..network()
    }));
    app.settle();
    assert!(
        app.harness().a11y_node_named("Too short").is_some()
            || app.harness().a11y_nodes().iter().any(|(_, node)| {
                node.description()
                    .is_some_and(|text| text.contains("Too short"))
            })
    );
    key(&mut app, "Enter");
    let asked = seat.take_asked();
    assert_eq!(asked, [PortalEffect::Submit]);
    assert!(!asked_text(&asked).contains("hunter2"));
    let answer = seat.take_answer().expect("the answer waits in the seat");
    assert_eq!(answer.values[0].as_str(), "adax");
    assert_eq!(answer.values[1].as_str(), "hunter2");
    assert!(
        seat.secrets()
            .iter()
            .all(|buffer| buffer.lock().unwrap().is_empty()),
        "taking the answer wipes the buffers"
    );
}

#[test]
fn escape_backs_out_of_a_secret_prompt() {
    let (mut app, seat) = seated(network());
    app.harness().type_text("ada");
    key(&mut app, "Escape");
    assert_eq!(seat.take_asked(), [PortalEffect::Cancel]);
    assert!(seat.take_answer().is_none());
}

#[test]
fn only_the_person_answers_but_anyone_may_decline() {
    use ui::tilo_resource::Effect as _;
    for declaration in PortalEffect::declarations() {
        let answers = matches!(declaration.id, "confirm" | "answer" | "submit");
        assert_eq!(
            declaration.attended, answers,
            "only answering needs the person: {}",
            declaration.id
        );
    }
}
