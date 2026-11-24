//! A workspace's apps as web pages: what a browser that is a device of a
//! workspace's member asks for at [`crate::web::LINK_PAGE_PATH`], and what it
//! is answered.
//!
//! The serving device opens the app's view of the workspace's document and
//! renders it as semantic HTML; the page shows that HTML and sends back what
//! its person does on it, one [`PageAsk::Act`] per click or edit, each
//! answered with the elements that changed. Beside those answers the page is
//! told what the person's devices show as notifications
//! ([`PageAnswer::Notices`]), which it raises while it is not in front, and a
//! notification they dismiss there is answered in the record their devices
//! keep ([`PageAsk::AnswerNotice`]). After the handshake on the session's
//! first stream, the same stream carries one JSON message per frame, each
//! behind a little-endian `u32` length, as the handshake's frames are.

use serde::{Deserialize, Serialize};

pub mod window;

/// Where a page of a workspace's app is: `/w/<workspace>/<app>`, and
/// `/w/<workspace>/<app>/<target>` for one thing it shows (a note).
/// `/w/<workspace>` is the workspace itself: the apps a member opens there.
pub const PAGE_PREFIX: &str = "/w/";

/// The most bytes one message on a page's stream carries.
pub const MAX_PAGE_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// The provider a host's own page actions are listed and run under: the
/// workspace's business a page offers beside what its records' actions and its
/// governance offer, and which the host runs itself (inviting, being in
/// another workspace of one's own, setting an agent up).
pub const PAGE_ACTIONS_PROVIDER: &str = "tilo.page";

/// One press puts a short invite link on the clipboard: the link is made
/// ahead and carried by the row, since a clipboard write has to happen inside
/// the press and the host's answer comes after it. Running the action hands
/// the next one over.
pub const COPY_INVITE_LINK: &str = "invite-link";

/// The addresses left at a workspace's entry, waiting to be let in: who may
/// invite reads them here and invites one by the address it left.
pub const WAITING_TO_JOIN: &str = "waiting";

/// The person's own devices, and what sets another one up (an agent on a
/// computer, or a computer of theirs).
pub const DEVICES_AND_ACCESS: &str = "devices-and-access";

/// What one of [`PAGE_ACTIONS_PROVIDER`]'s actions answered, when its press
/// opens something on the page rather than saying a line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageActionAnswer {
    /// The addresses waiting at the workspace's entry, and whether the entry
    /// mails an invitation to one it is asked about.
    WaitList {
        mails: bool,
        entries: Vec<WaitingAddress>,
    },
    /// A link the row hands over: on the clipboard from the press, and the
    /// row carries it for the next one.
    Link { link: String },
    /// The person's own devices, and where one more is set up: the site's
    /// setup screen, which makes the code an agent on another computer runs.
    Devices { setup: String },
}

/// An address waiting at a workspace's entry, as the host knows it: what it
/// left, and the id the workspace names it by when it invites it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingAddress {
    /// The address's id on the wait list, as [`PageActionAnswer`]'s inviter
    /// names it.
    pub entry: String,
    /// The address the person left.
    pub contact: String,
    /// Whether an invitation for it was already issued.
    pub invited: bool,
}

/// The app name a page's address carries for the workspace's desktop (`/w/<workspace>/desktop`):
/// the shell's own view of the layout the workspace's people share, its windows each a link to
/// its app's own page.
pub const DESKTOP: &str = "desktop";

/// What a page's address names: the workspace (its id), the app (the last
/// part of its id, `notes` for `so.tilo.notes`) and what it shows first. An
/// address naming no app is the workspace's front page, which lists them.
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
        let app = parts.next().filter(|part| !part.is_empty()).unwrap_or("");
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

    /// The path this address is read from ([`Self::parse`]): what a link to the page names.
    #[must_use]
    pub fn path(&self) -> String {
        let mut path = format!("{PAGE_PREFIX}{}/{}", self.workspace, self.app);
        if let Some(show) = &self.show {
            path.push('/');
            path.push_str(&encode_part(show));
        }
        path
    }

    /// Whether this address names the workspace itself rather than one of its
    /// apps.
    #[must_use]
    pub fn is_home(&self) -> bool {
        self.app.is_empty()
    }
}

/// `text` as an address's third part names it: every byte but an unreserved
/// one percent-encoded, so what it says survives as [`PageAddress::parse`]
/// reads it back.
#[must_use]
pub fn encode_part(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = std::fmt::Write::write_fmt(&mut out, format_args!("%{byte:02X}"));
        }
    }
    out
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
        /// The call its person signed for it, naming the page's app: whose the change the act
        /// makes is, which the serving device checks and every holder of the change checks
        /// again. `None` for an act signed by no key of theirs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signed: Option<Box<tilo_actions::remote::SignedCall>>,
    },
    /// What the browser's call service answered to the step `CallAnswer::Call` gave it, by the
    /// request id that step carried.
    CallResult { request_id: u64, answer: CallAnswer },
    /// The person is done with the notification `id` — they dismissed it, on the page or in the
    /// browser's own list: said in the record of their devices, which takes it off every one.
    AnswerNotice { id: String },
}

/// One notification the person's devices show, as the record of their devices holds it: what a
/// page raises as a browser notification while it is not in front, so a notice an app sends on one
/// of the person's devices reaches them on the one they are looking at. Its text is empty when the
/// person keeps previews on the device it was sent on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageNotice {
    /// The id the record keeps it under: 32 bytes in hex, which is also the notification's tag,
    /// so the browser shows one per notice however many pages raise it.
    pub id: String,
    /// The app that sent it, as the person's language names it.
    pub app_name: String,
    pub title: String,
    pub body: String,
    /// How much it matters: 0 says something, 1 warns, 2 tells of an error.
    pub urgency: i64,
}

/// A call step a page's view asked for, from the device that lays the view out to the browser
/// that shows the page: joining, leaving, muting, deafening, the camera or the screen, and the
/// watch that keeps who is in the call current. The browser carries the call itself — its
/// microphone, camera and screen, and the workspace's live session — so a call is never laid
/// out by a window process.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageCall {
    /// The number the browser answers this step by: the view's own request id.
    pub request_id: u64,
    pub command: runtime_collab::native_media::Request,
}

/// What the browser's call service answered to a [`PageCall`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallAnswer {
    /// The call as it stands after the step, in the vocabulary the view's call service reads.
    Answered(runtime_collab::native_media::Snapshot),
    /// Why the step could not be taken, in the browser's own words.
    Refused(String),
}

/// What a person does on an element of a page (`data-a`): one vocabulary with the page's
/// writer and every window that answers it.
pub use tilo_headless_schema::PageAction;

/// One change to a page, by its elements' numbers (`data-p`), applied in order: an element that
/// comes or goes is put in or taken out alone, so the rest of the page stays the elements it was,
/// with their scroll and focus.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PagePatch {
    /// The element numbered `id` becomes `html`.
    Replace { id: u32, html: String },
    /// `html` goes in as the element numbered `parent`'s child element `at`, counted once every
    /// [`PagePatch::Remove`] before it is applied.
    Insert { parent: u32, at: u32, html: String },
    /// The element numbered `id` is taken out.
    Remove { id: u32 },
}

impl PagePatch {
    /// The markup it brings in: none for a removal.
    #[must_use]
    pub fn html(&self) -> Option<&str> {
        match self {
            Self::Replace { html, .. } | Self::Insert { html, .. } => Some(html),
            Self::Remove { .. } => None,
        }
    }
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
    /// What the page's last ask changed, outermost first.
    Patched(Vec<PagePatch>),
    /// The page is not shown here, and why.
    Refused(PageRefusal),
    /// What others changed on the page since, unasked, outermost first.
    Changed(Vec<PagePatch>),
    /// A call step the page's view asked for: the browser carries it, and answers with
    /// [`PageAsk::CallResult`] under the step's request id.
    Call(PageCall),
    /// The notifications the person's devices show now, sent when that set changed: the page
    /// raises them as browser notifications while it is not in front, and takes away the ones
    /// that left the set — answered on a device of theirs, or run out.
    Notices(Vec<PageNotice>),
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
    /// The app was still working on `request` (its kind) past the bound the
    /// server waits: the page's document was opened again from what the
    /// workspace holds, and nothing that request or anything sent after it
    /// did was applied. The page shows the document again and sends what the
    /// person did since as new acts.
    Reopened { request: String },
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
        // The workspace itself: the page that lists its apps.
        assert_eq!(
            PageAddress::parse("/w/ws"),
            Some(PageAddress {
                workspace: "ws".into(),
                app: String::new(),
                show: None,
            })
        );
        assert!(PageAddress::parse("/w/ws").unwrap().is_home());
        assert_eq!(
            PageAddress::parse("/w/ws/"),
            Some(PageAddress {
                workspace: "ws".into(),
                app: String::new(),
                show: None,
            })
        );
        assert_eq!(PageAddress::parse("/w/"), None);
        assert_eq!(PageAddress::parse("/w/../notes"), None);
        assert_eq!(PageAddress::parse("/w/a b/notes"), None);
        assert_eq!(PageAddress::parse("/x/ws/notes"), None);
    }

    #[test]
    fn what_a_page_names_its_third_part_reads_back_as_written() {
        for text in [
            "notes:page:ab:cd",
            "a note with spaces",
            "chain/field/item",
            "ümlaut and %",
            "",
        ] {
            let path = format!("/w/ws/notes/{}", encode_part(text));
            assert_eq!(
                PageAddress::parse(&path).and_then(|address| address.show),
                (!text.is_empty()).then(|| text.to_owned()),
                "{path}"
            );
        }
        assert_eq!(encode_part("a/b c"), "a%2Fb%20c");
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
                signed: None,
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
        // The page answers a notification the person dismissed as the record names it.
        assert_eq!(
            serde_json::from_str::<PageAsk>(r#"{"answer_notice":{"id":"7f"}}"#).unwrap(),
            PageAsk::AnswerNotice { id: "7f".into() }
        );
        assert_eq!(
            serde_json::to_string(&PageAnswer::Notices(vec![PageNotice {
                id: "7f".into(),
                app_name: "Mail".into(),
                title: "A message from Ada".into(),
                body: String::new(),
                urgency: 0,
            }]))
            .unwrap(),
            r#"{"notices":[{"id":"7f","app_name":"Mail","title":"A message from Ada","body":"","urgency":0}]}"#
        );
        assert_eq!(
            serde_json::to_string(&PageAnswer::Patched(vec![
                PagePatch::Remove { id: 3 },
                PagePatch::Replace {
                    id: 2,
                    html: "<p data-p=\"2\">x</p>".into()
                },
                PagePatch::Insert {
                    parent: 1,
                    at: 0,
                    html: "<p data-p=\"4\">y</p>".into()
                },
            ]))
            .unwrap(),
            r#"{"patched":[{"remove":{"id":3}},{"replace":{"id":2,"html":"<p data-p=\"2\">x</p>"}},{"insert":{"parent":1,"at":0,"html":"<p data-p=\"4\">y</p>"}}]}"#
        );
        // A page that was shown again is answered with what its script reads
        // to redraw quietly (`answer.refused.reopened`), never as an error.
        assert_eq!(
            serde_json::to_string(&PageAnswer::Refused(PageRefusal::Reopened {
                request: "stall".into(),
            }))
            .unwrap(),
            r#"{"refused":{"reopened":{"request":"stall"}}}"#
        );
    }
}
