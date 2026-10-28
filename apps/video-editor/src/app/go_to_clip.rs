//! Go to clip: from a playhead parked between clips to the nearest clip
//! the preview can show, as a published action and as the button the
//! preview's hint carries.

#[allow(
    clippy::wildcard_imports,
    reason = "this file is one part of the `VideoEditorApp` impl split out of `app.rs`; it shares the parent's whole scope"
)]
use super::*;

/// The button's words, and its node's key in the transport's semantics.
pub(super) const GO_TO_CLIP_LABEL: &str = "Go to clip";
pub(super) const GO_TO_CLIP_KEY: &str = "go to clip";

impl VideoEditorApp {
    /// Where Go to clip takes the playhead: the start of the first picture
    /// clip after it, or of the last one before it when none follows.
    pub(super) fn go_to_clip_target(&self) -> Option<f64> {
        let starts = || {
            self.project
                .clips
                .iter()
                .filter(|clip| {
                    self.project
                        .track(clip.track)
                        .is_some_and(|track| track.kind == TrackKind::Video)
                })
                .map(|clip| clip.start)
        };
        starts()
            .filter(|start| *start > self.playhead)
            .min_by(f64::total_cmp)
            .or_else(|| {
                starts()
                    .filter(|start| *start < self.playhead)
                    .max_by(f64::total_cmp)
            })
    }

    /// Move the playhead onto the nearest clip; whether there was one.
    pub(super) fn go_to_clip(&mut self) -> bool {
        let Some(at) = self.go_to_clip_target() else {
            return false;
        };
        self.seek_to(at);
        true
    }

    /// Whether the preview shows its "no clip at the playhead" hint now,
    /// with a clip to go to: then the hint carries Go to clip.
    pub(super) fn preview_offers_go_to_clip(&self) -> bool {
        !self.project.clips.is_empty()
            && self.project.top_video_clip_at(self.playhead).is_none()
            && self.go_to_clip_target().is_some()
    }

    /// The preview's words over an empty canvas, stacked about its centre:
    /// where the title starts and, when `offer`, where Go to clip stands
    /// under the hint.
    pub(super) fn preview_hint_stack(
        canvas: Rect,
        tr: &mut TextRenderer,
        theme: &Theme,
        hint: bool,
        offer: bool,
    ) -> (f64, Option<Rect>) {
        let th = line_height(tr, &theme.text(TextRole::Body).strong());
        let hh = if hint {
            line_height(tr, &theme.text(TextRole::Caption))
        } else {
            -S1
        };
        let (bw, bh) = if offer {
            (
                ButtonFace::new(Rect::ZERO, GO_TO_CLIP_LABEL)
                    .control_size(ControlSize::Small)
                    .intrinsic_width(tr, theme),
                ButtonFace::height(ControlSize::Small, theme),
            )
        } else {
            (0.0, 0.0)
        };
        let below = if offer { S2 + bh } else { 0.0 };
        let top = canvas.center().y - (th + S1 + hh + below) / 2.0;
        let button = offer.then(|| {
            let y = top + th + S1 + hh + S2;
            let x = canvas.center().x - bw / 2.0;
            Rect::new(x, y, x + bw, y + bh)
        });
        (top, button)
    }

    /// Where Go to clip stands in the preview now, if it is offered.
    pub(super) fn go_to_clip_button(&self, r: &Regions) -> Option<Rect> {
        if !self.preview_offers_go_to_clip()
            || r.canvas.width() < MIN_CANVAS
            || r.canvas.height() < MIN_CANVAS
        {
            return None;
        }
        self.with_dialog_layout(|_, tr, theme| {
            Self::preview_hint_stack(r.canvas, tr, theme, true, true).1
        })
    }
}
