//! One offscreen frame of a widget, for app-specific screenshot handlers.
//!
//! [`LaunchSpec::shot_root`](super::LaunchSpec::shot_root) captures through
//! the full headless harness. Apps whose screenshots stage state between the
//! passes of a frame (seed the document after the first sync, pin a
//! selection once layout has settled, warm an image cache between two draws)
//! drive the passes themselves through [`ShotFrame`]:
//! `sync` → `build_layout` → layout → `apply_layout` → draw → PNG, the same
//! cycle as `ui::App`. [`render_widget_png`] runs that cycle unchanged.

use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;

use taffy::prelude::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};

use crate::bound_dispatch::BoundNodeMap;
#[cfg(not(target_arch = "wasm32"))]
use crate::offscreen::OffscreenRenderer;
use crate::permissions::PermissionContext;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::widget::{FloatingSurfaceWidgetExt, Widget, WidgetLayoutError};
use crate::{State, Theme};

/// The document, theme, text renderer and layout tree of one offscreen frame.
pub struct ShotFrame {
    /// Document the widget syncs from. Seed it before [`ShotFrame::sync`].
    pub state: State,
    /// Theme passed to layout and every draw pass.
    pub theme: Theme,
    /// Text renderer shared by measurement and drawing.
    pub text: TextRenderer,
    /// Presence passed to the draw passes; empty unless a shot stages peers.
    pub presence: PresenceCtx,
    /// Permission context scoped around the root `draw`.
    pub permission: PermissionContext,
    viewport: (u32, u32),
    taffy: TaffyTree<()>,
    node_map: HashMap<NodeId, *mut dyn Widget>,
    child: Option<NodeId>,
    /// The floating surfaces the last [`ShotFrame::draw`] and the
    /// [`ShotFrame::draw_overlay`] after it drew over the blurred material,
    /// with what lay below each; the PNG blurs them as a live window does.
    backdrop: Vec<crate::backdrop::RecordedBackdrop>,
}

impl ShotFrame {
    /// A frame at `viewport` with a fresh document, the theme's text scale
    /// applied to the text renderer, and settled layout. Its PNG lands on the
    /// window ground (`surfaces.content`), as a live window clears.
    #[must_use]
    pub fn new(viewport: (u32, u32), theme: Theme) -> Self {
        let mut text = TextRenderer::new();
        text.set_text_scale(theme.text_scale);
        Self {
            state: State::new(),
            theme,
            text,
            presence: PresenceCtx::empty(),
            permission: PermissionContext::permissive(),
            viewport,
            taffy: TaffyTree::new(),
            node_map: HashMap::new(),
            child: None,
            backdrop: Vec::new(),
        }
    }

    /// Replace the document the widget syncs from.
    #[must_use]
    pub fn with_state(mut self, state: State) -> Self {
        self.state = state;
        self
    }

    /// The viewport this frame lays out and renders at.
    #[must_use]
    pub fn viewport(&self) -> (u32, u32) {
        self.viewport
    }

    /// Pull the widget's visual state from [`ShotFrame::state`].
    pub fn sync(&mut self, widget: &mut dyn Widget) {
        widget.sync(&self.state.doc, &self.state.root);
        widget.settle(
            &mut crate::widget::Edits::lend(&mut self.state.doc),
            &self.state.root,
        );
        widget.sync(&self.state.doc, &self.state.root);
    }

    /// Build a fresh layout tree for the widget inside a viewport-sized root,
    /// resolve it, and apply the result.
    ///
    /// # Errors
    ///
    /// Returns the solver's error when Taffy rejects the tree or allocation
    /// dependent styles do not settle.
    pub fn layout(&mut self, widget: &mut dyn Widget) -> Result<(), WidgetLayoutError> {
        self.layout_at(widget, self.viewport)
    }

    /// [`ShotFrame::layout`] at a viewport other than the frame's own, to
    /// simulate the window size a widget saw before a resize.
    ///
    /// # Errors
    ///
    /// As [`ShotFrame::layout`].
    #[allow(
        clippy::cast_precision_loss,
        reason = "viewport dimensions are screen/image sizes, always far under 2^24"
    )]
    pub fn layout_at(
        &mut self,
        widget: &mut dyn Widget,
        (w, h): (u32, u32),
    ) -> Result<(), WidgetLayoutError> {
        let (w, h) = (w as f32, h as f32);
        self.child = None;
        self.taffy = TaffyTree::new();
        self.node_map = HashMap::new();
        let child = widget.build_layout(&mut self.taffy, &self.theme, &mut self.node_map);
        let root = self.taffy.new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(w),
                    height: Dimension::length(h),
                },
                ..Default::default()
            },
            &[child],
        )?;
        let available = Size {
            width: AvailableSpace::Definite(w),
            height: AvailableSpace::Definite(h),
        };
        let mut bindings = BoundNodeMap::new();
        // SAFETY: `widget`, `taffy`, `child` and `node_map` come from the
        // build above and stay exclusively borrowed for layout.
        unsafe {
            bindings.rebuild(widget, &self.taffy, child, &self.node_map);
            crate::widget::compute_widget_layout(
                &mut self.taffy,
                root,
                available,
                &self.node_map,
                &bindings,
                &mut self.text,
            )?;
        }
        widget.apply_layout(&self.taffy, child, Point::ORIGIN);
        self.child = Some(child);
        Ok(())
    }

    /// The document, the layout tree of the last [`ShotFrame::layout`] and the
    /// widget's node in it, for scenarios that stage input against laid-out
    /// geometry.
    pub fn layout_parts(&mut self) -> Option<(&mut State, &TaffyTree<()>, NodeId)> {
        let child = self.child?;
        Some((&mut self.state, &self.taffy, child))
    }

    /// The window the frame stands for, in its own coordinates.
    fn window(&self) -> vello::kurbo::Rect {
        vello::kurbo::Rect::new(
            0.0,
            0.0,
            f64::from(self.viewport.0),
            f64::from(self.viewport.1),
        )
    }

    /// Paint the widget's main pass into a new scene, culled against the
    /// frame's window as a live host culls.
    pub fn draw(&mut self, widget: &mut dyn Widget) -> Scene {
        let mut scene = Scene::new();
        let recording = crate::backdrop::BackdropRecording::begin();
        crate::backdrop::layer_scene(&scene, 0);
        let previous = self.presence.push_viewport(self.window());
        self.presence.with_permissions(self.permission.clone(), || {
            widget.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut self.text,
                &self.theme,
                &self.presence,
            )
        });
        self.presence.pop_viewport(previous);
        self.backdrop = recording.finish();
        scene
    }

    /// Composite the widget's top layer (open menus, popovers, modals) onto
    /// `scene` over the frame's window, as the live host does after the main
    /// pass and the window chrome.
    pub fn draw_overlay(&mut self, widget: &mut dyn Widget, scene: &mut Scene) {
        let recording =
            crate::backdrop::BackdropRecording::continuing(std::mem::take(&mut self.backdrop));
        crate::backdrop::layer_scene(scene, 0);
        let previous = self.presence.push_viewport(self.window());
        widget.draw_overlay(
            scene,
            Affine::IDENTITY,
            &mut self.text,
            &self.theme,
            &self.presence,
        );
        self.presence.pop_viewport(previous);
        self.backdrop = recording.finish();
    }

    /// Paint app-owned floating surfaces onto `scene`, in the order the live
    /// compositor stacks them above the window.
    pub fn draw_floating_surfaces(&mut self, widget: &mut dyn Widget, scene: &mut Scene) {
        widget.draw_floating_surfaces(
            scene,
            Affine::IDENTITY,
            &mut self.text,
            &self.theme,
            &self.presence,
        );
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ShotFrame {
    /// Rasterise `scene` over the window ground at the frame's viewport and
    /// publish it to `out` as a PNG, creating the parent directory.
    ///
    /// # Panics
    ///
    /// Panics when the offscreen renderer cannot publish the file, as
    /// [`OffscreenRenderer::render_png`] does.
    pub fn write_png(&self, scene: &Scene, out: &Path) {
        self.write_png_on(scene, self.theme.tokens.colors.surfaces.content, out);
    }

    fn write_png_on(&self, scene: &Scene, background: vello::peniko::Color, out: &Path) {
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let (w, h) = self.viewport;
        let mut renderer = OffscreenRenderer::new(w, h);
        renderer.set_backdrop(Some(std::sync::Arc::new(
            crate::backdrop::BackdropFrame::from_single_scene(self.backdrop.clone()),
        )));
        renderer.render_png(scene, background, out);
    }
}

/// Render one frame of `widget` at `viewport` under `theme` and write it to
/// `out` as a PNG: [`ShotFrame`] with its defaults, synced once, laid out
/// once and drawn once with its top layer.
///
/// # Errors
///
/// As [`ShotFrame::layout`].
#[cfg(not(target_arch = "wasm32"))]
pub fn render_widget_png(
    widget: &mut dyn Widget,
    viewport: (u32, u32),
    theme: Theme,
    out: &Path,
) -> Result<(), WidgetLayoutError> {
    let mut frame = ShotFrame::new(viewport, theme);
    frame.sync(widget);
    frame.layout(widget)?;
    let mut scene = frame.draw(widget);
    frame.draw_overlay(widget, &mut scene);
    frame.write_png(&scene, out);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::Spacer;

    #[test]
    fn shot_frames_follow_the_theme_scale_and_lay_out_inside_the_viewport() {
        // phase: text_scale_follows_the_theme
        {
            let frame = ShotFrame::new((100, 100), Theme::dark().with_text_scale(2.0));
            assert!((frame.text.text_scale() - 2.0).abs() < f32::EPSILON);
        }
        // phase: the_widget_is_laid_out_inside_the_viewport
        {
            let mut widget = Spacer::new();
            let mut frame = ShotFrame::new((320, 200), Theme::dark());
            assert!(frame.layout_parts().is_none(), "no layout yet");
            frame.sync(&mut widget);
            frame.layout(&mut widget).unwrap();
            let rect = widget.get_rect();
            assert_eq!((rect.x0, rect.y0), (0.0, 0.0));
            assert!(rect.x1 <= 320.0 && rect.y1 <= 200.0, "{rect:?}");
            let (_, taffy, child) = frame.layout_parts().expect("laid out");
            assert!(taffy.layout(child).is_ok());

            frame.layout_at(&mut widget, (100, 50)).unwrap();
            let rect = widget.get_rect();
            assert!(rect.x1 <= 100.0 && rect.y1 <= 50.0, "{rect:?}");
        }
    }
}
