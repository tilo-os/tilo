//! An [`App`] shown in a region of a host's scene: a tab of a desktop, a
//! pane of a device's screen. The app keeps everything a window of its own
//! would have (its input handling, focus, palette, history, collaboration
//! and the actions it publishes); the host only lays it out, hands it the
//! region's input and draws it where it sits.

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

/// How many frames an embedded app may run in one host frame to settle a
/// sync or layout its previous frame left.
const SETTLE_FRAMES: usize = 3;

/// An app a host shows in a region of its own scene.
pub struct EmbeddedApp {
    app: App,
    rect: KurboRect,
    region: Region,
    focused: bool,
    /// The control that had focus when focus last left the app.
    restore_focus: Option<NodeId>,
    /// Shift was held as focus last moved, so focus entering the app lands
    /// on its last control.
    entering_backwards: bool,
    /// The app asked for another frame.
    wants_frame: Cell<bool>,
    /// Painted into the host's scene at the region's origin, and painted
    /// again as it is until the app has something new to show.
    painted: RefCell<Scene>,
    /// The damage parts ([`crate::scene_parts`]) recorded with `painted`.
    painted_parts: RefCell<Option<crate::scene_parts::SectionParts>>,
    /// The app's background work woke it since its last frame.
    woken: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl EmbeddedApp {
    /// Show `app` in a region of a host's scene. `wake` is told when the
    /// app's background work wants it to run again.
    ///
    /// # Errors
    /// The app's wake scope is already bound to another host.
    pub fn new(
        mut app: App,
        wake: impl Fn(crate::wake::WakeEvent) + Send + Sync + 'static,
    ) -> Result<Self, crate::launch::LaunchError> {
        app.set_tab_order_continues_in_host(true);
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
        Ok(Self {
            app,
            rect: KurboRect::ZERO,
            region: Region {
                size: winit::dpi::PhysicalSize::new(0, 0),
                redraw: Cell::new(true),
                cursor: Cell::new(None),
            },
            focused: false,
            restore_focus: None,
            entering_backwards: false,
            wants_frame: Cell::new(true),
            painted: RefCell::new(Scene::new()),
            painted_parts: RefCell::new(None),
            woken,
        })
    }

    /// The app shown.
    pub fn app(&self) -> &App {
        &self.app
    }

    /// The app shown, to drive it.
    pub fn app_mut(&mut self) -> &mut App {
        &mut self.app
    }

    /// Ask the real embedded-host draw path for a fresh frame after a
    /// test-control fixture changes app-owned presence state.
    #[cfg(feature = "test-support")]
    pub fn request_frame_for_test(&self) {
        self.wants_frame.set(true);
    }

    /// The root widget of the app's component: what an app's own tests
    /// reach into, as they would in the app's window.
    pub fn component_root_mut(&mut self) -> Option<&mut dyn Widget> {
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
        self.app
            .root_widget
            .as_any_mut()?
            .downcast_mut::<crate::widget::BoundAppWidget>()
            .map(|root| root as &mut dyn AppWidget)
    }
}

impl Widget for EmbeddedApp {
    crate::provides!(
        dyn crate::component_lifecycle::ComponentLifecycle,
        dyn crate::widget::CompleteContentProvider
    );

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The app looks as the host does around it. A new palette must
        // replace its retained scene even when the region's size is unchanged.
        if self.app.accessibility.base_theme != *theme {
            self.app.set_theme(theme.clone());
            self.wants_frame.set(true);
        }
        let style = taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            ..taffy::Style::default()
        };
        let Ok(node) = taffy.new_leaf(style) else {
            return NodeId::new(0);
        };
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        let rect = crate::widget::placed(layout, origin);
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
            .app
            .root_widget
            .next_wake_deadline()
            .is_some_and(|deadline| deadline <= crate::time::now());
        let woken = self.woken.swap(false, std::sync::atomic::Ordering::AcqRel);
        if self.wants_frame.get() || woken || due {
            // A frame whose sync or layout leaves more to settle runs again
            // at once, so what the host paints is the app at rest.
            let mut again = true;
            for _ in 0..SETTLE_FRAMES {
                // The app's own damage parts travel with its scene, so a
                // host that tracks parts repaints a caret, not the tile.
                let (settling, parts) = crate::scene_parts::record(&mut painted, |painted| {
                    painted.reset();
                    self.app.surface_frame(&self.region, painted)
                });
                again = settling;
                *painted_parts = parts;
                if !again || !self.app.unsettled() {
                    break;
                }
            }
            // What the host's accessibility walk reads of the app.
            let _ = self.app.refresh_semantic_tree();
            self.wants_frame
                .set(again || self.region.redraw.replace(false));
        }
        crate::scene_parts::append(
            scene,
            &painted,
            painted_parts.as_ref(),
            Some(transform * Affine::translate((self.rect.x0, self.rect.y0))),
        );
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
            .app
            .root_widget
            .next_wake_deadline()
            .is_some_and(|deadline| deadline <= crate::time::now());
        if !due {
            return false;
        }
        self.app.pump_hosted_model();
        // A durability that changed in this pass is shown by the next frame,
        // which rebuilds for it as it would had the frame sampled it itself.
        self.app.layout.structure_dirty |= self.app.root_widget.take_structure_dirty();
        let visible = self.app.unsettled() || self.app.root_widget.needs_animation_frame();
        if visible {
            self.wants_frame.set(true);
        }
        visible
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.entering_backwards = ctx.modifiers.state().shift_key();
        let local = self.local_event(event, ctx.scale_factor);
        self.app.set_surface_modifiers(*ctx.modifiers);
        if self.app.surface_input(&self.region, local, false) {
            *ctx.handled = true;
        }
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

    /// The id of the app shown.
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
        if focused == self.focused {
            return;
        }
        self.focused = focused;
        if focused {
            if !self.app.restore_focused_control(self.restore_focus.take())
                || self.app.focused_control().is_none()
            {
                self.app.focus_order_edge(self.entering_backwards);
            }
        } else {
            self.restore_focus = self.app.focused_control();
            self.app.restore_focused_control(None);
        }
        self.wants_frame.set(true);
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

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        self.app.ime_window_state().map(|field| field.purpose)
    }

    fn ime_multiline(&self) -> bool {
        self.app
            .ime_window_state()
            .is_some_and(|field| field.multiline)
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

    fn start_deferred_text(&mut self) -> Option<crate::webview::text_input::Receipt> {
        self.app
            .mutate_node(self.app.focused_node?, |widget| {
                widget.start_deferred_text()
            })
            .ok()
            .flatten()
    }

    fn commit_deferred_text(&mut self, text: &str) -> Option<crate::webview::text_input::Receipt> {
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
        self.wants_frame.get()
    }

    fn has_pending_visual_work(&self) -> bool {
        self.app.root_widget.has_pending_visual_work()
    }

    fn has_transient_feedback(&self) -> bool {
        self.app.root_widget.has_transient_feedback()
    }

    /// The host's presentation preferences are the app's.
    fn set_motion_reduced(&mut self, reduced: bool) {
        self.app.set_reduce_motion(reduced);
    }

    fn set_platform_conventions(&mut self, conventions: crate::platform::PlatformConventions) {
        self.app.set_platform_conventions(conventions);
    }

    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        self.app.set_interaction_environment(environment);
    }

    /// The app's own accessibility tree, as this tile's subtree: a screen
    /// reader walks from the host straight into the app.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let tree = self.app.semantic_tree();
        let offset = vello::kurbo::Vec2::new(self.rect.x0, self.rect.y0);
        let root = tree.node(tree.root())?;
        let mut node = crate::a11y::AccessNode::new(crate::a11y::Role::Group);
        node.name = AppWidget::app_title(self).to_owned().into();
        node.children = root
            .children
            .iter()
            .filter_map(|child| access_subtree(tree, child, offset))
            .collect();
        Some(node)
    }

    fn perform_action_at(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        let Some(id) = semantic_target(self.app.semantic_tree(), key_path) else {
            return false;
        };
        let Some(name) = self
            .app
            .semantic_tree()
            .node(&id)
            .and_then(|node| {
                node.actions.iter().find(|candidate| {
                    matches!(&candidate.kind, crate::a11y_tree::SemanticActionKind::Verb(verb)
                        if std::mem::discriminant(verb) == std::mem::discriminant(action))
                })
            })
            .map(|candidate| candidate.name.clone())
        else {
            return false;
        };
        let argument = match action {
            crate::a11y::Action::SetValue(value) => Some(value.as_str()),
            _ => None,
        };
        let done = self
            .app
            .invoke_semantic_action(&id, &name, argument)
            .unwrap_or(false);
        self.wants_frame.set(true);
        done
    }

    fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        self.app.root_widget.next_wake_deadline()
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
        crate::component_lifecycle::checkpoint_components(self.app.root_widget.as_mut())
    }
    fn finish_admitted_close(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        crate::component_lifecycle::finish_admitted_close(self.app.root_widget.as_mut())
    }
    fn close_components(&mut self) -> Vec<crate::application::ComponentCloseReport> {
        self.app.close_components();
        self.app.take_component_close_reports()
    }
    fn take_component_close_reports(&mut self) -> Vec<crate::application::ComponentCloseReport> {
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
    /// The app keeps its own record; the host binds none for it.
    fn uses_host_record(&self) -> bool {
        false
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
        self.app_widget_mut()
            .ok_or("Terminal is not available")?
            .device_shell_operation(operation)
    }
    fn request_device_shell(&mut self) -> bool {
        self.app_widget_mut()
            .is_some_and(AppWidget::request_device_shell)
    }
    fn take_host_intent(&mut self) -> Option<crate::widget::AppHostIntent> {
        self.app_widget_mut()?.take_host_intent()
    }
}

/// The app's semantic node `id` and what is below it, as synthetic
/// accessibility nodes keyed by their semantic ids, placed in the host.
/// Nodes with no place on screen (published resources) are left out.
fn access_subtree(
    tree: &crate::a11y_tree::SemanticTree,
    id: &crate::a11y_tree::SemanticId,
    offset: vello::kurbo::Vec2,
) -> Option<crate::a11y::AccessNode> {
    let node = tree.node(id)?;
    let bounds = node.bounds?;
    let mut access = crate::a11y::AccessNode::new(node.role);
    access.name.clone_from(&node.name);
    access.value.clone_from(&node.value);
    access.state = node.state;
    access.key = Some(crate::a11y::hash_key(&id.as_str()));
    let placed = bounds + offset;
    access.bounds = Some(crate::a11y::AccessBounds {
        x0: placed.x0,
        y0: placed.y0,
        x1: placed.x1,
        y1: placed.y1,
    });
    access.children = node
        .children
        .iter()
        .filter_map(|child| access_subtree(tree, child, offset))
        .collect();
    Some(access)
}

/// The semantic node a key path from [`access_subtree`] names.
fn semantic_target(
    tree: &crate::a11y_tree::SemanticTree,
    key_path: &[u64],
) -> Option<crate::a11y_tree::SemanticId> {
    let mut current = tree.root().clone();
    for key in key_path {
        current = tree
            .node(&current)?
            .children
            .iter()
            .find(|child| crate::a11y::hash_key(&child.as_str()) == *key)?
            .clone();
    }
    (!key_path.is_empty()).then_some(current)
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
}

/// TIL-696: a tile whose record was still being written at its last frame
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

    fn due(tile: &EmbeddedApp, now: crate::time::Instant) -> bool {
        tile.next_wake_deadline()
            .is_some_and(|deadline| deadline <= now)
    }

    #[test]
    fn a_record_saving_at_the_last_frame_is_resampled_by_the_pump_not_a_paint() {
        let host = ProbeHost::new();
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
                automerge::transaction::Transactable::put(doc, automerge::ROOT, "til-696", 1_u64)
                    .unwrap();
            })
            .unwrap();
        sample(&mut tile);
        assert_eq!(sampled(&mut tile), Some(RecordSaveStatus::Saving));
        assert!(tile.next_wake_deadline().is_some());

        // Still being written when the deadline comes due: the pump samples
        // it again and the deadline moves on. Nothing changed on screen, so
        // no frame is asked for.
        bound(&mut tile).age_record_save_sample();
        assert!(due(&tile, crate::time::now()));
        assert!(
            !pump(&mut tile),
            "a resample that changed nothing paints nothing"
        );
        assert!(!tile.needs_redraw());
        assert_eq!(sampled(&mut tile), Some(RecordSaveStatus::Saving));
        assert!(
            !due(&tile, crate::time::now()),
            "the resample re-arms the deadline from itself"
        );

        // The writer finishes off the event loop and wakes nobody: the stale
        // deadline is all that is left of the save.
        binding.records.try_flush_record(&id).unwrap();
        binding.records.await_submitted(&id).unwrap();
        assert_eq!(
            binding.records.save_status(&id).unwrap(),
            RecordSaveStatus::Saved
        );
        bound(&mut tile).age_record_save_sample();
        let now = crate::time::now();
        assert!(due(&tile, now));

        // The pump, with no draw, finds it saved: the deadline is gone and one
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
