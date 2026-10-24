#![allow(
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    reason = "bounded app-local arithmetic; Taffy allocation is infallible for this leaf"
)]

use crate::view::{self, ScrollOffset, Shared};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use ui::framework::Actions;
use ui::tilo_resource::Invoke;
use ui::time::{Duration, Instant};
use ui::widget::Element;
use ui::widget::{AppModel, ModelPath};

use ui::framework::{
    Dispatcher, EffectCx, EffectStatus, Effected, Progress, ResourceSurface, SurfaceCx,
};
use ui::presence::PresenceCtx;
use ui::taffy::{NodeId, TaffyTree};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Point, Rect};
use ui::widget::Widget;
use ui::winit::event::WindowEvent;

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
    bytes: u64,
    result: ResultSet,
    server: String,
}

/// Where a run stands, shared between the surface and its worker.
#[derive(Default)]
struct RunSignal {
    phase: AtomicU8,
    stop: AtomicBool,
    live_bits: AtomicU64,
    bytes: AtomicU64,
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
    shared: Shared,
    root: Option<Element>,
    scroll: ScrollOffset,
    actions: Option<Actions<SpeedTest>>,
    structure_dirty: bool,
    view_phase: Option<Phase>,
    rect: Rect,
    phase: Phase,
    transferred: u64,
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
    /// The device this window runs on, which every run it records names.
    device: Option<String>,
}

impl SpeedTestApp {
    fn refresh_view(&mut self) {
        let phase = match self.phase {
            Phase::Idle => crate::model::Phase::Idle,
            Phase::Ping => crate::model::Phase::Ping,
            Phase::Download => crate::model::Phase::Download,
            Phase::Upload => crate::model::Phase::Upload,
            Phase::Complete => crate::model::Phase::Complete,
            Phase::Error => crate::model::Phase::Error,
        };
        self.shared.borrow_mut().present(crate::model::Projection {
            phase,
            result: self
                .result
                .map(|r| (r.ping, r.jitter, r.download, r.upload)),
            live: self.display_value,
            bytes: self.transferred,
            ring: self.ring_progress,
            history: self.history(),
            current: (self.phase == Phase::Complete)
                .then(|| self.results.first())
                .flatten(),
            error: self.error.clone(),
            server: self.server_label().to_owned(),
        });
        if self.view_phase != Some(self.phase) {
            self.view_phase = Some(self.phase);
            self.structure_dirty = true;
            self.root = None;
        }
    }
    fn run_binding(&self) -> Option<ui::tilo_actions::ActionBinding<()>> {
        self.actions.as_ref()?.effect(&if self.is_running() {
            SpeedEffect::Stop
        } else {
            SpeedEffect::Start
        })
    }

    pub fn new() -> Self {
        Self {
            shared: Rc::new(RefCell::new(crate::model::Model::new())),
            root: None,
            scroll: Rc::default(),
            actions: None,
            structure_dirty: true,
            view_phase: None,
            rect: Rect::ZERO,
            phase: Phase::Idle,
            transferred: 0,
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
            device: None,
        }
    }

    /// Begin a run: the signal its worker reports through.
    fn begin(&mut self) -> Arc<RunSignal> {
        self.stop();
        let signal = Arc::new(RunSignal::default());
        self.run = Some(Arc::clone(&signal));
        self.phase = Phase::Ping;
        self.display_value = 0.0;
        self.transferred = 0;
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
        self.display_value = 0.0;
        self.transferred = 0;
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
            Ok(Run {
                result,
                server,
                bytes,
            }) => {
                self.transferred = bytes;
                self.result = Some(result);
                self.display_value = result.download;
                self.phase = Phase::Complete;
                cx.submit(self.recorded(&result, &server));
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

    /// The shared dial and frame scheduler use the same bounded reading.
    fn dial_target(&self) -> f64 {
        match self.phase {
            Phase::Idle | Phase::Error => 0.0,
            Phase::Ping => (self.display_value / 50.0).clamp(0.08, 0.92),
            Phase::Download => (self.display_value / 750.0).clamp(0.06, 1.0),
            Phase::Upload => (self.display_value / 180.0).clamp(0.06, 1.0),
            Phase::Complete => self
                .result
                .map_or(0.0, |result| (result.download / 800.0).clamp(0.08, 1.0)),
        }
    }

    fn is_running(&self) -> bool {
        matches!(self.phase, Phase::Ping | Phase::Download | Phase::Upload)
    }
}

impl Default for SpeedTestApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for SpeedTestApp {
    ui::provides!(dyn ui::widget::controls::ActionControls);
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.refresh_view();
        if let Some(root) = &mut self.root {
            root.sync(state, path);
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let inner = self
            .root
            .as_mut()
            .is_some_and(|root| root.take_structure_dirty());
        own | inner
    }

    fn wants_pointer_capture(&self) -> bool {
        self.root
            .as_ref()
            .is_some_and(|root| root.wants_pointer_capture())
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.root
            .iter_mut()
            .map(|root| root.as_mut() as &mut (dyn Widget + '_))
            .collect()
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.root.iter().map(AsRef::as_ref).collect()
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(root) = &mut self.root {
            root.handle_event(event, ctx);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(root) = &mut self.root {
            root.draw(scene, transform, text, theme, presence);
        }
    }

    fn update(&mut self, doc: &AppModel, path: &ModelPath) -> bool {
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
        if self.is_running()
            && !self.shot
            && let Some(run) = &self.run
        {
            let measured = f64::from_bits(run.live_bits.load(Ordering::Acquire));
            let bytes = run.bytes.load(Ordering::Acquire);
            if !measured.total_cmp(&self.display_value).is_eq() || bytes != self.transferred {
                self.display_value = measured;
                self.transferred = bytes;
                changed = true;
            }
        }
        let now = ui::time::now();
        let delta = now
            .duration_since(self.last_animation)
            .as_secs_f64()
            .min(0.05);
        self.last_animation = now;
        let target = self.dial_target();
        let eased = 1.0 - (-delta * 9.0).exp();
        let previous = self.ring_progress;
        self.ring_progress += (target - self.ring_progress) * eased;
        changed |= (previous - self.ring_progress).abs() > 0.0005;
        self.refresh_view();
        if let Some(root) = &mut self.root {
            changed |= root.update(doc, path);
        }
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.refresh_view();
        if self.root.is_none() {
            self.root = Some(view::build(
                &self.shared,
                &self.scroll,
                self.run_binding(),
                theme,
            ));
        }
        self.root
            .as_mut()
            .unwrap()
            .build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        if let Some(root) = &mut self.root {
            root.apply_layout(taffy, node, origin);
            self.rect = root.get_rect();
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
        let target = self.dial_target();
        (self.is_running() || (self.ring_progress - target).abs() > 0.001)
            .then(|| ui::time::now() + Duration::from_millis(16))
    }
}

impl SpeedTestApp {
    /// The action that records `result`, measured against `server` on this
    /// window's device.
    fn recorded(&self, result: &ResultSet, server: &str) -> Invoke<SpeedTest> {
        SpeedTest::RECORD.with((
            result.ping,
            result.jitter,
            result.download,
            result.upload,
            server.to_owned(),
            self.device.clone().unwrap_or_default(),
        ))
    }
}

impl ResourceSurface<SpeedTest, SpeedEffect> for SpeedTestApp {
    fn present(&mut self, state: &Rc<SpeedTest>) {
        self.results = state.newest_first().cloned().collect();
        if self.phase == Phase::Complete {
            // The completed dial and its metadata represent the same newest
            // record. A document change may replace it or clear all results.
            if let Some(latest) = self.results.first() {
                self.result = Some(ResultSet {
                    ping: latest.ping,
                    jitter: latest.jitter,
                    download: latest.download,
                    upload: latest.upload,
                });
                self.display_value = latest.download;
            } else {
                self.stop();
            }
        }
        self.root = None;
        self.structure_dirty = true;
        self.refresh_view();
    }

    fn attach(&mut self, cx: SurfaceCx<SpeedTest>) {
        self.dispatcher = Some(cx.dispatcher());
        self.actions = Some(cx.actions());
        self.device = cx.device().map(str::to_owned);
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

/// Measure, reporting each phase to the surface and the effect's caller.
impl ui::widget::controls::ActionControls for SpeedTestApp {
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        self.root
            .as_mut()
            .and_then(|root| ui::widget::controls::take_control_invocation(root.as_mut()))
    }
}

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

/// The server every stage measures against.
const SERVER: &str = "https://speed.cloudflare.com";
/// Bytes the download stage fetches.
const DOWNLOAD_BYTES: u64 = 10_000_000;
/// Bytes the upload stage sends.
const UPLOAD_BYTES: usize = 2_000_000;

fn client() -> Result<tilo_net::Client, String> {
    tilo_net::Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent("Tilo Speed Test/0.1")
        .build()
        .map_err(|error| error.to_string())
}

async fn measure(signal: &RunSignal, progress: &Progress) -> Result<Run, String> {
    progress.report("ping", Some(0), Some(3));
    let client = client()?;
    let mut samples = Vec::new();
    let mut server = "Cloudflare".to_owned();
    for request in 0..6 {
        stopped(signal, progress)?;
        let ping_start = ui::time::now();
        let reached = client
            .get(format!("{SERVER}/__down?bytes=1"))
            .send()
            .await
            .and_then(tilo_net::Response::error_for_status)
            .map_err(|error| format!("Could not reach the test server: {error}"))?;
        let sample = ping_start.elapsed().as_secs_f64() * 1000.0;
        if request == 0 {
            server = reached
                .headers()
                .get("cf-meta-colo")
                .and_then(|colo| colo.to_str().ok())
                .map_or_else(
                    || "Cloudflare".to_owned(),
                    |colo| format!("Cloudflare at {colo}"),
                );
        } else {
            samples.push(sample);
            signal.live_bits.store(sample.to_bits(), Ordering::Release);
        }
    }
    let (ping, jitter) = crate::measure::latency_stats(&samples)
        .ok_or_else(|| "The test server did not answer.".to_owned())?;
    stopped(signal, progress)?;
    signal.live_bits.store(0.0_f64.to_bits(), Ordering::Release);
    signal.set(RunSignal::DOWNLOAD);
    progress.report("download", Some(1), Some(3));
    let (bytes, download) =
        download_stage(&client, SERVER, signal, || stopped(signal, progress)).await?;

    signal.live_bits.store(0.0_f64.to_bits(), Ordering::Release);
    signal.set(RunSignal::UPLOAD);
    progress.report("upload", Some(2), Some(3));
    let upload = upload_stage(&client, SERVER).await?;
    stopped(signal, progress)?;
    Ok(Run {
        bytes: bytes + UPLOAD_BYTES as u64,
        result: ResultSet {
            ping,
            jitter,
            download,
            upload,
        },
        server,
    })
}

/// Download from `server`, showing the rate on `signal` as it runs: the
/// bytes moved and the rate they came at. A refusal (a busy server's 429,
/// say) is a failed stage, never its error page timed as the connection.
async fn download_stage(
    client: &tilo_net::Client,
    server: &str,
    signal: &RunSignal,
    stopped: impl Fn() -> Result<(), String> + Send,
) -> Result<(u64, f64), String> {
    let failed = |error: tilo_net::NetError| format!("Download test failed: {error}");
    let start = ui::time::now();
    let mut response = client
        .get(format!("{server}/__down?bytes={DOWNLOAD_BYTES}"))
        .send()
        .await
        .and_then(tilo_net::Response::error_for_status)
        .map_err(failed)?;
    let mut bytes = 0_u64;
    while let Some(chunk) = response.chunk().await.map_err(failed)? {
        stopped()?;
        bytes += chunk.len() as u64;
        signal.bytes.store(bytes, Ordering::Release);
        signal.live_bits.store(
            crate::measure::mbps(bytes, start.elapsed()).to_bits(),
            Ordering::Release,
        );
    }
    Ok((bytes, crate::measure::mbps(bytes, start.elapsed())))
}

/// Upload to `server` and the rate it went at; a refusal fails the stage.
async fn upload_stage(client: &tilo_net::Client, server: &str) -> Result<f64, String> {
    let start = ui::time::now();
    client
        .post(format!("{server}/__up"))
        .body(vec![0_u8; UPLOAD_BYTES])
        .send()
        .await
        .and_then(tilo_net::Response::error_for_status)
        .map_err(|error| format!("Upload test failed: {error}"))?;
    Ok(crate::measure::mbps(UPLOAD_BYTES as u64, start.elapsed()))
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

    /// A local server that reads each request whole and answers it with
    /// `status` and `body`.
    fn serve(status: &'static str, body: Vec<u8>) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut head = Vec::new();
                let mut byte = [0_u8; 1];
                while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
                    head.push(byte[0]);
                }
                let sent = String::from_utf8_lossy(&head)
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<u64>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                let _ = std::io::copy(&mut (&mut stream).take(sent), &mut std::io::sink());
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    #[test]
    fn a_server_that_refuses_fails_the_stage_instead_of_timing_its_refusal() {
        let client = client().unwrap();
        let signal = RunSignal::default();
        let busy = serve("429 Too Many Requests", b"rate limited".to_vec());
        let download = ui::rt::block_on(download_stage(&client, &busy, &signal, || Ok(())))
            .expect("a test thread may block");
        assert!(
            download
                .as_ref()
                .is_err_and(|e| e.starts_with("Download test failed")),
            "a refused download is no measurement: {download:?}"
        );
        let upload =
            ui::rt::block_on(upload_stage(&client, &busy)).expect("a test thread may block");
        assert!(
            upload
                .as_ref()
                .is_err_and(|e| e.starts_with("Upload test failed")),
            "a refused upload is no measurement: {upload:?}"
        );

        let answering = serve("200 OK", vec![0; 4096]);
        let (bytes, rate) =
            ui::rt::block_on(download_stage(&client, &answering, &signal, || Ok(())))
                .expect("a test thread may block")
                .expect("an answered download is measured");
        assert_eq!(bytes, 4096);
        assert!(rate > 0.0);
        assert_eq!(signal.bytes.load(Ordering::Acquire), 4096);
        ui::rt::block_on(upload_stage(&client, &answering))
            .expect("a test thread may block")
            .expect("an answered upload is measured");
    }

    #[test]
    fn the_speed_scale_is_monotonic_and_bounded() {
        let mut app = SpeedTestApp::new();
        assert!(app.dial_target().total_cmp(&0.0).is_eq());
        for phase in [Phase::Download, Phase::Upload] {
            app.phase = phase;
            let values: Vec<_> = [0.0, 10.0, 100.0, 500.0, 50_000.0]
                .into_iter()
                .map(|value| {
                    app.display_value = value;
                    app.dial_target()
                })
                .collect();
            assert!(values.windows(2).all(|pair| pair[0] <= pair[1]));
            assert!(values.iter().all(|value| (0.0..=1.0).contains(value)));
            assert!((values.last().unwrap() - 1.0).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn the_host_shows_measured_worker_progress_and_stop_clears_it() {
        let mut app = TestApp::<SpeedTestShell>::new().expect("the production host opens");
        let signal = app
            .with_widget(|surface: &mut SpeedTestApp| surface.begin())
            .expect("the canonical surface is mounted");
        signal
            .live_bits
            .store(13.0_f64.to_bits(), Ordering::Release);
        app.settle();
        app.with_widget(|surface: &mut SpeedTestApp| {
            assert!(matches!(surface.phase, Phase::Ping));
            assert!(surface.display_value.total_cmp(&13.0).is_eq());
            assert_eq!(surface.shared.borrow().readings.ping, None);
        })
        .expect("the surface remains mounted");
        signal.set(RunSignal::DOWNLOAD);
        signal
            .live_bits
            .store(300.0_f64.to_bits(), Ordering::Release);
        signal.bytes.store(10_000_000, Ordering::Release);
        app.settle();
        app.with_widget(|surface: &mut SpeedTestApp| {
            assert!(matches!(surface.phase, Phase::Download));
            assert!(surface.display_value.total_cmp(&300.0).is_eq());
            assert_eq!(surface.transferred, 10_000_000);
            assert_eq!(surface.shared.borrow().stage(), 1);
        })
        .expect("the surface remains mounted");
        app.effect(&SpeedEffect::Stop)
            .expect("the published stop effect succeeds");
        assert!(signal.stop.load(Ordering::Acquire));
        app.with_widget(|surface: &mut SpeedTestApp| {
            assert!(matches!(surface.phase, Phase::Idle));
            assert!(surface.display_value.total_cmp(&0.0).is_eq());
            assert_eq!(surface.transferred, 0);
        })
        .expect("the stopped surface remains mounted");
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
        app.invoke(SpeedTest::RECORD.with((
            9.0,
            1.0,
            500.0,
            90.0,
            "Cloudflare · FRA".into(),
            "test-device".into(),
        )))
        .expect("a run is recorded");
        let app = app.reopen().expect("the app reopens");
        assert_eq!(app.state().results.len(), 1, "measurements are kept");
    }

    #[test]
    fn a_run_is_recorded_with_the_device_that_ran_it() {
        let mut app = TestApp::<SpeedTestShell>::new().expect("the app opens");
        let result = ResultSet {
            ping: 9.0,
            jitter: 1.0,
            download: 500.0,
            upload: 90.0,
        };
        let recorded = app
            .with_widget(|surface: &mut SpeedTestApp| surface.recorded(&result, "Cloudflare"))
            .expect("the surface is hosted");
        app.invoke(recorded).expect("the run is recorded");
        let state = app.state();
        let run = state.newest_first().next().expect("a run");
        assert_eq!(run.device, "test-device", "the device the test host names");
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
        assert_eq!(app.server_label(), "Cloudflare at LHR");

        ResourceSurface::<SpeedTest, SpeedEffect>::stage(&mut app, Some("complete"));
        let shown = app.result.expect("the newest run is on the dial");
        assert!((shown.download - 642.7).abs() < f64::EPSILON);
        let below: Vec<f64> = app.history().iter().map(|run| run.download).collect();
        assert_eq!(below, [618.2, 584.6]);
    }

    #[test]
    fn completed_dial_follows_record_changes_and_clearing_results() {
        let mut app = TestApp::<SpeedTestShell>::capture(Some("complete"))
            .expect("the completed production surface opens");
        app.invoke(SpeedTest::RECORD.with((
            20.0,
            2.0,
            123.0,
            45.0,
            "Another server".into(),
            "another-device".into(),
        )))
        .expect("record another measurement");
        app.with_widget(|surface: &mut SpeedTestApp| {
            assert!(surface.result.unwrap().download.total_cmp(&123.0).is_eq());
            assert_eq!(surface.server_label(), "Another server");
            assert_eq!(
                surface.history().len(),
                3,
                "only the dial's record is omitted"
            );
            assert!(surface.history()[0].download.total_cmp(&642.7).is_eq());
        })
        .unwrap();
        app.invoke_confirmed(SpeedTest::CLEAR.invoke())
            .expect("clear results");
        app.with_widget(|surface: &mut SpeedTestApp| {
            assert!(matches!(surface.phase, Phase::Idle));
            assert!(surface.result.is_none());
            assert!(surface.history().is_empty());
            assert!(surface.shared.borrow().readings.download.is_none());
        })
        .unwrap();
    }

    #[test]
    fn metadata_and_dial_never_touch_the_primary_action() {
        fn bounds(widget: &dyn Widget, name: &str) -> Option<Rect> {
            if widget.name() == name {
                return Some(widget.get_rect());
            }
            widget
                .children()
                .into_iter()
                .find_map(|child| bounds(child, name))
        }
        for viewport in [
            (1180, 520),
            (1180, 640),
            (1180, 760),
            (1180, 900),
            (480, 820),
            (560, 820),
            (700, 820),
            (849, 820),
        ] {
            let mut app = TestApp::<SpeedTestShell>::capture_at(Some("complete"), viewport)
                .expect("the actual completed screen");
            let harness = app.harness();
            let action = *harness
                .targets()
                .get("run")
                .expect("the production primary action");
            let dial =
                bounds(harness.root_widget(), "ProgressRing").expect("the actual shared dial");
            let facts =
                bounds(harness.root_widget(), "PropertyList").expect("the actual metadata rows");
            assert!(action.width() > 0.0 && dial.width() > 0.0 && facts.width() > 0.0);
            assert!(
                action.intersect(facts).area().total_cmp(&0.0).is_eq(),
                "metadata and primary action overlap at {viewport:?}"
            );
            assert!(
                dial.y1 < action.y0,
                "dial and primary action overlap at {viewport:?}"
            );
        }
    }
}
