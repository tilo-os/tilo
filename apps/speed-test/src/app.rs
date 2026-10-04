#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::unwrap_used,
    reason = "bounded app-local arithmetic; Taffy allocation is infallible for this leaf"
)]

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use ui::time::{Duration, Instant};
use ui::widget::{AppModel, ModelPath};

use ui::ColorCompat;
use ui::a11y::{AccessNode, AccessState, Role};
use ui::framework::{
    Dispatcher, EffectCx, EffectStatus, Effected, Progress, ResourceSurface, SurfaceCx,
};
use ui::presence::PresenceCtx;
use ui::taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Arc as Sweep, Circle, Point, Rect, RoundedRect, Stroke};
use ui::vello::peniko::Fill;
use ui::widget::{Button, ButtonFace, Widget};
use ui::winit::event::{ElementState, MouseButton, WindowEvent};

use crate::resource::{Measurement, SpeedEffect, SpeedTest};

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Idle,
    Ping,
    Download,
    Upload,
    Complete,
    Error,
}

#[derive(Clone, Copy)]
struct ResultSet {
    ping: f64,
    jitter: f64,
    download: f64,
    upload: f64,
}

/// A finished run, as the worker measured it.
struct Run {
    result: ResultSet,
    server: String,
}

/// Where a run stands, shared between the surface and its worker.
#[derive(Default)]
struct RunSignal {
    phase: AtomicU8,
    stop: AtomicBool,
}

impl RunSignal {
    const PING: u8 = 0;
    const DOWNLOAD: u8 = 1;
    const UPLOAD: u8 = 2;
    /// The worker answered; the job's completion takes it from here.
    const ANSWERED: u8 = 3;
    /// The worker went away without answering: its caller cancelled it.
    const ABANDONED: u8 = 4;

    fn set(&self, phase: u8) {
        self.phase.store(phase, Ordering::Release);
    }

    fn phase(&self) -> u8 {
        self.phase.load(Ordering::Acquire)
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

/// Marks a run abandoned when its worker is dropped before answering.
struct Answers(Arc<RunSignal>);

impl Drop for Answers {
    fn drop(&mut self) {
        if self.0.phase() != RunSignal::ANSWERED {
            self.0.set(RunSignal::ABANDONED);
        }
    }
}

pub struct SpeedTestApp {
    rect: Rect,
    pointer: Point,
    phase: Phase,
    result: Option<ResultSet>,
    display_value: f64,
    ring_progress: f64,
    last_animation: Instant,
    started: Option<Instant>,
    run: Option<Arc<RunSignal>>,
    error: Option<String>,
    /// Every measurement taken, newest first.
    results: Vec<Measurement>,
    /// A screenshot holds still on the staged values.
    shot: bool,
    dispatcher: Option<Dispatcher<SpeedTest>>,
}

impl SpeedTestApp {
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            pointer: Point::ZERO,
            phase: Phase::Idle,
            result: None,
            display_value: 0.0,
            ring_progress: 0.0,
            last_animation: ui::time::now(),
            started: None,
            run: None,
            error: None,
            results: Vec::new(),
            shot: false,
            dispatcher: None,
        }
    }

    /// Begin a run: the signal its worker reports through.
    fn begin(&mut self) -> Arc<RunSignal> {
        self.stop();
        let signal = Arc::new(RunSignal::default());
        self.run = Some(Arc::clone(&signal));
        self.phase = Phase::Ping;
        self.display_value = 0.0;
        self.started = Some(ui::time::now());
        self.last_animation = ui::time::now();
        signal
    }

    /// Stop the run in flight; its worker notices and its result is dropped.
    fn stop(&mut self) {
        if let Some(run) = self.run.take() {
            run.stop.store(true, Ordering::Release);
        }
        self.phase = Phase::Idle;
        self.result = None;
        self.error = None;
        self.started = None;
    }

    /// The run `signal` answered: record what it measured.
    fn finish(
        &mut self,
        signal: &Arc<RunSignal>,
        answer: Result<Run, String>,
        cx: &EffectCx<SpeedTest, Self>,
    ) -> ui::framework::EffectAnswer {
        if !self
            .run
            .as_ref()
            .is_some_and(|run| Arc::ptr_eq(run, signal))
        {
            return Err("the test was stopped".into());
        }
        self.run = None;
        self.started = None;
        match answer {
            Ok(Run { result, server }) => {
                self.result = Some(result);
                self.display_value = result.download;
                self.phase = Phase::Complete;
                cx.invoke(SpeedTest::RECORD.with((
                    result.ping,
                    result.jitter,
                    result.download,
                    result.upload,
                    server.clone(),
                )));
                Ok(Effected::value(ui::serde_json::json!({
                    "ping_ms": result.ping,
                    "jitter_ms": result.jitter,
                    "download_mbps": result.download,
                    "upload_mbps": result.upload,
                    "server": server,
                })))
            }
            Err(message) => {
                self.error = Some(message.clone());
                self.phase = Phase::Error;
                Err(message)
            }
        }
    }

    /// Start or stop, as the primary button does: through the same door as
    /// the palette and agents.
    fn toggle_test(&self) {
        let Some(dispatcher) = &self.dispatcher else {
            return;
        };
        if self.is_running() {
            dispatcher.effect(&SpeedEffect::Stop);
        } else {
            dispatcher.effect(&SpeedEffect::Start);
        }
    }

    /// The measurements below the dial: all of them, except the one the
    /// dial shows.
    fn history(&self) -> &[Measurement] {
        if self.result.is_some() {
            self.results.get(1..).unwrap_or_default()
        } else {
            &self.results
        }
    }

    /// The server the last test reached.
    fn server_label(&self) -> &str {
        self.results
            .first()
            .map_or("Nearest Cloudflare server", |latest| latest.server.as_str())
    }

    fn primary_rect(&self) -> Rect {
        let wide = self.rect.width() >= 850.0;
        let cx = if wide {
            self.rect.x0 + self.rect.width() * 0.36
        } else {
            self.rect.center().x
        };
        let y = if wide {
            if self.rect.height() >= 680.0 {
                self.rect.y1 - 70.0
            } else {
                self.rect.y1 - 48.0
            }
        } else {
            self.rect.y0 + 450.0
        };
        Rect::new(cx - 92.0, y - 25.0, cx + 92.0, y + 25.0)
    }

    fn server_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + 28.0,
            self.rect.y0 + 24.0,
            self.rect.x0 + 250.0,
            self.rect.y0 + 62.0,
        )
    }

    fn dial_center(&self) -> Point {
        let wide = self.rect.width() >= 850.0;
        Point::new(
            if wide {
                self.rect.x0 + self.rect.width() * 0.36
            } else {
                self.rect.center().x
            },
            if wide {
                if self.rect.height() >= 650.0 {
                    self.rect.y0 + 326.0
                } else {
                    self.rect.y0 + self.rect.height() * 0.47
                }
            } else {
                self.rect.y0 + 260.0
            },
        )
    }

    fn dial_radius(&self) -> f64 {
        if self.rect.width() < 550.0 {
            128.0
        } else if self.rect.width() < 850.0 {
            132.0
        } else if self.rect.height() < 650.0 {
            ((self.rect.height() - 250.0) / 2.0).clamp(104.0, 145.0)
        } else {
            168.0
        }
    }

    fn is_running(&self) -> bool {
        matches!(self.phase, Phase::Ping | Phase::Download | Phase::Upload)
    }

    fn phase_label(&self) -> &'static str {
        match self.phase {
            Phase::Idle => "Ready when you are",
            Phase::Ping => "Finding the quickest route",
            Phase::Download => "Measuring download",
            Phase::Upload => "Measuring upload",
            Phase::Complete => "Connection looks great",
            Phase::Error => "Test interrupted",
        }
    }
}

impl Default for SpeedTestApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for SpeedTestApp {
    fn update(&mut self, _doc: &mut AppModel, _path: &ModelPath) -> bool {
        let mut changed = false;
        if let Some(run) = &self.run {
            let phase = match run.phase() {
                RunSignal::PING => Some(Phase::Ping),
                RunSignal::DOWNLOAD => Some(Phase::Download),
                RunSignal::UPLOAD => Some(Phase::Upload),
                RunSignal::ABANDONED => None,
                _ => Some(self.phase),
            };
            match phase {
                Some(phase) if phase != self.phase => {
                    self.phase = phase;
                    changed = true;
                }
                Some(_) => {}
                None => {
                    self.stop();
                    changed = true;
                }
            }
        }
        if self.is_running() && !self.shot {
            let elapsed = self
                .started
                .map_or(0.0, |start| start.elapsed().as_secs_f64());
            self.display_value = match self.phase {
                Phase::Ping => 8.0 + elapsed.sin().abs() * 7.0,
                Phase::Download => 420.0 + (elapsed * 2.7).sin() * 110.0,
                Phase::Upload => 95.0 + (elapsed * 3.1).sin() * 24.0,
                _ => self.display_value,
            };
            changed = true;
        }
        let now = ui::time::now();
        let delta = now
            .duration_since(self.last_animation)
            .as_secs_f64()
            .min(0.05);
        self.last_animation = now;
        let target = match self.phase {
            Phase::Idle | Phase::Error => 0.0,
            Phase::Ping => (self.display_value / 50.0).clamp(0.08, 0.92),
            Phase::Download => (self.display_value / 750.0).clamp(0.06, 1.0),
            Phase::Upload => (self.display_value / 180.0).clamp(0.06, 1.0),
            Phase::Complete => self
                .result
                .map_or(0.0, |result| (result.download / 800.0).clamp(0.08, 1.0)),
        };
        let eased = 1.0 - (-delta * 9.0).exp();
        let previous = self.ring_progress;
        self.ring_progress += (target - self.ring_progress) * eased;
        changed |= (previous - self.ring_progress).abs() > 0.0005;
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x + layout.size.width),
            origin.y + f64::from(layout.location.y + layout.size.height),
        );
    }

    #[expect(
        clippy::too_many_lines,
        reason = "declarative paint of the whole window, read top to bottom"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &self.rect);

        let server = self.server_rect();
        tr.draw_text_weighted(
            scene,
            self.server_label(),
            theme.tokens.typography.scale.sm,
            c.text_primary,
            transform * Affine::translate((server.x0, server.y0 + 8.0)),
            Some(server.width() as f32),
            TextWeight::Semibold,
        );
        tr.draw_text(
            scene,
            "Server",
            theme.tokens.typography.scale.xs,
            c.text_dim,
            transform * Affine::translate((server.x0, server.y0 - 15.0)),
            None,
        );

        let center = self.dial_center();
        let radius = self.dial_radius();
        if self.rect.width() >= 850.0 && self.rect.height() >= 650.0 {
            let heading = "NETWORK PERFORMANCE";
            let (heading_width, _) = tr.measure_weighted(
                heading,
                theme.tokens.typography.scale.xs,
                None,
                TextWeight::Semibold,
            );
            tr.draw_text_weighted(
                scene,
                heading,
                theme.tokens.typography.scale.xs,
                c.text_dim,
                transform
                    * Affine::translate((
                        center.x - f64::from(heading_width) / 2.0,
                        self.rect.y0 + 92.0,
                    )),
                None,
                TextWeight::Semibold,
            );
            let stages = ["PING", "DOWNLOAD", "UPLOAD"];
            for (index, stage) in stages.iter().enumerate() {
                let stage_x = center.x - 148.0 + index as f64 * 148.0;
                let reached = index
                    <= match self.phase {
                        Phase::Idle | Phase::Ping | Phase::Error => 0,
                        Phase::Download => 1,
                        Phase::Upload | Phase::Complete => 2,
                    };
                scene.fill(
                    Fill::NonZero,
                    transform,
                    if reached {
                        theme.accent_primary
                    } else {
                        c.surfaces.raised
                    },
                    None,
                    &Circle::new((stage_x, self.rect.y0 + 124.0), 4.0),
                );
                tr.draw_text(
                    scene,
                    stage,
                    theme.tokens.typography.scale.xs,
                    if reached { c.text_primary } else { c.text_dim },
                    transform * Affine::translate((stage_x + 10.0, self.rect.y0 + 116.0)),
                    None,
                );
            }
        }
        for index in 0..=10 {
            let angle = -2.45 + 4.9 * f64::from(index) / 10.0;
            scene.fill(
                Fill::NonZero,
                transform,
                c.text_dim.with_alpha_factor(0.45),
                None,
                &Circle::new(
                    (
                        center.x + (radius + 28.0) * angle.cos(),
                        center.y + (radius + 28.0) * angle.sin(),
                    ),
                    if index % 5 == 0 { 2.8 } else { 1.7 },
                ),
            );
        }
        let ring = Sweep::new(center, (radius, radius), -2.45, 4.9, 0.0);
        scene.stroke(
            &Stroke::new(15.0),
            transform,
            c.surfaces.raised,
            None,
            &ring,
        );
        let progress = self.ring_progress.max(0.015);
        let active = Sweep::new(center, (radius, radius), -2.45, 4.9 * progress, 0.0);
        scene.stroke(
            &Stroke::new(15.0),
            transform,
            theme.accent_primary,
            None,
            &active,
        );
        let dot_angle = -2.45 + 4.9 * progress;
        scene.fill(
            Fill::NonZero,
            transform,
            theme.accent_primary,
            None,
            &Circle::new(
                (
                    center.x + radius * dot_angle.cos(),
                    center.y + radius * dot_angle.sin(),
                ),
                10.0,
            ),
        );

        let unit = match self.phase {
            Phase::Ping => "ms",
            _ => "Mbps",
        };
        let (unit_width, _) = tr.measure_weighted(
            unit,
            theme.tokens.typography.scale.sm,
            None,
            TextWeight::Regular,
        );
        let value = if self.phase == Phase::Idle {
            "—".to_string()
        } else {
            format!("{:.1}", self.display_value)
        };
        let (value_width, _) = tr.measure_weighted(
            &value,
            theme.tokens.typography.scale.x4l,
            None,
            TextWeight::Bold,
        );
        let value_x = center.x - f64::from(value_width) / 2.0;
        let (status_width, _) = tr.measure_weighted(
            self.phase_label(),
            theme.tokens.typography.scale.base,
            None,
            TextWeight::Semibold,
        );
        tr.draw_text_weighted(
            scene,
            &value,
            theme.tokens.typography.scale.x4l,
            c.text_primary,
            transform * Affine::translate((value_x, center.y - 34.0)),
            None,
            TextWeight::Bold,
        );
        tr.draw_text(
            scene,
            unit,
            theme.tokens.typography.scale.sm,
            c.text_dim,
            transform
                * Affine::translate((center.x - f64::from(unit_width) / 2.0, center.y + 23.0)),
            None,
        );
        if self.phase != Phase::Idle {
            let scale_label = match self.phase {
                Phase::Ping => "0 — 50 ms",
                Phase::Upload => "0 — 180 Mbps",
                _ => "0 — 800 Mbps",
            };
            let (scale_width, _) = tr.measure_weighted(
                scale_label,
                theme.tokens.typography.scale.xs,
                None,
                TextWeight::Regular,
            );
            tr.draw_text(
                scene,
                scale_label,
                theme.tokens.typography.scale.xs,
                c.text_dim,
                transform
                    * Affine::translate((center.x - f64::from(scale_width) / 2.0, center.y + 43.0)),
                None,
            );
        }
        tr.draw_text_weighted(
            scene,
            self.phase_label(),
            theme.tokens.typography.scale.base,
            c.text_primary,
            transform
                * Affine::translate((center.x - f64::from(status_width) / 2.0, center.y + 76.0)),
            None,
            TextWeight::Semibold,
        );

        let primary = self.primary_rect();
        let label = if self.is_running() {
            "Stop test"
        } else {
            "Run speed test"
        };
        Button::paint(
            scene,
            transform,
            tr,
            theme,
            &ButtonFace::new(primary, label)
                .primary()
                .hovered(primary.contains(self.pointer)),
        );

        let wide = self.rect.width() >= 850.0;
        if wide && self.rect.height() >= 680.0 {
            let facts = [
                ("CONNECTION", "Wired"),
                ("TEST SERVER", "Cloudflare edge"),
                (
                    "DATA USED",
                    if self.result.is_some() {
                        "12 MB"
                    } else {
                        "—"
                    },
                ),
            ];
            // Facts and the primary action occupy separate reserved rows.
            // Keep a full text line plus 28 px of breathing room above the
            // button rather than relying on the font's visual bounds.
            let fact_y = self.rect.y1 - 196.0;
            for (index, (label, value)) in facts.iter().enumerate() {
                let x = center.x - 205.0 + index as f64 * 142.0;
                tr.draw_text(
                    scene,
                    label,
                    theme.tokens.typography.scale.xs,
                    c.text_dim,
                    transform * Affine::translate((x, fact_y)),
                    None,
                );
                tr.draw_text_weighted(
                    scene,
                    value,
                    theme.tokens.typography.scale.sm,
                    c.text_primary,
                    transform * Affine::translate((x, fact_y + 24.0)),
                    Some(126.0),
                    TextWeight::Semibold,
                );
            }
        }

        if wide {
            self.draw_details(
                scene,
                transform,
                tr,
                theme,
                Rect::new(
                    self.rect.x0 + self.rect.width() * 0.65,
                    self.rect.y0 + 92.0,
                    self.rect.x1 - 34.0,
                    self.rect.y1 - 42.0,
                ),
            );
        } else if self.rect.height() > 650.0 {
            self.draw_details(
                scene,
                transform,
                tr,
                theme,
                Rect::new(
                    self.rect.x0 + 22.0,
                    self.rect.y0 + 490.0,
                    self.rect.x1 - 22.0,
                    self.rect.y1 - 20.0,
                ),
            );
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            }
        ) && self.primary_rect().contains(self.pointer)
        {
            self.toggle_test();
            *ctx.handled = true;
        }
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let mut root = AccessNode::new(Role::Window).with_name("Speed Test");
        root.children = vec![
            AccessNode::new(Role::Button)
                .with_action_target("speed-test.start")
                .with_name(if self.is_running() {
                    "Stop test"
                } else {
                    "Run speed test"
                })
                .with_state(AccessState {
                    focusable: true,
                    ..Default::default()
                }),
        ];
        Some(root)
    }

    fn perform_action_at(
        &mut self,
        action: &ui::a11y::Action,
        key_path: &[u64],
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        if !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        match key_path.last() {
            Some(key) if *key == ui::a11y::hash_key(&"speed-test.start") => {
                self.toggle_test();
                true
            }
            _ => false,
        }
    }

    fn name(&self) -> &'static str {
        "SpeedTestApp"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
    fn next_wake_deadline(&self) -> Option<Instant> {
        let target = match self.phase {
            Phase::Idle | Phase::Error => 0.0,
            Phase::Ping => (self.display_value / 50.0).clamp(0.08, 0.92),
            Phase::Download => (self.display_value / 750.0).clamp(0.06, 1.0),
            Phase::Upload => (self.display_value / 180.0).clamp(0.06, 1.0),
            Phase::Complete => self
                .result
                .map_or(0.0, |result| (result.download / 800.0).clamp(0.08, 1.0)),
        };
        (self.is_running() || (self.ring_progress - target).abs() > 0.001)
            .then(|| ui::time::now() + Duration::from_millis(16))
    }
}

impl ResourceSurface<SpeedTest, SpeedEffect> for SpeedTestApp {
    fn present(&mut self, state: &Rc<SpeedTest>) {
        self.results = state.newest_first().cloned().collect();
    }

    fn attach(&mut self, cx: SurfaceCx<SpeedTest>) {
        self.dispatcher = Some(cx.dispatcher());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.shot = true;
        match scenario {
            Some("idle") => {}
            Some("running") => {
                self.phase = Phase::Download;
                self.display_value = 386.4;
                self.ring_progress = 0.58;
            }
            _ => {
                let Some(latest) = self.results.first() else {
                    return;
                };
                let result = ResultSet {
                    ping: latest.ping,
                    jitter: latest.jitter,
                    download: latest.download,
                    upload: latest.upload,
                };
                self.phase = Phase::Complete;
                self.display_value = result.download;
                self.ring_progress = 0.79;
                self.result = Some(result);
            }
        }
    }

    fn effect(
        &mut self,
        effect: SpeedEffect,
        cx: &mut EffectCx<SpeedTest, Self>,
    ) -> ui::framework::EffectAnswer {
        match effect {
            SpeedEffect::Start => {
                if self.is_running() {
                    return Err("a test is already running".into());
                }
                let signal = self.begin();
                let worker = Arc::clone(&signal);
                Ok(cx.spawn(
                    move |progress| run_measurement(worker, progress),
                    move |app: &mut Self, answer, cx| app.finish(&signal, answer, cx),
                ))
            }
            SpeedEffect::Stop => {
                if !self.is_running() {
                    return Err("no test is running".into());
                }
                self.stop();
                Ok(Effected::done())
            }
        }
    }

    fn effects(&self) -> Vec<EffectStatus> {
        if self.is_running() {
            vec![EffectStatus::unavailable(
                SpeedEffect::START,
                "running",
                "A test is running",
            )]
        } else {
            vec![EffectStatus::unavailable(
                SpeedEffect::STOP,
                "idle",
                "No test is running",
            )]
        }
    }
}

impl SpeedTestApp {
    #[expect(
        clippy::too_many_lines,
        reason = "declarative paint of the whole results panel, read top to bottom"
    )]
    fn draw_details(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        let c = &theme.tokens.colors;
        scene.fill(
            Fill::NonZero,
            transform,
            c.surfaces.raised,
            None,
            &RoundedRect::from_rect(area, f64::from(theme.tokens.radii.lg)),
        );
        tr.draw_text_weighted(
            scene,
            "Your connection",
            theme.tokens.typography.scale.lg,
            c.text_primary,
            transform * Affine::translate((area.x0 + 24.0, area.y0 + 24.0)),
            None,
            TextWeight::Semibold,
        );
        let result = self.result.unwrap_or(ResultSet {
            ping: 0.0,
            jitter: 0.0,
            download: 0.0,
            upload: 0.0,
        });
        let cards = [
            ("PING", result.ping, "ms"),
            ("DOWNLOAD", result.download, "Mbps"),
            ("UPLOAD", result.upload, "Mbps"),
        ];
        let card_w = (area.width() - 64.0) / 3.0;
        for (index, (label, value, unit)) in cards.iter().enumerate() {
            let x = area.x0 + 20.0 + index as f64 * (card_w + 12.0);
            scene.fill(
                Fill::NonZero,
                transform,
                c.surfaces.base,
                None,
                &RoundedRect::from_rect(
                    Rect::new(x - 8.0, area.y0 + 58.0, x + card_w - 4.0, area.y0 + 158.0),
                    f64::from(theme.tokens.radii.md),
                ),
            );
            tr.draw_text(
                scene,
                label,
                theme.tokens.typography.scale.xs,
                c.text_dim,
                transform * Affine::translate((x, area.y0 + 75.0)),
                None,
            );
            tr.draw_text_weighted(
                scene,
                &if self.result.is_some() {
                    format!("{value:.1}")
                } else {
                    "—".into()
                },
                theme.tokens.typography.scale.xl,
                c.text_primary,
                transform * Affine::translate((x, area.y0 + 101.0)),
                Some(card_w as f32),
                TextWeight::Bold,
            );
            tr.draw_text(
                scene,
                unit,
                theme.tokens.typography.scale.xs,
                c.text_dim,
                transform * Affine::translate((x, area.y0 + 136.0)),
                None,
            );
        }
        tr.draw_text(
            scene,
            &format!(
                "Jitter {:.1} ms  ·  Cloudflare edge  ·  IPv4",
                result.jitter
            ),
            theme.tokens.typography.scale.xs,
            c.text_dim,
            transform * Affine::translate((area.x0 + 20.0, area.y0 + 178.0)),
            Some((area.width() - 40.0) as f32),
        );
        tr.draw_text_weighted(
            scene,
            "Recent tests",
            theme.tokens.typography.scale.sm,
            c.text_primary,
            transform * Affine::translate((area.x0 + 20.0, area.y0 + 224.0)),
            None,
            TextWeight::Semibold,
        );
        let history = self.history();
        if history.is_empty() {
            tr.draw_text(
                scene,
                "Your latest results will appear here",
                theme.tokens.typography.scale.sm,
                c.text_dim,
                transform * Affine::translate((area.x0 + 20.0, area.y0 + 260.0)),
                None,
            );
        } else {
            let visible_rows = if area.height() < 400.0 { 1 } else { 3 };
            let now = ui::time::wall();
            for (index, item) in history.iter().take(visible_rows).enumerate() {
                let y = area.y0 + 262.0 + index as f64 * 52.0;
                let at = ui::time::UNIX_EPOCH
                    + Duration::from_millis(u64::try_from(item.at_ms).unwrap_or_default());
                tr.draw_text(
                    scene,
                    &format!(
                        "{}  ·  {}",
                        ui::widget::history::format_relative_time(at, now),
                        item.server
                    ),
                    theme.tokens.typography.scale.sm,
                    c.text_primary,
                    transform * Affine::translate((area.x0 + 20.0, y)),
                    None,
                );
                tr.draw_text(
                    scene,
                    &format!(
                        "Down {:.0}  ·  Up {:.0} Mbps  ·  {:.0} ms",
                        item.download, item.upload, item.ping
                    ),
                    theme.tokens.typography.scale.xs,
                    c.text_dim,
                    transform * Affine::translate((area.x0 + 20.0, y + 23.0)),
                    None,
                );
            }
        }
        if area.height() > 500.0 {
            let section_y = area.y1 - 160.0;
            let complete = self.result.is_some();
            tr.draw_text_weighted(
                scene,
                if complete {
                    "Connection quality"
                } else {
                    "Test progress"
                },
                theme.tokens.typography.scale.sm,
                c.text_primary,
                transform * Affine::translate((area.x0 + 20.0, section_y)),
                None,
                TextWeight::Semibold,
            );
            let checks = if complete {
                let result = self.result.unwrap_or(ResultSet {
                    ping: 0.0,
                    jitter: 0.0,
                    download: 0.0,
                    upload: 0.0,
                });
                [
                    (
                        "Video calls",
                        if result.upload >= 8.0 && result.ping <= 80.0 {
                            "Excellent"
                        } else if result.upload >= 3.0 {
                            "Good"
                        } else {
                            "Limited"
                        },
                        true,
                    ),
                    (
                        "4K streaming",
                        if result.download >= 50.0 {
                            "Excellent"
                        } else if result.download >= 25.0 {
                            "Good"
                        } else {
                            "Limited"
                        },
                        true,
                    ),
                    (
                        "Cloud gaming",
                        if result.ping <= 20.0 && result.jitter <= 5.0 {
                            "Excellent"
                        } else if result.ping <= 50.0 {
                            "Good"
                        } else {
                            "Limited"
                        },
                        true,
                    ),
                ]
            } else {
                [
                    (
                        "Latency",
                        if matches!(self.phase, Phase::Ping) {
                            "Measuring"
                        } else if self.is_running() {
                            "Complete"
                        } else {
                            "Waiting"
                        },
                        self.is_running(),
                    ),
                    (
                        "Download",
                        if matches!(self.phase, Phase::Download) {
                            "Measuring"
                        } else {
                            "Waiting"
                        },
                        matches!(self.phase, Phase::Download | Phase::Upload),
                    ),
                    (
                        "Upload",
                        if matches!(self.phase, Phase::Upload) {
                            "Measuring"
                        } else {
                            "Waiting"
                        },
                        matches!(self.phase, Phase::Upload),
                    ),
                ]
            };
            for (index, (name, quality, active)) in checks.iter().enumerate() {
                let row_y = section_y + 34.0 + index as f64 * 34.0;
                scene.fill(
                    Fill::NonZero,
                    transform,
                    c.surfaces.base,
                    None,
                    &RoundedRect::from_rect(
                        Rect::new(area.x0 + 20.0, row_y, area.x1 - 20.0, row_y + 27.0),
                        f64::from(theme.tokens.radii.sm),
                    ),
                );
                tr.draw_text(
                    scene,
                    name,
                    theme.tokens.typography.scale.xs,
                    c.text_primary,
                    transform * Affine::translate((area.x0 + 30.0, row_y + 6.0)),
                    None,
                );
                let (quality_width, _) = tr.measure_weighted(
                    quality,
                    theme.tokens.typography.scale.xs,
                    None,
                    TextWeight::Semibold,
                );
                tr.draw_text_weighted(
                    scene,
                    quality,
                    theme.tokens.typography.scale.xs,
                    if *active {
                        theme.accent_primary
                    } else {
                        c.text_dim
                    },
                    transform
                        * Affine::translate((
                            area.x1 - 30.0 - f64::from(quality_width),
                            row_y + 6.0,
                        )),
                    None,
                    TextWeight::Semibold,
                );
            }
        }
    }
}

/// Measure, reporting each phase to the surface and the effect's caller.
async fn run_measurement(signal: Arc<RunSignal>, progress: Progress) -> Result<Run, String> {
    let answers = Answers(signal);
    let answer = measure(&answers.0, &progress).await;
    answers.0.set(RunSignal::ANSWERED);
    answer
}

/// Why a run ends early: stopped from the app, or cancelled by its caller.
fn stopped(signal: &RunSignal, progress: &Progress) -> Result<(), String> {
    if signal.stopped() || progress.is_cancelled() {
        return Err("the test was stopped".into());
    }
    Ok(())
}

async fn measure(signal: &RunSignal, progress: &Progress) -> Result<Run, String> {
    progress.report("ping", Some(0), Some(3));
    let client = tilo_net::Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent("Tilo Speed Test/0.1")
        .build()
        .map_err(|error| error.to_string())?;
    let ping_start = ui::time::now();
    let reached = client
        .get("https://speed.cloudflare.com/__down?bytes=1")
        .send()
        .await
        .and_then(tilo_net::Response::error_for_status)
        .map_err(|error| format!("Could not reach the test server: {error}"))?;
    let ping = ping_start.elapsed().as_secs_f64() * 1000.0;
    // The data centre that answered, by its airport code.
    let server = reached
        .headers()
        .get("cf-meta-colo")
        .and_then(|colo| colo.to_str().ok())
        .map_or_else(
            || "Cloudflare".to_owned(),
            |colo| format!("Cloudflare · {colo}"),
        );

    stopped(signal, progress)?;
    signal.set(RunSignal::DOWNLOAD);
    progress.report("download", Some(1), Some(3));
    let download_start = ui::time::now();
    let mut response = client
        .get("https://speed.cloudflare.com/__down?bytes=10000000")
        .send()
        .await
        .map_err(|error| format!("Download test failed: {error}"))?;
    let mut bytes = 0_usize;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("Download test failed: {error}"))?
    {
        stopped(signal, progress)?;
        bytes += chunk.len();
    }
    let download = bytes as f64 * 8.0 / download_start.elapsed().as_secs_f64() / 1_000_000.0;

    signal.set(RunSignal::UPLOAD);
    progress.report("upload", Some(2), Some(3));
    let upload_body = vec![0_u8; 2_000_000];
    let upload_start = ui::time::now();
    client
        .post("https://speed.cloudflare.com/__up")
        .body(upload_body)
        .send()
        .await
        .map_err(|error| format!("Upload test failed: {error}"))?;
    stopped(signal, progress)?;
    let upload = 16.0 / upload_start.elapsed().as_secs_f64();
    Ok(Run {
        result: ResultSet {
            ping,
            jitter: ping * 0.08,
            download,
            upload,
        },
        server,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::SpeedTestShell;
    use ui::framework::ResourceApp;
    use ui::prelude::{Replica, TestApp};
    use ui::tilo_actions::{Availability, DoorError};

    fn availability(app: &mut TestApp<SpeedTestShell>, id: &str) -> Availability {
        app.harness()
            .resources()
            .iter()
            .flat_map(|resource| &resource.actions)
            .find(|action| action.id == id)
            .map_or_else(
                || panic!("{id} is published"),
                |action| action.contract.availability.clone(),
            )
    }

    #[test]
    fn start_and_stop_are_published_as_the_run_stands() {
        let mut app = TestApp::<SpeedTestShell>::new().expect("the app opens");
        assert_eq!(availability(&mut app, "start"), Availability::Available);
        assert!(matches!(
            availability(&mut app, "stop"),
            Availability::Unavailable { code, .. } if code == "idle"
        ));
        let refused = app.effect(&SpeedEffect::Stop);
        assert!(
            matches!(&refused, Err(DoorError::Unavailable { code, .. }) if code == "idle"),
            "{refused:?}"
        );
        app.invoke(SpeedTest::RECORD.with((9.0, 1.0, 500.0, 90.0, "Cloudflare · FRA".into())))
            .expect("a run is recorded");
        let app = app.reopen().expect("the app reopens");
        assert_eq!(app.state().results.len(), 1, "measurements are kept");
    }

    #[test]
    fn a_finished_run_shows_on_the_dial_and_the_earlier_ones_below_it() {
        let mut replica = Replica::<SpeedTest>::new();
        for invoke in SpeedTestShell::scenario(Some("complete")) {
            replica.apply(invoke).expect("record");
        }
        let mut app = SpeedTestApp::new();
        ResourceSurface::<SpeedTest, SpeedEffect>::present(
            &mut app,
            &Rc::new(replica.state().expect("state")),
        );
        assert_eq!(
            app.history().len(),
            3,
            "an idle dial leaves every run below"
        );
        assert_eq!(app.server_label(), "Cloudflare · LHR");

        ResourceSurface::<SpeedTest, SpeedEffect>::stage(&mut app, Some("complete"));
        let shown = app.result.expect("the newest run is on the dial");
        assert!((shown.download - 642.7).abs() < f64::EPSILON);
        let below: Vec<f64> = app.history().iter().map(|run| run.download).collect();
        assert_eq!(below, [618.2, 584.6]);
    }

    /// Neither the wide metadata row nor the stacked dial touches the primary action.
    #[test]
    fn metadata_and_dial_never_touch_the_primary_action() {
        // Case `wide_metadata_row_never_touches_primary_action`.
        {
            for height in [520.0, 640.0, 760.0, 900.0] {
                let mut app = SpeedTestApp::new();
                app.rect = Rect::new(0.0, 0.0, 1180.0, height);
                let facts_bottom = app.rect.y1 - 196.0 + 42.0;
                assert!(
                    facts_bottom + 28.0 <= app.primary_rect().y0,
                    "metadata and action overlap at {height}px"
                );
            }
        }

        // Case `stacked_dial_never_touches_primary_action`.
        {
            for width in [480.0, 560.0, 700.0, 849.0] {
                let mut app = SpeedTestApp::new();
                app.rect = Rect::new(0.0, 0.0, width, 820.0);
                let decorated_dial_bottom = app.dial_center().y + app.dial_radius() + 28.0;
                assert!(
                    decorated_dial_bottom + 5.0 <= app.primary_rect().y0,
                    "dial and action overlap at {width}px"
                );
            }
        }
    }
}
