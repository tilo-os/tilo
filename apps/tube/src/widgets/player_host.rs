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
//! `PlayerHost` is the taffy node (a fixed 16:9 leaf)
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

use ui::ImageStore;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    FullscreenProvider, MediaArt, MediaAspect, MediaCardFace, MediaSource, TaffyTreeExt,
    VideoPlayer, Widget, layout_rect,
};

/// A shared, persistent [`VideoPlayer`]. Cloned cheaply into each rebuilt tree.
pub type SharedPlayer = Rc<RefCell<VideoPlayer>>;

/// Stable id so headless tests can assert the watch view laid out the real
/// video-player surface.
pub const PLAYER_HOST_ID: &str = "watch_video_surface";

/// A taffy leaf that hosts the app's shared [`VideoPlayer`] at a fixed,
/// width-driven 16:9 height.
pub struct PlayerHost {
    player: SharedPlayer,
    /// 16:9-derived player height (logical px).
    player_h: f32,
    /// This host's own laid-out rect (mirrors the inner player's).
    rect: Rect,
    /// The taffy node this host owns; the inner player is laid out against it.
    node: Option<NodeId>,
    /// The caption lines of the open video: the host paints the one the
    /// playhead is inside over the picture's lower edge, which the shared
    /// player has no other door for. Empty when captions are off or the video
    /// offers none.
    captions: Vec<crate::captions::CaptionCue>,
}

impl PlayerHost {
    pub fn new(player: SharedPlayer, player_h: f32) -> Self {
        Self {
            player,
            player_h,
            rect: Rect::ZERO,
            node: None,
            captions: Vec::new(),
        }
    }

    /// The caption lines to show over the picture, when the person turned
    /// them on and the video offers them.
    #[must_use]
    pub fn with_captions(mut self, captions: Vec<crate::captions::CaptionCue>) -> Self {
        self.captions = captions;
        self
    }

    /// The caption line the playhead is inside, painted over the picture's
    /// lower edge: on the media's own scrim in the media ink, above the
    /// progress line, and above the transport when the controls are showing so
    /// the words are never behind a control.
    fn paint_captions(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.captions.is_empty() || self.rect.width() <= 0.0 {
            return;
        }
        let (position_ms, controls_shown) = {
            let player = self.player.borrow();
            (player.position_ms(), player.controls_visible())
        };
        let Some(cue) = crate::captions::active_cue(&self.captions, position_ms) else {
            return;
        };
        let style = theme.text(ui::TextRole::Body);
        let inset = f64::from(theme.tokens.spacing.s3);
        let max_text = (self.rect.width() * 0.86).max(1.0);
        let (width, height) = text_renderer.measure_style(&cue.text, Some(max_text as f32), &style);
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        let pad_x = f64::from(theme.tokens.spacing.s3);
        let pad_y = f64::from(theme.tokens.spacing.s1);
        let clear_of_controls = if controls_shown {
            self.player.borrow().transport_rect(theme).y0 - inset
        } else {
            self.rect.y1 - inset
        };
        let y1 = clear_of_controls.max(self.rect.y0);
        let y0 = (y1 - f64::from(height) - 2.0 * pad_y).max(self.rect.y0);
        let x0 = (self.rect.center().x - f64::from(width) / 2.0 - pad_x).max(self.rect.x0);
        let x1 = (self.rect.center().x + f64::from(width) / 2.0 + pad_x).min(self.rect.x1);
        let scrim = Rect::new(x0, y0, x1, y1);
        // A caption is a media overlay, not a control: the scrim and the line
        // are painted together on purpose, and `paint_caption` keeps them out of
        // reach of anything that would draw one without the other.
        let caption = CaptionOverlay {
            scrim,
            pad_x,
            pad_y,
            max_text,
        };
        paint_caption(scene, text_renderer, theme, transform, &caption, &cue.text);
    }
    /// Show the video's art until the player has a frame to show, so a
    /// watch page that is still starting playback never stands empty. The
    /// art is the player's own poster, under its controls: the thumbnail
    /// (`url` from `store`), or without one the art generated for `title`,
    /// the same its card shows.
    #[must_use]
    pub fn with_poster(self, store: Option<ImageStore>, url: &str, title: &str) -> Self {
        let poster = if let Some(store) = store.filter(|_| !url.is_empty()) {
            MediaSource::Url {
                store,
                url: url.to_string(),
            }
        } else {
            let title = title.to_string();
            MediaSource::Paint(Box::new(move |ctx, rect| {
                MediaCardFace::new(MediaArt::Placeholder)
                    .aspect(MediaAspect::Wide)
                    .title(&title)
                    .paint(ctx, rect);
            }))
        };
        self.player.borrow_mut().set_poster(Some(poster));
        self
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

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        // Pumps the engine (pulls the freshest decoded frame + playhead).
        self.player.borrow_mut().sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Full column width, fixed 16:9 height.
        // The host (not the inner player) is registered in `node_map`, so events
        // route here and delegate — the inner player is never aliased by a raw
        // pointer.
        let node = taffy.leaf_node(Style {
            size: taffy::Size {
                width: percent(1.0_f32),
                height: length(self.player_h),
            },
            flex_shrink: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = layout_rect(taffy, node, origin);
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
        self.paint_captions(scene, transform, text_renderer, theme);
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

/// Where a caption sits and how wide it may be.
struct CaptionOverlay {
    scrim: Rect,
    pad_x: f64,
    pad_y: f64,
    max_text: f64,
}

/// A caption over the picture: its scrim, then its line.
fn paint_caption(
    scene: &mut vello::Scene,
    text_renderer: &mut ui::TextRenderer,
    theme: &Theme,
    transform: vello::kurbo::Affine,
    caption: &CaptionOverlay,
    text: &str,
) {
    let (scrim, pad_x, pad_y, max_text) = (
        caption.scrim,
        caption.pad_x,
        caption.pad_y,
        caption.max_text,
    );
    let media = &theme.tokens.colors.media;
    let style = theme.text(ui::TextRole::Body);
    scene.fill(
        vello::peniko::Fill::NonZero,
        transform,
        media.scrim,
        None,
        &vello::kurbo::RoundedRect::new(
            scrim.x0,
            scrim.y0,
            scrim.x1,
            scrim.y1,
            f64::from(theme.tokens.spacing.s2),
        ),
    );
    text_renderer.draw_style(
        scene,
        text,
        media.ink,
        transform.then_translate(vello::kurbo::Vec2::new(scrim.x0 + pad_x, scrim.y0 + pad_y)),
        Some(max_text as f32),
        &style,
    );
}
