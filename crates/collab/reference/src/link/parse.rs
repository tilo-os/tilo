//! The strict reader. It refuses rather than repairs: a link Tilo did not
//! write in exactly this spelling is refused with the rule it broke, so two
//! strings never open the same thing by accident and a newer link says it
//! needs a newer Tilo instead of silently losing what it meant.

use std::iter::Peekable;

use uuid::Uuid;

use super::{
    AppId, HostOrigin, Link, LinkError, LinkSecrets, LinkTarget, MAX_ROUTE_BYTES, MAX_SPAN_OFFSET,
    Mode, PACKAGE_PRIMARY, RecordSegment, ResourceAddress, Scope, Secret, Slug, WebEntries,
    WebEntry, encode,
};
use crate::{Span, SpanUnit};

/// Parameters in their only order. `chars` and `bytes` share a slot: a span has one unit.
const PARAMS: [(&str, u8); 8] = [
    ("view", 0),
    ("chars", 1),
    ("bytes", 1),
    ("invite", 2),
    ("ticket", 3),
    ("token", 4),
    ("pair", 5),
    ("install", 6),
];

#[derive(Clone, Copy)]
enum Context<'e> {
    Web(&'e WebEntry),
    Native,
}

impl Link {
    /// Read a web link under one of `entries`.
    ///
    /// # Errors
    ///
    /// The rule the link breaks. [`LinkError::NoRoute`] for an entry with no
    /// route, which the page opens as its home workspace or the person's own
    /// desktop.
    pub fn parse(url: &str, entries: &WebEntries) -> Result<Self, LinkError> {
        let (entry, rest) = entries.find(url).ok_or(LinkError::NotAnEntry)?;
        let (mode, rest) = match rest.strip_prefix("app/") {
            Some(rest) => (Mode::App, rest),
            None => (Mode::Desktop, rest),
        };
        // A query before `#` holds diagnostic switches only (the WebGPU
        // override, the site's `?embed=1`); it is never part of the address.
        let (before, route) = rest.split_once('#').unwrap_or((rest, ""));
        if !(before.is_empty() || before.starts_with('?')) {
            return Err(LinkError::UnknownPath);
        }
        let (target, secrets) = parse_route(route, Context::Web(entry))?;
        Ok(Self {
            mode,
            target,
            secrets,
        })
    }

    /// Read `tilo://open#<route>`. The scope is always explicit.
    ///
    /// # Errors
    ///
    /// [`LinkError::NotNative`] for any other URI, else the rule broken.
    pub fn parse_native(uri: &str) -> Result<Self, LinkError> {
        let route = uri
            .strip_prefix("tilo://open#")
            .ok_or(LinkError::NotNative)?;
        let (target, secrets) = parse_route(route, Context::Native)?;
        Ok(Self {
            mode: Mode::Desktop,
            target,
            secrets,
        })
    }
}

fn split_path(path: &str) -> Result<Vec<&str>, LinkError> {
    if path.is_empty() {
        return Ok(Vec::new());
    }
    let rest = path
        .strip_prefix('/')
        .ok_or(LinkError::NonCanonical("a route starts with /"))?;
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.iter().any(|part| part.is_empty()) {
        return Err(LinkError::EmptySegment);
    }
    Ok(parts)
}

fn parse_room(segment: &str) -> Result<(Option<Slug>, Uuid), LinkError> {
    let (slug, room) = match segment.split_once('~') {
        Some((slug, room)) => (Some(Slug::parse(slug)?), room),
        None => (None, segment),
    };
    let parsed = Uuid::try_parse(room).map_err(|_| LinkError::BadRoom)?;
    // Only the lowercase, hyphenated spelling is the room's id.
    if parsed.hyphenated().to_string() != room {
        return Err(LinkError::BadRoom);
    }
    Ok((slug, parsed))
}

fn parse_scope<'a, I: Iterator<Item = &'a str>>(
    segments: &mut Peekable<I>,
    context: Context<'_>,
    has_params: bool,
) -> Result<Scope, LinkError> {
    let first = match segments.next() {
        Some(first) => first,
        None if has_params => return Err(LinkError::MissingScope),
        None => return Err(LinkError::NoRoute),
    };
    if first == "me" {
        return Ok(Scope::Own);
    }
    let host = if let Some(authority) = first.strip_prefix('@') {
        let host = HostOrigin::from_authority(authority)?;
        if let Context::Web(entry) = context
            && host == *entry.origin()
        {
            return Err(LinkError::NonCanonical(
                "leave out the host when it is this page's own",
            ));
        }
        if segments.next() != Some("w") {
            return Err(LinkError::BadScope);
        }
        host
    } else if first == "w" {
        match context {
            Context::Web(entry) => entry.origin().clone(),
            Context::Native => return Err(LinkError::NativeNeedsHost),
        }
    } else if AppId::new(first).is_ok() {
        return Err(LinkError::MissingScope);
    } else {
        return Err(LinkError::BadScope);
    };
    let (slug, room) = parse_room(segments.next().ok_or(LinkError::BadRoom)?)?;
    Ok(Scope::Room { host, room, slug })
}

fn parse_offset(text: &str) -> Result<usize, LinkError> {
    let canonical = !text.is_empty()
        && text.bytes().all(|b| b.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    let value = text
        .parse::<u64>()
        .ok()
        .filter(|value| canonical && *value <= MAX_SPAN_OFFSET);
    value
        .and_then(|value| usize::try_from(value).ok())
        .ok_or(LinkError::BadSpan)
}

fn parse_span(value: &str, unit: SpanUnit) -> Result<Span, LinkError> {
    let (start, end) = value.split_once('-').ok_or(LinkError::BadSpan)?;
    let (start, end) = (parse_offset(start)?, parse_offset(end)?);
    if start > end {
        return Err(LinkError::BadSpan);
    }
    Ok(Span { start, end, unit })
}

fn unknown_param(name: &str) -> LinkError {
    let printable = (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    LinkError::UnknownParam(if printable {
        name.to_owned()
    } else {
        "an unknown setting".to_owned()
    })
}

fn parse_route(route: &str, context: Context<'_>) -> Result<(LinkTarget, LinkSecrets), LinkError> {
    if route.len() > MAX_ROUTE_BYTES {
        return Err(LinkError::TooLong);
    }
    let (path, params) = match route.split_once('?') {
        Some((path, params)) => (path, Some(params)),
        None => (route, None),
    };
    let mut segments = split_path(path)?.into_iter().peekable();
    let mut target = LinkTarget::new(parse_scope(&mut segments, context, params.is_some())?);
    if let Some(app) = segments.next() {
        target.app = Some(AppId::new(encode::decode_segment(app)?)?);
    }
    if let Some(record) = segments.next() {
        target.record = Some(if record == "-" {
            RecordSegment::Primary
        } else {
            let id = encode::decode_segment(record)?;
            if id == PACKAGE_PRIMARY {
                return Err(LinkError::NonCanonical("write the primary record as -"));
            }
            match RecordSegment::new(&id)? {
                RecordSegment::Id(id) => RecordSegment::Id(id),
                RecordSegment::Primary => return Err(LinkError::BadRecord),
            }
        });
    }
    let rest: Vec<&str> = segments.collect();
    target.resource = match rest.as_slice() {
        [] => None,
        [id] => {
            let id = encode::decode_segment(id)?;
            let kind = id
                .split_once(':')
                .map(|(kind, _)| kind)
                .filter(|kind| !kind.is_empty())
                .ok_or(LinkError::BadResource)?
                .to_owned();
            Some(ResourceAddress {
                kind,
                id,
                span: None,
            })
        }
        [kind, id] => {
            let resource = ResourceAddress {
                kind: encode::decode_segment(kind)?,
                id: encode::decode_segment(id)?,
                span: None,
            };
            if resource.self_describing() {
                return Err(LinkError::NonCanonical(
                    "an id that starts with its kind is one segment",
                ));
            }
            Some(resource)
        }
        _ => return Err(LinkError::TooManySegments),
    };
    let mut secrets = LinkSecrets::default();
    if let Some(params) = params {
        let mut last: Option<u8> = None;
        let mut span = None;
        for pair in params.split('&') {
            let (name, value) = pair.split_once('=').ok_or(LinkError::BadParam)?;
            let slot = PARAMS
                .iter()
                .find(|(known, _)| *known == name)
                .map(|(_, slot)| *slot)
                .ok_or_else(|| unknown_param(name))?;
            match last {
                Some(previous) if previous == slot => return Err(LinkError::DuplicateParam),
                Some(previous) if previous > slot => return Err(LinkError::ParamOrder),
                _ => last = Some(slot),
            }
            match name {
                "view" => target.view = Some(encode::decode_segment(value)?),
                "chars" => span = Some(parse_span(value, SpanUnit::Chars)?),
                "bytes" => span = Some(parse_span(value, SpanUnit::Bytes)?),
                "invite" => secrets.invite = Some(Secret::new(value)?),
                "ticket" => secrets.ticket = Some(Secret::new(value)?),
                "token" => secrets.token = Some(Secret::new(value)?),
                "pair" => secrets.pair = Some(Secret::new(value)?),
                _ => secrets.install = Some(Secret::new(value)?),
            }
        }
        if let Some(span) = span {
            let resource = target
                .resource
                .as_mut()
                .ok_or(LinkError::SpanWithoutResource)?;
            resource.span = Some(span);
        }
    }
    target.check()?;
    Ok((target, secrets))
}
