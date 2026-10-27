//! Keyboard focus on the desktop surface, inside the frame pipeline the
//! browser desktop runs: the window the person is in owns the keyboard. Tab
//! cycles through that app's controls and never reaches the desktop's
//! chrome, a window brought forward (opened, clicked, switched to) takes
//! focus, a click anywhere in a window, partly off screen too, aims typing
//! there, and an app's own search button opens search over its window.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed lookup is the assertion"
)]
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use tilo_desktop_model::{IdMinter, Tab, TileContent, WindowWidth, Workspace};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource, seed_tree};
use ui::application::{
    AppComponent, ApplicationDefinition, HostContext, HostContextInputs, LaunchTarget, RestoreInput,
};
use ui::automerge::{ROOT, transaction::Transactable};
use ui::kurbo::{Point, Rect};
use ui::owner_wake::OwnerWakeQueue;
use ui::taffy::{NodeId, Style, TaffyTree};
use ui::widget::{
    AppKind, AppWidget, EventCtx, IconDescriptor, TaffyTreeExt as _, TileHost, Widget,
};
use ui::winit::event::{ElementState, MouseButton, WindowEvent};

const APP: &str = "so.tilo.keyboard-test";

/// What each field was typed, by its test id.
type Typed = Rc<RefCell<HashMap<String, String>>>;

/// A focusable control that keeps what is typed while it has focus.
struct Field {
    id: String,
    typed: Typed,
    focused: bool,
    pointer: Point,
    rect: Rect,
}

impl Widget for Field {
    fn name(&self) -> &'static str {
        "KeyboardField"
    }
    fn test_id(&self) -> Option<&str> {
        Some(&self.id)
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let mut node = ui::a11y::AccessNode::new(ui::a11y::Role::TextInput)
            .with_action_target(self.id.clone());
        node.name = Some(self.id.clone());
        node.state.focused = self.focused;
        Some(node)
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &ui::Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: ui::taffy::Size {
                width: ui::taffy::Dimension::percent(1.0),
                height: ui::taffy::Dimension::length(40.0),
            },
            ..Style::default()
        };
        taffy.widget_leaf(style, nodes, self)
    }
    ui::rect_geometry!();
    /// A value set through assistive technology replaces what was typed.
    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        _: &[u64],
        _: &mut ui::widget::Edits<'_>,
        _: &ui::widget::ModelPath,
    ) -> bool {
        let ui::a11y::Action::SetValue(value) = action else {
            return false;
        };
        self.typed
            .borrow_mut()
            .insert(self.id.clone(), value.clone());
        true
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => self.pointer = ctx.logical(position),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.rect.contains(self.pointer) => {
                *ctx.focus_target = Some(ctx.node_id);
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput { event: key, .. }
                if self.focused && key.state == ElementState::Pressed =>
            {
                if let Some(text) = &key.text {
                    self.typed
                        .borrow_mut()
                        .entry(self.id.clone())
                        .or_default()
                        .push_str(text);
                    *ctx.handled = true;
                }
            }
            WindowEvent::Ime(ui::winit::event::Ime::Commit(text)) if self.focused => {
                self.typed
                    .borrow_mut()
                    .entry(self.id.clone())
                    .or_default()
                    .push_str(text);
                *ctx.handled = true;
            }
            _ => {}
        }
    }
}

/// The app: its search button above two fields, and an action button it
/// adds once it has seen its document, as apps build header actions for
/// what they show.
struct Root {
    children: Vec<Box<dyn Widget>>,
    pressed: Rc<std::cell::Cell<usize>>,
    tab: u64,
    /// Takes every key and composed text, focused or not, the way an app
    /// that forgets to check its focus would.
    greedy: Option<Typed>,
    built_late: bool,
    dirty_late: bool,
    rect: Rect,
}

impl Widget for Root {
    ui::provides!(
        dyn ui::search::capability::SearchSource,
        dyn ui::resources::PublishesResources
    );

    fn name(&self) -> &'static str {
        "KeyboardApp"
    }
    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        if !self.built_late {
            self.built_late = true;
            self.dirty_late = true;
            let pressed = Rc::clone(&self.pressed);
            self.children.push(Box::new(
                ui::widget::Button::new(&format!("late-{}", self.tab), "Add")
                    .on_effect(move || pressed.set(pressed.get().saturating_add(1))),
            ));
        }
        for child in &mut self.children {
            child.sync(state, path);
        }
        // A panel toggle in its window's bar, as apps moved to the bar's
        // slots declare their actions.
        ui::window_toolbar::declare(Some(ui::window_toolbar::WindowToolbar {
            actions: vec![ui::window_toolbar::ToolbarAction::new(
                "panel",
                "pin",
                "Toggle panel",
            )],
            ..ui::window_toolbar::WindowToolbar::default()
        }));
        let _ = ui::window_toolbar::take_events();
    }
    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = std::mem::take(&mut self.dirty_late);
        for child in &mut self.children {
            dirty |= child.take_structure_dirty();
        }
        dirty
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &ui::Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let children: Vec<NodeId> = self
            .children
            .iter_mut()
            .map(|child| child.build_layout(taffy, theme, nodes))
            .collect();
        let style = Style {
            display: ui::taffy::Display::Flex,
            flex_direction: ui::taffy::FlexDirection::Column,
            size: ui::taffy::Size::from_percent(1.0, 1.0),
            ..Style::default()
        };
        taffy.widget_node(style, &children, nodes, self)
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = self.rect.origin();
        let nodes = taffy.children(node).unwrap_or_default();
        for (child, node) in self.children.iter_mut().zip(nodes) {
            child.apply_layout(taffy, node, inner);
        }
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if let Some(typed) = &self.greedy {
            let text = match event {
                WindowEvent::KeyboardInput { event: key, .. } => key.text.as_deref(),
                WindowEvent::Ime(ui::winit::event::Ime::Commit(text)) => Some(text.as_str()),
                _ => None,
            };
            if let Some(text) = text {
                typed
                    .borrow_mut()
                    .entry(format!("greedy-{}", self.tab))
                    .or_default()
                    .push_str(text);
                *ctx.handled = true;
                return;
            }
        }
        let nodes = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        for (child, node) in self.children.iter_mut().zip(nodes) {
            child.handle_event(event, &mut ctx.reborrow(ctx.path, node));
        }
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.children.iter().map(AsRef::as_ref).collect()
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.children
            .iter_mut()
            .map(|child| child.as_mut() as &mut dyn Widget)
            .collect()
    }
}

impl ui::search::capability::SearchSource for Root {
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(Service { tab: self.tab })]
    }
}

/// The resource the app in tab `tab` publishes its `open` on.
fn videos(tab: u64) -> String {
    format!("keyboard/videos/{tab}")
}
/// The operation type of the app's `open`.
const OPEN_TYPE: &str = "keyboard.open";

/// The app's `open`, as a framework app publishes it: show the item at an
/// anchor, as picking one of its search hits does.
impl ui::resources::PublishesResources for Root {
    fn resources(&self, _: &ui::automerge::AutoCommit) -> Vec<tilo_actions::Resource> {
        let open = tilo_actions::ActionSpec::typed(
            "open",
            "Open",
            "Item to open",
            tilo_actions::ExecutionLocus::ClientLocal,
            vec![tilo_actions::ActionParam::required(
                "anchor",
                tilo_actions::ParamType::Text,
                "Item to open: the anchor of a search hit",
            )],
            |args| {
                tilo_actions::EncodedAction::encode(
                    OPEN_TYPE,
                    &Open {
                        anchor: args.text("anchor").unwrap_or_default().to_owned(),
                    },
                )
                .unwrap()
            },
        );
        let label = SEARCH
            .with_borrow(|search| search.labels.get(&self.tab).cloned())
            .unwrap_or_else(|| "Videos".to_owned());
        vec![
            tilo_actions::Resource::new(videos(self.tab), "keyboard.videos", label)
                .with_action(open),
        ]
    }
}

/// What the app's `open` shows: it notes the anchor it was opened at.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Open {
    anchor: String,
}

impl tilo_actions::Action for Open {
    fn apply(&self, doc: &mut ui::automerge::AutoCommit) -> Result<(), tilo_actions::ActionError> {
        SEARCH.with_borrow_mut(|search| search.opened.push(self.anchor.clone()));
        doc.put(ROOT, "opened", self.anchor.as_str())?;
        Ok(())
    }
}

/// The app's search: like a framework app's, it takes a query in on a later
/// frame and only then answers with a row for it ("Search `YouTube` for …"),
/// a hit naming no action that opens through the app's `open`.
struct Service {
    /// The tab of the window whose app this is.
    tab: u64,
}

impl ui::search::SearchProvider for Service {
    fn name(&self) -> &'static str {
        "Videos"
    }
    fn search(&self, _: &str) -> Vec<ui::search::SearchResult> {
        Vec::new()
    }
    fn open_action(&self) -> Option<tilo_actions::ActionAddress> {
        Some(tilo_actions::ActionAddress::new(videos(self.tab), "open"))
    }
    fn search_page(
        &self,
        query: &ui::search::SearchQuery,
        _: Option<&ui::search::SearchCursor>,
        _: usize,
        _: usize,
    ) -> ui::search::SearchPage {
        SEARCH.with_borrow_mut(|search| search.asked.push(query.text().to_owned()));
        let mut page = ui::search::SearchPage::empty();
        if SEARCH.with_borrow(|search| search.answered) {
            page.results.push(ui::search::SearchResult::new(
                "Videos",
                format!("Search the service for “{}”", query.text()),
                format!("videos.search:{}", query.text()),
            ));
        } else {
            page.pending = Some(ui::search::PendingToken::new(1));
        }
        page
    }
    fn poll_page(&self, _: &ui::search::PendingToken) -> ui::search::PagePoll {
        if SEARCH.with_borrow(|search| search.answered) {
            ui::search::PagePoll::Answered(ui::search::SearchPage::empty())
        } else {
            ui::search::PagePoll::Pending
        }
    }
}

#[derive(Default)]
struct SearchLog {
    asked: Vec<String>,
    /// The app has taken the query in.
    answered: bool,
    /// The anchors the app's `open` ran at.
    opened: Vec<String>,
    /// What the app in a tab names the resource it publishes, where not
    /// "Videos".
    labels: HashMap<u64, String>,
}

impl AppWidget for Root {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Keyboard"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("K")
    }
}

thread_local! {
    /// The test ids the next built app gives its fields, and where they record.
    static NEXT: RefCell<Option<(u64, Typed)>> = const { RefCell::new(None) };
    /// How often each app's late-built button was pressed, by tab.
    static PRESSED: RefCell<HashMap<u64, Rc<std::cell::Cell<usize>>>> = RefCell::new(HashMap::new());
    /// Whether the next built app takes typing that is not its own.
    static GREEDY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// What the apps' search was asked, and whether it has answered.
    static SEARCH: RefCell<SearchLog> = RefCell::default();
}

fn factory(
    mut context: HostContext,
    _: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, ui::launch::LaunchError> {
    context
        .initialize_new_record(|doc| {
            doc.put(ROOT, "body", "").map_err(|e| e.to_string())?;
            doc.commit();
            Ok(())
        })
        .map_err(ui::launch::LaunchError::Record)?;
    let (tab, typed) = NEXT.with_borrow_mut(|next| next.take().unwrap());
    let field = |n: u8| -> Box<dyn Widget> {
        Box::new(Field {
            id: format!("field-{tab}-{n}"),
            typed: Rc::clone(&typed),
            focused: false,
            pointer: Point::ZERO,
            rect: Rect::ZERO,
        })
    };
    let search = ui::search::search_button(ui::search::SearchRequest::this_app());
    let pressed = Rc::new(std::cell::Cell::new(0));
    PRESSED.with_borrow_mut(|all| all.insert(tab, Rc::clone(&pressed)));
    let greedy = GREEDY
        .with(|greedy| greedy.replace(false))
        .then(|| Rc::clone(&typed));
    let root = Root {
        children: vec![Box::new(search), field(1), field(2)],
        pressed,
        tab,
        greedy,
        built_late: false,
        dirty_late: false,
        rect: Rect::ZERO,
    };
    let mut registry = tilo_actions::ActionRegistry::new();
    registry.register::<Open>(OPEN_TYPE);
    Ok(AppComponent::new(context, Box::new(root), registry))
}

struct Source {
    owners: ui::record::RecordOwners,
    root: std::path::PathBuf,
    queue: OwnerWakeQueue,
    typed: Typed,
}

impl TileSource for Source {
    fn tile(&mut self, workspace: &Workspace, tab: &Tab) -> TileHost {
        let room = workspace.room().to_string();
        NEXT.with_borrow_mut(|slot| *slot = Some((tab.id, Rc::clone(&self.typed))));
        let definition = ApplicationDefinition {
            app_id: APP.into(),
            title: "Keyboard".into(),
            icon: IconDescriptor::Glyph("K"),
            record_kind: ui::record::RecordKind::new("keyboard-test"),
            room_record: false,
            required: vec![],
            optional: vec![],
            opens: &[],
            create: factory,
        };
        let records = self.owners.open(APP, &room).unwrap();
        let views = Rc::new(
            ui::application_view::InstanceViewStore::new(self.root.clone(), "device", &room)
                .unwrap(),
        );
        let permissions = Rc::new(RefCell::new(ui::permissions::PermissionContext::member(
            "keyboard-author",
            ui::permissions::Role::Owner,
        )));
        let context = HostContext::prepare(
            &definition,
            HostContextInputs {
                records,
                views,
                device_id: "device".into(),
                instance_id: tab.id,
                permissions,
                author: ui::permissions::UserId("keyboard-author".into()),
                action_context: tilo_actions::ActionContext::default().with_app(APP, "Keyboard"),
            },
            &LaunchTarget::New,
        )
        .unwrap();
        let mut component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .unwrap();
        component
            .mount_with_owner_queue(&self.queue, |_| {})
            .unwrap();
        TileHost::from_component(component)
    }

    fn launch(&mut self, _: &Workspace, id: u64, app: &str) -> Option<Tab> {
        (app == APP).then(|| keyboard_tab(id))
    }
}

fn keyboard_tab(id: u64) -> Tab {
    Tab {
        id,
        title: "Keyboard".into(),
        tile: TileContent::LocalApp {
            app_id: APP.into(),
            label: "Keyboard".into(),
            note: String::new(),
        },
        app_title: None,
    }
}

struct Desk {
    driver: ui::headless::FrameHarness,
    handle: DesktopHandle,
    typed: Typed,
    tabs: Vec<u64>,
    root: std::path::PathBuf,
}

impl Desk {
    /// `windows` app windows, each `permille` of the strip wide, in a
    /// viewport 1200 px wide: wide windows run off its right edge.
    fn new(windows: usize, permille: u32) -> Self {
        ui::i18n::I18nManager::init("en-US");
        let root = std::env::temp_dir().join(format!("tilo-keyboard-{}", uuid::Uuid::new_v4()));
        let mut minter = IdMinter::new(29);
        let tabs: Vec<Tab> = (0..windows).map(|_| keyboard_tab(minter.mint())).collect();
        let ids = tabs.iter().map(|tab| tab.id).collect();
        let mut tree = seed_tree(&mut minter, "Desk", tabs);
        for group in &mut tree.workspaces[0].strips[0].window_groups {
            group.width = WindowWidth::Fraction(permille);
        }
        let queue = OwnerWakeQueue::default();
        let typed = Typed::default();
        let source = Source {
            owners: ui::record::RecordOwners::new(root.clone()),
            root: root.clone(),
            queue: queue.clone(),
            typed: Rc::clone(&typed),
        };
        let (surface, handle) = DesktopSurface::new(tree, 29, Box::new(source));
        handle.set_viewport_width(1200.0);
        let mut driver = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::state::State::new(),
            Box::new(surface),
            (1200, 800),
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("keyboard-author".into()),
        );
        driver.set_caller_palette_enabled(false);
        driver.attach_owner_wake_queue(queue).unwrap();
        let mut desk = Self {
            driver,
            handle,
            typed,
            tabs: ids,
            root,
        };
        desk.settle();
        desk
    }

    fn settle(&mut self) {
        for _ in 0..4 {
            self.driver.relayout();
            self.driver.service_pending_owner_models().unwrap();
        }
    }

    /// The retained path of the widget holding keyboard focus.
    fn focus_path(&self) -> Option<String> {
        self.driver.inspection().keyboard_focus_owner
    }

    /// The accessible name of the control holding keyboard focus: a
    /// field's test id.
    fn focused_name(&self) -> Option<String> {
        self.driver.focused_access_node()?.name
    }

    fn typed(&self, field: &str) -> String {
        self.typed.borrow().get(field).cloned().unwrap_or_default()
    }

    fn rect(&self, name: &str) -> Rect {
        self.driver.targets()[name]
    }
}

impl Drop for Desk {
    fn drop(&mut self) {
        self.driver.close_components();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn tab_cycles_through_the_focused_app_and_never_reaches_the_desktop_chrome() {
    let mut desk = Desk::new(2, 450);
    let first = desk.tabs[0];
    // The first window is focused: its app's search button, two fields
    // and late-built button are where Tab goes, in turn.
    let app = "DesktopSurface[0]/GroupRow[0]/WindowGroup[0]/TileHost[0]/";
    let mut visited = std::collections::HashSet::new();
    for _ in 0..8 {
        desk.driver.key("Tab");
        let focused = desk.focus_path();
        assert!(
            focused.as_ref().is_some_and(|path| path.starts_with(app)),
            "Tab stays in the focused window's app, not its bar or the next window: {focused:?}"
        );
        visited.extend(focused);
    }
    assert_eq!(
        visited.len(),
        4,
        "Tab reaches each of the app's controls: {visited:?}"
    );
    // And typing goes where Tab put focus.
    let field = format!("field-{first}-2");
    for _ in 0..4 {
        if desk.focused_name().as_deref() == Some(field.as_str()) {
            break;
        }
        desk.driver.key("Tab");
    }
    desk.driver.type_text("ok");
    assert_eq!(desk.typed(&field), "ok");
}

#[test]
fn the_window_shown_first_and_a_window_just_opened_take_the_keyboard() {
    let mut desk = Desk::new(1, 450);
    let first = desk.tabs[0];
    desk.driver.type_text("a");
    assert_eq!(
        desk.typed(&format!("field-{first}-1")),
        "a",
        "the desktop's focused window has the keyboard from the start"
    );

    let command = serde_json::from_value(serde_json::json!({
        "kind": "launch", "generation": "g", "app": APP
    }))
    .unwrap();
    desk.handle.dispatch(&command, "g").unwrap();
    desk.settle();
    let opened = desk
        .handle
        .shown_tabs()
        .into_iter()
        .map(|(tab, _)| tab)
        .find(|tab| *tab != first)
        .expect("the launched window is shown");
    desk.driver.type_text("b");
    assert_eq!(
        desk.typed(&format!("field-{opened}-1")),
        "b",
        "what is typed right after opening a window reaches it"
    );
    assert_eq!(desk.typed(&format!("field-{first}-1")), "a");
}

#[test]
fn a_click_in_a_window_partly_off_screen_aims_typing_at_it() {
    let mut desk = Desk::new(2, 700);
    let second = desk.tabs[1];
    let field = format!("field-{second}-2");
    let rect = desk.rect(&field);
    assert!(
        rect.x0 < 1200.0 && rect.x1 > 1200.0,
        "the second window runs off the right edge: {rect:?}"
    );
    desk.driver
        .click_at(f64::midpoint(rect.x0, 1200.0), rect.center().y);
    desk.settle();
    desk.driver.type_text("hi");
    assert_eq!(
        desk.typed(&field),
        "hi",
        "typing reaches the clicked control"
    );
    let first = desk.tabs[0];
    assert_eq!(desk.typed(&format!("field-{first}-1")), "");
}

#[test]
fn an_apps_own_search_button_opens_search_over_its_window() {
    let mut desk = Desk::new(2, 450);
    assert!(!desk.handle.search_open());
    desk.driver.click_named("search-open").unwrap();
    desk.settle();
    assert!(
        desk.handle.search_open(),
        "the app's search button opens the desktop's search"
    );
}

#[test]
fn search_an_app_opened_asks_it_and_follows_its_answer_once_it_is_in() {
    let mut desk = Desk::new(1, 450);
    desk.driver.click_named("search-open").unwrap();
    desk.settle();
    assert!(desk.handle.search_open());
    desk.driver.type_text("lofi");
    desk.settle();
    let asked = SEARCH.with_borrow(|search| search.asked.clone());
    assert_eq!(
        asked.last().map(String::as_str),
        Some("lofi"),
        "the app is asked what was typed: {asked:?}"
    );
    // The app answers on a later frame: search asks again then, so what it
    // offers for the query (a row searching its service) shows.
    let before = asked.len();
    SEARCH.with_borrow_mut(|search| search.answered = true);
    desk.settle();
    let asked = SEARCH.with_borrow(|search| search.asked.clone());
    assert!(
        asked.len() > before && asked.last().map(String::as_str) == Some("lofi"),
        "search asks the app again once its answer is in: {asked:?}"
    );
}

/// What an app's search offers for the query ("Search `YouTube` for …") is
/// a row of the search over its window, and choosing it runs inside the
/// app, at the hit's anchor, through the app's `open`.
#[test]
fn a_hit_chosen_in_an_apps_search_opens_inside_that_app() {
    SEARCH.with_borrow_mut(|search| search.answered = true);
    let mut desk = Desk::new(1, 450);
    desk.driver.click_named("search-open").unwrap();
    desk.settle();
    desk.driver.type_text("lofi");
    desk.settle();
    desk.driver.key("Enter");
    desk.settle();
    assert_eq!(
        SEARCH.with_borrow(|search| search.opened.clone()),
        ["videos.search:lofi"],
        "the app opens at the hit chosen"
    );
    assert!(!desk.handle.search_open());
    assert_eq!(desk.driver.refusal_shown(), None);
    assert!(
        desk.driver.control_failures().is_empty(),
        "{:?}",
        desk.driver.control_failures()
    );
}

/// An app's action chosen in search over everything runs inside that app,
/// under its custody, not at the desktop's door, and its window comes
/// forward to show it.
#[test]
fn an_apps_action_chosen_in_search_over_everything_runs_in_its_window() {
    let mut desk = Desk::new(2, 450);
    let second = desk.tabs[1];
    SEARCH.with_borrow_mut(|search| search.labels.insert(second, "Archive".into()));
    desk.handle.open_search(None);
    desk.settle();
    assert!(desk.handle.search_open());
    desk.driver.type_text("open archive");
    desk.settle();
    desk.driver.key("Enter");
    desk.settle();
    // `open` asks for the item; it runs once that is given.
    desk.driver.type_text("clip");
    desk.driver.key("Enter");
    desk.settle();
    assert_eq!(
        SEARCH.with_borrow(|search| search.opened.clone()),
        ["clip"],
        "the action runs in the app that offered it"
    );
    assert_eq!(desk.driver.refusal_shown(), None);
    assert!(
        desk.driver.control_failures().is_empty(),
        "{:?}",
        desk.driver.control_failures()
    );
    assert!(!desk.handle.search_open());
    desk.driver.type_text("z");
    assert_eq!(
        desk.typed(&format!("field-{second}-1")),
        "z",
        "the app's window came forward and has the keyboard"
    );
}

#[test]
fn a_button_an_app_builds_after_it_opened_answers_its_click() {
    let mut desk = Desk::new(2, 450);
    for tab in desk.tabs.clone() {
        let button = format!("late-{tab}");
        desk.driver.click_named(&button).unwrap();
        desk.settle();
        let pressed = PRESSED.with_borrow(|all| all[&tab].get());
        assert_eq!(pressed, 1, "{button} ran its effect once");
        assert!(
            desk.driver.control_failures().is_empty(),
            "{:?}",
            desk.driver.control_failures()
        );
    }
}

#[test]
fn typing_reaches_the_focused_window_even_when_a_window_before_it_claims_keys() {
    GREEDY.with(|greedy| greedy.set(true));
    let mut desk = Desk::new(2, 450);
    let (first, second) = (desk.tabs[0], desk.tabs[1]);
    let field = format!("field-{second}-1");
    let rect = desk.rect(&field);
    desk.driver.click_at(rect.center().x, rect.center().y);
    desk.settle();
    desk.driver.type_text("ab");
    desk.driver.commit_text("cd");
    assert_eq!(
        desk.typed(&field),
        "abcd",
        "keys and composed text reach the focused window's field"
    );
    assert_eq!(
        desk.typed(&format!("greedy-{first}")),
        "",
        "the window beside it is not offered what is typed"
    );
}

#[test]
fn one_click_in_an_inactive_window_gives_the_control_under_it_the_keyboard() {
    for coalesce in [false, true] {
        let mut desk = Desk::new(2, 450);
        desk.driver.set_coalesce_input_frames(coalesce);
        let second = desk.tabs[1];
        let field = format!("field-{second}-2");
        let rect = desk.rect(&field);
        desk.driver
            .send_host_event(&ui::synthetic_input::cursor_moved(
                rect.center().x,
                rect.center().y,
            ));
        desk.driver
            .send_host_event(&ui::synthetic_input::mouse_input(
                ElementState::Pressed,
                MouseButton::Left,
            ));
        desk.driver
            .send_host_event(&WindowEvent::Ime(ui::winit::event::Ime::Disabled));
        desk.driver.send_host_event(&WindowEvent::Focused(true));
        desk.driver
            .send_host_event(&ui::synthetic_input::mouse_input(
                ElementState::Released,
                MouseButton::Left,
            ));
        for _ in 0..4 {
            let _ = desk.driver.poll_frame();
        }
        desk.driver.type_text("hi");
        assert_eq!(
            desk.typed(&field),
            "hi",
            "coalesce {coalesce}: {:?}",
            desk.focus_path()
        );
    }
}

/// The retained path of the widget whose semantics are named `name`.
fn semantic_path(node: &ui::headless::InspectionNode, name: &str) -> Option<String> {
    if node
        .semantics
        .as_ref()
        .is_some_and(|semantics| semantics.name.as_deref() == Some(name))
    {
        return Some(node.path.clone());
    }
    node.children
        .iter()
        .find_map(|child| semantic_path(child, name))
}

/// The page's accessibility tree names a field by its node's own target,
/// and a screen reader, voice input, autofill or an automation's `fill`
/// sets its value there.
#[test]
fn a_value_set_on_a_fields_semantics_reaches_the_app() {
    let mut desk = Desk::new(2, 450);
    let second = desk.tabs[1];
    let field = format!("field-{second}-1");
    let path = semantic_path(&desk.driver.inspection().root, &field).unwrap();
    desk.driver
        .perform_semantic_named(&path, Some(&field), &ui::a11y::Action::Focus)
        .unwrap();
    desk.settle();
    desk.driver
        .perform_semantic_named(
            &path,
            Some(&field),
            &ui::a11y::Action::SetValue("Ada".into()),
        )
        .unwrap();
    desk.settle();
    assert_eq!(desk.typed(&field), "Ada");
}

/// A press on one of the app's own slots in its window's bar is a press on
/// the app: the keyboard the click gave the bar goes back to the control
/// that had it, so Escape (which closes the panel the slot opened) and what
/// is typed next reach the app, not the bar.
#[test]
fn a_press_on_an_apps_slot_in_its_bar_leaves_the_keyboard_with_the_app() {
    let mut desk = Desk::new(1, 450);
    let tab = desk.tabs[0];
    let field = format!("field-{tab}-1");
    let rect = desk.rect(&field);
    desk.driver.click_at(rect.center().x, rect.center().y);
    desk.settle();
    desk.driver.type_text("a");
    assert_eq!(desk.typed(&field), "a", "the clicked field takes typing");

    let slot = desk
        .driver
        .a11y_nodes()
        .into_iter()
        .find_map(|(_, node)| {
            (node.label() == Some("Toggle panel"))
                .then(|| node.bounds())
                .flatten()
        })
        .expect("the app's slot stands in its window's bar");
    desk.driver.click_at(
        f64::midpoint(slot.x0, slot.x1),
        f64::midpoint(slot.y0, slot.y1),
    );
    desk.settle();
    assert_eq!(
        desk.focused_name().as_deref(),
        Some(field.as_str()),
        "the field keeps the keyboard: {:?}",
        desk.focus_path()
    );
    desk.driver.type_text("b");
    assert_eq!(
        desk.typed(&field),
        "ab",
        "typing after the press reaches the app"
    );
}
