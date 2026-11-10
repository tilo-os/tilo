//! Holding an enrolled workspace's link for as long as the workspace is held:
//! dialling the governor its enrollment pins, standing in with another
//! host its governance names while the governor is out of reach, renewing
//! this device's membership once custody renewed its certificate, and
//! dialling again after each loss.
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
use tilo_identity_host::custody::HostIdentity;
use tokio::sync::Notify;

/// The first pause before dialling again; each failure doubles it.
const FIRST_PAUSE: Duration = Duration::from_secs(1);
/// The longest pause between dials.
const LONGEST_PAUSE: Duration = Duration::from_secs(60);

/// Whoever holds a workspace's link: what [`hold`] asks of it.
pub trait Holder: Send + Sync {
    /// How the workspace's host is reached now.
    fn reach(&self) -> Reach;
    /// Whether [`hold`] dials again on its own after a pause; otherwise it
    /// dials only once told to ([`Link::retry`]).
    fn paces_itself(&self) -> bool {
        true
    }
    /// Dialling `via`, the workspace's `governor` or a host standing in for it,
    /// failed with `reason`. The hold no longer counts as establishing a
    /// link unless it goes on to another host.
    fn dial_failed(
        &self,
        via: TransportHint,
        governor: bool,
        reason: String,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
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
    /// Nothing reached the host: dial again after a growing pause.
    Failed,
    /// Nothing to dial until [`Link::retry`].
    Wait,
}

/// Hold `connect`'s workspace's link for `holder`, as `identity`, until the
/// workspace is no longer held here. Without custody there is nothing to dial.
pub async fn hold(
    connect: Connect,
    identity: Option<HostIdentity>,
    holder: &dyn Holder,
    link: &Link,
) {
    let Some(identity) = identity else {
        // Nothing will ever be dialled here.
        link.settled();
        return;
    };
    let paced = holder.paces_itself();
    let mut pause = FIRST_PAUSE;
    while Connect::new(connect.owner().clone(), connect.workspace()).is_ok() {
        let outcome = Box::pin(once(&connect, holder, link, &identity)).await;
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

/// The link `once` dials, and the pin it is checked against.
struct Dialled {
    linked: Linked,
    via: TransportHint,
}

async fn once(
    connect: &Connect,
    holder: &dyn Holder,
    link: &Link,
    identity: &HostIdentity,
) -> Outcome {
    let Ok((hint, cert, _)) = connect.material(identity) else {
        return renew(connect, holder, link, identity).await;
    };
    let reach = holder.reach();
    let own = cert.device_public_key;
    // A workspace placed on this device waits for its host to run.
    if super::hosted_by(&hint, &own) && reach.own_host.is_none() {
        return Outcome::Wait;
    }
    let Ok(key) = identity.signing_key() else {
        return Outcome::Failed;
    };
    let dial = |via: TransportHint, reach: Reach| {
        let key = key.clone();
        async move {
            reach
                .session(connect, &via, key)
                .await
                .map(|linked| Dialled { linked, via })
        }
    };
    link.establishing.store(true, Ordering::Release);
    let governor = dial(hint.clone(), reach.clone()).await;
    let dialled = match governor {
        Ok(dialled) => dialled,
        Err(reason) => {
            // A host that did not answer stands for one out of reach: the
            // workspace goes on through the next host its governance names.
            let next = connect.next_host(&own, None).ok().flatten();
            link.establishing.store(next.is_some(), Ordering::Release);
            holder.dial_failed(hint.clone(), true, reason).await;
            let Some(dialled) = fail_over(connect, holder, link, (own, next), |via| {
                dial(via, reach.clone())
            })
            .await
            else {
                return Outcome::Failed;
            };
            dialled
        }
    };
    let mut current = dialled;
    loop {
        // The workspace's security may have moved on while dialling (another
        // member joined): the link is still this device's as long as
        // its host and certificate are, and its standing checks out now.
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
        let Dialled { linked, via } = current;
        let reaches_governor = via == hint;
        let scope = Arc::new(Scope::via(
            connect.clone(),
            hint.clone(),
            via,
            cert.clone(),
            continuity,
        ));
        if reaches_governor {
            holder.run(linked, scope).await;
            return Outcome::Again;
        }
        // A host stands in for the governor until the governor's own
        // link is up again.
        let mut run = holder.run(linked, scope);
        current = loop {
            tokio::select! {
                () = &mut run => return Outcome::Again,
                () = wait(link, holder.paces_itself().then_some(LONGEST_PAUSE)) => {}
            }
            match dial(hint.clone(), reach.clone()).await {
                Ok(back) => break back,
                Err(reason) => holder.dial_failed(hint.clone(), true, reason).await,
            }
        };
    }
}

/// Custody no longer matches the workspace's record: once it renewed the
/// certificate, this device proves its membership with the renewal.
async fn renew(
    connect: &Connect,
    holder: &dyn Holder,
    link: &Link,
    identity: &HostIdentity,
) -> Outcome {
    if !matches!(connect.renewal(identity), Ok(Some(_))) {
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
            Outcome::Failed
        }
    }
}

/// Dial `next` and each host the workspace names after it until one answers.
async fn fail_over<F, D>(
    connect: &Connect,
    holder: &dyn Holder,
    link: &Link,
    (own, mut next): ([u8; 32], Option<TransportHint>),
    dial: D,
) -> Option<Dialled>
where
    D: Fn(TransportHint) -> F,
    F: Future<Output = Result<Dialled, String>>,
{
    loop {
        let via = next?;
        tracing::info!(
            workspace = %connect.workspace(),
            host = %hex::encode(via.host.device_public_key),
            "the workspace's governor is out of reach; dialling another of its hosts"
        );
        match dial(via.clone()).await {
            Ok(dialled) => return Some(dialled),
            Err(reason) => {
                next = connect.next_host(&own, Some(&via)).ok().flatten();
                link.establishing.store(next.is_some(), Ordering::Release);
                holder.dial_failed(via, false, reason).await;
            }
        }
    }
}
