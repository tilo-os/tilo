//! What an instance's YouTube origin and the YouTube connector process say to each other over
//! the socket the session hands both: a question ([`Ask`]) one way, its answer
//! ([`FromConnector`]) the other. Each message is a frame: its length as four big-endian bytes,
//! then its JSON, as the mail connector's are.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

use crate::service::{ChannelPage, SearchPage, ServiceError, TrendingPage};

/// The largest frame either side accepts: a channel's page with its videos fits many times over.
pub const MAX_FRAME: usize = 64 << 20;

/// What the origin asks of the connector, under the id it knows it by.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ask {
    Search { request: u64, query: String },
    Channel { request: u64, key: String },
    Trending { request: u64 },
    Stream { request: u64, key: String },
}

/// The id a question is asked under.
#[must_use]
pub const fn request_of(ask: &Ask) -> u64 {
    match ask {
        Ask::Search { request, .. }
        | Ask::Channel { request, .. }
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
}

impl FromConnector {
    /// The id of the question this answers.
    #[must_use]
    pub const fn request_of(&self) -> u64 {
        match self {
            Self::Search { request, .. }
            | Self::Channel { request, .. }
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
    let body = serde_json::to_vec(message).map_err(io::Error::other)?;
    if body.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a YouTube connector message larger than a frame",
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
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "a YouTube frame's length"))?;
    if length > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a YouTube connector message larger than a frame",
        ));
    }
    let mut body = vec![0_u8; length];
    from.read_exact(&mut body)?;
    serde_json::from_slice(&body).map(Some).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a YouTube connector message that does not decode: {error}"),
        )
    })
}

/// [`ServiceError`] as a frame says it.
#[must_use]
pub fn failed(error: &ServiceError) -> String {
    error.0.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
