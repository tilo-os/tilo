//! The measurement itself: latency, download and upload against Cloudflare's
//! fixed test endpoints. Everything it reports is something it measured; the
//! worker narrates progress to the UI over a channel.

use std::sync::mpsc::Sender;

use ui::time::{Duration, Instant};

const ORIGIN: &str = "https://speed.cloudflare.com";

/// Latency requests. The first only opens the connection and is not counted.
const PING_REQUESTS: usize = 6;
const DOWNLOAD_BYTES: u64 = 10_000_000;
const UPLOAD_CHUNK_BYTES: usize = 500_000;
const UPLOAD_CHUNKS: usize = 4;
/// How often a running transfer reports its average speed so far.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// What the worker tells the interface.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum WorkerMessage {
    /// One latency sample, in milliseconds.
    Ping {
        sample: f64,
    },
    /// Latency is settled and the download starts.
    Latency {
        ping: f64,
        jitter: f64,
        location: Option<String>,
    },
    /// The download's average speed so far, in Mbps.
    Download {
        live: f64,
    },
    /// The download finished and the upload starts.
    DownloadDone {
        mbps: f64,
        bytes: u64,
    },
    /// The upload's average speed so far, in Mbps.
    Upload {
        live: f64,
    },
    /// The upload finished: the test is complete.
    Done {
        mbps: f64,
        bytes: u64,
    },
    Failed(String),
}

/// Run one full measurement, reporting through `tx`.
pub(crate) async fn run(tx: Sender<WorkerMessage>) {
    if let Err(error) = measure(&tx).await {
        let _ = tx.send(WorkerMessage::Failed(error));
    }
}

/// Megabits per second for `bytes` moved in `elapsed`.
#[allow(
    clippy::cast_precision_loss,
    reason = "byte counts of a few megabytes are exact in f64"
)]
pub(crate) fn mbps(bytes: u64, elapsed: Duration) -> f64 {
    let seconds = elapsed.as_secs_f64();
    if seconds <= 0.0 {
        return 0.0;
    }
    bytes as f64 * 8.0 / seconds / 1_000_000.0
}

/// The median latency of `samples` and its jitter: the mean absolute
/// difference between consecutive samples.
#[allow(
    clippy::cast_precision_loss,
    clippy::indexing_slicing,
    reason = "a handful of samples; indices are bounded by the length checks"
)]
pub(crate) fn latency_stats(samples: &[f64]) -> Option<(f64, f64)> {
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

async fn measure(tx: &Sender<WorkerMessage>) -> Result<(), String> {
    let client = tilo_net::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent("Tilo Speed Test/0.1")
        .build()
        .map_err(|error| error.to_string())?;

    let mut samples = Vec::new();
    let mut location = None;
    for request in 0..PING_REQUESTS {
        let started = ui::time::now();
        let response = client
            .get(format!("{ORIGIN}/__down?bytes=0"))
            .send()
            .await
            .and_then(tilo_net::Response::error_for_status)
            .map_err(|error| format!("Could not reach the test server: {error}"))?;
        let sample = started.elapsed().as_secs_f64() * 1000.0;
        if request == 0 {
            location = response
                .headers()
                .get("cf-meta-city")
                .and_then(|value| value.to_str().ok())
                .filter(|city| !city.is_empty())
                .map(str::to_owned);
            continue;
        }
        samples.push(sample);
        let _ = tx.send(WorkerMessage::Ping { sample });
    }
    let (ping, jitter) =
        latency_stats(&samples).ok_or_else(|| "The test server did not answer.".to_owned())?;
    let _ = tx.send(WorkerMessage::Latency {
        ping,
        jitter,
        location,
    });

    let started = ui::time::now();
    let mut response = client
        .get(format!("{ORIGIN}/__down?bytes={DOWNLOAD_BYTES}"))
        .send()
        .await
        .map_err(|error| format!("The download test failed: {error}"))?;
    let mut received = 0_u64;
    let mut reported = ui::time::now();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("The download test failed: {error}"))?
    {
        received += chunk.len() as u64;
        if reported.elapsed() >= PROGRESS_EVERY {
            reported = ui::time::now();
            let _ = tx.send(WorkerMessage::Download {
                live: mbps(received, started.elapsed()),
            });
        }
    }
    let _ = tx.send(WorkerMessage::DownloadDone {
        mbps: mbps(received, started.elapsed()),
        bytes: received,
    });

    let started: Instant = ui::time::now();
    let mut sent = 0_u64;
    for _ in 0..UPLOAD_CHUNKS {
        client
            .post(format!("{ORIGIN}/__up"))
            .body(vec![0_u8; UPLOAD_CHUNK_BYTES])
            .send()
            .await
            .and_then(tilo_net::Response::error_for_status)
            .map_err(|error| format!("The upload test failed: {error}"))?;
        sent += UPLOAD_CHUNK_BYTES as u64;
        let _ = tx.send(WorkerMessage::Upload {
            live: mbps(sent, started.elapsed()),
        });
    }
    let _ = tx.send(WorkerMessage::Done {
        mbps: mbps(sent, started.elapsed()),
        bytes: sent,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_is_the_median_and_jitter_the_mean_step() {
        let (ping, jitter) = latency_stats(&[10.0, 14.0, 12.0, 12.0, 12.0]).unwrap();
        assert!((ping - 12.0).abs() < 1e-9);
        // Steps: 4, 2, 0, 0.
        assert!((jitter - 1.5).abs() < 1e-9);
    }

    #[test]
    fn a_single_sample_has_no_jitter() {
        assert_eq!(latency_stats(&[9.0]), Some((9.0, 0.0)));
        assert_eq!(latency_stats(&[]), None);
    }

    #[test]
    fn speed_is_megabits_per_second() {
        let speed = mbps(1_250_000, Duration::from_secs(1));
        assert!((speed - 10.0).abs() < 1e-9);
        assert!(mbps(10, Duration::ZERO).abs() < f64::EPSILON);
    }
}
