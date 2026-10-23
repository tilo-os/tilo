//! A window's loading line: the system's, under every window's bar.
//!
//! The desktop owns window chrome, so how a window says "this is loading" is
//! the desktop's too: a thin line (`metrics.loading_line_px`, 2 px at 1×)
//! along the top edge of the window's content, directly under its bar,
//! spanning the content's width and clipped to its shape (its rounded
//! corners; edge to edge in full screen, where it stands at the top edge).
//!
//! - **Progress** (`0..=1` reported): an accent bar grows from the left. It
//!   follows the reports on the standard motion from where it is drawn, so it
//!   never jumps and never goes backwards; between reports it creeps slowly
//!   toward the next step and never reaches it ([`crate::motion::Loading`]).
//! - **Busy** (no progress known): a soft accent highlight with a gradient
//!   head and tail sweeps left to right across a faint track.
//! - **Done:** the bar completes quickly, then fades out. The line takes no
//!   layout space at any time; hidden, it draws nothing and asks for no
//!   frames.
//! - A load shorter than [`crate::motion::Loading::DELAY`] never shows.
//! - Reduced motion: progress still grows, without the creep; busy is a
//!   gently pulsing track instead of the sweep.
//!
//! An app says what its window is doing with one call: [`busy`],
//! [`progress`] or [`done`] (or [`set`] with a [`WindowLoading`]). The app
//! runner carries it over the app's `shell-ui` connection, and the shell
//! draws the line for the tab that shows the app ([`LoadingLine`], drawn by
//! the window's tile as a damage part of its own, so animating it repaints
//! the line's band and nothing of the window's content). An equal report is
//! free, so an app may say it on every frame.

use std::cell::Cell;

use serde::{Deserialize, Serialize};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use crate::motion::{Loading, MotionValue};
use crate::theme::Theme;
use crate::time::{Duration, Instant};
use crate::tokens::EffectiveMotion;

/// What a window is doing, as its app reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WindowLoading {
    /// Nothing is loading.
    #[default]
    Idle,
    /// Busy, with no progress known.
    Busy,
    /// Loading, `fraction` (`0..=1`) of the way.
    Progress { fraction: f32 },
}

impl WindowLoading {
    /// Loading `fraction` of the way, clamped to `0..=1`; busy when the
    /// fraction is not a number.
    #[must_use]
    pub fn progress(fraction: f32) -> Self {
        Self::Progress { fraction }.bounded()
    }

    /// The same report, with a fraction the line can draw: clamped to
    /// `0..=1`, and busy in place of one that is not a number. Whatever an
    /// app sends is bounded before it is drawn.
    #[must_use]
    pub fn bounded(self) -> Self {
        match self {
            Self::Progress { fraction } if fraction.is_finite() => Self::Progress {
                fraction: fraction.clamp(0.0, 1.0),
            },
            Self::Progress { .. } => Self::Busy,
            other => other,
        }
    }

    /// Whether something is loading.
    #[must_use]
    pub fn is_loading(self) -> bool {
        !matches!(self, Self::Idle)
    }
}

thread_local! {
    static CURRENT: Cell<WindowLoading> = const { Cell::new(WindowLoading::Idle) };
    static REVISION: Cell<u64> = const { Cell::new(0) };
}

/// Say what the window is doing. Cheap when nothing changed.
pub fn set(loading: WindowLoading) {
    let loading = loading.bounded();
    CURRENT.with(|current| {
        if current.replace(loading) != loading {
            REVISION.with(|revision| revision.set(revision.get().wrapping_add(1)));
        }
    });
}

/// The window is busy, with no progress known (a long load just started).
pub fn busy() {
    set(WindowLoading::Busy);
}

/// The window's load is `fraction` (`0..=1`) of the way.
pub fn progress(fraction: f32) {
    set(WindowLoading::progress(fraction));
}

/// The window's load is done: its line completes and fades out.
pub fn done() {
    set(WindowLoading::Idle);
}

/// What the window last said it is doing, on this thread.
#[must_use]
pub fn current() -> WindowLoading {
    CURRENT.with(Cell::get)
}

/// Bumped by every [`set`] that changed something, so a runner sends a
/// report once.
#[must_use]
pub fn revision() -> u64 {
    REVISION.with(Cell::get)
}

/// A load that ended while its line was shown: the bar completes, then the
/// line fades out.
#[derive(Clone, Copy, Debug)]
struct Finishing {
    /// When the completed bar starts to fade.
    fade_from: Instant,
    /// The load was busy, with no progress known: its track fades with it.
    busy: bool,
}

/// One window's loading line: what its app reported, animated as
/// [`crate::motion::Loading`] says, and drawn along the top of the window's
/// content.
#[derive(Clone, Debug)]
pub struct LoadingLine {
    reported: WindowLoading,
    /// When the line shows (or showed) for the current load: the load's start
    /// and [`Loading::DELAY`]. `None` while idle.
    shown_at: Option<Instant>,
    /// The bar's progress, following the reports.
    fill: MotionValue<f64>,
    /// The last progress reported for this load (raised to where the bar
    /// already stood), which the bar creeps on from once it is there.
    target: Option<f64>,
    /// When the bar arrives at [`Self::target`].
    arrived_at: Instant,
    /// The bar as last drawn: it never goes back below it during a load.
    drawn: f64,
    finishing: Option<Finishing>,
    /// The motion the person allows, latched from the theme at draw.
    motion: EffectiveMotion,
    /// The line's length in device pixels, as last drawn: how far one pixel
    /// of progress is, so a creep asks for a frame per pixel, not per
    /// refresh.
    width_px: f64,
    /// When the reduced-motion pulse was last drawn.
    pulsed_at: Option<Instant>,
}

impl Default for LoadingLine {
    fn default() -> Self {
        Self {
            reported: WindowLoading::Idle,
            shown_at: None,
            fill: MotionValue::new(0.0),
            target: None,
            arrived_at: crate::motion::frame_now(),
            drawn: 0.0,
            finishing: None,
            motion: EffectiveMotion::default(),
            width_px: 0.0,
            pulsed_at: None,
        }
    }
}

/// What the line draws at one instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LoadingVisual {
    /// A bar `fill` (`0..=1`) of the way across, at `opacity`; over the
    /// faint track while a busy load finishes.
    Bar {
        fill: f64,
        opacity: f32,
        track: bool,
    },
    /// Busy: the faint track and the highlight centred `sweep` (`0..=1`) of
    /// its travel across, at `opacity`.
    Sweep { sweep: f64, opacity: f32 },
    /// Busy under reduced motion: the track alone, at `opacity` (its pulse
    /// included).
    Pulse { opacity: f32 },
}

impl LoadingLine {
    /// What the app reported last.
    #[must_use]
    pub fn reported(&self) -> WindowLoading {
        self.reported
    }

    /// The app reported `loading` at `now`. Returns whether that changed
    /// anything.
    pub fn set(&mut self, loading: WindowLoading, now: Instant) -> bool {
        let loading = loading.bounded();
        if loading == self.reported {
            return false;
        }
        let was = self.reported;
        self.reported = loading;
        match (was.is_loading(), loading.is_loading()) {
            (false, true) => self.begin(now),
            (true, false) => self.finish(now),
            _ => {}
        }
        if let WindowLoading::Progress { fraction } = loading {
            self.report(f64::from(fraction), now);
        }
        true
    }

    /// A load began at `now`.
    fn begin(&mut self, now: Instant) {
        // A load that starts while the last one's line still fades out
        // takes the line over, whole, rather than hiding and showing it.
        let still_shown = self.finishing.is_some() && self.opacity(now).is_some();
        self.shown_at = Some(if still_shown {
            now.checked_sub(Loading::appear_time(self.motion))
                .unwrap_or(now)
        } else {
            now + Loading::DELAY
        });
        self.finishing = None;
        self.fill.snap(0.0);
        self.target = None;
        self.arrived_at = now;
        self.drawn = 0.0;
        self.pulsed_at = None;
    }

    /// The load ended at `now`: a line never shown stays hidden; a shown one
    /// completes and fades out.
    fn finish(&mut self, now: Instant) {
        if self.shown_at.is_none_or(|at| now < at) {
            self.reset();
            return;
        }
        let busy = self.target.is_none();
        let from = self.bar(now);
        self.fill.snap(from);
        self.fill.set(1.0, Loading::finish_timing(self.motion), now);
        self.finishing = Some(Finishing {
            fade_from: self.fill.settles_at().unwrap_or(now),
            busy,
        });
    }

    /// Progress `fraction` was reported at `now`: the bar goes there from
    /// where it is drawn, and never back.
    fn report(&mut self, fraction: f64, now: Instant) {
        let from = self.bar(now);
        let to = fraction.clamp(0.0, 1.0).max(from);
        self.fill.snap(from);
        self.fill
            .set(to, Loading::progress_timing(self.motion), now);
        self.target = Some(to);
        self.arrived_at = self.fill.settles_at().unwrap_or(now);
    }

    fn reset(&mut self) {
        self.reported = WindowLoading::Idle;
        self.shown_at = None;
        self.finishing = None;
        self.fill.snap(0.0);
        self.target = None;
        self.drawn = 0.0;
        self.pulsed_at = None;
    }

    /// The bar's progress at `now`: following the reports, creeping on once
    /// it has arrived, completing once the load ended, and never below
    /// where it was drawn.
    fn bar(&self, now: Instant) -> f64 {
        if self.finishing.is_some() {
            return self.fill.get(now).clamp(self.drawn, 1.0);
        }
        let Some(target) = self.target else {
            return self.drawn;
        };
        let following = self.fill.get(now).min(target);
        let crept = if now < self.arrived_at {
            following
        } else {
            following.max(Loading::creep(
                target,
                now.saturating_duration_since(self.arrived_at),
                self.motion,
            ))
        };
        crept.max(self.drawn)
    }

    /// The line's opacity at `now`; `None` while it shows nothing.
    fn opacity(&self, now: Instant) -> Option<f32> {
        let shown_at = self.shown_at.filter(|at| now >= *at)?;
        let appear = Loading::appear(now.saturating_duration_since(shown_at), self.motion);
        let fade = match self.finishing {
            Some(finishing) if now >= finishing.fade_from => {
                let since = now.saturating_duration_since(finishing.fade_from);
                if since >= Loading::fade_time(self.motion) {
                    return None;
                }
                Loading::fade(since, self.motion)
            }
            _ => 1.0,
        };
        Some(appear * fade)
    }

    /// What the line shows at `now`; `None` while it shows nothing.
    #[must_use]
    pub fn visual(&self, now: Instant) -> Option<LoadingVisual> {
        let opacity = self.opacity(now)?;
        if let Some(finishing) = self.finishing {
            return Some(LoadingVisual::Bar {
                fill: self.bar(now),
                opacity,
                track: finishing.busy,
            });
        }
        match self.reported {
            WindowLoading::Idle => None,
            WindowLoading::Progress { .. } => Some(LoadingVisual::Bar {
                fill: self.bar(now),
                opacity,
                track: false,
            }),
            WindowLoading::Busy if self.motion.loops() => {
                let since = self
                    .shown_at
                    .map_or(Duration::ZERO, |at| now.saturating_duration_since(at));
                Some(LoadingVisual::Sweep {
                    sweep: Loading::sweep(since),
                    opacity,
                })
            }
            WindowLoading::Busy => {
                let since = self
                    .shown_at
                    .map_or(Duration::ZERO, |at| now.saturating_duration_since(at));
                Some(LoadingVisual::Pulse {
                    opacity: opacity * Loading::pulse(since),
                })
            }
        }
    }

    /// Whether the line changes from frame to frame at `now`, so its window
    /// needs frames at the display's rate. A line waiting out its delay, a
    /// creeping bar and the reduced-motion pulse ask for a wake instead
    /// ([`Self::next_wake`]), and a frame once it is due.
    #[must_use]
    pub fn needs_frame(&self, now: Instant) -> bool {
        if self.next_wake(now).is_some_and(|due| due <= now) {
            return true;
        }
        let Some(shown_at) = self.shown_at.filter(|at| now >= *at) else {
            return false;
        };
        if self.finishing.is_some() {
            // Until the fade is over; the draw after it hides the line.
            return self.opacity(now).is_some();
        }
        if now.saturating_duration_since(shown_at) < Loading::appear_time(self.motion) {
            return true;
        }
        match self.reported {
            WindowLoading::Idle => false,
            WindowLoading::Busy => self.motion.loops(),
            WindowLoading::Progress { .. } => self.fill.is_moving(now),
        }
    }

    /// When the line next changes without a report: the end of its delay,
    /// the creeping bar's next device pixel, the next pulse.
    #[must_use]
    pub fn next_wake(&self, now: Instant) -> Option<Instant> {
        let shown_at = self.shown_at?;
        if now < shown_at {
            return Some(shown_at);
        }
        if self.finishing.is_some() {
            return None;
        }
        match self.reported {
            WindowLoading::Idle => None,
            WindowLoading::Busy if self.motion.loops() => None,
            WindowLoading::Busy => Some(Loading::next_pulse_frame(self.pulsed_at.unwrap_or(now))),
            WindowLoading::Progress { .. } => {
                let target = self.target?;
                if self.fill.is_moving(now) || self.width_px <= 0.0 {
                    return None;
                }
                let next = self.drawn.max(target) + 1.0 / self.width_px;
                Loading::creep_reaches(target, next, self.motion)
                    .and_then(|after| self.arrived_at.checked_add(after))
            }
        }
    }

    /// Draw the line into `band` (the top edge of the window's content, the
    /// line's full length and thickness) at `transform`, under whatever clip
    /// gives it the content's shape. Returns the bounds it painted, in the
    /// scene's coordinates: none while it shows nothing.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        band: Rect,
    ) -> Vec<Rect> {
        self.motion = theme.motion();
        let now = crate::motion::frame_now();
        if self.finishing.is_some() && self.opacity(now).is_none() {
            // The fade is over: nothing is left to draw or to wait for.
            self.reset();
        }
        let Some(visual) = self.visual(now) else {
            return Vec::new();
        };
        let device = transform.transform_rect_bbox(band);
        self.width_px = device.width();
        let colors = &theme.tokens.colors;
        match visual {
            LoadingVisual::Bar {
                fill,
                opacity,
                track,
            } => {
                self.drawn = self.drawn.max(fill);
                if track {
                    paint(scene, transform, band, colors.loading_track(), opacity);
                }
                let bar = Rect::new(band.x0, band.y0, band.x0 + band.width() * fill, band.y1);
                if bar.width() > 0.0 {
                    paint(scene, transform, bar, colors.loading_line(), opacity);
                }
            }
            LoadingVisual::Sweep { sweep, opacity } => {
                paint(scene, transform, band, colors.loading_track(), opacity);
                paint_highlight(
                    scene,
                    transform,
                    band,
                    sweep,
                    colors.loading_line().multiply_alpha(opacity),
                );
            }
            LoadingVisual::Pulse { opacity } => {
                self.pulsed_at = Some(now);
                paint(scene, transform, band, colors.loading_track(), opacity);
            }
        }
        vec![device.inflate(1.0, 1.0)]
    }
}

/// Fill `rect` with `color` at `opacity`.
fn paint(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    color: vello::peniko::Color,
    opacity: f32,
) {
    scene.fill(
        vello::peniko::Fill::NonZero,
        transform,
        color.multiply_alpha(opacity),
        None,
        &rect,
    );
}

/// The busy highlight, centred `sweep` of its travel across `band`: clear at
/// its tail, `color` at its head, clear again just past it.
fn paint_highlight(
    scene: &mut Scene,
    transform: Affine,
    band: Rect,
    sweep: f64,
    color: vello::peniko::Color,
) {
    let width = band.width() * Loading::SWEEP_SHARE;
    if width <= 0.0 {
        return;
    }
    let x0 = band.x0 - width + sweep * (band.width() + width);
    let x1 = x0 + width;
    let shown = Rect::new(x0.max(band.x0), band.y0, x1.min(band.x1), band.y1);
    if shown.width() <= 0.0 {
        return;
    }
    let clear = color.multiply_alpha(0.0);
    let gradient = vello::peniko::Gradient::new_linear((x0, band.y0), (x1, band.y0)).with_stops(
        &[
            (0.0, clear),
            (0.45, color.multiply_alpha(0.55)),
            (0.72, color),
            (1.0, clear),
        ][..],
    );
    scene.fill(
        vello::peniko::Fill::NonZero,
        transform,
        &gradient,
        None,
        &shown,
    );
}

/// Where a window's loading line stands: along the top of `content` (or of
/// what is under the app's own header row holding the window's bar, from
/// `top`), its full width and `theme`'s line thickness.
#[must_use]
pub fn band(content: Rect, top: f64, theme: &Theme) -> Rect {
    let thickness = f64::from(theme.tokens.metrics.loading_line_px.max(0.0));
    let y0 = top.clamp(content.y0, content.y1);
    Rect::new(content.x0, y0, content.x1, (y0 + thickness).min(content.y1))
}

#[cfg(test)]
#[path = "window_loading_tests.rs"]
mod tests;
