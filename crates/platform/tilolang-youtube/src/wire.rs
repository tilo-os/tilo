//! What an instance's YouTube origin and the YouTube connector process say to each other over
//! the socket the session hands both: a question ([`Ask`]) one way, its answer
//! ([`FromConnector`]) the other. Each message is a frame: its length as four big-endian bytes,
//! then its JSON, as the mail connector's are.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

use crate::service::{ChannelPage, PlaylistPage, SearchPage, ServiceError, TrendingPage};

/// The largest frame either side accepts: a channel's page with its videos fits many times over.
pub const MAX_FRAME: usize = 64 << 20;

const FRAMING: wire_frame::Framing = wire_frame::Framing::big_endian(MAX_FRAME);

/// What the origin asks of the connector, under the id it knows it by.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ask {
    Search { request: u64, query: String },
    Channel { request: u64, key: String },
    Trending { request: u64 },
    Stream { request: u64, key: String },
    Playlist { request: u64, key: String },
}

/// The id a question is asked under.
#[must_use]
pub const fn request_of(ask: &Ask) -> u64 {
    match ask {
        Ask::Search { request, .. }
        | Ask::Channel { request, .. }
        | Ask::Playlist { request, .. }
        | Ask::Trending { request }
        | Ask::Stream { request, .. } => *request,
    }
}

/// What the connector answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_wire_fixture<T>(value: &T, expected: &[u8])
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let mut encoded = Vec::new();
        write(&mut encoded, value).expect("written");
        assert_eq!(encoded, expected);

        let mut reading = expected;
        assert_eq!(
            read::<T>(&mut reading).expect("readable").as_ref(),
            Some(value)
        );
        assert_eq!(read::<T>(&mut reading).expect("readable"), None);
    }

    #[test]
    fn a_question_and_its_answer_round_trip_as_frames() {
        let dir = tempfile::tempdir().expect("a directory");
        let path = dir.path().join("frames");
        let ask = Ask::Search {
            request: 7,
            query: "phone reviews".to_owned(),
        };
        {
            let mut file = std::fs::File::create(&path).expect("a file");
            write(&mut file, &ask).expect("written");
            write(
                &mut file,
                &FromConnector::Failed {
                    request: 7,
                    reason: "unreachable".to_owned(),
                },
            )
            .expect("written");
        }
        let mut file = std::fs::File::open(&path).expect("the file");
        let read_back: Ask = read(&mut file).expect("readable").expect("a frame");
        assert_eq!(read_back, ask);
        let answer: FromConnector = read(&mut file).expect("readable").expect("a frame");
        assert_eq!(answer.request_of(), 7);
        assert!(read::<Ask>(&mut file).expect("readable").is_none());
    }

    #[test]
    fn wire_bytes_match_frozen_fixtures() {
        // Literal bytes captured from main before migrating wire.rs to wire-frame.
        const SEARCH_WIRE: &[u8] =
            b"\x00\x00\x000{\"Search\":{\"request\":7,\"query\":\"phone reviews\"}}";
        const TRENDING_WIRE: &[u8] = b"\x00\x00\x00\x1a{\"Trending\":{\"request\":8}}";
        const STREAM_WIRE: &[u8] =
            b"\x00\x00\x00B{\"Stream\":{\"request\":9,\"url\":\"https://video.example/watch?v=abc\"}}";
        const FAILED_WIRE: &[u8] =
            b"\x00\x00\x00/{\"Failed\":{\"request\":7,\"reason\":\"unreachable\"}}";

        assert_wire_fixture(
            &Ask::Search {
                request: 7,
                query: "phone reviews".to_owned(),
            },
            SEARCH_WIRE,
        );
        assert_wire_fixture(&Ask::Trending { request: 8 }, TRENDING_WIRE);
        assert_wire_fixture(
            &FromConnector::Stream {
                request: 9,
                url: "https://video.example/watch?v=abc".to_owned(),
            },
            STREAM_WIRE,
        );
        assert_wire_fixture(
            &FromConnector::Failed {
                request: 7,
                reason: "unreachable".to_owned(),
            },
            FAILED_WIRE,
        );
    }

    #[test]
    fn write_flushes_and_refuses_partial_prefix_and_oversized_frames() {
        let mut writer = io::BufWriter::new(Vec::new());
        write(&mut writer, &Ask::Trending { request: 1 }).expect("written");
        assert!(writer.buffer().is_empty(), "write must flush the writer");

        let mut empty = &b""[..];
        assert_eq!(read::<Ask>(&mut empty).expect("readable"), None);

        for partial in [&[0_u8][..], &[0, 0][..], &[0, 0, 0][..]] {
            let mut slice = partial;
            let error = read::<Ask>(&mut slice).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        }

        let mut truncated_body = &b"\x00\x00\x00\x1a{\"Trending"[..];
        let error = read::<Ask>(&mut truncated_body).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);

        let mut oversized = u32::MAX.to_be_bytes().to_vec();
        oversized.extend_from_slice(b"{}");
        let error = read::<FromConnector>(&mut oversized.as_slice()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);

        let mut invalid_json = &b"\x00\x00\x00\x02{}"[..];
        let error = read::<FromConnector>(&mut invalid_json).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
