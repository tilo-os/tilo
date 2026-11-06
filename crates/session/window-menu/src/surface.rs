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
            .map(|row| {
                Some(Row {
                    id: row.get("id")?.as_str()?.to_owned(),
                    label: row.get("label")?.as_str()?.to_owned(),
                    enabled: row.get("enabled")?.as_bool()?,
                    reason: row.get("reason").and_then(Value::as_str).map(str::to_owned),
                    shortcut: row
                        .get("shortcut")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
            })
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
    /// The row each item of the menu is for, as the reading listed it.
    ids: Vec<String>,
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
                reached.attach(wake);
                if let Ok(mut slot) = opened.lock() {
                    *slot = Some(reached);
                }
            }
            asking.store(false, Ordering::Relaxed);
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
            return;
        }
        let mut items = Vec::with_capacity(self.reading.rows.len());
        self.ids = Vec::with_capacity(self.reading.rows.len());
        for row in &self.reading.rows {
            let mut item = MenuItem::selection(row.label.clone());
            if let Some(shortcut) = &row.shortcut {
                item = item.with_shortcut(shortcut.as_str());
            }
            if !row.enabled {
                item = item.with_enabled(false);
                if let Some(reason) = &row.reason {
                    item = item.with_detail(reason.clone());
                }
            }
            items.push(item);
            self.ids.push(row.id.clone());
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

    /// The row the open menu was just asked for, if the person chose one.
    fn take_choice(&mut self) -> Option<String> {
        let index = self.menu.take_selection()?;
        self.menu.close();
        self.ids.get(index).cloned()
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

impl Widget for WindowMenuSurface {
    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        self.follow()
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
        assert_eq!(surface.ids, ["window-normal", "window-move-workspace"]);
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
}
