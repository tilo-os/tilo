//! Identifiers inside free text: links, email addresses, mentions and room
//! aliases found in a run of prose.
//!
//! The shaping path runs this over every [`TextKind::Prose`] run, so a link
//! in a chat message, an address in a mail body or a handle in a note is set
//! as what it is — [`TextKind::Url`] or [`TextKind::Identifier`] — with no
//! app involvement. Apps that want to act on the same spans (open a link,
//! resolve a mention) call [`identifier_spans`] and get exactly the ranges
//! the text is drawn with.
//!
//! The scan is a pure function of the text: whitespace-separated words,
//! trimmed of the punctuation prose wraps them in, then classified. It runs
//! in one pass over the bytes and allocates only for what it finds.

use std::ops::Range;

use crate::text_render::TextKind;

/// What an identifier span is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Detected {
    /// A web or other scheme link (`https://…`, `www.…`, a bare
    /// `example.com/path`).
    Url,
    /// An email address.
    Email,
    /// A handle: `@ilya`, `@ilya:example.org`.
    Mention,
    /// A room address: `#general:example.org`.
    RoomAlias,
}

impl Detected {
    /// The kind the span is set as. Links get the slashed zero as well as
    /// distinct I and l: a look-alike domain is a phishing vector.
    #[must_use]
    pub const fn kind(self) -> TextKind {
        match self {
            Self::Url => TextKind::Url,
            Self::Email | Self::Mention | Self::RoomAlias => TextKind::Identifier,
        }
    }
}

/// One identifier found in a run of text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetectedSpan {
    /// Byte range in the scanned text, on character boundaries.
    pub range: Range<usize>,
    pub what: Detected,
}

/// Top-level domains a bare host (no scheme, no `www.`, no path) must end
/// in to count as a link. With a scheme, `www.` or a path any alphabetic
/// TLD does; without, `file.txt` or `node.js` in a sentence must stay prose.
const BARE_TLDS: &[&str] = &[
    "com", "org", "net", "io", "dev", "app", "so", "ai", "co", "me", "info", "edu", "gov", "eu",
    "de", "uk", "fr", "nl", "be", "ch", "at", "es", "it", "se", "no", "dk", "fi", "pl", "cz", "pt",
    "ie", "us", "ca", "au", "nz", "jp", "cn", "in", "br", "ru", "ua", "xyz", "online", "site",
    "tech", "social", "chat", "link", "page", "blog", "news", "cloud", "tv", "fm", "gg",
];

/// Punctuation prose puts before a word that is not part of it.
fn is_leading_punct(c: char) -> bool {
    matches!(
        c,
        '(' | '[' | '{' | '<' | '"' | '\'' | '“' | '‘' | '«' | '‹' | '„' | '‚' | '*' | '_' | '~'
    )
}

/// Punctuation prose puts after a word that is not part of it.
fn is_trailing_punct(c: char) -> bool {
    matches!(
        c,
        '.' | ','
            | ';'
            | ':'
            | '!'
            | '?'
            | ')'
            | ']'
            | '}'
            | '>'
            | '"'
            | '\''
            | '”'
            | '’'
            | '»'
            | '›'
            | '…'
            | '*'
            | '_'
            | '~'
    )
}

/// A soft hyphen the line breaker inserted is part of the word it sits in.
const fn transparent(c: char) -> bool {
    matches!(c, '\u{AD}' | '\u{200B}' | '\u{2060}')
}

fn is_host_char(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || transparent(c)
}

fn is_handle_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '.' | '-') || transparent(c)
}

/// Whether `word` starts with `prefix`, ignoring ASCII case.
fn starts_with_ignore_case(word: &str, prefix: &str) -> bool {
    word.get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
}

fn is_email_local_char(c: char) -> bool {
    c.is_alphanumeric()
        || matches!(
            c,
            '.' | '!'
                | '#'
                | '$'
                | '%'
                | '&'
                | '*'
                | '+'
                | '/'
                | '='
                | '?'
                | '^'
                | '_'
                | '`'
                | '{'
                | '|'
                | '}'
                | '~'
                | '-'
        )
        || transparent(c)
}

fn is_scheme_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-')
}

/// The visible characters of `s`, without soft hyphens.
fn visible(s: &str) -> String {
    s.chars().filter(|c| !transparent(*c)).collect()
}

/// Length in bytes of the host at the start of `s` (`label(.label)+`) and
/// its top-level domain, lowercased. The host has a dot and ends in an
/// alphabetic TLD of two or more letters; a trailing dot is not part of it.
fn host_len(s: &str) -> Option<(usize, String)> {
    let mut end = 0;
    let mut previous_dot = true;
    for (index, c) in s.char_indices() {
        if is_host_char(c) {
            previous_dot = false;
            end = index + c.len_utf8();
        } else if c == '.' && !previous_dot {
            previous_dot = true;
        } else {
            break;
        }
    }
    let host = s.get(..end)?;
    let (_, tld) = host.rsplit_once('.')?;
    let tld = visible(tld);
    if tld.chars().count() < 2 || !tld.chars().all(char::is_alphabetic) {
        return None;
    }
    Some((end, tld.to_lowercase()))
}

/// Length of a URL path, query or fragment at the start of `s`, with the
/// trailing punctuation prose closes it with removed. Parentheses stay
/// when they balance inside the URL (Wikipedia-style links).
fn tail_len(s: &str) -> usize {
    let mut end = s.len();
    loop {
        let Some(slice) = s.get(..end) else {
            return 0;
        };
        let Some(last) = slice.chars().next_back() else {
            return 0;
        };
        let strip = match last {
            ')' => slice.matches(')').count() > slice.matches('(').count(),
            ']' => slice.matches(']').count() > slice.matches('[').count(),
            c => is_trailing_punct(c) && c != '/' && c != '_' && c != '~' && c != '*',
        };
        if !strip {
            return end;
        }
        end -= last.len_utf8();
    }
}

/// Classify one word (whitespace-free, leading punctuation already
/// removed) starting at byte `base` of the scanned text.
fn classify(word: &str, base: usize, out: &mut Vec<DetectedSpan>) {
    if !word.bytes().any(|b| matches!(b, b'@' | b'.' | b':' | b'#')) {
        return;
    }
    // A link with a scheme anywhere in the word: `https://`, `tilo://`.
    if let Some(sep) = word.find("://") {
        let before = word.get(..sep).unwrap_or_default();
        let scheme_start = before
            .char_indices()
            .rev()
            .take_while(|(_, c)| is_scheme_char(*c))
            .last()
            .map_or(sep, |(index, _)| index);
        let scheme = word.get(scheme_start..sep).unwrap_or_default();
        let scheme_start = scheme
            .char_indices()
            .find(|(_, c)| c.is_ascii_alphabetic())
            .map_or(sep, |(index, _)| scheme_start + index);
        let rest = word.get(sep + 3..).unwrap_or_default();
        let len = tail_len(rest);
        if scheme_start < sep && len > 0 {
            out.push(DetectedSpan {
                range: base + scheme_start..base + sep + 3 + len,
                what: Detected::Url,
            });
            return;
        }
    }
    if ["mailto:", "xmpp:", "tel:"]
        .iter()
        .any(|scheme| starts_with_ignore_case(word, scheme))
    {
        let len = tail_len(word);
        if word.find(':').is_some_and(|colon| colon + 1 < len) {
            out.push(DetectedSpan {
                range: base..base + len,
                what: Detected::Url,
            });
            return;
        }
    }
    // A handle or a room alias: `@name`, `@name:server.org`, `#room:server.org`.
    if let Some(first) = word.chars().next()
        && (first == '@' || first == '#')
    {
        let body = word.get(1..).unwrap_or_default();
        let mut len = body
            .char_indices()
            .find(|(_, c)| !is_handle_char(*c))
            .map_or(body.len(), |(index, _)| index);
        let handle = body.get(..len).unwrap_or_default();
        // `@name:server.org` and `@name@server.org`: the server is part of
        // the handle.
        let server = body.get(len..).and_then(|s| {
            s.strip_prefix(':')
                .or_else(|| s.strip_prefix('@').filter(|_| first == '@'))
        });
        if let Some(server) = server
            && let Some((host, _)) = host_len(server)
        {
            len += 1 + host;
        } else if first == '#' {
            // A bare hashtag is a tag, not an address.
            return;
        }
        let handle_len = tail_len(body.get(..len).unwrap_or_default());
        if handle_len > 0 && handle.chars().any(char::is_alphanumeric) {
            out.push(DetectedSpan {
                range: base..base + 1 + handle_len,
                what: if first == '@' {
                    Detected::Mention
                } else {
                    Detected::RoomAlias
                },
            });
        }
        return;
    }
    // An email address: local@host.tld.
    if let Some(at) = word.find('@') {
        let local_start = word
            .get(..at)
            .unwrap_or_default()
            .char_indices()
            .rev()
            .take_while(|(_, c)| is_email_local_char(*c))
            .last()
            .map_or(at, |(index, _)| index);
        let local = word.get(local_start..at).unwrap_or_default();
        let local_start = local_start + (local.len() - local.trim_start_matches('.').len());
        if local_start < at
            && let Some((host, _)) = host_len(word.get(at + 1..).unwrap_or_default())
        {
            out.push(DetectedSpan {
                range: base + local_start..base + at + 1 + host,
                what: Detected::Email,
            });
        }
        return;
    }
    // `www.example.org`, or a bare `example.org/path` or `example.com`.
    let www = starts_with_ignore_case(word, "www.");
    if let Some((host, tld)) = host_len(word) {
        let rest = word.get(host..).unwrap_or_default();
        let path = rest.starts_with(['/', '?', '#']);
        let port = rest.starts_with(':')
            && rest
                .get(1..)
                .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()));
        if www || path || port || BARE_TLDS.contains(&tld.as_str()) {
            let len = if path || port {
                host + tail_len(rest)
            } else {
                host
            };
            out.push(DetectedSpan {
                range: base..base + len,
                what: Detected::Url,
            });
        }
    }
}

/// Every link, email address, mention and room alias in `text`, in order,
/// as byte ranges on character boundaries.
#[must_use]
pub fn identifier_spans(text: &str) -> Vec<DetectedSpan> {
    let mut out = Vec::new();
    // Cheap rejection: every identifier has one of these.
    if !text.bytes().any(|b| matches!(b, b'@' | b'.' | b':' | b'#')) {
        return out;
    }
    let mut start = None;
    for (index, c) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        if c.is_whitespace() {
            if let Some(word_start) = start.take()
                && let Some(word) = text.get(word_start..index)
            {
                let lead = word
                    .char_indices()
                    .find(|(_, c)| !is_leading_punct(*c))
                    .map_or(word.len(), |(i, _)| i);
                if let Some(core) = word.get(lead..) {
                    classify(core, word_start + lead, &mut out);
                }
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    out
}

/// [`identifier_spans`] as `(range, kind)` pairs, for shaping.
#[must_use]
pub fn identifier_kinds(text: &str) -> Vec<(Range<usize>, TextKind)> {
    identifier_spans(text)
        .into_iter()
        .map(|span| (span.range, span.what.kind()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(text: &str) -> Vec<(&str, Detected)> {
        identifier_spans(text)
            .into_iter()
            .map(|span| (&text[span.range], span.what))
            .collect()
    }

    #[test]
    fn prose_has_no_identifiers() {
        for text in [
            "Illinois is a state. Ill-advised, e.g. not i.e. this.",
            "Version 1.0 shipped at 10:30; see section 3.2.",
            "Open file.txt and node.js, then Mr.Smith arrives.",
            "#rust is a tag, and @ alone is not a handle.",
            "",
        ] {
            assert!(found(text).is_empty(), "{text}: {:?}", found(text));
        }
    }

    #[test]
    fn links_with_and_without_scheme() {
        assert_eq!(
            found("See https://paypa1.com/login?x=0, or (www.example.org)."),
            [
                ("https://paypa1.com/login?x=0", Detected::Url),
                ("www.example.org", Detected::Url),
            ]
        );
        assert_eq!(
            found("Docs at tilo.so and example.net/guide/intro."),
            [
                ("tilo.so", Detected::Url),
                ("example.net/guide/intro", Detected::Url),
            ]
        );
        assert_eq!(
            found("https://en.wikipedia.org/wiki/Foo_(bar) and (https://a.org/x)"),
            [
                ("https://en.wikipedia.org/wiki/Foo_(bar)", Detected::Url),
                ("https://a.org/x", Detected::Url),
            ]
        );
        assert_eq!(
            found("link:tilo://room/0OIl, mailto:ilya@example.org"),
            [
                ("tilo://room/0OIl", Detected::Url),
                ("mailto:ilya@example.org", Detected::Url),
            ]
        );
        assert_eq!(
            found("localhost.dev:8080/a"),
            [("localhost.dev:8080/a", Detected::Url)]
        );
    }

    #[test]
    fn emails_mentions_and_rooms() {
        assert_eq!(
            found(
                "Mail Ilya.l1@example.org, ping @ilya_l1 or @Il1:matrix.org in #general:tilo.so."
            ),
            [
                ("Ilya.l1@example.org", Detected::Email),
                ("@ilya_l1", Detected::Mention),
                ("@Il1:matrix.org", Detected::Mention),
                ("#general:tilo.so", Detected::RoomAlias),
            ]
        );
        assert_eq!(found("“@lena”"), [("@lena", Detected::Mention)]);
        assert_eq!(
            found("@lena@social.example.org!"),
            [("@lena@social.example.org", Detected::Mention)]
        );
        assert_eq!(found("someone@localhost"), []);
    }

    #[test]
    fn ranges_are_on_character_boundaries_in_unicode_text() {
        let text = "Привет — @Илья, письмо на почта@пример.рф и https://пример.рф/путь…";
        let spans = identifier_spans(text);
        for span in &spans {
            assert!(text.is_char_boundary(span.range.start));
            assert!(text.is_char_boundary(span.range.end));
        }
        assert_eq!(
            found(text),
            [
                ("@Илья", Detected::Mention),
                ("почта@пример.рф", Detected::Email),
                ("https://пример.рф/путь", Detected::Url),
            ]
        );
    }

    #[test]
    fn soft_hyphens_from_the_line_breaker_stay_inside_the_span() {
        let text = "visit ex\u{AD}ample.org now";
        assert_eq!(found(text), [("ex\u{AD}ample.org", Detected::Url)]);
    }

    #[test]
    fn kinds_follow_the_policy() {
        assert_eq!(Detected::Url.kind(), TextKind::Url);
        assert_eq!(Detected::Email.kind(), TextKind::Identifier);
        assert_eq!(Detected::Mention.kind(), TextKind::Identifier);
        assert_eq!(Detected::RoomAlias.kind(), TextKind::Identifier);
    }
}
