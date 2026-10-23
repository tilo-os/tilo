//! The window's search in place: Find opens it on the active tab's app, what
//! is typed reaches the app's document, and closing gives it back untouched.

use super::*;
use crate::synthetic_input::{char_key, named_key};
use crate::testing::Dispatch;
use crate::widget::find_bar::{FindMatch, FindOptions, Findable};
use crate::widget::{InPlace, SearchHandoff, SearchReach};
use winit::keyboard::{ModifiersState, NamedKey};

/// A document app: finds in its text, keeps which match is current and
/// whether it was told to stop.
#[derive(Default)]
struct Draft {
    text: String,
    found: usize,
    current: Option<usize>,
    cleared: bool,
    rect: Rect,
}

impl Findable for Draft {
    fn find(&mut self, query: &str, _options: FindOptions) -> Vec<FindMatch> {
        self.cleared = false;
        if query.is_empty() {
            self.found = 0;
            return Vec::new();
        }
        let found: Vec<FindMatch> = self
            .text
            .to_lowercase()
            .match_indices(&query.to_lowercase())
            .map(|(start, m)| FindMatch::new(start, start + m.len()))
            .collect();
        self.found = found.len();
        found
    }
    fn reveal(&mut self, index: usize) {
        self.current = Some(index);
    }
    fn clear_find(&mut self) {
        self.cleared = true;
        self.found = 0;
        self.current = None;
    }
}

impl Widget for Draft {
    crate::provides!(dyn Findable);
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), node_map, self)
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn name(&self) -> &'static str {
        "Draft"
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

impl crate::widget::app_widget::AppWidget for Draft {
    fn uses_host_record(&self) -> bool {
        false
    }
    fn app_kind(&self) -> crate::widget::AppKind {
        crate::widget::AppKind::Files
    }
    fn app_title(&self) -> &'static str {
        "Notes"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("N")
    }
}

fn draft(group: &mut WindowGroup) -> &mut Draft {
    let Some(TileContent::App(app)) = group.active_host_mut().map(TileHost::content_mut) else {
        panic!("the window shows the draft");
    };
    (app.as_mut() as &mut dyn Widget)
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<Draft>())
        .expect("the draft")
}

fn searchable_window() -> WindowGroup {
    let mut group = WindowGroup::new(
        10,
        vec![(
            TabDescriptor::new(1, "Draft"),
            TileContent::App(Box::new(Draft {
                text: "Cat cat concat cat".into(),
                ..Draft::default()
            })),
        )],
        1,
        WindowWidth::Normal,
    );
    group.focused = true;
    group
}

#[test]
fn find_opens_the_search_in_the_window_bar_and_highlights_the_document_with_n_of_m() {
    let mut group = searchable_window();
    let mut find = Dispatch::new().modifiers(ModifiersState::CONTROL);
    assert!(
        find.send(&mut group, &char_key("f")).handled,
        "Find is the window's"
    );
    assert!(group.window_search().is_open());
    assert!(
        group.window_search().has_keyboard(),
        "typing goes to the field"
    );
    assert_eq!(group.window_search().in_place(), InPlace::Document);

    let mut typing = Dispatch::new();
    for key in ["c", "a", "t"] {
        assert!(
            typing.send(&mut group, &char_key(key)).handled,
            "not the app's"
        );
    }
    assert_eq!(group.window_search().query(), "cat");
    assert_eq!(group.window_search().position(), Some((1, 4)), "1 of 4");
    assert_eq!(
        draft(&mut group).found,
        4,
        "the document highlights every match"
    );

    typing.send(&mut group, &named_key(NamedKey::Enter));
    assert_eq!(
        group.window_search().position(),
        Some((2, 4)),
        "Enter steps on"
    );
    assert_eq!(draft(&mut group).current, Some(1));

    typing.send(&mut group, &named_key(NamedKey::Escape));
    assert!(!group.window_search().is_open());
    assert!(draft(&mut group).cleared, "closing stops the highlights");
}

#[test]
fn find_again_widens_the_search_to_the_whole_app_with_the_query() {
    let mut group = searchable_window();
    let mut find = Dispatch::new().modifiers(ModifiersState::CONTROL);
    find.send(&mut group, &char_key("f"));
    let mut typing = Dispatch::new();
    for key in ["c", "o", "n"] {
        typing.send(&mut group, &char_key(key));
    }
    find.send(&mut group, &char_key("f"));
    assert_eq!(
        group.take_pending_search_open(),
        Some(SearchHandoff {
            tab: 1,
            reach: SearchReach::App,
            query: "con".into(),
        }),
        "the platform search takes the query on, narrowed to the app"
    );
    assert!(
        !group.window_search().is_open(),
        "the window's search hands over"
    );
    assert!(draft(&mut group).cleared);
}

#[test]
fn a_window_with_nothing_to_search_in_place_opens_the_platform_search_for_its_app() {
    let mut group = WindowGroup::new(
        10,
        vec![(
            TabDescriptor::new(1, "Terminal"),
            TileContent::Stub {
                kind: crate::widget::AppKind::Terminal,
                label: "Terminal".into(),
            },
        )],
        1,
        WindowWidth::Normal,
    );
    group.open_search(crate::tokens::MotionSource::Pointer);
    assert!(!group.window_search().is_open());
    assert_eq!(
        group
            .take_pending_search_open()
            .map(|handoff| handoff.reach),
        Some(SearchReach::App)
    );
}

fn app_window_in_its_own_process() -> WindowGroup {
    let mut group = WindowGroup::new(
        10,
        vec![(
            TabDescriptor::new(1, "Files"),
            TileContent::Stub {
                kind: crate::widget::AppKind::Files,
                label: "Files".into(),
            },
        )],
        1,
        WindowWidth::Normal,
    );
    group.focused = true;
    group
}

#[test]
fn an_app_in_its_own_process_gets_the_query_and_the_bar_shows_its_count() {
    use crate::window_toolbar::{SearchCount, SearchOffer, ToolbarEvent, WindowOffer};
    let mut group = app_window_in_its_own_process();
    let list = WindowOffer {
        search: SearchOffer::List,
        ..WindowOffer::default()
    };
    assert!(group.set_tab_window_offer(1, Some(list.clone())));
    assert!(
        !group.set_tab_window_offer(1, Some(list.clone())),
        "nothing new"
    );

    group.open_search(crate::tokens::MotionSource::Pointer);
    assert!(group.window_search().is_open() && group.window_search().is_remote());
    assert_eq!(group.window_search().in_place(), InPlace::List);
    assert_eq!(
        group.take_toolbar_events(),
        vec![(
            1,
            ToolbarEvent::Search {
                query: String::new()
            }
        )],
        "opening tells the app"
    );
    let mut typing = Dispatch::new();
    for key in ["r", "e"] {
        typing.send(&mut group, &char_key(key));
    }
    assert_eq!(
        group.take_toolbar_events(),
        vec![
            (1, ToolbarEvent::Search { query: "r".into() }),
            (1, ToolbarEvent::Search { query: "re".into() }),
        ]
    );
    group.set_tab_window_offer(
        1,
        Some(WindowOffer {
            count: Some(SearchCount {
                shown: 12,
                total: 40,
            }),
            ..list
        }),
    );
    assert_eq!(
        group.window_search().count(),
        Some(crate::widget::FilterCount::new(12, 40)),
        "12 of 40"
    );

    // The app's window went away under its search: nothing is sent to it.
    assert!(group.set_tab_window_offer(1, None));
    assert!(!group.window_search().is_open());
    assert!(group.take_toolbar_events().is_empty());
}

#[test]
fn an_app_in_its_own_process_with_nothing_to_search_in_place_opens_the_platform_search() {
    use crate::window_toolbar::WindowOffer;
    let mut group = app_window_in_its_own_process();
    group.set_tab_window_offer(1, Some(WindowOffer::default()));
    group.open_search(crate::tokens::MotionSource::Pointer);
    assert!(!group.window_search().is_open());
    assert_eq!(
        group
            .take_pending_search_open()
            .map(|handoff| handoff.reach),
        Some(SearchReach::App)
    );
    assert!(group.take_toolbar_events().is_empty());
}

/// Lay the window out and draw it, as its host does in a frame.
fn laid_out_window(group: &mut WindowGroup) {
    use taffy::prelude::TaffyMaxContent;
    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = group.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy.parent_node(Style::default(), &[node]);
    taffy
        .compute_layout(root, Size::MAX_CONTENT)
        .expect("the window lays out");
    group.apply_layout(&taffy, node, Point::ZERO);
    // The bar's search lays itself out as it is drawn.
    group.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &theme,
        &PresenceCtx::empty(),
    );
}

/// What the bar's top layer paints, for a host that draws it itself.
fn bar_top_layer_tags(group: &mut WindowGroup) -> usize {
    let mut scene = Scene::new();
    group.draw_bar_popups(
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &Theme::dark(),
        &PresenceCtx::empty(),
    );
    scene.encoding().draw_tags.len()
}

#[test]
fn a_host_that_paints_the_bars_top_layer_itself_paints_the_open_search_options() {
    use crate::synthetic_input::{cursor_moved, mouse_press, mouse_release};
    let mut group = searchable_window();
    group.rect = Rect::new(0.0, 0.0, 900.0, 600.0);
    let mut find = Dispatch::new().modifiers(ModifiersState::CONTROL);
    find.send(&mut group, &char_key("f"));
    let mut typing = Dispatch::new();
    for key in ["c", "a", "t"] {
        typing.send(&mut group, &char_key(key));
    }
    laid_out_window(&mut group);
    let closed = bar_top_layer_tags(&mut group);

    let (_, options) = group
        .window_search()
        .test_rects()
        .into_iter()
        .find(|(name, _)| name == "window-search-options")
        .expect("the options control is in the bar");
    let mut pointer = Dispatch::new();
    pointer.send(
        &mut group,
        &cursor_moved(options.center().x, options.center().y),
    );
    pointer.send(&mut group, &mouse_press());
    pointer.send(&mut group, &mouse_release());
    assert!(group.window_search().options_open());
    laid_out_window(&mut group);
    assert!(
        bar_top_layer_tags(&mut group) > closed,
        "the options panel is painted with the bar's menus"
    );
}
