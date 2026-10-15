//! The service loop: publish the actions, then answer invocations until the
//! broker goes away.
//!
//! Invocations run on one bounded runtime, a few at a time; the loop itself
//! only moves events and updates. It blocks between them on one channel that
//! both the broker transport and finished invocations wake, so an idle
//! service costs nothing.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ui::collaboration::{CollabEvent, CollabRequest, CollaborationManager};
use ui::tilo_actions::outcome::{InvocationId, InvocationUpdate};

use crate::actions::{self, code, failure};

/// Invocations run at once; past this a new one is refused as busy.
pub const MAX_RUNNING: usize = 8;
/// How long the broker may be unreachable before the service exits. Whoever
/// started it starts it again with the next session.
const DISCONNECTED_GRACE: Duration = Duration::from_secs(60);
/// How often the loop looks at the connection when nothing happens.
const IDLE_CHECK: Duration = Duration::from_secs(5);

enum Wake {
    /// The broker transport has events.
    Transport,
    /// An invocation finished.
    Done(u64, InvocationUpdate),
}

struct Running {
    invocation: InvocationId,
    task: tokio::task::JoinHandle<()>,
}

/// Connect to the broker for `workspace`, publish, and serve until the
/// broker is gone.
///
/// # Errors
///
/// When the launcher handed down no broker connection, the runtime cannot
/// start, or a request cannot be queued.
pub fn run(workspace: uuid::Uuid) -> Result<()> {
    let (tx, rx) = channel();
    let mut scope = ui::wake::WakeScope::new();
    let transport_tx = std::sync::Mutex::new(tx.clone());
    // A fresh scope has no destination yet, so binding cannot fail.
    let _ = scope.bind(move |_| {
        let _ = transport_tx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .send(Wake::Transport);
    });
    let manager = {
        let _entered = scope.enter();
        CollaborationManager::for_route(actions::APP_ID, workspace, None, None)
    }
    .context("connect through TILO_SVC_COLLAB_FD")?;
    let runtime = tilo_rt::daemon("web-search")
        .workers(2)
        .build()
        .context("start the network runtime")?;
    let client = tilo_web_search::client().context("set up the HTTP client")?;
    let mut service = Service {
        manager,
        runtime,
        client,
        tx,
        running: HashMap::new(),
    };
    service.send(actions::publication())?;
    tracing::info!(resource = actions::RESOURCE_ID, "published web search");
    let outcome = service.serve(&rx);
    // The transport wakes through the scope, so it goes first.
    drop(service);
    drop(scope);
    outcome
}

struct Service {
    manager: CollaborationManager,
    runtime: tokio::runtime::Runtime,
    client: tilo_net::Client,
    tx: Sender<Wake>,
    running: HashMap<u64, Running>,
}

impl Service {
    fn send(&mut self, request: CollabRequest) -> Result<()> {
        anyhow::ensure!(
            self.manager.send(request),
            "the broker refused to queue a request"
        );
        Ok(())
    }

    fn serve(&mut self, rx: &Receiver<Wake>) -> Result<()> {
        let mut disconnected_since: Option<Instant> = None;
        loop {
            for event in self.manager.poll() {
                self.handle(event)?;
            }
            if self.manager.is_connected() {
                disconnected_since = None;
            } else {
                let since = *disconnected_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= DISCONNECTED_GRACE {
                    tracing::info!("the broker is gone; stopping");
                    return Ok(());
                }
            }
            match rx.recv_timeout(IDLE_CHECK) {
                Ok(Wake::Done(id, update)) => self.finish(id, update)?,
                Ok(Wake::Transport) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
            // Take whatever else is already waiting without blocking again.
            while let Ok(wake) = rx.try_recv() {
                if let Wake::Done(id, update) = wake {
                    self.finish(id, update)?;
                }
            }
        }
    }

    fn handle(&mut self, event: CollabEvent) -> Result<()> {
        match event {
            CollabEvent::Invocation {
                id,
                invocation,
                resource,
                action,
                argument,
                actor,
                ..
            } => self.start(
                id,
                invocation,
                &resource,
                &action,
                argument.as_deref(),
                &actor,
            ),
            CollabEvent::InvocationCancellation { id } => {
                if let Some(running) = self.running.remove(&id) {
                    running.task.abort();
                    self.send(CollabRequest::UpdateInvocation {
                        id,
                        update: InvocationUpdate::Cancelled {
                            invocation: running.invocation,
                            message: Some("cancelled by the caller".into()),
                        },
                    })?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn start(
        &mut self,
        id: u64,
        invocation: InvocationId,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &str,
    ) -> Result<()> {
        let failed = |error: actions::Failure| InvocationUpdate::Failed {
            invocation,
            error: *error,
        };
        if self.running.len() >= MAX_RUNNING {
            return self.send(CollabRequest::UpdateInvocation {
                id,
                update: failed(failure(
                    code::BUSY,
                    "web search is already running as many requests as it allows",
                    true,
                )),
            });
        }
        let call = match actions::bind(resource, action, argument) {
            Ok(call) => call,
            Err(error) => {
                return self.send(CollabRequest::UpdateInvocation {
                    id,
                    update: failed(error),
                });
            }
        };
        // The query or address is the caller's; log that a lookup happened,
        // not what was looked up.
        tracing::info!(invocation = id, action, actor, "running");
        let client = self.client.clone();
        let tx = self.tx.clone();
        let task = self.runtime.spawn(async move {
            let update = match actions::run(&client, call).await {
                Ok(output) => InvocationUpdate::Succeeded { invocation, output },
                Err(error) => InvocationUpdate::Failed {
                    invocation,
                    error: *error,
                },
            };
            let _ = tx.send(Wake::Done(id, update));
        });
        self.running.insert(id, Running { invocation, task });
        Ok(())
    }

    fn finish(&mut self, id: u64, update: InvocationUpdate) -> Result<()> {
        // Absent when it was cancelled: the caller already has its answer.
        if self.running.remove(&id).is_some() {
            self.send(CollabRequest::UpdateInvocation { id, update })?;
        }
        Ok(())
    }
}
