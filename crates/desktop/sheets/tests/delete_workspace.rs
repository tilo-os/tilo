//! "Delete \<name\>?" as a person, a screen reader and an agent meet it:
//! what goes with the workspace, and the answer being the person's own.

use tilo_sheets::SheetState as _;
use tilo_sheets::delete_workspace::{
    Asked, DeleteWorkspaceApp, DeleteWorkspaceEffect, DeleteWorkspaceState,
};
use ui::a11y::Action as A11yAction;
use ui::framework::testing::HeadlessApp;

const VIEWPORT: (u32, u32) = (1280, 800);

/// The sheet as the shell lends it, asking about the workspace called
/// `name`.
fn mounted(name: &str) -> (HeadlessApp<DeleteWorkspaceApp>, DeleteWorkspaceState) {
    let state = DeleteWorkspaceState::default();
    state.show(name.to_owned());
    state.presentation().open(true);
    let mut app =
        HeadlessApp::<DeleteWorkspaceApp>::for_person_with_lent("person", state.clone()).unwrap();
    app.harness().resize(VIEWPORT).unwrap();
    app.settle();
    (app, state)
}

fn says(app: &mut HeadlessApp<DeleteWorkspaceApp>, text: &str) -> bool {
    app.harness().a11y_nodes().iter().any(|(_, node)| {
        node.label().is_some_and(|label| label.contains(text))
            || node.description().is_some_and(|said| said.contains(text))
            || node.value().is_some_and(|value| value.contains(text))
    })
}

fn press(app: &mut HeadlessApp<DeleteWorkspaceApp>, id: &str) {
    app.harness()
        .perform_named(id, &A11yAction::Activate)
        .unwrap();
    app.settle();
}

tilo_test_env::scenario! {
    /// "Delete workspace?": what the question names, and each way of answering it.
    fn the_delete_question_names_its_workspace_and_answers_the_shell;

    phase the_question_names_the_workspace_and_what_goes_with_it;
    {
        let (mut app, _state) = mounted("Design studio");
        for text in [
            "Delete Design studio?",
            "Everything in this workspace goes with it",
            "Delete",
            "Keep",
        ] {
            assert!(says(&mut app, text), "{text:?} is shown");
        }
    }

    phase the_delete_button_is_the_only_way_to_delete_and_every_other_keeps_the_workspace;
    // Each way of answering, on a question of its own: the keyboard starts on
    // the safe answer of a deletion, every way out keeps the workspace, and the
    // delete button is the only one that deletes.
    {
        let ways = [
            (Way::Key("Enter"), DeleteWorkspaceEffect::Keep),
            (Way::Key("Escape"), DeleteWorkspaceEffect::Keep),
            (
                Way::Press("workspace-delete.secondary"),
                DeleteWorkspaceEffect::Keep,
            ),
            (Way::PressBeside, DeleteWorkspaceEffect::Keep),
            (
                Way::Press("workspace-delete.primary"),
                DeleteWorkspaceEffect::Delete,
            ),
        ];
        for (way, answer) in ways {
            let (mut app, state) = mounted("Design studio");
            match way {
                Way::Key(key) => {
                    app.harness().key(key);
                    app.settle();
                }
                Way::Press(id) => press(&mut app, id),
                Way::PressBeside => {
                    app.harness().click_at(10.0, 10.0);
                    app.settle();
                }
            }
            assert_eq!(state.take_answers(), [answer], "{way:?}");
        }
    }

    phase a_new_question_replaces_the_one_before_it;
    {
        let (mut app, state) = mounted("Design studio");
        state.show("Notes".to_owned());
        app.settle();
        assert_eq!(
            state.asked(),
            Asked {
                name: "Notes".to_owned()
            }
        );
        assert!(says(&mut app, "Delete Notes?"));
        assert!(!says(&mut app, "Delete Design studio?"));
    }
}

/// One way of answering the question.
#[derive(Clone, Copy, Debug)]
enum Way {
    /// A key, pressed as the sheet comes up.
    Key(&'static str),
    /// A press on the button with this id.
    Press(&'static str),
    /// A press beside the dialog, on the dimmed desktop.
    PressBeside,
}
