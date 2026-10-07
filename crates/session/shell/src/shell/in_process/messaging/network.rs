//! Temporary network consent through the compositor's protected portal sheet.
//! No app request, account row or persisted workspace capability grants egress.
use super::{Arc, NativeMessagingIdentity, SendError};
use crate::portal::{PortalCaller, PromptOutcome, PromptQueueHandle};
use launcher::owned_service::ServiceCancellation;

const SIGNAL_HOSTS: &[&str] = &[
    "chat.signal.org",
    "cdn.signal.org",
    "cdn2.signal.org",
    "cdn3.signal.org",
    "storage.signal.org",
    "cdsi.signal.org",
    "svr2.signal.org",
];
fn hosts(original: &NativeMessagingIdentity) -> Result<&'static [&'static str], SendError> {
    match original.owner.service {
        tilo_messaging_protocol::Service::Signal => Ok(SIGNAL_HOSTS),
        tilo_messaging_protocol::Service::Whatsapp => Ok(&["web.whatsapp.com"]),
        _ => Err(SendError::InvalidRequest),
    }
}
#[derive(Clone)]
pub(super) struct Policy {
    pub cancellation: ServiceCancellation,
    pub prompts: Option<PromptQueueHandle>,
}
pub(super) struct Consent {
    original: NativeMessagingIdentity,
    cancellation: ServiceCancellation,
}
impl Consent {
    pub(super) fn check(&self) -> Result<(), SendError> {
        self.original.check()?;
        if self.cancellation.is_cancelled() {
            return Err(SendError::Revoked);
        }
        Ok(())
    }
    pub(super) fn hosts(&self) -> std::io::Result<Vec<String>> {
        self.check()
            .map_err(|_| std::io::ErrorKind::PermissionDenied)?;
        Ok(hosts(&self.original)
            .map_err(|_| std::io::ErrorKind::PermissionDenied)?
            .iter()
            .map(|host| (*host).into())
            .collect())
    }
}
struct Pending {
    queue: PromptQueueHandle,
    key: String,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.queue.cancel_enqueued_request(&self.key);
    }
}
pub(super) fn request(
    original: NativeMessagingIdentity,
    policy: &Policy,
) -> Result<Arc<Consent>, SendError> {
    let consent = Arc::new(Consent {
        original,
        cancellation: policy.cancellation.clone(),
    });
    consent.check()?;
    let hosts = hosts(&consent.original)?;
    let queue = policy.prompts.clone().ok_or(SendError::Unavailable)?;
    let pending = Pending {
        queue,
        key: format!("messages-network-{}", uuid::Uuid::new_v4()),
    };
    let mut response = pending.queue.enqueue_for_request(
        PortalCaller::new(
            crate::shell::state::WorkspaceId(consent.original.workspace),
            "so.tilo.messages",
        ),
        crate::shell::state::Capability::Network {
            hosts: Some(hosts.iter().map(|host| (*host).into()).collect()),
            scope: crate::shell::state::GRANT_ONCE,
        },
        pending.key.clone(),
    );
    crate::session_runtime::runtime().map_err(|_| SendError::Unavailable)?.handle().block_on(async {
        let timeout = tokio::time::sleep(std::time::Duration::from_secs(60));
        tokio::pin!(timeout);
        loop {
            tokio::select! {
                result = &mut response => {
                    consent.check()?;
                    // This request never writes an Always grant or revives one.
                    return match result {
                        Ok(PromptOutcome::AllowOnce | PromptOutcome::AllowSession) => Ok(consent.clone()),
                        _ => Err(SendError::Revoked),
                    };
                }
                () = &mut timeout => return Err(SendError::Unavailable),
                () = tokio::time::sleep(std::time::Duration::from_millis(100)) => consent.check()?,
            }
        }
    })
}
