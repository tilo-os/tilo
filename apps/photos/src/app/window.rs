//! The Photos window: the shared [`Photos`] laid out with the empty state
//! the window keeps while its content column has nothing to show. The view
//! holds the state with the window, so its effect handler and publications
//! reach what the window shows; what an effect asks of the window waits in
//! the state for the window's next sync.

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

/// The Photos window: the shared [`Photos`] it lays out, and the empty state
/// it shows while the content column has nothing to lay out.
pub struct PhotosApp {
    photos: Rc<RefCell<Photos>>,
    /// What the content column shows while it has nothing to lay out.
    pub(super) empty_state: Option<EmptyState>,
    /// What the empty state shows, and whether the framework's door was
    /// attached to bind its action.
    empty_state_key: Option<(EmptyShown, bool)>,
    /// The tab's title as the last sync named it.
    title: String,
    /// The library as the document holds it, shown as it changes; `None`
    /// for a window outside the framework.
    live: Option<Live<PhotoLibrary>>,
    /// The `(revision, loads)` of the library last shown.
    presented: Option<(u64, u64)>,
}

impl Default for PhotosApp {
    fn default() -> Self {
        Self::new()
    }
}

impl PhotosApp {
    /// A window whose content store lives in memory with it: tests and
    /// offscreen shots. The runtime uses [`Self::new_persistent`].
    pub fn new() -> Self {
        Self::with_content(ContentStore::memory())
    }

    /// A window keeping imported originals in, and reading photographs
    /// from, `content`.
    pub fn with_content(content: ContentStore) -> Self {
        Self::over(Photos::with_content(content))
    }

    /// Runtime constructor: see [`Photos::new_persistent`].
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

    /// This window, launched naming `path`: a folder of photographs to
    /// import into the library it shows, or a photograph the launch
    /// imported, to show.
    #[must_use]
    pub fn launched_on(self, path: Option<std::path::PathBuf>) -> Self {
        self.state().launched = path;
        self
    }

    /// This window editing through `dispatcher`: what a test of the
    /// window's own methods attaches
    /// ([`testing::dispatcher`](ui::framework::testing::dispatcher)).
    #[must_use]
    pub fn with_dispatcher(self, dispatcher: ui::framework::Dispatcher<PhotoLibrary>) -> Self {
        self.state().dispatch = Some(dispatcher);
        self
    }

    /// This window in a shell: its sidebar shows what `places` shows, and
    /// the shell's own buttons join the window bar through `bar`.
    #[must_use]
    pub fn in_shell(self, places: &NavFeed, bar: WindowBarLink) -> Self {
        self.state().join_shell(places, bar);
        self
    }

    /// What the window shows, shared with the view's effect handler and
    /// publications.
    pub fn state(&self) -> RefMut<'_, Photos> {
        self.photos.borrow_mut()
    }

    /// Show the library the document holds now, when it changed since.
    fn follow_document(&mut self) {
        if let Some(library) = self
            .live
            .as_ref()
            .and_then(|live| live.changed_since(&mut self.presented))
        {
            self.state().adopt_resource_state(&library);
        }
    }

    /// Keep the shared empty state mounted as a retained child whenever the
    /// content column is empty, so keyboard, accessibility and automation
    /// reach its action through the same door as a click.
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
                // Import and a new album are effects of the app, not actions
                // of the library record: they bind through the framework's
                // door, as the header's Open does.
                let (label, id, effect) = match shown.action {
                    None => return empty,
                    // After a failure, Open says it is for another photo:
                    // the one that failed stays failed.
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

/// The view of the library: `window` over the document, its state shared
/// with the effect handler and the rows it publishes.
pub(crate) fn view(cx: &ViewCx<PhotoLibrary>, mut window: PhotosApp) -> Element {
    window.live = Some(cx.live().clone());
    let photos = Rc::clone(&window.photos);
    photos
        .borrow_mut()
        .attach_framework(cx.dispatcher(), cx.actions());
    // A search hit, or an agent's `open`, shows the photograph or album it
    // names.
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

/// The open photograph's own scale, for `Ctrl+=` / `Ctrl+-` / `Ctrl+0`,
/// `Ctrl+scroll` and a pinch: the picture zooms, not the window's text. In the
/// library the gesture is left to the window, which is what a grid wants.
impl ui::content_zoom::ContentZoom for PhotosApp {
    fn zoom_content(&mut self, step: ui::content_zoom::ZoomStep) -> Option<f32> {
        let mut photos = self.state();
        if photos.library_mode || photos.viewer.is_none() {
            return None;
        }
        Some(photos.zoom_photo(step))
    }
}

/// Showing photographs to a room: the window goes full screen while the
/// person presents, and leaving full screen any other way ends it.
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
        // Photos projects its custom media surfaces and retained buttons into
        // one complete semantic tree above. Inventory that visible tree once:
        // appending every retained widget here also advertises controls that
        // are currently hidden (other routes, closed sheets, disabled jobs),
        // producing orphan automation bindings with no matching UI target.
        // The buttons' canonical resources remain published by `resources`
        // and are dispatched through the same retained widgets.
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

    /// The system's back gesture lowers or puts away an editor's sheet
    /// before anything navigates (`Photos::sheet_back`).
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

    /// Shared controls remain retained children even in the painted media
    /// surface, with their own focus, action custody and accessibility.
    fn children(&self) -> Vec<&dyn Widget> {
        self.empty_state
            .as_ref()
            .map(|empty| empty as &dyn Widget)
            .into_iter()
            .collect()
    }

    /// Every widget the canvas may give a layout node: the empty state, the
    /// photograph, and the People surface's name field and buttons.
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

    /// The slide show is a timed presentation: the window sleeps until the
    /// next photograph is due rather than repainting while it waits.
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

    /// Hand the concrete app back out of a `Box<dyn Widget>`.
    ///
    /// A compositor hosting this in a tab holds it as a trait object, so
    /// "open this file in the photo viewer that is already on screen" has no
    /// route in without it. Opening still goes through
    /// [`Photos::open_path`] — this only recovers the type, it does not add
    /// a second way to change what is shown.
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

/// Lets the compositor host Photos in a tab directly, the same way it hosts
/// the terminal, the file explorer and the word processor, rather than
/// standing a placeholder card where the photograph should be.
impl ui::widget::AppWidget for PhotosApp {
    fn app_kind(&self) -> ui::widget::AppKind {
        // The toolkit's tab vocabulary has no picture variant. Photos edits a
        // file and writes the result back, which is what `Editor` names; the
        // glyph below is what actually tells the two apart in a tab pill.
        ui::widget::AppKind::Editor
    }

    fn app_title(&self) -> &str {
        &self.title
    }

    fn app_icon(&self) -> ui::widget::IconDescriptor {
        // Nerd Font image glyph, matching the convention every other hosted
        // app's tab pill uses.
        ui::widget::IconDescriptor::Glyph("\u{F02E9}")
    }

    fn permission_hint(&self) -> Option<ui::widget::AppKind> {
        Some(ui::widget::AppKind::Editor)
    }
}
