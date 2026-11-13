use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::time::{Duration, Instant};
use crate::widget::TaffyTreeExt;
use crate::widget::app_widget::AppWidget;
use crate::widget::{AppModel, ModelPath};
use crate::widget::{
    AudiencePresentationProvider, AudiencePresentationWidgetExt, CompleteContentWidgetExt as _,
    Continuable, FloatingSurfaceProvider, FloatingSurfaceWidgetExt, FullscreenProvider,
    FullscreenWidgetExt, Widget,
};
use automerge::{AutoCommit, ObjId, ROOT};
use std::collections::{HashMap, VecDeque};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Shape};
use winit::event::WindowEvent;

use crate::motion::{Direction, WindowMotion};
use crate::tokens::{EffectiveMotion, MotionSource};
use crate::widget::DrawChild;

fn spawn_progress(elapsed: Duration) -> f64 {
    WindowMotion::OPEN.progress_after(
        Direction::Enter,
        EffectiveMotion::default(),
        MotionSource::Pointer,
        elapsed,
    )
}

fn spawn_time() -> Duration {
    WindowMotion::OPEN.settle_time(
        Direction::Enter,
        EffectiveMotion::default(),
        MotionSource::Pointer,
    )
}

/// One logical pixel: heavier reads as a drawn outline around the app,
/// not the boundary of a surface.
const EDGE_STROKE_WIDTH: f64 = 1.0;

/// The focused window's edge: two logical pixels, so which window takes the
/// keyboard is seen at a glance rather than looked for. The edge sits outside
/// the card, so the extra pixel is desktop, never the app's own.
pub(crate) const FOCUSED_EDGE_STROKE_WIDTH: f64 = 2.0;

const WIDEST_EDGE_STROKE_WIDTH: f64 = FOCUSED_EDGE_STROKE_WIDTH;

/// Rects along `rect`'s outline that hold every pixel of an edge drawn
/// around it at corner `radius`: a square at each corner, where the edge
/// curves up to `radius` inside the outline, and between them a thin band
/// along each side, where it is a straight stroke at most its widest width
/// from the outline (outside it, or inside it when drawn flush). One more
/// pixel each way covers antialiasing. A focus change repaints these, so
/// the sides stay as thin as the stroke rather than as deep as the corners.
fn edge_bands(rect: Rect, radius: f64) -> Vec<Rect> {
    let outside = WIDEST_EDGE_STROKE_WIDTH + 1.0;
    let corner = radius.max(0.0) + outside;
    let outer = rect.inflate(outside, outside);
    let (left, right) = (rect.x0 + corner, rect.x1 - corner);
    let (top, bottom) = (rect.y0 + corner, rect.y1 - corner);
    if left >= right || top >= bottom {
        return vec![
            Rect::new(outer.x0, outer.y0, outer.x1, top.min(outer.y1)),
            Rect::new(outer.x0, bottom.max(outer.y0), outer.x1, outer.y1),
            Rect::new(outer.x0, outer.y0, left.min(outer.x1), outer.y1),
            Rect::new(right.max(outer.x0), outer.y0, outer.x1, outer.y1),
        ];
    }
    vec![
        Rect::new(outer.x0, outer.y0, left, top),
        Rect::new(right, outer.y0, outer.x1, top),
        Rect::new(outer.x0, bottom, left, outer.y1),
        Rect::new(right, bottom, outer.x1, outer.y1),
        Rect::new(left, outer.y0, right, rect.y0 + outside),
        Rect::new(left, rect.y1 - outside, right, outer.y1),
        Rect::new(outer.x0, top, rect.x0 + outside, bottom),
        Rect::new(rect.x1 - outside, top, outer.x1, bottom),
    ]
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum AppKind {
    Terminal,
    Files,
    Editor,
    Browser,
    Settings,
    /// Any other app, by its own glyph from the icon set
    /// ([`crate::icons::app_glyph`]): a window wears its own app's mark,
    /// never another app's.
    App(crate::icons::Icon),
}

impl AppKind {
    pub fn as_str(&self) -> &str {
        match self {
            AppKind::Terminal => "Terminal",
            AppKind::Files => "Files",
            AppKind::Editor => "Editor",
            AppKind::Browser => "Browser",
            AppKind::Settings => "Settings",
            AppKind::App(_) => "App",
        }
    }
}

pub enum TileContent {
    App(Box<dyn AppWidget>),
    Stub {
        kind: AppKind,
        label: String,
    },
    Notice {
        title: String,
        detail: String,
    },
    /// The place of an app whose code or record is still on its way (a
    /// browser bundle fetches each app's module the first time one of its
    /// windows opens). It has drawn nothing, so its window reserves its
    /// place and shows the framework's placeholder, as for any content whose
    /// first frame is not drawn yet ([`crate::arriving`]); the app built in
    /// its place once it is in goes on with the same arrival
    /// ([`TileHost::continue_arrival`]). It holds no record and takes no
    /// focus or input: nothing typed while it waits lands in a field that is
    /// not there yet.
    Arriving,
}

/// How far around a widget's damage its drawing may reach: antialiasing,
/// shadows and focus rings outside its layout box, in logical pixels (as
/// the native window pads its widget damage).
const APP_DAMAGE_REACH: f64 = 16.0;

#[derive(Debug)]
struct ContentDamage {
    source: u64,
    generation: u64,
    painted: Option<(Affine, Vec<(crate::damage::WidgetIdentity, Rect)>)>,
    steps: std::collections::VecDeque<(u64, Vec<Rect>)>,
}

const APP_DAMAGE_STEPS: usize = 4;

impl ContentDamage {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            source: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            generation: 0,
            painted: None,
            steps: std::collections::VecDeque::new(),
        }
    }

    /// Note that `app` was just drawn at `placement`, clipped to `clip`, and
    /// say what changed since its last drawing: where its widgets were and
    /// are, and what they damaged. No hint when it moved, or when widgets
    /// paint other people's marks placed from their content, which their
    /// damage does not cover.
    ///
    /// With `copies` (the host's device pixels per unit of the app's own
    /// coordinates), the host keeps the pixels it presented and can move
    /// them: a scroll in the app is handed over as what moved
    /// ([`crate::scene_parts::DamageHint::copies`]) and the strip it
    /// exposed, not as its whole viewport.
    fn hint(
        &mut self,
        app: &dyn Widget,
        placement: Affine,
        clip: Rect,
        presence: &PresenceCtx,
        copies: Option<f64>,
    ) -> Option<crate::scene_parts::DamageHint> {
        let boxes = crate::damage::widget_boxes(app);
        let from = self.generation;
        self.generation = self.generation.wrapping_add(1);
        let previous = self.painted.replace((placement, boxes));
        let before = previous
            .filter(|(placed, _)| *placed == placement)
            .map(|(_, before)| before);
        let (Some(before), Some((_, after))) = (
            before.filter(|_| !presence.places_content_marks(crate::time::now())),
            self.painted.as_ref(),
        ) else {
            self.steps.clear();
            return None;
        };
        let mut damage = Vec::new();
        let offers = if copies.is_some() {
            crate::scroll_copy::accepting(|| app.collect_damage(&mut damage))
        } else {
            app.collect_damage(&mut damage);
            Vec::new()
        };
        let to_section = |rect: Rect| {
            placement
                .transform_rect_bbox(rect.inflate(APP_DAMAGE_REACH, APP_DAMAGE_REACH))
                .intersect(clip)
        };
        let (mut rects, copies) = match copies.filter(|_| !offers.is_empty()) {
            None => {
                damage.extend(crate::damage::changed_boxes_besides_own_damage(
                    app, &before, after,
                ));
                (
                    damage.into_iter().map(to_section).collect::<Vec<_>>(),
                    Vec::new(),
                )
            }
            Some(scale) => {
                let (rects, copies) = planned_copies(
                    app,
                    (&before, after),
                    &offers,
                    damage,
                    (placement, clip),
                    scale,
                );
                (
                    rects.into_iter().map(to_section).chain(copies.1).collect(),
                    copies.0,
                )
            }
        };
        rects.retain(|rect| !rect.is_zero_area());
        let repainted: Vec<Rect> = rects
            .iter()
            .copied()
            .chain(copies.iter().map(|copy| copy.destination().expand()))
            .collect();
        let earlier = (0..self.steps.len())
            .map(|first| {
                let since = self.steps.range(first..).flat_map(|(_, rects)| rects);
                (
                    self.steps[first].0,
                    since.chain(&repainted).copied().collect::<Vec<_>>(),
                )
            })
            .collect();
        self.steps.push_back((from, repainted));
        if self.steps.len() > APP_DAMAGE_STEPS {
            self.steps.pop_front();
        }
        Some(crate::scene_parts::DamageHint {
            source: self.source,
            from,
            to: self.generation,
            placement,
            rects,
            copies,
            earlier,
        })
    }

    fn forget(&mut self) {
        self.painted = None;
        self.steps.clear();
    }
}

/// What an app's drawing changed when the host can move what its scrollers
/// offer to copy (`offers`, collected with its `damage` in the app's own
/// coordinates): the damage, with what moved otherwise and what came or
/// went in a scrolled content ([`crate::scroll_copy::moves_besides`]), in
/// the app's coordinates; and the copies (in the section's, moved by the
/// placement) with what they expose and what stays put over them (in the
/// section's, clipped to `clip`). `scale` is the host's device pixels per
/// unit of the app's coordinates.
fn planned_copies(
    app: &dyn Widget,
    (before, after): (
        &[(crate::damage::WidgetIdentity, Rect)],
        &[(crate::damage::WidgetIdentity, Rect)],
    ),
    offers: &[crate::scroll_copy::ScrollOffer],
    mut damage: Vec<Rect>,
    (placement, clip): (Affine, Rect),
    scale: f64,
) -> (Vec<Rect>, (Vec<crate::scroll_copy::MovedRegion>, Vec<Rect>)) {
    let covered = crate::damage::boxes_covered_by_own_damage(app);
    let uncovered = |boxes: &[(crate::damage::WidgetIdentity, Rect)]| -> Vec<_> {
        boxes
            .iter()
            .filter(|(widget, _)| !covered.contains(widget))
            .copied()
            .collect()
    };
    let moves = crate::scroll_copy::moves_besides(&uncovered(before), &uncovered(after), offers);
    damage.extend(moves.besides);
    damage.extend(moves.carried);
    // What the window shows, in the app's coordinates; the planner's target
    // reaches to its far corner, in the app's device pixels.
    let shown = placement
        .inverse()
        .transform_rect_bbox(clip)
        .intersect(Rect::new(
            0.0,
            0.0,
            f64::from(u32::MAX),
            f64::from(u32::MAX),
        ));
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a window's far corner in device pixels, clamped to what a u32 holds"
    )]
    let size = (
        (shown.x1 * scale).ceil().clamp(0.0, f64::from(u32::MAX)) as u32,
        (shown.y1 * scale).ceil().clamp(0.0, f64::from(u32::MAX)) as u32,
    );
    let plan = crate::scroll_copy::plan_placed(offers, &[app], (placement, shown), scale, size);
    let to_section = |rect: Rect| {
        placement
            .transform_rect_bbox(Rect::new(
                rect.x0 / scale,
                rect.y0 / scale,
                rect.x1 / scale,
                rect.y1 / scale,
            ))
            .intersect(clip)
    };
    let mut fixed: Vec<Rect> = plan
        .damage
        .iter()
        .chain(&plan.exposed)
        .copied()
        .map(to_section)
        .collect();
    let mut copies = Vec::with_capacity(plan.copies.len());
    for copy in &plan.copies {
        let moved = crate::scroll_copy::MovedRegion::of(copy, scale);
        match moved.transformed(placement) {
            Some(moved) => copies.push(moved),
            None => fixed.push(to_section(copy.destination_rect())),
        }
    }
    (damage, (copies, fixed))
}

/// The name of the card a tile shows while its record cannot be opened.
#[must_use]
pub fn record_unavailable_title() -> String {
    crate::i18n::I18nManager::lookup_plain("record-unavailable-title")
}

/// The card a tile shows while its record cannot be opened: what happened
/// and what to do, in the person's words, with the store's reason (`detail`)
/// behind "Details".
fn record_unavailable_card(detail: &str) -> super::EmptyState {
    super::EmptyState::new(record_unavailable_title())
        .with_icon(crate::icons::Icon::File)
        .with_message(crate::i18n::I18nManager::lookup_plain(
            "record-unavailable-hint",
        ))
        .with_details(detail)
}

#[path = "tile_host/window_search.rs"]
mod window_search;

#[allow(
    clippy::struct_excessive_bools,
    reason = "history policy, record refusal, focus, layout dirtiness and presentation latches are independent"
)]
#[crate::component::not_a_component(shell, "hosts an app's window in the strip")]
pub struct TileHost {
    content: TileContent,
    history: Option<crate::history_mode::host::HistoryChrome>,
    pending_history: Option<crate::history_mode::HistoryRegistration>,
    history_opening: bool,
    /// Where the History control stands while History is closed: `None` in
    /// the app's own header row (a tile outside a desktop window), `Some`
    /// once a desktop window hosts the tile. `Some(true)`: the window's tab
    /// row presents it, so the tile draws no History control of its own;
    /// `Some(false)`: the tab row is hidden (a maximized window), so the
    /// app's header row may hold it, but never a band of its own.
    window_history: Option<bool>,
    live_callback_node: Option<NodeId>,
    retained_invocations: HashMap<tilo_actions::InvocationId, crate::history_mode::HistoryOwner>,
    retained_updates: HashMap<tilo_actions::InvocationId, VecDeque<tilo_actions::InvocationUpdate>>,
    /// Operations the component shown admitted that have not ended, so a
    /// component taking its place cannot leave them unanswered.
    component_invocations: std::collections::HashSet<tilo_actions::InvocationId>,
    owner_bound_work: bool,
    owner_bound_custody: Option<crate::history_mode::HistoryOwner>,
    close_reports: Vec<crate::application::ComponentCloseReport>,
    history_lifetime: std::rc::Rc<crate::history_mode::owner::RecordOwnerLifetime>,
    custody_parent: Option<crate::ControlDomain>,
    custody_scope: Option<crate::ControlDomain>,
    custody_renewal: Option<crate::control_custody::CustodyAttachment>,
    record_error: Option<String>,
    error_card: Option<(String, super::EmptyState)>,
    record_transition_error: Option<String>,
    structure_dirty: bool,
    node: Option<NodeId>,
    remount: bool,
    /// Latched: once this tile has shown something, its window stays on
    /// screen for good. Must survive [`TileHost::replace_content`] — a
    /// peer's already-open window arrives as a card, then gets replaced
    /// by this machine's own not-yet-drawn app instance, and without the
    /// latch the window would blink out and back.
    was_presented: std::cell::Cell<bool>,
    /// Readiness of this occupant, separate from the tile's visibility.
    /// Reset on replacement so a visible notice cannot mark an app ready.
    content_was_presented: std::cell::Cell<bool>,
    panic_scope: std::cell::Cell<Option<u64>>,
    hosted_window: Option<crate::hosted_window::HostedWindow>,
    rect: Rect,
    declared_arrival_shape: Option<super::SkeletonShape>,
    /// How far [`Self::offset_layout`] moved the tile from where layout put
    /// it. The content keeps its own layout rect, so it is drawn this much
    /// further right instead.
    content_shift: f64,
    content_damage: ContentDamage,
    pub focused: bool,
    pointer: Option<Point>,
    flush_edges: bool,
    /// The window's frame is growing past what this tile last drew (a
    /// window maximizing keeps its last layout until its frame rests): the
    /// frame's ground continues the tile, so no edge marks where it ends.
    inside_growing_frame: bool,
    edge_by_card: bool,
    full_bleed: bool,
    square_bottom: bool,
    spawned_at: Option<Instant>,
    held: Option<held::HeldContent>,
    /// The arrival of content whose first frame is not drawn yet
    /// ([`arrival`]): from when it was asked for until its first frame has
    /// faded in.
    arrival: Option<arrival::TileArrival>,
    content_presented_at: std::cell::Cell<Option<Instant>>,
    arrival_motion: EffectiveMotion,
}

/// While a tile's application runs: its tab named to a panic hook, and its
/// window current for its bar's slots.
struct Running {
    _panic: super::panic_scope::Scope,
    _window: crate::window_toolbar::WindowScope,
}

impl TileHost {
    /// Move the tile by `dx`; see
    /// [`super::window_group::WindowGroup::offset_layout`]. The content keeps
    /// the rect it was laid out at and is drawn translated instead.
    pub(crate) fn offset_layout(&mut self, dx: f64) {
        if dx == 0.0 {
            return;
        }
        self.rect = self.rect + vello::kurbo::Vec2::new(dx, 0.0);
        self.content_shift += dx;
    }

    #[must_use]
    pub fn content_shift(&self) -> f64 {
        self.content_shift
    }

    fn install_live_custody(&mut self, attachment: crate::control_custody::CustodyAttachment) {
        let Some(parent) = self.custody_parent.clone() else {
            return;
        };
        let owner = self.history_owner();
        let domain = match owner
            .as_ref()
            .and_then(crate::history_mode::HistoryOwner::component_binding)
        {
            Some(binding) => binding.control_domain.upgrade(),
            None if self.has_separate_document_owner() => None,
            None => Some(self.history_lifetime.control_domain.clone()),
        };
        if let Some(domain) = domain {
            if owner.is_none() && domain.inherit_owner(&parent).is_err() {
                parent.unsafe_attachment();
                return;
            }
            let scope = self.custody_scope.get_or_insert_with(|| parent.child());
            if scope
                .attach_to(&parent)
                .and_then(|()| domain.attach_to(scope))
                .is_err()
            {
                parent.unsafe_attachment();
                return;
            }
            let attachment = self.custody_renewal.take().unwrap_or(attachment);
            if let TileContent::App(root) = &mut self.content {
                crate::widget::controls::install_control_custody(
                    root.as_mut(),
                    &domain,
                    attachment,
                );
            }
            if let Some(history) = &mut self.history {
                history.set_control_domain(&domain);
            }
        }
    }

    pub fn take_host_intent(&mut self) -> Option<crate::widget::AppHostIntent> {
        match &mut self.content {
            TileContent::App(root) => root.take_host_intent(),
            _ => None,
        }
    }

    pub fn mount_component(
        mut component: crate::application::AppComponent,
        sink: impl Fn(crate::wake::WakeEvent) + Send + Sync + 'static,
    ) -> Result<Self, crate::launch::LaunchError> {
        if let Err(error) = component.mount(sink) {
            return Err(component.setup_error(error));
        }
        Ok(Self::from_component(component))
    }

    pub fn from_component(mut component: crate::application::AppComponent) -> Self {
        let registration = component.history.take();
        let mut tile = Self::new(TileContent::App(Box::new(
            crate::widget::app_widget::BoundAppWidget::new(component),
        )));
        if let Some(registration) = registration {
            tile.install_history_registration(registration);
        }
        tile
    }

    /// A desktop window hosts this tile; `tab_presents` says whether the
    /// window's tab row shows (and so presents the History control). The
    /// tile then never draws a History band of its own: closed, History is
    /// opened from the tab row, or from the app's header row while the tab
    /// row is hidden.
    pub fn set_window_presents_history(&mut self, tab_presents: bool) {
        if self.window_history == Some(tab_presents) {
            return;
        }
        self.window_history = Some(tab_presents);
        if let Some(history) = &mut self.history {
            history.remove_fallback_bar();
            history.set_open_button(!tab_presents);
            history.set_window_presents(tab_presents);
            self.structure_dirty = true;
        }
        if let Some(app) = self.app_history_mut() {
            app.set_window_presents_history(tab_presents);
        }
    }

    fn app_history(&self) -> Option<&dyn crate::widget::WindowHistory> {
        match &self.content {
            TileContent::App(app) => super::capability::capability::<
                dyn crate::widget::WindowHistory,
            >(app.as_ref() as &dyn Widget),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => None,
        }
    }

    fn app_history_mut(&mut self) -> Option<&mut (dyn crate::widget::WindowHistory + 'static)> {
        match &mut self.content {
            TileContent::App(app) => super::capability::capability_mut::<
                dyn crate::widget::WindowHistory,
            >(app.as_mut() as &mut dyn Widget),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => None,
        }
    }

    /// Queue `invocation` as one of the hosted app's own controls: an action
    /// the person chose over this window (its in-app search) runs inside the
    /// app, under the app's custody and permissions, exactly as clicking the
    /// control in it would. The outer document never resolves an app's
    /// actions by address, so this is the only way such a choice reaches
    /// the app.
    pub fn queue_control(
        &mut self,
        invocation: tilo_actions::ControlInvocation,
    ) -> Result<(), tilo_actions::ControlInvocation> {
        let Some(binding) = crate::widget::Widget::callback_binding(self) else {
            return self.run_in_own_document(invocation);
        };
        let Ok(_entry) = binding.enter() else {
            return Err(invocation);
        };
        let mut queued = binding.queued_controls.borrow_mut();
        if queued.len() >= 64 {
            queued.pop_front();
        }
        queued.push_back(invocation);
        Ok(())
    }

    pub fn visit_search_root(
        &mut self,
        visit: &mut dyn FnMut(&mut dyn Widget) -> bool,
    ) -> Option<bool> {
        let TileContent::App(app) = &mut self.content else {
            return None;
        };
        let app = app.as_mut() as &mut dyn Widget;
        let runtime =
            super::capability::capability::<dyn crate::widget::SearchableWindow>(&*app).is_some();
        let changed = if runtime {
            super::capability::capability_mut::<dyn crate::widget::SearchableWindow>(app)
                .is_some_and(|window| window.visit_search_root(visit))
        } else {
            visit(app)
        };
        Some(changed)
    }

    pub fn in_place_search(&mut self) -> crate::widget::InPlace {
        let mut found = crate::widget::InPlace::Nothing;
        let _ = self.visit_search_root(&mut |root| {
            found = crate::widget::InPlace::of(root);
            false
        });
        found
    }

    fn node_style(holds_app: bool) -> Style {
        let fill = Size {
            width: Dimension::percent(1.0),
            height: Dimension::percent(1.0),
        };
        if !holds_app {
            return Style {
                size: fill,
                ..Default::default()
            };
        }
        Style {
            size: fill,
            // The window gives its app its room; what the app shows never
            // sets a floor under it. With the automatic minimum, a page whose
            // document was taller than the window kept the tile from making
            // way for the window's bar, and pushed the window past its strip.
            min_size: Size {
                width: Dimension::length(0.0),
                height: Dimension::length(0.0),
            },
            ..Default::default()
        }
    }

    fn build_content(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> Option<NodeId> {
        self.live_callback_node = None;
        let TileContent::App(w) = &mut self.content else {
            return None;
        };
        let child = if let Some(history) = &mut self.history {
            history.build_composition(w.as_mut(), taffy, theme, node_map)
        } else {
            w.build_layout(taffy, theme, node_map)
        };
        self.live_callback_node = self
            .history
            .as_ref()
            .map_or(Some(child), |history| history.live_layout_node());
        Some(child)
    }

    pub(crate) fn discard_layout_nodes(&mut self) {
        self.live_callback_node = None;
        self.node = None;
        self.structure_dirty |= std::mem::take(&mut self.remount);
        if let Some(history) = &mut self.history {
            history.discard_layout_nodes();
        }
    }
    pub fn register_document_history(
        &mut self,
        definition: &'static crate::history_mode::HistoryDefinition,
    ) -> Result<(), crate::history_mode::HistoryRegistrationError> {
        use crate::history_mode::{
            HistoryPresentation, HistoryRegistration, HistoryRegistrationError,
            HistorySourceRegistration,
        };
        self.prepare_record_resolution()
            .map_err(|_| HistoryRegistrationError::UnsupportedOwner)?;
        let owner = self
            .history_owner()
            .ok_or(HistoryRegistrationError::UnsupportedOwner)?;
        let sources = definition
            .scopes
            .iter()
            .map(|scope| match scope.presentation {
                HistoryPresentation::Domain { .. }
                | HistoryPresentation::DomainWithAssets { .. } => {
                    HistorySourceRegistration::owned(scope.key, definition.app_id, owner.clone())
                }
                HistoryPresentation::External { reason } => {
                    Ok(HistorySourceRegistration::Unversioned {
                        scope: scope.key,
                        reason,
                    })
                }
                HistoryPresentation::CheckedFiles { .. } => {
                    Err(HistoryRegistrationError::UnsupportedOwner)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.install_history_registration(HistoryRegistration::new(definition, sources)?);
        if self.record_error.is_some() {
            return Err(HistoryRegistrationError::UnsupportedOwner);
        }
        Ok(())
    }

    pub fn new(content: TileContent) -> Self {
        let registration = match &content {
            TileContent::App(root) => {
                crate::history_mode::capability::history_registration(root.as_ref())
            }
            _ => Ok(None),
        };
        let mut tile = Self {
            content,
            history: None,
            pending_history: None,
            history_opening: false,
            window_history: None,
            live_callback_node: None,
            retained_invocations: HashMap::new(),
            retained_updates: HashMap::new(),
            component_invocations: std::collections::HashSet::new(),
            owner_bound_work: false,
            owner_bound_custody: None,
            close_reports: vec![],
            history_lifetime: crate::history_mode::owner::RecordOwnerLifetime::new(),
            custody_parent: None,
            custody_scope: None,
            custody_renewal: None,
            record_error: None,
            error_card: None,
            record_transition_error: None,
            structure_dirty: false,
            node: None,
            remount: false,
            was_presented: std::cell::Cell::new(false),
            content_was_presented: std::cell::Cell::new(false),
            panic_scope: std::cell::Cell::new(None),
            hosted_window: None,
            rect: Rect::ZERO,
            declared_arrival_shape: None,
            content_shift: 0.0,
            content_damage: ContentDamage::new(),
            focused: false,
            pointer: None,
            flush_edges: false,
            inside_growing_frame: false,
            edge_by_card: false,
            full_bleed: false,
            square_bottom: false,
            spawned_at: None,
            held: None,
            arrival: None,
            content_presented_at: std::cell::Cell::new(None),
            arrival_motion: EffectiveMotion::default(),
        };
        match registration {
            Ok(Some(registration)) => tile.install_history_registration(registration),
            Err(error) => tile.record_error = Some(format!("History unavailable: {error}")),
            Ok(None) => {}
        }
        tile.arrival = tile.begin_arrival(None);
        tile
    }

    #[must_use]
    pub fn asking_for_input(&self) -> bool {
        match &self.content {
            TileContent::App(app) => app.ime_content().is_some(),
            _ => false,
        }
    }

    #[must_use]
    pub fn with_arrival_shape(mut self, shape: super::SkeletonShape) -> Self {
        self.declared_arrival_shape = Some(shape);
        self
    }

    #[must_use]
    pub fn arriving_from(mut self, from: crate::arriving::Source) -> Self {
        if let Some(arrival) = &self.arrival
            && !arrival.revealed()
        {
            self.arrival = Some(arrival.answered_from(from));
        }
        self
    }

    fn begin_arrival(&self, since: Option<Instant>) -> Option<arrival::TileArrival> {
        let pending = !self.content_presented();
        if !pending && since.is_none() {
            return None;
        }
        let from = match &self.content {
            TileContent::App(app) => app.arrives_from(),
            _ => crate::arriving::Source::Local,
        };
        let since = since.unwrap_or_else(crate::time::now);
        let mut arrival =
            arrival::TileArrival::new(crate::arriving::ArrivalClock::new(since, from));
        if !pending {
            arrival.arrive(crate::time::now());
        }
        Some(arrival)
    }

    fn content_presented(&self) -> bool {
        let _ = Widget::presented(self);
        self.content_was_presented.get()
    }

    /// The arrival as of now: with the content's first frame landed when it
    /// has, which a query that cannot change the tile reads.
    fn arrival_now(&self) -> Option<arrival::TileArrival> {
        let mut arrival = self.arrival.clone()?;
        if self.content_presented() {
            arrival.arrive(
                self.content_presented_at
                    .get()
                    .unwrap_or_else(crate::time::now),
            );
        }
        Some(arrival)
    }

    fn advance_arrival(&mut self, now: Instant) {
        let presented = self.content_presented();
        let at = self.content_presented_at.get();
        let motion = self.arrival_motion;
        let Some(arrival) = self.arrival.as_mut() else {
            return;
        };
        if presented {
            arrival.arrive(at.unwrap_or(now));
        }
        arrival.reveal_when_due(motion, now);
        if arrival.paint_at(now).is_none() {
            self.arrival = None;
        }
    }

    fn update_content(&mut self, s: &AppModel, p: &ModelPath) -> bool {
        let _running = self.running();
        if !self.history_lifetime.active.get()
            || crate::history_mode::frame::FrameworkInputFrame::active()
        {
            return false;
        }
        if self.prepare_record_resolution().is_err() {
            return false;
        }
        self.install_live_custody(crate::control_custody::CustodyAttachment::Dynamic);
        if self.record_error.is_some() {
            return false;
        }
        let TileContent::App(w) = &mut self.content else {
            return false;
        };
        w.update(s, p)
    }

    /// The placeholder step standing in the tile at `now`, while one does:
    /// the content's first frame is not shown yet.
    #[must_use]
    pub fn arrival_step(&self, now: Instant) -> Option<crate::arriving::Step> {
        let arrival = self.arrival_now()?;
        let due = arrival.clock().shows_at().is_some_and(|at| now >= at);
        (!arrival.revealed() && !due).then(|| arrival.placeholder_step(now))
    }

    /// Whether the tile still stands for content whose first frame is not
    /// shown: its placeholder (or the ground it reserves) is what is there.
    #[must_use]
    pub fn is_arriving(&self) -> bool {
        self.arrival_step(crate::time::now()).is_some()
    }

    #[must_use]
    pub fn holds_first_frame_until(&self, now: Instant) -> Option<Instant> {
        if self.content_presented() {
            return None;
        }
        self.arrival
            .as_ref()
            .filter(|arrival| !arrival.revealed())
            .and_then(|arrival| arrival.clock().holds_frame_until(now))
    }

    pub(crate) fn set_flush_edges(&mut self, flush: bool) {
        self.flush_edges = flush;
    }

    pub(crate) fn set_inside_growing_frame(&mut self, inside: bool) {
        self.inside_growing_frame = inside;
    }

    pub(crate) fn set_edge_by_card(&mut self, by_card: bool) {
        self.edge_by_card = by_card;
    }

    /// Name `tab` to a panic hook while this tile's application runs: see
    /// [`panic_scope`](super::panic_scope).
    pub fn set_panic_scope(&self, tab: u64) {
        self.panic_scope.set(Some(tab));
    }

    fn running(&self) -> Running {
        Running {
            _panic: super::panic_scope::enter(self.panic_scope.get()),
            _window: crate::window_toolbar::enter(self.hosted_window.as_ref()),
        }
    }

    pub fn set_hosted_window(&mut self, window: crate::hosted_window::HostedWindow) {
        self.hosted_window = Some(window);
    }

    #[must_use]
    pub fn hosted_window(&self) -> Option<&crate::hosted_window::HostedWindow> {
        self.hosted_window.as_ref()
    }

    fn lays_out_in_place(&self) -> bool {
        self.history.is_none()
    }

    fn composes_live_app(&self) -> bool {
        self.history
            .as_ref()
            .is_none_or(|history| history.live_visible())
    }

    /// Stand where this tile stands from now on: `next`, built for the same
    /// window (an app whose code or record arrived where a placeholder held
    /// its place), goes on with the arrival ([`Self::continue_arrival`])
    /// and takes this tile's retained layout node: the app is laid out under
    /// that node in place and the rest of the host's tree is kept, unless a
    /// History around it is busy.
    pub fn take_place_of(&mut self, mut next: Self) {
        next.continue_arrival(self);
        next.node = self.node;
        next.remount = self.node.is_some() && next.lays_out_in_place();
        next.structure_dirty = !next.remount;
        *self = next;
    }

    /// Built in place of `previous` (an app whose code or record just
    /// arrived, in the window that reserved its place): while `previous`
    /// still stood for content not shown, this host goes on with that
    /// arrival, so its placeholder keeps its timing and the app cross-fades
    /// in over it instead of the window cutting or starting over.
    pub fn continue_arrival(&mut self, previous: &Self) {
        let Some(standing) = previous
            .arrival
            .as_ref()
            .filter(|arrival| !arrival.revealed())
        else {
            return;
        };
        let since = standing.clock().since();
        let from = match &self.content {
            TileContent::App(app) if !self.content_presented() => app.arrives_from(),
            _ => standing.clock().source().clone(),
        };
        let mut arrival =
            arrival::TileArrival::new(crate::arriving::ArrivalClock::new(since, from));
        if self.content_presented() {
            arrival.arrive(
                self.content_presented_at
                    .get()
                    .unwrap_or_else(crate::time::now),
            );
        }
        self.arrival = Some(arrival);
    }

    #[must_use]
    pub fn record_transition_error(&self) -> Option<&str> {
        self.record_transition_error.as_deref()
    }

    fn collect_retained_updates_before_release(&mut self) -> Result<(), String> {
        let invocations = self
            .retained_invocations
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for invocation in invocations {
            if self
                .retained_updates
                .get(&invocation)
                .is_some_and(|updates| !updates.is_empty())
            {
                return Err(
                    "A retained operation update must be delivered before switching documents"
                        .into(),
                );
            }
            let Some(owner) = self.retained_invocations.get(&invocation).cloned() else {
                continue;
            };
            owner
                .validate()
                .map_err(|error| format!("Outgoing document observation unavailable: {error}"))?;
            let (store, checkout) = owner.store_checkout();
            let TileContent::App(root) = &mut self.content else {
                continue;
            };
            let update = store
                .read_checked(&checkout.id, |document| {
                    crate::widget::poll_resource_operation(
                        root.as_mut(),
                        invocation,
                        false,
                        document,
                        &ROOT,
                    )
                })
                .map_err(|error| format!("Outgoing document observation unavailable: {error}"))?;
            if let Some(update) = update {
                let terminal = update.is_terminal();
                self.retained_updates
                    .entry(invocation)
                    .or_default()
                    .push_back(update);
                if terminal {
                    self.retained_invocations.remove(&invocation);
                } else {
                    return Err(
                        "A retained operation update must be delivered before switching documents"
                            .into(),
                    );
                }
            }
        }
        Ok(())
    }

    fn take_retained_update(
        &mut self,
        invocation: tilo_actions::InvocationId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        let update = self.retained_updates.get_mut(&invocation)?.pop_front();
        if self
            .retained_updates
            .get(&invocation)
            .is_some_and(VecDeque::is_empty)
        {
            self.retained_updates.remove(&invocation);
        }
        update
    }

    fn unavailable_update(
        invocation: tilo_actions::InvocationId,
    ) -> tilo_actions::InvocationUpdate {
        tilo_actions::InvocationUpdate::Failed {
            invocation,
            error: tilo_actions::ActionFailure {
                code: "owner_unavailable".into(),
                message: "The admitted operation's document owner is no longer available.".into(),
                details: None,
                retryable: false,
            },
        }
    }

    fn prepare_owner_transition(&mut self) -> Result<(), String> {
        if !self.owner_bound_work {
            self.record_transition_error = None;
            return Ok(());
        }
        if !self.history_lifetime.active.get() {
            let error = "The current document owner is closed".to_string();
            self.record_transition_error = Some(error.clone());
            return Err(error);
        }
        if let Err(error) = self.collect_retained_updates_before_release() {
            self.record_transition_error = Some(error.clone());
            return Err(error);
        }
        let release = match &mut self.content {
            TileContent::App(root) => {
                crate::record::capability::release_record_scope(root.as_mut())
            }
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => Ok(()),
        };
        if let Err(error) = release {
            self.record_transition_error = Some(error.clone());
            return Err(error);
        }
        for invocation in self
            .retained_invocations
            .drain()
            .map(|(invocation, _)| invocation)
        {
            let updates = self.retained_updates.entry(invocation).or_default();
            if !updates
                .iter()
                .any(tilo_actions::InvocationUpdate::is_terminal)
            {
                updates.push_back(Self::unavailable_update(invocation));
            }
        }
        self.owner_bound_work = false;
        self.owner_bound_custody = None;
        self.record_transition_error = None;
        Ok(())
    }

    fn current_binding_owner(&self) -> Option<crate::history_mode::HistoryOwner> {
        self.callback_binding()
            .map(crate::history_mode::HistoryOwner::component)
    }

    fn owner_bound_custody_changed(&self) -> bool {
        self.owner_bound_work
            && self.owner_bound_custody.as_ref().is_some_and(|admitted| {
                self.current_binding_owner()
                    .is_none_or(|current| !current.same_owner(admitted))
            })
    }

    fn renew_history_lifetime(&mut self) {
        self.history_lifetime.control_domain.close();
        self.history_lifetime.active.set(false);
        let previous = self.history_lifetime.control_domain.clone();
        self.history_lifetime = crate::history_mode::owner::RecordOwnerLifetime::new();
        self.custody_renewal = Some(self.history_lifetime.control_domain.renewal_from(&previous));
    }

    fn event_is_for_this_tile(&self, event: &WindowEvent, scale_factor: f64) -> bool {
        match event {
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_) => self.focused,
            WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. } => self.pointer.is_some_and(|p| self.rect.contains(p)),
            WindowEvent::Touch(touch) => self.rect.contains(Point::new(
                touch.location.x / scale_factor,
                touch.location.y / scale_factor,
            )),
            _ => false,
        }
    }

    fn prepare_record_resolution(&mut self) -> Result<(), String> {
        if !self.history_lifetime.active.get() {
            let error = "The current document owner is closed".to_string();
            self.record_transition_error = Some(error.clone());
            return Err(error);
        }
        if self.owner_bound_custody_changed() {
            self.prepare_owner_transition()?;
        }
        Ok(())
    }

    pub fn kind_string(&self) -> String {
        match &self.content {
            TileContent::App(_) => "App".into(),
            TileContent::Stub { kind, .. } => kind.as_str().into(),
            TileContent::Notice { title, .. } => title.clone(),
            TileContent::Arriving => "Arriving".into(),
        }
    }
    pub fn app_mut(&mut self) -> Option<&mut (dyn AppWidget + 'static)> {
        match &mut self.content {
            TileContent::App(app) => Some(app.as_mut()),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => None,
        }
    }
    /// Mutable legacy content access. Canonical components use checked replacement.
    ///
    /// Tests only: input and per-frame paths reach the app through
    /// [`Self::app_mut`] and replace it through [`Self::replace_content`] or
    /// [`Self::replace_component`]. A window's edge drag once went through
    /// here and turned an ordinary resize into this panic.
    /// # Panics
    /// Refuses extraction of a canonical component through the legacy content API.
    #[cfg(test)]
    pub(crate) fn content_mut(&mut self) -> &mut TileContent {
        assert!(
            self.callback_binding().is_none(),
            "canonical content requires checked replacement"
        );
        self.renew_history_lifetime();
        self.structure_dirty = true;
        &mut self.content
    }
    #[must_use]
    pub fn replace_component(
        &mut self,
        mut component: crate::application::AppComponent,
    ) -> Vec<crate::application::ComponentCloseReport> {
        let history = component.history.take();
        self.replace_content(TileContent::App(Box::new(
            crate::widget::app_widget::BoundAppWidget::new(component),
        )));
        if let Some(history) = history {
            self.install_history_registration(history);
        }
        crate::component_lifecycle::take_component_close_reports(self)
    }
    pub fn replace_content(&mut self, content: TileContent) {
        if let TileContent::App(root) = &mut self.content {
            self.close_reports
                .extend(crate::component_lifecycle::close_components(root.as_mut()));
            self.close_reports
                .extend(crate::component_lifecycle::take_component_close_reports(
                    root.as_mut(),
                ));
        }
        self.renew_history_lifetime();
        if let Some(window) = &self.hosted_window {
            window.declare(None);
        }
        self.retained_invocations.clear();
        self.component_invocations.clear();
        self.retained_updates.clear();
        self.owner_bound_work = false;
        self.owner_bound_custody = None;
        self.record_transition_error = None;
        self.content = content;
        self.install_live_custody(crate::control_custody::CustodyAttachment::Dynamic);
        let registration = match &self.content {
            TileContent::App(root) => {
                crate::history_mode::capability::history_registration(root.as_ref())
            }
            _ => Ok(None),
        };
        self.history = None;
        self.pending_history = None;
        match registration {
            Ok(Some(registration)) => self.install_history_registration(registration),
            Err(error) => self.record_error = Some(format!("History unavailable: {error}")),
            Ok(None) => {}
        }
        if let Some(tab_presents) = self.window_history
            && let Some(app) = self.app_history_mut()
        {
            app.set_window_presents_history(tab_presents);
        }
        let standing = self
            .arrival
            .as_ref()
            .filter(|arrival| !arrival.revealed())
            .map(|arrival| arrival.clock().since());
        self.content_was_presented.set(false);
        self.content_presented_at.set(None);
        self.arrival = self.begin_arrival(standing);
        self.install_live_custody(crate::control_custody::CustodyAttachment::Dynamic);
        self.structure_dirty = true;
    }
    pub fn content(&self) -> &TileContent {
        &self.content
    }
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    pub fn start_spawn_animation(&mut self) {
        self.spawned_at = Some(crate::time::now());
    }

    pub fn spawn_in_flight(&self) -> bool {
        match self.spawned_at {
            Some(t) => crate::time::now().saturating_duration_since(t) < spawn_time(),
            None => false,
        }
    }

    /// Desktop-space card rect for an app-provided complete semantic canvas.
    /// The provider reports tile-content size; the shell card grows from the
    /// live tile's top-left and never shrinks below the live window surface.
    pub fn complete_content_rect(&self) -> Option<Rect> {
        if let Some(held) = self.held.as_ref().filter(|_| self.content_held()) {
            return self.held_complete_rect(held);
        }
        self.live_complete_content_rect()
    }

    fn live_complete_content_rect(&self) -> Option<Rect> {
        let TileContent::App(app) = &self.content else {
            return None;
        };
        let size = app.complete_content_size()?;
        if !size.width.is_finite()
            || !size.height.is_finite()
            || size.width <= 0.0
            || size.height <= 0.0
        {
            return None;
        }
        Some(Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x0 + size.width.max(self.rect.width()),
            self.rect.y0 + size.height.max(self.rect.height()),
        ))
    }

    /// Paint a complete app-provided canvas in the ordinary window card. The
    /// app owns only its semantic pixels; shared clipping, corner radius and
    /// focus edge remain toolkit code, exactly as on screen.
    pub fn draw_complete_content_capture(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) -> Option<Rect> {
        if self.content_held() {
            return self.draw_held_complete_content(scene, transform);
        }
        let rect = self.live_complete_content_rect()?;
        let radius = f64::from(theme.tokens.radii.tile);
        let shape = RoundedRect::from_rect(rect, radius);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.content,
            None,
            &shape,
        );
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &shape,
        );
        let painted = match &mut self.content {
            TileContent::App(app) => {
                app.draw_complete_content(scene, rect, transform, tr, theme, presence)
            }
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => false,
        };
        scene.pop_layer();
        if !painted {
            return None;
        }
        if !self.inside_growing_frame && !self.edge_by_card {
            self.draw_edge_at(scene, transform, theme, radius, rect);
        }
        Some(rect)
    }

    #[cfg(test)]
    pub fn spawn_alpha(&self) -> Option<f32> {
        self.spawned_at.and_then(|t| {
            let elapsed = crate::time::now().saturating_duration_since(t);
            (elapsed < spawn_time()).then(|| crate::motion::unit_opacity(spawn_progress(elapsed)))
        })
    }
}

impl Continuable for TileHost {
    fn continuation_metadata(
        &self,
        state: &AutoCommit,
        path: &ObjId,
    ) -> Option<crate::collaboration::ContinuationMetadata> {
        match &self.content {
            TileContent::App(app) => {
                crate::widget::capability::capability::<dyn Continuable>(&**app)
                    .and_then(|continuable| continuable.continuation_metadata(state, path))
            }
            _ => None,
        }
    }

    fn restore_continuation_metadata(
        &mut self,
        metadata: &crate::collaboration::ContinuationMetadata,
        state: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> bool {
        match &mut self.content {
            TileContent::App(app) => {
                crate::widget::capability::capability_mut::<dyn Continuable>(&mut **app)
                    .is_some_and(|continuable| {
                        continuable.restore_continuation_metadata(metadata, state, path)
                    })
            }
            _ => false,
        }
    }

    fn restore_live_continuation_metadata(
        &mut self,
        metadata: &crate::collaboration::ContinuationMetadata,
        state: &AutoCommit,
        path: &ObjId,
    ) -> bool {
        match &mut self.content {
            TileContent::App(app) => {
                crate::widget::capability::capability_mut::<dyn Continuable>(&mut **app)
                    .is_some_and(|continuable| {
                        continuable.restore_live_continuation_metadata(metadata, state, path)
                    })
            }
            _ => false,
        }
    }
}

impl FloatingSurfaceProvider for TileHost {
    fn on_floating_surface_focus_changed(&mut self, id: Option<&str>, focused: bool) {
        if let TileContent::App(widget) = &mut self.content {
            widget.on_floating_surface_focus_changed(id, focused);
        }
    }

    fn on_floating_surface_ime_source_changed(&mut self, id: Option<&str>) {
        if let TileContent::App(widget) = &mut self.content {
            widget.on_floating_surface_ime_source_changed(id);
        }
    }

    fn on_floating_surface_event_source(&mut self, id: Option<&str>) {
        if let TileContent::App(widget) = &mut self.content {
            widget.on_floating_surface_event_source(id);
        }
    }

    fn floating_surface_event(
        &mut self,
        id: &str,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        match &mut self.content {
            TileContent::App(widget) => widget.floating_surface_event(id, event, ctx),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => false,
        }
    }

    fn floating_surface_accessibility(&self, id: &str) -> Option<crate::a11y::AccessNode> {
        match &self.content {
            TileContent::App(widget) => widget.floating_surface_accessibility(id),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => None,
        }
    }

    fn perform_floating_surface_accessibility_action(
        &mut self,
        id: &str,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> bool {
        match &mut self.content {
            TileContent::App(widget) => widget
                .perform_floating_surface_accessibility_action(id, target, action, state, path),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => false,
        }
    }

    fn floating_surface_ime(&self, id: &str) -> Option<(crate::ime::InputContent, Rect)> {
        match &self.content {
            TileContent::App(widget) => widget.floating_surface_ime(id),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => None,
        }
    }

    fn take_floating_surface_request(
        &mut self,
    ) -> Option<crate::floating_surface::FloatingSurfaceRequest> {
        match &mut self.content {
            TileContent::App(widget) => widget.take_floating_surface_request(),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => None,
        }
    }

    fn take_capture_studio_request(&mut self) -> Option<super::CaptureMode> {
        match &mut self.content {
            TileContent::App(widget) => widget.take_capture_studio_request(),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => None,
        }
    }

    fn take_floating_surface_source_updates(&mut self) -> Vec<(String, Rect)> {
        match &mut self.content {
            TileContent::App(widget) => widget.take_floating_surface_source_updates(),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => {
                Vec::new()
            }
        }
    }

    fn draw_floating_surfaces(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let TileContent::App(widget) = &mut self.content {
            widget.draw_floating_surfaces(scene, transform, text_renderer, theme, presence);
        }
    }

    fn draw_floating_surface(
        &mut self,
        id: &str,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) -> bool {
        match &mut self.content {
            TileContent::App(widget) => {
                widget.draw_floating_surface(id, scene, transform, text_renderer, theme, presence)
            }
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => false,
        }
    }

    fn set_floating_surface_bounds(&mut self, bounds: Rect) {
        if let TileContent::App(widget) = &mut self.content {
            widget.set_floating_surface_bounds(bounds);
        }
    }

    fn floating_surface_presentations(
        &self,
    ) -> Vec<crate::floating_surface::FloatingSurfacePresentation> {
        match &self.content {
            TileContent::App(widget) => widget.floating_surface_presentations(),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => {
                Vec::new()
            }
        }
    }

    fn active_floating_surface_ids(&self) -> Vec<String> {
        match &self.content {
            TileContent::App(widget) => widget.active_floating_surface_ids(),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => {
                Vec::new()
            }
        }
    }

    fn on_floating_surface_host_message(
        &mut self,
        message: &crate::floating_surface::FloatingSurfaceHostMessage,
    ) {
        if let TileContent::App(widget) = &mut self.content {
            widget.on_floating_surface_host_message(message);
        }
    }
}

impl AudiencePresentationProvider for TileHost {
    fn audience_presentation_request(
        &self,
    ) -> Option<crate::fullscreen::AudiencePresentationRequest> {
        match &self.content {
            TileContent::App(app) => app.audience_presentation_request(),
            _ => None,
        }
    }

    fn on_audience_presentation_changed(&mut self, active: bool) {
        if let TileContent::App(app) = &mut self.content {
            app.on_audience_presentation_changed(active);
        }
    }

    fn draw_audience_presentation(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) -> bool {
        match &mut self.content {
            TileContent::App(app) => {
                app.draw_audience_presentation(scene, transform, text_renderer, theme, rect)
            }
            _ => false,
        }
    }

    fn handle_audience_presentation_event(
        &mut self,
        event: &WindowEvent,
        state: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> bool {
        match &mut self.content {
            TileContent::App(app) => app.handle_audience_presentation_event(event, state, path),
            _ => false,
        }
    }

    fn on_audience_presentation_closed(&mut self) {
        if let TileContent::App(app) = &mut self.content {
            app.on_audience_presentation_closed();
        }
    }
}

impl FullscreenProvider for TileHost {
    fn fullscreen_intent(&self) -> Option<bool> {
        match &self.content {
            TileContent::App(app) => app.fullscreen_intent(),
            _ => None,
        }
    }

    fn fullscreen_params(&self) -> crate::fullscreen::FullscreenRequest {
        match &self.content {
            TileContent::App(app) => app.fullscreen_params(),
            _ => crate::fullscreen::FullscreenRequest::mirror(
                crate::fullscreen::FullscreenTarget::Largest,
            ),
        }
    }

    fn on_fullscreen_changed(&mut self, active: bool) {
        if let TileContent::App(app) = &mut self.content {
            app.on_fullscreen_changed(active);
        }
    }
}

impl Widget for TileHost {
    crate::provides!(
        dyn crate::widget::FullscreenProvider,
        dyn crate::widget::Continuable,
        dyn crate::widget::FloatingSurfaceProvider,
        dyn crate::widget::AudiencePresentationProvider,
        dyn crate::collaboration::participants::CollaborationTopology,
        dyn crate::history_mode::capability::FrameworkHistoryTopology,
        dyn crate::widget::controls::ActionControls,
        dyn crate::search::capability::SearchSource,
        dyn crate::view::capability::ViewState,
        dyn crate::component_lifecycle::ComponentLifecycle,
        dyn crate::widget::ResourceOperations
    );

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn primary_vertical_scroll(&self) -> Option<crate::widget::PrimaryScrollRegion> {
        if self
            .history
            .as_ref()
            .is_some_and(|history| !history.live_interactive())
        {
            return None;
        }
        if let TileContent::App(app) = &self.content {
            app.primary_vertical_scroll()
        } else {
            None
        }
    }
    fn apply_primary_vertical_scroll(
        &mut self,
        action: crate::widget::PrimaryScrollAction,
    ) -> crate::widget::PrimaryScrollResult {
        if self
            .history
            .as_ref()
            .is_some_and(|history| !history.live_interactive())
        {
            return crate::widget::PrimaryScrollResult::Unsupported;
        }
        if let TileContent::App(app) = &mut self.content {
            app.apply_primary_vertical_scroll(action)
        } else {
            crate::widget::PrimaryScrollResult::Unsupported
        }
    }

    fn resets_binding_inheritance(&self) -> bool {
        self.history.is_some()
    }
    fn inactive_bound_dispatch_boundary(&self) -> Option<NodeId> {
        self.history
            .as_ref()
            .filter(|history| !history.live_interactive())
            .and_then(|history| history.live_layout_node())
    }
    #[allow(
        private_interfaces,
        reason = "toolkit-only component request preserves hidden live lifecycle without exposing root/registry loans"
    )]
    fn visit_components(
        &mut self,
        request: &mut crate::bound_dispatch::ComponentRequest<'_>,
    ) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
        let _running = self.running();
        if let TileContent::App(live) = &mut self.content {
            live.visit_components(request)
        } else {
            Ok(())
        }
    }

    #[allow(
        private_interfaces,
        reason = "finite toolkit resource ownership requests"
    )]
    fn visit_resource_owners(
        &self,
        request: &mut crate::bound_dispatch::local_target::ResourceReadRequest<'_>,
    ) -> Result<(), tilo_actions::DoorError> {
        if let TileContent::App(live) = &self.content {
            request.tile(
                self.history.as_ref().is_none_or(|h| h.live_interactive()),
                live.as_ref(),
            )
        } else {
            Ok(())
        }
    }
    fn mount_native_views(&mut self, host: &crate::widget::native_view::NativeViewHost<'_>) {
        let _running = self.running();
        if let TileContent::App(live) = &mut self.content {
            if self
                .history
                .as_ref()
                .is_none_or(|history| history.live_interactive())
            {
                live.mount_native_views(host);
            } else {
                live.unmount_native_views();
            }
        }
    }
    fn unmount_native_views(&mut self) {
        if let TileContent::App(live) = &mut self.content {
            live.unmount_native_views();
        }
    }
    fn native_view_frame_start(&mut self) {
        let _running = self.running();
        if self
            .history
            .as_ref()
            .is_none_or(|history| history.live_interactive())
            && let TileContent::App(live) = &mut self.content
        {
            live.native_view_frame_start();
        }
    }
    fn on_external_event(
        &mut self,
        payload: &str,
        state: &mut crate::widget::Edits<'_>,
        root: &ModelPath,
    ) {
        let _running = self.running();
        if let TileContent::App(app) = &mut self.content {
            app.on_external_event(payload, state, root);
        }
    }
    fn take_focus_reset(&mut self) -> bool {
        match &mut self.content {
            TileContent::App(app) => app.take_focus_reset(),
            _ => false,
        }
    }
    fn take_app_activation(&mut self) -> Option<crate::application::InstanceId> {
        None
    }
    fn needs_idle_sync(&self) -> bool {
        let _running = self.running();
        match &self.content {
            TileContent::App(app) => app.needs_idle_sync(),
            _ => false,
        }
    }

    fn history_owner(&self) -> Option<crate::history_mode::HistoryOwner> {
        if self.record_error.is_some() || !self.history_lifetime.active.get() {
            return None;
        }
        self.callback_binding()
            .map(crate::history_mode::HistoryOwner::component)
    }

    fn has_separate_document_owner(&self) -> bool {
        self.callback_binding().is_some()
    }

    fn callback_binding(&self) -> Option<crate::bound_dispatch::CallbackBinding> {
        match &self.content {
            TileContent::App(widget) => widget.callback_binding(),
            _ => None,
        }
    }
    fn children(&self) -> Vec<&dyn Widget> {
        if self.record_error.is_some() {
            return vec![];
        }
        let mut children = vec![];
        if let Some(history) = &self.history {
            children.push(history.as_ref() as &dyn Widget);
        }
        if self
            .history
            .as_ref()
            .is_none_or(|history| history.live_interactive())
            && let TileContent::App(widget) = &self.content
        {
            children.push(widget.as_ref());
        }
        children
    }
    /// The app also while History keeps it inert.
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        if self.remount {
            return true;
        }
        if let Some(history) = &self.history {
            visitor(history.as_ref());
        }
        if let TileContent::App(widget) = &self.content {
            visitor(widget.as_ref());
        }
        true
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.record_error.is_some() {
            return vec![];
        }
        let interactive = self
            .history
            .as_ref()
            .is_none_or(|history| history.live_interactive());
        let mut children: Vec<&mut dyn Widget> = vec![];
        if let Some(history) = &mut self.history {
            children.push(history.as_mut());
        }
        if interactive && let TileContent::App(widget) = &mut self.content {
            children.push(widget.as_mut());
        }
        children
    }

    fn is_layout_dirty(&self) -> bool {
        if self.record_error.is_some() {
            return true;
        }
        let mut has_children = false;
        let mut dirty = false;
        self.for_each_child(&mut |child| {
            has_children = true;
            dirty = dirty || child.is_layout_dirty();
        });
        if has_children {
            dirty
        } else {
            self.rect == Rect::ZERO
        }
    }

    fn sync(&mut self, s: &AppModel, p: &ModelPath) {
        let _running = self.running();
        if !self.history_lifetime.active.get()
            || crate::history_mode::frame::FrameworkInputFrame::active()
        {
            return;
        }
        if let TileContent::App(widget) = &mut self.content
            && let Some(replacement) = crate::component_lifecycle::take_replacement(widget.as_mut())
        {
            let (component, outcome) = replacement.into_parts();
            outcome.answer(Ok(()));
            // The outgoing component answers the operations it admitted (the
            // open that asked) before it closes; the answers outlive it until
            // the host polls them, and work it cannot finish is reported
            // unavailable rather than inherited by the new component.
            let _ = self.collect_retained_updates_before_release();
            let mut answers = std::mem::take(&mut self.retained_updates);
            let (doc, path) = crate::doc::document_ref(s, p);
            for invocation in std::mem::take(&mut self.component_invocations) {
                if let TileContent::App(app) = &mut self.content
                    && let Some(update) = crate::widget::poll_resource_operation(
                        app.as_mut(),
                        invocation,
                        false,
                        doc,
                        path,
                    )
                {
                    answers.entry(invocation).or_default().push_back(update);
                }
                let updates = answers.entry(invocation).or_default();
                if !updates
                    .iter()
                    .any(tilo_actions::InvocationUpdate::is_terminal)
                {
                    updates.push_back(Self::unavailable_update(invocation));
                }
            }
            for invocation in self.retained_invocations.keys().copied() {
                let updates = answers.entry(invocation).or_default();
                if !updates
                    .iter()
                    .any(tilo_actions::InvocationUpdate::is_terminal)
                {
                    updates.push_back(Self::unavailable_update(invocation));
                }
            }
            let reports = self.replace_component(component);
            self.close_reports.extend(reports);
            self.retained_updates = answers;
        }
        if self.prepare_record_resolution().is_err() {
            return;
        }
        self.install_live_custody(crate::control_custody::CustodyAttachment::Dynamic);
        if self.record_error.is_some() {
            return;
        }
        if let TileContent::App(w) = &mut self.content {
            w.sync(s, p);
        }
    }

    fn settle(&mut self, edits: &mut crate::widget::Edits<'_>, path: &ObjId) {
        let _running = self.running();
        if !self.history_lifetime.active.get()
            || crate::history_mode::frame::FrameworkInputFrame::active()
            || self.record_error.is_some()
        {
            return;
        }
        if let TileContent::App(app) = &mut self.content {
            app.settle(edits, path);
        }
    }

    fn update(&mut self, s: &AppModel, p: &ModelPath) -> bool {
        let adopted = self.adopt_loaded_history();
        let progress = self.arrival_progress();
        let now = crate::motion::frame_now();
        let arrival = self
            .arrival_now()
            .is_some_and(|arrival| arrival.needs_redraw(now, progress));
        // The arrival's steps advance with time, not with drawing: a tile on
        // a strip that is not shown is never drawn, and a step that came due
        // there, unconsumed, would be due again on every wake, turning the
        // loop as fast as it can.
        self.advance_arrival(now);
        self.update_content(s, p) || arrival || adopted
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let _running = self.running();
        let node = match self.build_content(taffy, theme, node_map) {
            Some(child) => taffy.parent_node(Self::node_style(true), &[child]),
            None => taffy.leaf_node(Self::node_style(false)),
        };
        self.node = Some(node);
        self.remount = false;
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        // This build composed the current content and admission, so a change
        // flagged before it (a rebind, a replacement) is satisfied. A host
        // that inspects the flag only after building must not discard the
        // maps it just made; a sync that replaces delegates after this build
        // sets the flag afresh.
        self.structure_dirty = false;
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let _running = self.running();
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        if self.remount {
            return;
        }
        if let TileContent::App(w) = &mut self.content {
            let children = taffy.children(node).unwrap_or_default();
            if let Some(&child_node) = children.first() {
                let rect = self.rect;
                let origin = Point::new(rect.x0, rect.y0);
                crate::widget::modal_dialog::within_window(node, rect, || {
                    if let Some(history) = &mut self.history {
                        history.apply_composition(w.as_mut(), taffy, child_node, origin);
                        self.structure_dirty |= history.width_needs_refresh(rect.width());
                    } else {
                        w.apply_layout(taffy, child_node, origin);
                    }
                });
            } else {
                tracing::warn!(
                    "TileHost::apply_layout: App content with no taffy child; \
                     skipping (next frame's structure rebuild should resolve)"
                );
            }
        }
    }
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let outer = (self.rect.width() > 0.0 && self.rect.height() > 0.0)
            .then(|| presence.push_viewport(self.rect));
        crate::widget::defaults::draw_overlay(
            self.children_mut(),
            scene,
            transform,
            text_renderer,
            theme,
            presence,
        );
        if let Some(outer) = outer {
            presence.pop_viewport(outer);
        }
    }
    fn set_full_bleed(&mut self, full_bleed: bool) {
        self.full_bleed = full_bleed;
        if let TileContent::App(app) = &mut self.content {
            app.set_full_bleed(full_bleed);
        }
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let _running = self.running();
        if let Some(t) = self.spawned_at
            && crate::time::now().saturating_duration_since(t) >= spawn_time()
        {
            self.spawned_at = None;
        }
        if theme.reduce_motion {
            self.spawned_at = None;
        }
        if self.spawn_in_flight()
            && let Some(started) = self.spawned_at
        {
            let progress =
                spawn_progress(crate::motion::frame_now().saturating_duration_since(started));
            let pose = WindowMotion::content_pose(progress, self.rect, theme.motion());
            let alpha = pose.opacity;
            // The window edge sits just outside the card; a few pixels of
            // clip keep it whole through the scale.
            let clip = Rect::new(
                self.rect.x0 - 4.0,
                self.rect.y0 - 4.0,
                self.rect.x1 + 4.0,
                self.rect.y1 + 4.0,
            );
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                alpha,
                transform,
                &clip,
            );
            let anim_transform = pose.applied_to(transform);
            self.draw_window(scene, anim_transform, tr, theme, presence);
            scene.pop_layer();
            crate::widget::draw_gate_and_peer_focus(self, scene, transform, theme, presence);
            return;
        }
        self.draw_window(scene, transform, tr, theme, presence);
        crate::widget::draw_gate_and_peer_focus(self, scene, transform, theme, presence);
    }
    fn needs_redraw(&self) -> bool {
        if let Some(redraw) = self.held_redraw() {
            return redraw;
        }
        let now = crate::motion::frame_now();
        let progress = self.arrival_progress();
        let arrival = self.arrival_now();
        if arrival
            .as_ref()
            .is_some_and(|arrival| arrival.needs_redraw(now, progress))
        {
            return true;
        }
        // Content its placeholder still stands for is not drawn: what it
        // would redraw is not on screen. Its first frame landing is the
        // arrival's to notice, above.
        if self.arrival_step(now).is_some() {
            return false;
        }
        let mut children = 0usize;
        self.for_each_child(&mut |_| children += 1);
        children > 0 && super::defaults::needs_redraw(&|visit| self.for_each_child(visit))
    }
    fn mark_drawn(&mut self) {
        self.drop_ended_hold();
        super::defaults::mark_drawn(self.children_mut());
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let _running = self.running();
        if *ctx.handled {
            return;
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Some(Point::new(
                position.x / ctx.scale_factor,
                position.y / ctx.scale_factor,
            ));
        }
        // The card of a record that cannot be opened opens its "Details".
        if self.record_error.is_some()
            && let Some((_, card)) = &mut self.error_card
        {
            card.handle_event(event, ctx);
            if *ctx.handled {
                return;
            }
        }
        // Refuse what is meant for this tile while its owner is closed or
        // switching, or its content cannot be shown; leave the rest for its
        // neighbours (a loading window must not freeze the ones beside it).
        if self.prepare_record_resolution().is_err() {
            *ctx.handled |= self.event_is_for_this_tile(event, ctx.scale_factor);
            return;
        }
        self.install_live_custody(crate::control_custody::CustodyAttachment::Dynamic);
        if self.record_error.is_some() {
            *ctx.handled |= self.event_is_for_this_tile(event, ctx.scale_factor);
            return;
        }
        let TileContent::App(app) = &mut self.content else {
            return;
        };
        if let Some(history) = &mut self.history {
            history.observe_pointer(event, ctx.scale_factor);
            if history.controls_event(event, ctx.scale_factor) {
                history.route_event(app.as_mut(), event, ctx);
                let leftover_typing = matches!(
                    event,
                    WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
                ) && !*ctx.handled
                    && !history.captures_input();
                if !leftover_typing {
                    return;
                }
            } else if !history.live_interactive() {
                return;
            }
        }
        let children = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        let Some(child_node) = self
            .live_callback_node
            .or_else(|| children.first().copied())
        else {
            return;
        };
        app.handle_event(event, &mut ctx.reborrow(ctx.path, child_node));
    }
    fn name(&self) -> &'static str {
        "TileHost"
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let _running = self.running();
        if let Some(detail) = &self.record_error {
            let card = record_unavailable_card(detail);
            let node = card
                .accessibility()
                .unwrap_or_else(|| crate::a11y::AccessNode::new(crate::a11y::Role::Group));
            return Some(node.with_bounds(self.rect));
        }
        if let Some(detail) = &self.record_transition_error {
            return Some(
                crate::a11y::AccessNode::new(crate::a11y::Role::Group)
                    .with_name(crate::i18n::widget_format(
                        "widget-document-switch-blocked",
                        &[],
                    ))
                    .with_description(detail.clone())
                    .with_live(crate::a11y::Politeness::Assertive)
                    .with_bounds(self.rect),
            );
        }
        if let Some(step) = self.arrival_step(crate::time::now()) {
            let local = crate::arriving::Source::Local;
            let source = self
                .arrival
                .as_ref()
                .map_or(&local, |arrival| arrival.clock().source());
            let face = super::ArrivalFace::at_step(self.arrival_shape(), step, source)
                .with_progress(self.arrival_progress());
            let mut node = crate::a11y::AccessNode::new(crate::a11y::Role::Group);
            let title = match &self.content {
                TileContent::App(app) => app.app_title(),
                TileContent::Stub { label, .. } => label.as_str(),
                TileContent::Notice { title, .. } => title.as_str(),
                TileContent::Arriving => "",
            };
            if !title.is_empty() {
                node = node.with_name(title.to_owned());
            }
            return Some(
                node.with_description(face.description())
                    .with_state(crate::a11y::AccessState::new().busy(true))
                    .with_bounds(self.rect),
            );
        }
        match &self.content {
            TileContent::Notice { title, detail } => Some(
                crate::a11y::AccessNode::new(crate::a11y::Role::Group)
                    .with_name(title.clone())
                    .with_description(detail.clone())
                    .with_bounds(self.rect),
            ),
            TileContent::App(_) | TileContent::Stub { .. } | TileContent::Arriving => None,
        }
    }
    fn presented(&self) -> bool {
        if !self.content_was_presented.get() {
            let now = match &self.content {
                TileContent::App(w) => w.presented(),
                TileContent::Stub { .. } | TileContent::Notice { .. } => true,
                TileContent::Arriving => false,
            };
            self.content_was_presented.set(now);
            if now {
                self.content_presented_at.set(Some(crate::time::now()));
            }
            self.was_presented.set(self.was_presented.get() || now);
        }
        self.was_presented.get()
    }
    fn arrives_from(&self) -> crate::arriving::Source {
        self.arrival
            .as_ref()
            .map_or(crate::arriving::Source::Local, |arrival| {
                arrival.clock().source().clone()
            })
    }
    fn holds_frame_until(&self, now: Instant) -> Option<Instant> {
        let content = super::defaults::holds_frame_until(&|visit| self.for_each_child(visit), now);
        self.holds_first_frame_until(now)
            .into_iter()
            .chain(content)
            .max()
    }
    fn next_wake_deadline(&self) -> Option<Instant> {
        let now = crate::time::now();
        let own = self
            .arrival_now()
            .and_then(|arrival| arrival.next_step_at(now));
        let held = self.holds_first_frame_until(now);
        own.into_iter()
            .chain(held)
            .chain(super::defaults::next_wake_deadline(&|visit| {
                self.for_each_child(visit);
            }))
            .min()
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn wants_pointer_capture(&self) -> bool {
        match &self.content {
            TileContent::App(widget) => widget.wants_pointer_capture(),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => false,
        }
    }
    fn take_structure_dirty(&mut self) -> bool {
        self.take_structure_change().rebuilds()
    }
    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        use crate::widget::StructureChange;
        let swapped = std::mem::take(&mut self.structure_dirty);
        let composes_live = self.composes_live_app();
        let inner = match &mut self.content {
            TileContent::App(w) if self.remount => {
                let _ = w.take_structure_dirty();
                StructureChange::None
            }
            // A History showing a version in the app's place builds the
            // app's nodes from it, not from the app: a change the app reports
            // stays with it, and is asked for again once the History returns
            // to it. Taken here, it would ask for a composition this host
            // never builds the app into, pass after pass.
            TileContent::App(_) if !composes_live => StructureChange::None,
            TileContent::App(w) => w.take_structure_change(),
            _ => StructureChange::None,
        };
        if self.remount {
            return StructureChange::Children;
        }
        let rebuilds =
            swapped || matches!(inner, StructureChange::Described | StructureChange::Rebuild);
        match (rebuilds, self.node) {
            (true, Some(_)) if self.lays_out_in_place() => {
                self.remount = true;
                self.live_callback_node = None;
                if let Some(history) = &mut self.history {
                    history.discard_layout_nodes();
                }
                StructureChange::Children
            }
            (true, _) => StructureChange::Rebuild,
            (false, _) if self.lays_out_in_place() => inner,
            (false, _) => StructureChange::of(inner.rebuilds()),
        }
    }
    fn relaid_layout_node(&self) -> Option<NodeId> {
        if self.remount { self.node } else { None }
    }
    /// Lay the content that took a placeholder's place out under the
    /// tile's retained node; otherwise carry out the app's own change.
    fn remount_children(&mut self, remount: &mut crate::widget::Remount<'_>) -> bool {
        if !self.remount {
            return self
                .children_mut()
                .into_iter()
                .all(|child| child.remount_children(remount));
        }
        let Some(node) = self.node else {
            return false;
        };
        let _running = self.running();
        self.remount = false;
        for child in remount.taffy().children(node).unwrap_or_default() {
            remount.unmount(child);
        }
        let child = remount.mount_with(node, |taffy, theme, node_map| {
            self.build_content(taffy, theme, node_map)
        });
        let _ = remount
            .taffy()
            .set_style(node, Self::node_style(child.is_some()));
        remount.set_children(node, child.as_slice()).is_ok()
    }
    fn needs_animation_frame(&self) -> bool {
        let _running = self.running();
        // Override so the spawn fade, the arrival's cross-fade and a hosted
        // app's own signal all bubble up. Content still on its way is not an
        // animation: its placeholder steps are deadlines
        // (`next_wake_deadline`), so a slow app's first frame never holds the
        // compositor at display cadence.
        let inner = match &self.content {
            TileContent::App(w) => w.needs_animation_frame(),
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => false,
        };
        let arrival = self
            .arrival
            .as_ref()
            .is_some_and(|arrival| arrival.is_moving(crate::motion::frame_now()));
        self.spawn_in_flight() || arrival || inner
    }
}

impl crate::widget::ResourceOperations for TileHost {
    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        let _running = self.running();
        let (_, scope) = op.with_custody();
        op.check()?;
        if !matches!(&self.content, TileContent::App(_)) {
            return Ok(None);
        }
        if !self.history_lifetime.active.get() {
            return Err("The current document owner is closed".into());
        }
        self.prepare_record_resolution()?;
        if scope.is_none() {
            self.install_live_custody(crate::control_custody::CustodyAttachment::Dynamic);
        }
        let owner = self.history_owner();
        let TileContent::App(app) = &mut self.content else {
            return Ok(None);
        };
        let mut start = |document: &mut AutoCommit, root: &ObjId| match scope {
            Some(scope) => scope.with_document_mutation(document, |document| {
                crate::widget::start_operation(
                    app.as_mut(),
                    op,
                    &mut crate::widget::Edits::lend(document),
                    root,
                )
            }),
            None => crate::widget::start_operation(
                app.as_mut(),
                op,
                &mut crate::widget::Edits::lend(document),
                root,
            ),
        };
        if scope.is_none() {
            let started = start(doc.document_mut(), path)?;
            if matches!(
                started,
                Some(crate::widget::ResourceOperationStart::Accepted)
            ) && self.callback_binding().is_some()
            {
                self.component_invocations.insert(op.invocation());
            }
            return Ok(started);
        }
        let started = start(doc.document_mut(), path)?;
        if matches!(
            started,
            Some(crate::widget::ResourceOperationStart::Accepted)
        ) && let Some(owner) = owner
        {
            self.retained_invocations
                .insert(op.invocation(), owner.clone());
            self.owner_bound_work = true;
            self.owner_bound_custody.get_or_insert(owner);
        }
        Ok(started)
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        let _running = self.running();
        if let Some(update) = self.take_retained_update(invocation) {
            return Some(update);
        }
        if self.component_invocations.contains(&invocation) {
            let TileContent::App(app) = &mut self.content else {
                return None;
            };
            let update = crate::widget::poll_resource_operation(
                app.as_mut(),
                invocation,
                cancellation_requested,
                doc,
                path,
            );
            if update
                .as_ref()
                .is_some_and(tilo_actions::InvocationUpdate::is_terminal)
            {
                self.component_invocations.remove(&invocation);
            }
            return update;
        }
        // Admitted work belongs to the retained owner even while its local
        // presentation is hidden. A replacement must never inherit the id.
        if let Some(owner) = self.retained_invocations.get(&invocation).cloned() {
            if owner.validate().is_err()
                || self
                    .history_owner()
                    .is_none_or(|current| !current.same_owner(&owner))
            {
                self.retained_invocations.remove(&invocation);
                return Some(Self::unavailable_update(invocation));
            }
            let (store, checkout) = owner.store_checkout();
            let TileContent::App(app) = &mut self.content else {
                return None;
            };
            let update = store
                .read_checked(&checkout.id, |document| {
                    crate::widget::poll_resource_operation(
                        app.as_mut(),
                        invocation,
                        cancellation_requested,
                        document,
                        &ROOT,
                    )
                })
                .ok()
                .flatten();
            if update
                .as_ref()
                .is_some_and(tilo_actions::InvocationUpdate::is_terminal)
            {
                self.retained_invocations.remove(&invocation);
            }
            return update;
        }
        match &mut self.content {
            TileContent::App(app) => crate::widget::poll_resource_operation(
                app.as_mut(),
                invocation,
                cancellation_requested,
                doc,
                path,
            ),
            _ => None,
        }
    }
}

impl crate::component_lifecycle::ComponentLifecycle for TileHost {
    fn unsaved_documents(&self, doc: &AutoCommit) -> Vec<crate::close_admission::UnsavedDocument> {
        match &self.content {
            TileContent::App(root) => {
                crate::component_lifecycle::unsaved_documents(root.as_ref(), doc)
            }
            _ => Vec::new(),
        }
    }
    fn checkpoint_components(&mut self) -> Vec<String> {
        let mut errors = crate::close_admission::retry_reports(&mut self.close_reports);
        errors.extend(self.record_error.iter().cloned());
        if let TileContent::App(root) = &mut self.content {
            errors.extend(crate::component_lifecycle::checkpoint_components(
                root.as_mut(),
            ));
        }
        errors
    }
    fn finish_admitted_close(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        if let TileContent::App(root) = &mut self.content {
            self.close_reports
                .extend(crate::component_lifecycle::finish_admitted_close(
                    root.as_mut(),
                ));
        }
        self.take_component_close_reports()
    }
    fn close_components(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        self.history_lifetime.control_domain.close();
        self.history_lifetime.active.set(false);
        if let TileContent::App(root) = &mut self.content {
            self.close_reports
                .extend(crate::component_lifecycle::close_components(root.as_mut()));
        }
        self.take_component_close_reports()
    }
    fn take_component_close_reports(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        let mut reports = std::mem::take(&mut self.close_reports);
        if let TileContent::App(root) = &mut self.content {
            reports.extend(crate::component_lifecycle::take_component_close_reports(
                root.as_mut(),
            ));
        }
        reports
    }
}

impl crate::view::capability::ViewState for TileHost {
    fn view_state(&self) -> Option<crate::view::ViewContribution> {
        match &self.content {
            TileContent::App(app) => crate::view::capability::view_state(app.as_ref()),
            _ => None,
        }
    }
    fn restore_view_state(&mut self, view: &crate::view::ViewContribution) {
        if let TileContent::App(app) = &mut self.content {
            crate::view::capability::restore_view_state(app.as_mut(), view);
        }
    }
}

impl crate::widget::controls::ActionControls for TileHost {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        // An unbound tile inherits its surrounding owner exactly once. Moving
        // within that incarnation is presentation reparenting; a different
        // owner needs an explicit close/renew transition, never this hook.
        if self.history_owner().is_none() && !self.has_separate_document_owner() {
            if domain.closed() {
                domain.unsafe_attachment();
                return;
            }
            if let Some(previous) = self.custody_parent.as_ref() {
                if domain.permits_renewal(previous, attachment) {
                    self.history_lifetime.control_domain.close();
                    self.history_lifetime.active.set(false);
                    self.history_lifetime = crate::history_mode::owner::RecordOwnerLifetime::new();
                    self.custody_scope = None;
                    self.custody_renewal = None;
                } else if previous.closed() || !previous.same_owner_incarnation(domain) {
                    domain.unsafe_attachment();
                    return;
                }
            }
        }
        self.custody_parent = Some(domain.clone());
        self.install_live_custody(attachment);
    }
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        if self.has_separate_document_owner() {
            return None;
        }
        self.children_mut()
            .into_iter()
            .find_map(crate::widget::controls::take_control_invocation)
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        if self.has_separate_document_owner() {
            return;
        }
        for child in self.children_mut() {
            crate::widget::controls::refresh_control_action_availability(child, allowed);
        }
    }
}

impl crate::history_mode::capability::FrameworkHistoryTopology for TileHost {
    fn visit_framework_history(
        &self,
        visitor: &mut dyn FnMut(&crate::history_mode::FrameworkHistorySurface, &dyn Widget),
    ) {
        if let TileContent::App(live) = &self.content {
            if let Some(history) = &self.history {
                visitor(history.as_ref(), live.as_ref());
            }
            crate::history_mode::capability::visit_framework_history(live.as_ref(), visitor);
        }
    }
    fn visit_framework_history_mut(
        &mut self,
        visitor: &mut dyn FnMut(&mut crate::history_mode::FrameworkHistorySurface, &mut dyn Widget),
    ) {
        let owner = self.history_owner();
        if let TileContent::App(live) = &mut self.content {
            if let Some(history) = &mut self.history {
                history.set_enclosing_owner(owner);
                visitor(history.as_mut(), live.as_mut());
            }
            crate::history_mode::capability::visit_framework_history_mut(live.as_mut(), visitor);
        }
    }
}

impl crate::collaboration::participants::CollaborationTopology for TileHost {
    fn collaboration_children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        match &mut self.content {
            TileContent::App(live) => vec![live.as_mut()],
            TileContent::Stub { .. } | TileContent::Notice { .. } | TileContent::Arriving => {
                Vec::new()
            }
        }
    }
}

impl TileHost {
    /// The window's outline. Painted *outside* the card's silhouette, safe
    /// from app content by the clip in `draw_steady_state` rather than a
    /// paint-order race: a stroke over unknown app content can't pick one
    /// alpha that reads right on every ground, but the desktop outside is
    /// known, so `Theme::window_edge` can derive one that does. Inflated by
    /// half the stroke width so the band sits flush with zero overlap; the
    /// radius grows by the same half so it stays concentric at the
    /// corners. Focus reuses this same edge (colour + weight) rather than a
    /// second ring, keeping the signal part of the window's own silhouette.
    fn draw_edge_at(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        radius: f64,
        rect: Rect,
    ) {
        // Focus is the window's own edge in the accent, and a pixel heavier:
        // the band is drawn outside the card, so the extra pixel is desktop,
        // and the weight is what tells the window taking the keyboard from
        // its neighbours at a glance.
        let edge_color = if self.focused {
            theme.tokens.colors.window_edge_focused()
        } else {
            theme.window_edge()
        };
        let edge_width = if self.focused {
            FOCUSED_EDGE_STROKE_WIDTH
        } else {
            EDGE_STROKE_WIDTH
        };
        let outer_offset = if self.flush_edges { 0.0 } else { edge_width };
        let inner_offset = outer_offset - edge_width;
        let outer = RoundedRect::from_rect(
            rect.inflate(outer_offset, outer_offset),
            self.corner_radii(radius + outer_offset),
        );
        let inner = RoundedRect::from_rect(
            rect.inflate(inner_offset, inner_offset),
            self.corner_radii((radius + inner_offset).max(0.0)),
        );
        let mut edge = outer.to_path(0.01);
        edge.extend(inner.path_elements(0.01));
        scene.fill(
            vello::peniko::Fill::EvenOdd,
            transform,
            edge_color,
            None,
            &edge,
        );
    }

    fn corner_radii(&self, radius: f64) -> vello::kurbo::RoundedRectRadii {
        if self.square_bottom {
            vello::kurbo::RoundedRectRadii::new(radius, radius, 0.0, 0.0)
        } else {
            vello::kurbo::RoundedRectRadii::from_single_radius(radius)
        }
    }

    pub fn set_square_bottom(&mut self, square: bool) {
        self.square_bottom = square;
    }

    fn draw_edge(&self, scene: &mut Scene, transform: Affine, theme: &Theme, radius: f64) {
        self.draw_edge_at(scene, transform, theme, radius, self.rect);
    }

    fn draw_transition_status(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.record_error.is_some() {
            return;
        }
        let Some(detail) = &self.record_transition_error else {
            return;
        };
        let margin = 12.0;
        let banner = Rect::new(
            self.rect.x0 + margin,
            (self.rect.y1 - 76.0).max(self.rect.y0 + margin),
            self.rect.x1 - margin,
            self.rect.y1 - margin,
        );
        crate::widget::Banner::paint(
            &mut crate::paint::PaintCtx::new(scene, transform, tr, theme),
            banner,
            &crate::widget::BannerContent::new(
                crate::widget::BannerKind::Error,
                "Document switch blocked",
            )
            .with_message(Some(detail)),
            Point::ZERO,
        );
    }

    fn draw_steady_state(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.full_bleed {
            self.draw_full_bleed(scene, transform, tr, theme, presence);
            return;
        }
        let radius = f64::from(theme.tokens.radii.tile);
        let shadow = theme.tokens.elevation.window;
        if let Some(reach) = crate::surface::shadow_bounds(self.rect, &shadow) {
            let below = Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, reach.y1);
            let key = crate::scene_parts::PartKey::new("ui.tile-shadow", ());
            let bounds = vec![transform.transform_rect_bbox(below)];
            crate::scene_parts::part(scene, key, bounds, |scene| {
                scene.push_clip_layer(vello::peniko::Fill::NonZero, transform, &below);
                crate::surface::shadow(scene, transform, self.rect, radius, &shadow);
                scene.pop_layer();
            });
        }
        let shape = RoundedRect::from_rect(self.rect, self.corner_radii(radius));
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.content,
            None,
            &shape,
        );
        // Hosted apps fill their own background as a plain rectangle (an
        // app can't know it's being framed); without this clip the first
        // fill squares off the window's rounded corners.
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &shape,
        );
        let content = crate::scene_parts::PartKey::new("ui.tile-content", ());
        let bounds = vec![transform.transform_rect_bbox(self.rect)];
        crate::scene_parts::part(scene, content, bounds, |scene| {
            self.draw_clipped_content(scene, transform, tr, theme, presence, &shape);
            self.draw_history_loading(scene, transform, tr, theme);
        });
        scene.pop_layer();
        let edge = crate::scene_parts::PartKey::new("ui.tile-edge", ());
        let bounds = edge_bands(self.rect, radius)
            .into_iter()
            .map(|band| transform.transform_rect_bbox(band))
            .collect();
        if !self.inside_growing_frame && !self.edge_by_card {
            crate::scene_parts::part(scene, edge, bounds, |scene| {
                self.draw_edge(scene, transform, theme, radius);
            });
        }
    }

    fn draw_full_bleed(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.content,
            None,
            &self.rect,
        );
        let shape = RoundedRect::from_rect(self.rect, 0.0);
        let content = crate::scene_parts::PartKey::new("ui.tile-content", ());
        let bounds = vec![transform.transform_rect_bbox(self.rect)];
        crate::scene_parts::part(scene, content, bounds, |scene| {
            self.draw_clipped_content(scene, transform, tr, theme, presence, &shape);
        });
    }

    fn draw_clipped_content(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        shape: &RoundedRect,
    ) {
        let now = crate::motion::frame_now();
        self.arrival_motion = theme.motion();
        self.advance_arrival(now);
        let progress = self.arrival_progress();
        match self
            .arrival
            .as_ref()
            .and_then(|arrival| arrival.paint_at(now))
        {
            Some(arrival::ArrivalPaint::Placeholder(step)) => {
                self.paint_arrival_face(scene, transform, tr, theme, step);
                if let Some(arrival) = self.arrival.as_mut() {
                    arrival.note_drawn(step, progress);
                }
            }
            Some(arrival::ArrivalPaint::Fading {
                replaced,
                outgoing,
                incoming,
            }) => {
                if outgoing > 0.0 {
                    scene.push_layer(
                        vello::peniko::Fill::NonZero,
                        vello::peniko::BlendMode::default(),
                        outgoing,
                        transform,
                        shape,
                    );
                    self.paint_arrival_face(scene, transform, tr, theme, replaced);
                    scene.pop_layer();
                }
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    incoming,
                    transform,
                    shape,
                );
                self.draw_content(scene, transform, tr, theme, presence);
                scene.pop_layer();
            }
            None => self.draw_content(scene, transform, tr, theme, presence),
        }
    }

    fn paint_arrival_face(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        step: crate::arriving::Step,
    ) {
        let local = crate::arriving::Source::Local;
        let source = self
            .arrival
            .as_ref()
            .map_or(&local, |arrival| arrival.clock().source());
        let margin = f64::from(theme.tokens.spacing.rhythm.group);
        let page = self.rect.inset(-margin);
        super::ArrivalFace::at_step(self.arrival_shape(), step, source)
            .with_progress(self.arrival_progress())
            .paint(
                &mut crate::paint::PaintCtx::new(scene, transform, tr, theme),
                page,
            );
    }

    fn arrival_shape(&self) -> super::SkeletonShape {
        let declared = self.declared_arrival_shape;
        match &self.content {
            // What the app's own root declares, and while it declares
            // nothing (its first frame is not drawn yet), what its manifest
            // declared for the window before the code was in.
            TileContent::App(app) => match app.arrival_shape() {
                super::SkeletonShape::Ground => declared.unwrap_or(super::SkeletonShape::Ground),
                own => own,
            },
            TileContent::Arriving => declared.unwrap_or(super::SkeletonShape::Ground),
            _ => super::SkeletonShape::Ground,
        }
    }

    fn arrival_progress(&self) -> Option<crate::arriving::Progress> {
        match &self.content {
            TileContent::App(app) if !self.content_was_presented.get() => app.arrival_progress(),
            _ => None,
        }
    }

    fn draw_content(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(detail) = self.record_error.clone() {
            if self
                .error_card
                .as_ref()
                .is_none_or(|(shown, _)| *shown != detail)
            {
                self.error_card = Some((detail.clone(), record_unavailable_card(&detail)));
            }
            let rect = self.rect;
            if let Some((_, card)) = &mut self.error_card {
                card.apply_layout_external(rect);
                card.draw_child(scene, transform, tr, theme, presence);
            }
        } else {
            let content_transform = if self.content_shift == 0.0 {
                transform
            } else {
                transform.pre_translate(vello::kurbo::Vec2::new(self.content_shift, 0.0))
            };
            match &mut self.content {
                TileContent::App(w) => {
                    if self
                        .history
                        .as_ref()
                        .is_none_or(|history| history.live_visible())
                    {
                        let key = crate::scene_parts::PartKey::new("ui.tile-app", ());
                        crate::scene_parts::begin(scene, key);
                        w.draw_child(scene, content_transform, tr, theme, presence);
                        let copies = (crate::scroll_copy::host_copies()
                            && theme.tokens.colors.surfaces.content.components[3] >= 1.0)
                            .then(crate::scroll_copy::host_device_scale);
                        let hint = self.content_damage.hint(
                            &**w,
                            content_transform,
                            transform.transform_rect_bbox(self.rect),
                            presence,
                            copies,
                        );
                        crate::scene_parts::end_hinted(scene, hint);
                    } else {
                        self.content_damage.forget();
                    }
                    if let Some(history) = &mut self.history {
                        history.draw_child(scene, content_transform, tr, theme, presence);
                    }
                }
                TileContent::Stub { kind, label } => {
                    let detail = format!("{} coming soon", kind.as_str());
                    draw_card_text(scene, transform, tr, theme, self.rect, label, &detail);
                }
                TileContent::Notice { title, detail } => {
                    draw_card_text(scene, transform, tr, theme, self.rect, title, detail);
                }
                TileContent::Arriving => {}
            }
        }
        self.draw_transition_status(scene, transform, tr, theme);
    }
}

/// One function so the two callers (`Stub`, `Notice`) can't drift apart —
/// only the sentence they supply is meant to read as meaningful.
fn draw_card_text(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    title: &str,
    detail: &str,
) {
    let mut content = super::EmptyStateContent::new(title);
    content.message = (!detail.is_empty()).then_some(detail);
    let mut paint = crate::paint::PaintCtx::new(scene, transform, tr, theme);
    super::EmptyState::paint(&mut paint, rect, &content, Point::new(-1.0, -1.0));
}

mod arrival;
mod held;
mod history;
mod search;
pub use held::{hold_windows, release_windows};
#[cfg(test)]
mod held_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "tile_host_arriving_tests.rs"]
mod arriving_tests;
#[cfg(test)]
#[path = "tile_host_copy_tests.rs"]
mod copy_tests;
#[cfg(test)]
#[path = "tile_host_window_tests.rs"]
mod window_tests;
