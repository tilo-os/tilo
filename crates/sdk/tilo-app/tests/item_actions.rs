//! Actions on the items of a collection are offered on the items: in the
//! published tree (palette, agents) and in a row's context menu.

use tilo_app::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Todos {
    pub items: Collection<Todo>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Todo {
    pub text: String,
    pub done: bool,
}

#[resource(kind = "test.todos", label = "Todos")]
impl Todos {
    /// Add a todo.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx, text: String) {
        self.items.push(cx.id(), Todo { text, done: false });
    }

    /// Check or uncheck a todo.
    #[action]
    pub fn set_done(&mut self, item: &Id, done: bool) -> Result<(), &'static str> {
        self.items.get_mut(item).ok_or("no such todo")?.done = done;
        Ok(())
    }

    /// Remove a todo.
    #[action]
    pub fn remove(&mut self, item: &Id) {
        self.items.remove(item);
    }
}

struct TodosApp;

impl ResourceApp for TodosApp {
    type Resource = Todos;
    const ID: &'static str = "so.tilo.test-todos";
    const TITLE: &'static str = "Todos";

    fn view(cx: &mut ViewCx<Todos>) -> Element {
        Box::new(Column::new().push(Box::new(cx.each(
            |todos| &todos.items,
            |row| {
                let id = row.item().cloned().unwrap_or_else(|| Id::from_raw(""));
                let label = row.read(|todo| todo.map(|todo| todo.text.clone()).unwrap_or_default());
                let control = format!("set-done-{id}");
                Box::new(row.control_with(
                    |shown| {
                        Checkbox::from_state(shown)
                            .with_id(&control)
                            .with_label(label)
                    },
                    |todo| todo.done,
                    Todos::SET_DONE.map(move |done| (id.clone(), done)),
                ))
            },
        ))))
    }
}

fn texts(app: &TestApp<TodosApp>) -> Vec<String> {
    app.state()
        .items
        .values()
        .map(|todo| todo.text.clone())
        .collect()
}

#[test]
fn item_actions_are_offered_on_the_item() {
    let mut app = TestApp::<TodosApp>::new().unwrap();
    app.invoke(Todos::ADD.with("milk")).unwrap();
    app.invoke(Todos::ADD.with("eggs")).unwrap();
    let milk = app.state().items.ids().next().cloned().unwrap();

    // The palette and agents see each item as a resource of its own, with
    // the item's actions bound to it.
    let resources = app.harness().resources();
    let root = resources
        .iter()
        .find(|resource| resource.kind == Todos::KIND)
        .unwrap();
    let item = root
        .children
        .iter()
        .find(|child| child.label == "milk")
        .expect("the item is published");
    let actions: Vec<_> = item.actions.iter().map(|spec| spec.id.as_str()).collect();
    // The item verbs with the item bound, and the item's own fields.
    assert_eq!(actions, ["set-done", "remove", "field-edit"]);
    let set_done = item.actions.first().unwrap();
    let params: Vec<_> = set_done.params().iter().map(|p| p.name.as_ref()).collect();
    assert_eq!(params, ["done"], "the item is bound, not asked for");
    let on_record = root
        .actions
        .iter()
        .find(|spec| spec.id == "remove")
        .unwrap();
    assert!(on_record.is_programmatic(), "no id prompt in the palette");

    // The row's context menu offers them too: focus the row, open its menu
    // from the keyboard and choose "Remove a todo".
    app.harness()
        .click_named(&format!("set-done-{milk}"))
        .unwrap();
    app.harness().set_modifiers(&["shift".into()]);
    app.harness().key("F10");
    app.harness().set_modifiers(&[]);
    let menu = app.harness().inspection_json().unwrap();
    assert!(menu.contains("Remove a todo"), "{menu}");
    app.harness().key("ArrowDown");
    app.harness().key("ArrowDown");
    app.harness().key("Enter");
    app.harness().relayout();
    assert_eq!(texts(&app), ["eggs"]);
    assert!(app.undo());
    assert_eq!(app.state().items.len(), 2);
}
