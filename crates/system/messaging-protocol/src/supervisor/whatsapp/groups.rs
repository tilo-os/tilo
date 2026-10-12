//! Fresh, private group inspection from the exact supervised SDK session.
use super::super::{AccountAuthority, AccountSupervisor};
use std::sync::Arc;
use tilo_messaging::{
    GroupClient, GroupHost, GroupList, GroupMember, GroupMembership, GroupRole, GroupSummary,
    GroupsFuture, MembershipFuture, SendError, ServiceAccount,
};
use whatsapp_rust::{Client, features::GroupMetadata};

#[derive(Clone)]
pub(crate) struct Host {
    source: Arc<Client>,
    authority: Arc<dyn AccountAuthority>,
    account: ServiceAccount,
    identity: (String, Option<String>),
    backend: crate::whatsapp_store::EncryptedBackend,
    contact: Option<tilo_messaging::MessagingClient>,
    broker: Option<Arc<crate::external_broker::ExternalBroker>>,
}
impl Host {
    fn check(&self) -> Result<(String, Option<String>), SendError> {
        self.authority.check()?;
        if !self.source.is_logged_in() || !self.source.is_connected() {
            return Err(SendError::Unavailable);
        }
        let device = self.source.persistence_manager().get_device_snapshot();
        let pn = device
            .pn
            .as_ref()
            .ok_or(SendError::Revoked)?
            .to_non_ad_string();
        let identity = (
            pn,
            device
                .lid
                .as_ref()
                .map(whatsapp_rust::Jid::to_non_ad_string),
        );
        if identity != self.identity {
            return Err(SendError::Revoked);
        }
        Ok(identity)
    }
    fn membership(
        &self,
        meta: GroupMetadata,
        identity: &(String, Option<String>),
    ) -> Result<GroupMembership, SendError> {
        if meta.has_missing_participant_identification
            || meta.participants.len() > tilo_messaging::MAX_GROUP_MEMBERS
            || !meta.participants.iter().any(|p| {
                p.jid.to_non_ad_string() == identity.0
                    || identity
                        .1
                        .as_ref()
                        .is_some_and(|lid| p.jid.to_non_ad_string() == *lid)
            })
        {
            return Err(SendError::Revoked);
        }
        let mut selves = meta
            .participants
            .iter()
            .map(|p| p.jid.to_non_ad_string())
            .filter(|id| id == &identity.0 || identity.1.as_ref() == Some(id));
        let self_member = selves.next().ok_or(SendError::Revoked)?;
        if selves.next().is_some() {
            return Err(SendError::Revoked);
        }
        Ok(GroupMembership {
            self_member,
            account: self.account.clone(),
            group: GroupSummary {
                id: meta.id.to_non_ad_string(),
                title: meta.subject,
            },
            members: meta
                .participants
                .into_iter()
                .map(|p| GroupMember {
                    address: p.jid.to_non_ad_string(),
                    role: if p.is_super_admin() {
                        GroupRole::SuperAdmin
                    } else if p.is_admin() {
                        GroupRole::Admin
                    } else {
                        GroupRole::Member
                    },
                })
                .collect(),
            announcement: meta.is_announcement,
            review: String::new(),
        })
    }
}
impl GroupHost for Host {
    fn list(&self) -> GroupsFuture {
        let host = self.clone();
        Box::pin(async move {
            let identity = host.check()?;
            let metadata = tokio::time::timeout(
                std::time::Duration::from_secs(20),
                host.source.groups().get_participating(),
            )
            .await
            .map_err(|_| SendError::Unavailable)?
            .map_err(|_| SendError::Unavailable)?;
            if host.check()? != identity {
                return Err(SendError::Revoked);
            }
            if metadata.len() > tilo_messaging::MAX_GROUPS {
                return Err(SendError::InvalidRequest);
            }
            let mut groups = Vec::with_capacity(metadata.len());
            for (id, meta) in metadata {
                if id != meta.id {
                    return Err(SendError::Revoked);
                }
                groups.push(host.membership(meta, &identity)?.group);
            }
            groups.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(GroupList {
                account: host.account,
                groups,
            })
        })
    }
    fn change(
        &self,
        request: tilo_messaging::GroupChangeRequest,
    ) -> tilo_messaging::GroupActionFuture {
        actions::change(self.clone(), request)
    }
    fn send(&self, request: tilo_messaging::GroupSendRequest) -> tilo_messaging::GroupActionFuture {
        actions::send(self.clone(), request)
    }
    fn history(&self, group: String) -> tilo_messaging::GroupHistoryFuture {
        actions::history(self.clone(), group)
    }
    fn read(&self, group: String) -> MembershipFuture {
        let host = self.clone();
        Box::pin(async move {
            let identity = host.check()?;
            let jid = group.parse().map_err(|_| SendError::InvalidRequest)?;
            let metadata = tokio::time::timeout(
                std::time::Duration::from_secs(20),
                host.source.groups().get_metadata(&jid),
            )
            .await
            .map_err(|_| SendError::Unavailable)?
            .map_err(|_| SendError::Unavailable)?;
            if host.check()? != identity || metadata.id.to_non_ad_string() != group {
                return Err(SendError::Revoked);
            }
            let review = host.snapshot(&metadata, &identity)?;
            let mut page = host.membership(metadata, &identity)?;
            host.check()?;
            page.review = host
                .backend
                .review_group(review)
                .await
                .map_err(|_| SendError::Unavailable)?;
            host.check()?;
            Ok(page)
        })
    }
}
impl AccountSupervisor {
    pub(in super::super) fn whatsapp_groups(
        &self,
        account: &ServiceAccount,
    ) -> Option<GroupClient> {
        let host = self.whatsapp_group_host(account)?;
        Some(GroupClient::new(
            self.owner.principal.clone(),
            account.clone(),
            host,
        ))
    }
    pub(crate) fn whatsapp_group_host(&self, account: &ServiceAccount) -> Option<Arc<Host>> {
        if account.service != "whatsapp"
            || account.account != self.owner.account
            || self.owner.service != crate::Service::Whatsapp
        {
            return None;
        }
        let authority = self.authority.clone()?;
        authority.check().ok()?;
        let (source, backend, contact, broker) = {
            let runtime = self.runtime.lock().ok()?;
            let values = (
                runtime.whatsapp_source.clone()?,
                runtime.whatsapp_backend.clone()?,
                runtime.client.clone(),
                runtime.whatsapp_broker.clone()?,
            );
            drop(runtime);
            values
        };
        let device = source.persistence_manager().get_device_snapshot();
        let identity = (
            device.pn.as_ref()?.to_non_ad_string(),
            device
                .lid
                .as_ref()
                .map(whatsapp_rust::Jid::to_non_ad_string),
        );
        Some(Arc::new(Host {
            source,
            authority,
            account: account.clone(),
            identity,
            backend,
            contact,
            broker: Some(broker),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use whatsapp_rust::features::{GroupParticipant, ParticipantType};
    fn required<T, E>(result: Result<T, E>) -> T {
        let Ok(value) = result else {
            panic!("controlled fixture construction failed");
        };
        value
    }
    async fn fixture() -> (
        tempfile::TempDir,
        Host,
        Arc<super::super::network::tests::Gate>,
    ) {
        let root = required(tempfile::tempdir());
        let (gate, _, network) = super::super::network::tests::fixture(false, false);
        let owner = crate::journal::Owner {
            principal: "11".repeat(32),
            device: "device".into(),
            service: crate::Service::Whatsapp,
            account: "account-a".into(),
            linking_generation: 1,
        };
        let bootstrap = required(
            super::super::WhatsappBootstrap::open_profile(
                &root.path().join("sdk"),
                owner,
                secret_vault::VaultKey::from_bytes([21; 32]),
                network,
                gate.clone(),
            )
            .await,
        );
        (
            root,
            Host {
                source: bootstrap.client(),
                authority: gate.clone(),
                account: ServiceAccount::new("whatsapp", "account-a"),
                identity: ("123@s.whatsapp.net".into(), Some("456@lid".into())),
                backend: bootstrap.backend,
                contact: None,
                broker: None,
            },
            gate,
        )
    }
    #[tokio::test]
    async fn default_sdk_group_inspection_refuses_unlinked_and_revoked_sessions() {
        let (_root, host, gate) = fixture().await;
        assert!(matches!(host.list().await, Err(SendError::Unavailable)));
        assert!(matches!(
            host.read("123@g.us".into()).await,
            Err(SendError::Unavailable)
        ));
        let request = || tilo_messaging::GroupChangeRequest {
            group: "123@g.us".into(),
            review: "review".into(),
            operation: uuid::Uuid::new_v4().to_string(),
            change: tilo_messaging::GroupChange::Leave,
        };
        assert!(matches!(
            host.change(request()).await,
            Err(SendError::Unavailable)
        ));
        assert!(matches!(
            host.send(tilo_messaging::GroupSendRequest {
                group: "123@g.us".into(),
                review: "review".into(),
                operation: uuid::Uuid::new_v4().to_string(),
                text: "text".to_owned().into()
            })
            .await,
            Err(SendError::Unavailable)
        ));
        assert!(matches!(
            host.history("123@g.us".into()).await,
            Err(SendError::Unavailable)
        ));
        gate.0.store(false, Ordering::Release);
        assert!(matches!(host.list().await, Err(SendError::Revoked)));
        assert!(matches!(
            host.read("123@g.us".into()).await,
            Err(SendError::Revoked)
        ));
        assert!(matches!(
            host.change(request()).await,
            Err(SendError::Revoked)
        ));
        assert!(matches!(
            host.history("123@g.us".into()).await,
            Err(SendError::Revoked)
        ));
        assert!(!host.source.is_logged_in());
        assert!(!host.source.is_connected());
    }
    #[tokio::test]
    async fn service_group_membership_cannot_substitute_names_or_another_account_identity() {
        let (_root, host, _gate) = fixture().await;
        let identity = ("123@s.whatsapp.net".into(), Some("456@lid".into()));
        let participant = |jid: &str| GroupParticipant {
            jid: required(jid.parse()),
            phone_number: None,
            lid: None,
            username: None,
            participant_type: ParticipantType::Admin,
            details: None,
        };
        let mut meta = GroupMetadata {
            id: required("123@g.us".parse()),
            subject: "Private group fixture".into(),
            participants: vec![participant("456@lid")],
            ..Default::default()
        };
        let page = required(host.membership(meta.clone(), &identity));
        assert_eq!(page.members[0].address, "456@lid");
        assert!(matches!(page.members[0].role, GroupRole::Admin));
        meta.participants = vec![participant("789@lid")];
        assert!(matches!(
            host.membership(meta.clone(), &identity),
            Err(SendError::Revoked)
        ));
        meta.participants[0].phone_number = Some(required("123@s.whatsapp.net".parse()));
        assert!(matches!(
            host.membership(meta.clone(), &identity),
            Err(SendError::Revoked)
        ));
        meta.participants = vec![participant("456@lid")];
        meta.has_missing_participant_identification = true;
        assert!(matches!(
            host.membership(meta, &identity),
            Err(SendError::Revoked)
        ));
    }
}

mod actions;
