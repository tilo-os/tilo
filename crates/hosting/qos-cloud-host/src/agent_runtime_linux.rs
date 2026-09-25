//! Linux execution custody, held only by the single runtime thread.
use super::*;
use crate::agent_runtime_adapter::launch_retry::{GatewayConnector, JoinedGateway};
use launcher::{
    managed::{
        ManagedLaunchEvent, ManagedLauncher,
        channel::{AdmittedManagedConnection, PendingManagedConnection},
    },
    services::EnrolledServiceContext,
};
use qos_action_broker::collab_broker::AppActionBroker;
use qos_agent_control_protocol::{AgentRequest, decode_request};
use runtime_collab::{EnrolledTaskEnvelope, TaskRunEnvelope, TaskRunPlanning};
use std::{collections::BTreeMap, net::Shutdown, os::unix::net::UnixStream};
struct IdentityThread {
    socket: UnixStream,
    thread: std::thread::JoinHandle<()>,
}
struct Pending {
    invocation: Uuid,
    connection: PendingManagedConnection,
    binding: launcher::managed::ManagedBinding,
    runtime: launcher::managed::ManagedRuntimeFacts,
}
pub(super) struct Execution {
    owner: Arc<RuntimeOwner>,
    broker: AppActionBroker,
    pending: BTreeMap<String, Pending>,
    connecting: BTreeMap<Uuid, GatewayConnector>,
    supervisors: BTreeMap<
        String,
        (
            Uuid,
            launcher::managed::ManagedBinding,
            launcher::managed::ManagedRuntimeFacts,
            launcher::managed::ProcessIdentity,
        ),
    >,
    identities: BTreeMap<String, IdentityThread>,
    admissions: Vec<std::thread::JoinHandle<()>>,
    admitted_tx: mpsc::Sender<(Uuid, Result<AdmittedManagedConnection, String>)>,
    admitted_rx: mpsc::Receiver<(Uuid, Result<AdmittedManagedConnection, String>)>,
    waiting: BTreeMap<Uuid, Instant>,
    controls: BTreeMap<Uuid, String>,
    next_transport_run: u64,
    #[cfg(test)]
    test_held_events: Option<(PathBuf, Vec<ManagedLaunchEvent>)>,
}
impl Execution {
    pub(super) fn new(owner: Arc<RuntimeOwner>, broker: AppActionBroker) -> Self {
        let (admitted_tx, admitted_rx) = mpsc::channel(4);
        #[cfg(test)]
        let test_held_events = owner
            .store()
            .ok()
            .map(|s| s.root.join("agent-test-hold-started"))
            .filter(|path| path.is_file())
            .map(|path| (path, Vec::new()));
        Self {
            owner,
            broker,
            pending: BTreeMap::new(),
            connecting: BTreeMap::new(),
            supervisors: BTreeMap::new(),
            identities: BTreeMap::new(),
            admissions: vec![],
            admitted_tx,
            admitted_rx,
            waiting: BTreeMap::new(),
            controls: BTreeMap::new(),
            next_transport_run: 1,
            #[cfg(test)]
            test_held_events,
        }
    }
    pub(super) fn running_ids(&self) -> impl Iterator<Item = Uuid> + '_ {
        self.controls.keys().copied()
    }
    pub(super) fn begin(&mut self, id: Uuid, config: &ManagedConfig) -> Result<(), String> {
        if self.controls.contains_key(&id) {
            return Ok(());
        }
        // Both Fresh and retained Spawn work pass the same installed-owner door.
        // A Message can only retain its original live attempt; it is never recovered.
        let is_message = self.owner.attempt(id).is_ok_and(|a| {
            matches!(
                decode_request(a.intent.request.as_bytes()),
                Ok(AgentRequest::Message { .. })
            )
        });
        if !is_message && !self.owner.recover_before_launch(id)? {
            return Err("Spawn no longer has sole admission; observation only".into());
        }
        let attempt = self.owner.attempt(id)?;
        match decode_request(attempt.intent.request.as_bytes()).map_err(|e| e.to_string())? {
            AgentRequest::Message { .. } => {
                self.owner.process(&attempt.ids.agent_id)?.facts()?;
                self.waiting
                    .insert(id, Instant::now() + Duration::from_secs(15));
                self.controls.insert(id, attempt.ids.agent_id.clone());
                Ok(())
            }
            AgentRequest::Spawn { .. } => {
                if self.connecting.len() >= 4 || self.connecting.contains_key(&id) {
                    return Err("gateway connection capacity unavailable".into());
                }
                self.connecting
                    .insert(id, GatewayConnector::start(attempt.clone(), config.clone()));
                self.controls.insert(id, attempt.ids.agent_id.clone());
                Ok(())
            }
            _ => Err("read request reached execution owner".into()),
        }
    }
    fn launch_connected(
        &mut self,
        id: Uuid,
        launcher: &mut ManagedLauncher,
        config: &ManagedConfig,
        joined: JoinedGateway,
    ) -> Result<(), String> {
        let (attempt, model, binding) = joined.prepare(&self.owner)?;
        let mut permit = launcher.authorize(binding.clone())?;
        #[cfg(test)]
        crate::agent_runtime_adapter::launch_retry::test_audit(&self.owner, &attempt, "authorized");
        let (pending, collab) = PendingManagedConnection::pair(&mut permit)?;
        let (host, identity) = UnixStream::pair().map_err(|e| e.to_string())?;
        let stop = host.try_clone().map_err(|e| e.to_string())?;
        let caller = attempt.caller.clone();
        let owner = self.owner.clone();
        let identity_thread = std::thread::Builder::new()
            .name("agent-identity".into())
            .spawn(move || {
                use runtime_identity::wire::{Request, Response};
                let _ = runtime_identity::connected::serve(host, |request| {
                    let authorized = owner
                        .store()
                        .and_then(|store| store.agent_caller(caller.device_public_key))
                        .is_ok_and(|current| current.certificate() == &caller);
                    if !authorized {
                        return Response::Denied("original caller no longer current".into());
                    }
                    match request {
                        Request::Metadata => Response::Metadata {
                            display_name: "Workspace agent".into(),
                            avatar_color: [90, 120, 170],
                            group_public_key: caller.user_public_key,
                            signing_public_key: caller.device_public_key,
                            device_certificate: serde_json::to_vec(&caller).ok(),
                            threshold: 1,
                            total_shares: 1,
                            share_index: 1,
                        },
                        _ => Response::Denied("enrolled agent has no signing capability".into()),
                    }
                });
            })
            .map_err(|e| e.to_string())?;
        self.identities.insert(
            binding.launch.clone(),
            IdentityThread {
                socket: stop,
                thread: identity_thread,
            },
        );
        let data_home = config.agent_data_home.join(&binding.agent);
        std::fs::create_dir_all(&data_home).map_err(|e| e.to_string())?;
        let runtime = self.owner.runtime()?.revalidate()?;
        let services = EnrolledServiceContext::new(
            binding.clone(),
            data_home,
            runtime.runtime_epoch.clone(),
            pending.channel_id().into(),
            config.provider.clone(),
            identity,
            collab,
            model,
        )?;
        self.pending.insert(
            binding.launch.clone(),
            Pending {
                invocation: id,
                connection: pending,
                binding: binding.clone(),
                runtime,
            },
        );
        let result = launcher.launch(permit, services);
        crate::agent_runtime_adapter::agent_timing("launch_requested", id);
        #[cfg(test)]
        if result.is_ok() {
            crate::agent_runtime_adapter::launch_retry::test_audit(
                &self.owner,
                &attempt,
                "launched",
            );
        }
        result
    }
    fn capture_started(
        &mut self,
        supervisor: &launcher::managed::VerifiedManagedSupervisor,
    ) -> Result<Pending, String> {
        let pending = self
            .pending
            .remove(&supervisor.binding().launch)
            .ok_or("started process lacks exact pending descriptor")?;
        if supervisor.binding() != &pending.binding {
            return Err("started process differs from held launch binding".into());
        }
        // These facts were authenticated while this exact launch permit/runtime was live.
        // The opaque Started supplies the original supervisor identity even after shutdown.
        self.supervisors.insert(
            pending.binding.launch.clone(),
            (
                pending.invocation,
                pending.binding.clone(),
                pending.runtime.clone(),
                supervisor.process(),
            ),
        );
        Ok(pending)
    }
    pub(super) async fn pump(
        &mut self,
        launcher: &mut ManagedLauncher,
        config: &ManagedConfig,
    ) -> Result<(), String> {
        let connected: Vec<_> = self
            .connecting
            .iter()
            .filter_map(|(id, connector)| connector.is_finished().then_some(*id))
            .collect();
        for id in connected {
            // The selected task has completed. Own and join that exact handle with
            // no Store/live/slot guards, before any launch-intent call or callback.
            let connector = self
                .connecting
                .remove(&id)
                .expect("selected connector retained");
            let result = connector.join().await;
            crate::agent_runtime_adapter::agent_timing("gateway_connected", id);
            if let Err(error) =
                result.and_then(|joined| self.launch_connected(id, launcher, config, joined))
            {
                self.owner.uncertain(id, &error)?;
                self.controls.remove(&id);
            }
        }

        let events = launcher.drain_events()?;
        #[cfg(test)]
        if let Some((path, held)) = &mut self.test_held_events {
            held.extend(events);
            if held
                .iter()
                .any(|event| matches!(event, ManagedLaunchEvent::Started(_)))
            {
                // Only a real opaque event can signal this gate; it grants no authority.
                std::fs::write(path.with_extension("observed"), b"genuine Started held")
                    .map_err(|e| e.to_string())?;
            }
            return Ok(());
        }
        for event in events {
            match event {
                ManagedLaunchEvent::Started(supervisor) => {
                    let pending = self.capture_started(&supervisor)?;
                    crate::agent_runtime_adapter::agent_timing(
                        "process_started",
                        pending.invocation,
                    );
                    if supervisor.runtime_facts()? != pending.runtime {
                        return Err("started runtime differs from held launch facts".into());
                    }
                    if self.admissions.iter().filter(|t| !t.is_finished()).count() >= 4 {
                        self.owner.uncertain(
                            pending.invocation,
                            "process admission capacity unavailable",
                        )?;
                        continue;
                    }
                    let owner = self.owner.clone();
                    let sender = self.admitted_tx.clone();
                    self.admissions.push(
                        std::thread::Builder::new()
                            .name("agent-channel-admission".into())
                            .spawn(move || {
                                let result = pending
                                    .connection
                                    .admit_connection(&supervisor, owner.as_ref());
                                if sender.try_send((pending.invocation, result)).is_err() {
                                    let _ = owner.uncertain(
                                        pending.invocation,
                                        "process admission result lost",
                                    );
                                }
                            })
                            .map_err(|e| e.to_string())?,
                    );
                }
                ManagedLaunchEvent::Finished {
                    binding,
                    exit,
                    event,
                    ..
                } => {
                    tracing::info!(target: "qos_cloud_host::agent_timing", agent = %binding.agent, ?event, "managed agent process finished");
                    crate::agent_live::process_exited(
                        &binding.agent,
                        !matches!(event, launcher::managed::ProcessEvent::Exited),
                    );
                    self.finished(&binding, &exit)?;
                }
                ManagedLaunchEvent::Failed {
                    binding, reason, ..
                } => {
                    tracing::warn!(target: "qos_cloud_host::agent_timing", agent = %binding.agent, %reason, "managed agent launch failed");
                    self.uncertain_agent(&binding.agent, &reason)?;
                    self.close_identity(&binding.launch);
                }
            }
        }
        while let Ok((id, result)) = self.admitted_rx.try_recv() {
            crate::agent_runtime_adapter::agent_timing("channel_admitted", id);
            let result = result.and_then(|connection| {
                let attempt = self.owner.attempt(id)?;
                self.broker
                    .install_enrolled_connection(connection, attempt.caller.clone())?;
                Ok(())
            });
            match result {
                Ok(()) => {
                    self.waiting
                        .insert(id, Instant::now() + Duration::from_secs(15));
                }
                Err(error) => self.owner.uncertain(id, &error)?,
            }
        }
        let ready: Vec<_> = self
            .waiting
            .iter()
            .filter_map(|(id, deadline)| {
                let available = self
                    .owner
                    .attempt(*id)
                    .and_then(|a| self.owner.process(&a.ids.agent_id))
                    .and_then(|p| p.facts())
                    .is_ok_and(|facts| self.broker.enrolled_recipient_ready(&facts.channel_id));
                (available || Instant::now() >= *deadline).then_some((*id, available))
            })
            .collect();
        for (id, available) in ready {
            self.waiting.remove(&id); // Remove before the only dispatch attempt.
            crate::agent_runtime_adapter::agent_timing("recipient_ready", id);
            if !available {
                self.owner
                    .uncertain(id, "recipient subscription was not established")?;
                continue;
            }
            if let Err(error) = self.dispatch(id) {
                self.owner.uncertain(id, &error)?;
            }
        }
        let mut n = 0;
        while n < self.admissions.len() {
            if self.admissions[n].is_finished() {
                let thread = self.admissions.swap_remove(n);
                thread
                    .join()
                    .map_err(|_| "process admission worker panicked")?;
            } else {
                n += 1
            }
        }
        Ok(())
    }
    fn dispatch(&mut self, id: Uuid) -> Result<(), String> {
        let attempt = self.owner.attempt(id)?;
        let process = self.owner.process(&attempt.ids.agent_id)?;
        let facts = process.facts()?;
        let title =
            match decode_request(attempt.intent.request.as_bytes()).map_err(|e| e.to_string())? {
                AgentRequest::Spawn { task, .. } => task,
                AgentRequest::Message { text, .. } => text,
                _ => return Err("invalid enrolled control".into()),
            };
        let run = self.next_transport_run;
        self.next_transport_run = run
            .checked_add(1)
            .ok_or("transport task handle exhausted")?;
        let ids = &attempt.ids;
        let envelope = EnrolledTaskEnvelope {
            version: 1,
            message_id: ids.message_id.clone(),
            task_id: ids.task_id.clone(),
            run_id: ids.run_id.clone(),
            launch_id: ids.launch_id.clone(),
            runtime_epoch: facts.runtime.runtime_epoch,
            recipient_agent: ids.agent_id.clone(),
            channel_lease: facts.channel_id,
            // Lane E: the agent's own durable history (see agent_history).
            history: crate::agent_history::for_dispatch(
                &self.owner,
                &ids.agent_id,
                id,
                attempt.caller.user_public_key,
            ),
            task: TaskRunEnvelope {
                version: 1,
                task_id: ids.task_id.clone(),
                title,
                run_id: run,
                lease: Uuid::new_v4().to_string(),
                planning: TaskRunPlanning::Model,
                action_context: serde_json::to_value(qos_actions::ActionContext {
                    action_label: "Enrolled task".into(),
                    group: Some(qos_actions::ActionGroup {
                        id: ids.task_id.clone(),
                        label: ids.task_id.clone(),
                        kind: "task".into(),
                    }),
                    ..Default::default()
                })
                .map_err(|e| e.to_string())?,
            },
        };
        self.broker
            .dispatch_enrolled_task(id, &attempt.caller, envelope)?;
        Ok(())
    }
    fn uncertain_agent(&mut self, agent: &str, reason: &str) -> Result<(), String> {
        let controls: Vec<_> = self
            .controls
            .iter()
            .filter_map(|(id, known)| (known == agent).then_some(*id))
            .collect();
        for id in controls {
            self.waiting.remove(&id);
            self.owner.uncertain(id, reason)?;
        }
        Ok(())
    }
    fn finished(
        &mut self,
        binding: &launcher::managed::ManagedBinding,
        exit: &launcher::managed::VerifiedManagedExit,
    ) -> Result<(), String> {
        self.uncertain_agent(
            &binding.agent,
            "managed process exited; task outcome requires retained evidence",
        )?;
        if let Some((id, held, runtime, supervisor)) = self.supervisors.get(&binding.launch) {
            if binding != held
                || exit.binding() != held
                || exit.runtime_facts() != runtime
                || exit.supervisor() != *supervisor
            {
                return Err("exact held supervisor exit binding differs".into());
            }
            // Genuine owned wait + exact live Started custody. Never reconstructed from Store.
            self.owner
                .store()?
                .record_agent_process_exit(
                    &AgentControlId::new(*id).map_err(|e| e.to_string())?,
                    exit,
                )
                .map_err(|e| e.to_string())?;
            self.supervisors.remove(&binding.launch);
        }
        self.close_identity(&binding.launch);
        Ok(())
    }
    fn close_identity(&mut self, launch: &str) {
        if let Some(identity) = self.identities.remove(launch) {
            let _ = identity.socket.shutdown(Shutdown::Both);
            let _ = identity.thread.join();
        }
    }
    pub(super) async fn close(
        &mut self,
        launcher: Option<&mut ManagedLauncher>,
    ) -> Result<(), String> {
        self.owner.stop_readiness();
        let mut stopped = Ok(());
        for (_, pending) in std::mem::take(&mut self.connecting) {
            pending.abort_and_join().await;
        }
        if let Some(launcher) = launcher {
            stopped = launcher.shutdown();
            match launcher.drain_events() {
                Ok(events) => {
                    #[cfg(test)]
                    let events = if let Some((_, mut held)) = self.test_held_events.take() {
                        held.extend(events);
                        held
                    } else {
                        events
                    };
                    for event in events {
                        let result = match event {
                            ManagedLaunchEvent::Started(supervisor) => {
                                self.capture_started(&supervisor).map(drop)
                            }
                            ManagedLaunchEvent::Finished { binding, exit, .. } => {
                                self.finished(&binding, &exit)
                            }
                            ManagedLaunchEvent::Failed {
                                binding, reason, ..
                            } => self.uncertain_agent(&binding.agent, &reason),
                        };
                        if let Err(error) = result {
                            stopped = Err(error);
                        }
                    }
                }
                Err(error) => stopped = Err(error),
            }
        }
        let launches: Vec<_> = self.identities.keys().cloned().collect();
        for launch in launches {
            self.close_identity(&launch);
        }
        self.pending.clear();
        self.admitted_rx.close();
        for thread in self.admissions.drain(..) {
            let _ = thread.join();
        }
        for id in self.controls.keys() {
            self.owner
                .uncertain(*id, "runtime stopped; retained status only")?;
        }
        stopped
    }
}
