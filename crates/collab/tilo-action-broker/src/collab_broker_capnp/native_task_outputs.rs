//! Link actual durable results, never proposed plans or public-progress prose.
use super::{CapnpBrokerState, DurableInvocation, PendingTaskRun};
use crate::services::NativeTaskActionRequirement;
use tilo_actions::task_messaging::{TaskOutputRef, TaskOutputStatus, TaskRunAddress};

impl CapnpBrokerState {
    pub(super) fn retain_native_output_links(
        &self,
        run: &PendingTaskRun,
        record: &DurableInvocation,
        requirement: &NativeTaskActionRequirement,
    ) -> Result<(), String> {
        if record.uncertain {
            return Ok(());
        }
        let tilo_actions::InvocationUpdate::Succeeded { output, .. } = &record.update else {
            return Ok(());
        };
        let original = run
            .native
            .as_ref()
            .ok_or("native output run binding absent")?;
        let binding = requirement
            .binding
            .as_ref()
            .ok_or("native output action binding absent")?;
        if binding.task_id != original.task_id
            || binding.agent_id != original.agent_id
            || binding.epoch != original.epoch
            || binding.starter != original.starter
        {
            return Err("native output retained action belongs to another run".into());
        }
        // Retained authorization supplied this exact admitted action revision.
        // The pending launch envelope is not a substitute for that provenance.
        let source = TaskRunAddress {
            task_id: binding.task_id.clone(),
            run_epoch: binding.epoch,
            agent_id: binding.agent_id.clone(),
        };
        // The journal terminal is written only after the SDK result has passed
        // its advertised contract (and, for effects, the owner receipt barrier).
        let proof = blake3::hash(
            &serde_json::to_vec(&(&record.key, &record.intent, &record.update))
                .map_err(|e| e.to_string())?,
        )
        .to_hex()
        .to_string();
        let read = requirement.need.kind == tilo_actions::ActionKind::Read;
        let mut resources = output.resources.clone();
        if read && resources.is_empty() {
            resources.push(requirement.resource.clone());
        }
        let record_id = record
            .intent
            .get("record_id")
            .and_then(serde_json::Value::as_str)
            .ok_or("native output record identity absent")?;
        let refs = {
            let surfaces = self
                .surfaces
                .lock()
                .map_err(|_| "native output publication unavailable")?;
            resources
                .into_iter()
                .filter_map(|resource| {
                    // Resolve the exact result id on its actual app/record, never
                    // fall back to the app root or to an arbitrary other provider.
                    let surface = surfaces.values().find(|surface| {
                        surface.live
                            && surface.route.cluster_id == run.cluster_id
                            && surface.app_id == requirement.app
                            && surface.route.record_id == record_id
                            && surface.actions.iter().any(|action| {
                                action.resource == resource
                                    && action.action == "open"
                                    && action.operation_kind == tilo_actions::OperationKind::View
                                    && action.view.is_some()
                                    && action.params.iter().all(|parameter| !parameter.required)
                            })
                    })?;
                    Some(TaskOutputRef {
                        version: 1,
                        task_run: source.clone(),
                        invocation: record.key.invocation.0.to_string(),
                        provider: super::launchable::launchable_provider(surface),
                        resource,
                        open_action: "open".into(),
                        durable_proof: proof.clone(),
                        // SDK resource results can reveal an existing item. Without
                        // an explicit mutation proof, do not assert it was created.
                        status: if read {
                            TaskOutputStatus::Viewed
                        } else {
                            TaskOutputStatus::Returned
                        },
                    })
                })
                .collect::<Vec<_>>()
        };
        if refs.is_empty() {
            return Ok(());
        }
        self.native_tasks(&run.cluster_id)
            .ok_or("native output owner unavailable")?
            .record_outputs(&run.cluster_id, binding, refs, requirement)
    }
}
