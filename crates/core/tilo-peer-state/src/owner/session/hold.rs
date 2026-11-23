//! Holding an enrolled workspace's link for as long as the workspace is held:
//! dialling its holders, every one alike, until one answers, keeping the link
//! with that one for as long as it lasts, renewing this device's membership
//! once ownership renewed its certificate, and dialling again after each loss,
//! starting with the holder it last reached.
use super::{Connect, Linked, Reach, Scope};
use crate::join::TransportHint;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tilo_identity::identity_store::DeviceIdentity;
use tilo_server_protocol::refusal::Refusal;
use tokio::sync::Notify;

/// The first pause before dialling again; each failure doubles it.
const FIRST_PAUSE: Duration = Duration::from_secs(1);
/// The longest pause between dials.
const LONGEST_PAUSE: Duration = Duration::from_secs(60);

/// Why a hold dials none of the workspace's holders now: nothing it does
/// reaches the workspace until this changes, so the workspace cannot arrive
/// while it lasts. The hold tries again when told to ([`Link::retry`]), and
/// on its own pace where it keeps one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CannotDial {
    /// This device has no identity to take part in the workspace with.
    NoIdentity,
    /// This device's certificate is no longer one the workspace takes,
    /// and renewing it there failed (or there was nothing to renew it with).
    NotRenewed,
    /// The list of who holds the workspace could not be read.
    HoldersUnreadable,
    /// The workspace names no holder to dial.
    NoHolders,
    /// The workspace is placed on this device, whose own service is not open.
    OwnServiceStopped,
}

/// Whoever holds a workspace's link: what [`hold`] asks of it.
pub trait Holder: Send + Sync {
    /// How the workspace's holders are reached now.
    fn reach(&self) -> Reach;
    /// Whether [`hold`] dials again on its own after a pause; otherwise it
    /// dials only once told to ([`Link::retry`]).
    fn paces_itself(&self) -> bool {
        true
    }
    /// Dialling `via`, one of the workspace's holders, failed with `reason`.
    /// The hold no longer counts as establishing a link unless it goes on to
    /// another holder. The reason is typed, so a workspace its owner deleted
    /// is told from a holder that is merely out of reach.
    fn dial_failed(
        &self,
        via: TransportHint,
        reason: Refusal,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
    /// The hold dials none of the workspace's holders now, for `cause`.
    fn cannot_dial(&self, cause: CannotDial) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
    /// Run `linked`, the workspace's link, pinned by `scope`, until it ends.
    fn run(
        &self,
        linked: Linked,
        scope: Arc<Scope>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

/// What a [`hold`] shares with its holder.
#[derive(Default)]
pub struct Link {
    retry: Notify,
    establishing: AtomicBool,
}

impl Link {
    /// Note that a dial is under way before the hold's first one: what the
    /// hold dials for a link, so a link between starting a hold and its
    /// first dial is not taken for one with no link to come.
    pub fn dialling(&self) {
        self.establishing.store(true, Ordering::Release);
    }
    /// No dial will follow: nothing here is establishing a link.
    pub fn settled(&self) {
        self.establishing.store(false, Ordering::Release);
    }
    /// Dial again now rather than after the pause: something that may let
    /// the dial succeed changed.
    pub fn retry(&self) {
        self.retry.notify_one();
    }
    /// Whether a dial, or a renewal of this device's membership ahead of
    /// one, is under way.
    #[must_use]
    pub fn establishing(&self) -> bool {
        self.establishing.load(Ordering::Acquire)
    }
}

enum Outcome {
    /// A link ran, or the material moved on: dial again soon.
    Again,
    /// No holder answered: dial again after a growing pause.
    Failed,
    /// Nothing to dial until [`Link::retry`].
    Wait,
}

/// Hold `connect`'s workspace's link for `holder`, as `identity`, until the
/// workspace is no longer held here. Without an identity there is nothing to
/// dial: the holder hears so, again each time it asks for a retry.
pub async fn hold(
    connect: Connect,
    identity: Option<DeviceIdentity>,
    holder: &dyn Holder,
    link: &Link,
) {
    let Some(identity) = identity else {
        link.settled();
        while Connect::new(connect.owner().clone(), connect.workspace()).is_ok() {
            holder.cannot_dial(CannotDial::NoIdentity).await;
            wait(link, None).await;
        }
        return;
    };
    let paced = holder.paces_itself();
    let mut pause = FIRST_PAUSE;
    // The holder the last link reached: dialled first next time, so a holder
    // that went away is not waited on again before every redial.
    let mut last: Option<[u8; 32]> = None;
    while Connect::new(connect.owner().clone(), connect.workspace()).is_ok() {
        let outcome = Box::pin(once(&connect, holder, link, &identity, &mut last)).await;
        link.settled();
        match outcome {
            Outcome::Again => {
                pause = FIRST_PAUSE;
                wait(link, paced.then_some(FIRST_PAUSE)).await;
            }
            Outcome::Failed => {
                wait(link, paced.then_some(pause)).await;
                pause = pause.saturating_mul(2).min(LONGEST_PAUSE);
            }
            Outcome::Wait => wait(link, None).await,
        }
    }
    link.settled();
}

async fn wait(link: &Link, pause: Option<Duration>) {
    match pause {
        Some(pause) => {
            tokio::select! {
                () = tokio::time::sleep(pause) => {}
                () = link.retry.notified() => {}
            }
        }
        None => link.retry.notified().await,
    }
}

async fn once(
    connect: &Connect,
    holder: &dyn Holder,
    link: &Link,
    identity: &DeviceIdentity,
    last: &mut Option<[u8; 32]>,
) -> Outcome {
    let Ok((hint, cert, _)) = connect.material(identity) else {
        return renew(connect, holder, link, identity).await;
    };
    let reach = holder.reach();
    let own = cert.device_public_key;
    // A workspace placed on this device waits for its own service to open.
    if super::hosted_by(&hint, &own) && reach.own_host.is_none() {
        holder.cannot_dial(CannotDial::OwnServiceStopped).await;
        return Outcome::Wait;
    }
    let mut holders = match connect.holders(&own) {
        Ok(holders) if holders.is_empty() => {
            holder.cannot_dial(CannotDial::NoHolders).await;
            return Outcome::Failed;
        }
        Ok(holders) => holders,
        Err(error) => {
            tracing::warn!(workspace = %connect.workspace(), %error, "the workspace's holders cannot be read");
            holder.cannot_dial(CannotDial::HoldersUnreadable).await;
            return Outcome::Failed;
        }
    };
    if let Some(index) = last.and_then(|last| {
        holders
            .iter()
            .position(|via| via.host.device_public_key == last)
    }) {
        holders.rotate_left(index);
    }
    link.establishing.store(true, Ordering::Release);
    let mut holders = holders.into_iter().peekable();
    let (linked, via) = loop {
        let Some(via) = holders.next() else {
            return Outcome::Failed;
        };
        let signer = Arc::clone(&identity.signer);
        match reach.session(connect, &via, signer).await {
            Ok(linked) => break (linked, via),
            Err(reason) => {
                link.establishing
                    .store(holders.peek().is_some(), Ordering::Release);
                tracing::info!(
                    workspace = %connect.workspace(),
                    host = %hex::encode(via.host.device_public_key),
                    %reason,
                    "a holder of the workspace is out of reach; dialling the next"
                );
                holder.dial_failed(via, reason).await;
            }
        }
    };
    *last = Some(via.host.device_public_key);
    // The workspace's security may have moved on while dialling (another
    // member joined): the link is still this device's as long as its
    // enrollment and certificate are, and its access checks out now.
    let Some(continuity) =
        connect
            .material(identity)
            .ok()
            .and_then(|(current_hint, device, continuity)| {
                (current_hint == hint && device == cert).then_some(continuity)
            })
    else {
        return Outcome::Again;
    };
    link.establishing.store(false, Ordering::Release);
    let scope = Arc::new(Scope::via(connect.clone(), hint, via, cert, continuity));
    holder.run(linked, scope).await;
    Outcome::Again
}

/// Ownership no longer matches the workspace's record: once it renewed the
/// certificate, this device proves its membership with the renewal.
async fn renew(
    connect: &Connect,
    holder: &dyn Holder,
    link: &Link,
    identity: &DeviceIdentity,
) -> Outcome {
    if !matches!(connect.renewal(identity), Ok(Some(_))) {
        holder.cannot_dial(CannotDial::NotRenewed).await;
        return Outcome::Failed;
    }
    let workspace = connect.workspace();
    link.establishing.store(true, Ordering::Release);
    match holder.reach().renew(connect, identity).await {
        Ok(_) => {
            tracing::info!(%workspace, "renewed this device's certificate in the workspace");
            Outcome::Again
        }
        Err(error) => {
            tracing::warn!(%workspace, %error, "this device's renewed certificate was not taken");
            link.settled();
            holder.cannot_dial(CannotDial::NotRenewed).await;
            Outcome::Failed
        }
    }
}
