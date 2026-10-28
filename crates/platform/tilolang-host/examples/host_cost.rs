//! `cargo run --release -p tilolang-host --example host_cost -- [count ...]`: what an app instance
//! costs in a running tilolang host (`docs/designs/app-language/ui-host.md`, step 2b).
//!
//! Opens Notes instances in one host, as the session hands them over: each on its own broker
//! connection (a broker the example plays), each over a room record of its own, each sending its
//! window patches over a `SOCK_SEQPACKET` connection whose far end the example drains, as a
//! window's process does. It prints
//!
//! - **marginal memory**: the anonymous memory each further instance adds to the running host;
//! - **first frame**: from the instance handed over to its window's first tree arriving at the
//!   window's end, cold (the host's first instance) and warm.
//!
//! Each count is cumulative: `1 10 30` opens 1, then 9 more, then 20 more.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_precision_loss,
    reason = "a measuring example: a failure ends it, and its sizes and counts are small"
)]

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use collaboration::{CollabEvent, CollabRequest, CollaborationManager, PeerTransport, RouteSpec};
use tilo_render_protocol::channel::Channel;
use tilo_render_protocol::patch::unix::ChannelTransport;
use tilo_render_protocol::patch::{self, ToUi, Transport as _};
use tilolang_host::TilolangHost;
use tilolang_host::hosted::{Hosted, Spec};
use tilolang_host::phrases::Phrases;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::instance::room::RoomRecord;

const APP: &str = "so.tilo.lang.notes";

fn notes() -> Arc<Bundle> {
    let lock = tilolang::IdsLock::parse(include_str!("../../../../apps/tl/notes/app.ids")).unwrap();
    let compiled =
        tilolang::compile_package(include_str!("../../../../apps/tl/notes/app.tl"), &lock, &[])
            .unwrap_or_else(|_| panic!("Notes compiles"));
    Arc::new(Bundle::new(&compiled, Vec::new()))
}

/// The broker's end of one instance's connection: it takes everything and delivers nothing.
struct Broker {
    route: RouteSpec,
    joined: Option<RouteSpec>,
}

impl PeerTransport for Broker {
    fn is_connected(&self) -> bool {
        true
    }

    fn send(&mut self, request: CollabRequest) -> Result<(), String> {
        if matches!(request, CollabRequest::Hello { .. }) {
            self.joined = Some(self.route.clone());
        }
        Ok(())
    }

    fn poll(&mut self) -> Vec<CollabEvent> {
        Vec::new()
    }

    fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
        self.joined.take().map(Ok)
    }
}

/// A field of `/proc/self/status`, in kB.
fn status(field: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .unwrap_or_default()
        .lines()
        .find_map(|line| line.strip_prefix(field)?.strip_prefix(':'))
        .and_then(|value| value.split_whitespace().next()?.parse().ok())
        .unwrap_or(0)
}

fn record(index: usize) -> (RoomRecord, RouteSpec) {
    let route = RouteSpec::app(APP, uuid::Uuid::new_v4());
    let mut manager = CollaborationManager::with_peer_transport(Broker {
        route: route.clone(),
        joined: None,
    });
    manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: None,
        record_kind: None,
        handle: Some(format!("person-{index}")),
    });
    let _ = manager.poll();
    (
        RoomRecord::new(automerge::AutoCommit::new(), manager, route.clone(), "ada"),
        route,
    )
}

/// Hand the host one more instance and run it until its window's first tree arrives. The
/// window's end, and how long that took.
fn open(
    host: &mut TilolangHost,
    bundle: &Arc<Bundle>,
    phrases: &Rc<Phrases>,
    index: usize,
) -> (ChannelTransport, Duration) {
    let started = Instant::now();
    let (record, route) = record(index);
    let (ours, theirs) = Channel::pair().unwrap();
    let kept = host.kept(APP).unwrap();
    let hosted = Hosted::with_record(
        Spec {
            app: APP.to_owned(),
            room: route.cluster_id.to_string(),
            instance: index as u64,
            person: "ada".to_owned(),
        },
        record,
        Arc::clone(bundle),
        &kept,
        Box::new(ChannelTransport::new(ours).unwrap()),
        Rc::clone(phrases),
        Arc::new(|| {}),
    );
    host.add(hosted, None);
    let mut window = ChannelTransport::new(theirs).unwrap();
    loop {
        host.turn();
        while let Some(batch) = window.recv().unwrap() {
            let messages = patch::decode_batch::<ToUi>(&batch).unwrap();
            if messages
                .iter()
                .any(|message| matches!(message, ToUi::Open { .. }))
            {
                return (window, started.elapsed());
            }
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the instance never showed its window"
        );
    }
}

fn main() {
    let counts: Vec<usize> = std::env::args()
        .skip(1)
        .filter_map(|arg| arg.parse().ok())
        .collect();
    let counts = if counts.is_empty() {
        vec![1, 10, 30]
    } else {
        counts
    };
    let bundle = notes();
    let phrases = Rc::new(Phrases::new("en-US"));
    let mut host = TilolangHost::new(None, Arc::new(|| {}));
    let mut windows = Vec::new();
    let mut frames = Vec::new();
    let before = status("RssAnon");
    let mut last = (0, before);
    for count in counts {
        while windows.len() < count {
            let (window, first) = open(&mut host, &bundle, &phrases, windows.len());
            windows.push(window);
            frames.push(first);
        }
        // Settle what the instances still do off their first frame (publication, indexes).
        while host.turn() {}
        let now = status("RssAnon");
        let added = windows.len().saturating_sub(last.0);
        println!(
            "{count:>3} instances: {:.1} MiB anonymous in all; {:.2} MiB per instance from {} to {count}",
            now.saturating_sub(before) as f64 / 1024.0,
            now.saturating_sub(last.1) as f64 / 1024.0 / added.max(1) as f64,
            last.0,
        );
        last = (windows.len(), now);
    }
    let cold = frames.first().copied().unwrap_or_default();
    let mut warm: Vec<Duration> = frames.iter().skip(1).copied().collect();
    warm.sort();
    let median = warm.get(warm.len() / 2).copied().unwrap_or_default();
    let worst = warm.last().copied().unwrap_or_default();
    println!(
        "first frame: cold {:.1} ms; warm median {:.1} ms, worst {:.1} ms",
        cold.as_secs_f64() * 1e3,
        median.as_secs_f64() * 1e3,
        worst.as_secs_f64() * 1e3
    );
}
