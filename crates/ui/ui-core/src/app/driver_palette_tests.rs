//! An app shown by the display-less driver answers what a person and an agent see of it: the rows
//! its search shows, where the person's caret is for their peers, where an agent acts and the view
//! that follows it there, the system's back gesture, the items past a listing's published page,
//! and the device shell its host asks it for.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixture: a failure is the assertion"
)]

use std::cell::Cell;
use std::rc::Rc;

use vello::kurbo::Rect;

use crate::gesture::NavDirection;
use crate::headless::Driver;
use crate::permissions::MemberId;
use crate::state::State;
use crate::widget::{Button, Column, Scrollable, Spacer, TextInput, Widget, resource_actions};

const VIEW: (u32, u32) = (400, 240);

fn viewport() -> Rect {
    Rect::new(0.0, 0.0, f64::from(VIEW.0), f64::from(VIEW.1))
}

/// An agent's `action` on `resource`, a second ago, as its peers' presence carries it.
fn agent_acting(
    driver: &mut Driver,
    resource: &str,
    action: &str,
) -> tilo_actions::SemanticInteraction {
    let mut event = crate::headless::agent_acting(resource, action);
    event.actor = tilo_actions::Acting::agent("scribe", "someone-else");
    let at = crate::time::now()
        .checked_sub(crate::time::Duration::from_secs(1))
        .expect("the clock is later than a second ago");
    driver.update_presence(|presence| presence.apply_interaction_at(event.clone(), at).is_ok());
    event
}

fn scribe() -> MemberId {
    MemberId::agent("scribe", "someone-else")
}

/// A long document whose one `edit` control sits far below the first screen, and how often an
/// edit ran.
fn long_document() -> (Driver, Rc<Cell<u32>>) {
    let ran = Rc::new(Cell::new(0));
    let count = Rc::clone(&ran);
    let resource = resource_actions::resource("doc", "Quarterly report", "edit", "Edit");
    let binding = tilo_actions::ActionBinding::unit_for(&resource, "edit").unwrap();
    let content = Column::new()
        .push(Box::new(Spacer::fixed(2000.0)))
        .push(Box::new(
            Button::new("far", "Closing paragraph").on_action(binding),
        ));
    let root = Scrollable::new_vertical().with_child(resource_actions::immediate(
        Box::new(content),
        resource,
        move |_| {
            count.set(count.get() + 1);
            Ok(tilo_actions::ActionOutput::default())
        },
    ));
    let mut driver = Driver::new(State::new(), Box::new(root), VIEW);
    driver.relayout();
    (driver, ran)
}

#[test]
fn searching_the_window_lists_the_apps_actions_and_escape_puts_it_away() {
    let (mut driver, ran) = long_document();
    assert!(!driver.palette_shown());
    assert!(
        driver.palette_rows().is_empty(),
        "nothing listed while closed"
    );
    driver
        .open_search(&crate::widget::search_request::SearchRequest {
            query: Some("Edit".into()),
            ..crate::widget::search_request::SearchRequest::this_app()
        })
        .unwrap();
    assert!(driver.palette_shown(), "the search shows");
    let rows = driver.palette_rows();
    assert!(
        rows.iter().any(|(label, _)| label == "Edit"),
        "the document's action is a row: {rows:?}"
    );
    let event = agent_acting(&mut driver, "doc:root", "edit");
    assert!(
        driver.semantic_locus(&event).is_none(),
        "the search covers the app: no control of it shows an action"
    );
    driver.key("Escape");
    assert!(!driver.palette_shown(), "Escape puts the search away");
    assert!(driver.palette_rows().is_empty());
    assert_eq!(ran.get(), 0, "looking runs nothing");
}

#[test]
fn a_focused_field_tells_peers_where_its_caret_is_and_an_unfocused_one_says_nothing() {
    let field =
        TextInput::new(vec![crate::presence::PathSegment::Field("title".into())]).with_id("title");
    let key = crate::presence::capability::presence_key(&field).expect("the field has a key");
    let mut driver = Driver::new(
        State::new(),
        Box::new(Column::new().push(Box::new(field))),
        VIEW,
    );
    driver.relayout();
    assert_eq!(driver.focus_presence(), None, "nothing is focused");
    driver.click_named("title").unwrap();
    driver.type_text("Hi");
    assert_eq!(
        driver.focus_presence(),
        Some((key, crate::collaboration::PresencePayload::Caret { pos: 2 })),
        "the caret after what the person typed"
    );
}

#[test]
fn following_an_agent_scrolls_to_the_control_it_acts_on_once_per_action() {
    let (mut driver, ran) = long_document();
    let event = agent_acting(&mut driver, "doc:root", "edit");
    let edit = driver.semantic_locus(&event).expect("the edit's control");
    assert!(
        edit.y0 > viewport().y1,
        "the control starts out of view: {edit:?}"
    );
    let mut elsewhere = crate::app::driver::FollowedView::default();
    assert!(!driver.follow_view(&MemberId::person("someone-else"), &mut elsewhere));
    assert_eq!(
        driver.semantic_locus(&event),
        Some(edit),
        "following someone else moves nothing"
    );
    let mut followed = crate::app::driver::FollowedView::default();
    assert!(driver.follow_view(&scribe(), &mut followed));
    let shown = driver.semantic_locus(&event).expect("the edit's control");
    assert!(
        viewport().contains(shown.origin()) && shown.y1 <= viewport().y1,
        "the control is in view: {shown:?}"
    );
    assert!(
        !driver.follow_view(&scribe(), &mut followed),
        "each action is gone to once"
    );
    assert_eq!(ran.get(), 0, "following runs nothing");
}

#[test]
fn following_an_agent_writing_far_down_a_field_scrolls_to_its_caret() {
    let field = TextInput::new(vec![crate::presence::PathSegment::Field("body".into())]);
    let key = crate::presence::capability::presence_key(&field).expect("the field has a key");
    let root = Scrollable::new_vertical().with_child(Box::new(
        Column::new()
            .push(Box::new(Spacer::fixed(1500.0)))
            .push(Box::new(field)),
    ));
    let mut driver = Driver::new(State::new(), Box::new(root), VIEW);
    driver.relayout();
    let event = agent_acting(&mut driver, "notes.document:root", "edit");
    driver.update_presence(|presence| {
        presence.set_caret(&key, scribe(), 0);
        true
    });
    let caret = driver
        .semantic_locus(&event)
        .expect("the agent is where its caret is");
    assert!(
        caret.y0 > viewport().y1,
        "the caret starts out of view: {caret:?}"
    );
    assert!(caret.height() < 40.0, "on its line: {caret:?}");
    let mut followed = crate::app::driver::FollowedView::default();
    assert!(driver.follow_view(&scribe(), &mut followed));
    let shown = driver.semantic_locus(&event).expect("the caret");
    assert!(
        viewport().contains(shown.origin()) && shown.y1 <= viewport().y1,
        "the caret is in view: {shown:?}"
    );
    assert_eq!(
        driver.focus_presence(),
        None,
        "the follower says nothing of its own"
    );
}

#[test]
fn following_an_agent_into_a_view_not_shown_opens_it_once() {
    let mut resource = resource_actions::resource("sheet", "Sheet 2", "open", "Open");
    // Opening a view changes what is shown, not the sheet: a view operation.
    resource.actions[0].type_mut().kind = tilo_actions::OperationKind::View;
    resource.views.push(tilo_actions::ResourceViewSpec::new(
        "sheet", "Sheet 2", "open",
    ));
    let opened = Rc::new(Cell::new(0));
    let count = Rc::clone(&opened);
    let root = resource_actions::immediate(Box::new(Column::new()), resource, move |_| {
        count.set(count.get() + 1);
        Ok(tilo_actions::ActionOutput::default())
    });
    let mut driver = Driver::new(State::new(), root, VIEW);
    driver.relayout();
    assert!(
        driver.resource("sheet:root").is_some(),
        "the sheet and its view are published"
    );
    agent_acting(&mut driver, "sheet:root", "edit");
    let mut followed = crate::app::driver::FollowedView::default();
    assert!(driver.follow_view(&scribe(), &mut followed));
    assert_eq!(opened.get(), 1, "the sheet's view opened");
    assert!(!driver.follow_view(&scribe(), &mut followed));
    assert_eq!(opened.get(), 1, "opened once");
}

/// A folder browser: Back goes up a folder, its listing publishes one page of `items` files and
/// pages the rest, and it runs the device's shell its host selects.
struct Folders {
    content: Column,
    depth: usize,
    items: usize,
    shell_asked: bool,
}

const PAGE: usize = 3;

fn file(index: usize) -> tilo_actions::ResourceSpec {
    tilo_actions::ResourceSpec::new(
        format!("folders.file:{index}"),
        "folders.file",
        format!("File {index}"),
    )
}

impl crate::widget::Decorator for Folders {
    crate::provides!(dyn crate::widget::resources::PublishesResources, dyn crate::widget::AppWidget; inner = content);
    type Inner = Column;

    fn inner(&self) -> &Column {
        &self.content
    }

    fn inner_mut(&mut self) -> &mut Column {
        &mut self.content
    }

    fn on_nav_gesture(
        &mut self,
        dir: NavDirection,
        _state: &crate::widget::AppModel,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        if dir != NavDirection::Back || self.depth == 0 {
            return false;
        }
        self.depth -= 1;
        true
    }
}

impl crate::widget::resources::PublishesResources for Folders {
    fn resources(&self, _doc: &automerge::AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        let mut folder =
            tilo_actions::ResourceSpec::new("folders.folder:root", "folders.folder", "Home");
        folder.children = (0..PAGE.min(self.items)).map(file).collect();
        vec![folder]
    }

    fn resources_past_page(&self, _doc: &automerge::AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        (PAGE..self.items).map(file).collect()
    }
}

impl crate::widget::AppWidget for Folders {
    fn app_kind(&self) -> crate::widget::AppKind {
        crate::widget::AppKind::Terminal
    }

    fn app_title(&self) -> &str {
        "Folders"
    }

    fn app_icon(&self) -> crate::widget::IconDescriptor {
        crate::widget::IconDescriptor::Glyph("F")
    }

    fn device_shell_operation(
        &mut self,
        operation: &crate::floating_surface::DeviceShellOperation,
    ) -> Result<serde_json::Value, String> {
        match operation {
            crate::floating_surface::DeviceShellOperation::Inspect if self.shell_asked => {
                Ok(serde_json::json!({ "session": "device" }))
            }
            _ => Err("Session no longer matches".into()),
        }
    }

    fn request_device_shell(&mut self) -> bool {
        self.shell_asked = true;
        true
    }
}

fn folders(depth: usize, items: usize) -> Driver {
    let root = Folders {
        content: Column::new().push(Box::new(Button::new("open", "Open"))),
        depth,
        items,
        shell_asked: false,
    };
    let mut driver = Driver::for_root(State::new(), root, VIEW, crate::theme::Theme::dark());
    driver.relayout();
    driver
}

#[test]
fn the_back_gesture_goes_up_a_folder_and_at_the_top_is_left_to_the_host() {
    let mut driver = folders(2, 0);
    assert!(driver.nav_gesture(NavDirection::Back), "the app takes Back");
    assert_eq!(driver.root::<Folders>().depth, 1);
    assert!(
        !driver.nav_gesture(NavDirection::Forward),
        "nothing is ahead to go forward to"
    );
    assert!(driver.nav_gesture(NavDirection::Back));
    assert_eq!(driver.root::<Folders>().depth, 0);
    assert!(
        !driver.nav_gesture(NavDirection::Back),
        "at the top the host closes the window"
    );
}

#[test]
fn a_listing_of_thousands_pages_every_item_past_its_published_page() {
    let mut driver = folders(0, 10_000);
    assert_eq!(driver.past_page_len(), 10_000 - PAGE);
    let page = driver.past_page_range(0, 4);
    let ids: Vec<&str> = page.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "folders.file:3",
            "folders.file:4",
            "folders.file:5",
            "folders.file:6"
        ]
    );
    let last = driver.past_page_range(10_000 - PAGE - 1, 4);
    assert_eq!(
        last.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(),
        ["folders.file:9999"],
        "the last page holds what is left"
    );
    assert!(driver.past_page_range(10_000, 4).is_empty());
    driver = folders(0, PAGE);
    assert_eq!(driver.past_page_len(), 0, "one page publishes everything");
}

#[test]
fn the_host_selects_the_device_shell_and_then_operates_it() {
    let mut driver = folders(0, 0);
    let inspect = crate::floating_surface::DeviceShellOperation::Inspect;
    assert_eq!(
        driver.device_shell_operation(&inspect),
        Err("Session no longer matches".into()),
        "no shell before the host selects one"
    );
    assert!(driver.request_device_shell(), "the app takes the selection");
    assert_eq!(
        driver.device_shell_operation(&inspect),
        Ok(serde_json::json!({ "session": "device" }))
    );
    let mut plain = Driver::new(State::new(), Box::new(Column::new()), VIEW);
    assert!(
        !plain.request_device_shell(),
        "an app with no shell refuses it"
    );
    assert_eq!(
        plain.device_shell_operation(&inspect),
        Err("Terminal is not available".into())
    );
}

// ---- a semantic locus only where one control shows the action ----

/// Two resources whose `edit` a button runs, the same label on both.
fn locus_root() -> (Box<dyn Widget>, Rc<Cell<u32>>) {
    let calls = Rc::new(Cell::new(0));
    let mut column = Column::new();
    for id in ["first", "second"] {
        let mut resource = resource_actions::resource(id, "Same label", "edit", "Edit");
        resource.id = tilo_actions::ResourceId::new(id);
        let binding = tilo_actions::ActionBinding::unit_for(&resource, "edit").unwrap();
        let counter = Rc::clone(&calls);
        column = column.push(resource_actions::immediate(
            Box::new(Button::new(id, "Same label").on_action(binding)),
            resource,
            move |_| {
                counter.set(counter.get() + 1);
                Ok(tilo_actions::ActionOutput::default())
            },
        ));
    }
    (Box::new(column), calls)
}

fn locus_interaction(resource: &str) -> tilo_actions::SemanticInteraction {
    let mut event = crate::headless::agent_acting(resource, "edit");
    event.actor = tilo_actions::Acting::person("synthetic-test-only");
    event.action.label = "Edit".into();
    event.resource.kind = "example".into();
    event.resource.label = "Same label".into();
    event
}

#[test]
fn semantic_locus_refuses_multiple_affordances_for_one_action() {
    use crate::widget::{Button, Column, resource_actions};
    let resource = resource_actions::resource("shared", "Same label", "edit", "Edit");
    let binding = tilo_actions::ActionBinding::unit_for(&resource, "edit").unwrap();
    let root = resource_actions::immediate(
        Box::new(
            Column::new()
                .push(Box::new(
                    Button::new("a", "Same label").on_action(binding.clone()),
                ))
                .push(Box::new(Button::new("b", "Same label").on_action(binding))),
        ),
        resource,
        |_| panic!("observation must not execute"),
    );
    let mut driver = Driver::new(State::new(), root, (400, 240));
    driver.relayout();
    assert_eq!(
        driver
            .action_ui_routes(&tilo_actions::ActionAddress::new("shared:root", "edit"))
            .len(),
        2
    );
    assert_eq!(
        driver.semantic_locus(&locus_interaction("shared:root")),
        None
    );
}

#[test]
fn semantic_locus_refuses_control_id_reuse_across_distinct_resources() {
    use crate::widget::{Button, Column, Container, resource_actions};
    let mut root = Column::new();
    for kind in ["one", "two"] {
        let resource = resource_actions::resource(kind, "Same label", "edit", "Edit");
        let owner = resource.id.as_str().to_owned();
        let binding = tilo_actions::ActionBinding::unit_for(&resource, "edit").unwrap();
        // A widget id addresses a container for inspection; only a semantic key such as a
        // landmark makes it an owner, so each resource's surface keeps its own `edit` control.
        root = root.push(resource_actions::immediate(
            Box::new(Container::new().landmark(kind, owner).with_child(Box::new(
                Column::new().push(Box::new(
                    Button::new("edit", "Same label").on_action(binding),
                )),
            ))),
            resource,
            |_| panic!("observation must not execute"),
        ));
    }
    let mut driver = Driver::new(State::new(), Box::new(root), (400, 300));
    driver.relayout();
    let paths = driver.semantic_tree().control_paths("edit");
    assert_eq!(
        paths.len(),
        2,
        "fixture contains distinct owners with the same control id"
    );
    for target in paths.iter().filter_map(|path| path.last()) {
        let mut event = locus_interaction("one:root");
        event.anchor = tilo_actions::InteractionAnchor::SemanticControl {
            id: target.as_str().into(),
        };
        assert_eq!(driver.semantic_locus(&event), None);
    }
    assert!(
        driver
            .action_ui_routes(&tilo_actions::ActionAddress::new("one:root", "edit"))
            .is_empty()
    );
}

#[test]
fn semantic_locus_refuses_a_control_whose_resource_is_no_longer_published() {
    use crate::widget::{Button, resource_actions};
    let resource = resource_actions::resource("stale", "Same label", "edit", "Edit");
    let binding = tilo_actions::ActionBinding::unit_for(&resource, "edit").unwrap();
    let mut driver = Driver::new(
        State::new(),
        Box::new(Button::new("stale-control", "Same label").on_action(binding)),
        (400, 240),
    );
    driver.relayout();
    assert_eq!(
        driver.semantic_locus(&locus_interaction("stale:root")),
        None
    );
}

/// A control below an unkeyed parent has no durable identity: the address a frame builds for it
/// names this frame's tree and no resource, so it never stands for that resource's action.
#[test]
fn semantic_locus_refuses_a_frame_local_projected_resource_identity() {
    let (inner, _) = locus_root();
    let root = Scrollable::new_vertical().with_child(inner);
    let mut driver = Driver::new(State::new(), Box::new(root), (400, 240));
    driver.relayout();
    let route = driver.action_ui_routes(&tilo_actions::ActionAddress::new("first", "edit"));
    let target = route[0].path.last().unwrap().clone();
    assert!(
        !driver
            .semantic_tree()
            .node(&target)
            .expect("the control's node")
            .durable_identity,
        "fixture sits below an unkeyed parent"
    );
    let encoded =
        percent_encoding::utf8_percent_encode(target.as_str(), percent_encoding::NON_ALPHANUMERIC);
    let mut event = locus_interaction(&format!("ui:{encoded}"));
    event.action.id = "activate".into();
    assert_eq!(driver.semantic_locus(&event), None);
}

// ---- a confirmation asked in a window ----

/// A confirmation is the device's ask, not a card the app stands: the press is kept as the ask,
/// named, and nothing of it runs; declining drops it, and the same press sent again with the
/// person's approve runs once.
#[test]
fn a_confirmation_asked_in_a_window_waits_for_the_person_and_runs_once() {
    let (root, log, _) =
        crate::app::control_fixture_tests::fixture(tilo_actions::ConfirmationPolicy::Confirm);
    let mut driver = Driver::from_managed_root_with_controlled_inputs(
        State::new(),
        root,
        VIEW,
        crate::Theme::dark(),
        crate::font_catalog::fixture_renderer(),
        MemberId::person("fixture"),
    )
    .unwrap();
    driver.connect_peer();
    driver.relayout();
    assert_eq!(driver.asking(), None, "nothing is asked before the press");

    driver
        .perform_named("ordinary-emitter", &crate::a11y::Action::Activate)
        .unwrap();
    let (address, name) = driver
        .asking()
        .expect("the window's press waits for the person's yes");
    assert!(!name.is_empty(), "the ask names the action");
    assert!(log.borrow().is_empty(), "nothing of it runs before the yes");

    assert!(driver.decline_asked().is_some(), "the person declines");
    assert_eq!(driver.asking(), None);
    assert!(log.borrow().is_empty(), "a declined call runs nothing");

    driver
        .perform_named("ordinary-emitter", &crate::a11y::Action::Activate)
        .unwrap();
    assert!(driver.asking().is_some(), "the press asks again");
    assert_eq!(
        driver.confirm_asked().map(|asked| asked.action),
        Some(address.action),
        "the allowed call is the one that was asked"
    );
    assert_eq!(log.borrow().len(), 1, "it runs once");
}
