//! Find and replace in the window's search: the find's options under the
//! field, the replacement, the replace chord, and the same through the
//! remote path and through semantic actions.

use automerge::AutoCommit;
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};

use super::*;
use crate::a11y::{Action, Role};
use crate::synthetic_input::{cursor_moved, mouse_press, mouse_release};
use crate::widget::find_bar::{FindMatch, FindMode};
use crate::window_toolbar::ToolbarEvent;

fn leaf(
    taffy: &mut taffy::TaffyTree<()>,
    node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    widget: &mut (dyn Widget + 'static),
) -> taffy::NodeId {
    use crate::widget::TaffyTreeExt;
    taffy.widget_leaf(taffy::Style::default(), node_map, widget)
}

/// A text that finds under the options it is given and can replace.
#[derive(Default)]
struct Draft {
    text: String,
    replaces: bool,
    regex: bool,
    /// The options the last find ran under.
    found_with: Option<FindOptions>,
    found: Vec<FindMatch>,
    current: Option<usize>,
}

impl Findable for Draft {
    fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
        self.found_with = Some(options);
        self.found = if query.is_empty() {
            Vec::new()
        } else if options.match_case {
            self.text
                .match_indices(query)
                .map(|(start, m)| FindMatch::new(start, start + m.len()))
                .collect()
        } else {
            self.text
                .to_lowercase()
                .match_indices(&query.to_lowercase())
                .map(|(start, m)| FindMatch::new(start, start + m.len()))
                .collect()
        };
        self.found.clone()
    }
    fn reveal(&mut self, index: usize) {
        self.current = Some(index);
    }
    fn supports_replace(&self) -> bool {
        self.replaces
    }
    fn supports_regex(&self) -> bool {
        self.regex
    }
    fn replace(&mut self, index: usize, with: &str) -> bool {
        let Some(found) = self.found.get(index) else {
            return false;
        };
        self.text.replace_range(found.start..found.end, with);
        true
    }
    fn replace_all(&mut self, with: &str) -> usize {
        let count = self.found.len();
        for found in self.found.iter().rev() {
            self.text.replace_range(found.start..found.end, with);
        }
        count
    }
    fn content_revision(&self) -> u64 {
        self.text.len() as u64
    }
}

impl Widget for Draft {
    crate::provides!(dyn Findable);
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        leaf(taffy, node_map, self)
    }
    fn name(&self) -> &'static str {
        "Draft"
    }
}

fn editable(text: &str) -> Draft {
    Draft {
        text: text.into(),
        replaces: true,
        ..Draft::default()
    }
}

/// A search open on `doc` with `query` typed and driven once.
fn searching(doc: &mut Draft, query: &str) -> WindowSearch {
    let mut search = WindowSearch::new();
    search.set_app_name("Writer");
    search.place(Rect::new(100.0, 6.0, 128.0, 34.0), 1100.0);
    search.open(InPlace::Document, MotionSource::Keyboard);
    search.field.set_query(query);
    search.drive(doc);
    search
}

fn act(search: &mut WindowSearch, target: &str, action: &Action) -> bool {
    let mut doc = AutoCommit::new();
    search.perform_semantic_action(
        target,
        action,
        &mut crate::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    )
}

/// The open search laid out at rest, as a frame draws it.
fn laid_out(search: &mut WindowSearch) {
    search.layout(1.0, &mut TextRenderer::new(), &Theme::dark());
}

fn rect_of(search: &WindowSearch, target: &str) -> Rect {
    search
        .test_rects()
        .into_iter()
        .find(|(name, _)| name == target)
        .unwrap_or_else(|| panic!("{target} is shown"))
        .1
}

/// Route `event` to the open search as its window does; whether it took it.
fn route(search: &mut WindowSearch, event: &WindowEvent) -> bool {
    let mut taffy = taffy::TaffyTree::<()>::new();
    let node = taffy
        .new_leaf(taffy::Style::default())
        .expect("a scratch node");
    let mut doc = AutoCommit::new();
    let modifiers = winit::event::Modifiers::default();
    let mut handled = false;
    let mut focus = None;
    let mut ctx = crate::widget::EventCtx {
        scale_factor: 1.0,
        modifiers: &modifiers,
        state: crate::widget::Edits::lend(&mut doc),
        path: &automerge::ROOT,
        handled: &mut handled,
        focus_target: &mut focus,
        node_id: node,
        taffy: &taffy,
    };
    search.route(event, &mut ctx)
}

/// A tap, as a touchscreen's contact arrives: the pointer moves there and
/// presses and lifts. Returns whether the search took the press.
fn tap(search: &mut WindowSearch, at: Point) -> bool {
    route(search, &cursor_moved(at.x, at.y));
    let pressed = route(search, &mouse_press());
    route(search, &mouse_release());
    pressed
}

#[test]
fn the_options_set_how_the_document_is_found_and_carry_over_to_the_next_search() {
    let mut doc = editable("Cat cat concat");
    let mut search = searching(&mut doc, "Cat");
    assert_eq!(search.position(), Some((1, 3)), "any case matches");
    search.choose(Choice::MatchCase);
    assert!(search.drive(&mut doc), "the document finds again");
    assert_eq!(doc.found_with.map(|options| options.match_case), Some(true));
    assert_eq!(search.position(), Some((1, 1)), "only Cat matches");

    search.close(MotionSource::Keyboard);
    search.drive(&mut doc);
    search.open(InPlace::Document, MotionSource::Keyboard);
    search.field.set_query("cat");
    search.drive(&mut doc);
    assert!(
        search.options().match_case,
        "the next search runs under the same options"
    );
    assert_eq!(search.position(), Some((1, 2)));
}

#[test]
fn a_mode_the_document_does_not_offer_runs_as_text() {
    let mut doc = editable("a.c abc");
    let mut search = searching(&mut doc, "a.c");
    search.set_options(FindOptions {
        mode: FindMode::Regex,
        ..FindOptions::default()
    });
    search.drive(&mut doc);
    assert_eq!(
        doc.found_with.map(|options| options.mode),
        Some(FindMode::Text)
    );
    doc.regex = true;
    search.drive(&mut doc);
    assert_eq!(
        search.options().mode,
        FindMode::Regex,
        "a document that offers it runs the regular expression"
    );
}

#[test]
fn replace_changes_the_current_match_and_replace_all_every_one() {
    let mut doc = editable("cat cat cat");
    let mut search = searching(&mut doc, "cat");
    search.step(true);
    search.drive(&mut doc);
    assert_eq!(search.position(), Some((2, 3)));

    search.panel.replace_field.set_query("dog");
    search.request_replace(false);
    assert!(search.drive(&mut doc), "the document changed");
    assert_eq!(doc.text, "cat dog cat", "the current match is replaced");
    assert_eq!(
        search.count(),
        Some(FilterCount::new(2, 2)),
        "the count follows, the place kept"
    );

    search.request_replace(true);
    search.drive(&mut doc);
    assert_eq!(doc.text, "dog dog dog");
    assert_eq!(search.count(), Some(FilterCount::new(0, 0)));
}

#[test]
fn a_document_that_cannot_replace_offers_only_the_find_options() {
    let mut doc = Draft {
        text: "cat".into(),
        ..Draft::default()
    };
    let mut search = searching(&mut doc, "cat");
    search.request_replace(true);
    assert!(search.replace.is_none(), "nothing to replace with");
    search.toggle_options();
    laid_out(&mut search);
    let shown: Vec<String> = search
        .test_rects()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert!(shown.contains(&"window-search-match-case".to_owned()));
    assert!(shown.contains(&"window-search-whole-word".to_owned()));
    assert!(!shown.contains(&"window-search-replacement".to_owned()));
    assert!(!shown.contains(&"window-search-replace-all".to_owned()));
}

#[test]
fn the_replace_chord_opens_the_replacement_once_the_document_says_it_can_replace() {
    let mut doc = editable("cat");
    let mut search = WindowSearch::new();
    search.open_as(
        InPlace::Document,
        MotionSource::Keyboard,
        SearchOpening::Replace,
    );
    assert!(
        !search.replacing(),
        "the document has not yet said it can replace"
    );
    search.drive(&mut doc);
    assert!(search.options_open(), "the options open");
    assert!(search.replacing(), "the replacement holds the keyboard");

    for key in ["d", "o", "g"] {
        route(&mut search, &crate::synthetic_input::char_key(key));
    }
    assert_eq!(search.replacement(), "dog");
    assert_eq!(search.query(), "", "the query is untouched");
    route(
        &mut search,
        &crate::synthetic_input::named_key(NamedKey::Escape),
    );
    assert!(!search.options_open(), "Escape closes the options first");
    assert!(search.is_open() && search.has_keyboard() && !search.replacing());
}

#[test]
fn a_tap_opens_the_options_sets_one_and_a_tap_outside_closes_them() {
    let mut doc = editable("Cat cat");
    let mut search = searching(&mut doc, "cat");
    laid_out(&mut search);
    let options = rect_of(&search, "window-search-options");
    assert!(tap(&mut search, options.center()));
    assert!(search.options_open());
    laid_out(&mut search);
    let match_case = rect_of(&search, "window-search-match-case");
    assert!(
        match_case.height() >= f64::from(Theme::dark().tokens.metrics.panel_row_height),
        "a whole row a finger hits"
    );
    assert!(tap(&mut search, match_case.center()));
    assert!(search.options().match_case);
    assert!(search.options_open(), "the panel stays for the next choice");

    let below = Point::new(options.center().x, 900.0);
    assert!(
        tap(&mut search, below),
        "the tap that closes the panel does not reach the content"
    );
    assert!(!search.options_open());
    assert!(search.is_open(), "the search stays open");
}

#[test]
fn an_app_in_its_own_process_hears_the_options_and_the_replace() {
    let mut search = WindowSearch::new();
    search.open_remote(InPlace::Document, MotionSource::Keyboard);
    search.set_remote_abilities(FindAbilities {
        replace: true,
        ..FindAbilities::default()
    });
    search.field.set_query("cat");
    assert_eq!(
        search.take_remote_events(),
        [ToolbarEvent::Search {
            query: "cat".into()
        }],
        "a search on the defaults sends only the query"
    );
    search.choose(Choice::WholeWord);
    search.panel.replace_field.set_query("dog");
    search.choose(Choice::ReplaceAll);
    assert_eq!(
        search.take_remote_events(),
        [
            ToolbarEvent::SearchOptions {
                options: FindOptions {
                    whole_word: true,
                    ..FindOptions::default()
                },
            },
            ToolbarEvent::SearchReplace {
                with: "dog".into(),
                all: true,
            },
        ]
    );
    assert!(search.take_remote_events().is_empty(), "each goes out once");
}

#[test]
fn agents_and_assistive_technology_set_the_options_and_replace_without_the_panel() {
    let mut doc = editable("cat cat");
    let mut search = searching(&mut doc, "cat");
    laid_out(&mut search);
    let node = search
        .accessibility()
        .expect("the open search is announced");
    let options = node
        .children
        .iter()
        .find(|child| child.action_target.as_deref() == Some("window-search-options"))
        .expect("the options control");
    assert_eq!(options.state.expanded, Some(false));
    let roles: Vec<(Role, Option<&str>)> = options
        .children
        .iter()
        .map(|child| (child.role, child.action_target.as_deref()))
        .collect();
    assert_eq!(
        roles,
        [
            (Role::Checkbox, Some("window-search-match-case")),
            (Role::Checkbox, Some("window-search-whole-word")),
            (Role::TextInput, Some("window-search-replacement")),
            (Role::Button, Some("window-search-replace")),
            (Role::Button, Some("window-search-replace-all")),
        ]
    );

    assert!(act(
        &mut search,
        "window-search-whole-word",
        &Action::ToggleCheck
    ));
    assert!(search.options().whole_word);
    assert!(act(
        &mut search,
        "window-search-replacement",
        &Action::SetValue("dog".into())
    ));
    assert_eq!(search.replacement(), "dog");
    assert!(act(
        &mut search,
        "window-search-replace-all",
        &Action::Activate
    ));
    search.drive(&mut doc);
    assert_eq!(doc.text, "dog dog");
    assert!(!search.options_open(), "the panel never opened");
    assert!(act(&mut search, "window-search-options", &Action::Expand));
    assert!(search.options_open());
}

#[test]
fn the_find_and_replace_chords_follow_the_platform() {
    let key = |text: &str, code: KeyCode| {
        let mut event = crate::synthetic_input::key_event(
            Key::Character(text.into()),
            None,
            ElementState::Pressed,
        );
        event.physical_key = PhysicalKey::Code(code);
        crate::synthetic_input::keyboard_input(event)
    };
    let linux = PlatformConventions::new(crate::platform::Platform::Linux);
    let mac = PlatformConventions::new(crate::platform::Platform::MacOs);
    let ctrl = ModifiersState::CONTROL;
    let f = key("f", KeyCode::KeyF);
    let h = key("h", KeyCode::KeyH);
    assert_eq!(search_chord(&f, ctrl, linux), Some(SearchOpening::Find));
    assert_eq!(search_chord(&h, ctrl, linux), Some(SearchOpening::Replace));
    assert_eq!(
        search_chord(&h, ctrl | ModifiersState::SHIFT, linux),
        None,
        "Ctrl+Shift+H is History"
    );
    assert_eq!(search_chord(&f, ModifiersState::empty(), linux), None);
    let cmd = ModifiersState::SUPER;
    assert_eq!(search_chord(&f, cmd, mac), Some(SearchOpening::Find));
    // Option turns the Mac's F into ƒ: the key names the chord.
    let option_f = key("ƒ", KeyCode::KeyF);
    assert_eq!(
        search_chord(&option_f, cmd | ModifiersState::ALT, mac),
        Some(SearchOpening::Replace)
    );
    assert_eq!(search_chord(&h, cmd, mac), None, "⌘H hides a Mac app");
}
