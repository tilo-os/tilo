//! Provenance for an existing device-issued Files session home grant.
//! This ledger cannot issue a grant: only exact rows still present in the
//! current workspace can survive a signed native projection. It is never
//! serialized, replicated, or reconstructed after session expiry.
use super::store::NativeStateRuntime;
use crate::shell::state::{
    CapOverride, Effect, Expiry, Principal, RoomId, Scope, Subject, Workspace, WorkspaceId,
};
use std::collections::HashMap;
use tilo_identity_host::broker::{LocalDeviceIdentity, trusted_current_device_public};

const MAX_CONSENTS: usize = 4096;
#[derive(Default)]
pub(super) struct Consents(HashMap<(WorkspaceId, RoomId, String), Consent>);
struct Consent {
    identity: LocalDeviceIdentity,
    capability: CapOverride,
    retired: bool,
}
impl NativeStateRuntime {
    pub(crate) fn remember_files_home_consent(
        &self,
        workspace: &Workspace,
        ids: &[String],
    ) -> bool {
        let Ok(identity) = trusted_current_device_public(&workspace.room.to_string()) else {
            return false;
        };
        let eligible: Vec<_> = workspace
            .sandbox
            .iter()
            .filter(|capability| {
                ids.contains(&capability.id)
                    && capability.effect == Effect::Grant
                    && capability.subject
                        == Subject::Principal(Principal::App(
                            crate::shell::search::providers::applications::SYSTEM_FILE_MANAGER
                                .into(),
                        ))
                    && capability.expiry == Expiry::EndOfSession
                    && matches!(capability.scope, Scope::FsPath(_))
            })
            .collect();
        let mut consents = self.0.local_filesystem_consents.lock();
        if eligible.len() != ids.len()
            || consents.0.len().saturating_add(eligible.len()) > MAX_CONSENTS
        {
            return false;
        }
        for capability in eligible {
            consents.0.insert(
                (workspace.id, workspace.room, capability.id.clone()),
                Consent {
                    identity: identity.clone(),
                    capability: capability.clone(),
                    retired: false,
                },
            );
        }
        drop(consents);
        true
    }

    pub(crate) fn preserve_files_home_consent(
        &self,
        workspace: &Workspace,
        peer: &tilo_peer_state::StatePeer,
    ) -> Vec<CapOverride> {
        let mut consents = self.0.local_filesystem_consents.lock();
        if !consents
            .0
            .keys()
            .any(|(slot, room, _)| *slot == workspace.id && *room == workspace.room)
        {
            return Vec::new();
        }
        let current = trusted_current_device_public(&workspace.room.to_string()).ok();
        let mut preserved = Vec::new();
        for ((slot, room, _), consent) in &mut consents.0 {
            if *slot != workspace.id || *room != workspace.room || consent.retired {
                continue;
            }
            let admitted = peer.permissions(
                consent.identity.device_public_key,
                std::time::SystemTime::now(),
            );
            if current.as_ref() != Some(&consent.identity)
                || !workspace.sandbox.contains(&consent.capability)
                || !admitted
                    .as_ref()
                    .is_ok_and(|authority| authority.person == consent.identity.principal_id)
            {
                consent.retired = true;
                continue;
            }
            preserved.push(consent.capability.clone());
        }
        drop(consents);
        preserved
    }

    pub(crate) fn forget_files_home_consent(&self, workspace: WorkspaceId, id: &str) {
        for ((slot, _, capability), consent) in &mut self.0.local_filesystem_consents.lock().0 {
            if *slot == workspace && capability == id {
                consent.retired = true;
            }
        }
    }

    pub(crate) fn files_home_consent_current(
        &self,
        workspace: &Workspace,
        capability: &CapOverride,
        actor: &str,
    ) -> bool {
        let consents = self.0.local_filesystem_consents.lock();
        let Some(consent) = consents
            .0
            .get(&(workspace.id, workspace.room, capability.id.clone()))
        else {
            return true;
        };
        let eligible = !consent.retired
            && consent.capability == *capability
            && consent.identity.principal_id == actor;
        let identity = consent.identity.clone();
        drop(consents);
        eligible
            && trusted_current_device_public(&workspace.room.to_string())
                .ok()
                .as_ref()
                == Some(&identity)
    }
}
