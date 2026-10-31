//! The dictation sheet as a person, a screen reader and the session meet
//! it: the microphone allowed by a press on the card alone, what is heard
//! shown while it is heard, and every answer going back to the session's
//! seat.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_sheets::dictation::{
    Answer, Dictation, DictationApp, DictationSeat, Failure, Prompt, RecoveryReason, Stage,
};
use ui::a11y::Action as A11yAction;
use ui::framework::testing::HeadlessApp;
use ui::prelude::Resource as _;
use ui::prelude::tilo_actions::{OperationKind, ResourceSpec};
use ui::vello::kurbo::Rect;

const VIEWPORT: (u32, u32) = (1280, 800);

fn staged(scenario: &str) -> HeadlessApp<DictationApp> {
    HeadlessApp::<DictationApp>::capture_at(Some(scenario), VIEWPORT).unwrap()
}

/// Every button, by its label, where it stands.
fn buttons(app: &mut HeadlessApp<DictationApp>) -> Vec<(String, Rect)> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.role() == ui::accesskit::Role::Button)
        .filter_map(|(_, node)| {
            let bounds = node.bounds()?;
            Some((
                node.label()?.to_owned(),
                Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1),
            ))
        })
        .collect()
}

/// What a screen reader is told by the card, all of it.
fn told(app: &mut HeadlessApp<DictationApp>) -> String {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .map(|(_, node)| {
            format!(
                "{} {}",
                node.label().unwrap_or_default(),
                node.description().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

fn asking(id: u64) -> Prompt {
    Prompt {
        id,
        stage: Stage::Asking,
        draft: String::new(),
        failure: None,
        recovery: None,
        copied: false,
        copy_failed: false,
    }
}

fn recovery(id: u64) -> Prompt {
    Prompt {
        id,
        stage: Stage::Recovery,
        draft: String::new(),
        failure: None,
        recovery: Some(RecoveryReason::TooLong),
        copied: false,
        copy_failed: false,
    }
}

fn seated(prompt: Prompt) -> (HeadlessApp<DictationApp>, DictationSeat) {
    let seat = DictationSeat::default();
    seat.show(Some(prompt));
    let mut app =
        HeadlessApp::<DictationApp>::for_person_with_lent("person", seat.clone()).unwrap();
    app.harness().resize(VIEWPORT).unwrap();
    app.settle();
    (app, seat)
}

#[test]
fn every_stage_says_what_happens() {
    let mut app = staged("asking");
    let said = told(&mut app);
    assert!(said.contains("Dictate into this field?"), "{said}");
    assert!(
        said.contains("Nothing is recorded before you allow it"),
        "{said}"
    );
    assert!(said.contains("the field you were in"), "{said}");
    let labels: Vec<String> = buttons(&mut app)
        .into_iter()
        .map(|(label, _)| label)
        .collect();
    assert_eq!(labels, ["Allow", "Not now"]);

    let mut app = staged("listening");
    let said = told(&mut app);
    assert!(said.contains("Dictation"), "{said}");
    assert!(said.contains("café by the market"), "{said}");
    let labels: Vec<String> = buttons(&mut app)
        .into_iter()
        .map(|(label, _)| label)
        .collect();
    assert_eq!(labels, ["Insert", "Cancel"]);

    let mut app = staged("finishing");
    let said = told(&mut app);
    assert!(said.contains("Finishing your text"), "{said}");
    let labels: Vec<String> = buttons(&mut app)
        .into_iter()
        .map(|(label, _)| label)
        .collect();
    assert_eq!(labels, ["Cancel"]);

    let mut app = staged("recovery-too-long");
    let said = told(&mut app);
    assert!(said.contains("The field took no words"), "{said}");
    assert!(said.contains("too long to insert in one step"), "{said}");
    assert!(
        said.contains("Kept privately for 10 minutes"),
        "the card says where the words are: {said}"
    );
    assert!(!said.contains("Copied"), "nothing was copied yet: {said}");
    let labels: Vec<String> = buttons(&mut app)
        .into_iter()
        .map(|(label, _)| label)
        .collect();
    assert_eq!(labels, ["Copy", "Discard"]);

    let mut app = staged("recovery-copied");
    let said = told(&mut app);
    assert!(said.contains("Copied."), "{said}");

    let mut app = staged("model-required");
    let said = told(&mut app);
    assert!(
        said.contains("Install an approved local speech model"),
        "{said}"
    );
    let labels: Vec<String> = buttons(&mut app)
        .into_iter()
        .map(|(label, _)| label)
        .collect();
    assert_eq!(
        labels,
        ["Set up", "Close"],
        "a missing model offers the way to install one first"
    );

    let mut app = staged("unavailable");
    let said = told(&mut app);
    assert!(said.contains("Dictation isn't available"), "{said}");
    assert!(said.contains("Microphone access is unavailable"), "{said}");
    let labels: Vec<String> = buttons(&mut app)
        .into_iter()
        .map(|(label, _)| label)
        .collect();
    assert_eq!(labels, ["Close"]);
}

#[test]
fn enter_allows_the_microphone_once_for_the_prompt_shown() {
    let (mut app, seat) = seated(asking(7));
    app.harness().key("Enter");
    app.settle();
    assert_eq!(seat.take_answers(), [Answer::Allow(7)]);
    assert_eq!(
        seat.take_answers(),
        [],
        "an answer is taken once, not read twice"
    );
}

#[test]
fn a_press_on_not_now_refuses_the_microphone() {
    let (mut app, seat) = seated(asking(7));
    let deny = buttons(&mut app)
        .into_iter()
        .find(|(label, _)| label == "Not now")
        .unwrap()
        .1;
    app.harness().click_at(deny.center().x, deny.center().y);
    app.settle();
    assert_eq!(seat.take_answers(), [Answer::Deny(7)]);
}

#[test]
fn escape_says_no_and_keeps_nothing_of_a_transcript() {
    let (mut app, seat) = seated(asking(7));
    app.harness().key("Escape");
    app.settle();
    assert_eq!(seat.take_answers(), [Answer::Deny(7)]);

    seat.show(Some(recovery(8)));
    app.settle();
    app.harness().key("Escape");
    app.settle();
    assert_eq!(seat.take_answers(), [Answer::Discard(8)]);
}

#[test]
fn escape_cancels_while_the_dictation_runs_or_cannot_run() {
    let stages = [
        (20, Stage::Preparing),
        (21, Stage::Loading),
        (22, Stage::Listening),
        (23, Stage::StoppingMicrophone),
        (24, Stage::Finishing),
        (25, Stage::Stopping),
    ];
    for (id, stage) in stages {
        let (mut app, seat) = seated(Prompt {
            id,
            stage,
            draft: String::new(),
            failure: None,
            recovery: None,
            copied: false,
            copy_failed: false,
        });
        app.harness().key("Escape");
        app.settle();
        assert_eq!(seat.take_answers(), [Answer::Cancel(id)], "{stage:?}");
    }
    let (mut app, seat) = seated(Prompt {
        id: 30,
        stage: Stage::Unavailable,
        draft: String::new(),
        failure: Some(Failure::MicrophoneUnavailable),
        recovery: None,
        copied: false,
        copy_failed: false,
    });
    assert!(
        told(&mut app).contains("Microphone access is unavailable"),
        "the card says why the microphone is not heard"
    );
    app.harness().key("Enter");
    app.settle();
    assert_eq!(
        seat.take_answers(),
        [Answer::Cancel(30)],
        "Close is the primary"
    );
    app.harness().key("Escape");
    app.settle();
    assert_eq!(seat.take_answers(), [Answer::Cancel(30)]);
}

#[test]
fn a_missing_model_takes_the_person_to_its_setup() {
    let (mut app, seat) = seated(Prompt {
        id: 31,
        stage: Stage::Unavailable,
        draft: String::new(),
        failure: Some(Failure::ModelRequired),
        recovery: None,
        copied: false,
        copy_failed: false,
    });
    let setup = buttons(&mut app)
        .into_iter()
        .find(|(label, _)| label == "Set up")
        .expect("the card that needs a model says where to get one")
        .1;
    app.harness().click_at(setup.center().x, setup.center().y);
    app.settle();
    assert_eq!(seat.take_answers(), [Answer::SetUp(31)]);

    // Enter gives the primary answer, which is the way there while no model
    // is set up; Escape still gives the dictation up.
    app.harness().key("Enter");
    app.settle();
    assert_eq!(seat.take_answers(), [Answer::SetUp(31)]);
    app.harness().key("Escape");
    app.settle();
    assert_eq!(seat.take_answers(), [Answer::Cancel(31)]);
}

#[test]
fn while_listening_enter_inserts_and_copy_is_the_cards_own_word() {
    let (mut app, seat) = seated(Prompt {
        id: 4,
        stage: Stage::Listening,
        draft: "Hello".to_owned(),
        failure: None,
        recovery: None,
        copied: false,
        copy_failed: false,
    });
    app.harness().key("Enter");
    app.settle();
    assert_eq!(seat.take_answers(), [Answer::Finish(4)]);

    seat.show(Some(recovery(5)));
    app.settle();
    let copy = buttons(&mut app)
        .into_iter()
        .find(|(label, _)| label == "Copy")
        .unwrap()
        .1;
    app.harness().click_at(copy.center().x, copy.center().y);
    app.settle();
    assert_eq!(seat.take_answers(), [Answer::Copy(5)]);
    assert!(
        !told(&mut app).contains("Copied."),
        "the card does not claim a copy the session has not made"
    );
    seat.show(Some(Prompt {
        copied: true,
        ..recovery(5)
    }));
    app.settle();
    assert!(
        told(&mut app).contains("Copied."),
        "the card says the transcript is on the clipboard once it is"
    );
}

#[test]
fn the_draft_follows_the_session_without_reopening_the_sheet() {
    let (mut app, seat) = seated(Prompt {
        id: 3,
        stage: Stage::Listening,
        draft: String::new(),
        failure: None,
        recovery: None,
        copied: false,
        copy_failed: false,
    });
    assert!(
        told(&mut app).contains("Microphone on. Speak, then finish to insert."),
        "nothing heard yet"
    );
    seat.show(Some(Prompt {
        id: 3,
        stage: Stage::Listening,
        draft: "the first few words".to_owned(),
        failure: None,
        recovery: None,
        copied: false,
        copy_failed: false,
    }));
    app.settle();
    assert!(told(&mut app).contains("the first few words"));
}

#[test]
fn nothing_is_shown_once_the_dictation_is_gone() {
    let (mut app, seat) = seated(asking(1));
    assert!(!buttons(&mut app).is_empty());
    seat.show(None);
    app.settle();
    assert!(buttons(&mut app).is_empty(), "no answer is left to give");
    let dialogs = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.role() == ui::accesskit::Role::Dialog)
        .count();
    assert_eq!(dialogs, 0, "and nothing left to announce");
}

#[test]
fn a_screen_reader_meets_a_modal_dialog_naming_the_stage() {
    let mut app = staged("asking");
    let nodes = app.harness().a11y_nodes();
    let dialog = nodes
        .iter()
        .map(|(_, node)| node)
        .find(|node| node.role() == ui::accesskit::Role::Dialog)
        .expect("the card is a dialog");
    assert_eq!(dialog.label(), Some("Dictate into this field?"));
    assert!(dialog.is_modal());
    assert!(
        dialog
            .description()
            .is_some_and(|text| text.contains("Tilo will listen through the microphone once")),
        "{:?}",
        dialog.description()
    );
}

#[test]
fn the_card_paints_only_itself_over_the_field() {
    let mut app = staged("asking");
    let pixels = app.harness().render_rgba_transparent();
    let alpha = |x: u32, y: u32| pixels[usize::try_from((y * VIEWPORT.0 + x) * 4 + 3).unwrap()];
    assert_eq!(alpha(10, 10), 0, "the field shows around the card");
    assert!(alpha(640, 400) > 0, "the card is painted in the middle");
}

#[test]
fn an_assistive_press_is_the_persons_own_answer() {
    let (mut app, seat) = seated(asking(7));
    app.harness()
        .perform_named("dictation-allow", &A11yAction::Activate)
        .ok();
    app.settle();
    assert_eq!(seat.take_answers(), [Answer::Allow(7)]);
}

#[test]
fn tab_walks_the_answers_and_enter_gives_the_one_reached() {
    let (mut app, seat) = seated(asking(7));
    app.harness().key("Tab");
    app.settle();
    app.harness().key("Enter");
    app.settle();
    assert_eq!(seat.take_answers(), [Answer::Deny(7)]);
}

/// The resource the sheet shows, wherever it stands in the published tree.
fn dictation_resource(specs: &[ResourceSpec]) -> Option<ResourceSpec> {
    specs.iter().find_map(|spec| {
        if spec.kind == Dictation::KIND {
            Some(spec.clone())
        } else {
            dictation_resource(&spec.children)
        }
    })
}

#[test]
fn the_sheet_publishes_no_action_that_could_answer_it() {
    let (mut app, _seat) = seated(asking(7));
    let published = dictation_resource(&app.harness().resources()).expect("the sheet is published");
    // The framework's own verbs stand beside every resource; the sheet adds
    // none. An effect is what the session's action door invokes, and a remote
    // action is not consent, so the vocabulary is empty.
    let effects: Vec<&str> = published
        .actions
        .iter()
        .filter(|action| action.kind == OperationKind::Effect)
        .map(|action| action.id.as_str())
        .collect();
    assert!(
        effects.is_empty(),
        "an effect would let anything that can reach the door answer the microphone: {effects:?}"
    );
    let answers = ["allow", "deny", "finish", "cancel", "copy", "discard"];
    let named: Vec<&str> = published
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .filter(|id| answers.iter().any(|verb| id.contains(verb)))
        .collect();
    assert!(
        named.is_empty(),
        "no verb is one of the card's answers: {named:?}"
    );
}
