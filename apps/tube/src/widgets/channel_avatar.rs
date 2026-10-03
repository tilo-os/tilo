//! `ChannelAvatar` — a circular uploader avatar that resolves its real photo
//! **lazily** from the [`ui::ImageStore`] and paints it circle-clipped, falling
//! back to a deterministic initials chip until (or unless) the image decodes.
//!
//! Why a Tube-local widget instead of `ui::widget::Avatar`? Two reasons, both
//! about scroll performance on the thumbnail grids:
//!
//! 1. **Lazy, rebuild-free image loading.** The card holds an [`ImageStore`]
//!    handle + the avatar URL and looks the decoded image up in [`sync`](crate::sync), so a
//!    photo that finishes downloading folds in on the next *redraw* — no
//!    whole-tree structural rebuild (which is what `imgs.get(url)` at build time
//!    forced, via the app's image-pending refresh, on every single completion).
//!
//! 2. **Layout-dirty keyed off the laid-out rect, not the drawn cache.**
//!    `ui::Avatar::is_layout_dirty` is `cache_rect == ZERO`, and `cache_rect` is
//!    only written during `draw`. An avatar scrolled off-screen is *culled* (never
//!    drawn), so it reports "layout dirty" forever — which makes the enclosing
//!    `NavSplit` re-solve its whole content pane (rebuilding a cosmic-text
//!    `FontSystem`!) on every scroll frame. Keying `is_layout_dirty` off the
//!    apply-set `rect` (like `Text`/`Thumbnail` do) keeps a culled avatar quiet,
//!    so a pure scroll stays on the cheap re-apply path.

use std::collections::HashMap;
use ui::widget::{AppModel, ModelPath};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};
use vello::peniko::{BlendMode, Fill, ImageBrush, ImageData as PenikoImage};

use ui::ImageStore;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Avatar, Widget};

pub struct ChannelAvatar {
    /// Background loader/cache; `None` on the hermetic offline / `--shots` path
    /// (then only the initials chip ever shows).
    store: Option<ImageStore>,
    /// Avatar image URL (empty ⇒ initials only).
    url: String,
    /// Uppercased, ≤2-char initials fallback.
    initials: String,
    size: f32,
    /// Decoded avatar once the store hands it over. Resolved in `sync`.
    image: Option<PenikoImage>,
    rect: Rect,
    dirty: bool,
}

impl ChannelAvatar {
    /// A `size`×`size` avatar for `name`, resolving `url` lazily from `store`.
    /// An empty `url` (or a `None` store) shows only the initials chip.
    pub fn new(store: Option<ImageStore>, url: &str, name: &str, size: f32) -> Self {
        let initials: String = crate::util::initials(name);
        Self {
            store,
            url: url.to_string(),
            initials,
            size,
            image: None,
            rect: Rect::ZERO,
            dirty: true,
        }
    }

    /// Test/`--shots` hook: inject an already-decoded image directly (bypassing
    /// the store), so a deterministic test can prove the image path paints.
    #[must_use]
    pub fn with_image(mut self, image: Option<PenikoImage>) -> Self {
        self.image = image;
        self
    }

    fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        if let Some(img) = &self.image {
            // Circle-clip → cover-fit the decoded photo → pop.
            let centre = self.rect.center();
            let radius = self.rect.width().min(self.rect.height()) * 0.5;
            let clip = Circle::new(centre, radius);
            scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &clip);
            let iw = f64::from(img.width.max(1));
            let ih = f64::from(img.height.max(1));
            let scale = (self.rect.width() / iw).max(self.rect.height() / ih);
            let dw = iw * scale;
            let dh = ih * scale;
            let dx = self.rect.x0 + (self.rect.width() - dw) * 0.5;
            let dy = self.rect.y0 + (self.rect.height() - dh) * 0.5;
            let brush_transform = Affine::translate((dx, dy)) * Affine::scale(scale);
            let brush = ImageBrush::new(img.clone());
            scene.fill(
                Fill::NonZero,
                transform,
                &brush,
                Some(brush_transform),
                &self.rect,
            );
            scene.pop_layer();
        } else {
            // Deterministic initials chip — identical colour + glyphs to
            // `ui::Avatar`, so a photo dropping in is the only visible change.
            let color = Avatar::color_for_initials(&self.initials, theme);
            Avatar::draw_at(
                scene,
                transform,
                self.rect,
                &self.initials,
                color,
                theme,
                text_renderer,
            );
        }
    }
}

impl Widget for ChannelAvatar {
    fn sync(&mut self, _state: &mut AppModel, _path: &ModelPath) {
        // Lazily resolve the real avatar. Only touch the store until we have the
        // image (an `Arc` clone thereafter would be wasted work); a freshly
        // decoded photo flips `dirty` so the next frame repaints it — no rebuild.
        if self.image.is_none()
            && !self.url.is_empty()
            && let Some(store) = &self.store
            && let Some(img) = store.get(&self.url)
        {
            self.image = Some(img);
            self.dirty = true;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = super::leaf_node(
            taffy,
            Style {
                size: Size {
                    width: Dimension::length(self.size),
                    height: Dimension::length(self.size),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let new = super::layout_rect(taffy, node, origin);
        if new != self.rect {
            self.dirty = true;
        }
        self.rect = new;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.paint(scene, transform, text_renderer, theme);
    }

    fn name(&self) -> &'static str {
        "ChannelAvatar"
    }
    fn needs_redraw(&self) -> bool {
        self.dirty
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
    /// Keyed off the **apply-set** rect (not a draw-time cache), so a culled
    /// off-screen avatar never falsely reports "needs measure" and drags the
    /// enclosing split into a full re-solve every scroll frame.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Role};
        let name = (!self.initials.is_empty()).then(|| self.initials.clone());
        Some(AccessNode::new(Role::Image).with_optional_name(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use vello::peniko::{Blob, ImageAlphaType, ImageFormat as Format};
    use vello_encoding::DrawTag;

    fn tiny_image() -> PenikoImage {
        let blob = Blob::new(Arc::new(vec![255u8; 2 * 2 * 4]));
        PenikoImage {
            data: blob,
            format: Format::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 2,
            height: 2,
        }
    }

    fn draw_tags(av: &mut ChannelAvatar) -> Vec<DrawTag> {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        av.rect = Rect::new(0.0, 0.0, 40.0, 40.0);
        av.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        scene.encoding().draw_tags.clone()
    }

    #[test]
    fn channel_avatar_paints_image_or_initials_and_stays_clean() {
        // Case `resolved_avatar_paints_circle_clipped_image`:
        // A resolved avatar paints a circle-clipped IMAGE (`BEGIN_CLIP` < IMAGE <
        // `END_CLIP`) — proof the real creator photo reaches the scene.
        {
            let mut av = ChannelAvatar::new(None, "http://x/a.png", "Blender", 40.0)
                .with_image(Some(tiny_image()));
            let tags = draw_tags(&mut av);
            let begin = tags.iter().position(|t| *t == DrawTag::BEGIN_CLIP);
            let image = tags.iter().position(|t| *t == DrawTag::IMAGE);
            let end = tags.iter().rposition(|t| *t == DrawTag::END_CLIP);
            assert!(
                matches!((begin, image, end), (Some(b), Some(i), Some(e)) if b < i && i < e),
                "avatar image must be circle-clipped (BEGIN_CLIP < IMAGE < END_CLIP); got {:?}",
                tags.iter().map(|t| t.0).collect::<Vec<_>>(),
            );
        }
        // Case `unresolved_avatar_paints_initials_not_image`:
        // Without an image the avatar paints the initials chip (a COLOR circle),
        // never an IMAGE — so the offline/`--shots` path is unchanged.
        {
            let mut av = ChannelAvatar::new(None, "", "Blender", 40.0);
            let tags = draw_tags(&mut av);
            assert!(!tags.contains(&DrawTag::IMAGE), "no image → initials only");
            assert!(
                tags.contains(&DrawTag::COLOR),
                "initials chip fills a circle"
            );
        }
        // Case `laid_out_but_never_drawn_avatar_is_not_layout_dirty`:
        // A culled (never-drawn) avatar with a real rect reports it is NOT layout
        // dirty — the property that stops the enclosing split re-solving on scroll.
        // (The `ui::Avatar` bug this widget was written to avoid fails exactly here.)
        {
            let mut av = ChannelAvatar::new(None, "", "Blender", 40.0);
            assert!(
                av.is_layout_dirty(),
                "a fresh avatar (rect ZERO) is layout-dirty"
            );
            // Give it a real rect (as `apply_layout` would) but never draw it.
            av.rect = Rect::new(0.0, 5000.0, 40.0, 5040.0);
            assert!(
                !av.is_layout_dirty(),
                "a laid-out avatar must be layout-clean even if it was never drawn (culled)",
            );
        }
    }
}
