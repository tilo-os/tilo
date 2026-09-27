//! Request bodies for PROPFIND and the `CalDAV`/RFC 6578 REPORTs.

use std::fmt::Write as _;

/// The `DAV:` namespace.
pub const DAV: &str = "DAV:";
/// The `CalDAV` namespace (RFC 4791).
pub const CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
/// The `CardDAV` namespace (RFC 6352).
pub const CARDDAV: &str = "urn:ietf:params:xml:ns:carddav";

/// A property name: namespace URI plus local name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PropName {
    pub namespace: &'static str,
    pub name: &'static str,
}

impl PropName {
    /// A `DAV:` property.
    pub const fn dav(name: &'static str) -> Self {
        Self {
            namespace: DAV,
            name,
        }
    }

    /// A `CalDAV` property.
    pub const fn caldav(name: &'static str) -> Self {
        Self {
            namespace: CALDAV,
            name,
        }
    }
}

/// Properties the Tilo clients request.
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

/// A PROPFIND body requesting `props`.
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

/// A `CalDAV` `calendar-multiget` REPORT body for `hrefs`.
pub fn calendar_multiget_body(props: &[PropName], hrefs: &[&str]) -> String {
    let mut ns = Namespaces::new(&[CALDAV], props);
    let prop = ns.prop_element(props);
    let mut out = format!("{XML_DECL}<C:calendar-multiget{}>{prop}", ns.declarations());
    for href in hrefs {
        let _ = write!(out, "<D:href>{}</D:href>", quick_xml::escape::escape(*href));
    }
    out.push_str("</C:calendar-multiget>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multistatus;

    #[test]
    fn request_bodies_are_well_formed_escaped_xml() {
        // Phase `sync_collection_escapes_the_token_and_declares_used_namespaces`.
        {
            let body = sync_collection_body("a<&\"'>", &[props::GETETAG, props::CALENDAR_DATA]);
            assert_eq!(
                body,
                "<?xml version=\"1.0\" encoding=\"utf-8\"?><D:sync-collection xmlns:D=\"DAV:\" \
    xmlns:C=\"urn:ietf:params:xml:ns:caldav\"><D:sync-token>a&lt;&amp;&quot;&apos;&gt;</D:sync-token>\
    <D:sync-level>1</D:sync-level><D:prop><D:getetag/><C:calendar-data/></D:prop></D:sync-collection>"
            );
        }
        // Phase `calendar_query_lists_the_whole_calendar_by_default`.
        {
            let body = calendar_query_body(&[props::GETETAG, props::CALENDAR_DATA], None);
            assert_eq!(
                body,
                "<?xml version=\"1.0\" encoding=\"utf-8\"?><C:calendar-query xmlns:D=\"DAV:\" \
    xmlns:C=\"urn:ietf:params:xml:ns:caldav\"><D:prop><D:getetag/><C:calendar-data/></D:prop>\
    <C:filter><C:comp-filter name=\"VCALENDAR\"/></C:filter></C:calendar-query>"
            );
            assert!(
                calendar_query_body(&[props::GETETAG], Some("VTODO"))
                    .contains("<C:comp-filter name=\"VCALENDAR\"><C:comp-filter name=\"VTODO\"/>")
            );
        }
        // Phase `multiget_and_propfind_bodies_are_well_formed_xml`.
        {
            let multiget =
                calendar_multiget_body(&[props::GETETAG], &["/cal/a&b.ics", "/cal/c.ics"]);
            assert!(multiget.contains("<D:href>/cal/a&amp;b.ics</D:href>"));
            let propfind = propfind_body(&[
                props::RESOURCETYPE,
                PropName {
                    namespace: "http://owncloud.org/ns",
                    name: "fileid",
                },
            ]);
            assert!(
                propfind.contains("xmlns:x1=\"http://owncloud.org/ns\""),
                "{propfind}"
            );
            for body in [multiget, propfind] {
                let options = roxmltree::ParsingOptions::default();
                assert!(
                    roxmltree::Document::parse_with_options(&body, options).is_ok(),
                    "{body}"
                );
            }
            // The parser refuses request documents: they are not multistatus.
            assert!(
                multistatus::parse(&propfind_body(&[]), &multistatus::Limits::default()).is_err()
            );
        }
    }
}
