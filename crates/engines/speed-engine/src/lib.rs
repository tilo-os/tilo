//! The device's work for Speed Test: one run measuring latency, download and upload to the
//! nearest Cloudflare test server, as the app's own job.
//!
//! An app runs sandboxed and streams nothing of its own, so this crate is what the package
//! reaches through `engine.start`, `engine.status` and `engine.cancel`; it registers as the engine
//! of `so.tilo.speed-test` and answers that app alone. Every answer is JSON the package reads into
//! its own records.
//!
//! | op | asks | `status` answers |
//! |---|---|---|
//! | `run` | `{}` | `{state, phase, live, bytes, server, ping, jitter, download, upload, device, failure}` |
//!
//! `state` is `running`, `done` or `failed`; `phase` is `ping`, `download` or `upload`. `live` is
//! the reading of the stage in progress (milliseconds while pinging, megabits per second after),
//! so a window shows the run moving rather than only how it ended.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;

/// The app this engine answers.
pub const APP: &str = "so.tilo.speed-test";

/// Where a run measures: Cloudflare's test endpoint, the one every speed test of theirs uses.
const SERVER: &str = "https://speed.cloudflare.com";

/// The bytes one run downloads, and the bytes it uploads.
const DOWNLOAD_BYTES: u64 = 10_000_000;
const UPLOAD_BYTES: usize = 2_000_000;

/// The latency samples a run takes: the first request finds the server, the rest are timed.
const PINGS: u32 = 6;

/// A request that takes longer than this is a failure of the stage, not a slow measurement.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);

/// The engine of the speed test, as the session keeps it for the app.
#[derive(Default)]
pub struct SpeedEngine {
    jobs: Mutex<Jobs>,
}

#[derive(Default)]
struct Jobs {
    next: u64,
    running: BTreeMap<String, Arc<Job>>,
}

impl SpeedEngine {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a run measuring against `server`, answering the job's id.
    fn begin(&self, server: &str) -> Result<String, String> {
        let mut jobs = self
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        jobs.next = jobs.next.saturating_add(1);
        let id = format!("speed-run-{}", jobs.next);
        let job = Arc::new(Job {
            stop: AtomicBool::new(false),
            state: Mutex::new(State::default()),
        });
        jobs.running.insert(id.clone(), Arc::clone(&job));
        drop(jobs);
        let server = server.to_owned();
        std::thread::Builder::new()
            .name("speed-test".to_owned())
            .spawn(move || run(&job, &server))
            .map_err(|error| format!("the run could not start: {error}"))?;
        Ok(id)
    }
}

/// One run in flight, as the worker writes it and the app asks about it.
struct Job {
    stop: AtomicBool,
    state: Mutex<State>,
}

/// What a run has measured so far.
#[derive(Clone)]
struct State {
    state: &'static str,
    phase: &'static str,
    live: f64,
    bytes: u64,
    server: String,
    ping: f64,
    jitter: f64,
    download: f64,
    upload: f64,
    device: String,
    failure: String,
}

impl Default for State {
    fn default() -> Self {
        Self {
            state: "running",
            phase: "ping",
            live: 0.0,
            bytes: 0,
            server: String::new(),
            ping: 0.0,
            jitter: 0.0,
            download: 0.0,
            upload: 0.0,
            device: String::new(),
            failure: String::new(),
        }
    }
}

impl Job {
    fn write(&self, f: impl FnOnce(&mut State)) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut state);
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

/// What a finished run measured.
struct Finished {
    ping: f64,
    jitter: f64,
    download: f64,
    upload: f64,
    bytes: u64,
    server: String,
}

impl tilolang_tilo::engine::Provider for SpeedEngine {
    fn call(&self, op: &str, _input: &str) -> Result<String, String> {
        Err(format!("`{op}` is no op this engine answers"))
    }

    fn start(&self, op: &str, _input: &str) -> Result<String, String> {
        if op != "run" {
            return Err(format!("`{op}` is no job this engine starts"));
        }
        Ok(json!({ "job": self.begin(SERVER)? }).to_string())
    }

    fn status(&self, job: &str) -> Result<String, String> {
        let mut jobs = self
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(run) = jobs.running.get(job).cloned() else {
            return Err(format!("no job `{job}` runs on this device"));
        };
        let state = run
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if state.state != "running" {
            jobs.running.remove(job);
        }
        Ok(json!({
            "state": state.state,
            "phase": state.phase,
            "live": state.live,
            "bytes": state.bytes,
            "server": state.server,
            "ping": state.ping,
            "jitter": state.jitter,
            "download": state.download,
            "upload": state.upload,
            "device": state.device,
            "failure": state.failure,
        })
        .to_string())
    }

    fn cancel(&self, job: &str) -> bool {
        let mut jobs = self
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(run) = jobs.running.remove(job) else {
            return false;
        };
        run.stop.store(true, Ordering::Release);
        true
    }
}

/// Measure against `server`, writing each stage and reading into the job as it goes; the run's
/// answer is left in the job's state.
fn run(job: &Job, server: &str) {
    let measured = tilo_rt::app()
        .block_on(measure(job, server))
        .unwrap_or_else(|error| Err(format!("the run could not start: {error}")));
    job.write(|state| match measured {
        Ok(finished) => {
            state.state = "done";
            state.phase = "upload";
            state.live = finished.upload;
            state.bytes = finished.bytes;
            state.server = finished.server;
            state.ping = finished.ping;
            state.jitter = finished.jitter;
            state.download = finished.download;
            state.upload = finished.upload;
            state.device = tilo_paths::session::host_name().unwrap_or_default();
        }
        Err(failure) => {
            let stopped = job.stopped();
            state.state = if stopped { "done" } else { "failed" };
            state.failure = if stopped { String::new() } else { failure };
        }
    });
}

/// Measure, reporting each stage to the window and its readings as they come.
async fn measure(job: &Job, server: &str) -> Result<Finished, String> {
    let client = tilo_net::Client::builder()
        .timeout(TIMEOUT)
        .user_agent("Tilo Speed Test/0.1")
        .build()
        .map_err(|error| error.to_string())?;
    let mut samples = Vec::new();
    let mut server_name = "Cloudflare".to_owned();
    for request in 0..PINGS {
        if job.stopped() {
            return Err("the test was stopped".to_owned());
        }
        let started = tilo_rt::timer_now();
        let reached = client
            .get(format!("{server}/__down?bytes=1"))
            .send()
            .await
            .and_then(tilo_net::Response::error_for_status)
            .map_err(|error| format!("Could not reach the test server: {error}"))?;
        let sample = started.elapsed().as_secs_f64() * 1000.0;
        if request == 0 {
            server_name = reached
                .headers()
                .get("cf-meta-colo")
                .and_then(|colo| colo.to_str().ok())
                .map_or_else(
                    || "Cloudflare".to_owned(),
                    |colo| format!("Cloudflare at {colo}"),
                );
            job.write(|state| state.server = server_name.clone());
        } else {
            samples.push(sample);
            job.write(|state| state.live = sample);
        }
    }
    let (ping, jitter) =
        latency_stats(&samples).ok_or_else(|| "The test server did not answer.".to_owned())?;
    job.write(|state| {
        state.phase = "download";
        state.live = 0.0;
        state.ping = ping;
        state.jitter = jitter;
    });
    let (bytes, download) = download_stage(&client, server, job).await?;
    job.write(|state| {
        state.phase = "upload";
        state.live = 0.0;
        state.bytes = bytes;
        state.download = download;
    });
    let upload = upload_stage(&client, server).await?;
    Ok(Finished {
        ping,
        jitter,
        download,
        upload,
        bytes: bytes + UPLOAD_BYTES as u64,
        server: server_name,
    })
}

/// Download from `server`, telling the job the bytes moved so far and the rate they came at. A
/// refusal (a busy server's 429, say) is a failed stage, never its error page timed as the
/// connection.
async fn download_stage(
    client: &tilo_net::Client,
    server: &str,
    job: &Job,
) -> Result<(u64, f64), String> {
    let failed = |error: tilo_net::NetError| format!("Download test failed: {error}");
    let started = tilo_rt::timer_now();
    let mut response = client
        .get(format!("{server}/__down?bytes={DOWNLOAD_BYTES}"))
        .send()
        .await
        .and_then(tilo_net::Response::error_for_status)
        .map_err(failed)?;
    let mut bytes = 0_u64;
    while let Some(chunk) = response.chunk().await.map_err(failed)? {
        if job.stopped() {
            return Err("the test was stopped".to_owned());
        }
        bytes += chunk.len() as u64;
        let rate = mbps(bytes, started.elapsed());
        job.write(|state| {
            state.bytes = bytes;
            state.live = rate;
        });
    }
    Ok((bytes, mbps(bytes, started.elapsed())))
}

/// Upload to `server` and the rate it went at; a refusal fails the stage.
async fn upload_stage(client: &tilo_net::Client, server: &str) -> Result<f64, String> {
    let started = tilo_rt::timer_now();
    client
        .post(format!("{server}/__up"))
        .body(vec![0_u8; UPLOAD_BYTES])
        .send()
        .await
        .and_then(tilo_net::Response::error_for_status)
        .map_err(|error| format!("Upload test failed: {error}"))?;
    Ok(mbps(UPLOAD_BYTES as u64, started.elapsed()))
}

/// Megabits per second, from a byte count and how long they took.
#[allow(
    clippy::cast_precision_loss,
    reason = "byte counts of a few megabytes are exact in f64"
)]
fn mbps(bytes: u64, elapsed: std::time::Duration) -> f64 {
    let seconds = elapsed.as_secs_f64();
    if seconds <= 0.0 {
        return 0.0;
    }
    bytes as f64 * 8.0 / seconds / 1_000_000.0
}

/// The median latency and its variation between samples, or `None` for none.
#[allow(
    clippy::cast_precision_loss,
    clippy::indexing_slicing,
    reason = "a handful of samples; indices are bounded by the length checks"
)]
fn latency_stats(samples: &[f64]) -> Option<(f64, f64)> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    let median = if sorted.len().is_multiple_of(2) {
        f64::midpoint(sorted[mid - 1], sorted[mid])
    } else {
        sorted[mid]
    };
    let jitter = if samples.len() < 2 {
        0.0
    } else {
        samples
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .sum::<f64>()
            / (samples.len() - 1) as f64
    };
    Some((median, jitter))
}

/// Register this crate as the engine of Speed Test: what the session and the app host call once
/// at their start, before any app asks.
pub fn register() {
    tilolang_tilo::engine::register(APP, Arc::new(SpeedEngine::new()));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A local server that reads each request whole and answers it with `status` and `body`.
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

    fn job() -> Job {
        Job {
            stop: AtomicBool::new(false),
            state: Mutex::new(State::default()),
        }
    }

    #[test]
    fn a_server_that_refuses_fails_the_stage_instead_of_timing_its_refusal() {
        let client = tilo_net::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .unwrap();
        let busy = serve("429 Too Many Requests", b"rate limited".to_vec());
        let refused = tilo_rt::app()
            .block_on(download_stage(&client, &busy, &job()))
            .unwrap();
        assert!(
            refused
                .as_ref()
                .is_err_and(|error| error.starts_with("Download test failed")),
            "a refused download is no measurement: {refused:?}"
        );
        let upload = tilo_rt::app()
            .block_on(upload_stage(&client, &busy))
            .unwrap();
        assert!(
            upload
                .as_ref()
                .is_err_and(|error| error.starts_with("Upload test failed")),
            "a refused upload is no measurement: {upload:?}"
        );

        let answering = serve("200 OK", vec![0; 4096]);
        let (bytes, rate) = tilo_rt::app()
            .block_on(download_stage(&client, &answering, &job()))
            .unwrap()
            .expect("an answered download is measured");
        assert_eq!(bytes, 4096);
        assert!(rate > 0.0);
        tilo_rt::app()
            .block_on(upload_stage(&client, &answering))
            .unwrap()
            .expect("an answered upload is measured");
    }

    #[test]
    fn a_run_measures_the_whole_test_and_says_what_it_found() {
        let answering = serve("200 OK", vec![0; 4096]);
        let job = job();
        run(&job, &answering);
        let state = job.state.lock().unwrap().clone();
        assert_eq!(state.state, "done");
        assert_eq!(state.bytes, 4096 + UPLOAD_BYTES as u64);
        assert!(state.download > 0.0 && state.upload > 0.0);
        assert_eq!(state.server, "Cloudflare");
        assert!(state.failure.is_empty());
    }

    #[test]
    fn a_stopped_run_reports_no_failure() {
        let answering = serve("200 OK", vec![0; 4096]);
        let job = job();
        job.stop.store(true, Ordering::Release);
        run(&job, &answering);
        let state = job.state.lock().unwrap().clone();
        assert_eq!(state.state, "done", "a stopped run is no failure");
        assert!(state.failure.is_empty());
    }

    #[test]
    fn latency_is_the_median_and_jitter_the_mean_step() {
        let (ping, jitter) = latency_stats(&[10.0, 14.0, 12.0, 12.0, 12.0]).unwrap();
        assert!((ping - 12.0).abs() < 1e-9);
        assert!((jitter - 1.5).abs() < 1e-9);
        assert_eq!(latency_stats(&[9.0]), Some((9.0, 0.0)));
        assert_eq!(latency_stats(&[]), None);
    }

    #[test]
    fn speed_is_megabits_per_second() {
        let speed = mbps(1_250_000, std::time::Duration::from_secs(1));
        assert!((speed - 10.0).abs() < 1e-9);
        assert!(mbps(10, std::time::Duration::ZERO).abs() < f64::EPSILON);
    }

    #[test]
    fn an_app_asks_for_its_own_run_and_reads_it_back() {
        use tilolang_tilo::engine::Provider as _;
        let answering = serve("200 OK", vec![0; 4096]);
        let engine = SpeedEngine::new();
        assert!(engine.start("probe", "{}").is_err());
        assert!(engine.call("run", "{}").is_err());
        let id = engine.begin(&answering).expect("a run starts");
        let mut seen = None;
        for _ in 0..600 {
            let asked: serde_json::Value =
                serde_json::from_str(&engine.status(&id).expect("the run is asked about")).unwrap();
            if asked["state"] != "running" {
                seen = Some(asked);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let seen = seen.expect("the run ends");
        assert_eq!(seen["state"], "done");
        assert_eq!(seen["server"], "Cloudflare");
        assert_eq!(seen["bytes"], 4096 + UPLOAD_BYTES as u64);
        assert!(
            engine.status(&id).is_err(),
            "a finished run is answered once"
        );
        let stopping = engine.begin(&answering).expect("a run starts");
        assert!(engine.cancel(&stopping), "a run in flight stops");
        assert!(!engine.cancel(&stopping), "and stops once");
        assert!(engine.status(&stopping).is_err(), "a stopped run is gone");
    }
}
