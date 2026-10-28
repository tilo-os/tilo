//! A workspace switch made by pointer (the workspace menu, a banner) moves:
//! the workspace shown before leaves along the shared axis while the one
//! chosen arrives from the side it lies on ([`Drill::WORKSPACE`]). Only the
//! windows move; the bar and the backdrop stay. The content is swapped and
//! takes input from the first frame; the slide is presentation only. A
//! shortcut's switch lands, so it starts no slide.

use ui::motion::{Direction, Drill, MotionValue, Pose};
use ui::time::Instant;
use ui::tokens::{EffectiveMotion, MotionSource};

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

/// A running workspace slide.
#[derive(Debug)]
pub(super) struct WorkspaceSlide {
    /// The workspace shown before, whose kept content leaves.
    from: u64,
    /// `1` toward a later workspace in the menu's order, `-1` toward an
    /// earlier one.
    toward: f64,
    /// `0` when the switch was made, `1` once the chosen workspace is in place.
    progress: MotionValue<f64>,
}

impl WorkspaceSlide {
    /// The slide from `from` toward a later workspace (`later`) or an
    /// earlier one, as `source` moves it; `None` when that lands at once.
    pub(super) fn start(
        from: u64,
        later: bool,
        motion: EffectiveMotion,
        source: MotionSource,
        now: Instant,
    ) -> Option<Self> {
        let timing = Drill::WORKSPACE.timing(Direction::of(later), motion, source);
        if timing.is_instant() {
            return None;
        }
        let mut progress = MotionValue::new(0.0);
        progress.set(1.0, timing, now);
        Some(Self {
            from,
            toward: if later { 1.0 } else { -1.0 },
            progress,
        })
    }

    pub(super) fn from(&self) -> u64 {
        self.from
    }

    pub(super) fn is_moving(&self, now: Instant) -> bool {
        self.progress.is_moving(now)
    }

    /// Hold the slide for `by`: time spent waiting on a full presenter is
    /// not progress, so the windows resume where they stood.
    pub(super) fn defer(&mut self, by: ui::time::Duration) {
        self.progress.defer(by);
    }

    /// The poses of the leaving and the arriving workspace in a view
    /// `width` wide.
    pub(super) fn poses(&self, width: f64, motion: EffectiveMotion, now: Instant) -> (Pose, Pose) {
        let p = self.progress.get(now);
        (
            Drill::pose(-self.toward * p, width, motion),
            Drill::pose(self.toward * (1.0 - p), width, motion),
        )
    }
}

impl ShellOverlayHost {
    /// The content shown for `shown_before` was just swapped for the active
    /// workspace's: slide between them when `source` moves a switch.
    pub(super) fn slide_workspace(
        &mut self,
        shown_before: Option<u64>,
        desktop: &crate::shell::state::DesktopTree,
        source: MotionSource,
    ) {
        let to = desktop.active.raw();
        let order = |id: u64| desktop.workspaces.iter().position(|w| w.id.raw() == id);
        let slide = shown_before
            .filter(|&from| from != to)
            .and_then(|from| Some((from, order(from)?, order(to)?)))
            .and_then(|(from, before, after)| {
                WorkspaceSlide::start(
                    from,
                    after > before,
                    self.overview_layout_theme.motion(),
                    source,
                    ui::motion::frame_now(),
                )
            });
        self.workspace_slide = slide;
    }

    /// Whether a workspace slide still moves.
    pub(super) fn workspace_slide_moving(&self, now: Instant) -> bool {
        self.workspace_slide
            .as_ref()
            .is_some_and(|slide| slide.is_moving(now))
    }

    /// The running slide's leaving workspace and the poses of the leaving and
    /// arriving windows; drops a slide that has come to rest.
    pub(super) fn workspace_slide_poses(&mut self, theme: &Theme) -> Option<(u64, Pose, Pose)> {
        let now = ui::motion::frame_now();
        if !self.workspace_slide_moving(now) {
            self.workspace_slide = None;
            return None;
        }
        let slide = self.workspace_slide.as_ref()?;
        let (leaving, arriving) = slide.poses(self.rect.width(), theme.motion(), now);
        Some((slide.from(), leaving, arriving))
    }

    /// Draw the windows of the workspace kept for `from` leaving at `pose`.
    pub(super) fn draw_leaving_workspace(
        &mut self,
        from: u64,
        pose: Pose,
        scene: &mut Scene,
        transform: Affine,
        (text_renderer, theme, presence): (&mut TextRenderer, &Theme, &PresenceCtx),
    ) {
        if !pose.is_visible() {
            return;
        }
        let clip = self.rect;
        let Some(kept) = self
            .parked_workspaces
            .content_mut(from)
            .and_then(|content| content.as_any_mut())
            .and_then(|any| any.downcast_mut::<tilo_desktop::composition::DesktopSurface>())
        else {
            return;
        };
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            pose.opacity,
            transform,
            &clip,
        );
        let _ = kept.draw_workspace_area(
            scene,
            transform.pre_translate(pose.transform.translation()),
            text_renderer,
            theme,
            presence,
        );
        scene.pop_layer();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::time::Duration;

    fn on() -> EffectiveMotion {
        EffectiveMotion::default()
    }

    /// A click in the workspace menu moves the windows: the shown workspace
    /// leaves toward the side it lies on and the chosen one arrives from the
    /// other, both at rest when the slide ends. A key lands.
    #[test]
    fn a_pointer_switch_slides_the_windows_and_a_key_lands() {
        let now = ui::time::now();
        assert!(WorkspaceSlide::start(7, true, on(), MotionSource::Keyboard, now).is_none());
        assert!(WorkspaceSlide::start(7, true, on(), MotionSource::Action, now).is_none());

        let slide =
            WorkspaceSlide::start(7, true, on(), MotionSource::Pointer, now).expect("it moves");
        assert_eq!(slide.from(), 7);
        let (leaving, arriving) = slide.poses(1600.0, on(), now);
        assert!(leaving.is_rest(), "the switch starts from what was shown");
        assert!(!arriving.is_visible(), "and the chosen workspace not yet");
        assert!(slide.is_moving(now + Duration::from_millis(100)));
        let (leaving, arriving) = slide.poses(1600.0, on(), now + Duration::from_millis(100));
        let shift = |pose: Pose| pose.transform.translation().x;
        assert!(
            shift(leaving) < 0.0,
            "toward a later workspace, what was shown leaves left"
        );
        assert!(
            shift(arriving) > 0.0,
            "and the chosen one arrives from the right"
        );
        assert!(
            shift(arriving) <= 28.0,
            "a shared axis, not a full-width push"
        );

        let back =
            WorkspaceSlide::start(7, false, on(), MotionSource::Pointer, now).expect("it moves");
        let (leaving, arriving) = back.poses(1600.0, on(), now + Duration::from_millis(50));
        assert!(
            shift(leaving) > 0.0 && shift(arriving) < 0.0,
            "back runs the other way"
        );

        let end = now + Duration::from_secs(1);
        assert!(!slide.is_moving(end), "a slide comes to rest");
        let (leaving, arriving) = slide.poses(1600.0, on(), end);
        assert!(!leaving.is_visible() && arriving.is_rest());
    }
}
