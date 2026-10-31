//! Who is here: the roster the bar's faces open, and every other person's
//! pointer where they are working.
//!
//! The roster reads the desk's collaborators, which the collaboration plane
//! writes each frame ([`crate::sync`]): the one feed the bar's faces and the
//! launcher's Happening now list read too. Everyone's pointer is the plane's
//! tile pointer, drawn over the tile of the tab they are on, in their own
//! colour.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_compositor::{HostKey, HostLayer, SurfaceHost};
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::navigation::{ActivityView, Command};
use tilo_surface_host::{Doors, Placement, Published, WidgetHost};
use ui::host_operation::ResourceOperationStart;
use ui::kurbo::{Affine, Point, Rect};
use ui::peniko::Color;
use ui::presence::PeerCursor;
use ui::taffy::{NodeId, Style, TaffyTree};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use ui::winit::event::WindowEvent;

use tilo_desktop::collaborators::{
    CollaboratorsHit, CollaboratorsModel, CollaboratorsOverlay, RosterRow,
    collaborator_roster_rows_named,
};

/// How long after the person pressed elsewhere a press on the faces does not
/// reopen the roster: their press closed it.
const DISMISS_GRACE: Duration = Duration::from_millis(250);

/// How long a wave this person sent shows on the row it went to.
const WAVE_SHOWN: Duration = Duration::from_millis(2400);

/// What the panel shows and where it stands, kept at rest between frames.
#[derive(Default)]
struct Shown {
    overlay: CollaboratorsOverlay,
    rows: Vec<RosterRow>,
    workspace: String,
    expanded: Option<String>,
    /// Whom the wave they sent goes to, and since when.
    waved: Option<(String, std::time::Instant)>,
    /// Where the pointer was over the desk: the panel opens under it.
    cursor: Option<Point>,
    /// Who else's pointer is over which tab, as the last frame drew them:
    /// their name and where they are.
    pointers: Vec<(String, String)>,
    /// Where the desk is drawn, as the surface last laid out.
    area: Rect,
}

/// The People panel and the other people's pointers: one surface above the
/// chrome, so the desk keeps its own drawing and the panel floats over it.
pub(crate) struct People {
    host: RefCell<WidgetHost>,
    shown: Rc<RefCell<Shown>>,
    open: Rc<Cell<bool>>,
    /// When the person last pressed elsewhere, closing the roster.
    dismissed: Cell<Option<std::time::Instant>>,
}

impl People {
    /// Open the roster, or close it again: what the bar's faces do.
    pub(crate) fn toggle(&self) {
        // Their press closed it a moment ago: this is that press, not another.
        let dismissed = self
            .dismissed
            .take()
            .is_some_and(|at| at.elapsed() < DISMISS_GRACE);
        if !dismissed {
            let open = !self.open.get();
            self.open.set(open);
            if !open {
                self.shown.borrow_mut().expanded = None;
            }
        }
        self.redraw();
    }

    /// Open the roster at the person `person`, their recent changes open: a
    /// press on them elsewhere (a wave in the bar, the launcher) lands here.
    pub(crate) fn show_person(&self, person: &str) {
        // A row this roster has not read yet still opens: the next frame
        // reads it and the row comes up expanded.
        self.shown.borrow_mut().expanded = Some(person.to_owned());
        self.open.set(true);
        self.dismissed.set(None);
        self.redraw();
    }

    /// Close the roster: the person pressed elsewhere.
    fn dismiss(&self) {
        let was_open = self.open.replace(false);
        if was_open {
            self.shown.borrow_mut().expanded = None;
            self.dismissed.set(Some(std::time::Instant::now()));
            self.redraw();
        }
    }

    /// Draw again, though no input reached the panel: it was opened or closed
    /// by the session.
    fn redraw(&self) {
        self.host.borrow_mut().relayout();
    }
}

impl SurfaceHost for People {
    fn layers(&self) -> Vec<HostLayer> {
        self.host.borrow().layers()
    }

    fn withholds_captures(&self) -> bool {
        self.host.borrow().withholds_captures()
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        self.host.borrow().accepts_pointer(x, y)
    }

    fn pointer_motion(&mut self, x: f64, y: f64) {
        self.host.borrow_mut().pointer_motion(x, y);
    }

    fn pointer_leave(&mut self) {
        self.host.borrow_mut().pointer_leave();
    }

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        self.host.borrow_mut().pointer_button(button, pressed);
    }

    fn pointer_axis(&mut self, horizontal: f64, vertical: f64) {
        self.host.borrow_mut().pointer_axis(horizontal, vertical);
    }

    fn pressed_elsewhere(&mut self) {
        self.dismiss();
        self.host.borrow_mut().pressed_elsewhere();
    }

    fn wants_keyboard(&self) -> bool {
        self.open.get()
    }

    fn key(&mut self, key: &HostKey) {
        self.host.borrow_mut().key(key);
    }

    fn prepare_frame(&mut self) -> bool {
        self.host.borrow_mut().prepare_frame()
    }

    fn set_backdrops(&mut self, images: Vec<Option<tilo_compositor::BackdropImage>>) {
        self.host.borrow_mut().set_backdrops(images);
    }

    fn outputs_changed(&mut self, outputs: &[(i32, i32, i32, i32)]) {
        self.host.borrow_mut().outputs_changed(outputs);
    }
}

impl Published for People {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        Vec::new()
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        _arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        Err(DoorError::Unknown(address.clone()))
    }

    /// Who is here, as the open roster lists them: each person's name, what
    /// they are doing, and where they are.
    fn accessible_labels(&mut self) -> Vec<String> {
        if !self.open.get() {
            return Vec::new();
        }
        let shown = self.shown.borrow();
        let snapshot = shown.overlay.accessibility_snapshot();
        let mut labels = Vec::new();
        let mut pending = vec![snapshot];
        while let Some(node) = pending.pop() {
            labels.extend(node.name.clone());
            labels.extend(node.value.clone());
            // What the row says of the person: where they are and how they
            // said they are.
            labels.extend(node.description.clone());
            pending.extend(node.children);
        }
        // Where everyone else is working: their pointer over a window of
        // this desk says so, beside their name.
        for (who, title) in &shown.pointers {
            labels.push(format!(
                "{who} — {}",
                tilo_desktop::collaborators::in_app(title)
            ));
        }
        labels.retain(|label| !label.trim().is_empty());
        labels
    }
}

/// The roster on the compositor's side, and the session's own handle of it.
pub(crate) fn mount(
    compositor: &mut tilo_compositor::Compositor,
    doors: &mut Doors,
    options: &crate::Options,
    handle: &DesktopHandle,
    sync: Option<Rc<crate::sync::Client>>,
) -> Rc<RefCell<People>> {
    let shown = Rc::new(RefCell::new(Shown::default()));
    let open = Rc::new(Cell::new(false));
    let panel = Panel {
        shown: Rc::clone(&shown),
        open: Rc::clone(&open),
        handle: handle.clone(),
        sync,
    };
    let placement = Placement {
        origin: (0, 0),
        size: (1, 1),
        scale: options.scale,
    };
    let mut host = WidgetHost::new(Box::new(panel), placement, false);
    host.set_theme((options.presentation)().theme);
    let host = host.covering_outputs().with_keyboard(Rc::clone(&open));
    let people = Rc::new(RefCell::new(People {
        host: RefCell::new(host),
        shown,
        open,
        dismissed: Cell::new(None),
    }));
    compositor.add_host(Box::new(Host(Rc::clone(&people))));
    doors.add(Rc::new(RefCell::new(Door(Rc::clone(&people)))));
    people
}

/// The panel as the compositor drives it.
struct Host(Rc<RefCell<People>>);

impl SurfaceHost for Host {
    fn layers(&self) -> Vec<HostLayer> {
        self.0.borrow().layers()
    }

    fn withholds_captures(&self) -> bool {
        self.0.borrow().withholds_captures()
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        self.0.borrow().accepts_pointer(x, y)
    }

    fn pointer_motion(&mut self, x: f64, y: f64) {
        self.0.borrow_mut().pointer_motion(x, y);
    }

    fn pointer_leave(&mut self) {
        self.0.borrow_mut().pointer_leave();
    }

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        self.0.borrow_mut().pointer_button(button, pressed);
    }

    fn pointer_axis(&mut self, horizontal: f64, vertical: f64) {
        self.0.borrow_mut().pointer_axis(horizontal, vertical);
    }

    fn pressed_elsewhere(&mut self) {
        self.0.borrow_mut().pressed_elsewhere();
    }

    fn wants_keyboard(&self) -> bool {
        self.0.borrow().wants_keyboard()
    }

    fn key(&mut self, key: &HostKey) {
        self.0.borrow_mut().key(key);
    }

    fn prepare_frame(&mut self) -> bool {
        self.0.borrow_mut().prepare_frame()
    }

    fn set_backdrops(&mut self, images: Vec<Option<tilo_compositor::BackdropImage>>) {
        self.0.borrow_mut().set_backdrops(images);
    }

    fn outputs_changed(&mut self, outputs: &[(i32, i32, i32, i32)]) {
        self.0.borrow_mut().outputs_changed(outputs);
    }
}

/// The roster as the session's door reaches it.
struct Door(Rc<RefCell<People>>);

impl Published for Door {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        self.0.borrow_mut().resources()
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ResourceOperationStart, DoorError> {
        self.0.borrow_mut().invoke(address, arguments)
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        self.0.borrow_mut().accessible_labels()
    }
}

/// The widget: the roster floating under the bar, and every other person's
/// pointer over the tile they are on.
struct Panel {
    shown: Rc<RefCell<Shown>>,
    open: Rc<Cell<bool>>,
    handle: DesktopHandle,
    sync: Option<Rc<crate::sync::Client>>,
}

impl Panel {
    /// Read who is here, as the plane wrote them into the desk's
    /// collaborators this frame: the roster, and the workspace it is of.
    fn read_roster(&self) -> (Vec<RosterRow>, String) {
        let own = self
            .sync
            .as_ref()
            .map_or_else(String::new, |sync| sync.publisher().to_string());
        let app_name = |_: &str| None;
        let mut tree = self.handle.read(Clone::clone);
        if let Some(sync) = &self.sync {
            sync.show(&mut tree, &app_name);
        }
        let (rows, _) = collaborator_roster_rows_named(&tree, &own, None);
        let workspace = tree
            .active_workspace()
            .map(|workspace| workspace.name.clone())
            .unwrap_or_default();
        (rows, workspace)
    }

    /// Act on what a press or a key on the roster meant.
    fn follow(&self, hit: CollaboratorsHit, shown: &mut Shown) {
        match hit {
            CollaboratorsHit::Dismiss => {
                shown.expanded = None;
                self.open.set(false);
            }
            CollaboratorsHit::ToggleDetails(index) => {
                let id = shown.rows.get(index).map(|row| row.id.clone());
                shown.expanded = shown
                    .expanded
                    .clone()
                    .filter(|open| Some(open) != id.as_ref())
                    .or(id);
            }
            CollaboratorsHit::ToggleFollow(index) => {
                let Some(id) = shown.rows.get(index).map(|row| row.id.clone()) else {
                    return;
                };
                let Some(sync) = &self.sync else {
                    return;
                };
                let mut own = sync.own();
                own.following = (own.following.as_deref() != Some(id.as_str())).then_some(id);
                sync.set_own(own);
            }
            CollaboratorsHit::Wave(index) => {
                let Some(id) = shown.rows.get(index).map(|row| row.id.clone()) else {
                    return;
                };
                if let Some(sync) = &self.sync {
                    sync.wave(&id);
                    shown.waved = Some((id, std::time::Instant::now()));
                }
            }
            // Their recent changes, where this workspace's history has them.
            CollaboratorsHit::OpenChanges(index) | CollaboratorsHit::OpenChange(index, _) => {
                let Some(author) = shown.rows.get(index).map(|row| row.id.clone()) else {
                    return;
                };
                self.open_activity(&author);
            }
            // No directory to invite from here (it is its own surface), no
            // call to join (a call is Chat's, in an app window), and nothing
            // else to do: the roster kept the press.
            CollaboratorsHit::Invite
            | CollaboratorsHit::JoinCall(_)
            | CollaboratorsHit::Pin
            | CollaboratorsHit::Absorb => {}
        }
    }

    /// Open this workspace's activity at what `author` changed.
    fn open_activity(&self, author: &str) {
        let workspace = self.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id.0.to_string())
        });
        let Some(workspace) = workspace else {
            return;
        };
        let command = Command::WorkspaceActivity {
            generation: crate::GENERATION.to_owned(),
            workspace,
            view: ActivityView::CatchUp,
            author: Some(author.to_owned()),
        };
        if let Err(error) = self.handle.dispatch(&command, crate::GENERATION) {
            tracing::debug!(%error, "activity could not open for that person");
        }
    }
}

impl Widget for Panel {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use ui::widget::TaffyTreeExt;
        let style = Style {
            position: ui::taffy::Position::Absolute,
            inset: ui::taffy::Rect {
                left: ui::taffy::LengthPercentageAuto::length(0.0),
                right: ui::taffy::LengthPercentageAuto::length(0.0),
                top: ui::taffy::LengthPercentageAuto::length(0.0),
                bottom: ui::taffy::LengthPercentageAuto::length(0.0),
            },
            ..Style::default()
        };
        taffy.widget_leaf(style, map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.shown.borrow_mut().area = ui::widget::layout_rect(taffy, node, origin);
    }

    fn get_rect(&self) -> Rect {
        self.shown.borrow().area
    }

    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn draw(
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
        let own_id = self
            .sync
            .as_ref()
            .map_or_else(String::new, |sync| sync.publisher().to_string());
        let mut shown = self.shown.borrow_mut();
        let open = self.open.get();
        // Where everyone else's pointer is now, from the plane: it names the
        // tabs their tiles hold it over.
        let over: Vec<(u64, String, f64, f64)> = self.sync.as_ref().map_or_else(Vec::new, |sync| {
            let Some((room, tabs)) = self.handle.read(|tree| {
                let workspace = tree.active_workspace()?;
                let tabs: Vec<u64> = workspace
                    .strips
                    .iter()
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                    .map(|tab| tab.id)
                    .collect();
                Some((workspace.room().to_string(), tabs))
            }) else {
                return Vec::new();
            };
            let mut over = Vec::new();
            for tab in tabs {
                for (publisher, x, y) in sync.pointers(&room, tab) {
                    over.push((tab, publisher, x, y));
                }
            }
            over
        });
        // Their names and colours are the roster's, read once for both the
        // rows and their pointers.
        if open || !over.is_empty() {
            let (rows, workspace) = self.read_roster();
            shown.rows = rows;
            if open {
                shown.workspace = workspace;
            }
        }
        if open {
            let following = self.sync.as_ref().map_or_else(Vec::new, |sync| {
                let target = sync.own().following.unwrap_or_default();
                if target.is_empty() {
                    Vec::new()
                } else {
                    vec![(own_id.clone(), target)]
                }
            });
            let followed = self.sync.as_ref().and_then(|sync| sync.own().following);
            let expanded = shown.expanded.clone();
            let workspace = shown.workspace.clone();
            let rows = shown.rows.clone();
            let waved = shown
                .waved
                .as_ref()
                .filter(|(_, at)| at.elapsed() < WAVE_SHOWN)
                .map(|(id, _)| id.clone())
                .into_iter()
                .collect::<Vec<_>>();
            let model = CollaboratorsModel {
                host: shown.area,
                workspace: &workspace,
                rows: &rows,
                unavailable: None,
                exposure: None,
                // Inviting is the directory's own surface here, and the
                // pinned overlay keeps the list on screen: neither is offered
                // from this panel yet.
                may_invite: false,
                may_pin: false,
                followed_id: followed.as_deref(),
                following_pairs: &following,
                expanded_id: expanded.as_deref(),
                watching: 0,
                waved: &waved,
                spectating: false,
            };
            let cursor = shown.cursor;
            shown
                .overlay
                .draw(scene, transform, text, theme, &model, cursor);
        }
        // Everyone else's pointer, over the tile of the tab they are on.
        let (cursors, places) = peer_cursors(&self.handle, &shown.rows, &over);
        shown.pointers = places;
        if !cursors.is_empty() {
            let area = shown.area;
            ui::presence::draw_peer_cursors(scene, transform, area, text, theme, &cursors);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if !self.open.get() {
            return;
        }
        let mut shown = self.shown.borrow_mut();
        let cursor = shown.cursor;
        let Some(hit) = shown.overlay.handle_event(event, ctx, true, cursor) else {
            return;
        };
        self.follow(hit, &mut shown);
    }

    fn name(&self) -> &'static str {
        "PeoplePanel"
    }

    fn test_id(&self) -> Option<&str> {
        Some("people-panel")
    }
}

/// Every other person's pointer over the desk now, in their own colour, at
/// the place the tile of their tab puts them: `over` names their tab, who
/// they are and where on that tile they are, and `roster` is what everyone
/// here is called and wears.
fn peer_cursors(
    handle: &DesktopHandle,
    roster: &[RosterRow],
    over: &[(u64, String, f64, f64)],
) -> (Vec<PeerCursor>, Vec<(String, String)>) {
    if over.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let tab_rects: HashMap<u64, Rect> = handle.shown_tabs().into_iter().collect();
    let titles: HashMap<u64, String> = handle.read(|tree| {
        tree.active_workspace()
            .into_iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .map(|tab| (tab.id, tab.title.clone()))
            .collect()
    });
    let who: HashMap<&str, (&str, Color)> = roster
        .iter()
        .map(|row| (row.id.as_str(), (row.label.as_str(), row.color)))
        .collect();
    let mut cursors = Vec::new();
    let mut places = Vec::new();
    for (tab, publisher, x, y) in over {
        let (label, colour) = who.get(publisher.as_str()).copied().unwrap_or_else(|| {
            let (r, g, b) = ui::presence::user_color(publisher);
            ("", Color::from_rgb8(r, g, b))
        });
        if let Some(title) = titles.get(tab) {
            places.push((label.to_owned(), title.clone()));
        }
        let Some(place) = tab_rects.get(tab) else {
            continue;
        };
        cursors.push(PeerCursor::new(
            (place.x0 + x, place.y0 + y),
            label.to_owned(),
            colour,
        ));
    }
    (cursors, places)
}
