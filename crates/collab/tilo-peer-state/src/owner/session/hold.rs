//! Holding an enrolled room's session for as long as the room is held:
//! dialling the governor its enrollment pins, standing in with another
//! host its governance names while the governor is out of reach, renewing
//! this device's membership once custody renewed its certificate, and
//! dialling again after each loss.
use super::{Connect, DialError, Reach, Scope, UNANSWERED_BEFORE_PROBE};
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
use tilo_relay_session::RelayCoeditSession;
use tokio::sync::Notify;

/// The first pause before dialling again; each failure doubles it.
const FIRST_PAUSE: Duration = Duration::from_secs(1);
/// The longest pause between dials.
const LONGEST_PAUSE: Duration = Duration::from_secs(60);

/// Whoever holds a room's session: what [`hold`] asks of it.
pub trait Holder: Send + Sync {
    /// How the room's host is reached now.
    fn reach(&self) -> Reach;
    /// Whether [`hold`] dials again on its own after a pause; otherwise it
    /// dials only once told to ([`Link::retry`]).
    fn paces_itself(&self) -> bool {
        true
    }
    /// Dialling `via`, the room's `governor` or a host standing in for it,
    /// failed with `error`. The hold no longer counts as establishing a
    /// session unless it goes on to another host.
    fn dial_failed(
        &self,
        via: TransportHint,
        governor: bool,
        error: DialError,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
    /// Run `session`, pinned by `scope`, until it ends.
    fn run(
        &self,
        session: RelayCoeditSession,
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
    /// A session ran, or the material moved on: dial again soon.
    Again,
    /// Nothing reached the host: dial again after a growing pause.
    Failed,
    /// Nothing to dial until [`Link::retry`].
    Wait,
}

/// Hold `connect`'s room's session for `holder`, as `identity`, until the
/// room is no longer held here. Without custody there is nothing to dial.
pub async fn hold(
    connect: Connect,
    identity: Option<HostIdentity>,
    holder: &dyn Holder,
    link: &Link,
) {
    let Some(identity) = identity else {
        return;
    };
    let paced = holder.paces_itself();
    let mut pause = FIRST_PAUSE;
    let mut unanswered = 0;
    while Connect::new(connect.owner().clone(), connect.room()).is_ok() {
        let outcome = Box::pin(once(&connect, holder, link, &identity, &mut unanswered)).await;
        link.establishing.store(false, Ordering::Release);
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

/// The session `once` dials, and the pins it is checked against.
struct Dialled {
    session: RelayCoeditSession,
    via: TransportHint,
}

async fn once(
    connect: &Connect,
    holder: &dyn Holder,
    link: &Link,
    identity: &HostIdentity,
    unanswered: &mut u32,
) -> Outcome {
    let room = connect.room();
    let Ok((hint, cert, revocations, _)) = connect.material(identity) else {
        return renew(connect, holder, link, identity).await;
    };
    let reach = holder.reach();
    let own = cert.device_public_key;
    // A room placed on this device waits for its host to run.
    if super::hosted_by(&hint, &own) && reach.own_host.is_none() {
        return Outcome::Wait;
    }
    let Ok(ws) = connect
        .owner()
        .with_current(room, |view| Ok(view.workspace()))
    else {
        return Outcome::Wait;
    };
    let dial = |via: TransportHint, reach: Reach, probe: bool| {
        let (cert, key, revocations) = (
            cert.clone(),
            identity.device_signing_key.clone(),
            revocations.clone(),
        );
        async move {
            reach
                .session(ws, &via, cert, key, revocations, probe)
                .await
                .map(|session| Dialled { session, via })
        }
    };
    link.establishing.store(true, Ordering::Release);
    // Over iroh a host from before versioned handshakes closes the stream on
    // this device's hello without a word, as a refusal does: after a few,
    // ask it the way such a host reads.
    let probe = reach.https.is_none() && *unanswered >= UNANSWERED_BEFORE_PROBE;
    let governor = dial(hint.clone(), reach.clone(), probe).await;
    *unanswered = match &governor {
        Err(DialError::NoAnswer(_)) => unanswered.saturating_add(1),
        _ => 0,
    };
    let dialled = match governor {
        Ok(dialled) => dialled,
        Err(error) => {
            // A host that did not answer stands for one out of reach: the
            // room goes on through the next host its governance names.
            let next = match error {
                DialError::NoAnswer(_) | DialError::Failed(_) => {
                    connect.next_host(&own, None).ok().flatten()
                }
                DialError::PeerVersion { .. } | DialError::NotAdmitted => None,
            };
            link.establishing.store(next.is_some(), Ordering::Release);
            holder.dial_failed(hint.clone(), true, error).await;
            let stand_in = Reach {
                https: None,
                ..reach.clone()
            };
            let Some(dialled) = fail_over(connect, holder, link, (own, next), |via| {
                dial(via, stand_in.clone(), false)
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
        // The room's security may have moved on while dialling (another
        // member joined): the session is still this device's as long as
        // its host and certificate are, and its standing checks out now.
        let Some(continuity) =
            connect
                .material(identity)
                .ok()
                .and_then(|(current_hint, device, _, continuity)| {
                    (current_hint == hint && device == cert).then_some(continuity)
                })
        else {
            return Outcome::Again;
        };
        link.establishing.store(false, Ordering::Release);
        let Dialled { session, via } = current;
        let reaches_governor = via == hint;
        let scope = Arc::new(Scope::via(
            connect.clone(),
            hint.clone(),
            via,
            cert.clone(),
            continuity,
        ));
        if reaches_governor {
            holder.run(session, scope).await;
            return Outcome::Again;
        }
        // A host stands in for the governor until the governor's own
        // session is up again.
        let mut run = holder.run(session, scope);
        current = loop {
            tokio::select! {
                () = &mut run => return Outcome::Again,
                () = wait(link, holder.paces_itself().then_some(LONGEST_PAUSE)) => {}
            }
            match dial(hint.clone(), reach.clone(), false).await {
                Ok(back) => break back,
                Err(error) => holder.dial_failed(hint.clone(), true, error).await,
            }
        };
    }
}

/// Custody no longer matches the room's record: once it renewed the
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
    let room = connect.room();
    link.establishing.store(true, Ordering::Release);
    match holder.reach().renew(connect, identity).await {
        Ok(_) => {
            tracing::info!(%room, "renewed this device's certificate in the room");
            Outcome::Again
        }
        Err(error) => {
            tracing::warn!(%room, %error, "this device's renewed certificate was not taken");
            Outcome::Failed
        }
    }
}

/// Dial `next` and each host the room names after it until one answers.
async fn fail_over<F, D>(
    connect: &Connect,
    holder: &dyn Holder,
    link: &Link,
    (own, mut next): ([u8; 32], Option<TransportHint>),
    dial: D,
) -> Option<Dialled>
where
    D: Fn(TransportHint) -> F,
    F: Future<Output = Result<Dialled, DialError>>,
{
    loop {
        let via = next?;
        tracing::info!(
            room = %connect.room(),
            host = %hex::encode(via.host.device_public_key),
            "the room's governor is out of reach; dialling another of its hosts"
        );
        match dial(via.clone()).await {
            Ok(dialled) => return Some(dialled),
            Err(error) => {
                next = connect.next_host(&own, Some(&via)).ok().flatten();
                link.establishing.store(next.is_some(), Ordering::Release);
                holder.dial_failed(via, false, error).await;
            }
        }
    }
}
