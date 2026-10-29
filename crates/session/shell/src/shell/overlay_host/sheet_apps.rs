//! Sheets the desktop shows over everything, as ordinary apps
//! ([`tilo_sheets`]): the shell mounts each one's component in this process
//! and shows it over the display it opens on, as a layer of the overlay
//! stack. The app draws, takes its input and publishes its actions; the
//! shell only places it, routes the layer's input to it and carries out what
//! it asks of the shell through the seat it lent it.

use std::collections::HashMap;
use std::time::Instant;

use taffy::{NodeId, TaffyTree};
use tilo_sheets::Seat;
use ui::a11y::AccessNode;
use ui::application::ApplicationDefinition;
use ui::theme::Theme;
use ui::widget::{AppModel, AppWidget, ModelPath, Widget};
use vello::kurbo::{Point, Rect};
use winit::event::WindowEvent;

use super::layers::{
    LayerCx, LayerGeometry, LayerId, LayerPaint, Lifecycle, Routed, ShellLayer, Surface,
};

/// The instance a sheet's component is mounted as: past every tab id, so a
/// sheet never names a window.
const SHEET_INSTANCE: u64 = u64::MAX - 0x5_4ee7;

/// One sheet: what it is to the stack, the seat the shell lent its app, and
/// the app once mounted.
pub(crate) struct SheetHost<S> {
    id: LayerId,
    /// What kind of surface it is; its life is the presentation's.
    kind: Surface,
    definition: fn() -> &'static ApplicationDefinition,
    seat: S,
    app: Option<Box<dyn AppWidget>>,
    /// The room the app publishes its actions in.
    room: Option<String>,
    /// The app's node in the host's tree and the node placing it, while it
    /// is laid out.
    node: Option<NodeId>,
    window: Option<NodeId>,
    /// The display area it was last laid out over.
    area: Rect,
}

impl<S: Seat> SheetHost<S> {
    pub(crate) fn new(
        id: LayerId,
        kind: Surface,
        definition: fn() -> &'static ApplicationDefinition,
    ) -> Self {
        Self {
            id,
            kind,
            definition,
            seat: S::default(),
            app: None,
            room: None,
            node: None,
            window: None,
            area: Rect::ZERO,
        }
    }

    /// What the shell lent the app, to set what it offers and take what it
    /// asked for.
    pub(crate) fn seat(&self) -> &S {
        &self.seat
    }

    pub(crate) fn is_mounted(&self) -> bool {
        self.app.is_some()
    }

    /// Mount the app in `room` unless it is mounted there already; never
    /// while it is on screen. `mount` builds the component and its route
    /// to the session's broker, lent the seat.
    pub(crate) fn mount(
        &mut self,
        room: &str,
        mount: impl FnOnce(
            &'static ApplicationDefinition,
            &str,
            u64,
            S,
        ) -> Result<
            (
                ui::application::AppComponent,
                Option<ui::collaboration::CollaborationManager>,
            ),
            ui::launch::LaunchError,
        >,
    ) {
        if self.room.as_deref() == Some(room) || (self.app.is_some() && self.is_drawn()) {
            return;
        }
        let mounted = mount((self.definition)(), room, SHEET_INSTANCE, self.seat.clone()).and_then(
            |(component, collaboration)| {
                tilo_desktop::resolver::app_tile(component, collaboration, |_| {
                    crate::shell::wake_compositor();
                })
            },
        );
        match mounted {
            Ok(app) => {
                self.app = Some(app);
                self.room = Some(room.to_owned());
                self.node = None;
                self.window = None;
            }
            Err(error) => tracing::warn!(?self.id, %error, "a sheet could not be mounted"),
        }
    }

    /// Put the sheet up, from the key or the press that asked.
    pub(crate) fn open(&self) {
        let by_keyboard = matches!(
            ui::motion::input_source(),
            ui::tokens::MotionSource::Keyboard | ui::tokens::MotionSource::Action
        );
        self.seat.presentation().open(by_keyboard);
    }

    /// Take the sheet down; `true` when it was up.
    pub(crate) fn close(&self) -> bool {
        self.seat.presentation().close()
    }

    /// Take the sheet down at once: nothing of it is drawn any more.
    pub(crate) fn close_now(&self) {
        self.seat.presentation().gone();
    }

    /// Give `event` to the app, for a sheet the host routes itself rather
    /// than through the overlay stack.
    pub(crate) fn route(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(app) = self.app.as_mut() {
            let node = self.node.unwrap_or(ctx.node_id);
            let path = ctx.path;
            app.handle_event(event, &mut ctx.reborrow(path, node));
        }
    }

    /// Asked to be up, mounted or not yet.
    pub(crate) fn requested(&self) -> bool {
        self.seat.presentation().is_open()
    }

    /// Something of it is on screen.
    pub(crate) fn is_drawn(&self) -> bool {
        self.app.is_some() && self.seat.presentation().is_drawn()
    }

    /// The app's node over `area`, out of the desktop's flow, while it is
    /// drawn.
    pub(crate) fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        area: Rect,
        origin: Point,
    ) -> Option<NodeId> {
        self.node = None;
        self.window = None;
        if !self.is_drawn() {
            return None;
        }
        let app = self.app.as_mut()?;
        let node = app.build_layout(taffy, theme, node_map);
        let style = taffy::Style {
            position: taffy::Position::Absolute,
            inset: taffy::Rect {
                left: taffy::LengthPercentageAuto::length(ui::num::f32_of_f64(area.x0 - origin.x)),
                top: taffy::LengthPercentageAuto::length(ui::num::f32_of_f64(area.y0 - origin.y)),
                right: taffy::LengthPercentageAuto::auto(),
                bottom: taffy::LengthPercentageAuto::auto(),
            },
            size: taffy::Size {
                width: taffy::Dimension::length(ui::num::f32_of_f64(area.width())),
                height: taffy::Dimension::length(ui::num::f32_of_f64(area.height())),
            },
            ..taffy::Style::default()
        };
        let window = ui::widget::TaffyTreeExt::parent_node(taffy, style, &[node]);
        self.node = Some(node);
        self.window = Some(window);
        self.area = area;
        Some(window)
    }

    pub(crate) fn apply_layout(&mut self, taffy: &TaffyTree<()>, origin: Point) {
        if let (Some(app), Some(node), Some(window)) = (self.app.as_mut(), self.node, self.window) {
            let placed = ui::widget::layout_rect(taffy, window, origin);
            app.apply_layout(taffy, node, placed.origin());
        }
    }

    /// Whether the host's tree must be built again: the sheet came up or
    /// went away since it was laid out.
    pub(crate) fn structure_changed(&self) -> bool {
        self.is_drawn() != self.node.is_some()
    }

    /// The app, while it is laid out in the host's tree.
    pub(crate) fn widget(&self) -> Option<&dyn Widget> {
        self.node?;
        self.app.as_deref().map(|app| app as &dyn Widget)
    }

    pub(crate) fn widget_mut(&mut self) -> Option<&mut (dyn Widget + '_)> {
        self.node?;
        self.app.as_deref_mut().map(|app| app as &mut dyn Widget)
    }

    /// Service what the app's background work and its callers asked of it,
    /// on screen or not: an agent may invoke a sheet's action while it is
    /// down. Whether that changed what it shows.
    pub(crate) fn update(&mut self, doc: &AppModel, path: &ModelPath) -> bool {
        self.app.as_mut().is_some_and(|app| app.update(doc, path)) && self.is_drawn()
    }

    /// Where the sheet stands: the display area it was laid out over.
    pub(crate) fn area(&self) -> Rect {
        self.area
    }

    /// Offer `event` to the app, for a sheet the host routes itself at its
    /// own place in the dispatch; whether the app took it. A key that
    /// follows the opening at once reaches the app before the host has laid
    /// it out.
    pub(crate) fn offer_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let moved = matches!(event, WindowEvent::CursorMoved { .. });
        let was = *ctx.handled;
        // The pointer's moves always reach it, so it knows where a press
        // lands; anything else something before it took is not its.
        if !ShellLayer::is_open(self) || (was && !moved) {
            return false;
        }
        let Some(app) = self.app.as_mut() else {
            return false;
        };
        let node = self.node.unwrap_or(ctx.node_id);
        let path = ctx.path;
        *ctx.handled = false;
        app.handle_event(event, &mut ctx.reborrow(path, node));
        let took = *ctx.handled;
        *ctx.handled = was || took;
        took
    }

    /// Draw the app, for a sheet the host draws itself at its own place in
    /// the scene.
    pub(crate) fn draw_app(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if self.node.is_none() {
            return;
        }
        if let Some(app) = self.app.as_mut() {
            app.draw(scene, transform, text_renderer, theme, presence);
        }
    }
}

impl<S: Seat> ShellLayer for SheetHost<S> {
    fn id(&self) -> LayerId {
        self.id
    }

    fn surface(&self) -> Option<Surface> {
        Some(Surface {
            lifecycle: Lifecycle::of(ShellLayer::is_open(self)),
            ..self.kind.clone()
        })
    }

    /// Up and able to take input: asked for and mounted.
    fn is_open(&self) -> bool {
        self.app.is_some() && self.requested()
    }

    /// Every event is the app's while it is up: a sheet is modal, and what
    /// it does not use reaches nothing under it.
    fn handle_event(&mut self, event: &WindowEvent, cx: &mut LayerCx<'_, '_>) -> Routed {
        // A key that follows the opening at once reaches the app before the
        // host has laid it out; the app keeps no node of its own.
        if let Some(app) = self.app.as_mut() {
            let (path, node) = (cx.event.path, self.node.unwrap_or(cx.event.node_id));
            app.handle_event(event, &mut cx.event.reborrow(path, node));
        }
        Routed::Consumed
    }

    fn draw(&mut self, cx: &mut LayerPaint<'_>) {
        if self.node.is_none() {
            return;
        }
        if let Some(app) = self.app.as_mut() {
            app.draw(
                cx.scene,
                cx.transform,
                cx.text_renderer,
                cx.theme,
                cx.presence,
            );
        }
    }

    /// The app publishes its own tree, as a child of the host.
    fn accessibility(&self, _geometry: LayerGeometry<'_>) -> Option<AccessNode> {
        None
    }

    fn accessibility_rect(&self, _geometry: LayerGeometry<'_>, _path: &[usize]) -> Option<Rect> {
        None
    }

    fn focus_accessibility(&mut self, _path: &[usize]) -> bool {
        false
    }

    fn dismiss(&mut self) -> bool {
        self.close()
    }

    fn is_animating(&self, _now: Instant) -> bool {
        self.is_drawn()
            && (self.node.is_none() || self.app.as_ref().is_some_and(|app| app.needs_redraw()))
    }
}

/// Mount a sheet's app lent `seat`: through the session's in-process apps
/// when this host has them, so its actions are published on the session's
/// broker, else as this device's own person with no route beyond it.
///
/// # Errors
/// The app refuses the context.
pub(crate) fn mount_sheet<S: Seat>(
    apps: Option<&crate::shell::in_process::SharedInProcessApps>,
    definition: &'static ApplicationDefinition,
    room: &str,
    instance: u64,
    seat: S,
) -> Result<
    (
        ui::application::AppComponent,
        Option<ui::collaboration::CollaborationManager>,
    ),
    ui::launch::LaunchError,
> {
    match apps {
        Some(apps) => apps
            .borrow()
            .mount_sheet(definition, room, instance, |cx| cx.lend(seat)),
        None => crate::shell::in_process::mount_sheet(
            definition,
            &crate::shell::state::local_user_id(),
            "this-device",
            room,
            instance,
            |cx| cx.lend(seat),
        )
        .map(|component| (component, None)),
    }
}

impl super::ShellOverlayHost {
    /// The room the person works in, which sheets publish their actions in.
    pub(super) fn sheet_room(&self) -> String {
        self.chrome
            .as_ref()
            .and_then(|chrome| {
                chrome
                    .borrow()
                    .tree()
                    .desktop
                    .active_workspace()
                    .map(|workspace| workspace.room().to_string())
            })
            .unwrap_or_else(|| "desktop".to_owned())
    }

    /// The session's in-process apps, when this host has them.
    pub(super) fn sheet_apps(&self) -> Option<crate::shell::in_process::SharedInProcessApps> {
        let chrome = self.chrome.as_ref()?;
        let state = chrome.borrow();
        state.hosted_inputs.in_process().cloned()
    }
}
