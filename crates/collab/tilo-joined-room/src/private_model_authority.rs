//! Owner-private approved model/account snapshots. Shared controls carry only a
//! content binding; reading a full profile requires authenticated run custody.
use crate::{NativeCommittedView, NativeWorkspaceOwner, RoomId};
use std::{collections::BTreeMap, time::SystemTime};
use tilo_desktop_model::task::WorkspaceTaskModelAuthority;
use tilo_peer_state::workspace_task::{
    WorkspaceTaskAuthority, WorkspaceTaskSnapshot, private_model_contract_reference,
};

pub(crate) fn validate(
    values: &BTreeMap<String, WorkspaceTaskModelAuthority>,
) -> Result<(), String> {
    if values.len() > 4096
        || serde_json::to_vec(values).map_err(|e| e.to_string())?.len() > 16 * 1024 * 1024
    {
        return Err("private model snapshot capacity exhausted".into());
    }
    for model in values.values() {
        let _: crate::task_admission::model::NativeModelContract =
            serde_json::from_value(model.contract.clone()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

impl NativeCommittedView {
    pub(crate) fn hydrate_private_models(
        &self,
        snapshot: &mut WorkspaceTaskSnapshot,
    ) -> Result<(), String> {
        for (id, control) in &mut snapshot.controls {
            let Some(public) = &mut control.model_authority else {
                continue;
            };
            let Some(full) = self.record.private_task_text.model_authorities.get(id) else {
                continue;
            };
            // Retained snapshots from an earlier run are never authority for a
            // later run, even when its provider selection happens to be equal.
            if full.epoch != public.epoch {
                continue;
            }
            let mut bound = full.clone();
            bound.contract = private_model_contract_reference(&bound.contract)?;
            if &bound != public {
                return Err("private model snapshot binding changed".into());
            }
            *public = full.clone();
        }
        Ok(())
    }
}

impl NativeWorkspaceOwner {
    /// Private production read for the trusted physical-host broker or approving
    /// person. The transport must supply `authority`, never application JSON.
    /// The normal private instruction read authenticates the exact current
    /// certificate, host pin, agent, and run before returning any profile bytes.
    pub fn workspace_task_private_model_contract(
        &self,
        room: RoomId,
        binding: &crate::task_policy::NativeTaskBinding,
        authority: &WorkspaceTaskAuthority,
        now: SystemTime,
    ) -> Result<crate::task_admission::model::NativeModelContract, String> {
        let generation = self.with_current(room, |view| Ok(view.generation()))?;
        self.workspace_task_private_instructions(
            room,
            &binding.task_id,
            binding.epoch,
            authority,
            now,
        )?;
        if matches!(authority, WorkspaceTaskAuthority::Agent { agent_id, .. } if agent_id != &binding.agent_id)
        {
            return Err("private model reader agent changed".into());
        }
        self.with_current(room, |view| {
            if view.generation() != generation {
                return Err("private model authorization changed; retry the read".into());
            }
            crate::task_admission::model::NativeModelContract::approved_for_run(view, binding)
        })
    }
}
