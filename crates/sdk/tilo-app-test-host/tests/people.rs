//! Who an app's people list offers: the workspace's members and the agents it keeps, and
//! nobody a device named that the workspace never admitted (a page's host is such a
//! device). The picker built over it lays one row out per offered member.

use tilo_app::prelude::*;
use tilo_app_test_host::TestApp;

/// The workspace the window is in.
const WORKSPACE: &str = "6f0c3a52-8e1b-4d6a-9c2f-3b7e5d1a4c90";

/// An issue-like resource with an assignee, so the picker has an action to bind.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Ticket {
    pub assignee: Option<String>,
}

#[resource(
    kind = "test.ticket",
    app = "so.tilo.test-people",
    catalogue = "tests/locales"
)]
impl Ticket {
    /// Give the ticket to someone, or to nobody.
    #[action]
    pub fn assign(&mut self, assignee: Option<String>) {
        self.assignee = assignee;
    }
}

/// The app whose view lists its people and offers the picker of them.
struct PeopleApp;

impl ResourceApp for PeopleApp {
    type Resource = Ticket;
    const ID: &'static str = "so.tilo.test-people";
    const TITLE: &'static str = "People";

    fn view(cx: &mut ViewCx<Ticket>) -> Element {
        let people = cx.people();
        let roster = people.revision();
        let listed = cx.switch(&roster, move |cx, _| {
            let names = people
                .members()
                .iter()
                .map(|member| member.name.clone())
                .collect::<Vec<_>>()
                .join(", ");
            let picker = cx.person_picker(
                "test-picker",
                "Assignee".to_owned(),
                "Nobody".to_owned(),
                |member| Some(member.name.clone()),
                |ticket: &Ticket| {
                    ticket
                        .assignee
                        .as_deref()
                        .map(tilo_app::framework::Actor::outside)
                },
                Ticket::ASSIGN.map(|assignee: Option<String>| ticket::Assign { assignee }),
            );
            Box::new(
                Column::new()
                    .push(Box::new(Text::new(names.as_str())))
                    .push(picker),
            ) as Element
        });
        Box::new(Column::new().push(listed))
    }
}

/// A person key, as the workspace names one.
fn person(seed: char) -> String {
    seed.to_string().repeat(64)
}

/// Everything the test needs to know about the window's people.
struct Fixture {
    app: TestApp<PeopleApp>,
    agent: String,
}

/// A window in a workspace with two people admitted, an agent it keeps, and a host
/// device that named itself but was never admitted.
fn fixture() -> Fixture {
    let mut app = TestApp::<PeopleApp>::in_workspace(WORKSPACE).unwrap();
    let (me, keeper, host) = (
        MemberId::person(person('1')),
        MemberId::person(person('2')),
        MemberId::person(person('3')),
    );
    let agent = "planner".to_owned();
    let presence = app.harness().presence_mut();
    presence.set_self_member(me.clone(), "Happy Heron");
    presence.set_display_name(keeper.clone(), "Ada Lovelace");
    presence.set_workspace_members([me, keeper]);
    // The workspace's agents resource names each to presence, as the page does.
    presence.set_display_name(MemberId::agent(agent.clone()), "Planner");
    // The page's host: a device that names a person the workspace never admitted.
    presence.set_display_name(host, "lounge workspace host");
    app.settle();
    Fixture { app, agent }
}

#[test]
fn the_people_a_page_offers_are_the_workspaces_members_and_agents() {
    let mut fixture = fixture();
    let shown = fixture.app.texts().join(" ");
    assert!(
        shown.contains("Happy Heron"),
        "the window's own person: {shown}"
    );
    assert!(
        shown.contains("Ada Lovelace"),
        "an admitted member: {shown}"
    );
    assert!(shown.contains("Planner"), "the workspace's agent: {shown}");
    assert!(
        !shown.contains("lounge workspace host"),
        "the host names nobody the workspace admitted: {shown}"
    );
}

#[test]
fn the_person_at_the_window_is_offered_after_a_reload() {
    // A restored window hears the workspace's member list and its agents before its board
    // says who is here: the person at the window is one of them either way.
    let mut app = TestApp::<PeopleApp>::in_workspace(WORKSPACE).unwrap();
    let me = MemberId::person(person('1'));
    let presence = app.harness().presence_mut();
    presence.set_self_member(me.clone(), "Jolly Hedgehog");
    presence.set_workspace_members([me]);
    presence.set_display_name(MemberId::agent("planner"), "Planner");
    app.settle();
    let shown = app.texts().join(" ");
    assert!(
        shown.contains("Jolly Hedgehog"),
        "the person at the window: {shown}"
    );
    assert!(shown.contains("Planner"), "the workspace's agent: {shown}");
}

#[test]
fn the_person_picker_lays_one_row_out_per_member() {
    let mut fixture = fixture();
    fixture
        .app
        .harness()
        .click_named("test-picker")
        .expect("the picker's chip is on screen");
    fixture.app.settle();
    let targets = fixture.app.harness().targets();
    // The rows in the order the picker lists them: nobody first, then the members by name.
    let rows = [
        "test-picker-none".to_owned(),
        format!("test-picker-person-{}", person('2')),
        format!("test-picker-person-{}", person('1')),
        format!("test-picker-agent-{}", fixture.agent),
    ];
    let shown = targets
        .keys()
        .filter(|key| key.starts_with("test-picker-"))
        .count();
    assert_eq!(
        shown,
        rows.len(),
        "one row per offered member and nobody else: {targets:?}"
    );
    let rects: Vec<(String, f64, f64, f64, f64)> = rows
        .iter()
        .map(|row| {
            let rect = *targets
                .get(row)
                .unwrap_or_else(|| panic!("{row} is a row of the picker: {targets:?}"));
            (row.clone(), rect.x0, rect.y0, rect.width(), rect.height())
        })
        .collect();
    for (row, _, _, width, height) in &rects {
        assert!(
            *height > 0.0,
            "{row} has a row's height, not the popover's share: {rects:?}"
        );
        assert!(*width > 0.0, "{row} stands a row wide: {rects:?}");
    }
    for pair in rects.windows(2) {
        assert!(
            pair[1].2 > pair[0].2 + pair[0].4 - 1.0,
            "each row stands under the one before it: {pair:?}"
        );
    }
}
