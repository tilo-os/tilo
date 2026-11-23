//! Keyboard focus on the desktop surface, inside the frame pipeline the browser desktop runs: the
//! window the person is in owns the keyboard. Tab cycles through that window's bar and app and
//! never reaches the other windows or the desktop's chrome, a window brought forward (opened,
//! clicked, switched to) takes focus, a click anywhere in a window, partly off screen too, aims
//! typing there, and an app's own search button opens search over its window. What each search
//! reaches — one app's window, a window group's applications, or the workspace — and where the hit
//! it offers opens are here too.
use std::time::Duration;
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use tilo_desktop::about::{AboutLinks, AboutSheet, AboutVariant};
use tilo_desktop::desktop_host::{DesktopChrome, DesktopRules};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_model::commands::DesktopCommand;
use tilo_desktop_model::{IdMinter, Layout, Tab, TileContent, WindowGroup, WindowWidth, Workspace};
use tilo_desktop_surface::{
    DesktopHandle, DesktopSurface, HostAction, SearchHost, SearchHostFeed, TileSource,
};
use tilo_session_core::desktop::seed_tree;
use ui::application::{
    AppComponent, ApplicationDefinition, LaunchTarget, ProcessContext, ProcessContextInputs,
    RestoreInput,
};
use ui::automerge::{ROOT, transaction::Transactable};
use ui::kurbo::{Point, Rect};
use ui::owner_wake::OwnerWakeQueue;
use ui::search::Scope;
use ui::synthetic_input::{char_key, named_key};
use ui::taffy::{NodeId, Style, TaffyTree};
use ui::widget::{
    AppKind, AppWidget, EventCtx, IconDescriptor, TaffyTreeExt as _, TileHost, Widget,
};
use ui::winit::event::{ElementState, MouseButton, WindowEvent};
use ui::winit::keyboard::NamedKey;

const APP: &str = "so.tilo.keyboard-test";
/// An app the shell has installed and not opened.
const NOTES: &str = "so.tilo.notes";

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

/// The app: its search button above two fields, and an action button it adds once it has seen its
/// document, as apps build header actions for what they show.
struct Root {
    children: Vec<Box<dyn Widget>>,
    pressed: Rc<std::cell::Cell<usize>>,
    tab: u64,
    /// Takes every key and composed text, focused or not, the way an app that forgets to check its
    /// focus would.
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
        // A panel toggle in its window's bar, as apps moved to the bar's slots declare their
        // actions.
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
/// The operation type of the app's `open` .
const OPEN_TYPE: &str = "keyboard.open";

/// The app's `open` , as a framework app publishes it: show the item at an anchor, as picking one
/// of its search hits does.
impl ui::resources::PublishesResources for Root {
    fn resources(&self, _: &ui::automerge::AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
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
            tilo_actions::ResourceSpec::new(videos(self.tab), "keyboard.videos", label)
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
    /// What the app in a tab names the resource it publishes, where not "Videos".
    labels: HashMap<u64, String>,
}

/// The shell's host, recording what each search asked it to cover.
#[derive(Default)]
struct HostLog {
    /// The scope search asked it to cover, in order.
    scopes: RefCell<Vec<Scope>>,
}

/// The host the shell searches through: one source of its own, which only a search of the workspace
/// asks.
struct ShellHost(Rc<HostLog>);

impl SearchHost for ShellHost {
    fn feed(&self, _: &str, scope: &Scope) -> SearchHostFeed {
        self.0.scopes.borrow_mut().push(scope.clone());
        SearchHostFeed {
            sources: vec![tilo_desktop_search::Held::new(Plan)],
            // A workspace app's row the workspace's action host publishes: an event's `Invite`,
            // which has nothing to act on with no event selected.
            published: vec![tilo_desktop_search::providers::PublishedActions::of(
                "so.tilo.calendar",
                "workspace",
                &[unselected_event()],
            )],
            ..SearchHostFeed::default()
        }
    }

    fn performs(&self, _: &tilo_desktop_search::Target) -> bool {
        false
    }
}

/// An event Calendar publishes with its `Invite` action, which cannot run while no event is
/// selected: the row that used to stand ahead of the workspace's own invite.
fn unselected_event() -> tilo_actions::ResourceSpec {
    let mut invite = tilo_actions::ActionSpec::immediate_encoded(
        "invite",
        "Invite",
        tilo_actions::ExecutionLocus::ClientLocal,
        tilo_actions::EncodedAction {
            type_id: "test.invite".into(),
            payload: Vec::new(),
        },
    );
    invite.type_mut().contract.availability = tilo_actions::Availability::Unavailable {
        code: "selection_empty".into(),
        reason: "Select an event first".into(),
    };
    tilo_actions::ResourceSpec::new("calendar.event:1", "calendar.event", "Standup")
        .with_action(invite)
}

/// What the host finds beyond the desktop: a plan another workspace holds.
struct Plan;

impl tilo_desktop_search::Source for Plan {
    fn id(&self) -> &'static str {
        "plan"
    }

    fn name(&self) -> String {
        "Plan".into()
    }

    fn query(&self, needle: &str) -> Vec<tilo_desktop_search::Hit> {
        if !needle.to_lowercase().contains("quarterly") {
            return Vec::new();
        }
        vec![tilo_desktop_search::Hit {
            row: None,
            preview: None,
            faces: Vec::new(),
            outcome: None,
            kind: ui::search::system::ResourceKind::Note,
            provider: self.id().into(),
            id: "quarterly-plan".into(),
            title: "Quarterly plan".into(),
            subtitle: Some("in Planning".into()),
            facts: Vec::new(),
            score: ui::search::system::MAX_PROVIDER_SCORE,
            icon: None,
            target: tilo_desktop_search::Target::Window { tab: 0, app: None },
        }]
    }
}

/// An app that answers a bare letter as a shortcut, as a calendar's week view does: "n" opens its
/// new-event dialog, and the letters after it land in the dialog's title. A window with no field of
/// its own focused is where such a shortcut is pressed, so this one keeps no field at all.
struct CalendarLike {
    tab: u64,
    typed: Typed,
    /// The title the dialog opened over this window holds, while it is open: what the letters after
    /// the shortcut went into, and `None` while no dialog was opened.
    dialog: Option<String>,
    rect: Rect,
}

impl Widget for CalendarLike {
    fn name(&self) -> &'static str {
        "KeyboardCalendar"
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &ui::Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), nodes, self)
    }
    ui::rect_geometry!();
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        let WindowEvent::KeyboardInput { event: key, .. } = event else {
            return;
        };
        if key.state != ElementState::Pressed {
            return;
        }
        let Some(text) = key.text.as_deref() else {
            return;
        };
        match &mut self.dialog {
            // The dialog's title field has the keyboard: the rest of the word lands in it.
            Some(title) => title.push_str(text),
            None if text.eq_ignore_ascii_case("n") => {
                NEW_EVENT.with(|opened| opened.set(opened.get().saturating_add(1)));
                self.dialog = Some(String::new());
            }
            // Every other letter is nothing this app does.
            None => return,
        }
        let title = self.dialog.clone().unwrap_or_default();
        self.typed
            .borrow_mut()
            .insert(format!("event-title-{}", self.tab), title);
        *ctx.handled = true;
    }
}

impl AppWidget for CalendarLike {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Calendar"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("C")
    }
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
    /// Whether the next window's app is the one that answers a bare letter as a shortcut
    /// ([`CalendarLike`]), and how often that shortcut opened its dialog.
    static AS_CALENDAR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static NEW_EVENT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// What the apps' search was asked, and whether it has answered.
    static SEARCH: RefCell<SearchLog> = RefCell::default();
}

fn factory(
    mut context: ProcessContext,
    _: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, ui::launch::LaunchError> {
    context
        .initialize_new_resource(|doc| {
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
    if AS_CALENDAR.with(|calendar| calendar.replace(false)) {
        let mut registry = tilo_actions::ActionRegistry::new();
        registry.register::<Open>(OPEN_TYPE);
        return Ok(AppComponent::new(
            context,
            Box::new(CalendarLike {
                tab,
                typed,
                dialog: None,
                rect: Rect::ZERO,
            }),
            registry,
        ));
    }
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
    owners: ui::resource::ResourceOwners,
    root: std::path::PathBuf,
    queue: OwnerWakeQueue,
    typed: Typed,
}

impl TileSource for Source {
    fn tile(&mut self, workspace: &Workspace, tab: &Tab) -> TileHost {
        // An app that is not what this shell is about: a window of its own,
        // enough to see where search puts what it opens.
        if matches!(&tab.tile, TileContent::LocalApp { app_id, .. } if app_id == NOTES) {
            return TileHost::new(ui::widget::TileContent::Notice {
                title: tab.title.clone(),
                detail: String::new(),
            });
        }
        let workspace = workspace.id().to_string();
        NEXT.with_borrow_mut(|slot| *slot = Some((tab.id, Rc::clone(&self.typed))));
        let definition = ApplicationDefinition {
            app_id: APP.into(),
            title: "Keyboard".into(),
            icon: IconDescriptor::Glyph("K"),
            resource_type: ui::resource::ResourceType::new("keyboard-test"),
            workspace_resource: false,
            opens_before_resource: false,
            opens: &[],
            create: factory,
        };
        let records = self.owners.open(APP, &workspace).unwrap();
        let views = Rc::new(
            ui::application_view::InstanceViewStore::new(self.root.clone(), "device", &workspace)
                .unwrap(),
        );
        let permissions =
            ui::permissions::SharedPermissions::new(ui::permissions::PermissionContext::member(
                &ui::permissions::MemberId::person("keyboard-author"),
                ui::permissions::Role::Owner,
            ));
        let context = ProcessContext::prepare(
            &definition,
            ProcessContextInputs {
                records,
                views,
                device_id: "device".into(),
                instance_id: tab.id,
                permissions,
                author: ui::permissions::MemberId::person("keyboard-author"),
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

    fn launch(&mut self, _: &Workspace, id: u64, app: &str, _beside: Option<u64>) -> Option<Tab> {
        match app {
            APP => Some(keyboard_tab(id)),
            NOTES => Some(notes_tab(id)),
            _ => None,
        }
    }

    fn search_applications(&self) -> Vec<tilo_desktop_search::providers::AppEntry> {
        [(APP, "Keyboard", "Tools"), (NOTES, "Notes", "Writing")]
            .into_iter()
            .map(
                |(id, title, category)| tilo_desktop_search::providers::AppEntry {
                    id: id.into(),
                    title: title.into(),
                    category: Some(category.into()),
                },
            )
            .collect()
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
        view: String::new(),
        pinned: false,
    }
}

fn notes_tab(id: u64) -> Tab {
    Tab {
        id,
        title: "Notes".into(),
        tile: TileContent::LocalApp {
            app_id: NOTES.into(),
            label: "Notes".into(),
            note: String::new(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    }
}

/// The shell's rules, as the session's own say who may change it.
struct ChromeRules {
    handle: DesktopHandle,
}

impl DesktopRules for ChromeRules {
    fn tree(&self) -> Layout {
        self.handle.read(Clone::clone)
    }

    fn author(&self) -> String {
        "Ada".into()
    }

    fn may(&self, _: &DesktopCommand) -> Result<(), String> {
        Ok(())
    }

    fn may_create_workspace(&self) -> Result<(), String> {
        Ok(())
    }
}

struct Shell {
    driver: ui::headless::FrameHarness,
    handle: DesktopHandle,
    typed: Typed,
    tabs: Vec<u64>,
    root: std::path::PathBuf,
    /// The tree the shell shows, as the chrome over it reads it.
    tree: Layout,
    /// The chrome the session draws over the windows, holding search's card,
    /// made when a test first reaches it.
    chrome: Option<ui::headless::FrameHarness>,
}

impl Shell {
    /// `windows` app windows, each `permille` of the strip wide, in a viewport 1200 px wide: wide
    /// windows run off its right edge.
    fn new(windows: usize, permille: u32) -> Self {
        Self::of(windows, permille, false)
    }

    /// The same shell with the windows' tabs on one bar, as a window group holding several tabs: a
    /// window a person adds a tab to.
    fn one_window(windows: usize, permille: u32) -> Self {
        Self::of(windows, permille, true)
    }

    fn of(windows: usize, permille: u32, one_window: bool) -> Self {
        ui::i18n::I18nManager::init("en-US");
        let root = std::env::temp_dir().join(format!("tilo-keyboard-{}", uuid::Uuid::new_v4()));
        let mut minter = IdMinter::new(29);
        let tabs: Vec<Tab> = (0..windows).map(|_| keyboard_tab(minter.mint())).collect();
        let ids = tabs.iter().map(|tab| tab.id).collect();
        let mut tree = seed_tree(&mut minter, "Office", tabs);
        if one_window {
            let strip = &mut tree.workspaces[0].strips[0];
            let id = strip.window_groups[0].id;
            let tabs: Vec<Tab> = strip
                .window_groups
                .drain(..)
                .flat_map(|group| group.tabs)
                .collect();
            let active_tab = tabs[0].id;
            strip.window_groups = vec![WindowGroup {
                id,
                tabs,
                active_tab,
                width: WindowWidth::Fraction(permille),
                maximized: false,
                focus_history: Vec::new(),
            }];
        }
        for group in &mut tree.workspaces[0].strips[0].window_groups {
            group.width = WindowWidth::Fraction(permille);
        }
        let queue = OwnerWakeQueue::default();
        let typed = Typed::default();
        let source = Source {
            owners: ui::resource::ResourceOwners::new(root.clone()),
            root: root.clone(),
            queue: queue.clone(),
            typed: Rc::clone(&typed),
        };
        let (surface, handle) = DesktopSurface::new(tree.clone(), 29, Box::new(source));
        handle.set_viewport_width(1200.0);
        let mut driver = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::state::State::new(),
            Box::new(surface),
            (1200, 800),
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::MemberId::person("keyboard-author"),
        );
        driver.set_caller_palette_enabled(false);
        driver.attach_owner_wake_queue(queue).unwrap();
        let mut shell = Self {
            driver,
            handle,
            typed,
            tabs: ids,
            root,
            tree,
            chrome: None,
        };
        shell.settle();
        shell
    }

    fn settle(&mut self) {
        for _ in 0..4 {
            self.driver.relayout();
            self.driver.service_pending_owner_models().unwrap();
            // The chrome stands over the windows, as the compositor draws it after the shell.
            if let Some(chrome) = self.chrome.as_mut() {
                chrome.relayout();
            }
        }
    }

    /// The chrome over the shell, as the session's chrome layer shows it.
    fn chrome(&mut self) -> &mut ui::headless::FrameHarness {
        let tree = self.tree.clone();
        let handle = self.handle.clone();
        self.chrome.get_or_insert_with(|| {
            let chrome = DesktopChrome::new(
                handle.clone(),
                "g".into(),
                Rc::new(ChromeRules {
                    handle: handle.clone(),
                }),
                TopBarChrome::from_tree(&tree),
                AboutSheet::new(AboutVariant::Native, &AboutLinks::default()),
                false,
            );
            let mut frame = ui::headless::FrameHarness::for_root(
                ui::state::State::new(),
                chrome,
                (1200, 800),
                ui::Theme::dark(),
            );
            frame.relayout();
            frame
        })
    }

    /// What the person types into the open palette's field: search's card is the chrome's, over the
    /// windows, so the keys go there.
    fn type_search(&mut self, text: &str) {
        for character in text.chars() {
            let event = char_key(&character.to_string());
            self.chrome().send_host_event(&event);
        }
        self.settle_search();
    }

    /// A named key pressed in the open palette's field.
    fn key_search(&mut self, key: NamedKey) {
        let event = named_key(key);
        self.chrome().send_host_event(&event);
        self.settle_search();
    }

    /// Frames until every source of the open search has answered. A source answers on a worker, and
    /// its answer wakes the shell: a test pumps what the wake asks for and draws the frame it
    /// brings, as a host does.
    ///
    /// Two things a search waits for elsewhere are not waited for here. An application's own answer
    /// is the host's to look for on its clock. And the rows are not cut to the card's room: this
    /// shell draws no card over its windows, so no layout ever gives the palette the room its rows
    /// stand in, and the rows a test reads are the ones cut to the guess the palette keeps until a
    /// host gives it one.
    fn settle_search(&mut self) {
        let background = ui::background::Background::current();
        let now = ui::time::now();
        let deadline = now.checked_add(Duration::from_secs(5)).unwrap_or(now);
        loop {
            self.settle();
            let busy = {
                let palette = self.handle.search_palette();
                let palette = palette.borrow();
                palette.answering()
            };
            if !busy {
                return;
            }
            assert!(ui::time::now() < deadline, "the search never finished");
            let now = ui::time::now();
            background.park(now.checked_add(Duration::from_millis(2)).unwrap_or(now));
        }
    }

    /// The retained path of the widget holding keyboard focus.
    fn focus_path(&self) -> Option<String> {
        self.driver.inspection().keyboard_focus_owner
    }

    /// The accessible name of the control holding keyboard focus: a field's test id.
    fn focused_name(&self) -> Option<String> {
        self.driver.focused_access_node()?.name
    }

    fn typed(&self, field: &str) -> String {
        self.typed.borrow().get(field).cloned().unwrap_or_default()
    }

    fn rect(&self, name: &str) -> Rect {
        self.driver.targets()[name]
    }

    /// Where the shown strip rests, as the shell publishes it.
    fn pan(&self) -> Option<f64> {
        self.handle.presentation().pan.map(|pan| pan.offset)
    }

    /// Search through a host that records what it is asked to cover.
    fn with_host(&self) -> Rc<HostLog> {
        let log = Rc::new(HostLog::default());
        self.handle
            .set_search_host(Rc::new(ShellHost(Rc::clone(&log))));
        log
    }

    /// The shell's windows, each `(strip, group, tabs)` .
    fn windows(&self) -> Vec<(u64, u64, usize)> {
        self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| {
                    strip
                        .window_groups
                        .iter()
                        .map(|group| (strip.id.0, group.id, group.tabs.len()))
                })
                .collect()
        })
    }

    /// Where the window's new-tab control is drawn, as a person finds it.
    fn new_tab_control(&self) -> Point {
        let node = self
            .driver
            .a11y_node_named("New tab")
            .expect("a window offers a new tab");
        let bounds = self
            .driver
            .a11y_nodes()
            .into_iter()
            .find(|(id, _)| *id == node)
            .and_then(|(_, node)| node.bounds())
            .expect("the new tab is drawn");
        Point::new(
            f64::midpoint(bounds.x0, bounds.x1),
            f64::midpoint(bounds.y0, bounds.y1),
        )
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        self.driver.close_components();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn tab_cycles_through_the_focused_window_and_never_reaches_the_desktop_chrome() {
    let mut shell = Shell::new(2, 450);
    let first = shell.tabs[0];
    // The first window is focused: its bar's controls and its app's search button, two fields and
    // late-built button are where Tab goes, in turn, round and round.
    let window = "DesktopSurface[0]/GroupRow[0]/WindowGroup[0]/";
    let app = format!("{window}TileHost[0]/");
    let mut visited = Vec::new();
    for _ in 0..24 {
        shell.driver.key("Tab");
        let focused = shell.focus_path();
        assert!(
            focused
                .as_ref()
                .is_some_and(|path| path.starts_with(window)),
            "Tab stays in the focused window, not the next window or the desktop: {focused:?}"
        );
        visited.extend(focused);
    }
    let distinct: std::collections::HashSet<&String> = visited.iter().collect();
    let (in_app, in_bar): (Vec<&String>, Vec<&String>) = distinct
        .into_iter()
        .partition(|path| path.starts_with(&app));
    assert_eq!(
        in_app.len(),
        4,
        "Tab reaches each of the app's controls: {visited:?}"
    );
    assert!(
        !in_bar.is_empty(),
        "and the window's bar, whose controls have no other keyboard route: {visited:?}"
    );
    // And typing goes where Tab put focus.
    let field = format!("field-{first}-2");
    for _ in 0..visited.len() {
        if shell.focused_name().as_deref() == Some(field.as_str()) {
            break;
        }
        shell.driver.key("Tab");
    }
    shell.driver.type_text("ok");
    assert_eq!(shell.typed(&field), "ok");
}

#[test]
fn the_window_shown_first_and_a_window_just_opened_take_the_keyboard() {
    let mut shell = Shell::new(1, 450);
    let first = shell.tabs[0];
    shell.driver.type_text("a");
    assert_eq!(
        shell.typed(&format!("field-{first}-1")),
        "a",
        "the desktop's focused window has the keyboard from the start"
    );

    shell
        .handle
        .dispatch(
            tilo_session_core::desktop::navigation::ChromeEffect::OpenApp {
                app: APP.into(),
                beside: None,
                background: None,
                float: None,
            },
        )
        .unwrap();
    shell.settle();
    let opened = shell
        .handle
        .shown_tabs()
        .into_iter()
        .map(|(tab, _)| tab)
        .find(|tab| *tab != first)
        .expect("the launched window is shown");
    shell.driver.type_text("b");
    assert_eq!(
        shell.typed(&format!("field-{opened}-1")),
        "b",
        "what is typed right after opening a window reaches it"
    );
    assert_eq!(shell.typed(&format!("field-{first}-1")), "a");
}

#[test]
fn a_click_in_a_window_partly_off_screen_aims_typing_at_it() {
    let mut shell = Shell::new(2, 700);
    let second = shell.tabs[1];
    let field = format!("field-{second}-2");
    let rect = shell.rect(&field);
    assert!(
        rect.x0 < 1200.0 && rect.x1 > 1200.0,
        "the second window runs off the right edge: {rect:?}"
    );
    shell
        .driver
        .click_at(f64::midpoint(rect.x0, 1200.0), rect.center().y);
    shell.settle();
    shell.driver.type_text("hi");
    assert_eq!(
        shell.typed(&field),
        "hi",
        "typing reaches the clicked control"
    );
    let first = shell.tabs[0];
    assert_eq!(shell.typed(&format!("field-{first}-1")), "");
}

/// A click in a window hanging off the right edge keeps the strip where it was: the window under
/// the pointer stays under it, the field the click landed in keeps what follows, and no key
/// reaches the window beside it.
#[test]
fn a_click_in_a_window_partly_off_screen_leaves_the_strip_where_it_was() {
    let mut shell = Shell::new(2, 700);
    let (first, second) = (shell.tabs[0], shell.tabs[1]);
    let field = format!("field-{second}-2");
    let rect = shell.rect(&field);
    assert!(
        rect.x0 < 1200.0 && rect.x1 > 1200.0,
        "the second window's field stands past the right edge: {rect:?}"
    );
    let before = shell.pan();
    let at = (f64::midpoint(rect.x0, 1200.0), rect.center().y);
    shell
        .driver
        .send_host_event(&ui::synthetic_input::cursor_moved(at.0, at.1));
    shell
        .driver
        .send_host_event(&ui::synthetic_input::mouse_input(
            ElementState::Pressed,
            MouseButton::Left,
        ));
    for _ in 0..2 {
        shell.driver.relayout();
    }
    assert_eq!(
        shell.pan(),
        before,
        "the press leaves the strip where it was"
    );
    shell
        .driver
        .send_host_event(&ui::synthetic_input::mouse_input(
            ElementState::Released,
            MouseButton::Left,
        ));
    shell.settle();
    assert_eq!(
        shell.pan(),
        before,
        "the click leaves the pressed window under the pointer"
    );
    assert!(
        shell.rect(&field).x1 > 1200.0,
        "the pressed field is where it was clicked"
    );
    shell.driver.type_text("buy milk");
    assert_eq!(
        shell.typed(&field),
        "buy milk",
        "what follows the click stays in the field it was clicked into: {:?}",
        shell.focus_path()
    );
    assert_eq!(
        shell.typed(&format!("field-{first}-1")),
        "",
        "no key reaches the window beside it"
    );
}

/// A press in a window clipped at the edge, away from any control, still aims what is typed next
/// at that window: its app takes the keyboard, never the window that had it before.
#[test]
fn a_press_in_a_window_partly_off_screen_aims_typing_at_it() {
    let mut shell = Shell::new(2, 700);
    let (first, second) = (shell.tabs[0], shell.tabs[1]);
    let rect = shell.rect(&format!("field-{second}-2"));
    assert!(rect.x0 < 1200.0 && rect.x1 > 1200.0, "{rect:?}");
    // Its body below the fields, clear of every control.
    let at = (f64::midpoint(rect.x0, 1200.0), rect.y1 + 120.0);
    let before = shell.pan();
    shell.driver.click_at(at.0, at.1);
    shell.settle();
    assert_eq!(
        shell.pan(),
        before,
        "the click leaves the strip where it was"
    );
    shell.driver.type_text("a");
    assert_eq!(
        shell.typed(&format!("field-{second}-1")),
        "a",
        "typing reaches the window the press landed in: {:?}",
        shell.focus_path()
    );
    assert_eq!(
        shell.typed(&format!("field-{first}-1")),
        "",
        "the window beside it is handed nothing"
    );
}

#[test]
fn an_apps_own_search_button_opens_search_over_its_window() {
    let mut shell = Shell::new(2, 450);
    assert!(!shell.handle.search_open());
    shell.driver.click_named("search-open").unwrap();
    shell.settle();
    assert!(
        shell.handle.search_open(),
        "the app's search button opens the desktop's search"
    );
}

#[test]
fn the_shell_says_who_holds_the_keyboard() {
    let mut shell = Shell::new(2, 450);
    assert_eq!(
        shell.handle.input_owner().as_deref(),
        Some(APP),
        "the focused window's application holds it"
    );
    shell.driver.click_named("search-open").unwrap();
    shell.settle();
    assert_eq!(
        shell.handle.input_owner().as_deref(),
        Some("system-search"),
        "search takes the keyboard while its palette is up"
    );
    shell.handle.close_search();
    shell.settle();
    assert_eq!(
        shell.handle.input_owner().as_deref(),
        Some(APP),
        "and gives it back to the window under it when it closes"
    );
    // A host that saw a press beside the card asks again: closing a closed search opens nothing.
    shell.handle.close_search();
    shell.settle();
    assert!(
        !shell.handle.search_open(),
        "dismissing twice never opens a search"
    );
}

/// Search owns the keyboard while it is up, as the shell says who holds it: a window's app is
/// offered no key. A bare letter is a shortcut in the app behind the card — a calendar's week view
/// opens its new-event dialog on "n" — so one that reached it opened a dialog over the search and
/// took the rest of the word into the dialog's title instead of the query.
#[test]
fn the_open_search_takes_every_key_from_the_window_under_it() {
    AS_CALENDAR.with(|calendar| calendar.set(true));
    let mut shell = Shell::new(1, 450);
    let tab = shell.tabs[0];
    assert_eq!(
        shell.handle.input_owner().as_deref(),
        Some(APP),
        "the window's calendar has the keyboard before search opens"
    );
    shell.handle.open_search(Scope::Workspace, None);
    shell.settle();
    assert!(shell.handle.search_open());
    // What the person types is the search's.
    shell.type_search("Notes");
    assert_eq!(
        shell.handle.search_rows().0,
        "Notes",
        "the query holds what was typed"
    );
    // The same keys handed to the shell itself — the frame the palette opened in, or a host that
    // routes the press — are the search's too: no shortcut fires and no dialog takes the rest.
    shell.driver.type_text("Notes");
    assert_eq!(
        NEW_EVENT.with(std::cell::Cell::get),
        0,
        "no app shortcut fires while search is open"
    );
    assert_eq!(
        shell.typed(&format!("event-title-{tab}")),
        "",
        "and no dialog opened over the search to take the letters after it"
    );
    assert_eq!(
        shell.handle.search_rows().0,
        "Notes",
        "the search still holds the query it was given"
    );
}

/// A host's own verb — the page's "Invite people", or anything only the page can do — is a row of
/// the search over everything, and Enter on it hands the id back and closes the search: the card
/// does not stay up over the sheet the host opens.
#[test]
fn a_host_verbs_row_hands_its_id_back_and_closes_the_search() {
    let mut shell = Shell::new(1, 450);
    shell.handle.set_host_actions(vec![HostAction {
        id: "invite-people".into(),
        title: "Invite people".into(),
        label: None,
        subtitle: Some("Bring someone into this workspace".into()),
        icon: None,
        destructive: false,
        unasked: true,
        source: None,
    }]);
    shell.handle.open_search(Scope::Workspace, None);
    shell.settle();
    shell.type_search("invite people");
    let (_, rows) = shell.handle.search_rows();
    assert!(
        rows.iter().any(|row| row.title == "Invite people"),
        "the host's verb is a row: {rows:?}"
    );
    let choosable = rows
        .iter()
        .position(|row| row.title == "Invite people")
        .expect("the row stands");
    for _ in 0..choosable {
        shell.key_search(NamedKey::ArrowDown);
    }
    shell.key_search(NamedKey::Enter);
    shell.settle();
    assert_eq!(
        shell.handle.take_host_choices(),
        ["invite-people"],
        "the host is asked to perform it"
    );
    assert!(
        !shell.handle.search_open(),
        "and the search closes over what the host opens"
    );
}

/// An app's action with nothing to act on now takes no row, so "invite" offers the workspace's own
/// invite first and Enter carries it out: the card never stands on a row that can only say why it
/// cannot run.
#[test]
fn an_action_that_cannot_run_now_takes_no_row_and_the_runnable_one_is_first() {
    let mut shell = Shell::new(1, 450);
    shell.with_host();
    shell.handle.set_host_actions(vec![HostAction {
        id: "invite-people".into(),
        title: "Invite people".into(),
        label: None,
        subtitle: Some("Bring someone into this workspace".into()),
        icon: None,
        destructive: false,
        unasked: true,
        source: None,
    }]);
    shell.handle.open_search(Scope::Workspace, None);
    shell.settle();
    shell.type_search("invite");
    let (_, rows) = shell.handle.search_rows();
    let titles: Vec<&str> = rows.iter().map(|row| row.title.as_str()).collect();
    assert_eq!(
        titles.first().copied(),
        Some("Invite people"),
        "the row that runs comes first: {titles:?}"
    );
    assert!(
        !titles.contains(&"Invite"),
        "the event's Invite, with no event selected, takes no row: {titles:?}"
    );
    shell.key_search(NamedKey::Enter);
    shell.settle();
    assert_eq!(
        shell.handle.take_host_choices(),
        ["invite-people"],
        "Enter carries out the row that stands first"
    );
}

/// A host set while search is open, as the page's own search arrives after its first frame, feeds
/// the open search: what the person already typed finds what the host adds.
#[test]
fn a_host_set_while_search_is_open_feeds_it() {
    let mut shell = Shell::new(1, 450);
    shell.handle.open_search(Scope::Workspace, None);
    shell.settle();
    shell.type_search("quarterly");
    let (_, rows) = shell.handle.search_rows();
    assert!(
        rows.iter().all(|row| row.title != "Quarterly plan"),
        "nothing finds the plan before the host is here: {rows:?}"
    );
    let host = shell.with_host();
    // Its source answers on a worker, as the page's own does: the search asks
    // it again for the query typed before it was here, and shows its row when
    // the answer arrives.
    shell.settle_search();
    let (query, rows) = shell.handle.search_rows();
    assert_eq!(query, "quarterly", "the person's query stays");
    assert!(
        rows.iter().any(|row| row.title == "Quarterly plan"),
        "the host's source answers it: {rows:?}"
    );
    assert_eq!(host.scopes.borrow().as_slice(), [Scope::Workspace]);
}

/// The chord that opens search is the shell's, never the window's under it: an app that takes every
/// key it is handed is not handed the chord, so no app's own Control+K runs on the press that opened
/// the search.
#[test]
fn the_search_chord_is_never_the_windows() {
    GREEDY.with(|greedy| greedy.set(true));
    let mut shell = Shell::new(1, 450);
    let tab = shell.tabs[0];
    shell.driver.set_modifiers(&["ctrl".into()]);
    shell.driver.key("k");
    shell.driver.set_modifiers(&[]);
    shell.settle();
    assert_eq!(
        shell.typed(&format!("greedy-{tab}")),
        "",
        "the window's app is offered nothing of the chord"
    );
}

/// Escape on the open palette closes it, whatever holds the field: a card over what the person is
/// doing is dismissed by the key that dismisses every card of theirs.
#[test]
fn escape_closes_the_search_that_has_a_query_in_its_field() {
    let mut shell = Shell::new(1, 450);
    shell.handle.open_search(Scope::Workspace, None);
    shell.settle_search();
    shell.type_search("notes");
    assert_eq!(shell.handle.search_rows().0, "notes");
    shell.key_search(NamedKey::Escape);
    assert!(
        !shell.handle.search_open(),
        "Escape closes the search with a query typed"
    );
}

#[test]
fn search_an_app_opened_asks_it_and_follows_its_answer_once_it_is_in() {
    let mut shell = Shell::new(1, 450);
    shell.driver.click_named("search-open").unwrap();
    shell.settle();
    assert!(shell.handle.search_open());
    shell.type_search("lofi");
    let asked = SEARCH.with_borrow(|search| search.asked.clone());
    assert_eq!(
        asked.last().map(String::as_str),
        Some("lofi"),
        "the app is asked what was typed: {asked:?}"
    );
    // The app answers on a later frame: search asks again then, so what it offers for the query (a
    // row searching its service) shows.
    let before = asked.len();
    SEARCH.with_borrow_mut(|search| search.answered = true);
    shell.settle();
    let asked = SEARCH.with_borrow(|search| search.asked.clone());
    assert!(
        asked.len() > before && asked.last().map(String::as_str) == Some("lofi"),
        "search asks the app again once its answer is in: {asked:?}"
    );
}

/// What an app's search offers for the query ("Search `YouTube` for …") is a row of the search over
/// its window, and choosing it runs inside the app, at the hit's anchor, through the app's `open` .
#[test]
fn a_hit_chosen_in_an_apps_search_opens_inside_that_app() {
    SEARCH.with_borrow_mut(|search| search.answered = true);
    let mut shell = Shell::new(1, 450);
    shell.driver.click_named("search-open").unwrap();
    shell.settle();
    shell.type_search("lofi");
    shell.key_search(NamedKey::Enter);
    assert_eq!(
        SEARCH.with_borrow(|search| search.opened.clone()),
        ["videos.search:lofi"],
        "the app opens at the hit chosen"
    );
    assert!(!shell.handle.search_open());
    assert_eq!(shell.driver.refusal_said(), None);
    assert!(
        shell.driver.control_failures().is_empty(),
        "{:?}",
        shell.driver.control_failures()
    );
}

/// An app's action chosen in search over everything runs inside that app,
/// under its ownership, not at the desktop's API, and its window comes
/// forward to show it.
#[test]
fn an_apps_action_chosen_in_search_over_everything_runs_in_its_window() {
    let mut shell = Shell::new(2, 450);
    let second = shell.tabs[1];
    SEARCH.with_borrow_mut(|search| search.labels.insert(second, "Archive".into()));
    shell.handle.open_search(Scope::Workspace, None);
    shell.settle();
    assert!(shell.handle.search_open());
    shell.type_search("open archive");
    shell.key_search(NamedKey::Enter);
    // `open` asks for the item; it runs once that is given.
    shell.type_search("clip");
    shell.key_search(NamedKey::Enter);
    assert_eq!(
        SEARCH.with_borrow(|search| search.opened.clone()),
        ["clip"],
        "the action runs in the app that offered it"
    );
    assert_eq!(shell.driver.refusal_said(), None);
    assert!(
        shell.driver.control_failures().is_empty(),
        "{:?}",
        shell.driver.control_failures()
    );
    assert!(!shell.handle.search_open());
    shell.driver.type_text("z");
    assert_eq!(
        shell.typed(&format!("field-{second}-1")),
        "z",
        "the app's window came forward and has the keyboard"
    );
}

/// Search over one app's window asks that window's app, and its host for the entries the app
/// published for that window alone: not the shell's own commands, and never another app's rows. The
/// hit the app offers opens inside it.
#[test]
fn search_over_an_apps_window_asks_that_app_and_not_the_shell() {
    SEARCH.with_borrow_mut(|search| search.answered = true);
    let mut shell = Shell::new(2, 450);
    let host = shell.with_host();
    shell.driver.click_named("search-open").unwrap();
    shell.settle();
    assert!(shell.handle.search_open());
    assert!(
        matches!(host.scopes.borrow().as_slice(), [Scope::InApp(_)]),
        "an app's search asks the host for that app's window: {:?}",
        host.scopes.borrow()
    );
    shell.type_search("lofi");
    let (_, rows) = shell.handle.search_rows();
    assert!(
        rows.iter().any(|row| row.title.contains("lofi")),
        "the window's app answers: {rows:?}"
    );
    assert!(
        rows.iter().all(|row| row.title != "Close window"),
        "the shell's own commands are not the window's: {rows:?}"
    );
    shell.key_search(NamedKey::Enter);
    assert_eq!(
        SEARCH.with_borrow(|search| search.opened.clone()),
        ["videos.search:lofi"],
        "the hit opens inside the app"
    );
    assert!(!shell.handle.search_open());
    assert!(
        shell.driver.control_failures().is_empty(),
        "{:?}",
        shell.driver.control_failures()
    );
}

/// An app that runs in a process of its own publishes its entries into the session's index and
/// gives the shell no tree to read: what its window holds is found there, and the row opens the
/// thing it names.
#[test]
fn a_windows_search_finds_what_the_session_holds_for_its_app() {
    SEARCH.with_borrow_mut(|search| search.answered = true);
    let mut shell = Shell::new(2, 450);
    let host = shell.with_host();
    shell.driver.click_named("search-open").unwrap();
    shell.settle();
    assert!(shell.handle.search_open());
    assert!(
        matches!(host.scopes.borrow().as_slice(), [Scope::InApp(_)]),
        "the host is asked for the app the window shows: {:?}",
        host.scopes.borrow()
    );
    shell.type_search("quarterly");
    let (_, rows) = shell.handle.search_rows();
    assert!(
        rows.iter().any(|row| row.title == "Quarterly plan"),
        "what the app published for its window is found: {rows:?}"
    );
}

/// A window group's new tab searches the workspace as the main box does: the applications to show
/// there and what the workspace's own search holds, and an application chosen opens as a tab of
/// that window group.
#[test]
fn a_window_groups_new_tab_searches_the_workspace_and_opens_the_app_chosen_there() {
    let mut shell = Shell::one_window(2, 450);
    let host = shell.with_host();
    let before = shell.windows();
    assert_eq!(before.len(), 1, "one window, two tabs: {before:?}");
    let (strip, group, tabs) = before[0];
    let plus = shell.new_tab_control();
    shell.driver.click_at(plus.x, plus.y);
    shell.settle_search();
    assert!(shell.handle.search_open());
    assert!(
        matches!(host.scopes.borrow().as_slice(), [Scope::WindowGroup(_)]),
        "the workspace's own search answers the new tab too: {:?}",
        host.scopes.borrow()
    );
    let (_, rows) = shell.handle.search_rows();
    assert!(
        rows.iter().any(|row| row.title == "Notes"),
        "what a new tab may show: {rows:?}"
    );
    shell.type_search("quarterly");
    let (_, rows) = shell.handle.search_rows();
    assert!(
        rows.iter().any(|row| row.title == "Quarterly plan"),
        "and what the workspace's own search holds: {rows:?}"
    );
    for _ in "quarterly".chars() {
        shell.key_search(NamedKey::Backspace);
    }
    shell.type_search("notes");
    shell.key_search(NamedKey::Enter);
    let after = shell.windows();
    assert_eq!(
        after.len(),
        before.len(),
        "no window opened of its own: {after:?}"
    );
    let opened = after
        .iter()
        .find(|(window_strip, window_group, _)| *window_strip == strip && *window_group == group)
        .expect("the window group is still there");
    assert_eq!(
        opened.2,
        tabs + 1,
        "the app opens a tab of that window group: {after:?}"
    );
}

/// Search over the workspace asks the shell's host and the shell's own windows; an application chosen
/// there opens as a window of its own.
#[test]
fn search_over_the_workspace_asks_the_host_and_opens_an_app_as_its_own_window() {
    let mut shell = Shell::new(2, 450);
    let host = shell.with_host();
    let before = shell.windows();
    shell
        .handle
        .open_search(Scope::Workspace, Some("quarterly".into()));
    // A source answers on a worker: the rows are the search's once each has
    // answered, not four frames in.
    shell.settle_search();
    assert_eq!(*host.scopes.borrow(), [Scope::Workspace]);
    let (_, rows) = shell.handle.search_rows();
    assert!(
        rows.iter().any(|row| row.title == "Quarterly plan"),
        "what the host holds is searched: {rows:?}"
    );

    shell
        .handle
        .open_search(Scope::Workspace, Some("notes".into()));
    shell.settle_search();
    shell.key_search(NamedKey::Enter);
    assert_eq!(
        *host.scopes.borrow(),
        [Scope::Workspace, Scope::Workspace],
        "each search covers the workspace in front"
    );
    let after = shell.windows();
    assert_eq!(
        after.len(),
        before.len() + 1,
        "the app opens a window of its own: {after:?}"
    );
    let opened = after
        .iter()
        .find(|(_, group, _)| !before.iter().any(|(_, open, _)| open == group))
        .expect("one window is new");
    assert_eq!(opened.2, 1, "the window shows that one tab: {after:?}");
}

#[test]
fn a_button_an_app_builds_after_it_opened_answers_its_click() {
    let mut shell = Shell::new(2, 450);
    for tab in shell.tabs.clone() {
        let button = format!("late-{tab}");
        shell.driver.click_named(&button).unwrap();
        shell.settle();
        let pressed = PRESSED.with_borrow(|all| all[&tab].get());
        assert_eq!(pressed, 1, "{button} ran its effect once");
        assert!(
            shell.driver.control_failures().is_empty(),
            "{:?}",
            shell.driver.control_failures()
        );
    }
}

#[test]
fn typing_reaches_the_focused_window_even_when_a_window_before_it_claims_keys() {
    GREEDY.with(|greedy| greedy.set(true));
    let mut shell = Shell::new(2, 450);
    let (first, second) = (shell.tabs[0], shell.tabs[1]);
    let field = format!("field-{second}-1");
    let rect = shell.rect(&field);
    shell.driver.click_at(rect.center().x, rect.center().y);
    shell.settle();
    shell.driver.type_text("ab");
    shell.driver.commit_text("cd");
    assert_eq!(
        shell.typed(&field),
        "abcd",
        "keys and composed text reach the focused window's field"
    );
    assert_eq!(
        shell.typed(&format!("greedy-{first}")),
        "",
        "the window beside it is not offered what is typed"
    );
}

/// A window with a field holding the keyboard, and the person typing into it: as a chat's composer
/// takes what is typed and sends it on Enter.
fn typing_in_the_first_window() -> (Shell, String) {
    let mut shell = Shell::new(1, 450);
    let field = format!("field-{}-1", shell.tabs[0]);
    let rect = shell.rect(&field);
    shell.driver.click_at(rect.center().x, rect.center().y);
    shell.settle();
    shell.driver.type_text("hi");
    assert_eq!(shell.typed(&field), "hi", "the field has the keyboard");
    (shell, field)
}

/// The window the person opened from search has the keyboard from then on, though its app has not
/// arrived (here it never does): what they type next never reaches the field of the window they
/// left, and neither does the focus closing the search would hand back.
#[test]
fn keys_after_search_opens_a_window_never_reach_the_window_left() {
    let (mut shell, field) = typing_in_the_first_window();
    shell.handle.open_search(Scope::Workspace, None);
    shell.settle();
    shell.type_search("Notes");
    shell.key_search(NamedKey::Enter);
    shell.settle();
    let notes = shell
        .handle
        .shown_tabs()
        .into_iter()
        .map(|(tab, _)| tab)
        .find(|tab| *tab != shell.tabs[0])
        .expect("Notes opened beside the first window");
    assert_eq!(
        shell.handle.input_owner().as_deref(),
        Some(NOTES),
        "Notes has the keyboard ({notes})"
    );
    shell.driver.type_text("Files");
    shell.driver.key("Enter");
    assert_eq!(
        shell.typed(&field),
        "hi",
        "nothing typed after reached the window left"
    );
    assert!(
        shell
            .focus_path()
            .is_none_or(|path| !path.contains("WindowGroup[0]")),
        "no control of the window left keeps the keyboard: {:?}",
        shell.focus_path()
    );
}

/// The same for a window opened by a command rather than search.
#[test]
fn keys_after_a_window_opens_never_reach_the_window_left() {
    let (mut shell, field) = typing_in_the_first_window();
    shell
        .handle
        .dispatch(
            tilo_session_core::desktop::navigation::ChromeEffect::OpenApp {
                app: NOTES.into(),
                beside: None,
                background: None,
                float: None,
            },
        )
        .unwrap();
    shell.settle();
    assert_eq!(shell.handle.input_owner().as_deref(), Some(NOTES));
    shell.driver.type_text("ls");
    shell.driver.key("Enter");
    assert_eq!(
        shell.typed(&field),
        "hi",
        "nothing typed after reached the window left"
    );
}

/// Coming back to the window left gives its field the keyboard again, as the person left it.
#[test]
fn the_window_left_has_its_field_back_on_return() {
    let (mut shell, field) = typing_in_the_first_window();
    shell
        .handle
        .dispatch(
            tilo_session_core::desktop::navigation::ChromeEffect::OpenApp {
                app: NOTES.into(),
                beside: None,
                background: None,
                float: None,
            },
        )
        .unwrap();
    shell.settle();
    shell.handle.open_search(Scope::Workspace, None);
    shell.settle();
    shell.type_search("Keyboard");
    shell.key_search(NamedKey::Enter);
    shell.settle();
    assert_eq!(shell.handle.input_owner().as_deref(), Some(APP));
    shell.driver.type_text("!");
    assert_eq!(
        shell.typed(&field),
        "hi!",
        "the field the person left has the keyboard again"
    );
}

#[test]
fn one_click_in_an_inactive_window_gives_the_control_under_it_the_keyboard() {
    for coalesce in [false, true] {
        let mut shell = Shell::new(2, 450);
        shell.driver.set_coalesce_input_frames(coalesce);
        let second = shell.tabs[1];
        let field = format!("field-{second}-2");
        let rect = shell.rect(&field);
        shell
            .driver
            .send_host_event(&ui::synthetic_input::cursor_moved(
                rect.center().x,
                rect.center().y,
            ));
        shell
            .driver
            .send_host_event(&ui::synthetic_input::mouse_input(
                ElementState::Pressed,
                MouseButton::Left,
            ));
        shell
            .driver
            .send_host_event(&WindowEvent::Ime(ui::winit::event::Ime::Disabled));
        shell.driver.send_host_event(&WindowEvent::Focused(true));
        shell
            .driver
            .send_host_event(&ui::synthetic_input::mouse_input(
                ElementState::Released,
                MouseButton::Left,
            ));
        for _ in 0..4 {
            let _ = shell.driver.poll_frame();
        }
        shell.driver.type_text("hi");
        assert_eq!(
            shell.typed(&field),
            "hi",
            "coalesce {coalesce}: {:?}",
            shell.focus_path()
        );
    }
}

/// The retained path of the widget whose semantics are named `name` .
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
    let mut shell = Shell::new(2, 450);
    let second = shell.tabs[1];
    let field = format!("field-{second}-1");
    let path = semantic_path(&shell.driver.inspection().root, &field).unwrap();
    shell
        .driver
        .perform_semantic_named(&path, Some(&field), &ui::a11y::Action::Focus)
        .unwrap();
    shell.settle();
    shell
        .driver
        .perform_semantic_named(
            &path,
            Some(&field),
            &ui::a11y::Action::SetValue("Ada".into()),
        )
        .unwrap();
    shell.settle();
    assert_eq!(shell.typed(&field), "Ada");
}

/// A press on one of the app's own slots in its window's bar is a press on the app: the keyboard
/// the click gave the bar goes back to the control that had it, so Escape (which closes the panel
/// the slot opened) and what is typed next reach the app, not the bar.
#[test]
fn a_press_on_an_apps_slot_in_its_bar_leaves_the_keyboard_with_the_app() {
    let mut shell = Shell::new(1, 450);
    let tab = shell.tabs[0];
    let field = format!("field-{tab}-1");
    let rect = shell.rect(&field);
    shell.driver.click_at(rect.center().x, rect.center().y);
    shell.settle();
    shell.driver.type_text("a");
    assert_eq!(shell.typed(&field), "a", "the clicked field takes typing");

    let slot = shell
        .driver
        .a11y_nodes()
        .into_iter()
        .find_map(|(_, node)| {
            (node.label() == Some("Toggle panel"))
                .then(|| node.bounds())
                .flatten()
        })
        .expect("the app's slot stands in its window's bar");
    shell.driver.click_at(
        f64::midpoint(slot.x0, slot.x1),
        f64::midpoint(slot.y0, slot.y1),
    );
    shell.settle();
    assert_eq!(
        shell.focused_name().as_deref(),
        Some(field.as_str()),
        "the field keeps the keyboard: {:?}",
        shell.focus_path()
    );
    shell.driver.type_text("b");
    assert_eq!(
        shell.typed(&field),
        "ab",
        "typing after the press reaches the app"
    );
}
