//! How a portal backend answers a request: the
//! `org.freedesktop.impl.portal.Request` response code with its results.
//! Backends compute an [`Outcome`] with `?` and turn it into the wire
//! [`Reply`] once, so no method spells the codes itself.

use std::collections::HashMap;

use zbus::zvariant::OwnedValue;

/// The results a granted request answers with.
pub type Results = HashMap<String, OwnedValue>;

/// A request's answer on the wire: 0 granted, 1 cancelled, 2 failed.
pub type Reply = (u32, Results);

/// Why a request ended without results.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// The user (or a policy acting for them) declined.
    Cancelled,
    /// The request was malformed, unauthorised or could not be served.
    Failed,
}

/// A request's result before it is put on the wire.
pub type Outcome = Result<Results, Ended>;

/// The wire [`Reply`] for `outcome`.
#[must_use]
pub fn reply(outcome: Outcome) -> Reply {
    match outcome {
        Ok(results) => (0, results),
        Err(Ended::Cancelled) => (1, Results::new()),
        Err(Ended::Failed) => (2, Results::new()),
    }
}

/// Runs a request and answers with its [`Reply`]. Taking the request as a
/// future lets an `async` block compute its [`Outcome`] with `?`.
pub async fn answer(request: impl Future<Output = Outcome>) -> Reply {
    reply(request.await)
}

/// Fails the request unless `condition` holds, for `?`.
///
/// # Errors
///
/// [`Ended::Failed`] when `condition` is false.
pub fn ensure(condition: bool) -> Result<(), Ended> {
    if condition {
        Ok(())
    } else {
        Err(Ended::Failed)
    }
}

/// Fails the request on a missing value or any error, for `?`.
pub trait OrFail<T> {
    /// `Ok` with the value, or [`Ended::Failed`].
    ///
    /// # Errors
    ///
    /// [`Ended::Failed`] when there is no value.
    fn or_fail(self) -> Result<T, Ended>;
}

impl<T> OrFail<T> for Option<T> {
    fn or_fail(self) -> Result<T, Ended> {
        self.ok_or(Ended::Failed)
    }
}

impl<T, E> OrFail<T> for Result<T, E> {
    fn or_fail(self) -> Result<T, Ended> {
        self.map_err(|_| Ended::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_map_to_the_request_response_codes() {
        let mut results = Results::new();
        results.insert("granted".into(), OwnedValue::from(true));
        let (code, granted) = reply(Ok(results));
        assert_eq!((code, granted.len()), (0, 1));
        assert_eq!(reply(Err(Ended::Cancelled)), (1, Results::new()));
        assert_eq!(reply(Err(Ended::Failed)), (2, Results::new()));
    }
}
