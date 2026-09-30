//! Bounded RFC 4918 `multistatus` parsing.
//!
//! A DAV server is an untrusted peer: the parser caps input size, node count
//! and nesting depth, rejects any DTD (so no entity expansion happens), and
//! insists on a `multistatus` root, so an HTML error page answered with a 2xx
//! status is an error rather than an empty — and therefore "authoritative" —
//! listing.
//!
//! Elements are matched by local name. Servers pick their own namespace
//! prefixes and a few omit the `DAV:` namespace entirely; the local names are
//! what stays stable. Each [`Property`] keeps its namespace for callers that
//! need to tell `DAV:` and extension properties apart.

use std::fmt;

use roxmltree::{Document, Node, ParsingOptions};

/// Caps applied to one multistatus document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Largest accepted document, in bytes.
    pub max_bytes: usize,
    /// Most `response` elements accepted.
    pub max_responses: usize,
    /// Deepest element nesting accepted.
    pub max_depth: usize,
    /// Most XML nodes (elements, text, comments) accepted.
    pub max_nodes: u32,
    /// Longest accepted `href`, in bytes.
    pub max_href_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 8 * 1024 * 1024,
            max_responses: 10_000,
            // multistatus/response/propstat/prop/resourcetype/collection is
            // six levels; extension properties nest a few more.
            max_depth: 32,
            max_nodes: 1_000_000,
            max_href_bytes: 4096,
        }
    }
}

/// Why a document was refused. Nothing partial is returned with an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XmlError {
    /// The document exceeds [`Limits::max_bytes`].
    TooLarge,
    /// The document is not UTF-8.
    NotUtf8,
    /// The document carries a DTD, which is never processed.
    Doctype,
    /// The document exceeds [`Limits::max_nodes`].
    TooManyNodes,
    /// The document nests deeper than [`Limits::max_depth`].
    TooDeep,
    /// The document holds more than [`Limits::max_responses`] responses.
    TooManyResponses,
    /// A response has no `href`, an empty one, or one over
    /// [`Limits::max_href_bytes`].
    InvalidHref,
    /// The root element is not `multistatus`.
    NotMultistatus,
    /// The document is not well-formed XML.
    Malformed,
}

impl fmt::Display for XmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "DAV response exceeds the size limit",
            Self::NotUtf8 => "DAV response is not UTF-8",
            Self::Doctype => "DAV response carries a DTD",
            Self::TooManyNodes => "DAV response has too many XML nodes",
            Self::TooDeep => "DAV response nests too deeply",
            Self::TooManyResponses => "DAV response lists too many resources",
            Self::InvalidHref => "DAV response contains an invalid resource href",
            Self::NotMultistatus => "DAV response is not a multistatus document",
            Self::Malformed => "DAV response is malformed XML",
        })
    }
}

impl std::error::Error for XmlError {}

/// A parsed `multistatus` document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Multistatus {
    /// The RFC 6578 `sync-token` that is a direct child of the root, as a
    /// `sync-collection` REPORT returns it. Trimmed; `None` when absent/empty.
    pub sync_token: Option<String>,
    /// Every `response`, in document order.
    pub responses: Vec<Response>,
}

/// One `response` element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    /// The first `href`, trimmed and exactly as the server spelled it.
    pub href: String,
    /// The response-level status code (a whole-resource status such as the
    /// 404 a `sync-collection` report uses for removed members).
    pub status: Option<u16>,
    /// Every `propstat`, in document order.
    pub propstats: Vec<Propstat>,
}

/// One `propstat` element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Propstat {
    /// The propstat status code.
    pub status: Option<u16>,
    /// The properties inside its `prop` element(s).
    pub props: Vec<Property>,
}

/// One property element inside a `prop`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Property {
    /// Namespace URI, if the element has one.
    pub namespace: Option<String>,
    /// Local name.
    pub name: String,
    /// The element's text content, untrimmed (`calendar-data` and
    /// `address-data` are payloads where whitespace matters).
    pub text: Option<String>,
    /// Local names of child elements, e.g. `collection` in `resourcetype`.
    pub children: Vec<String>,
}

impl Property {
    /// The text trimmed of surrounding whitespace, or `None` when empty.
    pub fn trimmed_text(&self) -> Option<&str> {
        self.text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
    }

    /// Whether a child element with this local name is present.
    pub fn has_child(&self, name: &str) -> bool {
        self.children.iter().any(|child| child == name)
    }
}

impl Propstat {
    /// Whether the properties in this propstat were returned successfully.
    /// A propstat without a status is treated as successful, as the
    /// pre-RFC servers that omit it only ever list found properties.
    pub fn is_success(&self) -> bool {
        self.status
            .is_none_or(|status| (200..300).contains(&status))
    }
}

impl Response {
    /// The first successfully returned property with this local name.
    pub fn prop(&self, name: &str) -> Option<&Property> {
        self.propstats
            .iter()
            .filter(|propstat| propstat.is_success())
            .flat_map(|propstat| propstat.props.iter())
            .find(|prop| prop.name == name)
    }

    /// Trimmed, non-empty text of a successfully returned property.
    pub fn prop_text(&self, name: &str) -> Option<&str> {
        self.prop(name).and_then(Property::trimmed_text)
    }

    /// Whether `resourcetype` marks this resource as a collection.
    pub fn is_collection(&self) -> bool {
        self.prop("resourcetype")
            .is_some_and(|prop| prop.has_child("collection"))
    }

    /// Whether any propstat returned successfully.
    pub fn has_success(&self) -> bool {
        self.propstats.iter().any(Propstat::is_success)
    }
}

/// Parse a multistatus document from raw bytes.
///
/// # Errors
/// See [`XmlError`]; the size cap is checked before decoding.
pub fn parse_bytes(bytes: &[u8], limits: &Limits) -> Result<Multistatus, XmlError> {
    if bytes.len() > limits.max_bytes {
        return Err(XmlError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| XmlError::NotUtf8)?;
    parse(text, limits)
}

/// Parse a multistatus document.
///
/// # Errors
/// See [`XmlError`].
pub fn parse(xml: &str, limits: &Limits) -> Result<Multistatus, XmlError> {
    if xml.len() > limits.max_bytes {
        return Err(XmlError::TooLarge);
    }
    check_depth(xml, limits.max_depth)?;
    let options = ParsingOptions {
        allow_dtd: false,
        nodes_limit: limits.max_nodes,
        ..ParsingOptions::default()
    };
    let doc = Document::parse_with_options(xml, options).map_err(|error| match error {
        roxmltree::Error::DtdDetected => XmlError::Doctype,
        roxmltree::Error::NodesLimitReached => XmlError::TooManyNodes,
        _ => XmlError::Malformed,
    })?;
    let root = doc.root_element();
    if root.tag_name().name() != "multistatus" {
        return Err(XmlError::NotMultistatus);
    }
    let sync_token = child(root, "sync-token")
        .and_then(|node| node.text())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(ToOwned::to_owned);
    let mut responses = Vec::new();
    for node in children(root, "response") {
        if responses.len() >= limits.max_responses {
            return Err(XmlError::TooManyResponses);
        }
        responses.push(parse_response(node, limits)?);
    }
    Ok(Multistatus {
        sync_token,
        responses,
    })
}

/// Refuse deep nesting before the tree parser sees the document: its
/// recursion is bounded only by the stack, so a hostile server could crash
/// the client with a few hundred KiB of `<x>`. This is a linear scan of tag
/// delimiters (skipping comments, CDATA, processing instructions and quoted
/// attribute values); well-formedness is still the parser's job.
fn check_depth(xml: &str, max_depth: usize) -> Result<(), XmlError> {
    let bytes = xml.as_bytes();
    let mut depth = 0_usize;
    let mut pos = 0_usize;
    while let Some(offset) = bytes
        .get(pos..)
        .and_then(|rest| rest.iter().position(|b| *b == b'<'))
    {
        let start = pos.saturating_add(offset);
        let rest = bytes.get(start..).unwrap_or_default();
        let skip_to = |terminator: &[u8]| -> usize {
            rest.windows(terminator.len())
                .position(|window| window == terminator)
                .map_or(bytes.len(), |end| {
                    start.saturating_add(end).saturating_add(terminator.len())
                })
        };
        if rest.starts_with(b"<!--") {
            pos = skip_to(b"-->");
        } else if rest.starts_with(b"<![CDATA[") {
            pos = skip_to(b"]]>");
        } else if rest.starts_with(b"<?") {
            pos = skip_to(b"?>");
        } else if rest.starts_with(b"<!") {
            pos = skip_to(b">");
        } else {
            // Find the tag's end, ignoring `>` inside quoted attribute values.
            let mut quote = None;
            let mut end = None;
            for (index, byte) in rest.iter().enumerate().skip(1) {
                match (quote, *byte) {
                    (Some(open), byte) if byte == open => quote = None,
                    (None, b'"' | b'\'') => quote = Some(*byte),
                    (None, b'>') => {
                        end = Some(index);
                        break;
                    }
                    _ => {}
                }
            }
            let Some(end) = end else {
                return Err(XmlError::Malformed);
            };
            let closing = rest.get(1) == Some(&b'/');
            let self_closing = end > 0 && rest.get(end.saturating_sub(1)) == Some(&b'/');
            if closing {
                depth = depth.saturating_sub(1);
            } else if !self_closing {
                depth = depth.saturating_add(1);
                if depth > max_depth {
                    return Err(XmlError::TooDeep);
                }
            }
            pos = start.saturating_add(end).saturating_add(1);
        }
    }
    Ok(())
}

fn parse_response(node: Node<'_, '_>, limits: &Limits) -> Result<Response, XmlError> {
    let href = child(node, "href")
        .and_then(|href| href.text())
        .map(str::trim)
        .filter(|href| !href.is_empty() && href.len() <= limits.max_href_bytes)
        .ok_or(XmlError::InvalidHref)?
        .to_owned();
    let status = child(node, "status").and_then(status_code);
    let propstats = children(node, "propstat")
        .map(|propstat| Propstat {
            status: child(propstat, "status").and_then(status_code),
            props: children(propstat, "prop")
                .flat_map(|prop| prop.children().filter(Node::is_element))
                .map(|prop| Property {
                    namespace: prop.tag_name().namespace().map(ToOwned::to_owned),
                    name: prop.tag_name().name().to_owned(),
                    text: prop.text().map(ToOwned::to_owned),
                    children: prop
                        .children()
                        .filter(Node::is_element)
                        .map(|child| child.tag_name().name().to_owned())
                        .collect(),
                })
                .collect(),
        })
        .collect();
    Ok(Response {
        href,
        status,
        propstats,
    })
}

/// `HTTP/1.1 200 OK` → `200`.
fn status_code(node: Node<'_, '_>) -> Option<u16> {
    node.text()?.split_ascii_whitespace().nth(1)?.parse().ok()
}

fn child<'a, 'input>(node: Node<'a, 'input>, name: &'a str) -> Option<Node<'a, 'input>> {
    children(node, name).next()
}

fn children<'a, 'input: 'a>(
    node: Node<'a, 'input>,
    name: &'a str,
) -> impl Iterator<Item = Node<'a, 'input>> + 'a {
    node.children()
        .filter(move |child| child.is_element() && child.tag_name().name() == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_default(xml: &str) -> Result<Multistatus, XmlError> {
        parse(xml, &Limits::default())
    }

    /// Nextcloud's PROPFIND shape: `d:`/`oc:`/`nc:` prefixes, a 404 propstat
    /// for unsupported properties and an empty `getcontentlength` on folders.
    const NEXTCLOUD_PROPFIND: &str = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:s="http://sabredav.org/ns" xmlns:oc="http://owncloud.org/ns" xmlns:nc="http://nextcloud.org/ns">
 <d:response>
  <d:href>/remote.php/dav/files/ada/</d:href>
  <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype><d:getetag>&quot;65f1&quot;</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
  <d:propstat><d:prop><d:getcontentlength/><d:getcontenttype/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat>
 </d:response>
 <d:response>
  <d:href>/remote.php/dav/files/ada/Notes%20and%20plans.md</d:href>
  <d:propstat><d:prop><d:displayname>Notes and plans.md</d:displayname><d:getcontentlength>1234</d:getcontentlength><d:getcontenttype>text/markdown</d:getcontenttype><d:resourcetype/><d:getetag>&quot;a1b2&quot;</d:getetag><oc:fileid>42</oc:fileid></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
 </d:response>
</d:multistatus>"#;

    /// Radicale's sync-collection REPORT: default-namespace `DAV:`, a
    /// `C:`-prefixed `CalDAV` payload and a 404 response for a removed member.
    const RADICALE_SYNC: &str = r#"<?xml version='1.0' encoding='utf-8'?>
<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><response><href>/ada/work/event-1.ics</href><propstat><prop><getetag>"8e5d3a"</getetag><C:calendar-data>BEGIN:VCALENDAR&#13;
VERSION:2.0&#13;
END:VCALENDAR&#13;
</C:calendar-data></prop><status>HTTP/1.1 200 OK</status></propstat></response><response><href>/ada/work/event-0.ics</href><status>HTTP/1.1 404 Not Found</status></response><sync-token>http://radicale.org/ns/sync/2b7c</sync-token></multistatus>"#;

    /// A Fastmail-like calendar-query: absolute hrefs and a CDATA payload.
    const FASTMAIL_QUERY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><D:response><D:href>https://caldav.fastmail.example/dav/calendars/user/ada@example.com/Default/abc.ics</D:href><D:propstat><D:prop><D:getetag>"1700000000"</D:getetag><C:calendar-data><![CDATA[BEGIN:VCALENDAR
BEGIN:VEVENT
UID:abc
END:VEVENT
END:VCALENDAR
]]></C:calendar-data></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>"#;

    #[test]
    fn server_multistatus_documents_parse_props_changes_and_payloads() {
        // Phase `nextcloud_listing_keeps_successful_props_and_ignores_404_propstats`.
        {
            let parsed = parse_default(NEXTCLOUD_PROPFIND).unwrap();
            assert_eq!(parsed.responses.len(), 2);
            let root = &parsed.responses[0];
            assert!(root.is_collection());
            assert_eq!(root.prop_text("getetag"), Some("\"65f1\""));
            assert_eq!(
                root.prop("getcontentlength"),
                None,
                "404 propstat is not a value"
            );
            let file = &parsed.responses[1];
            assert_eq!(
                file.href,
                "/remote.php/dav/files/ada/Notes%20and%20plans.md"
            );
            assert!(!file.is_collection());
            assert_eq!(file.prop_text("getcontentlength"), Some("1234"));
            let fileid = file.prop("fileid").unwrap();
            assert_eq!(fileid.namespace.as_deref(), Some("http://owncloud.org/ns"));
        }
        // Phase `radicale_sync_report_yields_token_changes_and_removals`.
        {
            let parsed = parse_default(RADICALE_SYNC).unwrap();
            assert_eq!(
                parsed.sync_token.as_deref(),
                Some("http://radicale.org/ns/sync/2b7c")
            );
            let changed = &parsed.responses[0];
            assert_eq!(changed.status, None);
            let data = changed.prop("calendar-data").unwrap();
            assert_eq!(
                data.namespace.as_deref(),
                Some("urn:ietf:params:xml:ns:caldav")
            );
            assert!(
                data.text
                    .as_deref()
                    .unwrap()
                    .starts_with("BEGIN:VCALENDAR\r\n")
            );
            assert_eq!(parsed.responses[1].status, Some(404));
            assert!(!parsed.responses[1].has_success());
        }
        // Phase `fastmail_like_cdata_payload_is_preserved`.
        {
            let parsed = parse_default(FASTMAIL_QUERY).unwrap();
            let response = &parsed.responses[0];
            assert!(
                response
                    .href
                    .starts_with("https://caldav.fastmail.example/")
            );
            let data = response
                .prop("calendar-data")
                .unwrap()
                .text
                .as_deref()
                .unwrap();
            assert!(data.contains("UID:abc\n"));
            assert_eq!(response.prop_text("getetag"), Some("\"1700000000\""));
        }
        // Phase `status_without_reason_phrase_is_parsed`.
        {
            let xml = r#"<multistatus xmlns="DAV:"><response><href>/a</href><propstat><prop><getetag>x</getetag></prop><status>HTTP/1.1 200</status></propstat></response></multistatus>"#;
            assert_eq!(
                parse_default(xml).unwrap().responses[0].propstats[0].status,
                Some(200)
            );
        }
    }

    #[test]
    fn hostile_or_malformed_multistatus_documents_are_rejected_within_limits() {
        // Phase `dtd_and_entity_expansion_are_rejected`.
        {
            let laughs = r#"<?xml version="1.0"?>
    <!DOCTYPE lolz [<!ENTITY lol "lol"><!ENTITY lol2 "&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;">]>
    <multistatus xmlns="DAV:"><response><href>&lol2;</href></response></multistatus>"#;
            assert_eq!(parse_default(laughs), Err(XmlError::Doctype));
            let external = r#"<?xml version="1.0"?><!DOCTYPE x [<!ENTITY xxe SYSTEM "file:///etc/passwd">]><multistatus xmlns="DAV:"><response><href>&xxe;</href></response></multistatus>"#;
            assert_eq!(parse_default(external), Err(XmlError::Doctype));
        }
        // Phase `oversized_documents_are_rejected_before_parsing`.
        {
            let limits = Limits {
                max_bytes: 64,
                ..Limits::default()
            };
            let huge = format!(
                "<multistatus xmlns=\"DAV:\">{}</multistatus>",
                " ".repeat(64)
            );
            assert_eq!(parse(&huge, &limits), Err(XmlError::TooLarge));
            assert_eq!(
                parse_bytes(huge.as_bytes(), &limits),
                Err(XmlError::TooLarge)
            );
        }
        // Phase `deep_nesting_is_rejected_without_exhausting_the_stack`.
        {
            let depth = 200_000;
            let xml = format!(
                "<multistatus xmlns=\"DAV:\">{}{}</multistatus>",
                "<x>".repeat(depth),
                "</x>".repeat(depth)
            );
            assert_eq!(parse_default(&xml), Err(XmlError::TooDeep));
        }
        // Phase `node_and_response_counts_are_capped`.
        {
            let many = format!(
                "<multistatus xmlns=\"DAV:\">{}</multistatus>",
                "<response><href>/a</href></response>".repeat(20)
            );
            let limits = Limits {
                max_responses: 10,
                ..Limits::default()
            };
            assert_eq!(parse(&many, &limits), Err(XmlError::TooManyResponses));
            let limits = Limits {
                max_nodes: 16,
                ..Limits::default()
            };
            assert_eq!(parse(&many, &limits), Err(XmlError::TooManyNodes));
        }
        // Phase `non_multistatus_and_malformed_documents_are_errors`.
        {
            assert_eq!(
                parse_default("<html><body>Login</body></html>"),
                Err(XmlError::NotMultistatus)
            );
            assert_eq!(parse_default("<D:multistatus>"), Err(XmlError::Malformed));
            assert_eq!(
                parse_bytes(b"\xff\xfe", &Limits::default()),
                Err(XmlError::NotUtf8)
            );
        }
        // Phase `missing_empty_or_overlong_hrefs_are_errors`.
        {
            for href in ["", "   "] {
                let xml = format!(
                    "<multistatus xmlns=\"DAV:\"><response><href>{href}</href></response></multistatus>"
                );
                assert_eq!(parse_default(&xml), Err(XmlError::InvalidHref));
            }
            let missing = "<multistatus xmlns=\"DAV:\"><response><status>HTTP/1.1 200 OK</status></response></multistatus>";
            assert_eq!(parse_default(missing), Err(XmlError::InvalidHref));
            let long = format!(
                "<multistatus xmlns=\"DAV:\"><response><href>/{}</href></response></multistatus>",
                "a".repeat(4096)
            );
            assert_eq!(parse_default(&long), Err(XmlError::InvalidHref));
        }
    }
}
