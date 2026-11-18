//! The types the window's rows are items of: a row says what it is — its
//! type's noun and the app's name — so search and an agent name it as they
//! name a resource. The kinds are the ones the window's rows always had.

use ui::tilo_resource::words::RowType;

pub(crate) struct Page;

impl RowType for Page {
    const KIND: &'static str = "browser.page";
    const LABEL: &'static str = "Page";
}

pub(crate) struct Session;

impl RowType for Session {
    const KIND: &'static str = "browser.history-pause";
    const LABEL: &'static str = "Session";
}

pub(crate) struct History;

impl RowType for History {
    const KIND: &'static str = "browser.history";
    const LABEL: &'static str = "History";
}

pub(crate) struct Visit;

impl RowType for Visit {
    const KIND: &'static str = "browser.history-entry";
    const LABEL: &'static str = "Visit";
}

pub(crate) struct Bookmarks;

impl RowType for Bookmarks {
    const KIND: &'static str = "browser.bookmarks";
    const LABEL: &'static str = "Bookmarks";
}

pub(crate) struct Bookmark;

impl RowType for Bookmark {
    const KIND: &'static str = "browser.bookmark";
    const LABEL: &'static str = "Bookmark";
}

pub(crate) struct Downloads;

impl RowType for Downloads {
    const KIND: &'static str = "browser.downloads";
    const LABEL: &'static str = "Downloads";
}

pub(crate) struct Download;

impl RowType for Download {
    const KIND: &'static str = "browser.download";
    const LABEL: &'static str = "Download";
}

pub(crate) struct Extensions;

impl RowType for Extensions {
    const KIND: &'static str = "browser.extensions";
    const LABEL: &'static str = "Extensions";
}

pub(crate) struct Extension;

impl RowType for Extension {
    const KIND: &'static str = "browser.extension";
    const LABEL: &'static str = "Extension";
}

pub(crate) struct Library;

impl RowType for Library {
    const KIND: &'static str = "browser.library";
    const LABEL: &'static str = "Library";
}

pub(crate) struct Settings;

impl RowType for Settings {
    const KIND: &'static str = "browser.settings";
    const LABEL: &'static str = "Settings";
}

pub(crate) struct ServiceSessions;

impl RowType for ServiceSessions {
    const KIND: &'static str = "browser.service-sessions";
    const LABEL: &'static str = "Account sessions";
}

pub(crate) struct ServiceStatus;

impl RowType for ServiceStatus {
    const KIND: &'static str = "browser.service-status";
    const LABEL: &'static str = "Account";
}

pub(crate) struct Continuity;

impl RowType for Continuity {
    const KIND: &'static str = "browser.continuity";
    const LABEL: &'static str = "Continuity";
}

pub(crate) struct PersonalBookmark;

impl RowType for PersonalBookmark {
    const KIND: &'static str = "browser.personal-bookmark";
    const LABEL: &'static str = "Bookmark";
}

pub(crate) struct PersonalBookmarkFolder;

impl RowType for PersonalBookmarkFolder {
    const KIND: &'static str = "browser.personal-bookmark-folder";
    const LABEL: &'static str = "Folder";
}

pub(crate) struct PersonalBookmarkPlacement;

impl RowType for PersonalBookmarkPlacement {
    const KIND: &'static str = "browser.personal-bookmark-placement";
    const LABEL: &'static str = "Bookmark";
}

pub(crate) struct PersonalBookmarkSeparator;

impl RowType for PersonalBookmarkSeparator {
    const KIND: &'static str = "browser.personal-bookmark-separator";
    const LABEL: &'static str = "Separator";
}

pub(crate) struct PersonalBookmarkNavigation;

impl RowType for PersonalBookmarkNavigation {
    const KIND: &'static str = "browser.personal-bookmark-navigation";
    const LABEL: &'static str = "Place";
}

pub(crate) struct PagePermission;

impl RowType for PagePermission {
    const KIND: &'static str = "browser.page-permission";
    const LABEL: &'static str = "Permission request";
}

pub(crate) struct PageDialog;

impl RowType for PageDialog {
    const KIND: &'static str = "browser.page-dialog";
    const LABEL: &'static str = "Dialog";
}

pub(crate) struct PageSignIn;

impl RowType for PageSignIn {
    const KIND: &'static str = "browser.page-sign-in";
    const LABEL: &'static str = "Sign-in";
}

pub(crate) struct Service;

impl RowType for Service {
    const KIND: &'static str = "browser.service";
    const LABEL: &'static str = "Account";
}
