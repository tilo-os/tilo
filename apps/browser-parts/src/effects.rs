//! What a browser window does that changes no document: navigating its
//! page, reading and operating the page for an agent, its library and
//! settings presentations, history and site data on this device, Picture in
//! Picture and downloads.
//!
//! Every effect reaches this device's engine, files or portals, so only the
//! person at it may invoke them. Each rides the row of what it acts on —
//! the page, the history, a download, the library — which the window
//! publishes; only showing the page again rides the browser itself.

use ui::tilo_resource::{self, Effect};
use ui::webview::{KeyModifiers, PageKey, PageRequest};

/// How long a settle waits for the page to go quiet, and at most, by default.
const PAGE_SETTLE_QUIET_MS: u32 = 500;
const PAGE_SETTLE_TIMEOUT_MS: u32 = 10_000;

#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "browser.effect", device_only)]
pub enum BrowserEffect {
    /// Read browser migration progress
    #[effect(row, returns, kind = "read")]
    ReadContinuity,
    /// Open account
    #[effect(row, returns)]
    OpenServiceSession {
        #[param("Selected account")]
        session_id: String,
    },
    /// Forget account on this device
    ///
    /// Fences and removes the selected website profile before revoking custody.
    #[effect(row, returns, destructive)]
    ForgetServiceSession {
        #[param("Selected account")]
        session_id: String,
    },
    /// Retry account deletion and custody revocation without resolving credentials
    #[effect(row, returns, destructive)]
    RetryForgetServiceSession {
        #[param("Selected account")]
        session_id: String,
    },
    /// List accounts through fresh private custody
    #[effect(row, returns, kind = "read")]
    ListServiceSessions,
    /// Read account connection
    #[effect(row, returns, kind = "read")]
    ReadServiceSession,
    /// Retry bringing your browsing into Tilo
    #[effect(row, returns)]
    RetryContinuity,
    /// Go back
    ///
    /// Answers with the address once the previous page loaded.
    #[effect(row, job, returns, idempotent)]
    Back,
    /// Go forward
    ///
    /// Answers with the address once the next page loaded.
    #[effect(row, job, returns, idempotent)]
    Forward,
    /// Reload
    ///
    /// Answers with the address once the page loaded again.
    #[effect(row, job, returns, idempotent)]
    Reload,
    /// Stop loading
    #[effect(view, row)]
    Stop,
    /// Open address
    ///
    /// Answers with the address once the page loaded.
    #[effect(row, job, returns, idempotent)]
    Navigate {
        #[param("Absolute URL or search terms")]
        url: String,
    },
    /// Open in new tab
    ///
    /// Another Browser, at the address, in a desktop tab right after this
    /// one; this window keeps its page.
    #[effect(row, returns)]
    OpenTab {
        #[param("Absolute URL or search terms")]
        url: String,
        #[param("Open behind this tab instead of in front of it")]
        background: Option<bool>,
    },
    /// Install the browser engine
    ///
    /// Offers the engine runtime's install through the Store, while this
    /// device has none.
    #[effect(row, returns)]
    InstallEngine,
    /// Open page
    ///
    /// Opens a page of the history or a bookmark in this window.
    #[effect(row, job, returns, idempotent)]
    Open {
        #[param("The page's address")]
        url: String,
    },
    /// Show current page
    #[effect(view)]
    ShowPage,
    /// Show history
    #[effect(view, row)]
    ShowHistory,
    /// Show bookmarks
    #[effect(view, row)]
    ShowBookmarks,
    /// Show downloads
    #[effect(view, row)]
    ShowDownloads,
    /// Show extensions
    #[effect(view, row)]
    ShowExtensions,
    /// Show browser settings
    #[effect(view, row)]
    ShowSettings,
    /// Filter library
    #[effect(view, row)]
    SetLibraryFilter {
        #[param("Words to match in the current library section")]
        query: String,
    },
    /// Clear library filter
    #[effect(view, row)]
    ClearLibraryFilter,
    /// Clear browsing history
    #[effect(row, returns, destructive)]
    ClearHistory,
    /// Request local site-data clear
    ///
    /// Asks this device's engine to clear cookies and site storage.
    #[effect(row, returns, destructive)]
    RequestSiteDataClear,
    /// Pause history in this window
    #[effect(row, returns)]
    TogglePause,
    /// Open Picture in Picture
    ///
    /// Floats the page's video, or closes the floating video.
    #[effect(row, job, returns)]
    PictureInPicture,
    /// Reader view
    ///
    /// Shows the page's article alone in a calm reading layout, or the page
    /// again.
    #[effect(row, job, returns)]
    ReaderView,
    /// Download address
    #[effect(row, returns)]
    StartDownload {
        #[param("Absolute HTTP or HTTPS URL")]
        url: String,
    },
    /// Show download
    #[effect(view, row)]
    ShowDownload {
        #[param("The download's id")]
        id: String,
    },
    /// Cancel download
    #[effect(row, returns, destructive)]
    CancelDownload {
        #[param("The download's id")]
        id: String,
    },
    /// Retry download
    #[effect(row, returns)]
    RetryDownload {
        #[param("The download's id")]
        id: String,
    },
    /// Open downloaded file
    #[effect(row, returns)]
    OpenDownload {
        #[param("The download's id")]
        id: String,
    },
    /// Show downloaded file in Files
    #[effect(row, returns)]
    RevealDownload {
        #[param("The download's id")]
        id: String,
    },
    /// Read page content
    ///
    /// Answers with the page's outline: its elements and their refs.
    #[effect(row, job, returns, idempotent, programmatic, untrusted)]
    ReadPage,
    /// What changed on the page
    ///
    /// Answers with the outline lines added and removed since the last read,
    /// or the whole outline when the document is new.
    #[effect(row, job, returns, idempotent, programmatic, untrusted)]
    ReadPageChanges,
    /// Element at a page point
    ///
    /// Answers with what a click at the point would hit, through overlays
    /// and banners, and the interactive element holding it.
    #[effect(row, job, returns, idempotent, programmatic, untrusted)]
    ReadPageElementAt {
        #[param("Viewport x in CSS pixels from the page's left edge")]
        x: f64,
        #[param("Viewport y in CSS pixels from the page's top edge")]
        y: f64,
    },
    /// Wait for the page to settle
    ///
    /// Answers once the document has loaded and stopped changing for a
    /// quiet period, or the longest wait passed.
    #[effect(row, job, returns, idempotent, programmatic, untrusted)]
    WaitPageSettled {
        #[param("How long the page must stay unchanged, milliseconds (default 500)")]
        quiet_ms: Option<u32>,
        #[param("Longest wait, milliseconds (default 10000, at most 20000)")]
        timeout_ms: Option<u32>,
    },
    /// Page console messages
    ///
    /// Console messages and uncaught errors, collected from the first time
    /// an agent touched the tab.
    #[effect(row, job, returns, idempotent, programmatic, untrusted)]
    ReadPageConsole {
        #[param(
            "Only entries after this sequence number: the `next` of the previous answer (default 0)"
        )]
        since: Option<u64>,
    },
    /// Page network requests
    ///
    /// Method, address, type, status, size and failure of the page's
    /// requests, never headers, bodies or cookies.
    #[effect(row, job, returns, idempotent, programmatic, untrusted)]
    ReadPageNetwork {
        #[param(
            "Only entries after this sequence number: the `next` of the previous answer (default 0)"
        )]
        since: Option<u64>,
    },
    /// Click page element
    #[effect(row, job, returns, programmatic, untrusted)]
    ClickPage {
        #[param("Element ref from the page outline, such as d1e5")]
        element: String,
    },
    /// Fill page field
    ///
    /// A password or card field is refused: those take the secret fill.
    #[effect(row, job, returns, programmatic, untrusted)]
    FillPage {
        #[param("Element ref from the page outline, such as d1e5")]
        element: String,
        #[param("Text that replaces the field's contents")]
        text: String,
    },
    /// Fill page password field
    ///
    /// Fills a password or card field; the text is a secret kept out of
    /// logs and history.
    #[effect(row, job, returns, programmatic, untrusted)]
    FillPageSecret {
        #[param("Element ref from the page outline, such as d1e5")]
        element: String,
        #[param(
            secret,
            "Password or card number for the field; kept out of logs and history"
        )]
        text: String,
    },
    /// Choose page options
    #[effect(row, job, returns, programmatic, untrusted)]
    SelectPage {
        #[param("Element ref from the page outline, such as d1e5")]
        element: String,
        #[param("Option values or visible labels to choose")]
        values: Vec<String>,
    },
    /// Press key on page
    #[effect(row, job, returns, programmatic, untrusted)]
    PressPage {
        #[param("Key or chord such as Enter, Tab, Escape, ArrowDown or Control+a")]
        key: String,
        #[param("Element ref to focus first; otherwise the focused element")]
        element: Option<String>,
    },
    /// Submit page form
    ///
    /// A form that sends its data to another site than the page's own is
    /// refused, through submit, click or Enter, unless `cross_origin` allows
    /// it.
    #[effect(row, job, returns, programmatic, untrusted)]
    SubmitPage {
        #[param("Element ref from the page outline, such as d1e5")]
        element: String,
        #[param("Allow sending the form to another site than the page's own (default false)")]
        cross_origin: Option<bool>,
    },
    /// Bookmark this page personally
    #[effect(row, returns, idempotent)]
    BookmarkPersonalPage {
        #[param("Current page URL")]
        url: String,
        #[param("Current page title")]
        title: String,
    },
    /// Import personal bookmarks
    #[effect(row, returns)]
    ImportPersonalBookmarks {
        #[param(secret, "Authenticated reviewed personal bookmark bundle")]
        bundle: String,
    },
    /// Remove personal bookmark
    #[effect(row, returns)]
    RemovePersonalBookmark {
        #[param("Personal bookmark URL")]
        url: String,
    },
    /// Remove personal bookmark item
    #[effect(row, returns)]
    RemovePersonalImportedNode {
        #[param("Reviewed source snapshot")]
        source: String,
        #[param("Exact folder or placement")]
        id: String,
    },
    /// Show personal bookmark folder
    #[effect(view, row)]
    ShowPersonalBookmarkFolder {
        #[param("Reviewed source snapshot; empty for overview")]
        source: String,
        #[param("Folder; absent for source roots")]
        id: Option<String>,
        #[param("First displayed ordered item")]
        offset: u32,
    },
}

impl BrowserEffect {
    /// What the window does for this effect.
    pub fn operation(self) -> BrowserOperation {
        let page = BrowserOperation::Page;
        match self {
            Self::ReadContinuity => BrowserOperation::ReadContinuity,
            Self::OpenServiceSession { .. } => BrowserOperation::OpenServiceSession,
            Self::ForgetServiceSession { .. } | Self::RetryForgetServiceSession { .. } => {
                BrowserOperation::ForgetServiceSession
            }
            Self::ReadServiceSession => BrowserOperation::ReadServiceSession,
            Self::ListServiceSessions => BrowserOperation::ListServiceSessions,
            Self::RetryContinuity => BrowserOperation::RetryContinuity,
            Self::BookmarkPersonalPage { url, title } => {
                BrowserOperation::BookmarkPersonalPage { url, title }
            }
            Self::ImportPersonalBookmarks { bundle } => {
                BrowserOperation::ImportPersonalBookmarks { bundle }
            }
            Self::RemovePersonalBookmark { url } => {
                BrowserOperation::RemovePersonalBookmark { url }
            }
            Self::RemovePersonalImportedNode { source, id } => {
                BrowserOperation::RemovePersonalImportedNode { source, id }
            }
            Self::ShowPersonalBookmarkFolder { source, id, offset } => {
                BrowserOperation::ShowPersonalBookmarkFolder { source, id, offset }
            }
            Self::Back => BrowserOperation::Back,
            Self::Forward => BrowserOperation::Forward,
            Self::Reload => BrowserOperation::Reload,
            Self::Stop => BrowserOperation::Stop,
            Self::Navigate { url } | Self::Open { url } => BrowserOperation::Navigate { url },
            Self::OpenTab { url, background } => BrowserOperation::OpenInNewTab {
                url,
                background: background.unwrap_or(false),
            },
            Self::InstallEngine => BrowserOperation::InstallEngine,
            Self::ShowPage => BrowserOperation::ShowPage,
            Self::ShowHistory => BrowserOperation::ShowLibrary {
                section: LibrarySection::History,
            },
            Self::ShowBookmarks => BrowserOperation::ShowLibrary {
                section: LibrarySection::Bookmarks,
            },
            Self::ShowDownloads | Self::ShowDownload { .. } => BrowserOperation::ShowLibrary {
                section: LibrarySection::Downloads,
            },
            Self::ShowExtensions => BrowserOperation::ShowLibrary {
                section: LibrarySection::Extensions,
            },
            Self::ShowSettings => BrowserOperation::ShowSettings,
            Self::SetLibraryFilter { query } => BrowserOperation::SetLibraryFilter { query },
            Self::ClearLibraryFilter => BrowserOperation::ClearLibraryFilter,
            Self::ClearHistory => BrowserOperation::ClearHistory,
            Self::RequestSiteDataClear => BrowserOperation::RequestWebsiteDataClear,
            Self::TogglePause => BrowserOperation::ToggleHistoryPause,
            Self::PictureInPicture => BrowserOperation::TogglePictureInPicture,
            Self::ReaderView => BrowserOperation::ToggleReaderView,
            Self::StartDownload { url } => BrowserOperation::StartDownload { url },
            Self::CancelDownload { id } => BrowserOperation::CancelDownload { id },
            Self::RetryDownload { id } => BrowserOperation::RetryDownload { id },
            Self::OpenDownload { id } => BrowserOperation::OpenDownload { id },
            Self::RevealDownload { id } => BrowserOperation::RevealDownload { id },
            Self::ReadPage => page(PageRequest::Outline),
            Self::ReadPageChanges => page(PageRequest::Changes),
            Self::ReadPageElementAt { x, y } => page(PageRequest::At { x, y }),
            Self::WaitPageSettled {
                quiet_ms,
                timeout_ms,
            } => page(PageRequest::Settle {
                quiet_ms: quiet_ms.unwrap_or(PAGE_SETTLE_QUIET_MS),
                timeout_ms: timeout_ms.unwrap_or(PAGE_SETTLE_TIMEOUT_MS),
            }),
            Self::ReadPageConsole { since } => page(PageRequest::Console {
                since: since.unwrap_or(0),
            }),
            Self::ReadPageNetwork { since } => page(PageRequest::Network {
                since: since.unwrap_or(0),
            }),
            Self::ClickPage { element } => page(PageRequest::Click { element }),
            Self::FillPage { element, text } => page(PageRequest::Fill {
                element,
                text,
                secret: false,
            }),
            Self::FillPageSecret { element, text } => page(PageRequest::Fill {
                element,
                text,
                secret: true,
            }),
            Self::SelectPage { element, values } => page(PageRequest::Select { element, values }),
            Self::PressPage { key, element } => page(PageRequest::Press {
                element: element.filter(|element| !element.is_empty()),
                key: PageKey::parse(&key).unwrap_or_else(|| PageKey {
                    key,
                    modifiers: KeyModifiers::default(),
                }),
            }),
            Self::SubmitPage {
                element,
                cross_origin,
            } => page(PageRequest::Submit {
                element,
                cross_origin: cross_origin.unwrap_or(false),
            }),
        }
    }
}

/// The library's sections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibrarySection {
    History,
    Bookmarks,
    Downloads,
    Extensions,
}

/// What Browser does for an effect, once decoded.
#[derive(Clone, Debug)]
pub enum BrowserOperation {
    BookmarkPersonalPage {
        url: String,
        title: String,
    },
    ImportPersonalBookmarks {
        bundle: String,
    },
    ReadContinuity,
    OpenServiceSession,
    ForgetServiceSession,
    ReadServiceSession,
    ListServiceSessions,
    RetryContinuity,
    RemovePersonalBookmark {
        url: String,
    },
    RemovePersonalImportedNode {
        source: String,
        id: String,
    },
    ShowPersonalBookmarkFolder {
        source: String,
        id: Option<String>,
        offset: u32,
    },
    Back,
    Forward,
    Reload,
    Stop,
    Navigate {
        url: String,
    },
    /// Offer the engine runtime's install through the Store.
    InstallEngine,
    /// Another Browser in a desktop tab right after this one.
    OpenInNewTab {
        url: String,
        background: bool,
    },
    ShowLibrary {
        section: LibrarySection,
    },
    SetLibraryFilter {
        query: String,
    },
    ClearLibraryFilter,
    ShowSettings,
    ShowPage,
    ClearHistory,
    RequestWebsiteDataClear,
    TogglePictureInPicture,
    /// Show the page's article in reader view, or the page again.
    ToggleReaderView,
    ToggleHistoryPause,
    StartDownload {
        url: String,
    },
    CancelDownload {
        id: String,
    },
    RetryDownload {
        id: String,
    },
    OpenDownload {
        id: String,
    },
    RevealDownload {
        id: String,
    },
    Page(PageRequest),
}
