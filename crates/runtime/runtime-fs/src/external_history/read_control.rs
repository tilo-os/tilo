//! Closed History results on the existing independently enrolled Read lane.
//! Routing identifiers and result bytes never grant source or record access.
use super::{Deserialize, Serialize, id, io, refused};
pub const HISTORY_PROJECTION_QUERY: &str = "user.tilo.history";
pub const RECORD_ID_QUERY: &str = "user.tilo.record-id";
pub fn valid_query(query: &str) -> bool {
    query == HISTORY_PROJECTION_QUERY
        || query == RECORD_ID_QUERY
        || fs_protocol::files::FileHistoryReadQueryV1::parse_xattr(query).is_ok()
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case", deny_unknown_fields)]
pub enum HistoryReadReply {
    Retired {
        request_id: String,
    },
    Adopted {
        request_id: String,
        completed_request_id: String,
    },
    Refused {
        request_id: String,
    },
    Read {
        request_id: String,
        bytes: Vec<u8>,
    },
}
impl HistoryReadReply {
    // Byte arrays in JSON can expand each byte to four characters.
    pub const LIMIT: usize = 256 * 1024;
    pub fn valid(&self) -> bool {
        match self {
            Self::Adopted {
                request_id,
                completed_request_id,
            } => id(request_id) && id(completed_request_id),
            Self::Refused { request_id } | Self::Retired { request_id } => id(request_id),
            Self::Read { request_id, bytes } => {
                id(request_id)
                    && !bytes.is_empty()
                    && bytes.len() <= fs_protocol::files::MAX_HISTORY_READ_BYTES
            }
        }
    }
    /// # Errors
    /// Encodes bounded descriptive responses, never native admission.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        if !self.valid() {
            return Err(refused());
        }
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        if bytes.len() > Self::LIMIT {
            return Err(refused());
        }
        Ok(bytes)
    }
    /// # Errors
    /// Refuses unexpected routing identities or malformed output.
    pub fn decode(bytes: &[u8], expected: &str) -> io::Result<Self> {
        if bytes.is_empty() || bytes.len() > Self::LIMIT {
            return Err(refused());
        }
        let value: Self = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        let actual = match &value {
            Self::Retired { request_id }
            | Self::Adopted { request_id, .. }
            | Self::Refused { request_id }
            | Self::Read { request_id, .. } => request_id,
        };
        if !value.valid() || actual != expected {
            return Err(refused());
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const REQUEST: &str = "11111111-2222-4333-8444-555555555555";
    #[test]
    fn bounded_history_reply_does_not_accept_unrelated_or_untyped_success() {
        let reply = HistoryReadReply::Read {
            request_id: REQUEST.into(),
            bytes: vec![1, 2, 3],
        };
        let encoded = reply.encode().unwrap();
        assert!(HistoryReadReply::decode(&encoded, REQUEST).is_ok());
        assert!(
            HistoryReadReply::decode(&encoded, "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee").is_err()
        );
        assert!(
            HistoryReadReply::decode(
                br#"{"result":"admitted","request_id":"11111111-2222-4333-8444-555555555555"}"#,
                REQUEST
            )
            .is_err()
        );
        assert!(
            HistoryReadReply::Read {
                request_id: REQUEST.into(),
                bytes: vec![0; fs_protocol::files::MAX_HISTORY_READ_BYTES + 1]
            }
            .encode()
            .is_err()
        );
        assert!(valid_query(HISTORY_PROJECTION_QUERY));
        assert!(valid_query(RECORD_ID_QUERY));
        assert!(!valid_query("user.tilo.history.restore"));
        assert!(!valid_query("/var/workspace/arbitrary-record"));
    }
}
