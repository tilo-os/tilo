//! A workspace's apps as web pages: what a browser that is a device of a
//! workspace's member asks for at [`crate::web::LINK_PAGE_PATH`], and what it
//! is answered.
//!
//! The serving device opens the app's view of the workspace's document and
//! renders it as semantic HTML; the page shows that HTML and sends back what
//! its person does on it, one [`PageAsk::Act`] per click or edit, each
//! answered with the elements that changed. After the handshake on the
//! session's first stream, the same stream carries one JSON message per frame,
//! each behind a little-endian `u32` length, as the handshake's frames are.

use serde::{Deserialize, Serialize};

/// Where a page of a workspace's app is: `/w/<workspace>/<app>`, and
/// `/w/<workspace>/<app>/<target>` for one thing it shows (a note).
pub const PAGE_PREFIX: &str = "/w/";

/// The most bytes one message on a page's stream carries.
pub const MAX_PAGE_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// What a page's address names: the workspace (its id), the app (the last
/// part of its id, `notes` for `so.tilo.notes`) and what it shows first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageAddress {
    pub workspace: String,
    pub app: String,
    /// The semantic target the page activates first: a row's resource id.
    pub show: Option<String>,
}

impl PageAddress {
    /// The address a path names, or `None` for a path that names no page.
    #[must_use]
    pub fn parse(path: &str) -> Option<Self> {
        let rest = path.strip_prefix(PAGE_PREFIX)?;
        let mut parts = rest.splitn(3, '/');
        let workspace = parts.next().filter(|part| !part.is_empty())?;
        let app = parts.next().filter(|part| !part.is_empty())?;
        let show = match parts.next().filter(|part| !part.is_empty()) {
            Some(part) => Some(decode_part(part)?),
            None => None,
        };
        let plain = |part: &str| {
            part.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        };
        (plain(workspace) && plain(app)).then(|| Self {
            workspace: workspace.to_owned(),
            app: app.to_owned(),
            show,
        })
    }
}

/// `part` with its `%XX` escapes decoded; `None` when they do not decode to
/// text.
fn decode_part(part: &str) -> Option<String> {
    let mut bytes = part.bytes();
    let mut out = Vec::with_capacity(part.len());
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let hex = [bytes.next()?, bytes.next()?];
            out.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            out.push(byte);
        }
    }
    String::from_utf8(out).ok()
}

/// What a page asks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageAsk {
    /// Show the page an address names, whole.
    Open(PageAddress),
    /// What its person did: `action` on the element numbered `id`
    /// (`data-p`), at the semantic target it carries (`data-t`).
    Act {
        id: u32,
        target: Option<String>,
        action: PageAction,
    },
}

/// What a person does on an element of a page (`data-a`), as the semantic
/// action of the view it stands for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageAction {
    Activate,
    Toggle,
    Focus,
    /// A field's or a document block's whole text.
    Value(String),
    /// A selection in a field's text, in grapheme indices.
    Select {
        anchor: u32,
        focus: u32,
    },
}

/// One element of a page replaced: the one numbered `id` takes `html`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PagePatch {
    pub id: u32,
    pub html: String,
}

/// A page a serving device opened: the number a page's next asks name it by,
/// and the page whole.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenedPage {
    pub page: u64,
    pub title: String,
    pub css: String,
    pub body: String,
}

/// What a page is answered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageAnswer {
    /// The serving device waits for the keys that open the workspace's
    /// documents, which are on their way: the page follows.
    Loading,
    /// The page whole: its title, the theme's stylesheet and its body.
    Shown {
        title: String,
        css: String,
        body: String,
    },
    /// What the page's last ask changed: the elements, outermost first.
    Patched(Vec<PagePatch>),
    /// The page is not shown here, and why.
    Refused(PageRefusal),
    /// What others changed on the page since, unasked: the elements,
    /// outermost first.
    Changed(Vec<PagePatch>),
}

/// Why a page is not shown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageRefusal {
    /// The device's person is not a member of the workspace.
    NotMember,
    /// This server holds the workspace only as a copy it stores and cannot
    /// read: it renders none of it.
    StoredCopy,
    /// The workspace is not held on this server.
    Elsewhere,
    /// No app of that name is served here.
    NoSuchApp,
    /// The page cannot be shown now; the reason is for logs.
    Unavailable(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_address_names_its_workspace_app_and_note() {
        assert_eq!(
            PageAddress::parse("/w/6f1d3a52-1c7e/notes/notes:page:ab%3Acd"),
            Some(PageAddress {
                workspace: "6f1d3a52-1c7e".into(),
                app: "notes".into(),
                show: Some("notes:page:ab:cd".into()),
            })
        );
        assert_eq!(
            PageAddress::parse("/w/ws/notes"),
            Some(PageAddress {
                workspace: "ws".into(),
                app: "notes".into(),
                show: None,
            })
        );
        assert_eq!(PageAddress::parse("/w/ws"), None);
        assert_eq!(PageAddress::parse("/w/../notes"), None);
        assert_eq!(PageAddress::parse("/w/a b/notes"), None);
        assert_eq!(PageAddress::parse("/x/ws/notes"), None);
    }

    #[test]
    fn what_a_page_says_reads_as_its_script_writes_it() {
        let ask: PageAsk = serde_json::from_str(
            r#"{"act":{"id":7,"target":"notes.block-text:b1","action":{"value":"Milk"}}}"#,
        )
        .unwrap();
        assert_eq!(
            ask,
            PageAsk::Act {
                id: 7,
                target: Some("notes.block-text:b1".into()),
                action: PageAction::Value("Milk".into()),
            }
        );
        let ask: PageAsk =
            serde_json::from_str(r#"{"act":{"id":3,"target":null,"action":"activate"}}"#).unwrap();
        assert!(matches!(
            ask,
            PageAsk::Act {
                action: PageAction::Activate,
                ..
            }
        ));
        assert_eq!(
            serde_json::to_string(&PageAnswer::Patched(vec![PagePatch {
                id: 2,
                html: "<p data-p=\"2\">x</p>".into()
            }]))
            .unwrap(),
            r#"{"patched":[{"id":2,"html":"<p data-p=\"2\">x</p>"}]}"#
        );
    }
}
