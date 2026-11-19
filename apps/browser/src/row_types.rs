//! The types the window's rows are items of: a row says what it is — its
//! type's noun and the app's name — so search and an agent name it as they
//! name a resource. The kinds are the ones the window's rows always had.

use ui::tilo_resource;

use crate::effects::BrowserEffect;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.page", label = "Page", effects = BrowserEffect)]
pub(crate) struct Page;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.history-pause", label = "Session", effects = BrowserEffect)]
pub(crate) struct Session;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.history", label = "History", effects = BrowserEffect)]
pub(crate) struct History;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.history-entry", label = "Visit", effects = BrowserEffect)]
pub(crate) struct Visit;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.bookmarks", label = "Bookmarks", effects = BrowserEffect)]
pub(crate) struct Bookmarks;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.bookmark", label = "Bookmark", effects = BrowserEffect)]
pub(crate) struct Bookmark;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.downloads", label = "Downloads", effects = BrowserEffect)]
pub(crate) struct Downloads;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.download", label = "Download", effects = BrowserEffect)]
pub(crate) struct Download;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.extensions", label = "Extensions", effects = BrowserEffect)]
pub(crate) struct Extensions;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.extension", label = "Extension", effects = BrowserEffect)]
pub(crate) struct Extension;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.library", label = "Library", effects = BrowserEffect)]
pub(crate) struct Library;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.settings", label = "Settings", effects = BrowserEffect)]
pub(crate) struct Settings;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.service-sessions", label = "Account sessions", effects = BrowserEffect)]
pub(crate) struct ServiceSessions;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.service-status", label = "Account", effects = BrowserEffect)]
pub(crate) struct ServiceStatus;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.continuity", label = "Continuity", effects = BrowserEffect)]
pub(crate) struct Continuity;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.personal-bookmark", label = "Bookmark", effects = BrowserEffect)]
pub(crate) struct PersonalBookmark;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.personal-bookmark-folder", label = "Folder", effects = BrowserEffect)]
pub(crate) struct PersonalBookmarkFolder;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.personal-bookmark-placement", label = "Bookmark", effects = BrowserEffect)]
pub(crate) struct PersonalBookmarkPlacement;

#[derive(ui::tilo_resource::Row)]
#[row(
    kind = "browser.personal-bookmark-separator",
    label = "Separator",
    effects = BrowserEffect
)]
pub(crate) struct PersonalBookmarkSeparator;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.personal-bookmark-navigation", label = "Place", effects = BrowserEffect)]
pub(crate) struct PersonalBookmarkNavigation;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.page-permission", label = "Permission request", effects = BrowserEffect)]
pub(crate) struct PagePermission;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.page-dialog", label = "Dialog", effects = BrowserEffect)]
pub(crate) struct PageDialog;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.page-sign-in", label = "Sign-in", effects = BrowserEffect)]
pub(crate) struct PageSignIn;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "browser.service", label = "Account", effects = BrowserEffect)]
pub(crate) struct Service;

/// The person's own bookmark library, published beside the workspace's
/// bookmarks: importing one into it is the row's own effect.
#[derive(ui::tilo_resource::Row)]
#[row(
    kind = "browser.personal-bookmarks",
    label = "Personal bookmarks",
    effects = BrowserEffect
)]
pub(crate) struct PersonalBookmarksList;
