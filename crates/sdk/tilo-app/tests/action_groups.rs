//! One action is one undo step; a group of actions is one too.

use tilo_app::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Shopping {
    pub title: String,
    pub items: Collection<Entry>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Entry {
    pub text: String,
    pub done: bool,
}

#[resource(kind = "test.shopping", label = "Shopping list")]
impl Shopping {
    /// Rename the list.
    #[action]
    pub fn rename(&mut self, title: String) {
        self.title = title;
    }

    /// Add an entry.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx, text: String) {
        self.items.push(cx.id(), Entry { text, done: false });
    }

    /// Remove every entry.
    #[action(destructive)]
    pub fn clear(&mut self) {
        self.items = Collection::new();
    }
}

struct ShoppingApp;

impl ResourceApp for ShoppingApp {
    type Resource = Shopping;
    const ID: &'static str = "so.tilo.test-shopping";
    const TITLE: &'static str = "Shopping";

    fn view(cx: &mut ViewCx<Shopping>) -> Element {
        Box::new(Column::new().push(Box::new(cx.button(
            "Start a list",
            Invoke::group([
                Shopping::RENAME.with("Weekend"),
                Shopping::ADD.with("bread"),
                Shopping::ADD.with("coffee"),
            ]),
        ))))
    }
}

fn texts(app: &TestApp<ShoppingApp>) -> Vec<String> {
    app.state()
        .items
        .values()
        .map(|entry| entry.text.clone())
        .collect()
}

#[test]
fn action_groups_are_one_undo_step_as_consequential_as_their_members_and_published() {
    // phase: one_action_is_one_undo_step_and_a_group_is_one_too
    {
        let mut app = TestApp::<ShoppingApp>::new().unwrap();
        app.invoke(Shopping::ADD.with("milk")).unwrap();
        app.invoke(Shopping::ADD.with("eggs")).unwrap();
        assert!(app.undo());
        assert_eq!(texts(&app), ["milk"]);

        // A button bound to a group applies it as one action.
        let versions = app.versions().len();
        app.harness().click_named("group").unwrap();
        assert_eq!(app.state().title, "Weekend");
        assert_eq!(app.state().items.len(), 3);
        assert_eq!(app.versions().len(), versions + 1);
        assert!(app.undo());
        assert_eq!(app.state().title, "");
        assert_eq!(texts(&app), ["milk"]);
        assert!(app.redo());
        assert_eq!(app.state().items.len(), 3);
    }
    // phase: a_group_is_as_consequential_as_its_members
    {
        let mut app = TestApp::<ShoppingApp>::new().unwrap();
        app.invoke(Shopping::ADD.with("milk")).unwrap();
        // One destructive member makes the whole group ask for confirmation.
        let tidy = Invoke::group([Shopping::ADD.with("tea"), Shopping::CLEAR.invoke()]);
        assert!(matches!(
            app.invoke(tidy.clone()),
            Err(tilo_actions::DoorError::ConfirmationRequired)
        ));
        assert_eq!(texts(&app), ["milk"], "nothing applied before confirmation");
        app.invoke_confirmed(tidy).unwrap();
        assert!(app.state().items.is_empty());
    }
    // phase: the_group_action_is_published_for_agents_and_scripts
    {
        let mut app = TestApp::<ShoppingApp>::new().unwrap();
        let resources = app.harness().resources();
        let group = resources
            .iter()
            .flat_map(|resource| &resource.actions)
            .find(|spec| spec.id == tilo_app::tilo_resource::GROUP)
            .expect("group is published");
        assert!(group.is_programmatic(), "never a palette row");
    }
}
