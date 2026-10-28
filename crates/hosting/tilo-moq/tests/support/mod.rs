#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    dead_code,
    reason = "test fixtures; each test binary uses a subset"
)]

use std::time::Duration;
use tilo_moq::carrier::{self, AdmissionError, Connected, OriginTask, Trust};

/// A local origin whose driver runs until the returned guard drops.
pub struct Origin {
    pub producer: moq_net::origin::Producer,
    _task: OriginTask,
}

impl Origin {
    pub fn new() -> Self {
        let (producer, task) =
            OriginTask::produce(carrier::origin_config(1024 * 1024, Duration::from_secs(2)));
        Self {
            producer,
            _task: task,
        }
    }

    /// Publish and announce `path` with one `audio` track.
    pub fn publish(&self, path: &str) -> (moq_net::broadcast::Producer, moq_net::track::Producer) {
        let broadcast = self.producer.create_broadcast(path).unwrap();
        let track = broadcast.create_track("audio", None).unwrap();
        broadcast
            .announce(moq_net::origin::Route::default())
            .unwrap();
        (broadcast, track)
    }

    /// The broadcast at `path`, once something announces it here.
    pub async fn broadcast(&self, path: &str) -> moq_net::broadcast::Consumer {
        self.producer
            .consume()
            .routed_broadcast(path)
            .await
            .expect("authorized announce")
    }
}

/// Dials one loopback relay.
#[derive(Clone)]
pub struct Dialer {
    pub url: url::Url,
    pub trust: Trust,
}

impl Dialer {
    pub fn new(endpoint: &str, fingerprints: &[String]) -> Self {
        Self {
            url: endpoint.parse().unwrap(),
            trust: Trust::Fingerprints(fingerprints.to_vec()),
        }
    }

    pub async fn connect(
        &self,
        grant: &[u8],
        publish: Option<&Origin>,
        subscribe: Option<&Origin>,
    ) -> Result<Connected, AdmissionError> {
        self.connect_to(&self.url, grant, publish, subscribe).await
    }

    pub async fn connect_to(
        &self,
        url: &url::Url,
        grant: &[u8],
        publish: Option<&Origin>,
        subscribe: Option<&Origin>,
    ) -> Result<Connected, AdmissionError> {
        carrier::connect(
            url,
            &self.trust,
            grant,
            publish.map(|origin| origin.producer.consume()),
            subscribe.map(|origin| origin.producer.clone()),
            tilo_test_env::hang_bound(),
        )
        .await
    }
}
