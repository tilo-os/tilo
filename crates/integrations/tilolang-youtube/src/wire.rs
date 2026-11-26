//! What an instance's YouTube origin and the YouTube connector process say to each other over
//! the socket the session hands both: a question ([`Ask`]) one way, its answer
//! ([`FromConnector`]) the other. Each message is a frame: its length as four big-endian bytes,
//! then its JSON, as the mail connector's are.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

use crate::service::{
    Branding, ChannelPage, CommentsPage, PlaylistPage, SearchFilter, SearchPage, SegmentPage,
    ServiceError, TrendingPage, Votes,
};

/// The largest frame either side accepts: a channel's page with its videos fits many times over.
pub const MAX_FRAME: usize = 64 << 20;

const FRAMING: wire_frame::Framing = wire_frame::Framing::big_endian(MAX_FRAME);

/// What the origin asks of the connector, under the id it knows it by.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ask {
    Search {
        request: u64,
        query: String,
        /// What the search narrows to; left out when it narrows nothing, so a plain search's
        /// frame is what it always was.
        #[serde(default, skip_serializing_if = "SearchFilter::is_any")]
        filter: SearchFilter,
    },
    Channel {
        request: u64,
        key: String,
    },
    Trending {
        request: u64,
    },
    Stream {
        request: u64,
        key: String,
        /// The tallest picture asked for (`auto` for the best that fits); left out when the
        /// ask names none.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        quality: String,
    },
    Playlist {
        request: u64,
        key: String,
    },
    Comments {
        request: u64,
        key: String,
    },
    Segments {
        request: u64,
        key: String,
    },
    Branding {
        request: u64,
        key: String,
    },
    Votes {
        request: u64,
        key: String,
    },
}

/// The id a question is asked under.
#[must_use]
pub const fn request_of(ask: &Ask) -> u64 {
    match ask {
        Ask::Search { request, .. }
        | Ask::Channel { request, .. }
        | Ask::Playlist { request, .. }
        | Ask::Trending { request }
        | Ask::Stream { request, .. }
        | Ask::Comments { request, .. }
        | Ask::Segments { request, .. }
        | Ask::Branding { request, .. }
        | Ask::Votes { request, .. } => *request,
    }
}

/// What the connector answers. Its pages carry a segment's times as numbers, so it is compared
/// as one but never ordered.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FromConnector {
    Search {
        request: u64,
        page: SearchPage,
    },
    Channel {
        request: u64,
        page: ChannelPage,
    },
    Trending {
        request: u64,
        page: TrendingPage,
    },
    Stream {
        request: u64,
        url: String,
    },
    /// YouTube could not answer, or refused: why.
    Failed {
        request: u64,
        reason: String,
    },
    Playlist {
        request: u64,
        page: PlaylistPage,
    },
    Comments {
        request: u64,
        page: CommentsPage,
    },
    Segments {
        request: u64,
        page: SegmentPage,
    },
    Branding {
        request: u64,
        page: Branding,
    },
    Votes {
        request: u64,
        page: Votes,
    },
}

/// What an answer answers, for a fault that says the connector answered another question.
#[must_use]
pub const fn kind_of(answer: &FromConnector) -> &'static str {
    match answer {
        FromConnector::Search { .. } => "a search",
        FromConnector::Channel { .. } => "a channel",
        FromConnector::Playlist { .. } => "a playlist",
        FromConnector::Trending { .. } => "the trending list",
        FromConnector::Stream { .. } => "a stream",
        FromConnector::Comments { .. } => "a video's comments",
        FromConnector::Segments { .. } => "a video's segments",
        FromConnector::Branding { .. } => "a video's community title",
        FromConnector::Votes { .. } => "a video's votes",
        FromConnector::Failed { .. } => "a failure",
    }
}

impl FromConnector {
    /// The id of the question this answers.
    #[must_use]
    pub const fn request_of(&self) -> u64 {
        match self {
            Self::Search { request, .. }
            | Self::Channel { request, .. }
            | Self::Playlist { request, .. }
            | Self::Trending { request, .. }
            | Self::Stream { request, .. }
            | Self::Comments { request, .. }
            | Self::Segments { request, .. }
            | Self::Branding { request, .. }
            | Self::Votes { request, .. }
            | Self::Failed { request, .. } => *request,
        }
    }
}

/// Write `message` as one frame.
///
/// # Errors
/// The socket's error, or a message that does not encode or is larger than [`MAX_FRAME`].
pub fn write<T: Serialize>(to: &mut impl Write, message: &T) -> io::Result<()> {
    tilolang_tilo::origin::write_json_frame(FRAMING, to, message)
}

/// Read one frame; `None` at a clean end of file, when the other side closed the connection.
///
/// # Errors
/// The socket's error, a truncated or oversized frame (past [`MAX_FRAME`]), or one that does not
/// decode.
pub fn read<T: for<'de> Deserialize<'de>>(from: &mut impl Read) -> io::Result<Option<T>> {
    tilolang_tilo::origin::read_json_frame(FRAMING, from)
}

/// [`ServiceError`] as a frame says it.
#[must_use]
pub fn failed(error: &ServiceError) -> String {
    error.0.clone()
}
