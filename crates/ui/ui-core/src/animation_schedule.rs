//! Paces animation-only redraw requests on the display refresh; input and renderer recovery bypass it.
//! The beat is the refresh interval of the output the window is on
//! ([`AnimationSchedule::set_refresh_millihertz`]), read again whenever an animation starts, so a 90 Hz
//! panel animates at 90 Hz and a window moved to a 60 Hz monitor follows it.
use crate::time::{Duration, Instant};

/// Only when the output reports no refresh rate (some X11 and headless
/// hosts): the common display rate, until the output says otherwise.
const UNREPORTED_REFRESH_INTERVAL: Duration = Duration::from_nanos(16_666_667);

const REFRESH_MILLIHERTZ: std::ops::RangeInclusive<u32> = 1_000..=1_000_000;

pub(super) struct AnimationSchedule {
    next_tick: Option<Instant>,
    interval: Duration,
}

impl Default for AnimationSchedule {
    fn default() -> Self {
        Self {
            next_tick: None,
            interval: UNREPORTED_REFRESH_INTERVAL,
        }
    }
}

impl AnimationSchedule {
    /// Whether no animation is running, so the next active poll starts one (the moment to read the
    /// output's refresh rate again).
    pub(super) fn idle(&self) -> bool {
        self.next_tick.is_none()
    }

    /// Take the beat from the output's refresh rate.
    pub(super) fn set_refresh_millihertz(&mut self, millihertz: Option<u32>) {
        if let Some(millihertz) = millihertz.filter(|mhz| REFRESH_MILLIHERTZ.contains(mhz)) {
            self.interval = Duration::from_nanos(1_000_000_000_000 / u64::from(millihertz));
        }
    }

    /// One refresh of the output: the animation beat.
    pub(super) fn interval(&self) -> Duration {
        self.interval
    }

    /// Returns whether to request a frame and the next animation wake deadline.
    pub(super) fn poll(&mut self, now: Instant, active: bool) -> (bool, Option<Instant>) {
        if !active {
            self.next_tick = None;
            return (false, None);
        }
        match self.next_tick {
            Some(deadline) if now < deadline => (false, Some(deadline)),
            Some(deadline) => {
                let behind = now.saturating_duration_since(deadline).as_nanos();
                let beat = self.interval.as_nanos().max(1);
                let beats = u32::try_from(behind / beat + 1).unwrap_or(u32::MAX);
                self.next_tick = deadline
                    .checked_add(self.interval.saturating_mul(beats))
                    .or_else(|| now.checked_add(self.interval));
                (true, self.next_tick)
            }
            None => {
                self.next_tick = Some(now.checked_add(self.interval).unwrap_or(now));
                (true, self.next_tick)
            }
        }
    }
}

const STILL_FRAMES: u32 = 90;

/// A check that animation frames move something.
#[derive(Default)]
pub(super) struct StillAnimation {
    unchanged: u32,
    named: bool,
}

impl StillAnimation {
    /// Note one redraw.
    pub(super) fn frame(&mut self, animating: bool, presented: bool) -> bool {
        if !animating || presented {
            self.unchanged = 0;
            self.named = false;
            return false;
        }
        self.unchanged = self.unchanged.saturating_add(1);
        if self.unchanged >= STILL_FRAMES && !self.named {
            self.named = true;
            return true;
        }
        false
    }
}

/// Paths to the deepest widgets under `root` asking for animation frames:
/// the ones actually holding the loop open, not every container above them,
/// each with its box and [`tilo_ui_base::widget::Widget::animation_detail`]. `~`
/// marks a widget that animates without asking to be redrawn: a retained
/// parent cache then never reaches it again. Allocates, so only diagnostics
/// call it.
pub fn animating_leaves(root: &dyn tilo_ui_base::widget::Widget) -> Vec<String> {
    fn walk(widget: &dyn tilo_ui_base::widget::Widget, path: &str, out: &mut Vec<String>) {
        if !widget.needs_animation_frame() {
            return;
        }
        let clean = if widget.needs_redraw() || widget.needs_animation_frame() {
            ""
        } else {
            "~"
        };
        let here = match widget.test_id() {
            Some(id) => format!("{path}/{}{clean}#{id}", widget.name()),
            None => format!("{path}/{}{clean}", widget.name()),
        };
        let before = out.len();
        widget.for_each_child(&mut |child| walk(child, &here, out));
        if out.len() == before {
            let rect = widget.get_rect();
            let detail = widget
                .animation_detail()
                .map(|detail| format!(" {detail}"))
                .unwrap_or_default();
            out.push(format!(
                "{here}@{:.0},{:.0} {:.0}x{:.0}{detail}",
                rect.x0,
                rect.y0,
                rect.width(),
                rect.height()
            ));
        }
    }
    let mut out = Vec::new();
    walk(root, "", &mut out);
    out
}

/// Paths to the deepest widgets under `root` whose one-shot wake deadline
/// is `due` or earlier: who wakes an otherwise idle loop next. Allocates, so
/// only diagnostics call it.
pub fn waking_leaves(root: &dyn tilo_ui_base::widget::Widget, due: Instant) -> Vec<String> {
    fn walk(widget: &dyn tilo_ui_base::widget::Widget, due: Instant, path: &str, out: &mut Vec<String>) {
        if widget.next_wake_deadline().is_none_or(|at| at > due) {
            return;
        }
        let here = match widget.test_id() {
            Some(id) => format!("{path}/{}#{id}", widget.name()),
            None => format!("{path}/{}", widget.name()),
        };
        let before = out.len();
        widget.for_each_child(&mut |child| walk(child, due, &here, out));
        if out.len() == before {
            out.push(here);
        }
    }
    let mut out = Vec::new();
    walk(
        root,
        due.checked_add(WAKE_MATCH_SLACK).unwrap_or(due),
        "",
        &mut out,
    );
    out
}

/// Deadlines computed against a slightly later clock than the minimum they are compared with still name
/// the same owner.
pub const WAKE_MATCH_SLACK: Duration = Duration::from_millis(1);
