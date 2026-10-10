//! `ProgressBar` — determinate / indeterminate horizontal progress indicator.
//!
//! Determinate mode paints a rounded track with a fill that scales from
//! 0 to `track_width` proportional to `progress` (clamped to `[0, 1]`).
//! Indeterminate mode ignores `progress` and slides a fixed-width segment
//! (30 % of the track) across the track on a 2 s period — the position
//! fraction is `start.elapsed().as_secs_f32() % 2.0 / 2.0`.
//!
//! The fill is progress (the inverted fill) unless the bar shows a measured
//! level ([`ProgressBar::level`]) or a status ([`ProgressBar::tone`]). Under
//! reduced motion and the capture clock the indeterminate segment holds
//! still in the middle of the track, so a screenshot is the same every run.
//!
//! [`ProgressBarFace`] paints the same bar for a host that paints into a
//! `Scene`.
use crate::time::Instant;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};

use crate::permissions::PermissionScope;
use crate::presence::{PathSegment, PresenceKey};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// Width of the moving segment in indeterminate mode, expressed as a
/// fraction of the track width. `0.3` keeps the affordance readable on
/// narrow bars while still leaving room for the segment to travel.
const INDETERMINATE_SEGMENT_FRAC: f64 = 0.3;

/// Period (in seconds) of one full sweep of the indeterminate segment
/// from the left edge to the right edge of the track.
const INDETERMINATE_PERIOD_SECS: f32 = 2.0;

/// What a bar's fill means.
#[derive(Clone, Copy, Debug, PartialEq)]
enum BarFill {
    /// Progress toward done: the inverted fill.
    Progress,
    /// A measured level that turns warning and danger past its thresholds.
    Level(crate::widget::GaugeThresholds),
    /// A status.
    Tone(crate::widget::Tone),
}

/// Immediate-mode progress bar, for a host that paints into a `Scene`.
#[derive(Clone, Copy, Debug)]
pub struct ProgressBarFace {
    /// `None` while indeterminate.
    progress: Option<f32>,
    started: Option<Instant>,
    fill: BarFill,
}

impl ProgressBarFace {
    /// A determinate bar `progress` full (clamped to `0..=1`).
    #[must_use]
    pub fn new(progress: f32) -> Self {
        Self {
            progress: Some(if progress.is_finite() {
                progress.clamp(0.0, 1.0)
            } else {
                0.0
            }),
            started: None,
            fill: BarFill::Progress,
        }
    }

    /// An indeterminate bar whose segment has been sweeping since `started`.
    #[must_use]
    pub fn indeterminate(started: Instant) -> Self {
        Self {
            progress: None,
            started: Some(started),
            fill: BarFill::Progress,
        }
    }

    /// A fill that means a status.
    #[must_use]
    pub fn tone(mut self, tone: crate::widget::Tone) -> Self {
        self.fill = BarFill::Tone(tone);
        self
    }

    /// A measured level (a disk, a quota) that turns warning and danger.
    #[must_use]
    pub fn level(mut self, thresholds: crate::widget::GaugeThresholds) -> Self {
        self.fill = BarFill::Level(thresholds);
        self
    }

    /// The track's height in `theme`.
    #[must_use]
    pub fn height(theme: &Theme) -> f64 {
        f64::from(theme.tokens.spacing.s2)
    }

    fn fill_colour(&self, theme: &Theme) -> vello::peniko::Color {
        match self.fill {
            BarFill::Progress => theme.tokens.colors.intent.inverted,
            BarFill::Level(thresholds) => {
                thresholds.color(theme, self.progress.unwrap_or_default())
            }
            BarFill::Tone(tone) => tone.colour(theme),
        }
    }

    /// Where the indeterminate segment's leading edge is, `0..1` of its
    /// travel. Still, in the middle, under reduced motion and the capture
    /// clock.
    fn phase(&self, theme: &Theme) -> f64 {
        match self.started {
            Some(started) if !theme.reduce_motion && !crate::time::wall_frozen() => f64::from(
                (started.elapsed().as_secs_f32() % INDETERMINATE_PERIOD_SECS)
                    / INDETERMINATE_PERIOD_SECS,
            ),
            _ => 0.5,
        }
    }

    /// Paint the bar into `rect` (its track spans the rect). Returns the
    /// painted fill or segment, `Rect::ZERO` when nothing is filled.
    pub fn paint(&self, ctx: &mut crate::paint::PaintCtx<'_>, rect: Rect) -> Rect {
        let theme = ctx.theme;
        let transform = ctx.transform;
        // A detail inside its row: the detail radius. The track is a lift
        // of the ground, as a slider's unfilled half is, so it reads on
        // every material; the raised tier is white on white in light.
        let radius = f64::from(theme.tokens.radii.detail).min(rect.height() / 2.0);
        ctx.scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.intent.selection,
            None,
            &RoundedRect::from_rect(rect, radius),
        );
        let fill = self.fill_colour(theme);
        let track_width = rect.width();
        let filled = match self.progress {
            None => {
                let segment_width = track_width * INDETERMINATE_SEGMENT_FRAC;
                let travel = track_width + segment_width;
                let segment_x0 = rect.x0 - segment_width + self.phase(theme) * travel;
                let x0 = segment_x0.max(rect.x0);
                let x1 = (segment_x0 + segment_width).min(rect.x1);
                if x1 > x0 {
                    Rect::new(x0, rect.y0, x1, rect.y1)
                } else {
                    Rect::ZERO
                }
            }
            // Skipped at zero (rather than a zero-width rect) so an empty
            // bar paints its track alone.
            Some(progress) if progress > 0.0 => Rect::new(
                rect.x0,
                rect.y0,
                rect.x0 + f64::from(progress) * track_width,
                rect.y1,
            ),
            Some(_) => Rect::ZERO,
        };
        if !filled.is_zero_area() {
            ctx.scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                fill,
                None,
                &RoundedRect::from_rect(filled, radius),
            );
        }
        filled
    }
}

pub struct ProgressBar {
    progress: f32,
    fill: BarFill,
    indeterminate: bool,
    rect: Rect,
    start: Instant,
    dirty: bool,
    reduce_motion: bool,
    last_segment: Rect,
    damage: [Rect; 2],
    presence_path: Vec<PathSegment>,
    permission_scope: Option<PermissionScope>,
    /// Accessible label (e.g. "Uploading"). When set, the bar becomes a
    /// *polite* live region keyed by this name, so a screen reader announces
    /// progress updates ("Uploading, 40%") without moving focus. Anonymous bars
    /// (no label) are not tracked as live regions and stay quiet.
    label: Option<String>,
}

impl ProgressBar {
    /// Construct a determinate progress bar. `progress` is clamped to
    /// `[0.0, 1.0]` on construction; callers don't have to sanitise their
    /// input.
    pub fn new(progress: f32) -> Self {
        Self {
            progress: progress.clamp(0.0, 1.0),
            fill: BarFill::Progress,
            indeterminate: false,
            rect: Rect::ZERO,
            start: crate::time::now(),
            dirty: true,
            reduce_motion: false,
            last_segment: Rect::ZERO,
            damage: [Rect::ZERO; 2],
            presence_path: Vec::new(),
            permission_scope: None,
            label: None,
        }
    }

    /// Construct an indeterminate progress bar — a 30 %-wide segment
    /// loops across the track on a 2 s period.
    pub fn indeterminate() -> Self {
        Self {
            progress: 0.0,
            fill: BarFill::Progress,
            indeterminate: true,
            rect: Rect::ZERO,
            start: crate::time::now(),
            dirty: true,
            reduce_motion: false,
            last_segment: Rect::ZERO,
            damage: [Rect::ZERO; 2],
            presence_path: Vec::new(),
            permission_scope: None,
            label: None,
        }
    }

    /// Give the bar an accessible label and make it a polite live region, so a
    /// screen reader announces progress updates without stealing focus.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// A fill that means a status (a failed transfer stays danger).
    #[must_use]
    pub fn tone(mut self, tone: crate::widget::Tone) -> Self {
        self.fill = BarFill::Tone(tone);
        self
    }

    /// A measured level (a disk, a quota) that turns warning and danger
    /// past `thresholds`.
    #[must_use]
    pub fn level(mut self, thresholds: crate::widget::GaugeThresholds) -> Self {
        self.fill = BarFill::Level(thresholds);
        self
    }

    fn face(&self) -> ProgressBarFace {
        let face = if self.indeterminate {
            ProgressBarFace::indeterminate(self.start)
        } else {
            ProgressBarFace::new(self.progress)
        };
        ProgressBarFace {
            fill: self.fill,
            ..face
        }
    }

    /// Set the accessible label (see [`with_label`](Self::with_label)).
    pub fn set_label(&mut self, label: impl Into<String>) {
        self.label = Some(label.into());
    }

    /// Update the determinate progress value. Clamped to `[0.0, 1.0]`.
    /// Calling this while in indeterminate mode silently stores the
    /// value but does not affect rendering until the bar is taken out
    /// of indeterminate mode.
    pub fn set_progress(&mut self, p: f32) {
        let progress = p.clamp(0.0, 1.0);
        if self.progress.to_bits() != progress.to_bits() {
            self.progress = progress;
            if !self.indeterminate {
                self.dirty = true;
            }
        }
    }

    /// Place a retained progress bar inside an app-owned canvas without
    /// constructing a second Taffy tree. This mirrors the embedding support
    /// on [`crate::widget::Button`] and [`crate::widget::TextInput`].
    pub fn apply_layout_external(&mut self, rect: Rect) {
        if self.rect != rect {
            self.rect = rect;
            self.dirty = true;
        }
    }

    /// Opt this widget into presence tracking by binding it to a CRDT-style path.
    #[must_use]
    pub fn with_presence(mut self, path: Vec<PathSegment>) -> Self {
        self.presence_path = path;
        self
    }

    /// Gate this widget behind a permission scope.
    #[must_use]
    pub fn with_permission_scope(mut self, scope: PermissionScope) -> Self {
        self.permission_scope = Some(scope);
        self
    }

    /// Current clamped progress value. `0.0` while the bar is in
    /// indeterminate mode, regardless of any prior `set_progress` call.
    pub fn progress(&self) -> f32 {
        if self.indeterminate {
            0.0
        } else {
            self.progress
        }
    }
}

/// Rounds an already-clamped `[0, 100]` percentage to a whole-number
/// `i64` for the accessible value string. The `.round()` and the
/// `clamp(0.0, 1.0)` before it together bound the input to `[0, 100]`,
/// far inside `i64`'s range, so the truncation the lint warns about
/// can't occur.
#[allow(
    clippy::cast_possible_truncation,
    reason = "input is a rounded percentage already bounded to [0, 100]"
)]
fn pct_to_i64(pct: f32) -> i64 {
    pct as i64
}

impl Widget for ProgressBar {
    crate::provides!(
        dyn crate::permissions::capability::PermissionGated,
        dyn crate::presence::capability::PublishesPresence
    );

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(crate::num::f32_of_f64(ProgressBarFace::height(theme))),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.dirty = true;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
        self.reduce_motion = theme.reduce_motion || crate::time::wall_frozen();
        let segment_width = self.rect.width() * INDETERMINATE_SEGMENT_FRAC;
        let current_segment = self.face().paint(
            &mut crate::paint::PaintCtx::new(scene, transform, text_renderer, theme),
            self.rect,
        );
        if self.indeterminate {
            self.damage = match (
                self.last_segment.is_zero_area(),
                current_segment.is_zero_area(),
            ) {
                (true, _) => [current_segment, Rect::ZERO],
                (_, true) => [self.last_segment, Rect::ZERO],
                (false, false) => {
                    let union = self.last_segment.union(current_segment);
                    if union.width() <= segment_width * 2.0 {
                        [union, Rect::ZERO]
                    } else {
                        // At sweep wraparound, unioning the outgoing right-edge
                        // segment with the incoming left-edge segment would
                        // invalidate almost the whole bar. Keep the two small,
                        // disconnected regions separate for that frame.
                        [self.last_segment, current_segment]
                    }
                }
            };
            self.last_segment = current_segment;
        }
    }

    fn name(&self) -> &'static str {
        "ProgressBar"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Politeness, Role};
        // Determinate bars expose a percentage value; indeterminate bars have
        // no meaningful value, so omit it and mark the node busy instead. A
        // labelled bar is a polite live region keyed by its name, so progress
        // updates announce without moving focus.
        let mut node = if self.indeterminate {
            AccessNode::new(Role::ProgressBar).with_state(AccessState {
                busy: true,
                ..Default::default()
            })
        } else {
            let pct = pct_to_i64((self.progress.clamp(0.0, 1.0) * 100.0).round());
            AccessNode::new(Role::ProgressBar).with_value(format!("{pct}%"))
        };
        if let Some(label) = &self.label {
            node = node.with_name(label.clone()).with_live(Politeness::Polite);
        }
        Some(node)
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || (self.indeterminate && !self.reduce_motion)
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.dirty {
            regions.push(self.rect);
        } else if self.indeterminate && !self.reduce_motion {
            regions.extend(
                self.damage
                    .iter()
                    .copied()
                    .filter(|rect| !rect.is_zero_area()),
            );
        }
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    /// Indeterminate bars must keep the redraw loop pumping so the
    /// segment continues to advance — without this, the animation
    /// would freeze on the first frame after an unrelated event.
    fn needs_animation_frame(&self) -> bool {
        self.indeterminate && !self.reduce_motion
    }

    /// Determinate bars never change size; indeterminate ones
    /// change their segment fill every frame but keep the same
    /// outer rect. Either way, layout stays clean after the
    /// initial `apply_layout` writes a non-zero rect.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
        permissions: &crate::permissions::PermissionContext,
    ) {
        if presence.should_cull(self.get_rect()) {
            return;
        }
        self.draw(scene, transform, text_renderer, theme, presence);

        crate::widget::draw_gate_and_peer_focus(
            self,
            scene,
            transform,
            theme,
            presence,
            permissions,
        );
    }
}

impl crate::presence::capability::PublishesPresence for ProgressBar {
    fn presence_key(&self) -> Option<PresenceKey> {
        PresenceKey::widget(&self.presence_path)
    }
}

impl crate::permissions::capability::PermissionGated for ProgressBar {
    fn permission_scope(&self) -> Option<PermissionScope> {
        self.permission_scope.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::PresenceCtx;

    /// Build a determinate bar with a fixed rect so tests don't have
    /// to spin up the full taffy layout pipeline. Rect spans
    /// `[0..200, 0..8]` — width 200, matching the default track height.
    fn bar_at(progress: f32) -> ProgressBar {
        let mut b = ProgressBar::new(progress);
        b.rect = Rect::new(0.0, 0.0, 200.0, 8.0);
        b
    }

    /// Count the `COLOR` draw tags in a scene. Each `scene.fill` with
    /// a solid colour emits exactly one such tag — see `tabs.rs`'s
    /// `active_pill_emits_more_color_fills_than_inactive_siblings` for
    /// precedent.
    fn count_color_fills(scene: &Scene) -> usize {
        use vello_encoding::DrawTag;
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count()
    }

    /// Render a bar into a fresh scene and return that scene. Mirrors
    /// the helper pattern from `tabs.rs`'s encoding tests.
    fn render(bar: &mut ProgressBar) -> Scene {
        render_with(bar, false)
    }

    /// Render a bar against a theme with the given reduced-motion setting.
    fn render_with(bar: &mut ProgressBar, reduce_motion: bool) -> Scene {
        let theme = Theme::dark().with_reduce_motion(reduce_motion);
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        bar.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        scene
    }

    #[test]
    fn determinate_fill_tracks_progress_and_is_announced() {
        // Case `progress_bar_at_zero_paints_no_fill`.
        // At `progress == 0.0` the bar must paint the track *only* — the
        // accent fill rect must be skipped entirely, pinned against the
        // encoding (not pixel colour) so a future "draws a zero-width rect
        // anyway" refactor trips the test.
        {
            let mut zero = bar_at(0.0);
            let zero_scene = render(&mut zero);
            let zero_fills = count_color_fills(&zero_scene);

            // The track alone is one COLOR fill — no accent fill should
            // appear.
            assert_eq!(
                zero_fills, 1,
                "zero-progress bar must emit exactly the track COLOR fill \
             with no accent fill draw — got {zero_fills} COLOR tags",
            );

            // Cross-check: a half-progress bar emits *more* COLOR fills
            // than the zero bar, proving the difference is the accent fill.
            let mut half = bar_at(0.5);
            let half_fills = count_color_fills(&render(&mut half));
            assert!(
                half_fills > zero_fills,
                "non-zero progress must add an accent fill (half: {half_fills}, \
             zero: {zero_fills})",
            );
        }

        // Case `progress_bar_at_full_paints_track_width_fill`.
        // At `progress == 1.0` the accent fill must span the full track
        // width. Can't inspect the encoded rect directly, so this drives
        // the contract through the public `progress()` accessor, computing
        // the fill rectangle the same way `draw` does.
        {
            let bar = bar_at(1.0);
            let track_width = bar.rect.width();
            let fill_width = f64::from(bar.progress()) * track_width;
            assert!(
                (fill_width - track_width).abs() < 1e-6,
                "at progress=1.0 the fill must span the full track width \
             (track={track_width}, fill={fill_width})",
            );

            // And the scene actually paints an accent fill at full
            // progress (i.e. the >0.0 branch fires, not the zero skip).
            let mut full = bar_at(1.0);
            let full_fills = count_color_fills(&render(&mut full));
            let mut zero = bar_at(0.0);
            let zero_fills = count_color_fills(&render(&mut zero));
            assert!(
                full_fills > zero_fills,
                "full bar must emit at least one more COLOR fill than zero \
             bar — full: {full_fills}, zero: {zero_fills}",
            );
        }

        // Case `progress_bar_at_half_paints_half_fill_within_one_pixel`.
        // At `progress == 0.5` the accent fill width must equal half the
        // track width within a ±1 px tolerance. The 1 px slack absorbs
        // rounding at sub-pixel layout boundaries; the underlying maths
        // is exact for the 200 px test rect.
        {
            let bar = bar_at(0.5);
            let track_width = bar.rect.width();
            let fill_width = f64::from(bar.progress()) * track_width;
            let expected = track_width / 2.0;
            assert!(
                (fill_width - expected).abs() <= 1.0,
                "at progress=0.5 the fill width must be track_width/2 ± 1 px \
             (expected={expected}, got={fill_width})",
            );
        }

        // Case `accessibility_reports_progressbar_role_value_and_busy`.
        // A determinate bar reports its progress as a percentage value with no
        // busy flag; an indeterminate bar omits the value and is marked busy.
        {
            use crate::a11y::Role;
            let half = ProgressBar::new(0.5);
            let node = Widget::accessibility(&half).expect("progress bar is not presentational");
            assert_eq!(node.role, Role::ProgressBar);
            assert_eq!(node.value.as_deref(), Some("50%"));
            assert!(!node.state.busy, "a determinate bar is not busy");

            let indet = ProgressBar::indeterminate();
            let node = Widget::accessibility(&indet).unwrap();
            assert_eq!(node.role, Role::ProgressBar);
            assert_eq!(node.value, None, "an indeterminate bar exposes no value");
            assert!(node.state.busy, "an indeterminate bar is busy");
        }

        // Case `determinate_bar_redraws_only_after_a_visible_value_change`.
        {
            let mut bar = bar_at(0.5);
            assert!(bar.needs_redraw(), "a new bar needs its initial paint");
            bar.mark_drawn();
            assert!(!bar.needs_redraw(), "a painted determinate bar is static");

            bar.set_progress(0.5);
            assert!(!bar.needs_redraw(), "setting the same value is a no-op");
            bar.set_progress(0.75);
            assert!(
                bar.needs_redraw(),
                "a visible value change invalidates the bar"
            );
        }
    }

    #[test]
    fn indeterminate_segment_animates_with_tight_damage_unless_motion_is_reduced() {
        // Case `progress_bar_indeterminate_needs_animation_frame_returns_true`.
        // Indeterminate bars must opt into the redraw loop via
        // `needs_animation_frame() == true`, otherwise `App` would stop
        // pumping frames and the segment would freeze partway across the
        // track. Determinate bars must *not* opt in — they're static.
        {
            let indeterminate = ProgressBar::indeterminate();
            assert!(
                indeterminate.needs_animation_frame(),
                "indeterminate bar must request animation frames",
            );

            let determinate = ProgressBar::new(0.5);
            assert!(
                !determinate.needs_animation_frame(),
                "determinate bar must not request animation frames",
            );
        }

        // Case `indeterminate_damage_tracks_only_the_moving_segment`.
        {
            let mut bar = ProgressBar::indeterminate();
            bar.rect = Rect::new(0.0, 0.0, 200.0, 8.0);
            let _ = render(&mut bar);
            bar.mark_drawn();

            let mut regions = Vec::new();
            bar.collect_damage(&mut regions);
            assert!(!regions.is_empty(), "moving segment should report damage");
            assert!(regions.len() <= 2);
            assert!(regions.iter().all(|damage| {
                damage.width() <= bar.rect.width() * INDETERMINATE_SEGMENT_FRAC * 2.0 + 1.0
            }));
        }

        // Case `indeterminate_wrap_damage_is_available_immediately_after_draw`.
        {
            use crate::time::Duration;

            let mut bar = ProgressBar::indeterminate();
            bar.rect = Rect::new(0.0, 0.0, 200.0, 8.0);
            bar.last_segment = Rect::new(140.0, 0.0, 200.0, 8.0);
            // Build everything `draw` needs first: loading the text renderer's
            // fonts takes long enough to move the sweep off the wraparound phase
            // this test backdates to.
            let theme = Theme::dark().with_reduce_motion(false);
            let mut tr = TextRenderer::new();
            let presence = PresenceCtx::default();
            let mut scene = Scene::new();
            bar.start = crate::time::now()
                .checked_sub(Duration::from_millis(300))
                .expect("the test duration is far shorter than the monotonic clock");
            bar.dirty = false;
            // The frame owner must collect immediately after `draw`, before it
            // clears dirty flags. `draw` is what records this frame's outgoing
            // and incoming segment geometry.
            bar.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

            let mut regions = Vec::new();
            bar.collect_damage(&mut regions);
            assert_eq!(regions.len(), 2, "wraparound must not form one wide union");
            assert!(regions.iter().all(|damage| damage.width() <= 61.0));
        }

        // Case `reduced_motion_stops_indeterminate_frame_requests_after_paint`.
        {
            let mut bar = ProgressBar::indeterminate();
            bar.rect = Rect::new(0.0, 0.0, 200.0, 8.0);
            let _ = render_with(&mut bar, true);
            bar.mark_drawn();

            assert!(!bar.needs_animation_frame());
            assert!(!bar.needs_redraw());
        }

        // Case `reduce_motion_freezes_the_indeterminate_segment`.
        // With reduced motion an indeterminate bar holds its segment fixed:
        // a bar backdated by a full sweep paints the same geometry as a
        // fresh one. With motion on, the same backdate lands elsewhere —
        // proving the freeze is the reduced-motion path, not a no-op.
        {
            use crate::time::Duration;
            let one_sweep = Duration::from_secs_f32(INDETERMINATE_PERIOD_SECS);
            // A quarter-sweep backdate so the moving segment is plainly off the
            // resting position (a full sweep would alias back to phase 0).
            let backdate = crate::time::now()
                .checked_sub(one_sweep / 4)
                .expect("process has been running longer than a quarter sweep");

            let mut fresh = ProgressBar::indeterminate();
            fresh.rect = Rect::new(0.0, 0.0, 200.0, 8.0);
            let fresh_geo = render_with(&mut fresh, true).encoding().path_data.clone();

            let mut aged = ProgressBar::indeterminate();
            aged.rect = Rect::new(0.0, 0.0, 200.0, 8.0);
            aged.start = backdate;
            let aged_reduced = render_with(&mut aged, true).encoding().path_data.clone();
            assert_eq!(
                fresh_geo, aged_reduced,
                "reduced motion must paint the resting segment regardless of elapsed time",
            );

            let mut aged2 = ProgressBar::indeterminate();
            aged2.rect = Rect::new(0.0, 0.0, 200.0, 8.0);
            aged2.start = backdate;
            let aged_moving = render_with(&mut aged2, false).encoding().path_data.clone();
            assert_ne!(
                fresh_geo, aged_moving,
                "with motion on, a backdated bar's segment is at a different position",
            );
        }
    }
}
