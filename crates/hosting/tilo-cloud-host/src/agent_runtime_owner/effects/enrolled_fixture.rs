//! macOS-runnable enrolled task fixture (tests only).
//!
//! Production launch evidence comes from the Linux launcher (verified runtime,
//! supervisor and kernel channel). Tests on macOS stand in exactly that part:
//! they admit through the real Store admission, then record the launch and
//! process-binding facts with fixture runtime/process data. Every later step
//! (dispatch, recipient acceptance, task actions, steps, TaskFinished) goes
//! through the production enrolled owner and broker paths.
use super::*;
use launcher::managed::{ManagedRuntimeFacts, ProcessIdentity};

pub(crate) struct EnrolledFixtureTask {
    pub(crate) custody: AgentAttemptCustody,
    pub(crate) ids: AgentExecutionIds,
    pub(crate) intent: AgentIntent,
    pub(crate) process: ChannelProcessFacts,
}

impl Store {
    pub(crate) fn enrolled_fixture_task(
        &mut self,
        caller: &VerifiedAgentCaller,
        invocation: uuid::Uuid,
        grants: Vec<GrantAction>,
    ) -> EnrolledFixtureTask {
        let room = self.state.room.clone();
        let intent = AgentIntent {
            scope: AgentInvocationScope {
                machine: self.state.machine_id.clone(),
                room: room.clone(),
                host: room.clone(),
                caller: caller.certificate().clone(),
            },
            route: AgentProviderRoute {
                app_id: "so.tilo.agent".into(),
                app_peer: tilo_relay::room_workspace_id(&room).unwrap().to_string(),
                cluster_id: room.clone(),
                record_id: String::new(),
                resource: format!("workspace:{room}:agents"),
                resource_kind: "workspace.agents".into(),
                action: "spawn".into(),
            },
            descriptor_revision: "a".repeat(64),
            request: String::from_utf8(
                tilo_agent_control_protocol::canonical_request(&AgentRequest::Spawn {
                    version: 1,
                    name: "Handoff worker".into(),
                    task: "Insert two tables".into(),
                    grant_set_id: uuid::Uuid::from_u128(99).to_string(),
                })
                .unwrap(),
            )
            .unwrap(),
            grants,
        };
        let AgentAdmissionResult::Fresh(admitted) = self
            .admit_agent_candidate(caller, invocation, &intent)
            .expect("real Store admission")
        else {
            panic!("fixture admission already existed")
        };
        let ids = admitted.ids.clone();
        let custody = admitted.into_custody();
        let runtime = RetainedRuntimeFacts {
            runtime_epoch: uuid::Uuid::new_v4().to_string(),
            package_id: "so.tilo.agent".into(),
            package_hash: "a".repeat(64),
            executable_hash: "b".repeat(64),
            runtime_hash: "c".repeat(64),
        };
        let (_, binding) = self
            .claim_launch_candidate(&custody, runtime.clone())
            .expect("real launch intent fact");
        let process = RetainedChannelProcessFacts {
            binding: binding_to_data(&binding),
            runtime: runtime.clone(),
            process: RetainedProcessIdentity {
                pid: 4242,
                start: 1,
            },
            supervisor: RetainedProcessIdentity {
                pid: 4241,
                start: 1,
            },
            channel_id: uuid::Uuid::new_v4().to_string(),
        };
        let governance = custody.admission.change.expected_governance.clone();
        self.append_effect_fact(
            &custody.id(),
            &governance,
            AgentRuntimeFact::ProcessBound {
                launch_fact: format!("{invocation}:launch_intended"),
                process: process.clone(),
            },
        )
        .expect("process binding fact");
        let live = ChannelProcessFacts {
            binding,
            runtime: ManagedRuntimeFacts {
                runtime_epoch: runtime.runtime_epoch,
                package_id: runtime.package_id,
                package_hash: runtime.package_hash,
                executable_hash: runtime.executable_hash,
                runtime_hash: runtime.runtime_hash,
            },
            process: ProcessIdentity {
                pid: process.process.pid,
                start: process.process.start,
            },
            supervisor: ProcessIdentity {
                pid: process.supervisor.pid,
                start: process.supervisor.start,
            },
            channel_id: process.channel_id,
        };
        EnrolledFixtureTask {
            custody,
            ids,
            intent,
            process: live,
        }
    }
}
