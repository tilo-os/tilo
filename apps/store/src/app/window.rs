//! The Store's window: the shared [`StoreState`] composed into its shell,
//! and the view that mounts it over the reports.

use std::cell::{RefCell, RefMut};
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, Size, Style, TaffyTree};
use ui::a11y::{AccessNode, Politeness, Role};
use ui::framework::{Live, Publishes, RunMode, ViewCx};
use ui::widget::{AppModel, Element, ModelPath, Widget};
use ui::{Theme, presence::PresenceCtx, text_render::TextRenderer};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use super::{Shared, StoreState, parent_node, view};
use crate::reports::Reports as Store;
use crate::resource::{ReportProjection, StoreEffect};
use crate::worker::StoreWorker;

/// The window: the shared [`StoreState`] in the shell its screens are
/// composed into.
pub struct StoreApp {
    pub(super) state: Shared,
    /// The reports as the document holds them, shown as they change; `None`
    /// for a window outside the framework.
    document: Option<Live<Store>>,
    presented: Option<(u64, u64)>,
    /// The rect the toplevel container resolves into during
    /// `apply_layout` — needed for `get_rect`.
    pub(super) rect: Rect,
    /// Root of the built layout so events can be routed to the
    /// composed sub-tree.
    pub(super) root: Option<Box<ui::widget::NavShell>>,
}

impl Default for StoreApp {
    fn default() -> Self {
        Self::new()
    }
}

impl StoreApp {
    pub fn new() -> Self {
        Self::showing(StoreState::new())
    }

    /// The window over `state`.
    pub(crate) fn showing(state: StoreState) -> Self {
        Self {
            state: Rc::new(RefCell::new(state)),
            document: None,
            presented: None,
            rect: Rect::ZERO,
            root: None,
        }
    }

    /// What the window shows, shared with the view's effect handler.
    pub fn state(&self) -> RefMut<'_, StoreState> {
        self.state.borrow_mut()
    }

    /// Show the reports the document holds now, when they changed since.
    fn follow_document(&mut self) {
        let Some(document) = &self.document else {
            return;
        };
        let Some(shown) = document.changed_since(&mut self.presented) else {
            return;
        };
        self.state().present_reports(&ReportProjection::of(&shown));
    }
}

/// Mount `window` as the view of the reports: the view shares its state
/// with the effect handler, and publishes the catalogue, what is on this
/// device, the remotes and the running jobs.
pub(crate) fn window(cx: &ViewCx<Store>, mut window: StoreApp) -> Element {
    window.document = Some(cx.live().clone());
    let state = Rc::clone(&window.state);
    {
        let mut store = state.borrow_mut();
        store.package_content = cx.content();
        match cx.mode() {
            RunMode::Live => store.start_worker(cx.waker()),
            RunMode::Capture => {
                if let Some(catalogue) = cx.fixture::<crate::resource::CaptureCatalogue>() {
                    store.show_capture_catalogue(catalogue.items.clone());
                }
            }
            RunMode::Past => {}
        }
        store.attach_dispatcher(cx.dispatcher());
    }
    {
        let (answer, status) = (Rc::clone(&state), Rc::clone(&state));
        cx.effects(move |effect: StoreEffect, cx| answer.borrow_mut().answer(effect, cx, &answer))
            .status(move || status.borrow().effect_statuses());
    }
    {
        let state = Rc::clone(&state);
        cx.on_stage(move |scenario| state.borrow_mut().stage_scene(scenario));
    }
    let (rows, row, entries) = (Rc::clone(&state), Rc::clone(&state), state);
    cx.publishing(
        Publishes::new()
            .live_rows(move || rows.borrow().live_rows())
            .live_row(move |id| row.borrow().live_row(id))
            // Every app of the complete catalogue, not only the rows the
            // current filter shows, each opening its details.
            .search_entries(move || entries.borrow().catalogue_search_entries())
            // Desktop search finds the catalogue's apps from the shared
            // catalogue whether or not the Store runs; keeping thousands of
            // them in the system's search index as well would find every
            // app twice.
            .search_entries_while_running(),
        Box::new(window),
    )
}

impl ui::widget::Collaborative for StoreApp {
    fn take_collab_requests(&mut self) -> Vec<ui::collaboration::CollabRequest> {
        ui::widget::Collaborative::take_collab_requests(&mut *self.state())
    }

    fn handle_collab_event(&mut self, event: &ui::collaboration::CollabEvent) {
        ui::widget::Collaborative::handle_collab_event(&mut *self.state(), event);
    }
}

impl ui::widget::Queryable for StoreApp {
    fn set_query_text(&mut self, text: &str) {
        ui::widget::Queryable::set_query_text(&mut *self.state(), text);
    }

    fn query_closed(&mut self) {
        ui::widget::Queryable::query_closed(&mut *self.state());
    }

    fn query_count(&self) -> Option<ui::widget::FilterCount> {
        ui::widget::Queryable::query_count(&*self.state.borrow())
    }
}

impl Widget for StoreApp {
    ui::provides!(
        dyn ui::widget::Collaborative,
        dyn ui::widget::controls::ActionControls,
        dyn ui::widget::Queryable
    );

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.state().answer_presses();
        let changed = self
            .root
            .as_mut()
            .is_some_and(|root| root.update(state, path))
            | self.sync_window_toolbar();
        let mut store = self.state();
        if store.back_requested.take() {
            store.show_catalogue();
            store.structure_dirty = true;
            true
        } else {
            changed
        }
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.follow_document();
        {
            let mut store = self.state();
            store.answer_presses();
            store.drain_worker();
        }
        self.sync_window_toolbar();
        if let Some(root) = self.root.as_mut() {
            root.sync(state, path);
        }
        let mut store = self.state();
        store.request_from_worker();
        store.publish_status();
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.root
            .as_ref()
            .and_then(|root| root.next_wake_deadline())
            .into_iter()
            .min()
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let locale = ui::i18n::I18nManager::current_locale();
        let pseudo = ui::i18n::I18nManager::pseudo_locale();
        let mut root = {
            let mut store = self.state.borrow_mut();
            let recompose = store.structure_dirty
                || store.composition_dirty
                || self.root.is_none()
                || store.composition_context.as_ref().is_none_or(
                    |(previous_theme, previous_locale, previous_pseudo)| {
                        previous_theme != theme
                            || previous_locale != &locale
                            || *previous_pseudo != pseudo
                    },
                );
            store.structure_dirty = false;
            let jobs = store.jobs.clone();
            let mut root = self.root.take().unwrap_or_else(|| {
                let can = Rc::clone(&store.can_go_back);
                let requested = Rc::clone(&store.back_requested);
                Box::new(
                    ui::widget::NavShell::new()
                        .content_kind(ui::widget::ContentKind::Grid)
                        .with_page_column(f32::from(view::Measure::CATALOGUE_LINES))
                        .with_id("store.shell")
                        .with_window_toolbar()
                        .bottom_band(Box::new(ui::jobs::Band::with_presses(jobs.clone())))
                        .on_back(move || can.get(), move || requested.set(true)),
                )
            });
            if recompose {
                // A resize only reflows retained controls. Replacing them here
                // discards input state and their text/layout caches.
                store
                    .can_go_back
                    .set(store.safety_open || store.remotes_open || store.focused_detail.is_some());
                root.set_content(store.compose(theme));
                store.composition_context = Some((theme.clone(), locale, pseudo));
                store.composition_dirty = false;
            }
            root
        };
        let child = root.build_layout(taffy, theme, node_map);
        self.root = Some(root);
        // A column, so the page's scroller can absorb the window's height.
        let node = parent_node(
            taffy,
            Style {
                flex_direction: taffy::FlexDirection::Column,
                flex_grow: 1.0,
                size: Size::from_percent(1.0, 1.0),
                ..Default::default()
            },
            &[child],
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        if let (Some(root), Ok(children)) = (self.root.as_mut(), taffy.children(node))
            && let Some(&child_node) = children.first()
        {
            root.apply_layout(taffy, child_node, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(root) = self.root.as_mut() {
            root.draw(scene, transform, text_renderer, theme, presence);
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
        if let Some(root) = self.root.as_mut() {
            root.draw_overlay(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        if let (Some(root), Ok(children)) = (self.root.as_mut(), ctx.taffy.children(ctx.node_id))
            && let Some(&child_node) = children.first()
        {
            root.handle_event(event, &mut ctx.reborrow(ctx.path, child_node));
        }
        let mut store = self.state();
        if store.back_requested.take() {
            store.show_catalogue();
            store.structure_dirty = true;
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.root
            .as_ref()
            .map(|r| r.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.root
            .as_mut()
            .map(|r| r.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &'static str {
        "StoreApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// The composed child tree supplies individual control semantics; the
    /// canvas-free root still names the application for assistive technology.
    fn accessibility(&self) -> Option<AccessNode> {
        let mut root = AccessNode::new(Role::Window)
            .with_name("App Store")
            .with_bounds(self.rect);
        let store = self.state.borrow();
        if !store.status_line.is_empty() {
            root.children.push(
                AccessNode::new(Role::Group)
                    .with_name(store.status_line.clone())
                    .with_live(Politeness::Polite),
            );
        }
        Some(root)
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = {
            let mut store = self.state();
            let own = std::mem::take(&mut store.structure_dirty);
            store.composition_dirty |= own;
            own
        };
        let child = self.root.as_mut().is_some_and(|r| r.take_structure_dirty());
        own || child
    }

    fn needs_animation_frame(&self) -> bool {
        let store = self.state.borrow();
        self.root
            .as_ref()
            .is_some_and(|r| r.needs_animation_frame())
            || store
                .images
                .as_ref()
                .is_some_and(|images| images.pending() > 0)
            || store.worker.as_ref().is_some_and(StoreWorker::has_pending)
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}
