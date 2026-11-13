//! The menu drawn from the session's reading: a context menu over the rows
//! the desk offers the window in front, whose every choice is one of the
//! session's verbs.
//!
//! The surface holds no menu state. What is up, for which window, where it
//! stands and which rows it offers are all in the reading
//! (`desktop:window-menu`), watched through [`SessionResource`]; closing the
//! menu anywhere — the Escape key, another window taking over, a script —
//! is the reading saying so, and the menu here follows. What this surface
//! adds is the painting and the pointer and arrow keys inside the open menu,
//! which are its own the way any widget's are.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use ui::collaboration::SessionResource;
use ui::framework::{NoEffect, ResourceSurface, SurfaceCx};
use ui::taffy::{NodeId, Style, TaffyTree};
use ui::theme::Theme;
use ui::vello::kurbo::{Point, Rect};
use ui::widget::{AppModel, ContextMenu, MenuItem, ModelPath, TaffyTreeExt as _, Widget};
use ui::winit::event::WindowEvent;

use crate::resource::WindowMenu;

/// The session's resource this view draws.
pub const WINDOW_MENU: &str = "desktop:window-menu";

/// The resource every sender runs a send through: the session's one send action, the same an
/// app's box, the canvas and an agent run.
const SEND_TO: &str = "session:send-to";

/// What that resource calls sending.
const SEND: &str = "send";

/// How long to wait before asking the session again for a reading it did
/// not answer: the menu is up for a moment, so a view that gives up is a
/// menu nobody sees.
const RETRY: Duration = Duration::from_secs(1);

/// What the shell says the window menu is: the window it is for, where it
/// stands, and the rows that window offers now.
#[derive(Debug, Default, PartialEq)]
struct Reading {
    tab: Option<String>,
    anchor: Option<(f64, f64)>,
    /// The window the menu is on, when the reading names it: what a typed
    /// sentence would be about.
    subject: Option<Subject>,
    rows: Vec<Row>,
}

/// The window a menu is on, named as the person knows it.
#[derive(Clone, Debug, PartialEq)]
struct Subject {
    title: String,
    app: Option<String>,
}

/// One row a window offers.
#[derive(Clone, Debug, PartialEq)]
struct Row {
    id: String,
    label: String,
    enabled: bool,
    reason: Option<String>,
    /// The chord that does the same thing, as the session's table binds it.
    shortcut: Option<String>,
    /// The picture of the command the row runs, as the session names it.
    icon: Option<ui::icons::Icon>,
    /// The rows a row that opens a submenu offers.
    children: Vec<Row>,
    /// A rule between two groups of rows, which chooses nothing.
    separator: bool,
}

impl Row {
    /// The row `text` spells, when it is one; its submenu's rows with it.
    fn parse(text: &Value) -> Option<Self> {
        Some(Self {
            id: text.get("id")?.as_str()?.to_owned(),
            label: text.get("label")?.as_str()?.to_owned(),
            enabled: text.get("enabled")?.as_bool()?,
            reason: text
                .get("reason")
                .and_then(Value::as_str)
                .map(str::to_owned),
            shortcut: text
                .get("shortcut")
                .and_then(Value::as_str)
                .map(str::to_owned),
            icon: text
                .get("icon")
                .and_then(Value::as_str)
                .and_then(ui::icons::Icon::from_name),
            children: text
                .get("children")
                .and_then(Value::as_array)
                .map(|children| children.iter().filter_map(Self::parse).collect())
                .unwrap_or_default(),
            separator: text
                .get("separator")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

impl Reading {
    /// The reading `text` spells, when it is one.
    fn parse(text: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(text).ok()?;
        let anchor = value
            .get("anchor")
            .and_then(|anchor| Some((anchor.get("x")?.as_f64()?, anchor.get("y")?.as_f64()?)));
        let rows = value
            .get("rows")?
            .as_array()?
            .iter()
            .map(Row::parse)
            .collect::<Option<Vec<Row>>>()?;
        let subject = value.get("subject").and_then(|subject| {
            Some(Subject {
                title: subject.get("title")?.as_str()?.to_owned(),
                app: subject
                    .get("app")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
        });
        Some(Self {
            tab: value.get("tab").and_then(Value::as_str).map(str::to_owned),
            anchor,
            subject,
            rows,
        })
    }

    /// Whether a menu is up: a window and something to offer it.
    fn is_open(&self) -> bool {
        self.tab.is_some() && !self.rows.is_empty()
    }
}

/// The view: the context menu, and the reading it draws.
pub struct WindowMenuSurface {
    menu: ContextMenu,
    /// The row each item of the menu is for, as the reading listed it:
    /// nothing for a rule and for the row that opens a submenu.
    ids: Vec<Option<String>>,
    /// The rows of each item's submenu, aligned with `ids`: empty where the
    /// item opens none.
    nested: Vec<Vec<Option<String>>>,
    reading: Reading,
    /// The session's resource, once a worker reached it.
    opened: Arc<Mutex<Option<SessionResource>>>,
    /// Whether a worker is asking for the resource now.
    asking: Arc<AtomicBool>,
    /// When to ask again, after a worker found no session to ask.
    again: Option<Instant>,
    /// Whether the view already said that no session publishes the menu.
    told: bool,
    /// The frame wake the reading moves the view with.
    wake: Option<ui::WakeHandle>,
    /// The area the menu may stand in: the surface's own size.
    viewport: Rect,
    rect: Rect,
}

impl Default for WindowMenuSurface {
    fn default() -> Self {
        Self {
            menu: ContextMenu::default(),
            ids: Vec::new(),
            nested: Vec::new(),
            reading: Reading::default(),
            opened: Arc::new(Mutex::new(None)),
            asking: Arc::new(AtomicBool::new(false)),
            again: None,
            told: false,
            wake: None,
            viewport: Rect::ZERO,
            rect: Rect::ZERO,
        }
    }
}

impl WindowMenuSurface {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The window menu as the session reads it now, for a host that shows the
    /// view without a session: what a staged picture and a test draw from.
    pub fn showing(reading: &str) -> Self {
        let mut surface = Self::new();
        if let Some(reading) = Reading::parse(reading) {
            surface.reading = reading;
            surface.draw();
        }
        surface
    }

    /// Ask the session for this resource, on a worker: opening it waits for
    /// the first reading, and the frame cannot.
    fn ask(&self) {
        if self.reached().is_some() || self.asking.load(Ordering::Relaxed) {
            return;
        }
        if self.again.is_some_and(|again| Instant::now() < again) {
            return;
        }
        let Some(wake) = self.wake.clone() else {
            return;
        };
        self.asking.store(true, Ordering::Relaxed);
        let opened = Arc::clone(&self.opened);
        let asking = Arc::clone(&self.asking);
        std::thread::spawn(move || {
            if let Ok(reached) = SessionResource::open(WINDOW_MENU) {
                reached.attach(wake.clone());
                if let Ok(mut slot) = opened.lock() {
                    *slot = Some(reached);
                }
            }
            asking.store(false, Ordering::Relaxed);
            // The reading the worker opened is one the session published
            // before this view existed — the menu a press opened a moment
            // ago — and a reading that arrived before the attach wakes
            // nobody. The view asks for its own frame so the menu it was
            // started for stands on screen instead of waiting for a press
            // that may never come.
            wake.wake();
        });
    }

    /// The session's resource, once a worker reached it.
    fn reached(&self) -> Option<SessionResource> {
        self.opened
            .lock()
            .ok()
            .and_then(|opened| opened.as_ref().cloned())
    }

    /// The reading the session published since the last call, if it moved.
    fn follow(&mut self) -> bool {
        let Some(opened) = self.reached() else {
            // A worker that found no session leaves nothing to read: ask
            // again in a moment, rather than never, and say so once.
            if !self.asking.load(Ordering::Relaxed) && self.again.is_none() {
                if !self.told {
                    tracing::warn!("the window menu is not published by a session");
                    self.told = true;
                }
                self.again = Some(
                    Instant::now()
                        .checked_add(RETRY)
                        .unwrap_or_else(Instant::now),
                );
            }
            if self.again.is_some_and(|again| Instant::now() >= again) {
                self.again = None;
                self.ask();
            }
            return false;
        };
        let reading = match opened.latest() {
            Some(Ok(text)) => Reading::parse(&text),
            Some(Err(error)) => {
                tracing::warn!(%error, "the window menu reading");
                None
            }
            None => None,
        };
        let Some(reading) = reading else {
            return false;
        };
        if reading == self.reading {
            return false;
        }
        self.reading = reading;
        self.draw();
        true
    }

    /// Draw the reading: the rows as the menu's items, standing where the
    /// session says, or nothing at all when no menu is up.
    fn draw(&mut self) {
        if !self.reading.is_open() {
            self.menu.close();
            self.ids.clear();
            self.nested.clear();
            return;
        }
        let mut items = Vec::with_capacity(self.reading.rows.len());
        self.ids = Vec::with_capacity(self.reading.rows.len());
        self.nested = Vec::with_capacity(self.reading.rows.len());
        for row in &self.reading.rows {
            let (item, choice) = item_of(row);
            items.push(item);
            self.ids.push(choice);
            self.nested
                .push(row.children.iter().map(|child| item_of(child).1).collect());
        }
        self.menu.set_items(items);
        // The session heard the secondary press that opened the menu and
        // says what it is on: typing narrows the rows here, and a sentence
        // is about that window when something can answer it.
        self.menu.set_takes_typing(true);
        self.menu
            .set_about(self.reading.subject.as_ref().map(|subject| {
                let named = match &subject.app {
                    Some(app) => format!("{} ({app})", subject.title),
                    None => subject.title.clone(),
                };
                ui::shared_selection::SharedSelection::of_text(&named)
            }));
        let anchor = self
            .reading
            .anchor
            .map_or(Rect::ZERO, |(x, y)| Rect::new(x, y, x, y));
        self.menu.open_anchored(anchor, self.viewport);
        self.menu.focus_first_action();
    }

    /// Choose row `id` through the session's verb, on a worker: the request
    /// waits for the session to take it, and the reading says what became of
    /// it.
    fn choose(&self, id: &str) {
        self.say("choose", vec![("row", Value::String(id.to_owned()))]);
    }

    /// Tell the session the menu is closed: what the view does when the
    /// person dismisses it here — a press outside it — rather than with the
    /// key the session hears.
    fn dismiss(&self) {
        self.say("close", Vec::new());
    }

    /// Invoke one of the session's verbs, on a worker: the request waits for
    /// the session to take it, and the reading says what became of it.
    fn say(&self, verb: &'static str, arguments: Vec<(&str, Value)>) {
        let Some(opened) = self.reached() else {
            return;
        };
        let taken = arguments
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect();
        std::thread::spawn(move || {
            if let Err(error) = opened.invoke(verb, &taken) {
                tracing::warn!(%error, verb, "the window menu's verb was not taken");
            }
        });
    }

    /// Hand what the menu's send row opened to the session: each send goes as the one send action
    /// every sender runs (`session:send-to#send`), carrying the window or tab the menu stands on.
    /// A box's search is its host's to answer, and this host knows no people or agents: a name the
    /// person types is the agent the box makes of it, as it does of a name the search never found.
    fn deliver(&self) {
        for request in ui::shared_selection::take_send_to() {
            let arguments = send_arguments(request);
            std::thread::spawn(move || {
                if let Err(error) =
                    SessionResource::invoke_without_watching(SEND_TO, SEND, &arguments)
                {
                    tracing::warn!(%error, "the window menu's send was not taken");
                }
            });
        }
        for (request, _query) in ui::shared_selection::take_recipient_searches() {
            ui::shared_selection::answer_recipients(request, Vec::new());
        }
    }

    /// The row the open menu was just asked for, if the person chose one: a
    /// row of a submenu by its row's place and its own.
    fn take_choice(&mut self) -> Option<String> {
        if let Some((row, nested)) = self.menu.take_submenu_selection() {
            self.menu.close();
            return self
                .nested
                .get(row)
                .and_then(|children| children.get(nested))
                .cloned()
                .flatten();
        }
        let index = self.menu.take_selection()?;
        self.menu.close();
        self.ids.get(index).cloned().flatten()
    }

    /// The labels of the rows shown, in order: what a test reads to see the
    /// menu a person sees.
    #[must_use]
    pub fn labels(&self) -> Vec<String> {
        if !self.menu.is_open() {
            return Vec::new();
        }
        self.menu
            .items()
            .iter()
            .filter(|item| !item.is_separator())
            .filter_map(|item| item.label().map(str::to_owned))
            .collect()
    }

    /// Whether a menu is up now.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.menu.is_open()
    }

    /// Where row `index` is painted, while the menu is open.
    #[must_use]
    pub fn item_rect(&self, index: usize) -> Option<Rect> {
        self.menu.item_rect(index)
    }
}

/// The menu item `row` draws as, and the row id choosing it sends: nothing
/// for a rule and for the row that opens its children as a submenu.
fn item_of(row: &Row) -> (MenuItem, Option<String>) {
    if row.separator {
        return (MenuItem::separator(), None);
    }
    let mut item = if row.children.is_empty() {
        MenuItem::selection(row.label.clone())
    } else {
        let children = row.children.iter().map(|child| item_of(child).0).collect();
        MenuItem::submenu(row.label.clone(), children)
    };
    if let Some(icon) = row.icon {
        item = item.with_icon(icon);
    }
    if let Some(shortcut) = &row.shortcut {
        item = item.with_shortcut(shortcut.as_str());
    }
    if !row.enabled {
        item = item.with_enabled(false);
        if let Some(reason) = &row.reason {
            item = item.with_detail(reason.clone());
        }
    }
    let choice = row.children.is_empty().then(|| row.id.clone());
    (item, choice)
}

/// The arguments of the session's one send action for what the box held: whom it goes to, what it
/// carries — the window or tab the menu stands on, as words — and the message.
fn send_arguments(request: ui::shared_selection::SendToRequest) -> serde_json::Map<String, Value> {
    let attached: Vec<Value> = request
        .about
        .iter()
        .filter_map(|about| about.to_attached(None, None))
        .filter_map(|attached| serde_json::to_value(attached).ok())
        .collect();
    let to = serde_json::to_value(&request.to).unwrap_or_else(|_| Value::Array(Vec::new()));
    let mut arguments = serde_json::Map::new();
    arguments.insert("to".to_owned(), to);
    arguments.insert("attached".to_owned(), Value::Array(attached));
    arguments.insert("text".to_owned(), Value::String(request.text));
    arguments
}

impl Widget for WindowMenuSurface {
    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        let followed = self.follow();
        self.deliver();
        followed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let menu = self.menu.build_layout(taffy, theme, map);
        // Over the whole host, out of its flow, like the other menus.
        let style = Style {
            position: ui::taffy::Position::Absolute,
            inset: ui::taffy::Rect {
                left: ui::taffy::LengthPercentageAuto::length(0.0),
                right: ui::taffy::LengthPercentageAuto::length(0.0),
                top: ui::taffy::LengthPercentageAuto::length(0.0),
                bottom: ui::taffy::LengthPercentageAuto::length(0.0),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &[menu], map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        // The menu stands within the surface's own area: the session places
        // that over the desk where the reading says.
        self.viewport = self.rect;
        let inner = Point::new(self.rect.x0, self.rect.y0);
        if let [menu] = taffy.children(node).unwrap_or_default()[..] {
            self.menu.apply_layout(taffy, menu, inner);
        }
        // A menu drawn before the host said how large the frame is was placed
        // against no room at all. Now that there is one, it stands against the
        // pointer again, inside the frame it draws in.
        if self.reading.is_open() {
            let anchor = self
                .reading
                .anchor
                .map_or(Rect::ZERO, |(x, y)| Rect::new(x, y, x, y));
            self.menu.reanchor(anchor, self.viewport);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let nodes = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        let [menu] = nodes[..] else {
            return;
        };
        let was_open = self.menu.is_open();
        if was_open {
            self.menu
                .handle_event(event, &mut ctx.reborrow(ctx.path, menu));
        }
        if let Some(id) = self.take_choice() {
            self.choose(&id);
        } else if was_open && !self.menu.is_open() {
            // The menu closed here without a row chosen: a press outside it.
            // The session hears the key that closes it but not this, so the
            // view says so, and the desk stops holding the keyboard.
            self.dismiss();
        }
        self.deliver();
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.menu]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.menu]
    }

    fn traps_focus(&self) -> bool {
        self.is_open()
    }

    fn name(&self) -> &'static str {
        "WindowMenuSurface"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ResourceSurface<WindowMenu, NoEffect> for WindowMenuSurface {
    fn present(
        &mut self,
        _state: &std::rc::Rc<<WindowMenu as ui::tilo_resource::Resource>::State>,
    ) {
    }

    fn attach(&mut self, cx: SurfaceCx<WindowMenu>) {
        self.wake = Some(cx.waker());
        // This host sends what its box holds through the session's one send action (`deliver`), so
        // its menu carries the shared Send to… row every other menu has.
        ui::shared_selection::set_send_host_available(true);
        self.ask();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A menu of two rows, one of which the window cannot choose.
    const READING: &str = r#"{
        "tab": "20",
        "anchor": { "x": 40, "y": 50 },
        "rows": [
            { "id": "window-normal", "label": "Normal", "enabled": true },
            {
                "id": "window-move-workspace",
                "label": "Move to workspace",
                "enabled": false,
                "reason": "There is no other workspace"
            }
        ]
    }"#;

    #[test]
    fn the_view_draws_the_rows_the_session_publishes() {
        let surface = WindowMenuSurface::showing(READING);
        assert!(surface.is_open());
        assert_eq!(
            surface.labels(),
            ["Normal", "Move to workspace"],
            "in the order the session lists them"
        );
        assert_eq!(
            surface.ids,
            [
                Some("window-normal".to_owned()),
                Some("window-move-workspace".to_owned())
            ]
        );
    }

    /// The view starts after the session published the menu, so its first
    /// frame has nothing to draw and the worker that opens the reading lands
    /// afterwards; a reading published before the attach wakes no view, and
    /// nothing else asks the menu for a frame. A view that does not wake
    /// itself is a menu nobody sees until some later press moves the desk.
    #[test]
    fn the_view_asks_for_a_frame_once_its_worker_lands() {
        let mut scope = ui::WakeScope::new();
        let (woken, woken_at) = std::sync::mpsc::channel();
        scope
            .bind(move |_| {
                let _ = woken.send(());
            })
            .expect("the view's wake is bound");
        let mut surface = WindowMenuSurface::new();
        surface.wake = Some(scope.handle());
        surface.ask();
        woken_at
            .recv_timeout(Duration::from_secs(10))
            .expect("the view is woken once its worker lands, and draws its reading");
    }

    /// The rows as the session publishes a window's menu: a rule between
    /// what covers the window and what carries it elsewhere, the places in a
    /// submenu, and each command's own picture.
    const GROUPED: &str = r#"{
        "tab": "20",
        "anchor": { "x": 40, "y": 50 },
        "rows": [
            {
                "id": "window-fullscreen",
                "label": "Full screen",
                "enabled": true,
                "icon": "fullscreen",
                "shortcut": "Super+F"
            },
            { "id": "", "label": "", "enabled": false, "separator": true },
            {
                "id": "window-move-to-strip",
                "label": "Move to strip",
                "enabled": true,
                "icon": "arrow-up-down",
                "children": [
                    {
                        "id": "window-move-to-strip:90",
                        "label": "Research",
                        "enabled": true
                    }
                ]
            },
            {
                "id": "window-close-window",
                "label": "Close this window",
                "enabled": true,
                "icon": "x"
            }
        ]
    }"#;

    #[test]
    fn the_view_draws_each_rows_picture_its_rule_and_its_submenu() {
        let surface = WindowMenuSurface::showing(GROUPED);
        let items = surface.menu.items();
        assert_eq!(
            items.len(),
            4,
            "a rule is an item of the menu, so the rows stay where the session put them"
        );
        assert!(
            !items[0].is_separator(),
            "the first group's row, not a rule before it"
        );
        assert!(items[1].is_separator(), "the rule between the groups");
        assert!(items[2].submenu_items().is_some(), "the places are nested");
        assert!(items[3].submenu_items().is_none());
        assert_eq!(
            surface.nested[2],
            [Some("window-move-to-strip:90".to_owned())],
            "the submenu's rows are the session's rows"
        );
        assert_eq!(
            surface.ids[2], None,
            "the submenu's own row chooses nothing"
        );
        assert_eq!(surface.ids[1], None, "and a rule chooses nothing");

        let icon = |item: &MenuItem| match item {
            MenuItem::Action { icon, .. } => *icon,
            _ => None,
        };
        assert_eq!(
            [
                icon(&items[0]),
                icon(&items[1]),
                icon(&items[2]),
                icon(&items[3])
            ],
            [
                Some(ui::icons::Icon::Fullscreen),
                None,
                Some(ui::icons::Icon::ArrowUpDown),
                Some(ui::icons::Icon::X)
            ],
            "every command draws the picture the session names, the rule none"
        );
        assert_eq!(
            items[0]
                .shortcut()
                .map(|chord| chord.label(ui::platform::PlatformConventions::new(
                    ui::platform::Platform::Linux
                ))),
            Some(ui::platform::Shortcut::from("Super+F").label(
                ui::platform::PlatformConventions::new(ui::platform::Platform::Linux)
            ))
        );
    }

    /// A row of a submenu is the choice: the session hears its id, not the
    /// submenu's.
    #[test]
    fn choosing_a_submenu_row_chooses_the_row_it_stands_for() {
        let mut surface = WindowMenuSurface::showing(GROUPED);
        assert!(surface.menu.choose_submenu_item(2, 0));
        assert_eq!(
            surface.take_choice(),
            Some("window-move-to-strip:90".to_owned())
        );
        assert!(!surface.menu.is_open(), "the menu closes on a choice");
    }

    /// A menu where one row has a shortcut and the other has none.
    const WITH_A_CHORD: &str = r#"{
        "tab": "20",
        "anchor": { "x": 40, "y": 50 },
        "rows": [
            {
                "id": "window-fullscreen",
                "label": "Full screen",
                "enabled": true,
                "shortcut": "Super+F"
            },
            { "id": "window-rename", "label": "Rename", "enabled": true }
        ]
    }"#;

    #[test]
    fn a_row_a_shortcut_does_shows_its_chord() {
        let surface = WindowMenuSurface::showing(WITH_A_CHORD);
        let conventions = ui::platform::PlatformConventions::new(ui::platform::Platform::Linux);
        let chords: Vec<Option<String>> = surface
            .menu
            .items()
            .iter()
            .map(|item| item.shortcut().map(|chord| chord.label(conventions)))
            .collect();
        let expected = ui::platform::Shortcut::from("Super+F").label(conventions);
        assert_eq!(
            chords,
            [Some(expected), None],
            "the chord the session sends stands at the row's end"
        );
    }

    /// A menu the length the desk publishes, naming its window.
    fn menu_of(rows: usize) -> String {
        let rows: Vec<Value> = (0..rows)
            .map(|i| {
                serde_json::json!({
                    "id": format!("window-row-{i}"),
                    "label": format!("Row {i}"),
                    "enabled": true,
                })
            })
            .collect();
        serde_json::json!({
            "tab": "20",
            "anchor": { "x": 40, "y": 50 },
            "subject": { "title": "Notes", "app": "so.tilo.notes" },
            "rows": rows,
        })
        .to_string()
    }

    #[test]
    fn the_menu_of_a_window_takes_typing_and_narrows_to_what_matches() {
        let mut surface = WindowMenuSurface::showing(&menu_of(14));
        assert!(surface.menu.takes_typing(), "a long menu can be typed into");
        surface.menu.set_query("row 13");
        let shown = surface.menu.shown_rows();
        let first = shown
            .first()
            .and_then(|&index| surface.menu.items().get(index))
            .and_then(MenuItem::label);
        assert_eq!(first, Some("Row 13"), "typing ranks the row it names first");
        assert!(shown.len() < 14, "and drops the rows it does not name");
    }

    #[test]
    fn the_reading_names_the_window_the_menu_is_on() {
        let surface = WindowMenuSurface::showing(&menu_of(2));
        assert_eq!(
            surface.reading.subject,
            Some(Subject {
                title: "Notes".into(),
                app: Some("so.tilo.notes".into()),
            })
        );
    }

    /// Lay the view out in the frame a host gives it, as the host does
    /// before it draws the surface a person looks at.
    fn lay_out(surface: &mut WindowMenuSurface, width: f32, height: f32) {
        let mut taffy = TaffyTree::new();
        let mut map = HashMap::new();
        let node = surface.build_layout(&mut taffy, &Theme::default(), &mut map);
        // The window's root fills the host, and the surface fills it.
        let root = taffy.fill_node(node);
        taffy
            .compute_layout(
                root,
                ui::taffy::Size {
                    width: ui::taffy::AvailableSpace::Definite(width),
                    height: ui::taffy::AvailableSpace::Definite(height),
                },
            )
            .expect("the frame lays out");
        surface.apply_layout(&taffy, node, Point::new(0.0, 0.0));
    }

    /// The menu a person sees is the rows in the frame the host draws: one
    /// the pointer opened stands at the pointer, and none of its rows falls
    /// outside the frame or is a sliver to aim at.
    #[test]
    fn the_rows_stand_in_the_frame_at_the_pointer() {
        let _settled = ui::motion::testing::settled();
        let mut surface = WindowMenuSurface::showing(READING);
        lay_out(&mut surface, 1280.0, 800.0);
        let frame = surface.get_rect();
        assert_eq!(frame, Rect::new(0.0, 0.0, 1280.0, 800.0), "the whole host");
        let rows: Vec<Rect> = (0..2)
            .map(|index| surface.item_rect(index).expect("the row is painted"))
            .collect();
        for row in &rows {
            assert!(
                frame.contains_rect(*row),
                "every row is in the frame: {row:?} in {frame:?}"
            );
            assert!(row.height() >= 20.0, "a row a person can aim at: {row:?}");
        }
        assert!(
            rows[0].x0 < 120.0,
            "the rows start at the pointer's own edge: {:?}",
            rows[0]
        );
        assert!(
            rows[0].y0 >= 50.0 && rows[0].y0 - 50.0 < 80.0,
            "the first row stands just below the pointer that opened the menu, \
             under the window the menu names: {:?}",
            rows[0]
        );
        assert!(
            rows[1].y0 >= rows[0].y1,
            "the rows stack in the order the session lists them: {rows:?}"
        );
    }

    /// The same rows, opened from the frame's last pixels: the corner of the
    /// desk the window menu doubles as the way out of.
    const AT_THE_CORNER: &str = r#"{
        "tab": "20",
        "anchor": { "x": 1272, "y": 792 },
        "rows": [
            { "id": "window-normal", "label": "Normal", "enabled": true },
            { "id": "window-close", "label": "Close", "enabled": true }
        ]
    }"#;

    /// A menu the session opened in the frame's last pixels is still drawn
    /// whole: its rows stand inside the frame, not past the edge a person
    /// cannot reach.
    #[test]
    fn a_menu_opened_at_the_frames_corner_stays_inside_it() {
        let _settled = ui::motion::testing::settled();
        let mut surface = WindowMenuSurface::showing(AT_THE_CORNER);
        lay_out(&mut surface, 1280.0, 800.0);
        let frame = surface.get_rect();
        let rows: Vec<Rect> = (0..2)
            .map(|index| surface.item_rect(index).expect("the row is painted"))
            .collect();
        for row in &rows {
            assert!(
                frame.contains_rect(*row),
                "every row is in the frame: {row:?} in {frame:?}"
            );
        }
        assert!(
            rows[1].y0 >= rows[0].y1 && rows[1].y1 <= 800.0,
            "the menu flips above the pointer when it has no room below: {rows:?}"
        );
    }

    #[test]
    fn a_reading_with_no_menu_up_draws_nothing() {
        let surface = WindowMenuSurface::showing(r#"{"tab":null,"anchor":null,"rows":[]}"#);
        assert!(!surface.is_open());
        assert!(surface.labels().is_empty());
    }

    #[test]
    fn a_reading_the_session_never_sent_draws_nothing() {
        let surface = WindowMenuSurface::showing("not a reading");
        assert!(!surface.is_open());
        assert!(surface.labels().is_empty());
    }

    /// The menu ends in the shared row every other menu has: the one the context menu adds itself,
    /// set off by its rule, before anything is typed. It is about the window the menu names, so
    /// what it sends is that window.
    #[test]
    fn the_menu_ends_in_the_shared_send_row() {
        ui::shared_selection::set_send_host_available(true);
        let surface = WindowMenuSurface::showing(&menu_of(3));
        let labels = surface.labels();
        assert_eq!(labels.len(), 4, "the window's three rows, then sending it");
        assert!(
            labels
                .last()
                .is_some_and(|last| last.starts_with("Send to")),
            "the send row stands last: {labels:?}"
        );
        assert!(
            surface
                .menu
                .items()
                .get(surface.menu.items().len() - 2)
                .is_some_and(MenuItem::is_separator),
            "set off from the window's own rows by its rule"
        );
        ui::shared_selection::set_send_host_available(false);
    }

    /// Sending is the session's own send: the recipients, the window the menu stands on as what the
    /// message carries, and its words.
    #[test]
    fn sending_carries_the_window_and_the_words_to_the_sessions_send() {
        let arguments = send_arguments(ui::shared_selection::SendToRequest {
            to: vec![ui::floating_surface::Recipient::new_agent("Ada")],
            about: vec![ui::shared_selection::SharedSelection::of_text("Notes")],
            text: "have a look".into(),
        });
        assert_eq!(arguments["text"], "have a look");
        assert_eq!(arguments["to"][0]["name"], "Ada");
        assert_eq!(arguments["to"][0]["kind"], "new-agent");
        assert_eq!(
            arguments["attached"][0]["kind"], "text",
            "the window is carried as words"
        );
        assert_eq!(arguments["attached"][0]["text"], "Notes");
    }
}
