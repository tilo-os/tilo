//! Bounded job lanes on acknowledged routes: a few routes at a time, each
//! served by a worker that lives as long as its [`Registration`], so a second
//! live lane on a route is refused rather than chosen between.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

/// The most routes one kind of lane serves at once.
const MAX_ROUTES: usize = 16;

/// The live routes of one kind of lane.
pub(crate) struct Lane<E> {
    /// What a refusal names the lane, as `no <what> route`.
    what: &'static str,
    routes: OnceLock<Mutex<BTreeMap<String, Weak<E>>>>,
}

impl<E> Lane<E> {
    pub(crate) const fn new(what: &'static str) -> Self {
        Self {
            what,
            routes: OnceLock::new(),
        }
    }

    fn routes(&self) -> &Mutex<BTreeMap<String, Weak<E>>> {
        self.routes.get_or_init(Mutex::default)
    }

    /// The live lane on `route`.
    ///
    /// # Errors
    /// When no lane is live there, or the registry is poisoned.
    pub(crate) fn endpoint(&self, route: &str) -> Result<Arc<E>, String> {
        self.routes()
            .lock()
            .map_err(|_| format!("{} lane unavailable", self.what))?
            .get(route)
            .and_then(Weak::upgrade)
            .ok_or_else(|| format!("no {} route", self.what))
    }

    /// Admit `endpoint` on `route`, reusing the slots of lanes that are gone.
    ///
    /// # Errors
    /// When the route is empty or too long, already has a live lane, or too
    /// many are live.
    pub(crate) fn admit(&self, route: String, endpoint: &Arc<E>) -> Result<(), String> {
        let mut entries = self
            .routes()
            .lock()
            .map_err(|_| format!("{} lane unavailable", self.what))?;
        entries.retain(|_, endpoint| endpoint.strong_count() != 0);
        if route.is_empty()
            || route.len() > 1024
            || entries.contains_key(&route)
            || entries.len() >= MAX_ROUTES
        {
            return Err(format!(
                "{} route is ambiguous or exceeds admission bounds",
                self.what
            ));
        }
        entries.insert(route, Arc::downgrade(endpoint));
        Ok(())
    }
}

/// A lane kept admitted on its route while this lives; dropping it retires
/// the lane and its worker.
pub(crate) struct Registration<E> {
    _endpoint: Arc<E>,
    worker: tokio::task::JoinHandle<()>,
}

impl<E> Registration<E> {
    pub(crate) fn new(endpoint: Arc<E>, worker: tokio::task::JoinHandle<()>) -> Self {
        Self {
            _endpoint: endpoint,
            worker,
        }
    }
}

impl<E> Drop for Registration<E> {
    fn drop(&mut self) {
        self.worker.abort();
    }
}
