//! What a browser window does that changes no document: navigating its
//! page, reading and operating the page for an agent, its library and
//! settings presentations, history and site data on this device, Picture in
//! Picture, zoom, downloads and extensions.
//!
//! Every effect reaches this device's engine, files or portals, so only the
//! person at it may invoke them. Each rides the row of what it acts on —
//! the page, the history, a download, the library — which the window
//! publishes; only showing the page again rides the browser itself.

use ui::tilo_resource::{self, Effect};
use ui::webview::{
    FrameId, KeyModifiers, PageKey, PageRequest, PointerButton, PointerPoint, PointerStep,
    PointerStepKind, ScrollTarget,
};

/// How long a settle waits for the page to go quiet, and at most, by default.
const PAGE_SETTLE_QUIET_MS: u32 = 500;
const PAGE_SETTLE_TIMEOUT_MS: u32 = 10_000;
/// Moves the pointer makes between a drag's two ends by default.
const PAGE_DRAG_STEPS: u32 = 8;

/// One `kind=move x=10 y=10 element=d1e4` step of a pointer gesture, or
/// nothing when a field is missing or unreadable; the engine then refuses
/// the gesture as out of bounds.
fn pointer_step(text: &str) -> Option<PointerStep> {
    let mut kind = None;
    let mut element = None;
    let mut frame = None;
    let mut x = None;
    let mut y = None;
    for field in text.split_whitespace() {
        let (name, value) = field.split_once('=')?;
        match name {
            "kind" => kind = Some(value),
            "element" if !value.is_empty() => element = Some(value.to_string()),
            "frame" if !value.is_empty() => frame = Some(page_frame(Some(value.to_string()))?),
            "x" => x = value.parse::<f64>().ok(),
            "y" => y = value.parse::<f64>().ok(),
            _ => return None,
        }
    }
    let kind = match kind? {
        "move" => PointerStepKind::Move,
        "down" => PointerStepKind::Down,
        "up" => PointerStepKind::Up,
        _ => return None,
    };
    // A step naming an element is placed inside its box; every other step is
    // already a viewport position.
    let (x, y) = (x?, y?);
    let at = if element.is_some() {
        PointerPoint::InElement { x, y }
    } else {
        PointerPoint::Viewport { x, y }
    };
    Some(PointerStep {
        kind,
        at,
        element,
        frame,
    })
}

#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "browser.effect")]
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
    /// one; this window keeps its page. The tab is an instance of its own:
    /// its page is addressed through that instance's peer id, not this
    /// window's refs.
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
    /// Turn extension on
    #[effect(row, returns)]
    TurnOnExtension {
        #[param("The extension's id")]
        id: String,
    },
    /// Turn extension off
    ///
    /// It keeps what it stored, such as its settings and logins.
    #[effect(row, returns)]
    TurnOffExtension {
        #[param("The extension's id")]
        id: String,
    },
    /// Pin extension to the toolbar
    #[effect(row, returns)]
    PinExtension {
        #[param("The extension's id")]
        id: String,
    },
    /// Unpin extension from the toolbar
    #[effect(row, returns)]
    UnpinExtension {
        #[param("The extension's id")]
        id: String,
    },
    /// Use extension
    ///
    /// What its toolbar button does: opens its popup over this page, or lets
    /// it act on this page.
    #[effect(row, returns)]
    UseExtension {
        #[param("The extension's id")]
        id: String,
    },
    /// Send words to extension
    ///
    /// What Enter does after its keyword in the address: the extension acts
    /// on the words, often by opening a page in this window.
    #[effect(row, returns)]
    SendToExtension {
        #[param("The extension's id")]
        id: String,
        #[param("The words, as typed after its keyword")]
        text: String,
    },
    /// Add extension from the Web Store
    ///
    /// On an extension's Chrome Web Store page: shows what it may do, and
    /// the person adds it with Add extension.
    #[effect(row, returns)]
    AddExtension,
    /// Open extension settings
    #[effect(row, returns)]
    OpenExtensionSettings {
        #[param("The extension's id")]
        id: String,
    },
    /// Open side panel
    ///
    /// Shows the extension's side panel beside this page, where it has one
    /// for the page.
    #[effect(row, returns)]
    OpenExtensionSidePanel {
        #[param("The extension's id")]
        id: String,
    },
    /// Close side panel
    #[effect(row, returns)]
    CloseExtensionSidePanel {
        #[param("The extension's id")]
        id: String,
    },
    /// Revoke extension access
    ///
    /// Takes back the access you allowed it after it was added. What it
    /// needed when you added it stays.
    #[effect(row, returns)]
    RevokeExtensionAccess {
        #[param("The extension's id")]
        id: String,
    },
    /// Remove extension
    ///
    /// Removes it from this device with everything it stored.
    #[effect(row, returns, destructive)]
    RemoveExtension {
        #[param("The extension's id")]
        id: String,
    },
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
    /// Forget one visited page
    ///
    /// Removes this one page from your browsing history. Bookmarks are kept.
    #[effect(row, returns, destructive)]
    ForgetVisit {
        #[param("The page's address")]
        url: String,
    },
    /// Clear browsing history
    ///
    /// Your saved browsing history will be removed. Bookmarks are kept.
    #[effect(row, returns, destructive)]
    ClearHistory,
    /// Clear site data
    ///
    /// Cookies and site storage on this device will be cleared. You may be signed out of websites.
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
    /// Read downloaded file
    ///
    /// Answers with the completed download's text, bounded; a file that is
    /// not UTF-8 text answers only its type and size.
    #[effect(row, returns, idempotent, audience = "agents", untrusted)]
    ReadDownload {
        #[param("The download's id")]
        id: String,
    },
    /// Keep the download
    ///
    /// Saves the file the page offered, answering where it went.
    #[effect(row, returns)]
    SaveDownload {
        #[param("The download's id")]
        id: String,
    },
    /// Discard the download
    ///
    /// Refuses the file the page offered, which is not saved.
    #[effect(row, returns, destructive)]
    DiscardDownload {
        #[param("The download's id")]
        id: String,
    },
    /// Zoom in
    ///
    /// Makes the page larger on this device; answers the page's size.
    #[effect(row, returns)]
    ZoomIn,
    /// Zoom out
    ///
    /// Makes the page smaller on this device; answers the page's size.
    #[effect(row, returns)]
    ZoomOut,
    /// Show actual size
    ///
    /// Puts the page back at its own size.
    #[effect(row, returns, idempotent)]
    ResetZoom,
    /// Read page content
    ///
    /// Answers with the page's outline: its elements and their refs. A frame
    /// carries the id that addresses its own document.
    #[effect(row, job, returns, idempotent, audience = "agents", untrusted)]
    ReadPage {
        #[param(
            "Frame id from the outline, such as 1 or 1.2: the action then runs in that frame's own document, with its own outline and refs"
        )]
        frame: Option<String>,
    },
    /// What changed on the page
    ///
    /// Answers with the outline lines added and removed since the last read,
    /// or the whole outline when the document is new.
    #[effect(row, job, returns, idempotent, audience = "agents", untrusted)]
    ReadPageChanges {
        #[param(
            "Frame id from the outline, such as 1 or 1.2: the action then runs in that frame's own document, with its own outline and refs"
        )]
        frame: Option<String>,
    },
    /// Element at a page point
    ///
    /// Answers with what a click at the point would hit, through overlays
    /// and banners, and the interactive element holding it.
    #[effect(row, job, returns, idempotent, audience = "agents", untrusted)]
    ReadPageElementAt {
        #[param("Viewport x in CSS pixels from the page's left edge")]
        x: f64,
        #[param("Viewport y in CSS pixels from the page's top edge")]
        y: f64,
        #[param(
            "Frame id from the outline, such as 1 or 1.2: the action then runs in that frame's own document, with its own outline and refs"
        )]
        frame: Option<String>,
    },
    /// Wait for the page to settle
    ///
    /// Answers once the document has loaded and stopped changing for a
    /// quiet period, or the longest wait passed.
    #[effect(row, job, returns, idempotent, audience = "agents", untrusted)]
    WaitPageSettled {
        #[param("How long the page must stay unchanged, milliseconds (default 500)")]
        quiet_ms: Option<u32>,
        #[param("Longest wait, milliseconds (default 10000, at most 20000)")]
        timeout_ms: Option<u32>,
        #[param(
            "Frame id from the outline, such as 1 or 1.2: the action then runs in that frame's own document, with its own outline and refs"
        )]
        frame: Option<String>,
    },
    /// Page console messages
    ///
    /// Console messages and uncaught errors, collected from the first time
    /// an agent touched the tab.
    #[effect(row, job, returns, idempotent, audience = "agents", untrusted)]
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
    #[effect(row, job, returns, idempotent, audience = "agents", untrusted)]
    ReadPageNetwork {
        #[param(
            "Only entries after this sequence number: the `next` of the previous answer (default 0)"
        )]
        since: Option<u64>,
    },
    /// Click page element
    #[effect(row, job, returns, audience = "agents", untrusted)]
    ClickPage {
        #[param("Element ref from the page outline, such as d1e5")]
        element: String,
        #[param(
            "Frame id from the outline, such as 1 or 1.2: the action then runs in that frame's own document, with its own outline and refs"
        )]
        frame: Option<String>,
    },
    /// Move the pointer over a page element
    ///
    /// Applies the page's hover styles there, such as a menu that opens
    /// when the pointer rests on its trigger, and answers with what the
    /// pointer is over now and what changed in the outline.
    #[effect(row, job, returns, audience = "agents", untrusted)]
    HoverPage {
        #[param("Element ref from the page outline, such as d1e5")]
        element: Option<String>,
        #[param("Viewport x in CSS pixels from the page's left edge, when no element is named")]
        x: Option<f64>,
        #[param("Viewport y in CSS pixels from the page's top edge, when no element is named")]
        y: Option<f64>,
    },
    /// Drag a page element or point
    ///
    /// Presses the pointer, moves it and releases it, as a person dragging
    /// does, so reordering lists and dropped targets receive the drag.
    #[effect(row, job, returns, audience = "agents", untrusted)]
    DragPage {
        #[param("Element ref to pick up, such as d1e5")]
        from: Option<String>,
        #[param("Viewport x in CSS pixels to pick up from, when no element is named")]
        from_x: Option<f64>,
        #[param("Viewport y in CSS pixels to pick up from, when no element is named")]
        from_y: Option<f64>,
        #[param("Element ref to drop on, such as d1e7")]
        to: Option<String>,
        #[param("Viewport x in CSS pixels to drop at, when no element is named")]
        to_x: Option<f64>,
        #[param("Viewport y in CSS pixels to drop at, when no element is named")]
        to_y: Option<f64>,
        #[param("How many moves the pointer makes between the two ends (default 8, at most 64)")]
        steps: Option<u32>,
    },
    /// Draw a pointer path on a page element
    ///
    /// Presses the pointer at the first point, moves it through the rest and
    /// releases it at the last, as drawing a signature on a canvas does.
    #[effect(row, job, returns, audience = "agents", untrusted)]
    PointerPath {
        #[param("Element ref to draw on, such as d1e5")]
        element: Option<String>,
        #[param(
            "Points as [x, y] pairs, measured from the element's top left corner when an element is named, otherwise from the viewport's"
        )]
        points: Vec<[f64; 2]>,
    },
    /// Fill page field
    ///
    /// A password or card field is refused: those take the secret fill.
    #[effect(row, job, returns, audience = "agents", untrusted)]
    FillPage {
        #[param("Element ref from the page outline, such as d1e5")]
        element: String,
        #[param("Text that replaces the field's contents")]
        text: String,
        #[param(
            "Frame id from the outline, such as 1 or 1.2: the action then runs in that frame's own document, with its own outline and refs"
        )]
        frame: Option<String>,
    },
    /// Fill page password field
    ///
    /// Fills a password or card field; the text is a secret kept out of
    /// logs and history.
    #[effect(row, job, returns, audience = "agents", untrusted)]
    FillPageSecret {
        #[param("Element ref from the page outline, such as d1e5")]
        element: String,
        #[param(
            secret,
            "Password or card number for the field; kept out of logs and history"
        )]
        text: String,
        #[param(
            "Frame id from the outline, such as 1 or 1.2: the action then runs in that frame's own document, with its own outline and refs"
        )]
        frame: Option<String>,
    },
    /// Choose page options
    #[effect(row, job, returns, audience = "agents", untrusted)]
    SelectPage {
        #[param("Element ref from the page outline, such as d1e5")]
        element: String,
        #[param("Option values or visible labels to choose")]
        values: Vec<String>,
        #[param(
            "Frame id from the outline, such as 1 or 1.2: the action then runs in that frame's own document, with its own outline and refs"
        )]
        frame: Option<String>,
    },
    /// Choose files for a page upload
    ///
    /// Sets what an `<input type=file>` holds, as a person's own file
    /// picker would, and the page's `input` and `change` handlers run. Each
    /// path is absolute and must be a regular file this Browser may read:
    /// a relative path, a folder or an unreadable file is refused. It also
    /// answers a file chooser a click opened, so a page that asks for a file
    /// can be finished without a native picker.
    #[effect(row, job, returns, audience = "agents", untrusted)]
    FillPageFiles {
        #[param("Element ref from the page outline, such as d1e5")]
        element: String,
        #[param("Absolute paths of the files to choose, in order")]
        paths: Vec<String>,
        #[param(
            "Frame id from the outline, such as 1 or 1.2: the action then runs in that frame's own document, with its own outline and refs"
        )]
        frame: Option<String>,
    },
    /// Press key on page
    #[effect(row, job, returns, audience = "agents", untrusted)]
    PressPage {
        #[param("Key or chord such as Enter, Tab, Escape, ArrowDown or Control+a")]
        key: String,
        #[param("Element ref to focus first; otherwise the focused element")]
        element: Option<String>,
        #[param(
            "Frame id from the outline, such as 1 or 1.2: the action then runs in that frame's own document, with its own outline and refs"
        )]
        frame: Option<String>,
    },
    /// Submit page form
    ///
    /// A form that sends its data to another site than the page's own is
    /// refused, through submit, click or Enter, unless `cross_origin` allows
    /// it.
    #[effect(row, job, returns, audience = "agents", untrusted)]
    SubmitPage {
        #[param("Element ref from the page outline, such as d1e5")]
        element: String,
        #[param("Allow sending the form to another site than the page's own (default false)")]
        cross_origin: Option<bool>,
        #[param(
            "Frame id from the outline, such as 1 or 1.2: the action then runs in that frame's own document, with its own outline and refs"
        )]
        frame: Option<String>,
    },
    /// Answer the page's question
    ///
    /// What its Close, Allow, Send, Leave or Reload button does: the page
    /// waits on its question until it is answered.
    #[effect(row, returns)]
    AcceptPageDialog {
        #[param("The question's id")]
        dialog: u32,
        #[param("The answer to a prompt; what its field holds when absent")]
        text: Option<String>,
    },
    /// Cancel the page's question
    ///
    /// What its Cancel button does: a confirm answers no, a prompt nothing,
    /// and a page asking to be left stays.
    #[effect(row, returns)]
    DismissPageDialog {
        #[param("The question's id")]
        dialog: u32,
    },
    /// Answer the site's permission request
    ///
    /// What its Allow or Block button does: the site's request for a
    /// capability, such as its location, is granted or refused, and the page
    /// goes on from there. Granting or refusing reaches the site's own
    /// privacy boundary, so it is asked first like the person's prompt is.
    #[effect(row, privileged, returns)]
    AnswerPermission {
        #[param("The request's id from the page's outline")]
        request: u32,
        #[param("Allow the site the capability (true) or refuse it (false)")]
        allow: bool,
    },
    /// Move the pointer on the page
    ///
    /// Runs one gesture of real pointer input, as a hand makes it: steps in
    /// order, each a move, a press or a release, placed in an element's box
    /// or at a viewport point, with the button held between a press and its
    /// release. Hovering reveals what a page shows under the pointer
    /// (`kind=move x=120 y=90`, then `kind=down` and `kind=up` on what
    /// appeared); dragging carries one element onto another (`kind=down
    /// element=d1e2`, `kind=move element=d1e3`, `kind=up element=d1e3`);
    /// drawing is a stroke per press and release (`kind=down element=d1e4
    /// x=20 y=20`, moves across, `kind=up`). The answer reports what
    /// changed on the page.
    #[effect(row, job, returns, untrusted)]
    PointerPage {
        #[param(
            "Steps in order, one per item, such as kind=move element=d1e4 x=10 y=10: a step naming an element is placed inside its box, every other step is a viewport position in CSS pixels"
        )]
        steps: Vec<String>,
        #[param("Button the gesture presses and releases: left or right (default left)")]
        button: Option<String>,
    },
    /// Scroll the page
    ///
    /// Turns the page's wheel as a hand does, so what a page loads or
    /// reveals on scrolling appears: infinite lists, lazy sections and
    /// virtualized tables. Answers with what changed in the outline. Say
    /// where it goes with `to` (`top` or `bottom`), how far with `by` (CSS
    /// pixels, negative to go back up), or which element to bring into
    /// view with `into_view`, which is also where a clipped list or table is
    /// scrolled. `element` names the box the wheel turns over, such as a
    /// scrollable list that is not the page itself, and the viewport's
    /// centre is used when no element is named. It works whatever holds the
    /// keyboard, so it scrolls after a click a focused control would eat
    /// `End` or `PageDown` for.
    #[effect(row, job, returns, audience = "agents", untrusted)]
    ScrollPage {
        #[param(
            "Element ref whose box the wheel turns over, such as d1e5; the viewport's centre by default"
        )]
        element: Option<String>,
        #[param("End of the page to go to: top or bottom")]
        to: Option<String>,
        #[param("Distance in CSS pixels down the page, negative to go up (for example -600)")]
        by: Option<i32>,
        #[param("Element ref to bring into view, such as d1e5")]
        into_view: Option<String>,
        #[param(
            "Frame id from the outline, such as 1 or 1.2: the action then runs in that frame's own document, with its own outline and refs"
        )]
        frame: Option<String>,
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

/// A frame id taken from the page outline, which every engine then checks
/// against the bounds it accepts. The frame is named only when the caller
/// named one: an absent frame is the page itself.
fn page_frame(frame: Option<String>) -> Option<FrameId> {
    frame
        .filter(|frame| !frame.is_empty())
        .map(|frame| FrameId::parse(&frame).unwrap_or_else(FrameId::malformed))
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
            Self::TurnOnExtension { id } => {
                BrowserOperation::Extension(ExtensionRequest::TurnOn(id))
            }
            Self::TurnOffExtension { id } => {
                BrowserOperation::Extension(ExtensionRequest::TurnOff(id))
            }
            Self::PinExtension { id } => {
                BrowserOperation::Extension(ExtensionRequest::Pin(id, true))
            }
            Self::UnpinExtension { id } => {
                BrowserOperation::Extension(ExtensionRequest::Pin(id, false))
            }
            Self::UseExtension { id } => BrowserOperation::Extension(ExtensionRequest::Use(id)),
            Self::SendToExtension { id, text } => {
                BrowserOperation::Extension(ExtensionRequest::Send(id, text))
            }
            Self::AddExtension => BrowserOperation::AddExtension,
            Self::ZoomIn => BrowserOperation::Zoom { step: 1 },
            Self::ZoomOut => BrowserOperation::Zoom { step: -1 },
            Self::ResetZoom => BrowserOperation::Zoom { step: 0 },
            Self::OpenExtensionSettings { id } => {
                BrowserOperation::Extension(ExtensionRequest::OpenSettings(id))
            }
            Self::OpenExtensionSidePanel { id } => {
                BrowserOperation::Extension(ExtensionRequest::OpenSidePanel(id))
            }
            Self::CloseExtensionSidePanel { id } => {
                BrowserOperation::Extension(ExtensionRequest::CloseSidePanel(id))
            }
            Self::RevokeExtensionAccess { id } => {
                BrowserOperation::Extension(ExtensionRequest::Revoke(id))
            }
            Self::RemoveExtension { id } => {
                BrowserOperation::Extension(ExtensionRequest::Remove(id))
            }
            Self::ShowSettings => BrowserOperation::ShowSettings,
            Self::SetLibraryFilter { query } => BrowserOperation::SetLibraryFilter { query },
            Self::ClearLibraryFilter => BrowserOperation::ClearLibraryFilter,
            Self::ForgetVisit { url } => BrowserOperation::ForgetVisit { url },
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
            Self::ReadDownload { id } => BrowserOperation::ReadDownload { id },
            Self::SaveDownload { id } => BrowserOperation::AnswerDownload { id, allow: true },
            Self::DiscardDownload { id } => BrowserOperation::AnswerDownload { id, allow: false },
            Self::AcceptPageDialog { dialog, text } => BrowserOperation::AnswerPageDialog {
                dialog,
                accept: true,
                text,
            },
            Self::DismissPageDialog { dialog } => BrowserOperation::AnswerPageDialog {
                dialog,
                accept: false,
                text: None,
            },
            Self::AnswerPermission { request, allow } => {
                BrowserOperation::AnswerPermission { request, allow }
            }
            Self::ReadPage { frame } => page(PageRequest::Outline {
                frame: page_frame(frame),
            }),
            Self::ReadPageChanges { frame } => page(PageRequest::Changes {
                frame: page_frame(frame),
            }),
            Self::ReadPageElementAt { x, y, frame } => page(PageRequest::At {
                x,
                y,
                frame: page_frame(frame),
            }),
            Self::WaitPageSettled {
                quiet_ms,
                timeout_ms,
                frame,
            } => page(PageRequest::Settle {
                quiet_ms: quiet_ms.unwrap_or(PAGE_SETTLE_QUIET_MS),
                timeout_ms: timeout_ms.unwrap_or(PAGE_SETTLE_TIMEOUT_MS),
                frame: page_frame(frame),
            }),
            Self::ReadPageConsole { since } => page(PageRequest::Console {
                since: since.unwrap_or(0),
            }),
            Self::ReadPageNetwork { since } => page(PageRequest::Network {
                since: since.unwrap_or(0),
            }),
            Self::ClickPage { element, frame } => page(PageRequest::Click {
                element,
                frame: page_frame(frame),
            }),
            Self::HoverPage { element, x, y } => {
                let element = element.filter(|element| !element.is_empty());
                let point = x.zip(y);
                page(match (element, point) {
                    (None, Some(point)) => PageRequest::Hover {
                        element: None,
                        point: Some(point),
                    },
                    (element, _) => PageRequest::Hover {
                        element,
                        point: None,
                    },
                })
            }
            Self::DragPage {
                from,
                from_x,
                from_y,
                to,
                to_x,
                to_y,
                steps,
            } => page(PageRequest::Drag {
                from: from.filter(|element| !element.is_empty()),
                from_point: from_x.zip(from_y),
                to: to.filter(|element| !element.is_empty()),
                to_point: to_x.zip(to_y),
                steps: u8::try_from(steps.unwrap_or(PAGE_DRAG_STEPS)).unwrap_or(u8::MAX),
            }),
            Self::PointerPath { element, points } => page(PageRequest::Path {
                element: element.filter(|element| !element.is_empty()),
                points: points.into_iter().map(|[x, y]| (x, y)).collect(),
            }),
            Self::FillPage {
                element,
                text,
                frame,
            } => page(PageRequest::Fill {
                element,
                text,
                secret: false,
                frame: page_frame(frame),
            }),
            Self::FillPageSecret {
                element,
                text,
                frame,
            } => page(PageRequest::Fill {
                element,
                text,
                secret: true,
                frame: page_frame(frame),
            }),
            Self::SelectPage {
                element,
                values,
                frame,
            } => page(PageRequest::Select {
                element,
                values,
                frame: page_frame(frame),
            }),
            Self::FillPageFiles {
                element,
                paths,
                frame,
            } => page(PageRequest::FillFiles {
                element,
                paths,
                frame: page_frame(frame),
            }),
            Self::PressPage {
                key,
                element,
                frame,
            } => page(PageRequest::Press {
                element: element.filter(|element| !element.is_empty()),
                key: PageKey::parse(&key).unwrap_or_else(|| PageKey {
                    key,
                    modifiers: KeyModifiers::default(),
                }),
                frame: page_frame(frame),
            }),
            Self::SubmitPage {
                element,
                cross_origin,
                frame,
            } => page(PageRequest::Submit {
                element,
                cross_origin: cross_origin.unwrap_or(false),
                frame: page_frame(frame),
            }),
            Self::PointerPage { steps, button } => page(PageRequest::Pointer {
                steps: steps.iter().filter_map(|step| pointer_step(step)).collect(),
                button: PointerButton::parse(button.as_deref().unwrap_or("left")),
            }),
            Self::ScrollPage {
                element,
                to,
                by,
                into_view,
                frame,
            } => {
                let named = |element: Option<String>| element.filter(|value| !value.is_empty());
                page(PageRequest::Scroll {
                    element: named(element),
                    to: to.as_deref().and_then(ScrollTarget::parse),
                    by,
                    into_view: named(into_view),
                    frame: page_frame(frame),
                })
            }
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

/// What the person asks of an installed extension, by the id Chromium
/// gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtensionRequest {
    TurnOn(String),
    TurnOff(String),
    /// Pin its button to the toolbar (`true`) or take it off.
    Pin(String, bool),
    OpenSettings(String),
    /// What its toolbar button does: open its popup, or tell it of the click.
    Use(String),
    /// Words for its keyword in the address, as Enter sends them.
    Send(String, String),
    /// Show its side panel beside the page, or close it.
    OpenSidePanel(String),
    CloseSidePanel(String),
    /// Take back the access the person allowed it after it was added.
    Revoke(String),
    Remove(String),
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
    /// Forget one visited page (`forget_visit`).
    ForgetVisit {
        url: String,
    },
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
    /// Answer the page's offer to download `id`: save it when `allow`.
    AnswerDownload {
        id: String,
        allow: bool,
    },
    /// Read a completed download's file, bounded and as text.
    ReadDownload {
        id: String,
    },
    Page(PageRequest),
    /// Answer the page's question `dialog`: OK, Leave or Reload when
    /// `accept`, else Cancel, with a prompt's `text`.
    AnswerPageDialog {
        dialog: u32,
        accept: bool,
        text: Option<String>,
    },
    /// Answer the site's permission request `request`: allow it when
    /// `allow`, else deny it.
    AnswerPermission {
        request: u32,
        allow: bool,
    },
    Extension(ExtensionRequest),
    /// Offer the Web Store item on the page, as Add to Tilo Browser does.
    AddExtension,
    /// Zoom the page a `step` in (`1`) or out (`-1`), or back to its own
    /// size (`0`).
    Zoom {
        step: i32,
    },
}
