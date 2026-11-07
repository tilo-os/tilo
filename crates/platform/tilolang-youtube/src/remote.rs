//! The service as an instance's YouTube origin reaches it: the YouTube connector process the
//! system runs for the app ([`crate::connector`]), over the socket the session handed the
//! instance. The instance's process holds no network; it asks, and takes in what YouTube
//! answered.

use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::service::{ChannelPage, SearchPage, Service, ServiceError, TrendingPage};
use crate::wire::{self, Ask, FromConnector};

/// [`Service`] over the socket to the YouTube connector process.
pub struct RemoteService {
    stream: Mutex<UnixStream>,
    /// The id the next question is asked under.
    next: AtomicU64,
}

impl RemoteService {
    /// The service at the far end of `stream`.
    #[must_use]
    pub fn new(stream: UnixStream) -> Self {
        Self {
            stream: Mutex::new(stream),
            next: AtomicU64::new(1),
        }
    }

    /// Ask the connector `ask` and wait for what it says about it.
    fn ask(&self, ask: &Ask) -> Result<FromConnector, ServiceError> {
        let request = wire::request_of(ask);
        let mut stream = self
            .stream
            .lock()
            .map_err(|_| ServiceError("the YouTube connector is unusable".to_owned()))?;
        wire::write(&mut *stream, ask)
            .map_err(|error| ServiceError(format!("the YouTube connector failed: {error}")))?;
        loop {
            match wire::read::<FromConnector>(&mut *stream) {
                Ok(Some(answer)) if answer.request_of() == request => return Ok(answer),
                // What belongs to another question is not this one's: it was answered twice.
                Ok(Some(_)) => {}
                Ok(None) => {
                    return Err(ServiceError("the YouTube connector stopped".to_owned()));
                }
                Err(error) => {
                    return Err(ServiceError(format!(
                        "the YouTube connector failed: {error}"
                    )));
                }
            }
        }
    }

    /// Ask `ask`, and take what it answers with `found`: a failure is why it could not answer,
    /// and an answer of another kind than the question is a fault.
    fn answered<T>(
        &self,
        ask: &Ask,
        found: impl FnOnce(FromConnector) -> Option<T>,
    ) -> Result<T, ServiceError> {
        match self.ask(ask)? {
            FromConnector::Failed { reason, .. } => Err(ServiceError(reason)),
            answer => {
                let said = wire::kind_of(&answer);
                found(answer).ok_or_else(|| {
                    ServiceError(format!(
                        "the YouTube connector answered {said} to another question"
                    ))
                })
            }
        }
    }
}

impl Service for RemoteService {
    fn search(&self, query: &str) -> Result<SearchPage, ServiceError> {
        self.answered(
            &Ask::Search {
                request: self.next.fetch_add(1, Ordering::Relaxed),
                query: query.to_owned(),
            },
            |answer| match answer {
                FromConnector::Search { page, .. } => Some(page),
                _ => None,
            },
        )
    }

    fn channel(&self, key: &str) -> Result<ChannelPage, ServiceError> {
        self.answered(
            &Ask::Channel {
                request: self.next.fetch_add(1, Ordering::Relaxed),
                key: key.to_owned(),
            },
            |answer| match answer {
                FromConnector::Channel { page, .. } => Some(page),
                _ => None,
            },
        )
    }

    fn trending(&self) -> Result<TrendingPage, ServiceError> {
        self.answered(
            &Ask::Trending {
                request: self.next.fetch_add(1, Ordering::Relaxed),
            },
            |answer| match answer {
                FromConnector::Trending { page, .. } => Some(page),
                _ => None,
            },
        )
    }

    fn stream(&self, key: &str) -> Result<String, ServiceError> {
        self.answered(
            &Ask::Stream {
                request: self.next.fetch_add(1, Ordering::Relaxed),
                key: key.to_owned(),
            },
            |answer| match answer {
                FromConnector::Stream { url, .. } => Some(url),
                _ => None,
            },
        )
    }
}
