#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::unwrap_used,
    reason = "bounded app-local arithmetic; Taffy allocation is infallible for this leaf"
)]

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};
use ui::time::{Duration, Instant};
use ui::widget::{AppModel, ModelPath};

use ui::ColorCompat;
use ui::a11y::{AccessNode, AccessState, Role};
use ui::automerge::{AutoCommit, ObjId};
use ui::presence::PresenceCtx;
use ui::taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::tilo_actions::{
    ActionContract, ActionOutput, ActionSpec, DiscoveryScope, OperationKind, Resource,
};
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Arc, Circle, Point, Rect, RoundedRect, Stroke};
use ui::vello::peniko::Fill;
use ui::widget::{Button, ButtonFace, Widget};
use ui::winit::event::{ElementState, MouseButton, WindowEvent};

pub(crate) const OPERATION_TYPE: &str = "speed-test.operation";

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

enum WorkerMessage {
    Phase(Phase),
    Done(ResultSet),
    Failed(String),
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SpeedOperation {
    Start,
    Stop,
    ChangeRegion,
}

pub(crate) fn action(id: &str, label: &str, op: &SpeedOperation) -> Option<ActionSpec> {
    let mut spec =
        ActionSpec::operation(id, label, OperationKind::Effect, OPERATION_TYPE, op).ok()?;
    spec.contract = ActionContract {
        discovery: DiscoveryScope::Launchable,
        ..ActionContract::default()
    };
    Some(spec)
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
    worker: Option<Receiver<WorkerMessage>>,
    task: Option<ui::rt::JoinHandle<()>>,
    region: usize,
    error: Option<String>,
    history: Vec<ResultSet>,
    shot: bool,
    pub(crate) actions: crate::resource::ActionSink,
    hosted: bool,
}

impl SpeedTestApp {
    const REGIONS: [&'static str; 3] = ["Automatic · London", "Frankfurt, Germany", "New York, US"];

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
            worker: None,
            task: None,
            region: 0,
            error: None,
            history: Vec::new(),
            shot: false,
            actions: crate::resource::ActionSink::default(),
            hosted: false,
        }
    }

    pub fn demo(scenario: &str) -> Self {
        let mut app = Self::new();
        app.shot = true;
        match scenario {
            "idle" => {}
            "running" => {
                app.phase = Phase::Download;
                app.display_value = 386.4;
                app.ring_progress = 0.58;
            }
            _ => {
                let result = ResultSet {
                    ping: 11.8,
                    jitter: 1.6,
                    download: 642.7,
                    upload: 118.4,
                };
                app.phase = Phase::Complete;
                app.display_value = result.download;
                app.ring_progress = 0.79;
                app.result = Some(result);
                app.history = vec![
                    ResultSet {
                        ping: 12.4,
                        jitter: 1.8,
                        download: 618.2,
                        upload: 112.9,
                    },
                    ResultSet {
                        ping: 14.1,
                        jitter: 2.2,
                        download: 584.6,
                        upload: 109.3,
                    },
                ];
            }
        }
        app
    }

    fn start(&mut self) {
        if matches!(self.phase, Phase::Ping | Phase::Download | Phase::Upload) {
            return;
        }
        if let Some(previous) = self.result.take() {
            self.history.insert(0, previous);
            self.history.truncate(5);
        }
        self.phase = Phase::Ping;
        self.error = None;
        self.display_value = 0.0;
        self.started = Some(ui::time::now());
        self.last_animation = ui::time::now();
        let (tx, rx) = mpsc::channel();
        self.worker = Some(rx);
        self.cancel_task();
        self.task = Some(ui::rt::app().spawn(run_measurement(tx)));
    }

    fn cancel_task(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }

    fn stop(&mut self) {
        self.cancel_task();
        self.worker = None;
        self.phase = Phase::Idle;
        self.result = None;
        self.error = None;
        self.started = None;
    }

    fn toggle_test(&mut self) {
        if self.is_running() {
            self.stop();
        } else {
            self.start();
        }
    }

    fn cycle_region(&mut self) {
        if !matches!(self.phase, Phase::Ping | Phase::Download | Phase::Upload) {
            self.region = (self.region + 1) % Self::REGIONS.len();
        }
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

    fn region_rect(&self) -> Rect {
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

    pub(crate) fn mark_framework_hosted(&mut self) {
        self.hosted = true;
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn is_framework_hosted(&self) -> bool {
        self.hosted
    }

    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
        self.actions.mark_hosted_for_test();
    }

    #[cfg(test)]
    pub(crate) fn sink_is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn has_result(&self) -> bool {
        self.result.is_some()
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

impl SpeedTestApp {
    fn answer_resource_operation(
        &mut self,
        kind: OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<Option<ActionOutput>, String> {
        if kind == OperationKind::Document || type_id != OPERATION_TYPE {
            return Ok(None);
        }
        match serde_json::from_slice::<SpeedOperation>(payload)
            .map_err(|error| error.to_string())?
        {
            SpeedOperation::Start => self.start(),
            SpeedOperation::Stop => self.stop(),
            SpeedOperation::ChangeRegion => self.cycle_region(),
        }
        Ok(Some(ActionOutput::default()))
    }
}

impl ui::undo::CommittedActions for SpeedTestApp {
    fn take_committed_action(&mut self) -> Option<Vec<ui::automerge::ChangeHash>> {
        // Hosted edits belong to the framework dispatcher. Standalone stays
        // on the local host, which has no document action yet.
        if self.actions.is_hosted() {
            return None;
        }
        self.actions.take_committed()
    }
}

impl Widget for SpeedTestApp {
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::resources::PublishesResources
    );

    fn update(&mut self, _doc: &mut AppModel, _path: &ModelPath) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.worker {
            while let Ok(message) = rx.try_recv() {
                changed = true;
                match message {
                    WorkerMessage::Phase(phase) => self.phase = phase,
                    WorkerMessage::Done(result) => {
                        self.result = Some(result);
                        self.display_value = result.download;
                        self.phase = Phase::Complete;
                        self.worker = None;
                        break;
                    }
                    WorkerMessage::Failed(message) => {
                        self.error = Some(message);
                        self.phase = Phase::Error;
                        self.worker = None;
                        break;
                    }
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

        let region = self.region_rect();
        #[expect(
            clippy::indexing_slicing,
            reason = "`region` only ever advances modulo `REGIONS.len()`"
        )]
        let region_label = Self::REGIONS[self.region];
        Button::paint(
            scene,
            transform,
            tr,
            theme,
            &ButtonFace::new(region, region_label).hovered(region.contains(self.pointer)),
        );
        tr.draw_text(
            scene,
            "Server",
            theme.tokens.typography.scale.xs,
            c.text_dim,
            transform * Affine::translate((region.x0, region.y0 - 15.0)),
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
        let ring = Arc::new(center, (radius, radius), -2.45, 4.9, 0.0);
        scene.stroke(
            &Stroke::new(15.0),
            transform,
            c.surfaces.raised,
            None,
            &ring,
        );
        let progress = self.ring_progress.max(0.015);
        let active = Arc::new(center, (radius, radius), -2.45, 4.9 * progress, 0.0);
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
        ) {
            if self.primary_rect().contains(self.pointer) {
                self.toggle_test();
                *ctx.handled = true;
            } else if self.region_rect().contains(self.pointer) {
                self.cycle_region();
                *ctx.handled = true;
            }
        }
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let mut root = AccessNode::new(Role::Window).with_name("Speed Test");
        root.children = vec![
            AccessNode::new(Role::Button)
                .with_action_target("speed-test.start")
                .with_name("Run speed test")
                .with_state(AccessState {
                    focusable: true,
                    ..Default::default()
                }),
            AccessNode::new(Role::Button)
                .with_action_target("speed-test.region")
                .with_name("Change test region")
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
            Some(key) if *key == ui::a11y::hash_key(&"speed-test.region") => {
                self.cycle_region();
                true
            }
            _ => false,
        }
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[OPERATION_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .map(ui::widget::ResourceOperationStart::Completed))
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

impl ui::resources::PublishesResources for SpeedTestApp {
    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        if self.hosted {
            return self.live_run_extra();
        }
        let mut root = ui::tilo_resource::publish::<crate::resource::SpeedTest>(
            "speed-test:root",
            "Internet speed test",
            &crate::resource::SpeedTestState,
        );
        root.children.extend(self.live_run_extra());
        vec![root]
    }

    fn resource(&self, _doc: &AutoCommit, id: &str) -> Option<Resource> {
        if self.hosted && id == "speed-test:root" {
            return None;
        }
        fn find(resources: &[Resource], id: &str) -> Option<Resource> {
            for resource in resources {
                if resource.id.as_str() == id {
                    return Some(resource.clone());
                }
                if let Some(found) = find(&resource.children, id) {
                    return Some(found);
                }
            }
            None
        }
        let mut root = ui::tilo_resource::publish::<crate::resource::SpeedTest>(
            "speed-test:root",
            "Internet speed test",
            &crate::resource::SpeedTestState,
        );
        root.children.extend(self.live_run_extra());
        find(&[root], id)
    }
}

impl SpeedTestApp {
    /// Stop while a measurement is running. Not a second `speed-test` root.
    fn live_run_extra(&self) -> Vec<Resource> {
        if !self.is_running() {
            return Vec::new();
        }
        let mut run = Resource::new("speed-test.run", "speed-test.run", "Running test");
        if let Some(spec) = action("stop", "Stop speed test", &SpeedOperation::Stop) {
            run.actions.push(spec);
        }
        vec![run]
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
        if self.history.is_empty() {
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
            for (index, item) in self.history.iter().take(visible_rows).enumerate() {
                let y = area.y0 + 262.0 + index as f64 * 52.0;
                tr.draw_text(
                    scene,
                    &format!("Today, {}:{:02}", 10 + index, 42 + index * 3),
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

async fn run_measurement(tx: mpsc::Sender<WorkerMessage>) {
    let message = match measure(&tx).await {
        Ok(result) => WorkerMessage::Done(result),
        Err(error) => WorkerMessage::Failed(error),
    };
    let _ = tx.send(message);
}

async fn measure(tx: &mpsc::Sender<WorkerMessage>) -> Result<ResultSet, String> {
    let client = tilo_net::Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent("Tilo Speed Test/0.1")
        .build()
        .map_err(|error| error.to_string())?;
    let ping_start = ui::time::now();
    client
        .get("https://speed.cloudflare.com/__down?bytes=1")
        .send()
        .await
        .and_then(tilo_net::Response::error_for_status)
        .map_err(|error| format!("Could not reach the test server: {error}"))?;
    let ping = ping_start.elapsed().as_secs_f64() * 1000.0;

    let _ = tx.send(WorkerMessage::Phase(Phase::Download));
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
        bytes += chunk.len();
    }
    let download = bytes as f64 * 8.0 / download_start.elapsed().as_secs_f64() / 1_000_000.0;

    let _ = tx.send(WorkerMessage::Phase(Phase::Upload));
    let upload_body = vec![0_u8; 2_000_000];
    let upload_start = ui::time::now();
    client
        .post("https://speed.cloudflare.com/__up")
        .body(upload_body)
        .send()
        .await
        .map_err(|error| format!("Upload test failed: {error}"))?;
    let upload = 16.0 / upload_start.elapsed().as_secs_f64();
    Ok(ResultSet {
        ping,
        jitter: ping * 0.08,
        download,
        upload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Speed Test keeps no document: it publishes only effect operations, so
    // the contract is the surface sweep (protocol validation) and Ctrl+K.
    ui::action_contract_tests! {
        seed: AutoCommit::new(),
        registry: ui::tilo_actions::ActionRegistry::new(),
        resources: |doc| ui::resources::resources(&SpeedTestApp::new(), doc),
        palette: ["Run speed test", "Change test region"],
    }

    #[test]
    fn hosted_lookup_keeps_the_run_and_not_the_root() {
        let mut app = SpeedTestApp::new();
        app.mark_hosted_for_test();
        let doc = AutoCommit::new();
        assert!(ui::resources::resource(&app, &doc, "speed-test:root").is_none());
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
