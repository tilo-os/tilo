#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! The framework on the real component runtime: controls invoke actions,
//! undo reverts them, viewers are refused, state survives a relaunch, and
//! the history of every action is attributed.

use tilo_app::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Checklist {
    pub title: String,
    pub items: Collection<Item>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Item {
    pub text: String,
    pub done: bool,
}

#[resource(kind = "test.checklist", label = "Checklist")]
impl Checklist {
    /// Rename the checklist.
    #[action]
    pub fn rename(&mut self, title: String) {
        self.title = title;
    }

    /// Add an item.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx, text: String) {
        self.items.push(cx.id(), Item { text, done: false });
    }

    /// Check or uncheck an item.
    #[action]
    pub fn set_done(&mut self, item: &Id, done: bool) -> Result<(), &'static str> {
        self.items.get_mut(item).ok_or("no such item")?.done = done;
        Ok(())
    }

    /// Remove every checked item.
    #[action(destructive)]
    pub fn clear_done(&mut self) {
        self.items.retain(|_, item| !item.done);
    }
}

struct ChecklistApp;

impl ResourceApp for ChecklistApp {
    type Resource = Checklist;
    const ID: &'static str = "so.tilo.test-checklist";
    const TITLE: &'static str = "Checklist";

    fn view(cx: &mut ViewCx<Checklist>) -> Element {
        Box::new(
            Column::new()
                .push(Box::new(
                    cx.input(|list| list.title.clone(), Checklist::RENAME)
                        .with_id("title"),
                ))
                .push(Box::new(cx.text(|list| {
                    let open = list.items.values().filter(|item| !item.done).count();
                    format!("{open} open")
                })))
                .push(Box::new(cx.each(
                    |list| &list.items,
                    |row| {
                        let id = row.item().cloned().unwrap_or_else(|| Id::from_raw(""));
                        Box::new(row.checkbox(
                            row.read(|item| item.map(|item| item.text.clone()).unwrap_or_default()),
                            |item| item.done,
                            Checklist::SET_DONE.map(move |done| (id.clone(), done)),
                        ))
                    },
                )))
                .push(Box::new(
                    cx.button("Clear done", Checklist::CLEAR_DONE.invoke()),
                )),
        )
    }
}

/// The same checklist, declared as showing secrets.
struct SecretChecklistApp;

impl ResourceApp for SecretChecklistApp {
    type Resource = Checklist;
    const ID: &'static str = "so.tilo.test-secret-checklist";
    const TITLE: &'static str = "Secret checklist";
    const BEHAVIOUR: tilo_app::application::ComponentBehaviour =
        tilo_app::application::ComponentBehaviour {
            capture_protected: true,
            ..tilo_app::application::ComponentBehaviour::DEFAULT
        };

    fn view(cx: &mut ViewCx<Checklist>) -> Element {
        ChecklistApp::view(cx)
    }
}

#[test]
fn the_live_window_withholds_itself_from_captures_only_when_the_app_declares_it() {
    use tilo_app::framework::testing::inspect_live_window;
    assert!(
        inspect_live_window::<SecretChecklistApp, _>(tilo_app::App::capture_protected).unwrap()
    );
    assert!(!inspect_live_window::<ChecklistApp, _>(tilo_app::App::capture_protected).unwrap());
}

fn texts(app: &TestApp<ChecklistApp>) -> Vec<(String, bool)> {
    app.state()
        .items
        .values()
        .map(|item| (item.text.clone(), item.done))
        .collect()
}

#[test]
fn actions_update_the_record_and_view_as_undoable_attributed_versions() {
    // phase: actions_through_the_door_update_the_record_and_the_view
    {
        let mut app = TestApp::<ChecklistApp>::new().unwrap();
        app.invoke(Checklist::ADD.with("milk".into())).unwrap();
        app.invoke(Checklist::ADD.with("eggs".into())).unwrap();
        assert_eq!(
            texts(&app),
            [("milk".into(), false), ("eggs".into(), false)]
        );
        let inspection = app.harness().inspection_json().unwrap();
        assert!(inspection.contains("2 open"), "{inspection}");

        // The row's checkbox is bound to set-done for its own item.
        let checkbox = app
            .harness()
            .targets()
            .keys()
            .find(|name| name.starts_with("set-done-"))
            .cloned()
            .unwrap();
        app.harness().click_named(&checkbox).unwrap();
        assert_eq!(
            app.state().items.values().filter(|item| item.done).count(),
            1
        );

        // A destructive action asks for confirmation: the unconfirmed click is
        // refused and changes nothing; the confirmed invocation applies.
        app.harness().click_named("clear-done").unwrap();
        assert_eq!(app.state().items.len(), 2);
        assert!(app.invoke(Checklist::CLEAR_DONE.invoke()).is_err());
        app.invoke_confirmed(Checklist::CLEAR_DONE.invoke())
            .unwrap();
        assert_eq!(app.state().items.len(), 1);
    }
    // phase: typing_in_a_bound_field_is_an_action
    {
        let mut app = TestApp::<ChecklistApp>::new().unwrap();
        app.harness().click_named("title").unwrap();
        app.harness().type_text("Groceries");
        assert_eq!(app.state().title, "Groceries");
    }
    // phase: undo_and_redo_revert_actions
    {
        let mut app = TestApp::<ChecklistApp>::new().unwrap();
        app.invoke(Checklist::RENAME.with("First".into())).unwrap();
        app.invoke(Checklist::RENAME.with("Second".into())).unwrap();
        assert!(app.undo());
        assert_eq!(app.state().title, "First");
        assert!(app.redo());
        assert_eq!(app.state().title, "Second");
    }
    // phase: undo_reverts_a_text_splice_exactly
    {
        let mut app = TestApp::<ChecklistApp>::new().unwrap();
        app.invoke(Checklist::RENAME.with("first thought".into()))
            .unwrap();
        app.invoke(Checklist::RENAME.with("second thought".into()))
            .unwrap();
        assert!(app.undo());
        assert_eq!(app.state().title, "first thought");
        assert!(app.undo());
        assert_eq!(app.state().title, "");
        assert!(app.redo());
        assert!(app.redo());
        assert_eq!(app.state().title, "second thought");
    }
    // phase: state_survives_a_relaunch
    {
        let mut app = TestApp::<ChecklistApp>::new().unwrap();
        app.invoke(Checklist::ADD.with("persisted".into())).unwrap();
        let app = app.reopen().unwrap();
        assert_eq!(texts(&app), [("persisted".into(), false)]);
    }
    // phase: every_action_is_an_attributed_version
    {
        let mut app = TestApp::<ChecklistApp>::new().unwrap();
        app.invoke(Checklist::RENAME.with("Weekly".into())).unwrap();
        let last = app.versions().pop().unwrap();
        assert_eq!(
            last.summary().as_deref(),
            Some("Rename the checklist · Checklist")
        );
    }
}

#[test]
fn actions_are_gated_by_role_and_confirmed_for_every_surface() {
    // phase: a_viewer_cannot_edit
    {
        let mut app = TestApp::<ChecklistApp>::with_role(tilo_app::Role::Viewer).unwrap();
        assert!(app.invoke(Checklist::ADD.with("sneaky".into())).is_err());
        assert!(app.state().items.is_empty());
    }
    // phase: the_palette_and_agents_see_every_action
    {
        let mut app = TestApp::<ChecklistApp>::new().unwrap();
        let resources = app.harness().resources();
        let ids: Vec<_> = resources
            .iter()
            .flat_map(|resource| resource.actions.iter().map(|spec| spec.id.clone()))
            .collect();
        for id in ["rename", "add", "set-done", "clear-done", "apply"] {
            assert!(
                ids.iter().any(|candidate| candidate == id),
                "{id} in {ids:?}"
            );
        }
    }
    // phase: the_generic_apply_action_is_for_agents_and_scripts_only
    {
        let mut app = TestApp::<ChecklistApp>::new().unwrap();
        let resources = app.harness().resources();
        let apply = resources
            .iter()
            .flat_map(|resource| &resource.actions)
            .find(|spec| spec.id == tilo_app::tilo_resource::APPLY)
            .expect("apply is published for agents and scripts");
        assert!(apply.is_programmatic(), "never a palette row");
        assert!(apply.contract.confirmation_depends_on_arguments());
        let named = resources
            .iter()
            .flat_map(|resource| &resource.actions)
            .find(|spec| spec.id == "rename")
            .unwrap();
        assert!(
            !named.is_programmatic(),
            "named actions stay in the palette"
        );
    }
    // phase: a_destructive_typed_value_is_confirmed_like_its_named_action
    {
        let mut app = TestApp::<ChecklistApp>::new().unwrap();
        // An ordinary typed edit applies directly.
        app.invoke(typed(ChecklistAction::Rename {
            title: "Weekly".into(),
        }))
        .unwrap();
        assert_eq!(app.state().title, "Weekly");

        app.invoke(Checklist::ADD.with("milk".into())).unwrap();
        let id = app.state().items.ids().next().cloned().unwrap();
        app.invoke(typed(ChecklistAction::SetDone {
            item: id,
            done: true,
        }))
        .unwrap();

        // `clear_done` is `#[action(destructive)]`: reached through `apply` it
        // needs the same confirmation its named declaration does.
        assert!(matches!(
            app.invoke(typed(ChecklistAction::ClearDone {})),
            Err(tilo_actions::DoorError::ConfirmationRequired)
        ));
        assert_eq!(
            app.state().items.len(),
            1,
            "the refused edit changed nothing"
        );
        app.invoke_confirmed(typed(ChecklistAction::ClearDone {}))
            .unwrap();
        assert!(app.state().items.is_empty());
    }
}

fn typed(operation: ChecklistAction) -> Invoke<Checklist> {
    Invoke::apply(&tilo_app::tilo_resource::Stamped {
        operation,
        now_ms: 0,
    })
    .unwrap()
}

#[test]
fn history_restores_a_past_version_through_the_host() {
    let mut app = TestApp::<ChecklistApp>::new().unwrap();
    app.invoke(Checklist::RENAME.with("Groceries".into()))
        .unwrap();
    app.invoke(Checklist::ADD.with("milk".into())).unwrap();
    let past = app.state();
    let version = app.versions().pop().unwrap().hash;
    app.invoke(Checklist::RENAME.with("Hardware".into()))
        .unwrap();
    app.invoke(Checklist::ADD.with("nails".into())).unwrap();
    assert_ne!(app.state(), past);

    let harness = app.harness();
    assert!(
        harness.targets().contains_key("history-open"),
        "a framework app has History"
    );
    harness.click_named("history-open").unwrap();
    let entry = tilo_app::history_source::entry_id_from_hash(&version.to_string());
    let target = format!(
        "timeline-entry:{}",
        tilo_app::widget::history_group::hex16(&entry)
    );
    harness
        .perform_semantic_named(
            "activity_timeline",
            Some(&target),
            &tilo_app::a11y::Action::Activate,
        )
        .unwrap();
    harness.click_named("history-restore").unwrap();
    harness.key("Enter");
    harness.relayout();
    assert_eq!(app.state(), past);

    // Back in the live app, the restore is one undoable step.
    app.harness().click_named("history-back-live").unwrap();
    assert!(app.undo());
    assert_eq!(app.state().title, "Hardware");
}
