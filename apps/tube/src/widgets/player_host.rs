//! `PlayerHost` — mounts the app's persistent [`ui::widget::VideoPlayer`] into
//! the watch page without rebuilding it.
//!
//! Tube rebuilds its whole widget tree on every structural change (the
//! `RebuildHarness` pattern in [`crate::app`]). A `VideoPlayer` owns a live
//! [`media::MediaEngine`] (a running `GStreamer` pipeline with real audio), so it
//! must **survive** those rebuilds — recreating it every notice/like/navigation
//! would restart the video. The player therefore lives in [`crate::app::TubeApp`]
//! behind an `Rc<RefCell<_>>`; each rebuilt watch tree gets a fresh, cheap
//! `PlayerHost` pointing at that same shared player.
//!
//! `PlayerHost` is the taffy node (a fixed 16:9 leaf, same slot the poster used)
//! and forwards every `Widget` pass to the shared player. Only the host is
//! registered in the App's `node_map`; the inner player is never aliased there,
//! so the short `RefCell` borrows each pass takes can't overlap a raw-pointer
//! call. The App only ever touches the player through this host or through
//! `TubeApp`'s own (non-reentrant) short borrows.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use ui::widget::{AppModel, ModelPath};

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{FullscreenProvider, VideoPlayer, Widget};

/// A shared, persistent [`VideoPlayer`]. Cloned cheaply into each rebuilt tree.
pub type SharedPlayer = Rc<RefCell<VideoPlayer>>;

/// Stable id so headless tests can assert the watch view laid out the real
/// video-player surface.
pub const PLAYER_HOST_ID: &str = "watch_video_surface";

/// A taffy leaf that hosts the app's shared [`VideoPlayer`] at a fixed,
/// width-driven 16:9 height.
pub struct PlayerHost {
    player: SharedPlayer,
    /// 16:9-derived player height (logical px), matching the poster's slot.
    player_h: f32,
    /// This host's own laid-out rect (mirrors the inner player's).
    rect: Rect,
    /// The taffy node this host owns; the inner player is laid out against it.
    node: Option<NodeId>,
}

impl PlayerHost {
    pub fn new(player: SharedPlayer, player_h: f32) -> Self {
        Self {
            player,
            player_h,
            rect: Rect::ZERO,
            node: None,
        }
    }
}

impl FullscreenProvider for PlayerHost {
    fn fullscreen_intent(&self) -> Option<bool> {
        FullscreenProvider::fullscreen_intent(&*self.player.borrow())
    }

    fn on_fullscreen_changed(&mut self, active: bool) {
        FullscreenProvider::on_fullscreen_changed(&mut *self.player.borrow_mut(), active);
    }
}

impl Widget for PlayerHost {
    ui::provides!(dyn ui::widget::FullscreenProvider);

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        // Pumps the engine (pulls the freshest decoded frame + playhead).
        self.player.borrow_mut().sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Same slot the poster occupied: full column width, fixed 16:9 height.
        // The host (not the inner player) is registered in `node_map`, so events
        // route here and delegate — the inner player is never aliased by a raw
        // pointer.
        let node = super::leaf_node(
            taffy,
            Style {
                size: taffy::Size {
                    width: percent(1.0_f32),
                    height: length(self.player_h),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = super::layout_rect(taffy, node, origin);
        // Drive the shared player's rect from this host's node.
        self.player.borrow_mut().apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.player
            .borrow_mut()
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        self.player.borrow_mut().handle_event(event, ctx);
    }

    fn name(&self) -> &'static str {
        "PlayerHost"
    }

    fn test_id(&self) -> Option<&str> {
        Some(PLAYER_HOST_ID)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.player.borrow().cursor_shape(point)
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.player.borrow_mut().set_focus(focused);
    }

    fn is_focused(&self) -> bool {
        self.player.borrow().is_focused()
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.player.borrow().accessibility()
    }

    fn needs_animation_frame(&self) -> bool {
        self.player.borrow().needs_animation_frame()
    }

    fn needs_redraw(&self) -> bool {
        self.player.borrow().needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.player.borrow_mut().mark_drawn();
    }

    fn is_layout_dirty(&self) -> bool {
        // Fixed 16:9 slot: never layout-dirty on its own (the inner player never
        // changes its own intrinsic size), so a playing video repaints without
        // forcing a relayout every frame.
        false
    }
}
