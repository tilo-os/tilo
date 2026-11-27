//! Tilo's HTTP API (`/v1`) on a connection to the device's daemon: what an SDK asks when it runs
//! on this computer. A request ([`ApiRequest`]) goes out with its body as the frame's binary body;
//! the device answers it as the connection is admitted (an app with its own grants, never its
//! person's), with the answer's status and headers (`ApiResponse`), then its body in pieces
//! ([`ApiBytes`] as `ApiBody`), last `ApiEnd`. A stream of events is the same answer whose body
//! goes on until it ends or the connection closes.

use serde::{Deserialize, Serialize};

/// The most bytes one piece of an answer's body carries.
pub const MAX_API_PIECE: usize = 256 * 1024;

/// The most bytes one request's body carries: a file larger than that goes up in pieces, each
/// its own `Content-Range` put.
pub const MAX_API_BODY: usize = 16 * 1024 * 1024;

/// One request of the API, as HTTP names it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiRequest {
    pub method: String,
    /// The path, with its query (`/v1/workspaces/…/resources?limit=50`).
    pub target: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<(String, String)>,
    /// How many bytes of body follow the frame.
    #[serde(default)]
    pub body: u32,
}

/// What a request was answered with, before its body.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiHead {
    pub status: u16,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<(String, String)>,
}

/// One piece of an answer's body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiBytes {
    pub correlation: u64,
    /// The bytes, as hex digits.
    #[serde(with = "hex_piece")]
    pub bytes: Vec<u8>,
}

mod hex_piece {
    use serde::{Deserialize as _, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let digits = String::deserialize(deserializer)?;
        if digits.len() > super::MAX_API_PIECE * 2 {
            return Err(serde::de::Error::custom(
                "a piece of an answer over its bound",
            ));
        }
        hex::decode(digits).map_err(serde::de::Error::custom)
    }
}
