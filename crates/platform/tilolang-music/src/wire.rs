//! What an instance's music origin and the music connector process say to each other over the
//! socket the session hands both: a question ([`Ask`]) one way, its answer
//! ([`FromConnector`]) the other. Each message is a frame: its length as four big-endian bytes,
//! then its JSON, as the mail connector's are.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

use crate::service::{AlbumPage, ArtistPage, HomePage, Lyrics, SearchPage, ServiceError};

/// The largest frame either side accepts: an artist's page with its songs fits many times over.
pub const MAX_FRAME: usize = 64 << 20;

/// What the origin asks of the connector, under the id it knows it by.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ask {
    Search {
        request: u64,
        query: String,
    },
    Home {
        request: u64,
    },
    Artist {
        request: u64,
        key: String,
    },
    Album {
        request: u64,
        key: String,
    },
    Playlist {
        request: u64,
        key: String,
    },
    Stream {
        request: u64,
        key: String,
    },
    Lyrics {
        request: u64,
        title: String,
        artist: String,
    },
    Pin {
        request: u64,
        url: String,
        name: String,
    },
}

/// The id a question is asked under.
#[must_use]
pub const fn request_of(ask: &Ask) -> u64 {
    match ask {
        Ask::Search { request, .. }
        | Ask::Home { request }
        | Ask::Artist { request, .. }
        | Ask::Album { request, .. }
        | Ask::Playlist { request, .. }
        | Ask::Stream { request, .. }
        | Ask::Lyrics { request, .. }
        | Ask::Pin { request, .. } => *request,
    }
}

/// What the connector answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FromConnector {
    Search {
        request: u64,
        page: SearchPage,
    },
    Home {
        request: u64,
        page: HomePage,
    },
    Artist {
        request: u64,
        page: ArtistPage,
    },
    Album {
        request: u64,
        page: AlbumPage,
    },
    Playlist {
        request: u64,
        page: AlbumPage,
    },
    Stream {
        request: u64,
        url: String,
    },
    Lyrics {
        request: u64,
        lyrics: Lyrics,
    },
    /// The content address the pinned bytes are kept at (`tilo-blob://…`).
    Pinned {
        request: u64,
        address: String,
    },
    /// The catalogue could not answer, or refused: why.
    Failed {
        request: u64,
        reason: String,
    },
}

/// What an answer answers, for a fault that says the connector answered another question.
#[must_use]
pub const fn kind_of(answer: &FromConnector) -> &'static str {
    match answer {
        FromConnector::Search { .. } => "a search",
        FromConnector::Home { .. } => "the home page",
        FromConnector::Artist { .. } => "an artist",
        FromConnector::Album { .. } => "an album",
        FromConnector::Playlist { .. } => "a playlist",
        FromConnector::Stream { .. } => "a stream",
        FromConnector::Lyrics { .. } => "lyrics",
        FromConnector::Pinned { .. } => "a pinned song",
        FromConnector::Failed { .. } => "a failure",
    }
}

impl FromConnector {
    /// The id of the question this answers.
    #[must_use]
    pub const fn request_of(&self) -> u64 {
        match self {
            Self::Search { request, .. }
            | Self::Home { request, .. }
            | Self::Artist { request, .. }
            | Self::Album { request, .. }
            | Self::Playlist { request, .. }
            | Self::Stream { request, .. }
            | Self::Lyrics { request, .. }
            | Self::Pinned { request, .. }
            | Self::Failed { request, .. } => *request,
        }
    }
}

/// Write `message` as one frame.
///
/// # Errors
/// The socket's error, or a message that does not encode or is larger than [`MAX_FRAME`].
pub fn write<T: Serialize>(to: &mut impl Write, message: &T) -> io::Result<()> {
    let body = serde_json::to_vec(message).map_err(io::Error::other)?;
    if body.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a music connector message larger than a frame",
        ));
    }
    let length = u32::try_from(body.len()).map_err(io::Error::other)?;
    let mut frame = Vec::with_capacity(body.len().saturating_add(4));
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&body);
    to.write_all(&frame)?;
    to.flush()
}

/// Read one frame; `None` at end of file, when the other side closed the connection.
///
/// # Errors
/// The socket's error, or a frame that does not decode or is larger than [`MAX_FRAME`].
pub fn read<T: for<'de> Deserialize<'de>>(from: &mut impl Read) -> io::Result<Option<T>> {
    let mut length = [0_u8; 4];
    match from.read_exact(&mut length) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let length = u32::from_be_bytes(length);
    let length = usize::try_from(length)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "a music frame's length"))?;
    if length > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a music connector message larger than a frame",
        ));
    }
    let mut body = vec![0_u8; length];
    from.read_exact(&mut body)?;
    serde_json::from_slice(&body).map(Some).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a music connector message that does not decode: {error}"),
        )
    })
}

/// [`ServiceError`] as a frame says it.
#[must_use]
pub fn failed(error: &ServiceError) -> String {
    error.0.clone()
}
