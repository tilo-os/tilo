//! The service as an instance's music origin reaches it: the music connector process the system
//! runs for the app ([`crate::connector`]), over the socket the session handed the instance. The
//! instance's process holds no network; it asks, and takes in what the catalogue answered.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tilo_socket::Stream;

use crate::service::{
    AlbumPage, ArtistPage, HomePage, Lyrics, SearchPage, Service, ServiceError, SpotifyOp,
    SpotifyReply,
};
use crate::wire::{self, Ask, FromConnector};

/// [`Service`] over the socket to the music connector process.
pub struct RemoteService {
    stream: Mutex<Stream>,
    /// The id the next question is asked under.
    next: AtomicU64,
}

impl RemoteService {
    /// The service at the far end of `stream`.
    #[must_use]
    pub fn new(stream: Stream) -> Self {
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
            .map_err(|_| ServiceError("the music connector is unusable".to_owned()))?;
        wire::write(&mut *stream, ask)
            .map_err(|error| ServiceError(format!("the music connector failed: {error}")))?;
        loop {
            match wire::read::<FromConnector>(&mut *stream) {
                Ok(Some(answer)) if answer.request_of() == request => return Ok(answer),
                // What belongs to another question is not this one's: it was answered twice.
                Ok(Some(_)) => {}
                Ok(None) => return Err(ServiceError("the music connector stopped".to_owned())),
                Err(error) => {
                    return Err(ServiceError(format!("the music connector failed: {error}")));
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
                        "the music connector answered {said} to another question"
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

    fn home(&self) -> Result<HomePage, ServiceError> {
        self.answered(
            &Ask::Home {
                request: self.next.fetch_add(1, Ordering::Relaxed),
            },
            |answer| match answer {
                FromConnector::Home { page, .. } => Some(page),
                _ => None,
            },
        )
    }

    fn artist(&self, key: &str) -> Result<ArtistPage, ServiceError> {
        self.answered(
            &Ask::Artist {
                request: self.next.fetch_add(1, Ordering::Relaxed),
                key: key.to_owned(),
            },
            |answer| match answer {
                FromConnector::Artist { page, .. } => Some(page),
                _ => None,
            },
        )
    }

    fn album(&self, key: &str) -> Result<AlbumPage, ServiceError> {
        self.answered(
            &Ask::Album {
                request: self.next.fetch_add(1, Ordering::Relaxed),
                key: key.to_owned(),
            },
            |answer| match answer {
                FromConnector::Album { page, .. } => Some(page),
                _ => None,
            },
        )
    }

    fn playlist(&self, key: &str) -> Result<AlbumPage, ServiceError> {
        self.answered(
            &Ask::Playlist {
                request: self.next.fetch_add(1, Ordering::Relaxed),
                key: key.to_owned(),
            },
            |answer| match answer {
                FromConnector::Playlist { page, .. } => Some(page),
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

    fn lyrics(&self, title: &str, artist: &str) -> Result<Lyrics, ServiceError> {
        self.answered(
            &Ask::Lyrics {
                request: self.next.fetch_add(1, Ordering::Relaxed),
                title: title.to_owned(),
                artist: artist.to_owned(),
            },
            |answer| match answer {
                FromConnector::Lyrics { lyrics, .. } => Some(lyrics),
                _ => None,
            },
        )
    }

    fn pin(&self, url: &str, name: &str) -> Result<String, ServiceError> {
        self.answered(
            &Ask::Pin {
                request: self.next.fetch_add(1, Ordering::Relaxed),
                url: url.to_owned(),
                name: name.to_owned(),
            },
            |answer| match answer {
                FromConnector::Pinned { address, .. } => Some(address),
                _ => None,
            },
        )
    }
    fn spotify(&self, op: &SpotifyOp) -> Result<SpotifyReply, ServiceError> {
        self.answered(
            &Ask::Spotify {
                request: self.next.fetch_add(1, Ordering::Relaxed),
                op: op.clone(),
            },
            |answer| match answer {
                FromConnector::Spotify { reply, .. } => Some(reply),
                _ => None,
            },
        )
    }
}
