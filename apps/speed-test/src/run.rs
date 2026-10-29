//! A run of the test: the worker that measures, and what the window shows
//! of it while it runs. Starting and stopping are the resource's effects; a
//! finished run is recorded with the resource's `record` action.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ui::framework::{EffectAnswer, EffectCx, EffectStatus, Effected, Progress, ViewCx};
use ui::tilo_resource::Invoke;
use ui::time::Duration;
use ui::{LocalState, LocalStateWriter};

use crate::model::Phase;
use crate::resource::{SpeedEffect, SpeedTest};

/// What a finished run measured.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ResultSet {
    pub ping: f64,
    pub jitter: f64,
    pub download: f64,
    pub upload: f64,
}

/// A finished run, as the worker measured it.
pub(crate) struct Run {
    bytes: u64,
    result: ResultSet,
    server: String,
}

/// Where the run stands: the page is built again whenever this changes.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Stage {
    pub phase: Phase,
    pub error: Option<String>,
    /// Bytes the finished run moved.
    pub bytes: u64,
}

impl Default for Stage {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            error: None,
            bytes: 0,
        }
    }
}

/// The reading of the stage in progress, which moves with every chunk
/// received: only the dial follows it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Reading {
    /// Milliseconds while pinging, megabits per second while transferring.
    pub live: f64,
    pub bytes: u64,
}

/// One run's worker side: how it reports, and whether it was stopped.
struct Signal {
    stop: AtomicBool,
    answered: AtomicBool,
    stage: LocalStateWriter<Stage>,
    reading: LocalStateWriter<Reading>,
}

impl Signal {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// The worker moved on to `phase`.
    fn enter(&self, phase: Phase) {
        if !self.stopped() {
            self.reading.set(Reading::default());
            self.stage.update(|stage| stage.phase = phase);
        }
    }

    fn read(&self, live: f64, bytes: u64) {
        if !self.stopped() {
            self.reading.set(Reading { live, bytes });
        }
    }
}

/// Marks a run over when its worker is dropped before answering: its
/// caller cancelled it.
struct Answers(Arc<Signal>);

impl Drop for Answers {
    fn drop(&mut self) {
        let signal = &self.0;
        if !signal.answered.load(Ordering::Acquire) && !signal.stopped() {
            signal.stop.store(true, Ordering::Release);
            signal.reading.set(Reading::default());
            signal.stage.set(Stage::default());
        }
    }
}

/// The window's run: the stage and reading the view shows, and the run in
/// flight, if any.
#[derive(Clone)]
pub(crate) struct Runs {
    pub stage: LocalState<Stage>,
    pub reading: LocalState<Reading>,
    current: Rc<RefCell<Option<Arc<Signal>>>>,
    /// The device this window runs on, which every run it records names.
    device: Option<String>,
}

impl Runs {
    pub(crate) fn new(cx: &ViewCx<SpeedTest>) -> Self {
        Self {
            stage: cx.local(Stage::default()),
            reading: cx.local(Reading::default()),
            current: Rc::default(),
            device: cx.device().map(str::to_owned),
        }
    }

    /// Runs of no window, for tests of how a run moves.
    #[cfg(test)]
    fn detached() -> Self {
        Self {
            stage: LocalState::new(Stage::default()),
            reading: LocalState::new(Reading::default()),
            current: Rc::default(),
            device: None,
        }
    }

    pub(crate) fn is_running(&self) -> bool {
        self.stage.with(|stage| stage.phase.is_running())
    }

    /// Begin a run: the signal its worker reports through.
    fn begin(&self) -> Arc<Signal> {
        self.stop();
        let signal = Arc::new(Signal {
            stop: AtomicBool::new(false),
            answered: AtomicBool::new(false),
            stage: self.stage.writer(),
            reading: self.reading.writer(),
        });
        *self.current.borrow_mut() = Some(Arc::clone(&signal));
        self.stage.set(Stage {
            phase: Phase::Ping,
            ..Stage::default()
        });
        signal
    }

    /// Stop the run in flight; its worker notices and its result is dropped.
    fn stop(&self) {
        if let Some(run) = self.current.borrow_mut().take() {
            run.stop.store(true, Ordering::Release);
        }
        self.reading.set(Reading::default());
        self.stage.set(Stage::default());
    }

    /// The run `signal` answered: record what it measured.
    fn finish(
        &self,
        signal: &Arc<Signal>,
        answer: Result<Run, String>,
        cx: &EffectCx<SpeedTest>,
    ) -> EffectAnswer {
        let current = self
            .current
            .borrow_mut()
            .take_if(|run| Arc::ptr_eq(run, signal));
        if current.is_none() {
            return Err("the test was stopped".into());
        }
        self.reading.set(Reading::default());
        match answer {
            Ok(Run {
                result,
                server,
                bytes,
            }) => {
                self.stage.set(Stage {
                    phase: Phase::Complete,
                    error: None,
                    bytes,
                });
                cx.submit(recorded(&result, &server, self.device.as_deref()));
                Ok(Effected::value(ui::serde_json::json!({
                    "ping_ms": result.ping,
                    "jitter_ms": result.jitter,
                    "download_mbps": result.download,
                    "upload_mbps": result.upload,
                    "server": server,
                })))
            }
            Err(message) => {
                self.stage.set(Stage {
                    phase: Phase::Error,
                    error: Some(message.clone()),
                    bytes: 0,
                });
                Err(message)
            }
        }
    }

    /// Answer Start and Stop, and say which of them can run now.
    pub(crate) fn answer_effects(&self, cx: &ViewCx<SpeedTest>) {
        let runs = self.clone();
        let status = self.clone();
        cx.effects(move |effect: SpeedEffect, cx| match effect {
            SpeedEffect::Start => {
                if runs.is_running() {
                    return Err("a test is already running".into());
                }
                let signal = runs.begin();
                let worker = Arc::clone(&signal);
                let finishing = runs.clone();
                Ok(cx.spawn(
                    move |progress| run_measurement(worker, progress),
                    move |(): &mut (), answer, cx| finishing.finish(&signal, answer, cx),
                ))
            }
            SpeedEffect::Stop => {
                if !runs.is_running() {
                    return Err("no test is running".into());
                }
                runs.stop();
                Ok(Effected::done())
            }
        })
        .status(move || {
            if status.is_running() {
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
        });
    }
}

/// The action that records `result`, measured against `server` on `device`.
pub(crate) fn recorded(
    result: &ResultSet,
    server: &str,
    device: Option<&str>,
) -> Invoke<SpeedTest> {
    SpeedTest::RECORD.with((
        result.ping,
        result.jitter,
        result.download,
        result.upload,
        server.to_owned(),
        device.unwrap_or_default().to_owned(),
    ))
}

async fn run_measurement(signal: Arc<Signal>, progress: Progress) -> Result<Run, String> {
    let answers = Answers(signal);
    let answer = measure(&answers.0, &progress).await;
    answers.0.answered.store(true, Ordering::Release);
    answer
}

/// Why a run ends early: stopped from the app, or cancelled by its caller.
fn stopped(signal: &Signal, progress: &Progress) -> Result<(), String> {
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

/// Measure, reporting each phase to the window and the effect's caller.
async fn measure(signal: &Signal, progress: &Progress) -> Result<Run, String> {
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
            signal.read(sample, 0);
        }
    }
    let (ping, jitter) = crate::measure::latency_stats(&samples)
        .ok_or_else(|| "The test server did not answer.".to_owned())?;
    stopped(signal, progress)?;
    signal.enter(Phase::Download);
    progress.report("download", Some(1), Some(3));
    let (bytes, download) = download_stage(
        &client,
        SERVER,
        |bytes, rate| signal.read(rate, bytes),
        || stopped(signal, progress),
    )
    .await?;

    signal.enter(Phase::Upload);
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

/// Download from `server`, telling `received` the bytes moved so far and
/// the rate they came at. A refusal (a busy server's 429, say) is a failed
/// stage, never its error page timed as the connection.
async fn download_stage(
    client: &tilo_net::Client,
    server: &str,
    received: impl Fn(u64, f64) + Send,
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
        received(bytes, crate::measure::mbps(bytes, start.elapsed()));
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
    use std::sync::atomic::AtomicU64;

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
        let busy = serve("429 Too Many Requests", b"rate limited".to_vec());
        let download = ui::rt::block_on(download_stage(&client, &busy, |_, _| {}, || Ok(())))
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
        let received = AtomicU64::new(0);
        let (bytes, rate) = ui::rt::block_on(download_stage(
            &client,
            &answering,
            |bytes, _| received.store(bytes, Ordering::Release),
            || Ok(()),
        ))
        .expect("a test thread may block")
        .expect("an answered download is measured");
        assert_eq!(bytes, 4096);
        assert!(rate > 0.0);
        assert_eq!(received.load(Ordering::Acquire), 4096);
        ui::rt::block_on(upload_stage(&client, &answering))
            .expect("a test thread may block")
            .expect("an answered upload is measured");
    }

    #[test]
    fn stopping_a_run_returns_to_idle_and_tells_its_worker() {
        let runs = Runs::detached();
        let signal = runs.begin();
        assert!(runs.is_running());
        signal.enter(Phase::Download);
        signal.read(300.0, 4096);
        assert_eq!(runs.stage.get().phase, Phase::Download);
        assert_eq!(runs.reading.get().bytes, 4096);

        runs.stop();
        assert!(!runs.is_running());
        assert!(signal.stopped(), "the worker sees the stop");
        assert_eq!(runs.reading.get(), Reading::default());
        signal.enter(Phase::Upload);
        signal.read(90.0, 8192);
        assert_eq!(
            runs.stage.get(),
            Stage::default(),
            "a stopped worker reports nothing"
        );
        assert_eq!(runs.reading.get(), Reading::default());
    }

    #[test]
    fn a_run_its_caller_cancelled_returns_to_idle() {
        let runs = Runs::detached();
        let signal = runs.begin();
        signal.enter(Phase::Download);
        drop(Answers(signal));
        assert!(!runs.is_running());
    }

    #[test]
    fn a_run_is_recorded_with_the_device_that_ran_it() {
        let result = ResultSet {
            ping: 9.0,
            jitter: 1.0,
            download: 500.0,
            upload: 90.0,
        };
        let mut replica = ui::prelude::Replica::<SpeedTest>::new();
        replica
            .apply(recorded(&result, "Cloudflare", Some("laptop")))
            .expect("the run is recorded");
        let state = replica.state().expect("state");
        let run = state.newest_first().next().expect("a run");
        assert_eq!(run.device, "laptop");
        assert!(run.download.total_cmp(&500.0).is_eq());
    }
}
