//! Why a link was refused, in words the person holding it can act on.

use std::fmt;

/// A refused link. [`LinkError::code`] is stable and shared with the page
/// and `tests/links.json`; the `Display` text is what a person reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkError {
    NotAnEntry,
    BadEntry(&'static str),
    UnknownPath,
    TooLong,
    NoRoute,
    MissingScope,
    NativeNeedsHost,
    BadScope,
    BadRoom,
    BadHost,
    BadSlug,
    BadApp,
    BadRecord,
    BadResource,
    BadView,
    BadSpan,
    SpanWithoutResource,
    Incomplete(&'static str),
    TooManySegments,
    EmptySegment,
    UnknownParam(String),
    DuplicateParam,
    ParamOrder,
    BadParam,
    BadSecret,
    Encoding,
    NonCanonical(&'static str),
    NotNative,
    BadProvider,
}

impl LinkError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotAnEntry => "not-an-entry",
            Self::BadEntry(_) => "bad-entry",
            Self::UnknownPath => "unknown-path",
            Self::TooLong => "too-long",
            Self::NoRoute => "no-route",
            Self::MissingScope => "missing-scope",
            Self::NativeNeedsHost => "native-needs-host",
            Self::BadScope => "bad-scope",
            Self::BadRoom => "bad-room",
            Self::BadHost => "bad-host",
            Self::BadSlug => "bad-slug",
            Self::BadApp => "bad-app",
            Self::BadRecord => "bad-record",
            Self::BadResource => "bad-resource",
            Self::BadView => "bad-view",
            Self::BadSpan => "bad-span",
            Self::SpanWithoutResource => "span-without-resource",
            Self::Incomplete(_) => "incomplete",
            Self::TooManySegments => "too-many-segments",
            Self::EmptySegment => "empty-segment",
            Self::UnknownParam(_) => "unknown-param",
            Self::DuplicateParam => "duplicate-param",
            Self::ParamOrder => "param-order",
            Self::BadParam => "bad-param",
            Self::BadSecret => "bad-secret",
            Self::Encoding => "encoding",
            Self::NonCanonical(_) => "non-canonical",
            Self::NotNative => "not-native",
            Self::BadProvider => "bad-provider",
        }
    }
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAnEntry => f.write_str("This address is not a Tilo link this page serves"),
            Self::BadEntry(why) => write!(f, "This web entry is misconfigured: {why}"),
            Self::UnknownPath => f.write_str("This address has an unknown path after the Tilo page; a link keeps everything after #"),
            Self::TooLong => f.write_str("This link is longer than 8 KiB, the most Tilo opens"),
            Self::NoRoute => f.write_str("This link names no workspace"),
            Self::MissingScope => f.write_str("This link is incomplete: it names an app but no workspace"),
            Self::NativeNeedsHost => f.write_str("A tilo:// link must name the workspace's host (/@host/w/…)"),
            Self::BadScope => f.write_str("The workspace part of this link is damaged: expected /me or /w/<id>"),
            Self::BadRoom => f.write_str("The workspace id in this link is damaged (expected a lowercase, hyphenated id)"),
            Self::BadHost => f.write_str("The host in this link is not a valid lowercase host name"),
            Self::BadSlug => f.write_str("The workspace name in this link may only use a-z, 0-9 and -"),
            Self::BadApp => f.write_str("The app in this link must be its full package id, like so.tilo.chat"),
            Self::BadRecord => f.write_str("The document id in this link is damaged"),
            Self::BadResource => f.write_str("The item in this link is damaged or too long"),
            Self::BadView => f.write_str("The view named in this link is damaged"),
            Self::BadSpan => f.write_str("The range in this link is damaged: expected start-end with start ≤ end"),
            Self::SpanWithoutResource => f.write_str("This link names a range but no item to take it from"),
            Self::Incomplete(why) => write!(f, "This link is incomplete: {why}"),
            Self::TooManySegments => f.write_str("This link has more parts than Tilo understands; it may need a newer Tilo"),
            Self::EmptySegment => f.write_str("This link has an empty part (two slashes in a row, or a trailing slash)"),
            Self::UnknownParam(name) => write!(f, "This link needs a newer Tilo (it uses `{name}`)"),
            Self::DuplicateParam => f.write_str("This link repeats a setting"),
            Self::ParamOrder => f.write_str("This link's settings are out of order, so Tilo did not make it"),
            Self::BadParam => f.write_str("This link's settings are damaged"),
            Self::BadSecret => f.write_str("The invitation or ticket in this link is damaged"),
            Self::Encoding => f.write_str("This link contains invalid percent-encoding"),
            Self::NonCanonical(what) => write!(f, "This link is not written the way Tilo writes it: {what}"),
            Self::NotNative => f.write_str("This is not a tilo://open link"),
            Self::BadProvider => f.write_str("This record provider is not record:<app>:<record>"),
        }
    }
}

impl std::error::Error for LinkError {}
