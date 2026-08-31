//! Applications provider: apps you can open, launchable from the palette.
//!
//! Typing an app's name into Ctrl+K offers `Open <App>`, launched through
//! [`crate::shell::launcher`]. This provider also offers apps the configured
//! registries describe but do not have installed (see
//! [`crate::shell::catalogue`]), with the same wording, subtitle, and
//! activation as an installed app — nothing distinguishes them, because
//! nothing different is happening from the user's side. A fetch-requiring
//! open raises a standing "loading" notice rather than naming the fetch —
//! the user asked to open an app, not to acquire one.

use std::rc::Rc;

use crate::shell::catalogue::{self, CatalogueApp};
use crate::shell::chrome_state::{Chrome, OpenTarget};
use crate::shell::launcher::{self, InstalledApp};
use crate::shell::notification::{Notification, NotificationSeverity};
use crate::shell::search::action_api::{ActionArgs, ActionError};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

/// Chip label / group header for launchable apps; also the provider's `name()`.
pub const APPLICATIONS_KIND: &str = "Applications";

/// Penalty for an app the machine does not have yet, against one it does —
/// enough to break a tie without dragging a better match below a worse one
/// (every band in [`match_score`] is at least five points apart).
const NOT_INSTALLED_TIEBREAK: i32 = 1;

pub struct ApplicationsProvider {
    kinds: Vec<ResourceKind>,
}

impl ApplicationsProvider {
    pub fn new() -> Self {
        Self {
            kinds: vec![ResourceKind::Application],
        }
    }

    /// Split out so tests can exercise scoring against a known list.
    fn apps() -> Vec<InstalledApp> {
        launcher::gui_apps()
    }

    /// Score `apps` against `needle`. `open_target` only words the row —
    /// the launch reads the live target again at activation time, since it
    /// can change between the query that built this row and the Enter that
    /// fires it.
    pub(crate) fn results_for(
        &self,
        needle: &str,
        apps: Vec<InstalledApp>,
        open_target: OpenTarget,
    ) -> Vec<SearchResult> {
        let needle_lower = needle.to_lowercase();
        let kind = ResourceKind::Application;
        let mut out = Vec::new();
        for app in apps {
            let name_lower = app.display_name.to_lowercase();
            let id_lower = app.package_id.to_lowercase();
            let category_lower = app.category.as_deref().map(str::to_lowercase);
            let Some(score) = match_score(
                &needle_lower,
                &name_lower,
                &id_lower,
                category_lower.as_deref(),
            ) else {
                continue;
            };
            let package_id = app.package_id.clone();
            let display_name = app.display_name.clone();
            // A declared category is a more useful second line than the version.
            let detail = match app.category.as_deref() {
                Some(cat) => format!("{cat} · {}", app.package_id),
                None => format!("{} · {}", app.package_id, app.version),
            };
            // Where the window will land leads the line, since it's the
            // part of a launch row that changes with context.
            let subtitle = format!("{} · {detail}", open_target.result_hint());
            out.push(SearchResult::nullary_fallible(
                kind.clone(),
                self.id(),
                app.package_id.clone(),
                format!("Open {}", app.display_name),
                Some(subtitle),
                score,
                move |_doc, _root, chrome: &Chrome| {
                    launch_installed_app(chrome, &package_id, &display_name)
                },
            ));
        }
        out
    }

    fn catalogue_apps() -> Vec<CatalogueApp> {
        catalogue::available_apps()
    }

    /// Score not-yet-installed apps and build their rows. Deliberately
    /// mirrors [`Self::results_for`] field-for-field, so a row can't reveal
    /// which list it came from. `installed` keeps an app from being offered
    /// twice between an install finishing and the catalogue refetching.
    pub(crate) fn catalogue_results_for(
        &self,
        needle: &str,
        apps: Vec<CatalogueApp>,
        installed: &[String],
        open_target: OpenTarget,
    ) -> Vec<SearchResult> {
        let needle_lower = needle.to_lowercase();
        let mut out = Vec::new();
        for app in apps {
            if installed.contains(&app.package_id) {
                continue;
            }
            let name_lower = app.display_name.to_lowercase();
            let id_lower = app.package_id.to_lowercase();
            let category_lower = app.category.as_deref().map(str::to_lowercase);
            let Some(score) = match_score(
                &needle_lower,
                &name_lower,
                &id_lower,
                category_lower.as_deref(),
            ) else {
                continue;
            };
            let package_id = app.package_id.clone();
            let display_name = app.display_name.clone();
            // Identical to the installed subtitle, field for field.
            let detail = match app.category.as_deref() {
                Some(cat) => format!("{cat} · {}", app.package_id),
                None => format!("{} · {}", app.package_id, app.version),
            };
            let subtitle = format!("{} · {detail}", open_target.result_hint());
            out.push(SearchResult {
                kind: ResourceKind::Application,
                provider_id: self.id().to_string(),
                id: app.package_id.clone(),
                title: format!("Open {}", app.display_name),
                subtitle: Some(subtitle),
                score: score.saturating_sub(NOT_INSTALLED_TIEBREAK),
                on_open: Rc::new(move |_doc, _root, chrome: &Chrome, _args: &ActionArgs| {
                    // Captured now, not re-read at launch: the window may be
                    // a minute away and the user's selection can change.
                    let context = {
                        let st = chrome.borrow();
                        let tree = st.tree();
                        crate::shell::placement::note_placement_for_launch_targeted(
                            &package_id,
                            &tree,
                            st.strip_viewport(),
                            st.open_target,
                        );
                        tree.active_workspace()
                            .map(|ws| (ws.room, ws.capabilities.clone()))
                    };
                    let Some((room, capabilities)) = context else {
                        // No active workspace is no room to launch into —
                        // reported exactly as an installed app's launch would.
                        let mut st = chrome.borrow_mut();
                        st.push_notification_full(
                            Notification::new(
                                format!("Couldn't open {display_name}"),
                                launcher::LaunchError::NoWorkspace.user_message(),
                            )
                            .with_severity(NotificationSeverity::Error)
                            .with_app_name("Applications"),
                        );
                        st.bump();
                        return Ok(());
                    };
                    // The palette closes the instant this returns, so this
                    // notice is what stands between the user and an
                    // apparently dead Enter key while the fetch runs.
                    let notice = {
                        let mut st = chrome.borrow_mut();
                        let id = st.push_notification_full(
                            Notification::new(
                                format!("Opening {display_name}"),
                                "Loading…".to_string(),
                            )
                            .with_app_name("Applications")
                            .with_dismiss_after(std::time::Duration::from_secs(120)),
                        );
                        st.bump();
                        id
                    };
                    catalogue::note_pending_notice(&package_id, notice);
                    catalogue::install_and_launch(
                        package_id.clone(),
                        display_name.clone(),
                        room,
                        capabilities,
                    );
                    Ok(())
                }),
                params: Vec::new(),
            });
        }
        out
    }
}

/// The package id of the file manager qOS ships as a system app.
pub(crate) const SYSTEM_FILE_MANAGER: &str = "dev.qos.file-explorer";

/// Give the system file manager the user's home before it starts. Every app
/// otherwise gets `HOME` pointed at its own directory, wrong for one whose
/// subject is the user's files. Not a hole in the model: the grant goes
/// through the same [`GrantStoreHandle`](crate::portal::GrantStoreHandle) the
/// permission prompt writes to — visible and revocable, scoped to the
/// session, just decided by the OS instead of a dialog. Names one package id
/// rather than a category rule a third party could satisfy by declaring the
/// right thing about itself.
fn grant_user_home_to_file_manager(chrome: &Chrome, package_id: &str) {
    let ws_id = {
        let st = chrome.borrow();
        if st.tree().active_workspace().is_none() {
            return;
        }
        st.tree().active
    };
    let granted = {
        let st = chrome.borrow_mut();
        let mut tree = st.tree_mut();
        grant_file_manager_home(&mut tree, ws_id, package_id)
    };
    if granted {
        chrome.borrow_mut().bump();
    }
}

/// [`grant_user_home_to_file_manager`] against a tree, for launches that
/// don't come from the palette (also reached from
/// [`crate::shell::peer_windows`]). Returns whether a grant was added, so a
/// caller with a repaint to do can skip it when nothing changed.
///
/// Every member mints their own rather than receiving a copy, since the path
/// names *this* machine's home and is meaningless on anybody else's. Doesn't
/// mirror into the room document the way `GrantStoreHandle::insert` does,
/// but the workspace projection writes `ws.capabilities` wholesale
/// (`state.rs`), so the grant still reaches every peer on the next persist.
pub(crate) fn grant_file_manager_home(
    tree: &mut crate::shell::state::WorkspaceTree,
    workspace: crate::shell::state::WorkspaceId,
    package_id: &str,
) -> bool {
    use crate::shell::state::{Capability, FsMode, GRANT_SESSION, ResourceRef, Verb};

    if package_id != SYSTEM_FILE_MANAGER {
        return false;
    }
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let Some(ws) = tree.workspaces.iter_mut().find(|w| w.id == workspace) else {
        return false;
    };
    // Asks the capability model rather than re-deriving prefix logic.
    let actor = ws.actor_for_app(package_id);
    if ws.allows(
        &actor,
        &ResourceRef::FsPath(home.to_string_lossy().into_owned()),
        Verb::Read,
    ) {
        return false;
    }
    ws.grant_app(
        package_id,
        &Capability::Filesystem {
            path: home,
            mode: FsMode::ReadWrite,
            scope: GRANT_SESSION,
        },
    );
    true
}

/// Launch an installed package into the active workspace, reporting a
/// pre-fork failure as a notification. Shared by the Applications row and
/// the catalog's post-install launch, so "the app opens" means the same
/// placement, grants, and room everywhere; the workspace id becomes the
/// collaboration room it joins. Grants are read fresh, never cached, so a
/// permission revoked a moment ago is honoured here.
pub(crate) fn launch_installed_app(
    chrome: &Chrome,
    package_id: &str,
    display_name: &str,
) -> Result<(), ActionError> {
    grant_user_home_to_file_manager(chrome, package_id);
    // If the room already has this app open, focus it rather than starting
    // a second copy. See `peer_windows::opened_for_the_room`.
    let already = {
        let st = chrome.borrow();
        let tree = st.tree();
        let salt = tree.local_salt();
        tree.active_workspace().and_then(|ws| {
            crate::shell::peer_windows::opened_for_the_room(ws, package_id, salt)
                .map(|(strip, group, tab)| (ws.id, strip, group, tab))
        })
    };
    if let Some((ws_id, strip, group, tab)) = already {
        chrome.borrow_mut().focus_tab(ws_id, strip, group, tab);
        return Ok(());
    }
    // Say the app is opening before the work starts, so the
    // acknowledgement lands on the same frame as the keystroke. Retired by
    // the window appearing (`retire_notices_for_opened_windows`), not a timer.
    let notice = {
        let mut st = chrome.borrow_mut();
        let id = st.push_notification_full(
            Notification::new(format!("Opening {display_name}"), "Loading…".to_string())
                .with_app_name("Applications")
                .with_dismiss_after(std::time::Duration::from_secs(120)),
        );
        st.bump();
        id
    };
    catalogue::note_pending_notice(package_id, notice);
    // Scoped so the borrow is released before the error branch below
    // takes a mutable one.
    let launched = {
        let st = chrome.borrow();
        let tree = st.tree();
        // Decided here, while selection and layout are still true — the
        // window itself maps seconds later, on another process.
        crate::shell::placement::note_placement_for_launch_targeted(
            package_id,
            &tree,
            st.strip_viewport(),
            st.open_target,
        );
        tree.active_workspace().map(|ws| {
            launcher::set_workspace_capabilities(ws.capabilities.clone());
            launcher::launch_in_workspace(package_id, ws)
        })
    };
    // Non-blocking past resolution — only pre-fork failures land here, the
    // rest arrive as launch events drained each frame.
    let outcome = launched.unwrap_or(Err(launcher::LaunchError::NoWorkspace));
    if let Err(err) = outcome {
        let message = err.user_message();
        {
            let mut st = chrome.borrow_mut();
            // Claimed back rather than left to expire, or the next window
            // to paint would retire somebody else's notice.
            if let Some(id) = catalogue::take_pending_notice(package_id) {
                st.dismiss_notification(id);
            }
            st.push_notification_full(
                Notification::new(format!("Couldn't open {display_name}"), message.clone())
                    .with_severity(NotificationSeverity::Error)
                    .with_app_name("Applications"),
            );
            st.bump();
        }
        // How a caller that cannot see the screen learns the launch failed.
        return Err(ActionError::Refused(format!(
            "Couldn't open {display_name}: {message}"
        )));
    }
    Ok(())
}

impl Default for ApplicationsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchProvider for ApplicationsProvider {
    fn id(&self) -> &'static str {
        "applications"
    }
    fn name(&self) -> &str {
        APPLICATIONS_KIND
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &self.kinds
    }
    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx
            .wanted
            .iter()
            .any(|k| matches!(k, ResourceKind::Application))
        {
            return Vec::new();
        }
        let installed = Self::apps();
        let installed_ids: Vec<String> = installed.iter().map(|a| a.package_id.clone()).collect();
        let mut out = self.results_for(ctx.needle, installed, ctx.chrome.open_target);
        out.extend(self.catalogue_results_for(
            ctx.needle,
            Self::catalogue_apps(),
            &installed_ids,
            ctx.chrome.open_target,
        ));
        out
    }
}

/// Case-insensitive, first matching band wins in the order written below:
/// name prefix 100, id in full or last segment 100, name substring 50, id
/// prefix 60, id substring 25, category substring 20, fuzzy subsequence on
/// the name 10. Empty needle matches everything at 0, so an empty palette
/// lists installed apps. Category ranks below the id because it's a bucket,
/// not an identity. Package ids score by how completely they were named,
/// not flat "contains" — `hex-editor` typed for `dev.qos.hex-editor` is the
/// app's identity in full and must outrank an incidental substring.
pub(crate) fn match_score(
    needle: &str,
    name: &str,
    package_id: &str,
    category: Option<&str>,
) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    if name.starts_with(needle) {
        return Some(100);
    }
    // The id in full, or its last segment — both identify exactly one app.
    if package_id == needle || package_id.rsplit('.').next() == Some(needle) {
        return Some(100);
    }
    if name.contains(needle) {
        return Some(50);
    }
    if package_id.starts_with(needle) {
        return Some(60);
    }
    if package_id.contains(needle) {
        return Some(25);
    }
    if category.is_some_and(|c| c.contains(needle)) {
        return Some(20);
    }
    if fuzzy_subsequence(needle, name) {
        return Some(10);
    }
    None
}

fn fuzzy_subsequence(needle: &str, hay: &str) -> bool {
    let mut hi = hay.chars();
    'ch: for nc in needle.chars() {
        for hc in hi.by_ref() {
            if hc == nc {
                continue 'ch;
            }
        }
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};
    use automerge::AutoCommit;
    use parking_lot::RwLock;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn chrome_rc() -> Chrome {
        let handle: WorkspaceTreeHandle = Arc::new(RwLock::new(WorkspaceTree::default()));
        ChromeState::new_with_handle(handle)
    }

    fn app(package_id: &str, display_name: &str) -> InstalledApp {
        app_in(package_id, display_name, None)
    }

    fn offered(package_id: &str, display_name: &str, category: Option<&str>) -> CatalogueApp {
        CatalogueApp {
            package_id: package_id.to_string(),
            display_name: display_name.to_string(),
            description: "does a thing".to_string(),
            category: category.map(str::to_string),
            version: "0.1.0".to_string(),
            registry: "qOS main".to_string(),
            trust: "verified".to_string(),
        }
    }

    fn app_in(package_id: &str, display_name: &str, category: Option<&str>) -> InstalledApp {
        InstalledApp {
            package_id: package_id.to_string(),
            display_name: display_name.to_string(),
            category: category.map(str::to_string),
            version: "0.1.0".to_string(),
            manifest_path: PathBuf::from("/apps/x/manifest.toml"),
            payload_root: PathBuf::from("/apps/x"),
            has_gui: true,
            opens_extensions: Vec::new(),
            opens_mime_types: Vec::new(),
            opens_uri_schemes: Vec::new(),
        }
    }

    #[test]
    fn matching_app_offers_an_open_row() {
        let p = ApplicationsProvider::new();
        let out = p.results_for(
            "hel",
            vec![app("dev.qos.hello", "Hello")],
            OpenTarget::Beside,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Open Hello");
        assert_eq!(out[0].id, "dev.qos.hello");
        assert_eq!(out[0].score, 100, "prefix match on the display name");
        assert_eq!(
            out[0].subtitle.as_deref(),
            Some("Opens beside · dev.qos.hello · 0.1.0"),
            "the row names the package it will launch"
        );
    }

    #[test]
    fn declared_category_is_matchable_and_replaces_the_version_subtitle() {
        let p = ApplicationsProvider::new();
        let out = p.results_for(
            "office",
            vec![
                app_in("dev.qos.sheets", "Sheets", Some("Office")),
                app_in("dev.qos.calculator", "Calculator", Some("Utility")),
            ],
            OpenTarget::Beside,
        );
        assert_eq!(out.len(), 1, "only the Office app matched: {out:?}");
        assert_eq!(out[0].id, "dev.qos.sheets");
        assert_eq!(out[0].score, 20, "category ranks below name and id matches");
        assert_eq!(
            out[0].subtitle.as_deref(),
            Some("Opens beside · Office · dev.qos.sheets"),
            "a declared category is the more useful second line than the version"
        );
    }

    #[test]
    fn category_never_outranks_a_name_or_id_match() {
        let p = ApplicationsProvider::new();
        // "system" is Monitor's category AND a substring of the other
        // app's display name. The name match has to win.
        let out = p.results_for(
            "system",
            vec![
                app_in("dev.qos.monitor", "Monitor", Some("System")),
                app_in("dev.qos.sysinfo", "System Info", Some("Utility")),
            ],
            OpenTarget::Beside,
        );
        assert_eq!(out.len(), 2);
        let score = |id: &str| out.iter().find(|r| r.id == id).unwrap().score;
        assert_eq!(score("dev.qos.sysinfo"), 100, "prefix on the display name");
        assert_eq!(score("dev.qos.monitor"), 20, "category-only match");
        assert!(score("dev.qos.sysinfo") > score("dev.qos.monitor"));
    }

    #[test]
    fn package_id_matches_score_below_name_matches() {
        let p = ApplicationsProvider::new();
        let out = p.results_for(
            "dev.qos",
            vec![app("dev.qos.hello", "Hello"), app("org.x.zed", "Zed")],
            OpenTarget::Beside,
        );
        assert_eq!(out.len(), 1, "only the matching package: {out:?}");
        // A shared id *prefix* names a publisher, not an app, so it
        // stays below a name match.
        assert_eq!(out[0].score, 60);
        let by_name = p.results_for(
            "hello",
            vec![app("dev.qos.hello", "Hello")],
            OpenTarget::Beside,
        );
        assert!(
            by_name[0].score > out[0].score,
            "naming the app must still beat naming its publisher"
        );
    }

    /// The display name is "Hex Editor", which never prefix-matches the id.
    #[test]
    fn the_id_last_segment_is_an_identity_match() {
        let p = ApplicationsProvider::new();
        let out = p.results_for(
            "hex-editor",
            vec![app("dev.qos.hex-editor", "Hex Editor")],
            OpenTarget::Beside,
        );
        assert_eq!(out.len(), 1, "the app must match at all: {out:?}");
        assert_eq!(
            out[0].score, 100,
            "naming an app's id segment identifies it as exactly as \
             naming the app does"
        );
        let full = p.results_for(
            "dev.qos.hex-editor",
            vec![app("dev.qos.hex-editor", "Hex Editor")],
            OpenTarget::Beside,
        );
        assert_eq!(full[0].score, 100);
    }

    #[test]
    fn non_matching_needle_returns_nothing() {
        let p = ApplicationsProvider::new();
        let out = p.results_for(
            "zzzqqq",
            vec![app("dev.qos.hello", "Hello")],
            OpenTarget::Beside,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn empty_needle_lists_every_app() {
        let p = ApplicationsProvider::new();
        let out = p.results_for(
            "",
            vec![app("dev.qos.hello", "Hello"), app("org.x.zed", "Zed")],
            OpenTarget::Beside,
        );
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|r| r.score == 0));
    }

    #[test]
    fn unrequested_kind_returns_empty() {
        let p = ApplicationsProvider::new();
        let c = chrome_rc();
        let cs = c.borrow();
        let doc = AutoCommit::new();
        let out = p.query(SearchContext {
            needle: "hello",
            wanted: &[ResourceKind::File],
            chrome: &cs,
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        assert!(out.is_empty());
    }

    /// A launch that cannot start must reach both audiences: a notification
    /// for the screen, and a refusal for the caller that invoked the row by
    /// address — without it, a script is told the app opened and waits for
    /// a window that never arrives. Drives the real `on_open` closure.
    #[test]
    fn a_failed_launch_notifies_the_user_and_refuses_the_caller() {
        use crate::shell::search::action_api::ActionError;

        let p = ApplicationsProvider::new();
        let c = chrome_rc();
        let mut doc = AutoCommit::new();
        // The registry does not actually contain this app, so launch refuses it.
        let out = p.results_for(
            "ghost",
            vec![app("dev.qos.ghost", "Ghost")],
            OpenTarget::Beside,
        );
        let row = out.first().expect("a row for the ghost app");
        assert!(c.borrow().notifications().is_empty());
        let outcome = (row.on_open)(
            &mut doc,
            &automerge::ROOT,
            &c,
            &crate::shell::search::action_api::ActionArgs::none(),
        );

        let err = outcome.expect_err("a launch that cannot start must not report success");
        assert!(
            matches!(err, ActionError::Refused(_)),
            "a launch failure is a refusal, not an argument error: {err:?}"
        );
        assert_eq!(
            err.code(),
            "action_failed",
            "the wire code must say the action ran and could not do its job"
        );
        assert!(
            err.to_string().contains("Ghost"),
            "the refusal must name what failed to open: {err}"
        );

        let st = c.borrow();
        let n = st
            .notifications()
            .last()
            .expect("a failed launch must notify the user");
        assert_eq!(n.title, "Couldn't open Ghost");
        assert_eq!(n.severity, NotificationSeverity::Error);
        assert!(
            !n.body.is_empty(),
            "the notification must say why the launch failed"
        );
    }

    // Apps not installed here yet: a fetched app's row must be
    // indistinguishable from an installed one — asserted on exact strings.

    #[test]
    fn an_uninstalled_app_offers_an_ordinary_open_row() {
        let p = ApplicationsProvider::new();
        let out = p.catalogue_results_for(
            "she",
            vec![offered("dev.qos.sheets", "Sheets", Some("Office"))],
            &[],
            OpenTarget::Beside,
        );
        assert_eq!(out.len(), 1, "the offered app must match: {out:?}");
        assert_eq!(
            out[0].title, "Open Sheets",
            "an app the machine has to fetch is still just opened"
        );
        assert_eq!(out[0].id, "dev.qos.sheets");
        assert_eq!(out[0].kind, ResourceKind::Application);
        assert_eq!(
            out[0].provider_id, "applications",
            "one provider means one group header — a second would be a \
             visible 'other kind of app' section"
        );
    }

    /// Nothing in the user-visible strings may hint at installing.
    #[test]
    fn nothing_in_an_uninstalled_row_says_install() {
        let p = ApplicationsProvider::new();
        let out = p.catalogue_results_for(
            "",
            vec![offered("dev.qos.sheets", "Sheets", Some("Office"))],
            &[],
            OpenTarget::Beside,
        );
        let row = &out[0];
        let visible = format!("{} {}", row.title, row.subtitle.clone().unwrap_or_default());
        let lowered = visible.to_lowercase();
        for word in ["install", "download", "fetch", "available", "get "] {
            assert!(
                !lowered.contains(word),
                "{word:?} must not appear in a row the user reads as opening \
                 an app: {visible:?}"
            );
        }
    }

    /// Byte-for-byte the same subtitle an installed app with the same
    /// metadata would produce.
    #[test]
    fn an_uninstalled_row_has_the_same_subtitle_as_an_installed_one() {
        let p = ApplicationsProvider::new();
        let installed = p.results_for(
            "",
            vec![app_in("dev.qos.sheets", "Sheets", Some("Office"))],
            OpenTarget::Beside,
        );
        let available = p.catalogue_results_for(
            "",
            vec![offered("dev.qos.sheets", "Sheets", Some("Office"))],
            &[],
            OpenTarget::Beside,
        );
        assert_eq!(installed[0].title, available[0].title);
        assert_eq!(
            installed[0].subtitle, available[0].subtitle,
            "the two rows have to be indistinguishable"
        );

        // With no category declared, the subtitle falls back to id + version.
        let installed = p.results_for("", vec![app("dev.qos.plain", "Plain")], OpenTarget::Beside);
        let available = p.catalogue_results_for(
            "",
            vec![offered("dev.qos.plain", "Plain", None)],
            &[],
            OpenTarget::Beside,
        );
        assert_eq!(installed[0].subtitle, available[0].subtitle);
    }

    #[test]
    fn an_app_you_have_wins_a_tie_but_not_a_better_match() {
        let p = ApplicationsProvider::new();
        let mine = p.results_for(
            "notes",
            vec![app("dev.qos.notes", "Notes")],
            OpenTarget::Beside,
        );
        let theirs = p.catalogue_results_for(
            "notes",
            vec![offered("org.other.notes", "Notes", None)],
            &[],
            OpenTarget::Beside,
        );
        assert!(
            mine[0].score > theirs[0].score,
            "at equal match quality the installed app ranks first: \
             {} vs {}",
            mine[0].score,
            theirs[0].score
        );

        // A weaker match on an installed app must NOT beat the app the
        // user actually named.
        let vague = p.results_for(
            "sheets",
            vec![app("dev.qos.shed", "Shed Helper Editor Extra Tool Set")],
            OpenTarget::Beside,
        );
        let named = p.catalogue_results_for(
            "sheets",
            vec![offered("dev.qos.sheets", "Sheets", None)],
            &[],
            OpenTarget::Beside,
        );
        assert!(
            !vague.is_empty() && !named.is_empty(),
            "both sides must actually match for this to prove anything"
        );
        assert!(
            named[0].score > vague[0].score,
            "naming an app you don't have beats a vague hit on one you do: \
             {} vs {}",
            named[0].score,
            vague[0].score
        );
    }

    /// The same app can be in both lists briefly; must produce one row, not two.
    #[test]
    fn an_app_that_just_became_installed_is_not_offered_twice() {
        let p = ApplicationsProvider::new();
        let out = p.catalogue_results_for(
            "sheets",
            vec![offered("dev.qos.sheets", "Sheets", None)],
            &["dev.qos.sheets".to_string()],
            OpenTarget::Beside,
        );
        assert!(
            out.is_empty(),
            "an installed app must not also be offered as available: {out:?}"
        );
    }

    #[test]
    fn a_non_matching_needle_offers_nothing_from_the_catalogue() {
        let p = ApplicationsProvider::new();
        let out = p.catalogue_results_for(
            "zzzqqq",
            vec![offered("dev.qos.sheets", "Sheets", None)],
            &[],
            OpenTarget::Beside,
        );
        assert!(out.is_empty());
    }

    /// Must say so in the same words an installed app's launch uses, not
    /// fail silently. Drives the real `on_open` closure.
    #[test]
    fn activating_with_no_workspace_reports_it_the_same_way() {
        let p = ApplicationsProvider::new();
        let c = chrome_rc();
        let mut doc = AutoCommit::new();
        let out = p.catalogue_results_for(
            "sheets",
            vec![offered("dev.qos.sheets", "Sheets", None)],
            &[],
            OpenTarget::Beside,
        );
        let row = out.first().expect("a row for the offered app");
        assert!(c.borrow().notifications().is_empty());
        let _ = (row.on_open)(&mut doc, &automerge::ROOT, &c, &ActionArgs::none());
        let st = c.borrow();
        let n = st
            .notifications()
            .last()
            .expect("activation must never be silent");
        assert_eq!(n.title, "Couldn't open Sheets");
        assert_eq!(n.severity, NotificationSeverity::Error);
        assert_eq!(
            n.body,
            launcher::LaunchError::NoWorkspace.user_message(),
            "the same failure gets the same words whichever list the app \
             came from"
        );
    }

    /// Must put something on screen on that frame, before the work starts —
    /// the wait is seconds, and silence reads as a dead keystroke.
    #[test]
    fn activating_an_app_that_has_to_load_says_so_immediately() {
        let p = ApplicationsProvider::new();
        let handle: WorkspaceTreeHandle = Arc::new(RwLock::new(WorkspaceTree::bootstrap_demo()));
        let c = ChromeState::new_with_handle(handle);
        let mut doc = AutoCommit::new();
        let out = p.catalogue_results_for(
            "sheets",
            vec![offered("dev.qos.sheets", "Sheets", None)],
            &[],
            OpenTarget::Beside,
        );
        let row = out.first().expect("a row for the offered app");
        assert!(c.borrow().notifications().is_empty());

        let _ = (row.on_open)(&mut doc, &automerge::ROOT, &c, &ActionArgs::none());

        let st = c.borrow();
        let n = st
            .notifications()
            .first()
            .expect("activation must never be silent");
        assert_eq!(n.title, "Opening Sheets");
        assert!(
            n.dismiss_after >= std::time::Duration::from_secs(30),
            "the indication has to outlast the wait it is covering, not \
             flash and leave the user watching nothing"
        );

        // Must not name the mechanism — the user asked to open an app, not
        // to acquire one.
        let visible = format!("{} {}", n.title, n.body).to_lowercase();
        for word in ["install", "download", "fetch", "registry"] {
            assert!(
                !visible.contains(word),
                "{word:?} must not appear in what the user reads while \
                 waiting for an app to open: {:?} / {:?}",
                n.title,
                n.body
            );
        }
        assert!(
            n.body.to_lowercase().contains("load"),
            "the one thing to say is that it is loading: {:?}",
            n.body
        );
    }

    /// A state that flashes for a hundred milliseconds is noise, not feedback.
    #[test]
    fn an_app_that_is_already_here_does_not_raise_a_loading_state() {
        let p = ApplicationsProvider::new();
        let out = p.results_for(
            "she",
            vec![app("dev.qos.sheets", "Sheets")],
            OpenTarget::Beside,
        );
        let row = out.first().expect("a row for the installed app");
        assert!(
            !row.title.to_lowercase().contains("load"),
            "an app that is here is just opened: {:?}",
            row.title
        );
    }
}
