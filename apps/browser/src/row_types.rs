//! The types the window's rows are items of: a row says what it is — its
//! type's noun and the app's name — so search and an agent name it as they
//! name a resource. The kinds are the ones the window's rows always had.

use ui::tilo_resource;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.page", label = "Page")]
pub(crate) struct Page;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.history-pause", label = "Session")]
pub(crate) struct Session;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.history", label = "History")]
pub(crate) struct History;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.history-entry", label = "Visit")]
pub(crate) struct Visit;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.bookmarks", label = "Bookmarks")]
pub(crate) struct Bookmarks;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.bookmark", label = "Bookmark")]
pub(crate) struct Bookmark;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.downloads", label = "Downloads")]
pub(crate) struct Downloads;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.download", label = "Download")]
pub(crate) struct Download;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.extensions", label = "Extensions")]
pub(crate) struct Extensions;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.extension", label = "Extension")]
pub(crate) struct Extension;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.library", label = "Library")]
pub(crate) struct Library;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.settings", label = "Settings")]
pub(crate) struct Settings;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.service-sessions", label = "Account sessions")]
pub(crate) struct ServiceSessions;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.service-status", label = "Account")]
pub(crate) struct ServiceStatus;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.continuity", label = "Continuity")]
pub(crate) struct Continuity;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.personal-bookmark", label = "Bookmark")]
pub(crate) struct PersonalBookmark;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.personal-bookmark-folder", label = "Folder")]
pub(crate) struct PersonalBookmarkFolder;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.personal-bookmark-placement", label = "Bookmark")]
pub(crate) struct PersonalBookmarkPlacement;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.personal-bookmark-separator", label = "Separator")]
pub(crate) struct PersonalBookmarkSeparator;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.personal-bookmark-navigation", label = "Place")]
pub(crate) struct PersonalBookmarkNavigation;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.page-permission", label = "Permission request")]
pub(crate) struct PagePermission;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.page-dialog", label = "Dialog")]
pub(crate) struct PageDialog;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.page-sign-in", label = "Sign-in")]
pub(crate) struct PageSignIn;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.service", label = "Account")]
pub(crate) struct Service;
