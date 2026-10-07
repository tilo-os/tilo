//! Current native owner and private device custody; presentation labels grant no authority.
use super::super::{NativeCertificates, NativeOwnerCommitService};
use collab_types::caps::{Actor, CapDecision, ResourceRef, Verb, decide_capability};
use std::{sync::Arc, time::SystemTime};
use tilo_action_broker::{
    managed_host::ManagedHostCustody, peer_actions::CertificateAuthority,
    services::ManagedTaskLaunchContext,
};
use tilo_user_identity::DeviceCert;

pub(super) struct Custody {
    pub owner: Arc<NativeOwnerCommitService>,
    pub certificates: NativeCertificates,
}
impl Custody {
    fn current<T>(
        &self,
        room: &str,
        inspect: impl FnOnce(&tilo_joined_room::NativeCommittedView) -> Result<T, String>,
    ) -> Result<T, String> {
        let room = crate::shell::state::RoomId::parse(room).ok_or("invalid managed native room")?;
        let owner = self
            .owner
            .owner
            .as_ref()
            .ok_or("managed native owner unavailable")?;
        owner.health()?;
        if let Some(network) = &self.owner.network {
            network.check_native_custody(owner, room)?;
            if network.governed_elsewhere(room) {
                return Err(
                    "managed launch requires native task commits from the local governing owner"
                        .into(),
                );
            }
        }
        owner.with_current(room, inspect)
    }
    pub(super) fn person(&self, context: &ManagedTaskLaunchContext) -> Result<(), String> {
        context.recheck()?;
        let caller = context.certificate();
        if caller.capabilities.can_act_as_host {
            return Err("managed launch requires an ordinary person device".into());
        }
        self.current(context.room(), |view| {
            if !view
                .peer()
                .certificates()
                .iter()
                .any(|certificate| certificate == caller)
            {
                return Err(
                    "managed launch person certificate is not current native evidence".into(),
                );
            }
            let now = SystemTime::now();
            let (role, caps, person) = view.peer().permissions(caller.device_public_key, now)?;
            let actor = Actor::person(Some(role), person).in_room(context.room());
            let seconds = now
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_err(|_| "native task clock invalid")?
                .as_secs();
            if !matches!(
                role,
                ui::permissions::Role::Owner | ui::permissions::Role::Editor
            ) || decide_capability(
                &actor,
                &ResourceRef::Workspace,
                Verb::Write,
                &caps,
                Some(seconds),
            ) != CapDecision::Allow
            {
                return Err(
                    "native task launch person lacks current workspace write authority".into(),
                );
            }
            Ok(())
        })
    }
}
impl ManagedHostCustody for Custody {
    fn authorize_host(&self, room: &str, certificate: &DeviceCert) -> Result<(), String> {
        let (key, current) = tilo_identity_host::broker::trusted_current_device(room)
            .map_err(|error| format!("native host private custody unavailable: {error}"))?;
        if &current != certificate
            || key.verifying_key().to_bytes() != certificate.device_public_key
        {
            return Err("native host private attestor changed".into());
        }
        self.current(room, |view| {
            if view.device() != certificate
                || !view
                    .peer()
                    .certificates()
                    .iter()
                    .any(|cert| cert == certificate)
            {
                return Err("native host certificate is not current owner evidence".into());
            }
            if view.host().unwrap_or_else(|| view.governor()) != certificate
                || (!certificate.capabilities.can_act_as_host
                    && view.peer().authority().device_public_key != certificate.device_public_key)
            {
                return Err("native workspace is hosted by a different certified device".into());
            }
            // Without the network's certified route, only the actual local governor can commit.
            if self.owner.network.is_none() && view.governor() != certificate {
                return Err("delegated host native task commit transport is unavailable".into());
            }
            view.peer()
                .permissions(certificate.device_public_key, SystemTime::now())?;
            Ok(())
        })
    }
}
impl CertificateAuthority for Custody {
    fn policy_time(&self) -> Result<Option<tilo_action_broker::peer_actions::PolicyTime>, String> {
        self.certificates.policy_time()
    }
    fn authorize(&self, room: uuid::Uuid, cert: &DeviceCert) -> Result<(), String> {
        self.certificates.authorize(room, cert)
    }
    fn authorize_join(&self, room: uuid::Uuid, cert: &DeviceCert) -> Result<(), String> {
        self.certificates.authorize_join(room, cert)
    }
    fn authorize_with_policy_time(
        &self,
        room: uuid::Uuid,
        cert: &DeviceCert,
        time: &tilo_action_broker::peer_actions::PolicyTime,
    ) -> Result<(), String> {
        self.certificates
            .authorize_with_policy_time(room, cert, time)
    }
    fn authorize_host_worker(
        &self,
        route: &tilo_action_broker::route::CrdtRoute,
        cert: &DeviceCert,
    ) -> Result<(), String> {
        self.authorize_host(&route.cluster_id, cert)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn current_native_governor_requires_its_actual_private_device_and_committed_certificate() {
        let mut home = tilo_test_env::TestHome::new().unwrap();
        let identity_root = home.config().join("native-managed-test-identity");
        home.env()
            .set("TILO_IDENTITY_ROOT", &identity_root)
            .set(tilo_identity_host::provisioning::FILE_SECRETS_ENV, "1")
            .set(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent-tilo-test-bus",
            )
            .remove("TILO_USER_IDENTITY")
            .remove("TILO_IDENTITY_BROKER_FD");
        let identity = runtime_identity::identity_dir_from_env().unwrap();
        crate::firstboot::ensure_throwaway_test_identity(
            &identity,
            &crate::firstboot::collaboration_identity_paths(),
        )
        .unwrap();
        let room = crate::shell::state::RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
        let registry = tilo_identity_host::registry::IdentityRegistryStore::at(&identity);
        let selected = registry
            .load_reconciled()
            .unwrap()
            .default_identity
            .unwrap();
        registry
            .bind_workspace(&room.to_string(), &selected)
            .unwrap();
        let (key, certificate) =
            tilo_identity_host::broker::trusted_current_device(&room.to_string()).unwrap();
        let mut initial = crate::shell::state::WorkspaceTree::bootstrap_default();
        assert!(initial.set_room(initial.desktop.active, room));
        initial.persist_path = Some(home.config().join("native-managed-state.json"));
        let tree = initial.shared();
        let owner = crate::shell::native_workspace_owner::open(tree.clone()).unwrap();
        let _governor =
            tilo_joined_room::testing::enroll_interactive_governor(&owner, &key, &certificate)
                .unwrap();
        let custody = Custody {
            owner: Arc::new(NativeOwnerCommitService::new(tree, None)),
            certificates: NativeCertificates::default(),
        };
        assert!(!certificate.capabilities.can_act_as_host);
        custody
            .authorize_host(&room.to_string(), &certificate)
            .unwrap();
        let mut relabeled = certificate.clone();
        relabeled.device_label = "Uncommitted host label".into();
        assert!(
            custody
                .authorize_host(&room.to_string(), &relabeled)
                .is_err()
        );
        let mut elevated = certificate.clone();
        elevated.capabilities.can_act_as_host = true;
        assert!(
            custody
                .authorize_host(&room.to_string(), &elevated)
                .is_err()
        );
        assert!(
            custody
                .authorize_host(&uuid::Uuid::new_v4().to_string(), &certificate)
                .is_err()
        );
        drop(custody);
        drop(owner);
        drop(home);
    }
}
