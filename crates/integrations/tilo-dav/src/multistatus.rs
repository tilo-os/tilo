//! Bounded RFC 4918 `multistatus` parsing.
//!
//! A DAV server is an untrusted peer: the parser caps input size, node count and
//! nesting depth, rejects any DTD (so no entity expansion happens), and insists on a
//! `multistatus` root, so an HTML error page answered with a 2xx status is an error
//! rather than an empty, and therefore "authoritative", listing.
//!
//! Elements are matched by local name: servers pick their own namespace prefixes and a
//! few omit the `DAV:` namespace entirely. Each [`Property`] keeps its namespace for
//! callers that need to tell `DAV:` and extension properties apart.

use std::fmt;

use roxmltree::{Document, Node, ParsingOptions};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_bytes: usize,
    pub max_responses: usize,
    pub max_depth: usize,
    pub max_nodes: u32,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XmlError {
    TooLarge,
    NotUtf8,
    Doctype,
    TooManyNodes,
    TooDeep,
    TooManyResponses,
    InvalidHref,
    NotMultistatus,
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Multistatus {
    /// The RFC 6578 `sync-token` that is a direct child of the root, as a
    /// `sync-collection` REPORT returns it. Trimmed; `None` when absent/empty.
    pub sync_token: Option<String>,
    pub responses: Vec<Response>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    /// The first `href`, trimmed and exactly as the server spelled it.
    pub href: String,
    /// The response-level status code (a whole-resource status such as the
    /// 404 a `sync-collection` report uses for removed members).
    pub status: Option<u16>,
    pub propstats: Vec<Propstat>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Propstat {
    pub status: Option<u16>,
    pub props: Vec<Property>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Property {
    pub namespace: Option<String>,
    pub name: String,
    /// The element's text content, untrimmed (`calendar-data` and
    /// `address-data` are payloads where whitespace matters).
    pub text: Option<String>,
    /// Local names of descendant elements, e.g. `collection` in `resourcetype`
    /// or `read` nested inside a privilege.
    pub children: Vec<String>,
}

impl Property {
    pub fn trimmed_text(&self) -> Option<&str> {
        self.text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
    }

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
    pub fn prop(&self, name: &str) -> Option<&Property> {
        self.propstats
            .iter()
            .filter(|propstat| propstat.is_success())
            .flat_map(|propstat| propstat.props.iter())
            .find(|prop| prop.name == name)
    }

    /// Trimmed, non-empty text of a returned property.
    pub fn prop_text(&self, name: &str) -> Option<&str> {
        self.prop(name).and_then(Property::trimmed_text)
    }

    pub fn is_collection(&self) -> bool {
        self.prop("resourcetype")
            .is_some_and(|prop| prop.has_child("collection"))
    }

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

/// Refuse deep nesting before the tree parser sees the document: its recursion is
/// bounded only by the stack, so a hostile server could crash the client with a few
/// hundred KiB of `<x>`. This is a linear scan of tag delimiters (skipping comments,
/// CDATA, processing instructions and quoted attribute values); well-formedness is
/// still the parser's job.
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
                    text: if matches!(
                        prop.tag_name().name(),
                        "current-user-principal" | "calendar-home-set"
                    ) {
                        prop.descendants()
                            .find(|node| node.is_element() && node.tag_name().name() == "href")
                            .and_then(|node| node.text())
                            .map(ToOwned::to_owned)
                    } else {
                        prop.text().map(ToOwned::to_owned)
                    },
                    children: prop
                        .descendants()
                        .skip(1)
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
