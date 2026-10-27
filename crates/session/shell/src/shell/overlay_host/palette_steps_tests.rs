//! The palette's interaction on the production router: Escape, the steps
//! that collect an action's arguments, "Show all", where a row opens, and
//! rows that keep showing the state they act on.
use super::*;
use crate::shell::search::action_api::{ActionArgs, ActionError, ActionParam, ParamType};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};
use std::cell::RefCell;
use std::rc::Rc;

/// What the fixture's rows were run with.
type Runs = Rc<RefCell<Vec<String>>>;

/// Rows for one query: two actions with typed parameters, an app, and a
/// long run of files.
struct StepsFixture {
    runs: Runs,
}

const QUERY: &str = "zqfixture";

impl StepsFixture {
    fn action(
        &self,
        id: &'static str,
        title: &'static str,
        params: Vec<ActionParam>,
        score: i32,
    ) -> SearchResult {
        let runs = self.runs.clone();
        let mut row = SearchResult::nullary(
            ResourceKind::Action,
            "steps-fixture",
            id,
            title,
            None,
            score,
            |_, _, _| {},
        );
        row.params = params;
        row.on_open = recording(move |args| {
            let value = args
                .integer("percent")
                .map(|n| n.to_string())
                .or_else(|| args.text("secret").map(str::to_string))
                .unwrap_or_default();
            runs.borrow_mut().push(format!("{id}={value}"));
        });
        row
    }
}

/// An invocation that hands its bound arguments to `record`.
fn recording(record: impl Fn(&ActionArgs) + 'static) -> Rc<crate::shell::search::OpenFn> {
    Rc::new(
        move |_: &mut ui::widget::Edits<'_>,
              _: &ObjId,
              _: &crate::shell::chrome_state::Chrome,
              args: &ActionArgs|
              -> Result<(), ActionError> {
            record(args);
            Ok(())
        },
    )
}

impl SearchProvider for StepsFixture {
    fn id(&self) -> &'static str {
        "steps-fixture"
    }
    fn name(&self) -> &'static str {
        "Steps fixture"
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[
            ResourceKind::Action,
            ResourceKind::Application,
            ResourceKind::File,
        ]
    }
    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.needle.starts_with(QUERY) {
            return Vec::new();
        }
        let mut rows = vec![
            self.action(
                "scale",
                "Zqfixture scale",
                vec![
                    ActionParam::required("percent", ParamType::Integer, "Scale in percent")
                        .with_choices(["100", "125", "150"]),
                ],
                100,
            ),
            self.action(
                "unlock",
                "Zqfixture unlock",
                vec![ActionParam::required(
                    "secret",
                    ParamType::Secret,
                    "Password",
                )],
                99,
            ),
        ];
        let mut lock = self.action(
            "lock",
            "Zqfixture lock",
            vec![
                ActionParam::required("minutes", ParamType::Integer, "Minutes")
                    .with_choices(["1", "10"]),
                ActionParam::optional("confirm", ParamType::Boolean, "Confirm")
                    .with_labelled_choices([("true", "Make it less safe")]),
            ],
            97,
        );
        let runs = self.runs.clone();
        lock.on_open = Rc::new(
            move |_: &mut ui::widget::Edits<'_>,
                  _: &ObjId,
                  _: &crate::shell::chrome_state::Chrome,
                  args: &ActionArgs|
                  -> Result<(), ActionError> {
                let minutes = args.integer("minutes").unwrap_or_default();
                let confirmed = args.boolean("confirm") == Some(true);
                if minutes > 1 && !confirmed {
                    return Err(ActionError::ConfirmationRequired);
                }
                runs.borrow_mut()
                    .push(format!("lock={minutes} {confirmed}"));
                Ok(())
            },
        );
        rows.push(lock);
        let runs = self.runs.clone();
        rows.push(SearchResult::nullary(
            ResourceKind::Application,
            "steps-fixture",
            "app:so.tilo.notes",
            "Zqfixture notes",
            None,
            98,
            move |_, _, chrome| {
                runs.borrow_mut()
                    .push(format!("notes@{:?}", chrome.borrow().open_target));
            },
        ));
        for i in 0..40 {
            rows.push(SearchResult::nullary(
                ResourceKind::File,
                "steps-fixture",
                format!("/home/fixture/zqfixture-{i}.txt"),
                format!("zqfixture-{i}.txt"),
                Some(format!("~/zqfixture-{i}.txt")),
                90_i32.saturating_sub(i),
                |_, _, _| {},
            ));
        }
        rows.into_iter()
            .filter(|row| ctx.wanted.contains(&row.kind))
            .collect()
    }
}

fn steps_harness(query: &str) -> (AbsorbHarness, Runs) {
    let mut h = build_harness_sized(1280.0, 800.0);
    let runs = Runs::default();
    h.host
        .search_registry
        .register(Rc::new(StepsFixture { runs: runs.clone() }));
    h.host.set_palette_visible(true);
    h.host.set_palette_focus(true);
    h.host.force_palette_settled();
    h.host.set_palette_query(query);
    h.host
        .refresh_search_results(&AutoCommit::new(), &automerge::ROOT);
    settle(&mut h, 1280.0, 800.0);
    (h, runs)
}

fn select(h: &mut AbsorbHarness, title: &str) {
    let index = h
        .host
        .palette
        .visible_entries()
        .iter()
        .position(|entry| entry.label == title)
        .unwrap_or_else(|| panic!("{title} is listed"));
    h.host.palette.set_cursor(index);
}

fn press(h: &mut AbsorbHarness, key: NamedKey) -> bool {
    pump(h, &key_pressed(Key::Named(key)))
}

fn typed(h: &mut AbsorbHarness, text: &str) {
    for c in text.chars() {
        let s = c.to_string();
        let event = ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
            Key::Character(s.as_str().into()),
            Some(s.as_str().into()),
            ElementState::Pressed,
        ));
        pump(h, &event);
    }
}

fn labels(h: &AbsorbHarness) -> Vec<String> {
    h.host
        .palette
        .visible_entries()
        .iter()
        .map(|entry| entry.label.clone())
        .collect()
}

#[test]
fn escape_clears_what_was_typed_before_it_closes_search() {
    let (mut h, _) = steps_harness(QUERY);
    assert!(press(&mut h, NamedKey::Escape));
    assert!(
        h.host.palette_visible(),
        "the first Escape keeps search open"
    );
    assert_eq!(h.host.palette_query(), "", "…and clears the query");
    assert!(press(&mut h, NamedKey::Escape));
    assert!(!h.host.palette_visible(), "Escape on an empty field closes");
}

#[test]
fn a_parameter_with_values_is_a_list_to_pick_from_never_json() {
    let (mut h, runs) = steps_harness(QUERY);
    select(&mut h, "Zqfixture scale");
    assert!(press(&mut h, NamedKey::Enter));
    assert!(h.host.palette_visible());
    assert_eq!(labels(&h), vec!["100", "125", "150"], "the values are rows");
    assert!(
        !h.host.palette.query().contains('{'),
        "nothing asks for JSON"
    );
    assert!(press(&mut h, NamedKey::ArrowDown));
    assert!(press(&mut h, NamedKey::Enter));
    assert_eq!(runs.borrow().as_slice(), ["scale=125"]);
    assert!(
        !h.host.palette_visible(),
        "a completed action closes search"
    );
}

#[test]
fn a_value_that_makes_it_less_safe_asks_one_more_step_to_confirm() {
    let (mut h, runs) = steps_harness(QUERY);
    select(&mut h, "Zqfixture lock");
    assert!(press(&mut h, NamedKey::Enter));
    assert_eq!(labels(&h), vec!["1", "10"]);
    assert!(press(&mut h, NamedKey::Enter));
    assert_eq!(
        runs.borrow().as_slice(),
        ["lock=1 false"],
        "a safer value runs at once"
    );

    let (mut h, runs) = steps_harness(QUERY);
    select(&mut h, "Zqfixture lock");
    assert!(press(&mut h, NamedKey::Enter));
    assert!(press(&mut h, NamedKey::ArrowDown));
    assert!(press(&mut h, NamedKey::Enter));
    assert!(h.host.palette_visible(), "nothing ran yet");
    assert_eq!(labels(&h), vec!["Make it less safe"], "the confirmation");
    assert!(runs.borrow().is_empty());
    assert!(press(&mut h, NamedKey::Enter));
    assert_eq!(
        runs.borrow().last().map(String::as_str),
        Some("lock=10 true")
    );
    assert!(!h.host.palette_visible());
}

#[test]
fn words_after_the_title_name_the_value_and_run_at_once() {
    let (mut h, runs) = steps_harness("zqfixture scale 150");
    select(&mut h, "Zqfixture scale");
    assert!(press(&mut h, NamedKey::Enter));
    assert_eq!(runs.borrow().as_slice(), ["scale=150"]);
}

#[test]
fn escape_in_a_step_returns_to_the_results_as_they_were() {
    let (mut h, runs) = steps_harness(QUERY);
    select(&mut h, "Zqfixture scale");
    assert!(press(&mut h, NamedKey::Enter));
    assert!(press(&mut h, NamedKey::Escape));
    assert!(h.host.palette_visible());
    assert_eq!(h.host.palette_query(), QUERY, "what was typed comes back");
    h.host
        .refresh_open_palette(&AutoCommit::new(), &automerge::ROOT);
    assert!(
        labels(&h).iter().any(|label| label == "Zqfixture scale"),
        "the results come back: {:?}",
        labels(&h)
    );
    assert!(runs.borrow().is_empty(), "nothing ran");
}

#[test]
fn a_secret_is_typed_into_a_masked_field_and_never_into_the_query() {
    let (mut h, runs) = steps_harness(QUERY);
    select(&mut h, "Zqfixture unlock");
    assert!(press(&mut h, NamedKey::Enter));
    assert!(h.host.palette.in_argument_mode(), "a field step");
    typed(&mut h, "hunter2");
    assert_eq!(h.host.palette_query(), QUERY, "the secret is not the query");
    let node = h
        .host
        .palette
        .accessibility()
        .expect("the palette publishes");
    let field = node.children.first().expect("its field");
    assert!(field.state.protected, "the field says it holds a secret");
    assert!(field.value.is_none(), "and never what: {:?}", field.value);
    assert!(press(&mut h, NamedKey::Enter));
    assert_eq!(runs.borrow().as_slice(), ["unlock=hunter2"]);
}

#[test]
fn a_capped_group_shows_the_rest_in_place_from_its_show_all_row() {
    let (mut h, _) = steps_harness(QUERY);
    let rows = labels(&h);
    let files = rows
        .iter()
        .filter(|label| label.starts_with("zqfixture-"))
        .count();
    assert!(files < 40, "the files share the list: {files}");
    let more = h
        .host
        .palette
        .visible_entries()
        .iter()
        .position(|entry| entry.id.starts_with(palette_flow::MORE_PREFIX))
        .expect("a capped group offers the rest");
    assert!(
        h.host.palette.visible_entries()[more].label.contains("40"),
        "it says how many there are"
    );
    h.host.palette.set_cursor(more);
    assert!(press(&mut h, NamedKey::Enter));
    let files = labels(&h)
        .iter()
        .filter(|label| label.starts_with("zqfixture-"))
        .count();
    assert_eq!(files, 40, "every file is listed now");
    assert_eq!(
        h.host.palette.cursor(),
        more,
        "the selection stays in place"
    );
    assert!(h.host.palette_visible());
}

#[test]
fn shift_and_ctrl_enter_open_a_window_here_or_beside_the_focused_one() {
    use crate::shell::chrome_state::OpenTarget;
    let (mut h, runs) = steps_harness(QUERY);
    let group = h
        .host
        .focused_group()
        .expect("the demo desktop has a focused group");
    select(&mut h, "Zqfixture notes");
    let entry = h.host.palette.visible_entries()[h.host.palette.cursor()].clone();
    assert_eq!(
        entry.alternates.len(),
        2,
        "an app row offers both placements"
    );
    assert!(pump_with_modifiers(
        &mut h,
        &key_pressed(Key::Named(NamedKey::Enter)),
        winit::event::Modifiers::from(winit::keyboard::ModifiersState::SHIFT),
    ));
    assert_eq!(
        runs.borrow().last().map(String::as_str),
        Some(format!("notes@{:?}", OpenTarget::Here(group)).as_str())
    );
    assert_eq!(
        h.chrome.borrow().open_target,
        OpenTarget::Beside,
        "the aim lasts one launch"
    );

    let (mut h, runs) = steps_harness(QUERY);
    let group = h.host.focused_group().expect("a focused group");
    select(&mut h, "Zqfixture notes");
    assert!(pump_with_modifiers(
        &mut h,
        &key_pressed(Key::Named(NamedKey::Enter)),
        winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL),
    ));
    assert_eq!(
        runs.borrow().last().map(String::as_str),
        Some(format!("notes@{:?}", OpenTarget::RightOf(group)).as_str())
    );
}

#[test]
fn tab_reaches_the_next_group_and_the_band_lists_only_kinds_with_hits() {
    let (mut h, _) = steps_harness(QUERY);
    let first_group = h.host.palette.visible_entries()[0].group.clone();
    assert!(press(&mut h, NamedKey::Tab), "Tab belongs to the results");
    let now = h.host.palette.visible_entries()[h.host.palette.cursor()]
        .group
        .clone();
    assert_ne!(now, first_group, "Tab moved to the next group");

    assert!(
        h.host.palette.filter_band_shown(),
        "several kinds: the band"
    );
    let chips: Vec<ResourceKind> = h.host.search_filters.kinds().cloned().collect();
    assert_eq!(
        chips.first(),
        Some(&ResourceKind::File),
        "most hits first: {chips:?}"
    );
    assert!(
        chips
            .iter()
            .all(|kind| h.host.search_results.iter().any(|row| row.kind == *kind)),
        "no chip for a kind with nothing to show: {chips:?}"
    );

    h.host.set_palette_query("");
    h.host
        .refresh_open_palette(&AutoCommit::new(), &automerge::ROOT);
    assert!(
        !h.host.palette.filter_band_shown(),
        "nothing typed: no band"
    );
}

#[test]
fn a_switch_shows_its_new_state_on_its_row_right_after_it_flips() {
    let mut h = build_harness_sized(1280.0, 800.0);
    {
        // The person owns the workspace, as on their own desktop: shell
        // actions run with its authority.
        let state = h.chrome.borrow();
        let mut tree = state.tree_mut();
        let workspace = tree
            .desktop
            .active_workspace_mut()
            .expect("active workspace");
        workspace.set_owner(crate::shell::state::local_user_id_for_room(
            &workspace.room(),
        ));
    }
    h.host.set_palette_visible(true);
    h.host.set_palette_focus(true);
    h.host.force_palette_settled();
    h.host.set_palette_query("do not disturb");
    h.host
        .refresh_search_results(&AutoCommit::new(), &automerge::ROOT);
    settle(&mut h, 1280.0, 800.0);
    let row = h
        .host
        .search_results
        .iter()
        .position(|row| row.id == "notifications.dnd.toggle")
        .expect("the switch is found");
    let id = format!("result:{row}");
    let index = h
        .host
        .palette
        .visible_entries()
        .iter()
        .position(|entry| entry.id == id)
        .expect("listed");
    let before = h.host.palette.visible_entries()[index].badge.clone();
    h.host.palette.set_cursor(index);
    assert!(press(&mut h, NamedKey::Enter));
    assert!(
        h.chrome.borrow().dnd(),
        "the switch flipped: row {:?}, notices {:?}",
        h.host.palette.visible_entries().get(index).map(|e| (
            e.label.clone(),
            e.badge.clone(),
            e.description.clone()
        )),
        h.chrome
            .borrow()
            .notifications()
            .iter()
            .map(|n| n.body.clone())
            .collect::<Vec<_>>()
    );
    assert!(h.host.palette_visible(), "a switch keeps search open");
    let selected = h.host.palette.selected_id().expect("still selected");
    let entry = h
        .host
        .palette
        .visible_entries()
        .into_iter()
        .find(|entry| entry.id == selected)
        .expect("the row");
    assert_eq!(
        h.host.search_results[selected
            .strip_prefix("result:")
            .and_then(|i| i.parse::<usize>().ok())
            .expect("a result row")]
        .id,
        "notifications.dnd.toggle",
        "the switch stays selected"
    );
    assert_ne!(entry.badge, before, "its pill shows the new state at once");
}

#[test]
fn rows_wear_their_own_pictures() {
    let (h, _) = steps_harness(QUERY);
    let notes = h
        .host
        .palette
        .visible_entries()
        .into_iter()
        .find(|entry| entry.label == "Zqfixture notes")
        .expect("the app row")
        .clone();
    assert_ne!(
        notes.icon,
        Some(ui::icons::Icon::LayoutGrid),
        "a Tilo app is not a generic grid"
    );
    assert_eq!(
        notes.icon,
        Some(ui::icons::app_glyph("so.tilo.notes", None))
    );
}
