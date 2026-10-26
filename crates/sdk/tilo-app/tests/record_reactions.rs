//! A record that holds reactions shows them in its window's bar, and a
//! press there reacts through the window's dispatcher, as the person.

use tilo_app::permissions::UserId;
use tilo_app::prelude::*;
use tilo_app::tilo_resource::Reactions;
use tilo_app::window_toolbar::{
    ToolbarEvent, ToolbarReaction, ToolbarReactions, deliver, reaction_action, reactions,
};

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Poster {
    pub title: String,
    pub reactions: Reactions,
}

#[resource(kind = "test.poster", label = "Poster")]
impl Poster {
    /// Retitle the poster.
    #[action]
    pub fn retitle(&mut self, title: String) {
        self.title = title;
    }
}

struct PosterApp;

impl ResourceApp for PosterApp {
    type Resource = Poster;
    const ID: &'static str = "so.tilo.test-poster";
    const TITLE: &'static str = "Poster";

    fn view(cx: &mut ViewCx<Poster>) -> Element {
        Box::new(cx.text(|poster| poster.title.clone()))
    }
}

/// The participant a test app edits as.
const ME: &str = "test-author";

fn press(app: &mut TestApp<PosterApp>, emoji: &str) {
    deliver(ToolbarEvent::Action {
        id: reaction_action(emoji),
    });
    app.settle();
}

fn joined_by_bob(app: &mut TestApp<PosterApp>) {
    app.harness()
        .presence_mut()
        .set_display_name(UserId("bob".into()), "Bob");
    app.settle();
}

#[test]
fn a_shared_record_offers_reactions_and_a_press_reacts_as_the_person() {
    let mut app = TestApp::<PosterApp>::new().unwrap();
    app.settle();
    assert_eq!(
        reactions(),
        None,
        "alone in a record without reactions, the bar offers none"
    );

    joined_by_bob(&mut app);
    assert_eq!(
        reactions(),
        Some(ToolbarReactions {
            pills: Vec::new(),
            can_react: true,
        }),
        "shared with Bob, the bar offers the add button"
    );

    press(&mut app, "🎉");
    let groups = app.state().reactions.groups();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].emoji, "🎉");
    assert!(groups[0].includes(ME), "the reaction is the person's own");
    let shown = reactions().expect("shown");
    assert_eq!(
        shown.pills,
        vec![ToolbarReaction {
            emoji: "🎉".into(),
            count: 1,
            mine: true,
            names: Vec::new(),
        }]
    );
    assert!(
        tilo_app::window_toolbar::take_events().is_empty(),
        "the app's own slots never hear a reaction"
    );

    press(&mut app, "🎉");
    assert!(
        app.state().reactions.is_empty(),
        "pressing one's own pill takes the reaction back"
    );
}

#[test]
fn reactions_follow_the_grant_and_a_viewer_sees_them_without_reacting() {
    let mut app = TestApp::<PosterApp>::new().unwrap();
    joined_by_bob(&mut app);
    press(&mut app, "👍");

    app.set_role(tilo_app::Role::Viewer);
    app.settle();
    let shown = reactions().expect("a viewer sees the reactions there are");
    assert!(!shown.can_react, "a viewer may not react");
    assert_eq!(shown.pills.len(), 1);
    press(&mut app, "👀");
    assert_eq!(
        app.state().reactions.len(),
        1,
        "a viewer's press reacts with nothing"
    );

    app.set_role(tilo_app::Role::Commenter);
    app.settle();
    assert!(
        reactions().is_some_and(|shown| shown.can_react),
        "a commenter may react, as they may comment"
    );
    press(&mut app, "👀");
    assert!(
        app.state().reactions.reacted("👀", ME),
        "a commenter's reaction lands: {:?}",
        app.refused()
    );
}

#[test]
fn a_record_in_a_room_with_other_members_offers_reactions_while_they_are_away() {
    use tilo_app::permissions::{LOCAL_ROOM, PermissionContext, Role, Standing, policy};
    let mut app = TestApp::<PosterApp>::new().unwrap();
    app.settle();
    assert_eq!(
        reactions(),
        None,
        "alone in one's own room, nothing is offered"
    );

    // Ada owns the room and is away; the person is an Editor in it.
    let mut room = policy::RoomPolicy::new(LOCAL_ROOM, "ada");
    room.members.insert(ME.into(), Role::Editor);
    app.set_permissions(PermissionContext::from_standing(
        LOCAL_ROOM,
        Standing::person(room, ME),
    ));
    app.settle();
    assert_eq!(
        reactions(),
        Some(ToolbarReactions {
            pills: Vec::new(),
            can_react: true,
        }),
        "the record is shared with Ada, here or not"
    );
}
