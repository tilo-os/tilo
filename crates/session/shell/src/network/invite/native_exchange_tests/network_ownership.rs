//! Endpoint custody survives retirement of its enrollment actor.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

struct ObservedLastSender {
    commands: Option<tokio::sync::mpsc::Sender<crate::network::actor::NetCmd>>,
    actor: tokio::task::AbortHandle,
    observed: Arc<AtomicBool>,
}
impl Drop for ObservedLastSender {
    fn drop(&mut self) {
        drop(self.commands.take());
        // Admit the real actor's shutdown in the old drop-before-abort window.
        // A fixed retirement has already joined its cancelled task here.
        let started = std::time::Instant::now();
        while !self.actor.is_finished() && started.elapsed() < std::time::Duration::from_secs(2) {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        self.observed
            .store(self.actor.is_finished(), Ordering::Release);
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_enrollment_actor_retirement_preserves_the_endpoint_for_mounted_roots() {
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap();
    let (commands, actor) = crate::network::actor::Actor::spawn_reporting_addrs(
        endpoint.clone(),
        iroh::address_lookup::memory::MemoryLookup::new(),
        None,
        Arc::new(Mutex::new(Vec::new())),
    );
    let (reply, answer) = tokio::sync::oneshot::channel();
    commands
        .send(crate::network::actor::NetCmd::WireTally(reply))
        .await
        .unwrap();
    tilo_test_env::within("the actual enrollment actor to own its router", answer)
        .await
        .unwrap();
    let observed = Arc::new(AtomicBool::new(false));
    let last_sender = ObservedLastSender {
        commands: Some(commands),
        actor: actor.abort_handle(),
        observed: observed.clone(),
    };
    retire_actor_before_endpoint_reuse(last_sender, actor).await;
    let closed = endpoint.is_closed();
    endpoint.close().await;
    assert!(
        observed.load(Ordering::Acquire),
        "the test must observe actual actor retirement"
    );
    assert!(
        !closed,
        "dropping the last command sender must not gracefully shut down the endpoint that the next mounted actor owns"
    );
}
