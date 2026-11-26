//! `LevelMeter`: how loud an audio input is right now, or how loud what an output plays.
//!
//! The component owns its source: while it runs it opens the device's meter itself
//! (`media::audio_runtime`) and reads the level between frames, so the view passes only the
//! device's name, which side it meters, and whether the person is testing it. An input's level
//! is read each frame; an output is metered on the capture's own thread and wakes the window when
//! sound arrives, so an idle sink costs no frames. The level rises with the sound and falls back
//! slowly, with the recent peak held and then eased down. A device with no microphone, or an
//! audio service that does not answer, is a designed state the widget draws and names.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use media::audio_runtime::{
    AudioRuntimeControl, BoundedSession, MicrophoneMeterSession, RealAudioRuntime,
};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect, Vec2};
use vello::peniko::Fill;

use tilo_ui_base::i18n::widget_text;

use tilo_ui_base::theme::Theme;
use crate::time::{Instant, now};
use crate::wake::WakeHandle;
use tilo_ui_base::{widget::TaffyTreeExt, widget::Widget};

#[path = "level_meter_component.rs"]
mod component;
pub use component::LevelMeterProps;

/// How fast the bar follows the sound up, and falls back, in seconds of its time constant.
const RISE_SECS: f32 = 0.04;
const FALL_SECS: f32 = 0.34;
/// How long the peak holds where it rose before it eases down, and how long that ease takes.
const PEAK_HOLD_SECS: f32 = 0.55;
const PEAK_FALL_SECS: f32 = 0.35;
/// A frame's step is clamped so a stalled window does not jump the bar in one go.
const MAX_STEP_SECS: f32 = 0.1;

/// The smoothed level and the held peak, advanced from readings: pure, and what the tests pin.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeterState {
    level: f32,
    peak: f32,
    since_peak: f32,
}

impl MeterState {
    /// The level now, `0..=1`.
    #[must_use]
    pub fn level(&self) -> f32 {
        self.level
    }

    /// The held peak now, `0..=1`, never below the level.
    #[must_use]
    pub fn peak(&self) -> f32 {
        self.peak
    }

    /// Forget what was read: the next reading starts the bar from silence.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Follow `reading` (`0..=1`) for `dt` seconds: rise quickly, fall slowly, and hold a rising
    /// peak before it eases back down to the level.
    pub fn advance(&mut self, reading: f32, dt: f32) {
        let reading = if reading.is_finite() {
            reading.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let dt = if dt.is_finite() {
            dt.clamp(0.0, MAX_STEP_SECS)
        } else {
            0.0
        };
        if dt > 0.0 {
            let tau = if reading > self.level {
                RISE_SECS
            } else {
                FALL_SECS
            };
            let follow = 1.0 - (-dt / tau).exp();
            self.level += (reading - self.level) * follow;
        }
        self.level = self.level.clamp(0.0, 1.0);
        if self.level >= self.peak {
            self.peak = self.level;
            self.since_peak = 0.0;
        } else {
            self.since_peak += dt;
            if self.since_peak > PEAK_HOLD_SECS {
                let eased = ((self.since_peak - PEAK_HOLD_SECS) / PEAK_FALL_SECS).clamp(0.0, 1.0);
                self.peak += (self.level - self.peak) * eased;
            }
        }
        self.peak = self.peak.max(self.level).clamp(0.0, 1.0);
    }
}

/// What the meter reads: a microphone per frame, or a sink's monitor pushing levels from its own
/// thread.
enum Session {
    Input(Box<dyn MicrophoneMeterSession>),
    Output(Box<dyn BoundedSession>),
}

impl Session {
    fn stop(&mut self) {
        match self {
            Self::Input(session) => session.stop(),
            Self::Output(session) => session.stop(),
        }
    }
}

/// Paints the bar: the track, the level as the accent's fill, and the held peak as a mark.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LevelMeterFace {
    state: MeterState,
    problem: bool,
}

impl LevelMeterFace {
    #[must_use]
    pub const fn new(state: MeterState, problem: bool) -> Self {
        Self { state, problem }
    }

    pub fn paint(&self, scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let colors = &theme.tokens.colors.intent;
        let radius = f64::from(theme.tokens.radii.detail).min(rect.height() / 2.0);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.value_track,
            None,
            &RoundedRect::from_rect(rect, radius),
        );
        if self.problem {
            return;
        }
        let level = f64::from(self.state.level());
        if level > 0.0 {
            let filled = Rect::new(rect.x0, rect.y0, rect.x0 + level * rect.width(), rect.y1);
            scene.fill(
                Fill::NonZero,
                transform,
                colors.value_fill,
                None,
                &RoundedRect::from_rect(filled, radius),
            );
        }
        // A meter that has read nothing has no peak to mark: a mark at zero
        // would read as a level the device never gave.
        if self.state.peak() <= 0.0 {
            return;
        }
        let peak_x =
            (rect.x0 + f64::from(self.state.peak()) * rect.width()).clamp(rect.x0, rect.x1);
        let half = 1.0;
        let peak = Rect::new(
            (peak_x - half).max(rect.x0),
            rect.y0,
            (peak_x + half).min(rect.x1),
            rect.y1,
        );
        if !peak.is_zero_area() {
            scene.fill(
                Fill::NonZero,
                transform,
                colors.accent_ink,
                None,
                &RoundedRect::from_rect(peak, 1.0),
            );
        }
    }
}

pub struct LevelMeter {
    source: String,
    running: bool,
    /// Whether the source is a sink to meter, rather than a microphone to read.
    output: bool,
    label: Option<String>,
    state: MeterState,
    /// Why there is no level to show, in words a person reads: a device with no microphone, or
    /// an audio service that did not answer.
    problem: Option<String>,
    session: Option<Session>,
    /// What a sink's monitor last published, and whether a reading arrived since the last frame.
    level: Arc<AtomicU32>,
    pending: Arc<AtomicBool>,
    wake: Option<WakeHandle>,
    last: Option<Instant>,
    rect: Rect,
    dirty: bool,
    /// What the meter says it measures and what it reads now, above the bar: their style, the
    /// gap under the words and the track's height, from the theme the meter was laid out in.
    caption_style: Option<tilo_ui_base::text_render::TextStyle>,
    caption_gap: f64,
    track_h: f64,
}

impl LevelMeter {
    #[must_use]
    pub fn new(source: impl Into<String>, running: bool) -> Self {
        let mut meter = Self {
            source: source.into(),
            running: false,
            output: false,
            label: None,
            state: MeterState::default(),
            problem: None,
            session: None,
            level: Arc::new(AtomicU32::new(0.0_f32.to_bits())),
            pending: Arc::new(AtomicBool::new(false)),
            wake: None,
            last: None,
            rect: Rect::ZERO,
            dirty: true,
            caption_style: None,
            caption_gap: 0.0,
            track_h: 0.0,
        };
        meter.set_running(running);
        meter
    }

    #[must_use]
    pub fn state(&self) -> MeterState {
        self.state
    }

    /// Why there is no level to show, when there is none.
    #[must_use]
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn set_label(&mut self, label: Option<String>) {
        if self.label != label {
            self.label = label;
            self.dirty = true;
        }
    }

    /// Meter the source as a sink's output rather than as a microphone.
    pub fn set_output(&mut self, output: bool) {
        if self.output == output {
            return;
        }
        self.output = output;
        if self.running {
            self.close();
            self.open();
        }
        self.dirty = true;
    }

    /// Read `source`; a new device restarts the meter where it was reading one.
    pub fn set_source(&mut self, source: impl Into<String>) {
        let source = source.into();
        if self.source == source {
            return;
        }
        self.source = source;
        if self.running {
            self.close();
            self.open();
        }
        self.dirty = true;
    }

    /// Open the meter only while this is true: the person's test starts it, and letting go
    /// closes it.
    pub fn set_running(&mut self, running: bool) {
        if self.running == running {
            return;
        }
        self.running = running;
        if running {
            self.open();
        } else {
            self.close();
        }
        self.dirty = true;
    }

    fn open(&mut self) {
        self.problem = None;
        self.state.reset();
        self.last = None;
        self.pending.store(false, Ordering::Relaxed);
        self.level.store(0.0_f32.to_bits(), Ordering::Relaxed);
        if self.session.is_some() {
            return;
        }
        if self.source.trim().is_empty() {
            self.problem = Some(widget_text("level-meter-no-microphone"));
            return;
        }
        if self.output {
            let level = Arc::clone(&self.level);
            let pending = Arc::clone(&self.pending);
            let wake = WakeHandle::new();
            self.wake = Some(wake.clone());
            let publish = move |value: f32| {
                level.store(value.to_bits(), Ordering::Relaxed);
                pending.store(true, Ordering::Relaxed);
                wake.wake();
            };
            match RealAudioRuntime.start_output_meter(&self.source, Box::new(publish)) {
                Ok(session) => self.session = Some(Session::Output(session)),
                Err(_) => self.problem = Some(widget_text("level-meter-no-output")),
            }
        } else {
            match RealAudioRuntime
                .start_microphone_meter(&self.source, Duration::from_secs(60 * 60))
            {
                Ok(session) => self.session = Some(Session::Input(session)),
                Err(_) => self.problem = Some(widget_text("level-meter-no-microphone")),
            }
        }
    }

    fn close(&mut self) {
        if let Some(mut session) = self.session.take() {
            session.stop();
        }
        self.state.reset();
        self.last = None;
        self.problem = None;
        self.wake = None;
        self.pending.store(false, Ordering::Relaxed);
    }

    /// The reading this frame: a microphone's level, or what a sink's monitor last published.
    fn reading(&mut self) -> f32 {
        if self.output {
            self.pending.store(false, Ordering::Relaxed);
            f32::from_bits(self.level.load(Ordering::Relaxed))
        } else {
            self.session.as_mut().map_or(0.0, |session| match session {
                Session::Input(input) => input.level(),
                Session::Output(_) => 0.0,
            })
        }
    }

    /// Read the level since the last frame and move the bar towards it.
    fn advance(&mut self) {
        if !self.running {
            return;
        }
        let at = now();
        let step = self
            .last
            .map_or(0.0, |last| at.saturating_duration_since(last).as_secs_f32());
        self.last = Some(at);
        let reading = self.reading();
        self.state.advance(reading, step);
        self.dirty = true;
    }

    fn name_text(&self) -> String {
        self.label
            .clone()
            .unwrap_or_else(|| widget_text("level-meter-name"))
    }

    fn value_text(&self) -> String {
        if let Some(problem) = &self.problem {
            return problem.clone();
        }
        if !self.running {
            // A meter that is not reading has no level: saying "0%" would
            // claim it measured silence.
            return widget_text("level-meter-idle");
        }
        let percent = (self.state.level() * 100.0).round();
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a rounded percentage below 100 is exact"
        )]
        let percent = percent as i64;
        format!("{percent}%")
    }

    /// Where the track stands in the meter's rect: along its foot, under the
    /// words that name it and say what it reads.
    fn track_rect(&self) -> Rect {
        let h = self.track_h.min(self.rect.height());
        Rect::new(self.rect.x0, self.rect.y1 - h, self.rect.x1, self.rect.y1)
    }
}

impl Drop for LevelMeter {
    fn drop(&mut self) {
        self.close();
    }
}

impl Widget for LevelMeter {
    tilo_ui_base::described!();

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.caption_style = Some(theme.text(tilo_ui_base::tokens::TextRole::Label));
        self.caption_gap = f64::from(theme.tokens.spacing.rhythm.tight);
        self.track_h = f64::from(theme.tokens.spacing.s2) * 2.0;
        // A named meter says its name and its reading above the bar; a bare
        // one is the bar alone, as it always was. The caption's line is the
        // role's own line box, so the height is settled without shaping words.
        let height = if self.label.is_some() {
            let line = self
                .caption_style
                .as_ref()
                .map_or(0.0, |style| f64::from(style.line_height_px()));
            Dimension::length(tilo_ui_base::num::f32_of_f64(
                (line + self.caption_gap + self.track_h).ceil(),
            ))
        } else {
            Dimension::length(tilo_ui_base::num::f32_of_f64(self.track_h))
        };
        let style = Style {
            size: Size {
                width: Dimension::auto(),
                height,
            },
            flex_grow: 1.0,
            flex_shrink: 1.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    tilo_ui_base::rect_geometry!();

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        self.advance();
        let track = self.track_rect();
        LevelMeterFace::new(self.state, self.problem.is_some())
            .paint(scene, transform, theme, track);
        if self.label.is_none() {
            return;
        }
        // What it measures at the leading edge, what it reads at the trailing
        // one: a bar with neither is unreadable.
        let style = self
            .caption_style
            .clone()
            .unwrap_or_else(|| theme.text(tilo_ui_base::tokens::TextRole::Label));
        let row = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            (track.y0 - self.caption_gap).max(self.rect.y0),
        );
        let (_, line) = text_renderer.measure_style("Ag", None, &style);
        let y = row.y0 + f64::from(row.height() - f64::from(line)).max(0.0) / 2.0;
        let value = self.value_text();
        let value_style = style.clone().numeric();
        let (value_width, _) = text_renderer.measure_style(&value, None, &value_style);
        let value_x = (row.x1 - f64::from(value_width)).max(row.x0);
        text_renderer.draw_style(
            scene,
            &value,
            theme.text_primary,
            transform.pre_translate(Vec2::new(value_x, y)),
            None,
            &value_style,
        );
        let gap = f64::from(theme.tokens.spacing.s1);
        let room = (value_x - row.x0 - gap).max(0.0);
        let name = self.name_text();
        let words = text_renderer.elide_to_width_style(&name, tilo_ui_base::num::f32_of_f64(room), &style);
        text_renderer.draw_style(
            scene,
            &words,
            theme.text_dim,
            transform.pre_translate(Vec2::new(row.x0, y)),
            None,
            &style,
        );
    }

    tilo_ui_base::redraw_flag!();

    fn name(&self) -> &'static str {
        "LevelMeter"
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn needs_animation_frame(&self) -> bool {
        // A meter that could not open its device shows why and rests: a
        // service that never answers must not paint the window every frame.
        if !self.running || self.problem.is_some() {
            return false;
        }
        if self.output {
            self.pending.load(Ordering::Relaxed)
        } else {
            true
        }
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::{a11y::AccessNode, a11y::Role};
        Some(
            AccessNode::new(Role::ProgressBar)
                .with_name(self.name_text())
                .with_value(self.value_text()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reading_rises_the_bar_towards_it_and_never_past_it() {
        let mut state = MeterState::default();
        for _ in 0..200 {
            state.advance(1.0, 0.016);
        }
        assert!(
            state.level() > 0.99,
            "rises to the reading: {}",
            state.level()
        );
        state.advance(2.0, 0.016);
        assert!(state.level() <= 1.0, "clamped to one");
        state.advance(-1.0, 0.016);
        assert!(state.level() >= 0.0, "clamped to zero");
    }

    #[test]
    fn the_bar_falls_slower_than_it_rises() {
        let mut risen = MeterState::default();
        for _ in 0..6 {
            risen.advance(1.0, 0.016);
        }
        let mut fallen = MeterState::default();
        fallen.advance(1.0, 0.096);
        let up = risen.level();
        fallen.advance(0.0, 0.096);
        let down = 1.0 - fallen.level();
        assert!(
            down < up,
            "a fall of one step is smaller than a rise: {down} against {up}"
        );
    }

    #[test]
    fn the_peak_holds_where_it_rose_then_eases_to_the_level() {
        let mut state = MeterState::default();
        for _ in 0..200 {
            state.advance(0.8, 0.016);
        }
        let held = state.peak();
        assert!(
            (held - state.level()).abs() < 1e-3,
            "peak rides the level up"
        );
        for _ in 0..20 {
            state.advance(0.0, 0.016);
        }
        assert!(
            (state.peak() - held).abs() < 1e-3,
            "peak holds while the level falls"
        );
        for _ in 0..200 {
            state.advance(0.0, 0.016);
        }
        assert!(state.peak() < held, "peak eases down after the hold");
        assert!(
            state.peak() >= state.level() - 1e-6,
            "peak never below the level"
        );
    }

    #[test]
    fn a_widget_without_a_device_names_the_state_instead_of_showing_zero() {
        let meter = LevelMeter::new("", false);
        assert!(
            meter.problem().is_none(),
            "a meter that is not running has no problem yet"
        );
        let mut meter = LevelMeter::new("", true);
        assert_eq!(meter.problem(), Some("No microphone"));
        assert_eq!(meter.value_text(), "No microphone");
        meter.set_running(false);
        assert!(meter.problem().is_none(), "closing clears the problem");
    }

    #[test]
    fn the_meter_is_named_and_valued_for_a_reader() {
        let mut meter = LevelMeter::new("alsa_input.mic", false);
        meter.set_label(Some("Built-in microphone".to_owned()));
        meter.running = true;
        let node = meter.accessibility().expect("the meter is announced");
        assert_eq!(node.name.as_deref(), Some("Built-in microphone"));
        assert_eq!(node.value.as_deref(), Some("0%"));
    }

    #[test]
    fn the_meter_paints_its_name_and_its_reading_above_the_bar() {
        use vello::Scene;
        use vello::kurbo::Affine;

        let mut meter = LevelMeter::new("alsa_input.mic", false);
        meter.set_label(Some("Microphone level".to_owned()));
        meter.state = MeterState {
            level: 0.5,
            peak: 0.5,
            since_peak: 0.0,
        };
        meter.running = true;
        meter.rect = Rect::new(0.0, 0.0, 200.0, 32.0);
        meter.track_h = 8.0;
        meter.caption_gap = 4.0;
        let theme = Theme::dark();
        let mut scene = Scene::new();
        let mut text = tilo_ui_base::text_render::TextRenderer::new();
        Widget::draw(
            &mut meter,
            &mut tilo_ui_base::draw::DrawCtx::new(
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                &theme,
                tilo_ui_base::presence::PresenceCtx::default().draw_env(),
            ),
        );
        assert!(
            !scene.encoding().resources.glyph_runs.is_empty(),
            "what it measures and what it reads are painted"
        );
        // The bar stands under the words, not through them.
        assert_eq!(meter.track_rect(), Rect::new(0.0, 24.0, 200.0, 32.0));
    }

    #[test]
    fn a_silent_meter_marks_no_peak() {
        use vello::Scene;
        use vello::kurbo::Affine;
        use vello_encoding::DrawTag;

        let mut scene = Scene::new();
        LevelMeterFace::new(MeterState::default(), false).paint(
            &mut scene,
            Affine::IDENTITY,
            &Theme::dark(),
            Rect::new(0.0, 0.0, 200.0, 8.0),
        );
        let fills = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|tag| **tag == DrawTag::COLOR)
            .count();
        assert_eq!(
            fills, 1,
            "the track alone: no peak stands where nothing was read"
        );
    }
}
