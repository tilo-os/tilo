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
    rows: Vec<Row>,
}

/// One row a window offers.
#[derive(Clone, Debug, PartialEq)]
struct Row {
    id: String,
    label: String,
    enabled: bool,
    reason: Option<String>,
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
                })
            })
            .collect::<Option<Vec<Row>>>()?;
        Some(Self {
            tab: value.get("tab").and_then(Value::as_str).map(str::to_owned),
            anchor,
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
