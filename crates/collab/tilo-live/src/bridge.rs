//! Another relay of one scope, carried into this device's own: a subscriber
//! session to a relay a sibling member serves, whose broadcasts are
//! republished into the relay this device serves
//! ([`crate::relay::Relay::bridge`]).
//!
//! Members of one workspace can end up on different relays — the first relay
//! one device reaches is not the first another reaches — and without this a
//! member would hear only those that landed on the same relay. What crosses
//! is sealed end to end: the bridge copies objects it cannot open, exactly as
//! the relay between them does.

use std::time::Duration;

use crate::admit::{AdmissionError, Connected, Dial, OriginTask, origin_config};
use crate::live;
use crate::plane::PlaneError;
use crate::relay::Relay;
use crate::scope::Scope;

/// Bytes the bridged relay's broadcasts may cache here.
const BYTES: u64 = 8 * 1024 * 1024;
/// How long an object of the other relay is retained here: as long as a plane
/// retains its own.
const RETAIN: Duration = Duration::from_secs(30);

/// One bridged relay: the subscriber session, its origin, and the copy into
/// the local relay. Dropping it stops carrying the other relay.
pub struct Bridge {
    _connected: Connected,
    _source: OriginTask,
    _copy: crate::relay::Bridge,
}

impl Bridge {
    /// Subscribe at the relay `dial` reaches, admitted with `grant`, and
    /// republish everything it serves for `scope` into `local`.
    ///
    /// # Errors
    ///
    /// [`PlaneError::Denied`] when the relay refuses the grant,
    /// [`PlaneError::Unavailable`] when it is not reached or `scope` cannot be
    /// spelled as a subscription.
    pub async fn join(
        dial: &impl Dial,
        grant: &[u8],
        scope: &Scope,
        local: &Relay,
        within: Duration,
    ) -> Result<Self, PlaneError> {
        let (incoming, source) = OriginTask::produce(origin_config(BYTES, RETAIN));
        let prefix = live::participants(scope);
        let pattern =
            moq_net::Pattern::subtree(prefix.as_str()).map_err(|_| PlaneError::Unavailable)?;
        let subscribe = incoming
            .scope("", &moq_net::Patterns::from(pattern))
            .map_err(|_| PlaneError::Unavailable)?;
        let connected = dial
            .dial(grant, None, Some(subscribe.clone()), within)
            .await
            .map_err(|error| match error {
                AdmissionError::Denied | AdmissionError::Invalid => PlaneError::Denied,
                AdmissionError::Unavailable => PlaneError::Unavailable,
            })?;
        let copy = local.bridge(subscribe.consume());
        Ok(Self {
            _connected: connected,
            _source: source,
            _copy: copy,
        })
    }
}
