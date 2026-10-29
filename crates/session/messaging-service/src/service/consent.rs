//! Network reach for one provider start, asked of the person once through
//! the compositor's protected prompt. No app request, account row or saved
//! grant gives a provider the network.

use std::sync::Arc;
use std::time::{Duration, Instant};

use launcher::owned_service::ServiceCancellation;
use tilo_messaging::SendError;

use super::identity::Identity;
use crate::door::{Call, DoorLink, Reply};

const SIGNAL_HOSTS: &[&str] = &[
    "chat.signal.org",
    "cdn.signal.org",
    "cdn2.signal.org",
    "cdn3.signal.org",
    "storage.signal.org",
    "cdsi.signal.org",
    "svr2.signal.org",
];
const WHATSAPP_HOSTS: &[&str] = &["web.whatsapp.com"];
/// How long the person has to answer.
const ANSWER: Duration = Duration::from_secs(60);

fn hosts(original: &Identity) -> Result<&'static [&'static str], SendError> {
    match original.owner.service {
        tilo_messaging_protocol::Service::Signal => Ok(SIGNAL_HOSTS),
        tilo_messaging_protocol::Service::Whatsapp => Ok(WHATSAPP_HOSTS),
        _ => Err(SendError::InvalidRequest),
    }
}

/// What a provider start asks consent with: its cancellation and the door.
#[derive(Clone)]
pub(crate) struct Policy {
    pub cancellation: ServiceCancellation,
    pub door: DoorLink,
}

/// The person's consent for one start, alive while its owner is.
pub(crate) struct Consent {
    original: Identity,
    cancellation: ServiceCancellation,
}

impl Consent {
    pub(crate) fn check(&self) -> Result<(), SendError> {
        self.original.check()?;
        if self.cancellation.is_cancelled() {
            return Err(SendError::Revoked);
        }
        Ok(())
    }

    pub(crate) fn hosts(&self) -> std::io::Result<Vec<String>> {
        self.check()
            .map_err(|_| std::io::ErrorKind::PermissionDenied)?;
        Ok(hosts(&self.original)
            .map_err(|_| std::io::ErrorKind::PermissionDenied)?
            .iter()
            .map(|host| (*host).into())
            .collect())
    }
}

/// Ask the person to let `original`'s provider reach its hosts, once.
/// Blocks; call it on a blocking worker.
///
/// # Errors
/// Refused, unanswered, cancelled, or the owner's admission ended.
pub(crate) fn request(original: Identity, policy: &Policy) -> Result<Arc<Consent>, SendError> {
    let consent = Arc::new(Consent {
        original,
        cancellation: policy.cancellation.clone(),
    });
    consent.check()?;
    let hosts = hosts(&consent.original)?;
    let key = format!("messages-network-{}", uuid::Uuid::new_v4());
    let mut answer = policy.door.call(Call::Consent {
        key: key.clone(),
        hosts: hosts.iter().map(|host| (*host).into()).collect(),
    });
    let deadline = Instant::now()
        .checked_add(ANSWER)
        .ok_or(SendError::Unavailable)?;
    let outcome = loop {
        if let Err(error) = consent.check() {
            break Err(error);
        }
        match answer.try_recv() {
            Ok(Reply::Done) => break Ok(()),
            Ok(_) | Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                break Err(SendError::Revoked);
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
        }
        if Instant::now() >= deadline {
            break Err(SendError::Unavailable);
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    if let Err(error) = outcome {
        drop(policy.door.call(Call::ConsentWithdrawn { key }));
        return Err(error);
    }
    consent.check()?;
    Ok(consent)
}
