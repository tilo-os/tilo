//! A record that holds reactions shows them in its window's bar, and a
//! press there reacts through the window's dispatcher, as the person.

use tilo_app::prelude::*;
use tilo_app::tilo_resource::Reactions;
use tilo_app::window_toolbar::{ToolbarReactions, reactions};
use tilo_app_test_host::TestApp;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Poster {
    pub title: String,
    pub reactions: Reactions,
}

#[resource(kind = "test.poster", catalogue = "tests/locales")]
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

/// The member a test app edits as.
const ME: &str = "test-author";

#[test]
fn a_record_in_a_workspace_with_other_members_offers_reactions_while_they_are_away() {
    let mut app = TestApp::<PosterApp>::new().unwrap();
    app.settle();
    assert_eq!(
        reactions(),
        None,
        "alone in one's own workspace, nothing is offered"
    );

    // The workspace's members' resource lists Ada beside the person; she is away.
    tilo_app::tilo_resource::members::know([
        (MemberId::person("ada"), "Ada".to_owned()),
        (MemberId::person(ME), "Me".to_owned()),
    ]);
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
