//! Watching a mount's graphics start-up until its first frame is out.
//!
//! A start that stays on one step (device, pipelines, surface, first frame)
//! is said to the page as stalled, so the person reads that it is taking
//! longer than it should and may try again. It is never failed for being
//! slow: the browser compiles every page's pipelines on the same threads
//! and does not cancel those of a page torn down, so on a busy machine with
//! a software rasteriser one page's pipelines waited over 60 s behind the
//! compiles of others while nothing of its own finished. Failing it then
//! turned a start that would have finished into "the desktop did not
//! start", and trying again only queued the same compiles once more.

/// Where graphics start-up stands for a mount that has not drawn yet, as the
/// page names it (`gpuStep` in the status).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum GpuStep {
    /// Not started: behind the runtime's own work or another mount's set-up.
    Queued,
    /// Asking the browser for a WebGPU adapter and device.
    Device,
    /// The browser compiling the renderer's pipelines.
    Pipelines,
    /// Configuring the canvas on the device.
    Surface,
    /// Set up; its first frame not presented yet.
    FirstFrame,
}

impl GpuStep {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Device => "device",
            Self::Pipelines => "pipelines",
            Self::Surface => "surface",
            Self::FirstFrame => "first-frame",
        }
    }

    pub(crate) const fn waiting_on(self) -> &'static str {
        match self {
            Self::Queued => "waiting for graphics set-up to start",
            Self::Device => "waiting for the WebGPU device",
            Self::Pipelines => "waiting for the renderer's pipelines to compile",
            Self::Surface => "waiting to configure the canvas",
            Self::FirstFrame => "waiting for the first frame to present",
        }
    }
}

/// How long a mount may stay visible on one step of graphics start-up
/// before the page says it stalled. Bounded per step, not from the mount: a
/// start that keeps moving on is slow, not stuck. On a busy machine with a
/// software rasteriser (`SwiftShader` in CI) the device alone took 28 s and
/// the pipelines 10 s; on a GPU the whole start takes about 2 s. Each
/// pipeline compiled is progress too.
pub(crate) const STALL_MS: u32 = 60_000;

/// What the watcher does on one tick.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Tick {
    /// The step changed: say it (`gpuStep`).
    pub(crate) step_changed: bool,
    /// Whether the page is told the start stalled, when that changed.
    pub(crate) stalled: Option<bool>,
}

/// One mount's start-up watch: fed the step, the pipelines compiled so far
/// and whether the mount is visible, once per `tick_ms`.
#[derive(Debug)]
pub(crate) struct Watch {
    tick_ms: u32,
    on: Option<(GpuStep, u32)>,
    visible_ms: u32,
    stalled: bool,
}

impl Watch {
    pub(crate) const fn new(tick_ms: u32) -> Self {
        Self {
            tick_ms,
            on: None,
            visible_ms: 0,
            stalled: false,
        }
    }

    pub(crate) fn tick(&mut self, step: GpuStep, built: u32, visible: bool) -> Tick {
        let mut out = Tick::default();
        if self.on != Some((step, built)) {
            out.step_changed = self.on.map(|(on, _)| on) != Some(step);
            self.on = Some((step, built));
            self.visible_ms = 0;
            if std::mem::take(&mut self.stalled) {
                out.stalled = Some(false);
            }
        }
        // Time hidden (a background tab draws nothing) does not count.
        if !visible {
            return out;
        }
        self.visible_ms = self.visible_ms.saturating_add(self.tick_ms);
        if self.visible_ms >= STALL_MS && !self.stalled {
            self.stalled = true;
            out.stalled = Some(true);
        }
        out
    }
}

/// How long a requested redraw may take to arrive before the first frame is
/// drawn without it ([`Drive`]).
pub(crate) const DRIVE_MS: u32 = 250;

/// What [`Drive::tick`] decides.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Drive {
    /// The first frame is out and redraws arrive: drawing is theirs again.
    Done,
    /// Nothing to do this tick.
    Wait,
    /// Draw now, through the same path a redraw takes.
    Draw,
}

/// Drawing a mount's first frame when the redraws it asks for do not
/// arrive. The browser delivers redraws on its animation frames, and those
/// stopped for a visible page whose device and pipelines were ready: with a
/// software rasteriser on a busy machine, the browser's GPU process was
/// still compiling pipelines of pages already closed, and the page waited
/// on a frame that never came. A timer is not paced by the GPU process, so
/// a frame still gets drawn. Only until the first frame is out and a redraw
/// has arrived; a hidden page draws nothing, as its animation frames pause
/// on purpose.
#[derive(Debug, Default)]
pub(crate) struct Driver {
    redraws: u32,
    arrived: bool,
}

impl Driver {
    /// One tick, every [`DRIVE_MS`]: `redraws` counts the redraws delivered
    /// so far, `frames` the frames presented, `wants_frame` whether the
    /// mount has something to draw.
    pub(crate) fn tick(
        &mut self,
        redraws: u32,
        frames: u64,
        visible: bool,
        wants_frame: bool,
    ) -> Drive {
        let arrived_now = redraws != self.redraws;
        self.redraws = redraws;
        self.arrived |= arrived_now;
        if frames > 0 && self.arrived {
            Drive::Done
        } else if arrived_now || !visible || !wants_frame {
            Drive::Wait
        } else {
            Drive::Draw
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: u32 = 1_000;

    fn run(watch: &mut Watch, seconds: u32, step: GpuStep, built: u32) -> Vec<Tick> {
        (0..seconds)
            .map(|_| watch.tick(step, built, true))
            .collect()
    }

    // Catches a slow pipeline compile on a busy machine failing the desktop
    // ("did not start") instead of the page saying it is slow and waiting.
    #[test]
    fn a_step_that_takes_long_is_said_to_stall_and_is_waited_for() {
        let mut watch = Watch::new(TICK);
        let ticks = run(&mut watch, 59, GpuStep::Pipelines, 0);
        assert!(ticks.iter().all(|t| t.stalled.is_none()));
        assert_eq!(watch.tick(GpuStep::Pipelines, 0, true).stalled, Some(true));
        // Said once, then waited for as long as it takes.
        let ticks = run(&mut watch, 600, GpuStep::Pipelines, 0);
        assert!(ticks.iter().all(|t| t.stalled.is_none()));
        // A pipeline compiled after all: no longer stalled.
        assert_eq!(watch.tick(GpuStep::Pipelines, 1, true).stalled, Some(false));
    }

    // Catches a compile that keeps finishing pipelines being called stalled.
    #[test]
    fn each_pipeline_compiled_is_progress() {
        let mut watch = Watch::new(TICK);
        for built in 0..10 {
            let ticks = run(&mut watch, 50, GpuStep::Pipelines, built);
            assert!(ticks.iter().all(|t| t.stalled.is_none()), "{built}");
        }
    }

    // Catches the time a background tab spends hidden counting as a stall.
    #[test]
    fn hidden_time_does_not_count() {
        let mut watch = Watch::new(TICK);
        for _ in 0..600 {
            let tick = watch.tick(GpuStep::Device, 0, false);
            assert!(tick.stalled.is_none());
        }
        let ticks = run(&mut watch, 59, GpuStep::Device, 0);
        assert!(ticks.iter().all(|t| t.stalled.is_none()));
    }

    #[test]
    fn a_new_step_is_said_once() {
        let mut watch = Watch::new(TICK);
        assert!(watch.tick(GpuStep::Device, 0, true).step_changed);
        assert!(!watch.tick(GpuStep::Device, 0, true).step_changed);
        assert!(watch.tick(GpuStep::Pipelines, 0, true).step_changed);
        assert!(!watch.tick(GpuStep::Pipelines, 1, true).step_changed);
    }

    // Catches a visible page waiting forever on a first frame whose
    // redraws (animation frames) stopped arriving.
    #[test]
    fn a_first_frame_whose_redraws_do_not_arrive_is_drawn_anyway() {
        let mut driver = Driver::default();
        assert_eq!(driver.tick(0, 0, true, true), Drive::Draw);
        assert_eq!(driver.tick(0, 0, true, true), Drive::Draw);
        // Out, but no redraw has arrived yet: keep drawing what is asked for.
        assert_eq!(driver.tick(0, 1, true, true), Drive::Draw);
        assert_eq!(driver.tick(0, 1, true, false), Drive::Wait);
        // Redraws arrive again: they draw from now on.
        assert_eq!(driver.tick(1, 1, true, true), Drive::Done);
    }

    // Catches the timer drawing beside redraws that arrive, or drawing a
    // hidden page whose animation frames pause on purpose.
    #[test]
    fn arriving_redraws_and_hidden_pages_are_left_alone() {
        let mut driver = Driver::default();
        assert_eq!(driver.tick(1, 0, true, true), Drive::Wait);
        assert_eq!(driver.tick(2, 0, true, true), Drive::Wait);
        assert_eq!(driver.tick(2, 0, false, true), Drive::Wait);
        assert_eq!(driver.tick(2, 0, true, true), Drive::Draw);
        assert_eq!(driver.tick(2, 1, true, true), Drive::Done);
    }
}
