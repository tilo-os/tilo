//! The Photos window: the shared [`Photos`] laid out with the empty state the window keeps while
//! its content column has nothing to show.

use std::cell::{RefCell, RefMut};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use automerge::AutoCommit;
use taffy::{NodeId, TaffyTree};
use tilo_content::ContentStore;
use ui::a11y::{AccessNode, Action as AccessAction};
use ui::framework::{Live, Publishes, ViewCx};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    AppModel, Element, EmptyState, ModelPath, NavFeed, TaffyTreeExt, Widget, WindowBarLink,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use super::{EmptyAction, EmptyShown, Photos};
use crate::actions::PhotosEffect;
use crate::model::PhotoLibrary;
use crate::strings::tr;

pub struct PhotosApp {
    photos: Rc<RefCell<Photos>>,
    pub(super) empty_state: Option<EmptyState>,
    empty_state_key: Option<(EmptyShown, bool)>,
    title: String,
    live: Option<Live<PhotoLibrary>>,
    presented: Option<(u64, u64)>,
}

impl Default for PhotosApp {
    fn default() -> Self {
        Self::new()
    }
}

impl PhotosApp {
    pub fn new() -> Self {
        Self::with_content(ContentStore::memory())
    }

    pub fn with_content(content: ContentStore) -> Self {
        Self::over(Photos::with_content(content))
    }

    #[must_use]
    pub fn new_persistent(content: ContentStore) -> Self {
        Self::over(Photos::new_persistent(content))
    }

    fn over(photos: Photos) -> Self {
        Self {
            title: photos.title().to_owned(),
            photos: Rc::new(RefCell::new(photos)),
            empty_state: None,
            empty_state_key: None,
            live: None,
            presented: None,
        }
    }

    #[must_use]
    pub fn launched_on(self, path: Option<std::path::PathBuf>) -> Self {
        self.state().launched = path;
        self
    }

    #[must_use]
    pub fn with_dispatcher(self, dispatcher: ui::framework::Dispatcher<PhotoLibrary>) -> Self {
        self.state().dispatch = Some(dispatcher);
        self
    }

    #[must_use]
    pub fn in_shell(self, places: &NavFeed, bar: WindowBarLink) -> Self {
        self.state().join_shell(places, bar);
        self
    }

    pub fn state(&self) -> RefMut<'_, Photos> {
        self.photos.borrow_mut()
    }

    fn follow_document(&mut self) {
        if let Some(library) = self
            .live
            .as_ref()
            .and_then(|live| live.changed_since(&mut self.presented))
        {
            self.state().adopt_resource_state(&library);
        }
    }

    fn refresh_empty_state(&mut self, doc: &AutoCommit) {
        let mut photos = self.photos.borrow_mut();
        let key = photos
            .empty_shown()
            .map(|shown| (shown, photos.actions.is_some()));
        if key != self.empty_state_key {
            self.empty_state = key.as_ref().map(|(shown, _)| {
                let empty = if shown.error {
                    EmptyState::error(&shown.title, &shown.body)
                } else {
                    EmptyState::new(&shown.title).with_message(&shown.body)
                }
                .with_icon(shown.icon);
                let (label, id, effect) = match shown.action {
                    None => return empty,
                    Some(EmptyAction::Open) => (
                        tr(if shown.error {
                            "photos-open-another"
                        } else {
                            "photos-open"
                        }),
                        "photos-empty-open",
                        PhotosEffect::Import { path: None },
                    ),
                    Some(EmptyAction::NewAlbum) => (
                        tr("photos-new-album"),
                        "photos-empty-new-album",
                        PhotosEffect::NewAlbum,
                    ),
                };
                let empty = empty.with_action(label).with_action_id(id);
                match photos
                    .actions
                    .as_ref()
                    .and_then(|actions| actions.effect(&effect))
                {
                    Some(binding) => empty.on_action(binding),
                    None => empty,
                }
            });
            self.empty_state_key = key;
            photos.structure_dirty = true;
        }
        if let Some(empty) = &mut self.empty_state {
            empty.sync(doc, &automerge::ROOT);
        }
    }
}

pub(crate) fn view(cx: &ViewCx<PhotoLibrary>, mut window: PhotosApp) -> Element {
    window.live = Some(cx.live().clone());
    let photos = Rc::clone(&window.photos);
    photos
        .borrow_mut()
        .attach_framework(cx.dispatcher(), cx.actions());
    cx.on_open_item(photos.borrow().opener());
    {
        let photos = Rc::clone(&photos);
        cx.effects(move |effect: PhotosEffect, cx| photos.borrow_mut().effect(effect, cx));
    }
    {
        let photos = Rc::clone(&photos);
        cx.on_stage(move |scenario| {
            photos
                .borrow_mut()
                .stage_scenario(scenario.unwrap_or("opened"));
        });
    }
    let rows = Rc::clone(&photos);
    cx.publishing(
        Publishes::new()
            .live_rows(move || rows.borrow().search_status_row().into_iter().collect())
            .live_row(move |id| {
                photos
                    .borrow()
                    .search_status_row()
                    .filter(|row| row.id() == id)
            }),
        Box::new(window),
    )
}

impl ui::content_zoom::ContentZoom for PhotosApp {
    fn zoom_content(&mut self, step: ui::content_zoom::ZoomStep) -> Option<f32> {
        let mut photos = self.state();
        if photos.library_mode || photos.viewer.is_none() {
            return None;
        }
        Some(photos.zoom_photo(step))
    }
}

impl ui::widget::FullscreenProvider for PhotosApp {
    fn fullscreen_intent(&self) -> Option<bool> {
        Some(self.photos.borrow().presenting)
    }

    fn on_fullscreen_changed(&mut self, active: bool) {
        if !active {
            let mut photos = self.state();
            photos.presenting = false;
            photos.slide_show = None;
        }
    }
}

impl ui::widget::controls::ActionControls for PhotosApp {
    fn install_control_custody(
        &mut self,
        domain: &ui::ControlDomain,
        attachment: ui::control_custody::CustodyAttachment,
    ) {
        for button in self.photos.borrow_mut().retained_buttons_mut() {
            ui::widget::controls::install_control_custody(button, domain, attachment);
        }
        if let Some(empty) = &mut self.empty_state {
            ui::widget::controls::install_control_custody(empty, domain, attachment);
        }
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.empty_state
            .as_mut()
            .and_then(|empty| ui::widget::controls::take_control_invocation(empty))
            .or_else(|| {
                self.photos
                    .borrow_mut()
                    .retained_buttons_mut()
                    .into_iter()
                    .find_map(|button| ui::widget::controls::take_control_invocation(button))
            })
    }

    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        let mut bindings = ui::widget::semantic_action_bindings(self.accessibility());
        if let Some(empty) = &self.empty_state {
            bindings.extend(ui::widget::controls::control_action_bindings(empty));
        }
        bindings
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &HashSet<tilo_actions::ActionAddress>,
    ) {
        for button in self.photos.borrow_mut().retained_buttons_mut() {
            button.refresh_control_action_availability(allowed);
        }
        if let Some(empty) = &mut self.empty_state {
            ui::widget::controls::refresh_control_action_availability(empty, allowed);
        }
    }
}

impl Widget for PhotosApp {
    ui::provides!(
        dyn ui::widget::controls::ActionControls,
        dyn ui::content_zoom::ContentZoom,
        dyn ui::widget::FullscreenProvider
    );

    fn on_nav_gesture(
        &mut self,
        dir: ui::gesture::NavDirection,
        state: &AppModel,
        path: &ModelPath,
    ) -> bool {
        if dir == ui::gesture::NavDirection::Back && self.state().sheet_back() {
            return true;
        }
        self.children_mut()
            .into_iter()
            .any(|child| child.on_nav_gesture(dir, state, path))
    }

    fn nav_availability(&self) -> ui::gesture::NavAvailability {
        let own = ui::gesture::NavAvailability {
            back: self.photos.borrow().editor_sheet_shown(),
            forward: false,
        };
        self.children()
            .into_iter()
            .fold(own, |seen, child| seen.or(child.nav_availability()))
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.empty_state
            .as_ref()
            .map(|empty| empty as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        if let Some(empty) = &self.empty_state {
            visitor(empty);
        }
        self.photos.borrow().for_each_layout_child(visitor);
        true
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.empty_state
            .as_mut()
            .map(|empty| empty as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.state().set_motion_reduced(reduced);
    }

    fn has_pending_visual_work(&self) -> bool {
        self.photos.borrow().has_pending_visual_work()
    }

    fn needs_animation_frame(&self) -> bool {
        self.photos.borrow().needs_animation_frame()
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.state().glide();
        let mut changed = false;
        for child in self.children_mut() {
            changed |= child.update(state, path);
        }
        changed
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.state().take_structure_dirty()
    }

    fn is_layout_dirty(&self) -> bool {
        true
    }

    fn layout_needs_measure(&self) -> bool {
        true
    }

    fn needs_redraw(&self) -> bool {
        true
    }

    fn sync(&mut self, doc: &AppModel, path: &ModelPath) {
        self.follow_document();
        self.state().sync(doc, path);
        let (doc, _) = ui::doc::document_ref(doc, path);
        self.refresh_empty_state(doc);
        self.photos.borrow().title().clone_into(&mut self.title);
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.children()
            .into_iter()
            .filter_map(Widget::next_wake_deadline)
            .chain(self.photos.borrow().next_wake_deadline())
            .min()
    }

    fn composition_follows_allocation(&self) -> bool {
        true
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let (style, children) = self.photos.borrow_mut().build_layout(
            self.empty_state.as_mut(),
            taffy,
            theme,
            node_map,
        );
        taffy.widget_node(style, &children, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.photos
            .borrow_mut()
            .apply_layout(self.empty_state.as_mut(), taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.photos.borrow_mut().draw(
            self.empty_state.as_mut(),
            scene,
            transform,
            text_renderer,
            theme,
            presence,
        );
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.state()
            .draw_overlay(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        self.photos
            .borrow_mut()
            .handle_event(self.empty_state.as_mut(), event, ctx);
    }

    fn name(&self) -> &'static str {
        "PhotosApp"
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn accessibility(&self) -> Option<AccessNode> {
        Some(
            self.photos
                .borrow()
                .accessibility_tree(self.empty_state.as_ref()),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &AccessAction,
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if self
            .empty_state
            .as_mut()
            .is_some_and(|empty| empty.perform_semantic_action(target, action, state, path))
        {
            return true;
        }
        self.state()
            .perform_semantic_action(target, action, state, path)
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        match path {
            [0] => Some(self.photos.borrow().canvas_rect),
            _ => None,
        }
    }

    fn get_rect(&self) -> Rect {
        self.photos.borrow().rect
    }
}

impl ui::widget::AppWidget for PhotosApp {
    fn app_kind(&self) -> ui::widget::AppKind {
        ui::widget::AppKind::Editor
    }

    fn app_title(&self) -> &str {
        &self.title
    }

    fn app_icon(&self) -> ui::widget::IconDescriptor {
        ui::widget::IconDescriptor::Glyph("\u{F02E9}")
    }

    fn permission_hint(&self) -> Option<ui::widget::AppKind> {
        Some(ui::widget::AppKind::Editor)
    }
}
