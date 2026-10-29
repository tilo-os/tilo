//! Optional paint-rate diagnostics (`TILO_PAINT_STATS=1`): every ~2 s, how
//! many frames painted vs idled and why, confirming the loop actually idles
//! without guessing. `TILO_PAINT_STATS=ticks` also prints a `paint-tick:`
//! line on every iteration, so a reader can tell when each one ran: a summary
//! covers everything since the previous one, which may lie long before the
//! period the reader measures (the idle gate counts only its window).

use std::time::{Duration, Instant};

use crate::compositor::idle::SessionManager;
use crate::shell::frame::{AnimationDemand, FrameBuilder};
use crate::shell::present::PresentPipe;

use super::WakeTally;

/// What made the loop's redraw gate open this tick, as a set.
#[derive(Clone, Copy, Default)]
pub(super) struct Causes(u8);

impl Causes {
    pub(super) const BACKEND_EVENT: u8 = 1;
    pub(super) const SESSION: u8 = 1 << 1;
    pub(super) const REVISION: u8 = 1 << 2;
    pub(super) const ANIMATION: u8 = 1 << 3;
    pub(super) const KEEPALIVE: u8 = 1 << 4;

    /// This set, plus `cause` when `present`.
    pub(super) fn with(self, cause: u8, present: bool) -> Self {
        Self(self.0 | if present { cause } else { 0 })
    }

    fn has(self, cause: u8) -> bool {
        self.0 & cause != 0
    }
}

/// Why this tick painted, as the loop's redraw gate decided it.
pub(super) struct TickCauses<'a> {
    pub(super) should_paint: bool,
    pub(super) causes: Causes,
    pub(super) animation_demand: &'a AnimationDemand,
}

pub(super) struct PaintStats {
    ticks: bool,
    /// What ended the park before this iteration, for `paint-tick:`.
    last_wake: &'static str,
    since: Instant,
    /// Report windows in which nothing painted, folded into the next report
    /// that has something to say: an idle session must not write two lines
    /// every two seconds into a bounded session log.
    idle: Duration,
    frames_total: u32,
    frames_painted: u32,
    cause_events: u32,
    cause_revision: u32,
    cause_anim: u32,
    cause_session: u32,
    cause_keepalive: u32,
    animation_sources_seen: AnimationDemand,
    animating_content_leaves: Vec<String>,
    wakes_seen: WakeTally,
    widget_wake_owner: Option<String>,
}

impl PaintStats {
    /// The diagnostics `TILO_PAINT_STATS` asks for; none when it is unset.
    pub(super) fn from_env() -> Option<Self> {
        let value = std::env::var("TILO_PAINT_STATS").ok()?;
        Some(Self {
            ticks: value.trim() == "ticks",
            last_wake: "start",
            since: Instant::now(),
            idle: Duration::ZERO,
            frames_total: 0,
            frames_painted: 0,
            cause_events: 0,
            cause_revision: 0,
            cause_anim: 0,
            cause_session: 0,
            cause_keepalive: 0,
            animation_sources_seen: AnimationDemand::default(),
            animating_content_leaves: Vec::new(),
            wakes_seen: WakeTally::default(),
            widget_wake_owner: None,
        })
    }

    /// Count this tick's paint decision, and report once a window is over.
    pub(super) fn note_tick(
        &mut self,
        tick: &TickCauses<'_>,
        frame_builder: &FrameBuilder,
        session: &SessionManager,
        frame_interval: Duration,
        present_pipe: Option<&PresentPipe>,
    ) {
        self.frames_total = self.frames_total.saturating_add(1);
        if self.ticks {
            crate::stderr_guard::write_line(format_args!(
                "paint-tick: painted={} woke={}",
                tick.should_paint, self.last_wake
            ));
        }
        if tick.should_paint {
            self.count_paint(tick, frame_builder);
        }
        if self.since.elapsed() >= Duration::from_secs(2) && self.frames_painted == 0 {
            // Idle: keep tallying wakes, report once painting resumes.
            self.idle = self.idle.saturating_add(self.since.elapsed());
            self.frames_total = 0;
            self.since = Instant::now();
        } else if self.since.elapsed() >= Duration::from_secs(2) {
            self.report(session, frame_interval, present_pipe);
        }
    }

    fn count_paint(&mut self, tick: &TickCauses<'_>, frame_builder: &FrameBuilder) {
        self.frames_painted = self.frames_painted.saturating_add(1);
        if tick.causes.has(Causes::BACKEND_EVENT) {
            self.cause_events = self.cause_events.saturating_add(1);
        }
        if tick.causes.has(Causes::SESSION) {
            self.cause_session = self.cause_session.saturating_add(1);
        }
        if tick.causes.has(Causes::REVISION) {
            self.cause_revision = self.cause_revision.saturating_add(1);
        }
        if tick.causes.has(Causes::ANIMATION) {
            let demand = tick.animation_demand;
            self.cause_anim = self.cause_anim.saturating_add(1);
            self.animation_sources_seen.host |= demand.host;
            self.animation_sources_seen.floating |= demand.floating;
            self.animation_sources_seen.transient |= demand.transient;
            self.animation_sources_seen.osk |= demand.osk;
            if demand.host & crate::shell::animation_diagnostics::CONTENT_BIT != 0
                && self.animating_content_leaves.is_empty()
            {
                self.animating_content_leaves = frame_builder.content_animating_leaves();
            }
        }
        let others = Causes::BACKEND_EVENT | Causes::SESSION | Causes::REVISION | Causes::ANIMATION;
        if tick.causes.has(Causes::KEEPALIVE) && !tick.causes.has(others) {
            self.cause_keepalive = self.cause_keepalive.saturating_add(1);
        }
    }

    fn report(
        &mut self,
        session: &SessionManager,
        frame_interval: Duration,
        present_pipe: Option<&PresentPipe>,
    ) {
        if !self.idle.is_zero() {
            crate::stderr_guard::write_line(format_args!(
                "paint-stats: idle {:.0}s before this, nothing painted",
                self.idle.as_secs_f64()
            ));
            self.idle = Duration::ZERO;
        }
        let animation_host_labels =
            crate::shell::animation_diagnostics::host_labels(self.animation_sources_seen.host)
                .collect::<Vec<_>>()
                .join(",");
        crate::stderr_guard::write_line(format_args!(
            "paint-stats: {}/{} painted | \
             events={} revision={} anim={} \
             session={} keepalive={} | \
             locked={} blanked={} | \
             anim_sources host=0x{:04x} host_labels={:?} content={:?} floating={} transient={} osk={} | \
             {:.3} Hz target | present={} backpressure={}",
            self.frames_painted,
            self.frames_total,
            self.cause_events,
            self.cause_revision,
            self.cause_anim,
            self.cause_session,
            self.cause_keepalive,
            session.is_locked(),
            session.is_blanked(),
            self.animation_sources_seen.host,
            animation_host_labels,
            self.animating_content_leaves.join(","),
            self.animation_sources_seen.floating,
            self.animation_sources_seen.transient,
            self.animation_sources_seen.osk,
            1.0 / frame_interval.as_secs_f64(),
            present_pipe.map_or_else(|| "inline".to_string(), |p| p.presented().to_string()),
            present_pipe.map_or(0, PresentPipe::backpressured),
        ));
        self.frames_total = 0;
        self.frames_painted = 0;
        self.cause_events = 0;
        self.cause_revision = 0;
        self.cause_anim = 0;
        self.cause_session = 0;
        self.cause_keepalive = 0;
        crate::stderr_guard::write_line(format_args!(
            "paint-stats: wakes {} | widget-deadline owner {:?}",
            self.wakes_seen.take_summary(),
            self.widget_wake_owner.take().unwrap_or_default(),
        ));
        self.animation_sources_seen = AnimationDemand::default();
        self.animating_content_leaves.clear();
        self.since = Instant::now();
    }

    /// What ended the park this iteration, for the next report.
    pub(super) fn note_wake(&mut self, wake: &'static str, frame_builder: &FrameBuilder) {
        self.wakes_seen.note(wake);
        self.last_wake = wake;
        if wake == "widget" && self.widget_wake_owner.is_none() {
            self.widget_wake_owner = Some(frame_builder.wake_deadline_owners().join(","));
        }
    }
}
