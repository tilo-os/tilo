//! An [`App`] shown in a region of a host's scene: a tab of a desktop, a
//! pane of a device's screen. The app keeps everything a window of its own
//! would have (its input handling, focus, palette, history, collaboration
//! and the actions it publishes); the host only lays it out, hands it the
//! region's input and draws it where it sits.

use crate::host_core::HostCore as _;
use std::cell::{Cell, RefCell};

use super::{
    Affine, App, AppSurface, CursorIcon, HashMap, KurboRect, NodeId, Point, PresenceCtx, Scene,
    TaffyTree, TextRenderer, Theme, Widget, WindowEvent,
};
use crate::widget::{AppKind, AppWidget, IconDescriptor};

/// The region an [`EmbeddedApp`] occupies, as the [`AppSurface`] its app
/// runs on. Logical pixels, so the host's own transform scales the app.
struct Region {
    size: winit::dpi::PhysicalSize<u32>,
    redraw: Cell<bool>,
    cursor: Cell<Option<CursorIcon>>,
}

impl AppSurface for Region {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        self.size
    }
    fn request_redraw(&self) {
        self.redraw.set(true);
    }
    fn set_cursor(&self, cursor: CursorIcon) {
        self.cursor.set(Some(cursor));
    }
    fn hosts_tab_chords(&self) -> bool {
        true
    }
}

// How many frames an embedded app may run in one host frame to settle a
// sync or layout its previous frame left.
use crate::widget::SETTLE_FRAMES;

/// An app a host shows in a region of its own scene.
#[crate::component::not_a_component(
    framework,
    "an app shown in a region of its host; the language embeds with `embed`"
)]
pub struct EmbeddedApp {
    app: Box<App>,
    /// Its actions are offered to this device's automation only.
    device_local: bool,
    rect: KurboRect,
    region: Region,
    focused: bool,
    /// Focus state represented by `painted`. A hidden app can lose and regain
    /// focus without changing the frame its host last showed.
    painted_focus: bool,
    /// The control `painted` shows focused: focus that moved between
    /// controls, or into the app before its first control was laid out,
    /// needs a frame too.
    painted_control: Option<NodeId>,
    restore_focus: Option<NodeId>,
    /// Shift was held as focus last moved, so focus entering the app lands
    /// on its last control.
    entering_backwards: bool,
    /// The app asked for another frame.
    wants_frame: Cell<bool>,
    /// The frame the app last drew asked for the next one: it moves. A change
    /// it has yet to draw is not motion; the update or input that made it
    /// answers it with one frame.
    moving: Cell<bool>,
    /// Painted into the host's scene at the region's origin, and painted
    /// again as it is until the app has something new to show.
    painted: RefCell<Scene>,
    /// The damage parts ([`crate::scene_parts`]) recorded with `painted`.
    painted_parts: RefCell<Option<crate::scene_parts::SectionParts>>,
    /// Background work not yet serviced by a model pump or frame.
    woken: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The app's next wake deadline as it stood after it last ran
    /// ([`Self::wake_deadline`]), while `deadline_known`.
    deadline: Cell<Option<crate::time::Instant>>,
    /// The app has not run since `deadline` was asked for.
    deadline_known: Cell<bool>,
    /// The app ran since its host last found it with no intent for the host
    /// ([`AppWidget::take_host_intent`]): only an app that runs raises one.
    intents_owed: Cell<bool>,
    /// Tells this app's damage hints from every other source's.
    source: u64,
    /// Where the app's frames changed since `painted` was last replaced.
    damage: super::SurfaceDamage,
    /// How `painted` differs from what was painted before it: the
    /// generations it went from and to, and where it changed in the app's
    /// own coordinates, `None` when it cannot say.
    painted_damage: PaintedDamage,
    /// The host presents each frame over the pixels it last presented and
    /// can move them ([`Self::set_accepts_scroll_copies`]).
    accepts_copies: bool,
}

/// How an embedded app's painted scene differs from the one before it.
#[derive(Default)]
struct PaintedDamage {
    from: u64,
    to: u64,
    rects: Option<Vec<KurboRect>>,
    /// What moved instead, in the host's device pixels at `copy_scale` per
    /// unit of the app's own coordinates: where a scroll's content took its
    /// pixels from and how far it moved them.
    copies: Vec<crate::scroll_copy::DeviceCopy>,
    copy_scale: f64,
}

/// How many pixels of the target of the host drawing now
/// ([`crate::scroll_copy::host_device_scale`]) one unit of an app placed
/// with `placement` is; 1 when the placement scales the axes apart, rotates
/// or skews (the host then makes no copy of it anyway).
fn copy_scale(placement: Affine) -> f64 {
    let [a, b, c, d, _, _] = crate::scroll_copy::on_screen(placement).as_coeffs();
    if b == 0.0 && c == 0.0 && a > 0.0 && a.to_bits() == d.to_bits() {
        crate::scroll_copy::host_device_scale() * a
    } else {
        1.0
    }
}

/// Hands out each embedded app's [`crate::scene_parts::DamageHint::source`].
static NEXT_SOURCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl EmbeddedApp {
    /// Show `app` in a region of a host's scene. `wake` is told when the
    /// app's background work wants it to run again.
    pub fn new(
        mut app: App,
        wake: impl Fn(crate::wake::WakeEvent) + Send + Sync + 'static,
    ) -> Result<Self, crate::launch::LaunchError> {
        app.set_tab_order_continues_in_host(true);
        // The host has the one search; the tile opens none of its own.
        app.actions.search_in_host = true;
        let woken = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        if let Some(scope) = app.wake_scope.as_mut() {
            let flag = std::sync::Arc::clone(&woken);
            scope
                .bind(move |event| {
                    flag.store(true, std::sync::atomic::Ordering::Release);
                    wake(event);
                })
                .map_err(|error| crate::launch::LaunchError::Operation(error.to_string()))?;
        }
        if app.wake_scope.is_some() {
            // As in a window of its own: a change of an outside authority
            // the app's permissions follow wakes it to show it.
            let _entry = app.enter_wake_scope();
            crate::permissions::wake_on_outside_change(crate::wake::WakeHandle::new());
        }
        Ok(Self {
            app: Box::new(app),
            rect: KurboRect::ZERO,
            region: Region {
                size: winit::dpi::PhysicalSize::new(0, 0),
                redraw: Cell::new(true),
                cursor: Cell::new(None),
            },
            focused: false,
            painted_focus: false,
            painted_control: None,
            restore_focus: None,
            entering_backwards: false,
            wants_frame: Cell::new(true),
            moving: Cell::new(false),
            painted: RefCell::new(Scene::new()),
            painted_parts: RefCell::new(None),
            woken,
            deadline: Cell::new(None),
            deadline_known: Cell::new(false),
            intents_owed: Cell::new(true),
            source: NEXT_SOURCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            damage: super::SurfaceDamage::default(),
            painted_damage: PaintedDamage::default(),
            accepts_copies: false,
            device_local: false,
        })
    }

    /// Offer the app's actions to its own device's automation
    /// ([`AppWidget::device_local_resources`]): for a private window, which
    /// has no collaboration route that would publish them.
    #[must_use]
    pub fn with_device_local_actions(mut self) -> Self {
        self.device_local = true;
        self
    }

    /// Whether the host keeps the pixels it presented and can move them
    /// within its target before it repaints the damage: a scroll in the app
    /// then hands it what moved ([`crate::scene_parts::DamageHint::copies`])
    /// instead of damaging the viewport. The app then stands on an opaque
    /// ground of the theme's content colour, as a window of its own stands on
    /// its target's, so what is under its content is uniform; a theme whose
    /// content colour is not opaque gets no copies.
    pub fn set_accepts_scroll_copies(&mut self, accepts: bool) {
        self.accepts_copies = accepts;
    }

    /// The ground the app stands on while it hands out copies, if it does.
    fn copy_ground(&self) -> Option<vello::peniko::Color> {
        let ground = self.app.theme.tokens.colors.surfaces.content;
        (self.accepts_copies && ground.components[3] >= 1.0).then_some(ground)
    }

    /// The app shown.
    pub fn app(&self) -> &App {
        &self.app
    }

    /// What the last painted frame handed its host, for a test reading why a
    /// change repainted more than it should: the generations, the damage
    /// (or none), why the app last repainted whole, and its tooltip.
    #[cfg(any(test, feature = "test-support"))]
    pub fn damage_report_for_test(&self) -> String {
        format!(
            "{}->{} damage={:?} copies={:?} repainted={:?} tooltip={:?} tooltip_pending={}",
            self.painted_damage.from,
            self.painted_damage.to,
            self.painted_damage.rects,
            self.painted_damage.copies,
            self.damage.repainted(),
            self.app.input.tooltip_host.visible(),
            self.app.input.tooltip_host.needs_frame(),
        )
    }

    /// `event`, in the region's own logical pixels, as the host hands it
    /// input, and a frame for it: what a benchmark or test drives the app
    /// with without a host's layout and focus.
    #[cfg(any(test, feature = "test-support"))]
    pub fn input_for_test(&mut self, event: WindowEvent) {
        self.runs();
        self.app.surface_input(&self.region, event, false);
        self.wants_frame.set(true);
    }

    /// Serve the app's shell-ui connection as its native runner does
    /// between frames, and draw it again when that changed what it shows:
    /// for a host that runs the app as a window of its own and draws it in
    /// a frame of its own (the screenshot harness). Returns whether it
    /// changed.
    #[cfg(feature = "test-support")]
    pub fn pump_shell_connection_for_test(&mut self) -> bool {
        self.runs();
        let changed = self.app.test_pump_floating_service();
        if changed {
            self.app.layout.layout_dirty = true;
            self.wants_frame.set(true);
        }
        changed
    }

    /// The app shown, to drive it.
    /// When the app next wants to run with nothing waking it. Its widgets
    /// change their deadlines only as the app runs (a pump, a frame, an
    /// input), so the tree is asked once after each run, not on every
    /// pump and frame of its host.
    fn wake_deadline(&self) -> Option<crate::time::Instant> {
        if self.deadline_known.get() {
            return self.deadline.get();
        }
        // A bound component's permissions changing with time is a deadline
        // too, as in a window of its own.
        let deadline = self
            .app
            .root_widget
            .next_wake_deadline()
            .into_iter()
            .chain(self.app.permissions_change_at())
            .min();
        self.deadline.set(deadline);
        self.deadline_known.set(true);
        deadline
    }

    /// Whether focus differs from what was last painted: the app gained or
    /// lost it, or it moved to another control.
    fn focus_moved(&self) -> bool {
        self.focused != self.painted_focus || self.app.focused_control() != self.painted_control
    }

    /// The app runs: its deadlines may move.
    fn runs(&self) {
        self.deadline_known.set(false);
        self.intents_owed.set(true);
    }

    pub fn app_mut(&mut self) -> &mut App {
        self.runs();
        &mut self.app
    }

    /// Build the app's tree when it has none yet, or, with `restructured`,
    /// when its structure changed since its last frame. The app is otherwise
    /// built as it is drawn, and focus or a key its host hands it before
    /// then (the key typed right after the shortcut that showed it) finds no
    /// control to land on.
    fn build_tree(&mut self, restructured: bool) {
        if self.app.layout.root_node.is_some() && !restructured {
            return;
        }
        self.runs();
        if self.app.surface_build_tree(&mut self.damage) {
            self.app.mark_semantic_tree_stale();
            self.wants_frame.set(true);
        }
    }

    /// The control with the keyboard moved from `before`: the tree the
    /// host's accessibility walk reads says so now. The host publishes that
    /// tree in the tick the focus moved, before any frame paints, and a
    /// reader left on the old control (or on the app as a whole) would
    /// stay there until something else repainted the app.
    fn follow_focus_in_semantics(&mut self, before: Option<NodeId>) {
        if self.app.focused_control() != before {
            let _ = self.app.refresh_semantic_tree();
        }
    }

    /// Give the app keyboard focus from its host's focus order, coming
    /// from after it with `backwards` (Shift+Tab): focus lands on its last
    /// control then, else on its first.
    pub fn enter_focus(&mut self, backwards: bool) {
        self.entering_backwards = backwards;
        Widget::set_focus(self, true);
    }

    /// The root widget of the app's component: what an app's own tests
    /// reach into, as they would in the app's window.
    pub fn component_root_mut(&mut self) -> Option<&mut dyn Widget> {
        self.runs();
        // Whatever the caller does to it is no change the tracking sees.
        self.app.mark_semantic_tree_stale();
        let root = self
            .app
            .root_widget
            .as_any_mut()?
            .downcast_mut::<crate::widget::BoundAppWidget>()?;
        let widget: &mut dyn Widget = root.component.root.as_deref_mut()?;
        Some(widget)
    }

    /// `event` from the host, in its physical pixels at `scale`, moved into
    /// the region's logical pixels.
    fn local_event(&self, event: &WindowEvent, scale: f64) -> WindowEvent {
        let local = |x: f64, y: f64| {
            winit::dpi::PhysicalPosition::new(x / scale - self.rect.x0, y / scale - self.rect.y0)
        };
        match event {
            WindowEvent::CursorMoved {
                device_id,
                position,
            } => WindowEvent::CursorMoved {
                device_id: *device_id,
                position: local(position.x, position.y),
            },
            WindowEvent::Touch(touch) => {
                let mut touch = *touch;
                touch.location = local(touch.location.x, touch.location.y);
                WindowEvent::Touch(touch)
            }
            other => other.clone(),
        }
    }

    fn app_widget(&self) -> Option<&dyn AppWidget> {
        self.app
            .root_widget
            .as_any()?
            .downcast_ref::<crate::widget::BoundAppWidget>()
            .map(|root| root as &dyn AppWidget)
    }

    fn app_widget_mut(&mut self) -> Option<&mut dyn AppWidget> {
        self.runs();
        self.app.mark_semantic_tree_stale();
        self.app
            .root_widget
            .as_any_mut()?
            .downcast_mut::<crate::widget::BoundAppWidget>()
            .map(|root| root as &mut dyn AppWidget)
    }
}

impl crate::widget::WindowHistory for EmbeddedApp {
    fn set_window_presents_history(&mut self, tab_presents: bool) {
        self.runs();
        self.app.set_window_presents_history(tab_presents);
    }

    fn window_history_control(&self) -> Option<crate::widget::HistoryControl> {
        self.app.window_history_control()
    }

    fn invoke_window_history_control(
        &mut self,
        control: crate::widget::HistoryControl,
    ) -> Result<(), String> {
        self.runs();
        self.app.invoke_window_history_control(control)?;
        self.wants_frame.set(true);
        Ok(())
    }
}

impl crate::widget::SearchableWindow for EmbeddedApp {
    fn visit_search_root(&mut self, visit: &mut dyn FnMut(&mut dyn Widget) -> bool) -> bool {
        self.runs();
        let changed = visit(self.app.root_widget.as_mut());
        if changed {
            // What the search found shows on the app's next frame: matches
            // painted, a match scrolled into view, a list narrowed. A
            // replacement is an edit the app's widgets write when they
            // settle, so that frame syncs.
            self.app.mark_semantic_tree_stale();
            self.app.layout.layout_dirty = true;
            self.app.needs_sync = true;
            self.wants_frame.set(true);
        }
        changed
    }
}

impl Widget for EmbeddedApp {
    crate::provides!(
        dyn crate::component_lifecycle::ComponentLifecycle,
        dyn crate::widget::CompleteContentProvider,
        dyn crate::widget::WindowHistory,
        dyn crate::widget::SearchableWindow
    );

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.runs();
        // The app looks as the host does around it. A new palette must
        // replace its retained scene even when the region's size is unchanged.
        if self.app.accessibility.base_theme != *theme {
            self.app.set_theme(theme.clone());
            self.wants_frame.set(true);
        }
        // Mounted, the app has its controls before it is first painted.
        self.build_tree(false);
        let style = taffy::Style {
            size: taffy::Size::from_percent(1.0, 1.0),
            ..taffy::Style::default()
        };
        let Ok(node) = taffy.new_leaf(style) else {
            return NodeId::new(0);
        };
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.runs();
        let rect = crate::widget::layout_rect(taffy, node, origin);
        if rect.size() != self.rect.size() {
            self.app.layout.layout_dirty = true;
            self.wants_frame.set(true);
        }
        self.rect = rect;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a region's logical size is a small non-negative pixel count"
        )]
        {
            self.region.size = winit::dpi::PhysicalSize::new(
                rect.width().round().max(0.0) as u32,
                rect.height().round().max(0.0) as u32,
            );
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let mut painted = self.painted.borrow_mut();
        let mut painted_parts = self.painted_parts.borrow_mut();
        let due = self
            .wake_deadline()
            .is_some_and(|deadline| deadline <= crate::time::now());
        let woken = self.woken.swap(false, std::sync::atomic::Ordering::AcqRel);
        // Drawn for a host that copies, the app hands out copies from now
        // on (a capture drawing it later repaints where they land).
        if crate::scroll_copy::host_copies() {
            self.accepts_copies = true;
        }
        // Starting or stopping copies changes the ground under the app and
        // what its damage describes: the next frame is repainted whole.
        let ground = self.copy_ground();
        if ground.is_some() != self.damage.is_copying() {
            self.damage = if ground.is_some() {
                super::SurfaceDamage::copying()
            } else {
                super::SurfaceDamage::default()
            };
            self.wants_frame.set(true);
        }
        let placement = transform * Affine::translate((self.rect.x0, self.rect.y0));
        if ground.is_some() {
            // Copies are planned in the host's device pixels: at 200 % a
            // step of half a logical pixel is a whole one and copies.
            self.damage.set_copy_scale(copy_scale(placement));
        }
        if self.wants_frame.get() || woken || due || self.focus_moved() {
            self.runs();
            // A frame whose sync or layout leaves more to settle runs again
            // at once, so what the host paints is the app at rest.
            let mut again = true;
            let from = self.painted_damage.to;
            for _ in 0..SETTLE_FRAMES {
                // The app's own damage parts travel with its scene, so a
                // host that tracks parts repaints a caret, not the tile.
                let region = KurboRect::new(
                    0.0,
                    0.0,
                    f64::from(self.region.size.width),
                    f64::from(self.region.size.height),
                );
                let (settling, parts) = crate::scene_parts::record(&mut painted, |painted| {
                    painted.reset();
                    if let Some(ground) = ground {
                        painted.fill(
                            vello::peniko::Fill::NonZero,
                            Affine::IDENTITY,
                            ground,
                            None,
                            &region,
                        );
                    }
                    self.app
                        .surface_frame_with_damage(&self.region, painted, &mut self.damage)
                });
                again = settling;
                *painted_parts = parts;
                if !again || !self.app.unsettled() {
                    break;
                }
            }
            // The frames just run changed only where their damage says,
            // from what was painted before them.
            let taken = self.damage.take_copying();
            self.painted_damage = PaintedDamage {
                from,
                to: from.wrapping_add(1),
                copies: taken
                    .as_ref()
                    .map(|(copies, _)| copies.clone())
                    .unwrap_or_default(),
                copy_scale: self.damage.copy_scale(),
                rects: taken.map(|(_, rects)| rects),
            };
            // What the host's accessibility walk reads of the app.
            let _ = self.app.refresh_semantic_tree();
            let next = again || self.region.redraw.replace(false);
            self.wants_frame.set(next);
            self.moving.set(next);
            self.painted_focus = self.focused;
            self.painted_control = self.app.focused_control();
        }
        // A host comparing this frame with one that painted the generation
        // before repaints only the app's damage; any other change to the
        // app (it moved, it repainted whole, frames were skipped) repaints
        // the part around it.
        let hint = self.painted_damage.rects.as_ref().map(|rects| {
            let mut rects: Vec<KurboRect> = rects
                .iter()
                .map(|rect| placement.transform_rect_bbox(*rect).inflate(1.0, 1.0))
                .collect();
            // The app plans copies in the host's device pixels.
            let mut copies = Vec::with_capacity(self.painted_damage.copies.len());
            for copy in &self.painted_damage.copies {
                let moved =
                    crate::scroll_copy::MovedRegion::of(copy, self.painted_damage.copy_scale);
                match moved.transformed(placement) {
                    Some(moved) => copies.push(moved),
                    None => rects.push(
                        placement
                            .transform_rect_bbox(moved.destination())
                            .inflate(1.0, 1.0),
                    ),
                }
            }
            crate::scene_parts::DamageHint {
                source: self.source,
                from: self.painted_damage.from,
                to: self.painted_damage.to,
                placement,
                rects,
                copies,
                earlier: Vec::new(),
            }
        });
        let key = crate::scene_parts::PartKey::new("ui.embedded-app", ());
        crate::scene_parts::hinted(scene, key, hint, |scene| {
            crate::scene_parts::append(scene, &painted, painted_parts.as_ref(), Some(placement));
        });
    }

    /// A due wake deadline is serviced here, on the host's pump, as a window
    /// of the app's own services it after its redraw is skipped: the app's
    /// model runs (which samples a record still being written again) and
    /// nothing is drawn unless that showed something new. Only `draw` ran the
    /// app before, so a deadline that came due after its last frame stayed in
    /// the past, and a host that does not paint for a due deadline woke for it
    /// at once on every park.
    fn update(
        &mut self,
        _state: &crate::widget::AppModel,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        let due = self
            .wake_deadline()
            .is_some_and(|deadline| deadline <= crate::time::now());
        // A wake the app raised from another thread (a question put to its
        // action directory, an invocation, a collaborator's edit) is serviced
        // here too, not only by the next frame: the host may park without
        // painting it, and an answer owed to a waiting caller would then wait
        // for something unrelated. Consume it before pumping: a nonvisual
        // answer needs no draw to retire, and a wake raised during the pump
        // must remain pending. Visible changes are retained by wants_frame.
        let woken = self.woken.swap(false, std::sync::atomic::Ordering::AcqRel);
        if !due && !woken {
            return false;
        }
        self.runs();
        let presentation_changed = self.app.pump_embedded_model();
        // A durability that changed in this pass left the layout dirty
        // (`App::sync_model`), so the next frame shows it; a structure change
        // the pass made is rebuilt by that frame.
        self.app.layout.structure_dirty |= self.app.take_root_structure_dirty();
        let visible = presentation_changed
            || self.app.unsettled()
            || (woken && self.app.root_widget.needs_redraw())
            || self.app.root_widget.needs_animation_frame()
            || self.app.permissions_walk_due(crate::time::now())
            || crate::motion::frames_wanted(crate::time::now());
        if visible {
            self.wants_frame.set(true);
        }
        visible
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.runs();
        self.build_tree(false);
        self.entering_backwards = ctx.modifiers.state().shift_key();
        let local = self.local_event(event, ctx.scale_factor);
        self.app.set_surface_modifiers(*ctx.modifiers);
        let control = self.app.focused_control();
        if self.app.surface_input(&self.region, local, false) {
            *ctx.handled = true;
        }
        self.follow_focus_in_semantics(control);
        if self.region.redraw.replace(false) {
            self.wants_frame.set(true);
        }
    }

    fn name(&self) -> &'static str {
        "EmbeddedApp"
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn test_id(&self) -> Option<&str> {
        self.app
            .root_widget
            .as_any()?
            .downcast_ref::<crate::widget::BoundAppWidget>()
            .map(|root| root.component.app_id())
    }

    fn get_rect(&self) -> KurboRect {
        self.rect
    }

    /// The app's own named targets, where they are in the host.
    fn test_rects(&self) -> Vec<(String, KurboRect)> {
        fn walk(
            widget: &dyn Widget,
            offset: vello::kurbo::Vec2,
            out: &mut Vec<(String, KurboRect)>,
        ) {
            if let Some(id) = widget.test_id() {
                out.push((id.to_owned(), widget.get_rect() + offset));
            }
            out.extend(
                widget
                    .test_rects()
                    .into_iter()
                    .map(|(name, rect)| (name, rect + offset)),
            );
            for child in widget.children() {
                walk(child, offset, out);
            }
        }
        let mut out = Vec::new();
        walk(
            self.app.root_widget.as_ref(),
            vello::kurbo::Vec2::new(self.rect.x0, self.rect.y0),
            &mut out,
        );
        out.retain(|(_, rect)| rect.area() > 0.0);
        out
    }

    fn cursor_shape(&self, _point: Point) -> Option<CursorIcon> {
        self.region.cursor.get()
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.runs();
        if focused == self.focused {
            return;
        }
        self.focused = focused;
        if focused {
            // Focus lands on the controls the app shows now, though no frame
            // has painted them yet.
            self.build_tree(true);
            if !self.app.restore_focused_control(self.restore_focus.take())
                || self.app.focused_control().is_none()
            {
                self.app.focus_order_edge(self.entering_backwards);
            }
        } else {
            self.restore_focus = self.app.focused_control();
            self.app.restore_focused_control(None);
        }
        // The reader follows the keyboard into (or out of) the app in the
        // tick it moved, as `follow_focus_in_semantics` says.
        let _ = self.app.refresh_semantic_tree();
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn wants_tab_input(&self) -> bool {
        true
    }

    fn ime_cursor_rect(&self) -> Option<KurboRect> {
        self.app
            .inspect_node(self.app.focused_node?, |widget| widget.ime_cursor_rect())
            .ok()
            .flatten()
            .map(|rect| rect + vello::kurbo::Vec2::new(self.rect.x0, self.rect.y0))
    }

    fn ime_content(&self) -> Option<crate::ime::InputContent> {
        self.app.ime_window_state().map(|field| field.content)
    }

    fn ime_multiline(&self) -> bool {
        self.app
            .ime_window_state()
            .is_some_and(|field| field.content.hints.multiline)
    }

    fn ime_requires_deferred_ack(&self) -> bool {
        self.app
            .focused_node
            .and_then(|node| {
                self.app
                    .inspect_node(node, |widget| widget.ime_requires_deferred_ack())
                    .ok()
            })
            .unwrap_or(false)
    }

    fn ime_refuses_ordinary_commits(&self) -> bool {
        self.app
            .focused_node
            .and_then(|node| {
                self.app
                    .inspect_node(node, |widget| widget.ime_refuses_ordinary_commits())
                    .ok()
            })
            .unwrap_or(false)
    }

    fn start_deferred_text(&mut self) -> Option<crate::webview::text_input::Receipt> {
        self.runs();
        self.app
            .mutate_node(self.app.focused_node?, |widget| {
                widget.start_deferred_text()
            })
            .ok()
            .flatten()
    }

    fn commit_deferred_text(&mut self, text: &str) -> Option<crate::webview::text_input::Receipt> {
        self.runs();
        self.app
            .mutate_node(self.app.focused_node?, |widget| {
                widget.commit_deferred_text(text)
            })
            .ok()
            .flatten()
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        let node = self.app.focused_node?;
        let field = self
            .app
            .inspect_node(node, |widget| widget.ime_edit_fingerprint(key))
            .ok()??;
        let document = self
            .app
            .bindings
            .document_edit_stamp(node, &self.app.state.doc)
            .ok()?;
        Some(document.bind_fingerprint(node, field, key))
    }

    fn needs_redraw(&self) -> bool {
        self.wants_frame.get() || self.focus_moved()
    }

    /// The app is moving while each frame it draws asks for the next, as a
    /// window of its own is redrawn for.
    fn needs_animation_frame(&self) -> bool {
        self.moving.get()
    }

    fn has_pending_visual_work(&self) -> bool {
        self.app.root_widget.has_pending_visual_work()
    }

    fn has_transient_feedback(&self) -> bool {
        self.app.root_widget.has_transient_feedback()
    }

    /// The host's presentation preferences are the app's.
    fn set_motion_reduced(&mut self, reduced: bool) {
        self.runs();
        self.app.set_reduce_motion(reduced);
    }

    fn set_platform_conventions(&mut self, conventions: crate::platform::PlatformConventions) {
        self.runs();
        self.app.set_platform_conventions(conventions);
    }

    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        self.runs();
        self.app.set_interaction_environment(environment);
    }

    /// The app's own accessibility tree, as this tile's subtree: a screen
    /// reader walks from the host straight into the app. Each control keeps
    /// the name the app reaches it by (the target its widget publishes, or
    /// its test id) as its action target, so the host, its agents and its
    /// tests name a control in the app as they name one of their own.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let tree = self.app.semantic_tree();
        let root = tree.node(tree.root())?;
        let mut node = crate::a11y::AccessNode::new(crate::a11y::Role::Group);
        node.name = AppWidget::app_title(self).to_owned().into();
        node.children = root
            .children
            .iter()
            .enumerate()
            .filter_map(|(ordinal, child)| self.access_subtree(child, ordinal, &[]))
            .collect();
        Some(node)
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        self.runs();
        let Some(id) = self.hosted_node_at(key_path) else {
            return false;
        };
        self.perform_hosted(&id, action)
    }

    /// An action on a control of the app named as [`Self::accessibility`]
    /// publishes it: by its target or test id, or by its key path where it
    /// has neither.
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        self.runs();
        let id = match key_route(target) {
            Some(keys) => self.hosted_node_at(&keys),
            None => self.hosted_node_named(target),
        };
        let Some(id) = id else {
            return false;
        };
        self.perform_hosted(&id, action)
    }

    fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        self.wake_deadline()
    }
}

impl crate::component_lifecycle::ComponentLifecycle for EmbeddedApp {
    fn unsaved_documents(
        &self,
        doc: &automerge::AutoCommit,
    ) -> Vec<crate::close_admission::UnsavedDocument> {
        crate::component_lifecycle::unsaved_documents(self.app.root_widget.as_ref(), doc)
    }
    fn checkpoint_components(&mut self) -> Vec<String> {
        self.runs();
        crate::component_lifecycle::checkpoint_components(self.app.root_widget.as_mut())
    }
    fn finish_admitted_close(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        self.runs();
        crate::component_lifecycle::finish_admitted_close(self.app.root_widget.as_mut())
    }
    fn close_components(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        self.runs();
        self.app.close_components();
        self.app.take_component_close_reports()
    }
    fn take_component_close_reports(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        self.runs();
        self.app.take_component_close_reports()
    }
}

impl crate::widget::CompleteContentProvider for EmbeddedApp {
    fn complete_content_size(&self) -> Option<vello::kurbo::Size> {
        use crate::widget::CompleteContentWidgetExt as _;
        self.app.root_widget.complete_content_size()
    }

    fn draw_complete_content(
        &mut self,
        scene: &mut Scene,
        bounds: KurboRect,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) -> bool {
        self.runs();
        use crate::widget::CompleteContentWidgetExt as _;
        let app = &mut self.app;
        let local = bounds - vello::kurbo::Vec2::new(self.rect.x0, self.rect.y0);
        let mut painted = Scene::new();
        let drawn = app.root_widget.draw_complete_content(
            &mut painted,
            local,
            Affine::IDENTITY,
            &mut app.text_renderer,
            &app.theme,
            &app.presence_ctx,
        );
        scene.append(
            &painted,
            Some(transform * Affine::translate((self.rect.x0, self.rect.y0))),
        );
        drawn
    }
}

impl AppWidget for EmbeddedApp {
    fn device_local_resources(&self) -> Option<Vec<tilo_actions::ResourceSpec>> {
        self.device_local.then(|| {
            crate::widget::published_resources(self.app.root_widget.as_ref(), &self.app.state.doc)
        })
    }
    fn window_search(&self) -> Option<crate::widget::WindowPublication> {
        let root = self.app.root_widget.as_ref();
        Some(crate::widget::WindowPublication {
            resources: crate::widget::published_resources(root, &self.app.state.doc),
            providers: crate::widget::published_search_providers(root),
        })
    }
    fn run_window_control(
        &mut self,
        invocation: tilo_actions::ControlInvocation,
    ) -> Result<(), tilo_actions::ControlInvocation> {
        match self
            .app
            .invoke_local_resource_action(&invocation.address, &invocation.arguments)
        {
            Ok(_) => {
                self.wants_frame.set(true);
                Ok(())
            }
            Err(error) => {
                tracing::warn!(%error, address = ?invocation.address, "the window's search row was refused");
                Err(invocation)
            }
        }
    }
    fn invoke_device_local_action(
        &mut self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<crate::widget::ResourceOperationStart, tilo_actions::DoorError> {
        self.runs();
        if !self.device_local {
            return Err(tilo_actions::DoorError::Unknown(address.clone()));
        }
        let started = self.app.invoke_local_resource_action(address, arguments);
        self.wants_frame.set(true);
        started
    }
    fn app_kind(&self) -> AppKind {
        self.app_widget()
            .map_or(AppKind::Editor, AppWidget::app_kind)
    }
    fn app_title(&self) -> &str {
        self.app_widget().map_or("", AppWidget::app_title)
    }
    fn app_icon(&self) -> IconDescriptor {
        self.app_widget()
            .map_or(IconDescriptor::Glyph(""), AppWidget::app_icon)
    }
    fn permission_hint(&self) -> Option<AppKind> {
        self.app_widget()?.permission_hint()
    }
    fn device_shell_operation(
        &mut self,
        operation: &crate::floating_surface::DeviceShellOperation,
    ) -> Result<serde_json::Value, String> {
        self.runs();
        self.app_widget_mut()
            .ok_or("Terminal is not available")?
            .device_shell_operation(operation)
    }
    fn request_device_shell(&mut self) -> bool {
        self.app_widget_mut()
            .is_some_and(AppWidget::request_device_shell)
    }
    /// Its host asks on every pump; the app is reached only when it ran
    /// since it last had none, and only taking one runs it.
    fn take_host_intent(&mut self) -> Option<crate::widget::AppHostIntent> {
        if !self.intents_owed.get() {
            return None;
        }
        let intent = self
            .app
            .root_widget
            .as_any_mut()
            .and_then(|root| root.downcast_mut::<crate::widget::BoundAppWidget>())
            .and_then(AppWidget::take_host_intent);
        if intent.is_some() {
            self.runs();
            self.app.mark_semantic_tree_stale();
        } else {
            self.intents_owed.set(false);
        }
        intent
    }
}

/// Whether `node` or a node under it is marked focused.
fn holds_focus(node: &crate::a11y::AccessNode) -> bool {
    node.state.focused || node.children.iter().any(holds_focus)
}

/// The prefix of the action target of a hosted control that has no name,
/// followed by its key path.
const KEY_ROUTE: &str = "tilo.key-action:embedded:";

/// The action target reaching the hosted control at `keys`.
fn encode_key_route(keys: &[u64]) -> String {
    let keys: Vec<String> = keys.iter().map(u64::to_string).collect();
    format!("{KEY_ROUTE}{}", keys.join("/"))
}

/// The key path an action target from [`encode_key_route`] names.
fn key_route(target: &str) -> Option<Vec<u64>> {
    let keys = target
        .strip_prefix(KEY_ROUTE)?
        .split('/')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    (!keys.is_empty()).then_some(keys)
}

/// The name a widget's own semantic target gives its node outside the app:
/// the target itself, or what a route through the node's painting ancestor
/// carries; none for a route by key path, which only the app knows.
fn published_name(target: &str) -> Option<&str> {
    let inner = match target.strip_prefix("tilo.child:") {
        Some(route) => route.split_once('>')?.1,
        None => target,
    };
    (!inner.is_empty() && !inner.starts_with('#') && !inner.starts_with("tilo.key-action:"))
        .then_some(inner)
}

impl EmbeddedApp {
    /// The name the app's node reaches it by: the target its widget
    /// publishes for it, or that widget's test id.
    fn hosted_name(&self, node: &crate::a11y_tree::SemanticNode) -> Option<String> {
        if let Some(target) = &node.semantic_target {
            return published_name(target).map(str::to_owned);
        }
        // Only a widget's own node has a layout node and no target.
        let taffy = node.widget_taffy_id?;
        self.app
            .inspect_node(NodeId::from(taffy), |widget| {
                widget.test_id().map(str::to_owned)
            })
            .ok()
            .flatten()
    }

    /// The key the host knows the app's node by among its siblings, the
    /// same from frame to frame: its name, else its durable identity, else
    /// its place among its siblings (a layout id changes as the app is
    /// rebuilt).
    fn hosted_key(
        node: &crate::a11y_tree::SemanticNode,
        name: Option<&str>,
        ordinal: usize,
    ) -> u64 {
        match name {
            Some(name) => crate::a11y::hash_key(&("target", name)),
            None if node.durable_identity => crate::a11y::hash_key(&node.id.as_str()),
            None => crate::a11y::hash_key(&("ordinal", ordinal)),
        }
    }

    /// The app's semantic node `id`, the `ordinal`th child of the node at
    /// `parent`, and what is below it, as synthetic accessibility nodes
    /// placed in the host. Nodes with no place on screen and none placed
    /// below them (published resources) are left out.
    fn access_subtree(
        &self,
        id: &crate::a11y_tree::SemanticId,
        ordinal: usize,
        parent: &[u64],
    ) -> Option<crate::a11y::AccessNode> {
        let tree = self.app.semantic_tree();
        let node = tree.node(id)?;
        let name = self.hosted_name(node);
        let key = Self::hosted_key(node, name.as_deref(), ordinal);
        let path = [parent, &[key]].concat();
        let children: Vec<_> = node
            .children
            .iter()
            .enumerate()
            .filter_map(|(ordinal, child)| self.access_subtree(child, ordinal, &path))
            .collect();
        // A container that names no place of its own (a menu around its
        // painted rows) still holds the controls placed inside it.
        if node.bounds.is_none() && children.is_empty() {
            return None;
        }
        let mut access = crate::a11y::AccessNode::new(node.role);
        access.name.clone_from(&node.name);
        access.value.clone_from(&node.value);
        access.description.clone_from(&node.description);
        access.state = node.state;
        // The app's own focus is the reader's only while the host has given
        // the app the keyboard: a host that moved it elsewhere has its own
        // focus to show.
        access.state.focused &= self.focused;
        access.key = Some(key);
        access.action_target = match name {
            Some(name) => Some(name),
            None => (!node.actions.is_empty()).then(|| encode_key_route(&path)),
        };
        access.bounds = node.bounds.map(|bounds| {
            let placed = bounds + vello::kurbo::Vec2::new(self.rect.x0, self.rect.y0);
            crate::a11y::AccessBounds {
                x0: placed.x0,
                y0: placed.y0,
                x1: placed.x1,
                y1: placed.y1,
            }
        });
        access.children = children;
        // The app's tree marks both its focused widget and the part of it
        // that has the keyboard (a dialog and its field). Here every node is
        // the tile's descendant and the first focused one is taken as the
        // focus, so only the innermost keeps the mark.
        if access.state.focused && access.children.iter().any(holds_focus) {
            access.state.focused = false;
        }
        Some(access)
    }

    /// The app's semantic node a key path from [`Self::access_subtree`]
    /// names.
    fn hosted_node_at(&self, key_path: &[u64]) -> Option<crate::a11y_tree::SemanticId> {
        let tree = self.app.semantic_tree();
        let mut current = tree.root().clone();
        for key in key_path {
            current = tree
                .node(&current)?
                .children
                .iter()
                .enumerate()
                .find(|(ordinal, child)| {
                    tree.node(child).is_some_and(|node| {
                        Self::hosted_key(node, self.hosted_name(node).as_deref(), *ordinal) == *key
                    })
                })?
                .1
                .clone();
        }
        (!key_path.is_empty()).then_some(current)
    }

    /// The app's semantic node on screen whose name is `name`, first in
    /// reading order.
    fn hosted_node_named(&self, name: &str) -> Option<crate::a11y_tree::SemanticId> {
        let tree = self.app.semantic_tree();
        let mut pending = vec![tree.root().clone()];
        while let Some(id) = pending.pop() {
            let Some(node) = tree.node(&id) else {
                continue;
            };
            if node.bounds.is_some() && self.hosted_name(node).as_deref() == Some(name) {
                return Some(id);
            }
            pending.extend(node.children.iter().rev().cloned());
        }
        None
    }

    /// Perform `action` on the app's node `id` as the app performs it for
    /// its own assistive technology: an action the node offers as it offers
    /// it (a press bound to one of the app's actions runs that action), any
    /// other verb by the widget that owns the node. A disabled control takes
    /// none, a read-only one no value.
    fn perform_hosted(
        &mut self,
        id: &crate::a11y_tree::SemanticId,
        action: &crate::a11y::Action,
    ) -> bool {
        use crate::a11y::Action;
        let Some(node) = self.app.semantic_tree().node(id) else {
            return false;
        };
        if node.state.disabled
            || (node.state.read_only
                && matches!(action, Action::SetValue(_) | Action::SetSelection { .. }))
        {
            return false;
        }
        let offered = node
            .actions
            .iter()
            .find(|candidate| {
                matches!(&candidate.kind, crate::a11y_tree::SemanticActionKind::Verb(verb)
                    if std::mem::discriminant(verb) == std::mem::discriminant(action))
            })
            .map(|candidate| candidate.name.clone());
        let control = self.app.focused_control();
        let done = match offered {
            Some(name) => {
                let argument = match action {
                    Action::SetValue(value) => Some(value.as_str()),
                    _ => None,
                };
                self.app.invoke_semantic_action(id, &name, argument)
            }
            None => self.app.perform_semantic_verb(
                id,
                action.clone(),
                matches!(action, Action::Activate | Action::ToggleCheck),
            ),
        }
        .unwrap_or(false);
        // Focus moved inside an app the host has not given the keyboard
        // lands there once the host does.
        if done && matches!(action, Action::Focus) && !self.focused {
            self.restore_focus = self.app.focused_control();
        }
        self.follow_focus_in_semantics(control);
        self.wants_frame.set(true);
        done
    }
}

#[cfg(test)]
mod theme_repaint_tests {
    use super::*;

    pub(super) fn layout(tile: &mut EmbeddedApp, theme: &Theme) {
        let mut taffy = TaffyTree::new();
        let node = tile.build_layout(&mut taffy, theme, &mut HashMap::new());
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(300.0),
                    height: taffy::AvailableSpace::Definite(100.0),
                },
            )
            .unwrap();
        tile.apply_layout(&taffy, node, Point::ZERO);
    }

    pub(super) fn paint(tile: &mut EmbeddedApp, theme: &Theme) -> Vec<u32> {
        let mut scene = Scene::new();
        tile.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            theme,
            &PresenceCtx::empty(),
        );
        scene.encoding().draw_data.clone()
    }

    /// Paint `tile` into a section tracked for damage parts, as the shell
    /// paints its tiles, and return the section with its parts.
    fn paint_tracked(
        tile: &mut EmbeddedApp,
        theme: &Theme,
    ) -> (Box<Scene>, crate::scene_parts::SectionParts) {
        let recording = crate::scene_parts::Recording::begin();
        let mut scene = Box::new(Scene::new());
        crate::scene_parts::track(&scene);
        tile.draw(
            &mut scene,
            Affine::translate((40.0, 20.0)),
            &mut TextRenderer::new(),
            theme,
            &PresenceCtx::empty(),
        );
        let parts = recording
            .finish()
            .of(&scene)
            .expect("the tile's parts are well nested");
        (scene, parts)
    }

    /// Focus a host gives an app it just mounted lands on the app's control
    /// before anything is painted: the key typed right after the shortcut
    /// that showed it reaches that control.
    #[test]
    fn focus_given_before_the_first_paint_lands_on_the_apps_control() {
        for laid_out in [false, true] {
            let app = App::new_with_controlled_inputs(
                crate::state::State::new(),
                Box::new(crate::widget::Button::new("probe", "Probe")),
            );
            let mut tile = EmbeddedApp::new(app, |_| {}).unwrap();
            if laid_out {
                layout(&mut tile, &Theme::light());
            }
            tile.set_focus(true);
            assert!(
                tile.app.focused_control().is_some(),
                "the button has focus (laid out: {laid_out})"
            );
            assert!(tile.needs_redraw(), "the tree built asks for its frame");
            let _ = paint(&mut tile, &Theme::light());
            assert!(tile.app.focused_control().is_some(), "and keeps it painted");
        }
    }

    #[test]
    fn a_tooltip_in_an_embedded_app_damages_its_chip_not_the_app() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let app = App::new_with_controlled_inputs(
            crate::state::State::new(),
            Box::new(crate::widget::Button::new("probe", "Probe").tooltip("Probe the tile")),
        );
        let mut tile = EmbeddedApp::new(app, |_| {}).unwrap();
        let theme = Theme::light();
        layout(&mut tile, &theme);
        for _ in 0..4 {
            let _ = paint_tracked(&mut tile, &theme);
        }
        let region = KurboRect::new(40.0, 20.0, 340.0, 120.0);
        let button = tile.app.root_widget.get_rect();
        let at = button.center();
        let moved = crate::synthetic_input::cursor_moved(at.x, at.y);
        tile.app.surface_input(&tile.region, moved, false);
        tile.wants_frame.set(true);
        let hovered = paint_tracked(&mut tile, &theme);
        assert!(
            tile.app.input.tooltip_host.needs_frame(),
            "the button's tooltip is pending over {button:?}"
        );
        // It rests: the tooltip shows, and only it changes.
        let rested = crate::time::now()
            + crate::widget::tooltip::HOVER_DWELL
            + crate::time::Duration::from_millis(50);
        tile.wants_frame.set(true);
        let shown =
            crate::time::testing::with_monotonic_time(rested, || paint_tracked(&mut tile, &theme));
        assert!(
            tile.app.input.tooltip_host.visible().is_some(),
            "the tooltip shows"
        );
        let damage = crate::scene_parts::damage(&hovered.0, &hovered.1, &shown.0, &shown.1)
            .expect("the tooltip is proven");
        let chip = tile.damage_report_for_test();
        assert!(!damage.is_empty(), "the tooltip is repainted");
        let repainted: f64 = damage.iter().map(|rect| rect.area()).sum();
        assert!(
            repainted < region.area(),
            "{damage:?} repaints the tooltip ({chip:?}), not the app's {region:?}"
        );
    }

    /// A tooltip is about where the pointer rests in a window the person
    /// is using: it goes when the pointer leaves, and when the window loses
    /// focus.
    #[test]
    fn a_tooltip_goes_when_the_pointer_leaves_or_the_window_loses_focus() {
        let _settled = crate::motion::testing::settled();
        for ending in [
            WindowEvent::CursorLeft {
                device_id: winit::event::DeviceId::dummy(),
            },
            WindowEvent::Focused(false),
        ] {
            let app = App::new_with_controlled_inputs(
                crate::state::State::new(),
                Box::new(crate::widget::Button::new("probe", "Probe").tooltip("Probe the tile")),
            );
            let mut tile = EmbeddedApp::new(app, |_| {}).unwrap();
            let theme = Theme::light();
            layout(&mut tile, &theme);
            let _ = paint_tracked(&mut tile, &theme);
            let at = tile.app.root_widget.get_rect().center();
            let moved = crate::synthetic_input::cursor_moved(at.x, at.y);
            tile.app.surface_input(&tile.region, moved, false);
            let rested = crate::time::now()
                + crate::widget::tooltip::HOVER_DWELL
                + crate::time::Duration::from_millis(50);
            tile.wants_frame.set(true);
            crate::time::testing::with_monotonic_time(rested, || {
                let _ = paint_tracked(&mut tile, &theme);
                assert!(
                    tile.app.input.tooltip_host.visible().is_some(),
                    "the tooltip shows"
                );
                tile.app.surface_input(&tile.region, ending.clone(), false);
            });
            assert!(
                tile.app.input.tooltip_host.visible().is_none(),
                "{ending:?} takes the tooltip away"
            );
        }
    }

    #[test]
    fn an_embedded_frame_hands_its_widget_damage_to_the_host() {
        let label = std::rc::Rc::new(std::cell::RefCell::new("before".to_owned()));
        let source = std::rc::Rc::clone(&label);
        let app = App::new_with_controlled_inputs(
            crate::state::State::new(),
            Box::new(crate::widget::Text::dynamic(move || {
                source.borrow().clone()
            })),
        );
        let mut tile = EmbeddedApp::new(app, |_| {}).unwrap();
        let theme = Theme::light();
        layout(&mut tile, &theme);
        let mut previous = paint_tracked(&mut tile, &theme);
        for _ in 0..4 {
            previous = paint_tracked(&mut tile, &theme);
            if !tile.needs_redraw() {
                break;
            }
        }
        let region = KurboRect::new(40.0, 20.0, 340.0, 120.0);
        // A frame that changed nothing hands over no damage, and the host
        // finds none.
        tile.wants_frame.set(true);
        let idle = paint_tracked(&mut tile, &theme);
        assert_eq!(tile.painted_damage.rects, Some(Vec::new()));
        assert_eq!(
            crate::scene_parts::damage(&previous.0, &previous.1, &idle.0, &idle.1),
            Ok(Vec::new())
        );
        // A changed label is the widget's damage, not more than the region.
        *label.borrow_mut() = "after".to_owned();
        tile.app.needs_sync = true;
        tile.wants_frame.set(true);
        let changed = paint_tracked(&mut tile, &theme);
        let damage = crate::scene_parts::damage(&idle.0, &idle.1, &changed.0, &changed.1)
            .expect("the change is proven");
        assert!(!damage.is_empty(), "the new label is repainted");
        assert!(
            damage
                .iter()
                .all(|rect| region.inflate(1.0, 1.0).union(*rect) == region.inflate(1.0, 1.0)),
            "{damage:?} stays in the app's region {region:?}"
        );
        assert_eq!(
            tile.painted_damage.from + 1,
            tile.painted_damage.to,
            "the painted generation follows the one before"
        );
    }

    #[test]
    fn embedded_host_theme_change_repaints_an_idle_scene() {
        let app = App::new_with_controlled_inputs(
            crate::state::State::new(),
            Box::new(crate::widget::Text::new("Theme probe")),
        );
        let mut tile = EmbeddedApp::new(app, |_| {}).unwrap();
        let light = Theme::light();
        layout(&mut tile, &light);
        let mut previous = Vec::new();
        for _ in 0..4 {
            previous = paint(&mut tile, &light);
            if !tile.needs_redraw() {
                break;
            }
        }
        assert!(
            !tile.needs_redraw(),
            "the actual tile must settle before a theme change"
        );
        assert!(!previous.is_empty(), "the actual label must be painted");
        layout(&mut tile, &light);
        assert!(
            !tile.needs_redraw(),
            "an unchanged theme keeps the idle tile cached"
        );
        assert_eq!(paint(&mut tile, &light), previous);
        for theme in [Theme::dark(), Theme::high_contrast()] {
            layout(&mut tile, &theme);
            assert!(
                tile.needs_redraw(),
                "a host theme change must repaint the retained scene"
            );
            let current = paint(&mut tile, &theme);
            assert_ne!(
                current, previous,
                "the actual scene must contain the new theme's ink"
            );
            previous = current;
        }
    }

    #[test]
    fn focus_restored_before_a_hidden_app_is_drawn_reuses_its_painted_scene() {
        let app = App::new_with_controlled_inputs(
            crate::state::State::new(),
            Box::new(crate::widget::Text::new("Focus probe")),
        );
        let mut tile = EmbeddedApp::new(app, |_| {}).unwrap();
        let theme = Theme::light();
        layout(&mut tile, &theme);
        for _ in 0..8 {
            paint(&mut tile, &theme);
            if !tile.needs_redraw() {
                break;
            }
        }
        assert!(
            !tile.needs_redraw(),
            "the app must settle before focus moves"
        );

        tile.set_focus(true);
        assert!(tile.needs_redraw(), "visible focus entering needs a frame");
        paint(&mut tile, &theme);
        assert!(!tile.needs_redraw());
        let shown = tile.painted_damage.to;

        tile.set_focus(false);
        assert!(tile.needs_redraw(), "visible focus leaving needs a frame");
        // A workspace switch parks this app without drawing it, then gives it
        // focus back before its next scene is composed.
        tile.set_focus(true);
        assert!(!tile.needs_redraw());
        paint(&mut tile, &theme);
        assert_eq!(tile.painted_damage.to, shown, "the app did not repaint");

        tile.set_focus(false);
        paint(&mut tile, &theme);
        assert!(
            tile.painted_damage.to > shown,
            "a lasting focus change repaints"
        );
        let unfocused = tile.painted_damage.to;
        tile.wants_frame.set(true);
        paint(&mut tile, &theme);
        assert!(
            tile.painted_damage.to > unfocused,
            "other app work still repaints"
        );
    }
}

/// #3986: a tile whose record was still being written at its last frame
/// left a wake deadline that only its next `draw` moved, so a compositor that
/// does not paint for a due deadline woke for it on every park.
#[cfg(test)]
mod saving_record_wake_tests {
    use super::theme_repaint_tests::{layout, paint};
    use super::*;
    use crate::application::{LaunchTarget, RestoreInput};
    use crate::application_test_support::{
        ProbeHost, runner_inputs, runner_permissions, runner_probe_definition,
    };
    use crate::record::RecordSaveStatus;

    fn bound(tile: &mut EmbeddedApp) -> &mut crate::application::AppComponent {
        &mut tile
            .app
            .root_widget
            .as_any_mut()
            .and_then(|root| root.downcast_mut::<crate::widget::BoundAppWidget>())
            .expect("the tile shows a component")
            .component
    }

    fn sampled(tile: &mut EmbeddedApp) -> Option<RecordSaveStatus> {
        bound(tile).sampled_record_save().cloned()
    }

    /// The host's per-tick pump, as `TileHost` runs it for a tile that keeps
    /// its own record.
    fn pump(tile: &mut EmbeddedApp) -> bool {
        let model = automerge::AutoCommit::new();
        tile.sync(&model, &automerge::ROOT);
        tile.update(&model, &automerge::ROOT)
    }

    /// What the frame the user last saw sampled: the app's sync, which reads
    /// the record's durability, with the record's edit not yet written.
    fn sample(tile: &mut EmbeddedApp) {
        let app = &mut tile.app;
        app.root_widget.sync(&app.state.doc, &app.state.root);
    }

    fn settle(tile: &mut EmbeddedApp, theme: &Theme) {
        for _ in 0..8 {
            layout(tile, theme);
            paint(tile, theme);
            if !tile.needs_redraw() {
                return;
            }
        }
        panic!("the tile must come to rest");
    }

    #[test]
    fn a_record_saving_at_the_last_frame_is_resampled_when_its_store_wakes_the_pump() {
        // The frame's sync offers the edit to the record's writer, which
        // would land it on its own after its debounce: on a loaded machine
        // that falls inside the pumps below that check a save still under
        // way. The store writes only when the test flushes it.
        let host = ProbeHost::holding_writes();
        let component = runner_probe_definition()
            .instantiate(
                host.prepare_with_permissions(
                    runner_probe_definition(),
                    11,
                    &LaunchTarget::New,
                    runner_permissions(),
                )
                .unwrap(),
                LaunchTarget::New,
                RestoreInput::None,
            )
            .unwrap();
        let binding = component.callback_binding();
        let id = binding.checkout.id.clone();
        let app = App::from_component(component, runner_inputs()).unwrap();
        let mut tile = EmbeddedApp::new(app, |_| {}).unwrap();
        let theme = Theme::dark();
        settle(&mut tile, &theme);

        // An edit the writer has not taken yet: the frame samples "Saving".
        binding
            .records
            .with(&id, |doc| {
                automerge::transaction::Transactable::put(doc, automerge::ROOT, "unsaved", 1_u64)
                    .unwrap();
            })
            .unwrap();
        sample(&mut tile);
        assert_eq!(sampled(&mut tile), Some(RecordSaveStatus::Saving));
        assert_eq!(
            tile.next_wake_deadline(),
            None,
            "the record's store wakes the tile once the write lands: no deadline polls for it"
        );

        // Nothing has landed and nothing woke the tile: the pump runs nothing
        // and asks for no frame, however often the host pumps.
        for _ in 0..100 {
            assert!(!pump(&mut tile), "a save still under way paints nothing");
        }
        assert!(!tile.needs_redraw());
        assert_eq!(sampled(&mut tile), Some(RecordSaveStatus::Saving));

        // The writer finishes off the event loop and its store wakes the tile.
        binding.records.try_flush_record(&id).unwrap();
        binding.records.await_submitted(&id).unwrap();
        assert_eq!(
            binding.records.save_status(&id).unwrap(),
            RecordSaveStatus::Saved
        );

        // The pump, with no draw, takes up the wake and finds it saved: one
        // frame shows the change.
        assert!(pump(&mut tile), "the finished save is shown");
        assert_eq!(sampled(&mut tile), Some(RecordSaveStatus::Saved));
        assert_eq!(tile.next_wake_deadline(), None);
        assert!(tile.needs_redraw());
        settle(&mut tile, &theme);

        // And the loop goes quiet: no deadline, no frame, however often the
        // host pumps.
        for _ in 0..100 {
            assert!(!pump(&mut tile));
        }
        assert_eq!(tile.next_wake_deadline(), None);
        assert!(!tile.needs_redraw());
        tile.app.close_components();
    }
}

#[cfg(test)]
#[path = "embedded_copy_tests.rs"]
mod copy_tests;

#[cfg(test)]
#[path = "embedded_wake_tests.rs"]
mod wake_tests;

#[cfg(test)]
#[path = "embedded_search_tests.rs"]
mod search_tests;

#[cfg(test)]
mod wake_deadline_tests {
    use super::theme_repaint_tests::{layout, paint};
    use super::*;
    use crate::widget::TaffyTreeExt as _;
    use std::rc::Rc;

    /// A leaf counting how often its deadline is asked for.
    struct Asked {
        asks: Rc<Cell<usize>>,
        deadline: crate::time::Instant,
        rect: KurboRect,
    }

    impl Widget for Asked {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            taffy.widget_leaf(taffy::Style::default(), node_map, self)
        }
        crate::rect_geometry!();
        fn draw(
            &mut self,
            _: &mut Scene,
            _: Affine,
            _: &mut TextRenderer,
            _: &Theme,
            _: &PresenceCtx,
        ) {
        }
        fn name(&self) -> &'static str {
            "Asked"
        }
        fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
            self.asks.set(self.asks.get() + 1);
            Some(self.deadline)
        }
    }

    /// An app hosted in a tile is asked for its next deadline once after it
    /// ran, not on every pump and paint of its host: nothing but the app
    /// running moves it.
    #[test]
    fn an_idle_embedded_app_is_asked_its_deadline_once_after_it_ran() {
        let asks = Rc::new(Cell::new(0));
        let deadline = crate::time::now() + crate::time::Duration::from_secs(3600);
        let app = App::new_with_controlled_inputs(
            crate::state::State::new(),
            Box::new(Asked {
                asks: Rc::clone(&asks),
                deadline,
                rect: KurboRect::ZERO,
            }),
        );
        let mut tile = EmbeddedApp::new(app, |_| {}).unwrap();
        let theme = Theme::light();
        layout(&mut tile, &theme);
        let _ = paint(&mut tile, &theme);
        let doc = automerge::AutoCommit::new();
        // The app ran in its first frame and pump.
        let _ = Widget::update(&mut tile, &doc, &automerge::ROOT);
        let _ = paint(&mut tile, &theme);
        assert_eq!(Widget::next_wake_deadline(&tile), Some(deadline));
        let settled = asks.get();
        for _ in 0..10 {
            assert!(!Widget::update(&mut tile, &doc, &automerge::ROOT));
            let _ = paint(&mut tile, &theme);
            assert_eq!(Widget::next_wake_deadline(&tile), Some(deadline));
        }
        assert_eq!(asks.get(), settled, "idle pumps and paints asked again");
        // Its host asks for its intents on every pump: an app that did not
        // run since it had none is not reached, so nothing it holds moved.
        for _ in 0..10 {
            assert!(AppWidget::take_host_intent(&mut tile).is_none());
            assert_eq!(Widget::next_wake_deadline(&tile), Some(deadline));
        }
        assert_eq!(asks.get(), settled, "asking for intents ran the app");
        // Whatever reaches into the app may move it: asked again.
        let _ = tile.app_mut();
        let _ = Widget::next_wake_deadline(&tile);
        assert!(asks.get() > settled);
    }
}

#[cfg(test)]
mod semantic_target_tests {
    use super::theme_repaint_tests::{layout, paint};
    use super::*;
    use std::rc::Rc;

    /// The node under `node` whose action target is `name`.
    fn named<'a>(
        node: &'a crate::a11y::AccessNode,
        name: &str,
    ) -> Option<&'a crate::a11y::AccessNode> {
        if node.action_target.as_deref() == Some(name) {
            return Some(node);
        }
        node.children.iter().find_map(|child| named(child, name))
    }

    fn settle(tile: &mut EmbeddedApp, theme: &Theme) {
        for _ in 0..8 {
            layout(tile, theme);
            let _ = paint(tile, theme);
            if !tile.needs_redraw() {
                return;
            }
        }
    }

    fn act(tile: &mut EmbeddedApp, target: &str, action: &crate::a11y::Action) -> bool {
        let mut doc = automerge::AutoCommit::new();
        Widget::perform_semantic_action(
            tile,
            target,
            action,
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        )
    }

    /// A host reaches the controls of an app it shows by the names the app
    /// gives them, where they are drawn in the host, under keys that stay
    /// the same as the app is rebuilt, and acts on them through those
    /// names as the app's own assistive technology would.
    #[test]
    fn a_hosted_apps_controls_are_reached_and_acted_on_by_name() {
        let pressed = Rc::new(Cell::new(0_usize));
        let counter = Rc::clone(&pressed);
        let column = crate::widget::Column::new()
            .push(Box::new(crate::widget::Button::new("first", "First")))
            .push(Box::new(
                crate::widget::Button::new("probe", "Probe")
                    .on_click(move || counter.set(counter.get() + 1)),
            ));
        let app = App::new_with_controlled_inputs(crate::state::State::new(), Box::new(column));
        let mut tile = EmbeddedApp::new(app, |_| {}).unwrap();
        let theme = Theme::light();
        settle(&mut tile, &theme);

        let access = tile.accessibility().expect("the app publishes its tree");
        let probe = named(&access, "probe").expect("the button is published by its name");
        let key = probe.key;
        assert!(named(&access, "first").is_some());
        assert!(
            probe
                .bounds
                .is_some_and(|bounds| bounds.to_rect().area() > 0.0),
            "{probe:?} is placed in the host"
        );

        // Rebuilt, the app's controls keep the keys the host knows them by.
        tile.build_tree(true);
        settle(&mut tile, &theme);
        let access = tile.accessibility().unwrap();
        assert_eq!(named(&access, "probe").and_then(|node| node.key), key);

        assert!(act(&mut tile, "probe", &crate::a11y::Action::Activate));
        assert_eq!(pressed.get(), 1, "activating the name presses the button");
        assert!(!act(&mut tile, "missing", &crate::a11y::Action::Activate));
        assert_eq!(pressed.get(), 1);

        // Focus moved to a control by name is where the keyboard lands once
        // the host gives the app focus.
        assert!(act(&mut tile, "first", &crate::a11y::Action::Focus));
        tile.set_focus(true);
        let focused = tile.app.focused_control().expect("a control has focus");
        assert_eq!(
            tile.app
                .inspect_node(focused, |widget| widget.test_id().map(str::to_owned))
                .unwrap()
                .as_deref(),
            Some("first")
        );
    }
}
