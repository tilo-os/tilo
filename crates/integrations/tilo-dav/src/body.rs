//! Request bodies for PROPFIND and the `CalDAV`/RFC 6578 REPORTs.

use std::fmt::Write as _;

pub const DAV: &str = "DAV:";
pub const CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
pub const CARDDAV: &str = "urn:ietf:params:xml:ns:carddav";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PropName {
    pub namespace: &'static str,
    pub name: &'static str,
}

impl PropName {
    pub const fn dav(name: &'static str) -> Self {
        Self {
            namespace: DAV,
            name,
        }
    }

    pub const fn caldav(name: &'static str) -> Self {
        Self {
            namespace: CALDAV,
            name,
        }
    }
}

pub mod props {
    use super::PropName;

    pub const DISPLAYNAME: PropName = PropName::dav("displayname");
    pub const GETCONTENTLENGTH: PropName = PropName::dav("getcontentlength");
    pub const GETCONTENTTYPE: PropName = PropName::dav("getcontenttype");
    pub const GETETAG: PropName = PropName::dav("getetag");
    pub const GETLASTMODIFIED: PropName = PropName::dav("getlastmodified");
    pub const RESOURCETYPE: PropName = PropName::dav("resourcetype");
    pub const SYNC_TOKEN: PropName = PropName::dav("sync-token");
    pub const CALENDAR_DATA: PropName = PropName::caldav("calendar-data");
}

const XML_DECL: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?>";

/// Namespace prefixes for one document: `D` for `DAV:`, `C` for `CalDAV`,
/// `CR` for `CardDAV`, `x0`… for anything else, declared in first-use order.
struct Namespaces(Vec<(&'static str, String)>);

impl Namespaces {
    fn new(required: &[&'static str], props: &[PropName]) -> Self {
        let mut set = Self(Vec::new());
        set.prefix(DAV);
        for namespace in required {
            set.prefix(namespace);
        }
        for prop in props {
            set.prefix(prop.namespace);
        }
        set
    }

    fn prefix(&mut self, namespace: &'static str) -> String {
        if let Some((_, prefix)) = self.0.iter().find(|(uri, _)| *uri == namespace) {
            return prefix.clone();
        }
        let prefix = match namespace {
            DAV => "D".to_owned(),
            CALDAV => "C".to_owned(),
            CARDDAV => "CR".to_owned(),
            _ => format!("x{}", self.0.len()),
        };
        self.0.push((namespace, prefix.clone()));
        prefix
    }

    fn declarations(&self) -> String {
        let mut out = String::new();
        for (uri, prefix) in &self.0 {
            let _ = write!(
                out,
                " xmlns:{prefix}=\"{}\"",
                quick_xml::escape::escape(*uri)
            );
        }
        out
    }

    fn prop_element(&mut self, props: &[PropName]) -> String {
        let mut out = String::from("<D:prop>");
        for prop in props {
            let prefix = self.prefix(prop.namespace);
            let _ = write!(out, "<{prefix}:{}/>", prop.name);
        }
        out.push_str("</D:prop>");
        out
    }
}

pub fn propfind_body(props: &[PropName]) -> String {
    let mut ns = Namespaces::new(&[], props);
    let prop = ns.prop_element(props);
    format!(
        "{XML_DECL}<D:propfind{}>{prop}</D:propfind>",
        ns.declarations()
    )
}

/// An RFC 6578 `sync-collection` REPORT body. An empty `token` requests the
/// initial full listing.
pub fn sync_collection_body(token: &str, props: &[PropName]) -> String {
    let mut ns = Namespaces::new(&[], props);
    let prop = ns.prop_element(props);
    format!(
        "{XML_DECL}<D:sync-collection{}><D:sync-token>{}</D:sync-token>\
<D:sync-level>1</D:sync-level>{prop}</D:sync-collection>",
        ns.declarations(),
        quick_xml::escape::escape(token)
    )
}

/// A `CalDAV` `calendar-query` REPORT body listing every `component` inside
/// the `VCALENDAR` (or every object when `component` is `None`).
pub fn calendar_query_body(props: &[PropName], component: Option<&str>) -> String {
    let mut ns = Namespaces::new(&[CALDAV], props);
    let prop = ns.prop_element(props);
    let filter = component.map_or_else(
        || "<C:comp-filter name=\"VCALENDAR\"/>".to_owned(),
        |component| {
            format!(
                "<C:comp-filter name=\"VCALENDAR\"><C:comp-filter name=\"{}\"/></C:comp-filter>",
                quick_xml::escape::escape(component)
            )
        },
    );
    format!(
        "{XML_DECL}<C:calendar-query{}>{prop}<C:filter>{filter}</C:filter></C:calendar-query>",
        ns.declarations()
    )
}
