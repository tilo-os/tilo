//! Shapes and safety invariants of the portable continuity semantic schemas.
//!
//! A [`ContinuitySemanticSchema`] is declared in a signed manifest and names
//! what a continuation payload *means*, not which app wrote it. The shapes and
//! invariants live here, beside the vocabulary, so every runtime that carries
//! continuations (the shell receiving another device's checkpoint, an app
//! composing its own) checks the same rules.
//!
//! Every schema is JSON with `deny_unknown_fields`: a payload carries exactly
//! the fields below and nothing an app might add (a path, a token, a member
//! list). Identifiers are path-free ([`portable_id`]), locations are plain
//! web URLs without credentials ([`portable_http_url`]).
//!
//! Two invariants protect the person on the receiving device. A continuation
//! must never start audio there unasked, so a [`MediaPosition`] is always
//! paused; and it must never share that device's audio unasked, so a
//! [`RealtimeSession`] resumes with the microphone facet on (muting is a
//! control the person sees) and audio sharing off.

use serde::{Deserialize, Serialize};

pub use crate::manifest::ContinuitySemanticSchema;

impl ContinuitySemanticSchema {
    /// Every schema the platform understands.
    pub const ALL: [Self; 5] = [
        Self::WebLocationV1,
        Self::PublicationPositionV1,
        Self::MediaPositionV1,
        Self::RealtimeSessionV1,
        Self::TextPositionV1,
    ];

    /// The number a continuation payload carries for this schema, so that a
    /// device which has not installed the app can still tell what the
    /// payload means. Disjoint from the toolkit's structural and generic
    /// envelope numbers (`u16::MAX - 1` and `u16::MAX`); apps write it
    /// through `ui::collaboration::semantic_schema`.
    #[must_use]
    pub const fn wire_tag(self) -> u16 {
        match self {
            Self::WebLocationV1 => 0x0101,
            Self::PublicationPositionV1 => 0x0102,
            Self::MediaPositionV1 => 0x0103,
            Self::RealtimeSessionV1 => 0x0104,
            Self::TextPositionV1 => 0x0105,
        }
    }

    /// The schema a payload number names, if it names one.
    #[must_use]
    pub const fn from_wire_tag(tag: u16) -> Option<Self> {
        match tag {
            0x0101 => Some(Self::WebLocationV1),
            0x0102 => Some(Self::PublicationPositionV1),
            0x0103 => Some(Self::MediaPositionV1),
            0x0104 => Some(Self::RealtimeSessionV1),
            0x0105 => Some(Self::TextPositionV1),
            _ => None,
        }
    }

    /// Whether `bytes` is a payload of this schema and keeps its invariants.
    ///
    /// [`Self::TextPositionV1`] anchors its carets in CRDT cursors, which
    /// only the document runtime can decode; this crate does not link it, so
    /// the caller passes that check as `text_position` (in the toolkit,
    /// `ui::text_position::TextPosition::valid`). It is called only for that
    /// schema.
    #[must_use]
    pub fn validate(self, bytes: &[u8], text_position: impl FnOnce(&[u8]) -> bool) -> bool {
        match self {
            Self::WebLocationV1 => decode::<WebLocation>(bytes).is_some_and(|p| p.valid()),
            Self::PublicationPositionV1 => {
                decode::<PublicationPosition>(bytes).is_some_and(|p| p.valid())
            }
            Self::MediaPositionV1 => decode::<MediaPosition>(bytes).is_some_and(|p| p.valid()),
            Self::RealtimeSessionV1 => decode::<RealtimeSession>(bytes).is_some_and(|p| p.valid()),
            Self::TextPositionV1 => text_position(bytes),
        }
    }
}

fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Option<T> {
    serde_json::from_slice(bytes).ok()
}

/// `web-location-v1`: the page a browser-like view shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebLocation {
    pub url: String,
}

impl WebLocation {
    #[must_use]
    pub fn valid(&self) -> bool {
        portable_http_url(&self.url)
    }
}

/// `publication-position-v1`: where a reader is in a publication.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationPosition {
    pub book_id: String,
    pub chapter: u64,
    pub page: u64,
}

impl PublicationPosition {
    #[must_use]
    pub fn valid(&self) -> bool {
        portable_id(&self.book_id)
    }
}

/// `media-position-v1`: a track and an offset into it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaPosition {
    pub track_id: String,
    pub position_ms: u64,
    /// Always `true`: a continuation never starts playback on another device.
    pub paused: bool,
}

impl MediaPosition {
    #[must_use]
    pub fn valid(&self) -> bool {
        self.paused && portable_id(&self.track_id)
    }
}

/// `realtime-session-v1`: the call a person was in, and how they were in it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealtimeSession {
    pub workspace_id: String,
    pub channel_id: String,
    pub media: RealtimeMediaFacets,
    pub controls: RealtimeControlIntent,
}

impl RealtimeSession {
    /// Besides the shape: the microphone facet is on and audio sharing is
    /// off, so resuming never shares the receiving device's audio unasked.
    #[must_use]
    pub fn valid(&self) -> bool {
        self.media.valid()
            && self.media.microphone
            && !self.media.share_audio
            && portable_id(&self.workspace_id)
            && portable_id(&self.channel_id)
    }
}

pub use collab_types::realtime::{RealtimeControlIntent, RealtimeMediaFacets};

/// A path-free identifier: 1 to 256 bytes of `[A-Za-z0-9._:-]`.
#[must_use]
pub fn portable_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':'))
}

/// An `http(s)` URL with a host and no credentials, control characters or
/// backslashes.
#[must_use]
pub fn portable_http_url(value: &str) -> bool {
    let Some(rest) = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    !authority.is_empty()
        && !authority.contains('@')
        && !value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b'\\')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rejects_text(_: &[u8]) -> bool {
        false
    }

    #[test]
    fn wire_tags_round_trip_and_avoid_reserved_numbers() {
        for schema in ContinuitySemanticSchema::ALL {
            let tag = schema.wire_tag();
            assert_eq!(ContinuitySemanticSchema::from_wire_tag(tag), Some(schema));
            assert!(![u16::MAX - 1, u16::MAX].contains(&tag));
        }
        for reserved in [0, 1, 2, u16::MAX - 1, u16::MAX] {
            assert_eq!(ContinuitySemanticSchema::from_wire_tag(reserved), None);
        }
    }

    #[test]
    fn semantic_schemas_validate_shape_and_invariants() {
        use ContinuitySemanticSchema as S;
        let realtime = |media: &str| {
            format!(
                r#"{{"workspace_id":"ws_acme","channel_id":"c_huddle","media":{media},
                "controls":{{"microphone_muted":true,"camera_muted":false,"screen_share_paused":false,"share_audio_muted":true}}}}"#
            )
        };
        let call = realtime(
            r#"{"microphone":true,"camera":true,"screen_share":false,"share_audio":false}"#,
        );
        let valid: [(S, &[u8]); 4] = [
            (S::WebLocationV1, br#"{"url":"https://tilo.so/a?b#c"}"#),
            (
                S::PublicationPositionV1,
                br#"{"book_id":"urn:isbn:978-0","chapter":3,"page":42}"#,
            ),
            (
                S::MediaPositionV1,
                br#"{"track_id":"t_1","position_ms":61000,"paused":true}"#,
            ),
            (S::RealtimeSessionV1, call.as_bytes()),
        ];
        for (schema, bytes) in valid {
            assert!(schema.validate(bytes, rejects_text), "{schema:?} accepts");
            // Unknown fields never ride along.
            let mut extra = bytes.to_vec();
            let close = extra.iter().rposition(|byte| *byte == b'}').unwrap();
            extra.splice(close..close, br#","password":"secret""#.iter().copied());
            assert!(
                !schema.validate(&extra, rejects_text),
                "{schema:?} refuses an unknown field"
            );
            // A payload of one schema is not a payload of another.
            for other in S::ALL {
                if other != schema && other != S::TextPositionV1 {
                    assert!(
                        !other.validate(bytes, rejects_text),
                        "{other:?} vs {schema:?}"
                    );
                }
            }
        }

        // Identifiers are path-free; locations carry no credentials.
        let refused: [(S, &[u8]); 6] = [
            (
                S::PublicationPositionV1,
                br#"{"book_id":"../x","chapter":0,"page":0}"#,
            ),
            (
                S::MediaPositionV1,
                br#"{"track_id":"../x","position_ms":0,"paused":true}"#,
            ),
            (S::WebLocationV1, br#"{"url":"file:///home/alice/private"}"#),
            (
                S::WebLocationV1,
                br#"{"url":"https://alice:secret@example.test"}"#,
            ),
            (S::WebLocationV1, br#"{"url":"https://a.test\\..\\b"}"#),
            // Media never resumes playing on the receiving device.
            (
                S::MediaPositionV1,
                br#"{"track_id":"t_1","position_ms":0,"paused":false}"#,
            ),
        ];
        for (schema, bytes) in refused {
            assert!(
                !schema.validate(bytes, rejects_text),
                "{schema:?} refuses {}",
                String::from_utf8_lossy(bytes)
            );
        }
        let path_channel = call.replace("c_huddle", "../x");
        assert!(!S::RealtimeSessionV1.validate(path_channel.as_bytes(), rejects_text));

        // A call never resumes sharing audio, and always with the microphone
        // facet on.
        for media in [
            r#"{"microphone":true,"camera":false,"screen_share":true,"share_audio":true}"#,
            r#"{"microphone":false,"camera":true,"screen_share":false,"share_audio":false}"#,
            r#"{"microphone":false,"camera":false,"screen_share":false,"share_audio":false}"#,
        ] {
            assert!(!S::RealtimeSessionV1.validate(realtime(media).as_bytes(), rejects_text));
        }
    }

    #[test]
    fn text_position_is_decided_by_the_document_runtime() {
        let mut seen = None;
        assert!(
            ContinuitySemanticSchema::TextPositionV1.validate(b"{}", |bytes| {
                seen = Some(bytes.to_vec());
                true
            })
        );
        assert_eq!(seen.as_deref(), Some(b"{}".as_slice()));
        assert!(!ContinuitySemanticSchema::TextPositionV1.validate(b"{}", rejects_text));
        // Other schemas never consult it.
        assert!(
            !ContinuitySemanticSchema::WebLocationV1
                .validate(b"{}", |_| panic!("not a text position"))
        );
    }
}
