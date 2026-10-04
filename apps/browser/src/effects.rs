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

use crate::browser_widget::{BrowserOperation, LibrarySection};

#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "browser.effect", device_only)]
pub enum BrowserEffect {
    /// Go back
    ///
    /// Answers with the address once the previous page loaded.
    #[effect(row, job, idempotent)]
    Back,
    /// Go forward
    ///
    /// Answers with the address once the next page loaded.
    #[effect(row, job, idempotent)]
    Forward,
    /// Reload
    ///
    /// Answers with the address once the page loaded again.
    #[effect(row, job, idempotent)]
    Reload,
    /// Stop loading
    #[effect(view, row)]
    Stop,
    /// Open address
    ///
    /// Answers with the address once the page loaded.
    #[effect(row, job, idempotent)]
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
    #[effect(row, job, idempotent)]
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
    #[effect(row, job)]
    PictureInPicture,
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
    #[effect(row, job, idempotent, programmatic, permission = "write")]
    ReadPage,
    /// Click page element
    #[effect(row, job, programmatic)]
    ClickPage {
        #[param("Element ref from the page outline, such as d1e5")]
        element: String,
    },
    /// Fill page field
    #[effect(row, job, programmatic)]
    FillPage {
        #[param("Element ref from the page outline, such as d1e5")]
        element: String,
        #[param("Text that replaces the field's contents")]
        text: String,
    },
    /// Choose page options
    #[effect(row, job, programmatic)]
    SelectPage {
        #[param("Element ref from the page outline, such as d1e5")]
        element: String,
        #[param("Option values or visible labels to choose")]
        values: Vec<String>,
    },
    /// Press key on page
    #[effect(row, job, programmatic)]
    PressPage {
        #[param("Key or chord such as Enter, Tab, Escape, ArrowDown or Control+a")]
        key: String,
        #[param("Element ref to focus first; otherwise the focused element")]
        element: Option<String>,
    },
    /// Submit page form
    #[effect(row, job, programmatic)]
    SubmitPage {
        #[param("Element ref from the page outline, such as d1e5")]
        element: String,
    },
}

impl BrowserEffect {
    /// What the window does for this effect.
    pub(crate) fn operation(self) -> BrowserOperation {
        let page = BrowserOperation::Page;
        match self {
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
            Self::StartDownload { url } => BrowserOperation::StartDownload { url },
            Self::CancelDownload { id } => BrowserOperation::CancelDownload { id },
            Self::RetryDownload { id } => BrowserOperation::RetryDownload { id },
            Self::OpenDownload { id } => BrowserOperation::OpenDownload { id },
            Self::RevealDownload { id } => BrowserOperation::RevealDownload { id },
            Self::ReadPage => page(PageRequest::Outline),
            Self::ClickPage { element } => page(PageRequest::Click { element }),
            Self::FillPage { element, text } => page(PageRequest::Fill { element, text }),
            Self::SelectPage { element, values } => page(PageRequest::Select { element, values }),
            Self::PressPage { key, element } => page(PageRequest::Press {
                element: element.filter(|element| !element.is_empty()),
                key: PageKey::parse(&key).unwrap_or_else(|| PageKey {
                    key,
                    modifiers: KeyModifiers::default(),
                }),
            }),
            Self::SubmitPage { element } => page(PageRequest::Submit { element }),
        }
    }
}
