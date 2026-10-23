#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! Child records a window's own record lists: creating, trashing and moving
//! one writes the record the window holds, so each step goes through the
//! window's own action path rather than the store beside it.

use tilo_app::framework::ChildRecord;
use tilo_app::prelude::*;
use tilo_app::tilo_resource::Segment;
use tilo_app::tilo_resource::children::Children as Container;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Room {
    pub entries: Container,
}

#[resource(kind = "test.child-room", label = "Room")]
impl Room {
    /// Rename nothing.
    #[action]
    pub fn touch(&mut self) {}
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Topic {
    #[model(label)]
    pub name: String,
}

#[resource(kind = "test.child-topic", label = "Topics")]
impl Topic {
    /// Rename the topic.
    #[action]
    pub fn rename(&mut self, name: String) {
        self.name = name;
    }
}

impl ChildRecord for Topic {
    type Parent = Room;
    fn container(parent: &Room) -> &Container {
        &parent.entries
    }
    fn container_path() -> Vec<Segment> {
        vec![Segment::Field("entries".into())]
    }
}

/// Buttons that make a topic, trash the first and move the last to the
/// front, through the topics the view holds.
struct TopicsApp;

impl ResourceApp for TopicsApp {
    type Resource = Room;
    const ID: &'static str = "so.tilo.test-child-records";
    const TITLE: &'static str = "Topics";

    fn records(records: &mut Records<'_>) {
        records.children::<Topic>();
    }

    fn view(cx: &mut ViewCx<Room>) -> Element {
        let topics = cx.children::<Topic>().expect("a window someone uses");
        let (making, trashing, moving) = (topics.clone(), topics.clone(), topics);
        Box::new(
            Column::new()
                .push(Box::new(Button::new("make", "Make").on_view(move || {
                    making
                        .create(serde_json::json!({ "name": "design" }), None, None)
                        .unwrap();
                })))
                .push(Box::new(Button::new("trash", "Trash").on_view(move || {
                    let (first, _, _) = trashing.children().into_iter().next().unwrap();
                    trashing.delete(&first).unwrap();
                })))
                .push(Box::new(Button::new("move", "Move").on_view(move || {
                    let listed = moving.children();
                    let (last, _, _) = listed.last().unwrap();
                    let (first, _, _) = listed.first().unwrap();
                    moving.move_to(last, None, Some(first)).unwrap();
                }))),
        )
    }
}

/// The topics the window's record lists, in order, and whether each is in
/// the trash.
fn listed(app: &TestApp<TopicsApp>) -> Vec<(Id, bool)> {
    app.state()
        .entries
        .iter()
        .map(|(id, child)| (id.clone(), child.trashed.is_some()))
        .collect()
}

#[test]
fn create_delete_and_move_write_the_record_the_window_holds() {
    let mut app = TestApp::<TopicsApp>::for_person(&"ad".repeat(32)).unwrap();
    for _ in 0..2 {
        app.harness().click_named("make").unwrap();
        app.settle();
        assert!(app.refused().is_none(), "{:?}", app.refused());
    }
    let made = listed(&app);
    assert_eq!(made.len(), 2, "both listed in the window's record");
    assert!(made.iter().all(|(_, trashed)| !trashed));

    app.harness().click_named("move").unwrap();
    app.settle();
    assert!(app.refused().is_none(), "{:?}", app.refused());
    let moved: Vec<Id> = listed(&app).into_iter().map(|(id, _)| id).collect();
    assert_eq!(moved, [made[1].0.clone(), made[0].0.clone()]);

    // Trashing can be undone, so it is not confirmed first.
    app.harness().click_named("trash").unwrap();
    app.settle();
    assert!(app.confirm_asked().is_none(), "the host does not ask");
    assert!(app.refused().is_none(), "{:?}", app.refused());
    let trashed = listed(&app);
    assert_eq!(trashed[0], (made[1].0.clone(), true), "kept, in the trash");
    assert_eq!(trashed[1], (made[0].0.clone(), false));
}
