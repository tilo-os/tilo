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

/// The request the questions here ask about, which every answer names.
fn request_1() -> String {
    "request-1".to_owned()
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
            PortalEffect::Select {
                handle: request_1(),
                index: 1,
            },
            PortalEffect::Select {
                handle: request_1(),
                index: 2,
            },
            PortalEffect::Select {
                handle: request_1(),
                index: 1,
            },
            PortalEffect::Confirm {
                handle: request_1()
            },
            PortalEffect::Cancel {
                handle: request_1()
            },
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
    assert_eq!(
        seat.take_asked(),
        [PortalEffect::Select {
            handle: request_1(),
            index: 0,
        }]
    );
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
    assert_eq!(
        seat.take_asked(),
        [
            PortalEffect::Next {
                handle: request_1()
            },
            PortalEffect::Next {
                handle: request_1()
            }
        ]
    );
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
        [
            PortalEffect::Back {
                handle: request_1()
            },
            PortalEffect::Confirm {
                handle: request_1()
            }
        ]
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
        [
            PortalEffect::Toggle {
                handle: request_1(),
                index: 1,
            },
            PortalEffect::Confirm {
                handle: request_1()
            }
        ]
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
    assert_eq!(
        seat.take_asked(),
        [PortalEffect::Unbind {
            handle: request_1()
        }]
    );
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
            handle: request_1(),
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
            PortalEffect::Remember {
                handle: request_1()
            },
            PortalEffect::Select {
                handle: request_1(),
                index: 1,
            },
            PortalEffect::Answer {
                handle: request_1(),
                index: 0,
            },
        ]
    );
    app.harness().click_named("portal-answer-1").unwrap();
    app.settle();
    assert_eq!(
        seat.take_asked(),
        [PortalEffect::Answer {
            handle: request_1(),
            index: 1,
        }]
    );
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
    assert_eq!(
        asked,
        [PortalEffect::Confirm {
            handle: request_1()
        }]
    );
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
    assert_eq!(
        seat.take_asked(),
        [PortalEffect::Toggle {
            handle: request_1(),
            index: 2,
        }]
    );
    key(&mut app, "Tab");
    assert_eq!(
        seat.take_asked(),
        [PortalEffect::Select {
            handle: request_1(),
            index: 0,
        }]
    );
    // A press on the check row ticks it.
    app.harness()
        .perform_named("portal-draft-2", &ui::a11y::Action::Activate)
        .unwrap();
    app.settle();
    assert_eq!(
        seat.take_asked(),
        [PortalEffect::Toggle {
            handle: request_1(),
            index: 2,
        }]
    );
    with_modifiers(&mut app, &["Control"], "Enter");
    let asked = seat.take_asked();
    assert_eq!(
        asked,
        [PortalEffect::Confirm {
            handle: request_1()
        }]
    );
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
    assert_eq!(
        asked,
        [PortalEffect::Submit {
            handle: request_1()
        }]
    );
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
    assert_eq!(
        seat.take_asked(),
        [PortalEffect::Cancel {
            handle: request_1()
        }]
    );
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

#[test]
fn typing_is_a_published_step_whose_text_stays_in_the_seat() {
    let (mut app, seat) = seated(dialog(draft()));
    app.effect(&PortalEffect::Type {
        handle: request_1(),
        field: "draft-0".to_owned(),
        text: "grace@example.org".to_owned(),
    })
    .unwrap();
    app.settle();
    assert_eq!(seat.text(Field::Draft(0)).as_str(), "grace@example.org");
    assert!(
        app.harness()
            .a11y_nodes()
            .iter()
            .any(|(_, node)| node.value() == Some("grace@example.org")),
        "the field shows what was typed"
    );
    assert!(
        seat.take_asked().is_empty(),
        "typing asks the shell nothing"
    );
    // Only the fields the question has, and the person may change.
    for field in ["draft-2", "draft-7", "answer", "name"] {
        assert!(
            app.effect(&PortalEffect::Type {
                handle: request_1(),
                field: field.to_owned(),
                text: "x".to_owned(),
            })
            .is_err(),
            "{field}"
        );
    }
    let (mut app, seat) = seated(dialog(Body::Entry {
        masked: true,
        required: true,
        choices: Vec::new(),
    }));
    app.effect(&PortalEffect::Type {
        handle: request_1(),
        field: "answer".to_owned(),
        text: "4711".to_owned(),
    })
    .unwrap();
    app.effect(&PortalEffect::Confirm {
        handle: request_1(),
    })
    .unwrap();
    let asked = seat.take_asked();
    assert_eq!(
        asked,
        [PortalEffect::Confirm {
            handle: request_1()
        }]
    );
    assert!(!asked_text(&asked).contains("4711"));
    assert_eq!(seat.text(Field::Answer).as_str(), "4711");
}

#[test]
fn a_secret_prompt_is_answered_through_the_door_as_the_prompt_checks_it() {
    let (mut app, seat) = seated(network());
    // A required field still empty: the prompt refuses, and nothing is
    // asked.
    assert!(
        app.effect(&PortalEffect::Submit {
            handle: request_1()
        })
        .is_err()
    );
    assert!(seat.take_asked().is_empty());
    assert!(seat.take_answer().is_none());
    app.effect(&PortalEffect::TypeSecret {
        handle: request_1(),
        field: 1,
        value: "hunter2".to_owned(),
    })
    .unwrap();
    assert!(
        app.effect(&PortalEffect::TypeSecret {
            handle: request_1(),
            field: 5,
            value: "x".to_owned(),
        })
        .is_err()
    );
    app.settle();
    assert_eq!(seat.secrets()[1].lock().unwrap().as_str(), "hunter2");
    app.effect(&PortalEffect::Submit {
        handle: request_1(),
    })
    .unwrap();
    let asked = seat.take_asked();
    assert_eq!(
        asked,
        [PortalEffect::Submit {
            handle: request_1()
        }]
    );
    assert!(!asked_text(&asked).contains("hunter2"));
    let answer = seat.take_answer().expect("the answer waits in the seat");
    assert_eq!(answer.values[0].as_str(), "ada");
    assert_eq!(answer.values[1].as_str(), "hunter2");
}

#[test]
fn remember_ticks_a_secret_prompts_check_row() {
    let Body::Secret(prompt) = network().body else {
        panic!("the network fixture asks with a secret prompt")
    };
    let (mut app, seat) = seated(Dialog {
        body: Body::Secret(Prompt {
            option: Some(("Remember the password".to_owned(), true)),
            ..prompt
        }),
        ..network()
    });
    app.effect(&PortalEffect::Remember {
        handle: request_1(),
    })
    .unwrap();
    app.settle();
    assert_eq!(seat.option(), Some(false));
    app.effect(&PortalEffect::TypeSecret {
        handle: request_1(),
        field: 1,
        value: "hunter2".to_owned(),
    })
    .unwrap();
    app.effect(&PortalEffect::Submit {
        handle: request_1(),
    })
    .unwrap();
    assert_eq!(seat.take_answer().unwrap().option, Some(false));
}

#[test]
fn an_answer_for_a_question_that_is_no_longer_showing_never_answers_the_next() {
    let (mut app, seat) = seated(dialog(choices(&["Writer", "Editor"])));
    // The person answers the first question.
    key(&mut app, "Enter");
    let mut asked = seat.take_asked();
    assert_eq!(
        asked,
        [PortalEffect::Confirm {
            handle: request_1()
        }]
    );
    let for_the_first = asked.remove(0);
    // The next question opens before that answer reaches the shell.
    seat.show(Some(Dialog {
        handle: "request-2".to_owned(),
        title: "Record your screen?".to_owned(),
        ..dialog(choices(&["Whole screen", "One window"]))
    }));
    app.settle();
    let refused = app
        .effect(&for_the_first)
        .expect_err("an answer for the question that is gone is refused");
    assert!(
        format!("{refused:?}").contains("no longer showing"),
        "the refusal says the question is gone: {refused:?}"
    );
    assert!(
        seat.take_asked().is_empty(),
        "the question that is up was not answered"
    );
    assert!(
        app.harness().a11y_node_named("Whole screen").is_some(),
        "the question that is up is still up"
    );
    // The question that is up answers itself, naming itself.
    key(&mut app, "Enter");
    assert_eq!(
        seat.take_asked(),
        [PortalEffect::Confirm {
            handle: "request-2".to_owned()
        }]
    );
}

/// An agent's question as a reviewer reads it: who wants to do what as the
/// title, the run it asks in, the record and the workspace, each argument a
/// row of its own with its secret withheld, and what allowing it risks as
/// the last line before the buttons. Deny stands focused, and the card hugs
/// what it says: the buttons sit one gutter under the last line, as the
/// title sits one gutter under the top.
#[test]
fn an_agents_question_says_who_wants_what_and_hugs_its_words() {
    use tilo_sheets::portal::AgentQuestion;
    let asked = AgentQuestion {
        agent: "Jane Doe’s agent".into(),
        run: "Fill Jane Doe’s visa application".into(),
        action: "Submit the application".into(),
        resource: "Schengen visa: Jane Doe".into(),
        arguments: vec![
            ("Name".into(), "Jane Doe".into()),
            ("Passport".into(), "X1234567".into()),
            ("Consulate login".into(), "•••".into()),
        ],
        risk: "destructive".into(),
        kind: "outside".into(),
        undoable: false,
        workspace: "Applicant: Jane Doe".into(),
    };
    let dialog = asked.dialog("request-1");
    assert_eq!(
        dialog.title,
        "Jane Doe’s agent wants to submit the application"
    );
    assert_eq!(dialog.heading, "Fill Jane Doe’s visa application");
    assert_eq!(
        dialog.detail,
        "On “Schengen visa: Jane Doe” in the workspace “Applicant: Jane Doe”."
    );
    assert_eq!(
        dialog.facts,
        [
            ("Name".to_owned(), "Jane Doe".to_owned()),
            ("Passport".to_owned(), "X1234567".to_owned()),
            ("Consulate login".to_owned(), "•••".to_owned()),
        ],
        "every argument is a row of its own, the secret withheld"
    );
    assert_eq!(
        dialog.caution.as_deref(),
        Some("It acts outside Tilo and can’t be undone.")
    );
    // A read of private data says so, and an undoable change risks nothing
    // worth a line.
    let reads = AgentQuestion {
        risk: "sensitive".into(),
        kind: "read".into(),
        undoable: false,
        ..asked.clone()
    };
    assert_eq!(
        reads.dialog("r").caution.as_deref(),
        Some("It can reveal private data.")
    );
    let edits = AgentQuestion {
        risk: "low".into(),
        kind: "undoable".into(),
        undoable: true,
        ..asked
    };
    assert_eq!(edits.dialog("r").caution, None);

    let (mut app, _seat) = seated(dialog.clone());
    let focused = app.harness().a11y_focus().map(|(_, node)| node);
    assert_eq!(
        focused.as_ref().and_then(|node| node.label()),
        Some("Deny"),
        "Deny stands focused"
    );
    let bounds = |role: ui::accesskit::Role, app: &mut HeadlessApp<PortalApp>| {
        app.harness()
            .a11y_nodes()
            .into_iter()
            .filter(|(_, node)| node.role() == role)
            .filter_map(|(_, node)| node.bounds())
            .collect::<Vec<_>>()
    };
    let card = bounds(ui::accesskit::Role::Dialog, &mut app)[0];
    // Each argument is a row of its own, in the order the question carries
    // them.
    let listed = listed_facts(&mut app);
    assert_eq!(
        listed, dialog.facts,
        "the arguments are rows in their own order"
    );
    let buttons = bounds(ui::accesskit::Role::Button, &mut app);
    let buttons_top = buttons
        .iter()
        .map(|button| button.y0)
        .fold(f64::MAX, f64::min);
    let buttons_end = buttons
        .iter()
        .map(|button| button.y1)
        .fold(f64::MIN, f64::max);
    // The risk follows every argument and stands last before the buttons.
    let after = last_row_end(&mut app).unwrap_or(card.y0);
    risk_line_is_last(&mut app, dialog.caution.as_deref(), after);
    let words_end = bounds(ui::accesskit::Role::Label, &mut app)
        .into_iter()
        .filter(|text| text.y0 > card.y0 && text.y1 < card.y1)
        .map(|text| text.y1)
        .fold(f64::MIN, f64::max);
    let under_words = buttons_top - words_end;
    let under_buttons = card.y1 - buttons_end;
    assert!(
        under_words > 0.0 && under_words <= under_buttons + 1.0,
        "the buttons sit one gutter under the words: {under_words} px there, {under_buttons} px \
         under the buttons"
    );
}

/// The rows of the sheet's property list, top to bottom: each label, its
/// value and the row's bounds.
fn listed_rows(app: &mut HeadlessApp<PortalApp>) -> Vec<(f64, f64, String, String)> {
    let nodes = app.harness().a11y_nodes();
    let list = nodes
        .iter()
        .find(|(_, node)| node.role() == ui::accesskit::Role::List)
        .and_then(|(_, node)| node.bounds())
        .expect("the sheet lists its rows");
    let mut rows = nodes
        .iter()
        .filter(|(_, node)| node.role() == ui::accesskit::Role::Label)
        .filter_map(|(_, node)| {
            let bounds = node.bounds()?;
            let inside = bounds.y0 >= list.y0 - 1.0 && bounds.y1 <= list.y1 + 1.0;
            inside.then(|| {
                (
                    bounds.y0,
                    bounds.y1,
                    node.label().unwrap_or_default().to_owned(),
                    node.value().unwrap_or_default().to_owned(),
                )
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| left.0.total_cmp(&right.0));
    rows
}

/// What the list says, label and value, in its own order.
fn listed_facts(app: &mut HeadlessApp<PortalApp>) -> Vec<(String, String)> {
    listed_rows(app)
        .into_iter()
        .map(|(_, _, label, value)| (label, value))
        .collect()
}

/// An action with more arguments than a review lists: the first six rows,
/// then how many more there are, the risk line still last.
#[test]
fn a_review_lists_six_arguments_and_counts_the_rest() {
    use tilo_sheets::portal::AgentQuestion;
    let asked = AgentQuestion {
        agent: "Jane Doe’s agent".into(),
        run: "Fill Jane Doe’s visa application".into(),
        action: "Submit the application".into(),
        resource: "Schengen visa: Jane Doe".into(),
        arguments: (0..8)
            .map(|at| (format!("Field {at}"), format!("Value {at}")))
            .collect(),
        risk: "destructive".into(),
        kind: "outside".into(),
        undoable: false,
        workspace: "Applicant: Jane Doe".into(),
    };
    let dialog = asked.dialog("request-1");
    assert_eq!(dialog.facts.len(), 8, "every argument is on the question");
    let (mut app, _seat) = seated(dialog.clone());
    assert_eq!(
        listed_facts(&mut app),
        dialog.facts[..6],
        "the first six arguments are rows"
    );
    let nodes = app.harness().a11y_nodes();
    let more = nodes
        .iter()
        .find(|(_, node)| node.label() == Some("and 2 more"))
        .and_then(|(_, node)| node.bounds())
        .expect("the rows left out are counted");
    let last_row = last_row_end(&mut app).unwrap_or(f64::MIN);
    assert!(more.y0 >= last_row, "the count follows the rows it counts");
    risk_line_is_last(&mut app, dialog.caution.as_deref(), more.y1);
}

/// Where the sheet's last listed row ends, when it lists any.
fn last_row_end(app: &mut HeadlessApp<PortalApp>) -> Option<f64> {
    listed_rows(app).last().map(|(_, end, _, _)| *end)
}

/// The risk line `caution` sits under `after` and is the last line before
/// the buttons.
fn risk_line_is_last(app: &mut HeadlessApp<PortalApp>, caution: Option<&str>, after: f64) {
    let nodes = app.harness().a11y_nodes();
    let buttons_top = nodes
        .iter()
        .filter(|(_, node)| node.role() == ui::accesskit::Role::Button)
        .filter_map(|(_, node)| node.bounds())
        .map(|bounds| bounds.y0)
        .fold(f64::MAX, f64::min);
    let line = nodes
        .iter()
        .find(|(_, node)| caution.is_some() && node.label() == caution)
        .and_then(|(_, node)| node.bounds())
        .expect("the risk line is there");
    assert!(
        line.y0 >= after,
        "the risk line follows what it is about: {} px below it",
        line.y0 - after
    );
    assert!(
        line.y1 <= buttons_top + 1.0,
        "the risk line stands last before the buttons: {} px above them",
        buttons_top - line.y1
    );
}
