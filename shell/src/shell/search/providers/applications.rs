//! Applications provider: apps you can open, launchable from the palette.
//!
//! Typing an installed app's name into Ctrl+K offers `Open <App>`, launched
//! through [`crate::shell::launcher`]. Registry apps that are not installed
//! intentionally take a different, safe route: the palette opens the Store's
//! stable detail URI. The Store then shows provenance and offers the current
//! revalidated install action; palette activation never downloads software.

use std::collections::HashSet;
use std::rc::Rc;

use crate::portal::desktop_services::DynamicLauncherRecord;
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
const STORE_PACKAGE_ID: &str = "dev.qos.store";

/// Product-owned ordering for the empty Applications palette. Search queries
/// continue to rank purely by match quality; this only prevents a fresh system
/// from presenting forty alphabetised tools as though they were equally likely
/// starting points. The first group is deliberately useful to curious
/// tinkerers without assuming they are developers.
fn empty_palette_presentation(package_id: &str) -> (&'static str, i32) {
    match package_id {
        "dev.qos.browser" => ("Start here", 90),
        "dev.qos.file-explorer" => ("Start here", 89),
        "dev.qos.notes" => ("Start here", 88),
        "dev.qos.terminal" => ("Start here", 87),
        "dev.qos.settings" => ("Start here", 86),
        "dev.qos.store" => ("Start here", 85),

        "dev.qos.calendar" => ("Everyday", 74),
        "dev.qos.mail" => ("Everyday", 73),
        "dev.qos.contacts" => ("Everyday", 72),
        "dev.qos.chat" => ("Everyday", 71),
        "dev.qos.calculator" => ("Everyday", 70),

        "dev.qos.word-processor" => ("Work", 59),
        "dev.qos.sheets" => ("Work", 58),
        "dev.qos.slides" => ("Work", 57),
        "dev.qos.pdf-viewer" => ("Work", 56),
        "dev.qos.typst-editor" => ("Work", 55),

        "dev.qos.music" => ("Media", 49),
        "dev.qos.tube" => ("Media", 48),
        "dev.qos.video-player" => ("Media", 47),
        "dev.qos.photos" => ("Media", 46),
        "dev.qos.camera" => ("Media", 45),
        "dev.qos.weather" => ("Everyday", 44),
        "dev.qos.maps" => ("Everyday", 43),
        "dev.qos.feeds" => ("Everyday", 42),
        "dev.qos.ebook" => ("Media", 41),
        "dev.qos.totp" => ("Everyday", 40),

        "dev.qos.monitor" | "dev.qos.disks" | "dev.qos.logs" | "dev.qos.firewall"
        | "dev.qos.vpn" | "dev.qos.remote" | "dev.qos.backups" | "dev.qos.imager"
        | "dev.qos.themes" => ("System tools", 25),
        _ => ("More apps", 10),
    }
}

fn presentation_score(needle: &str, package_id: &str, match_score: i32) -> i32 {
    if needle.is_empty() {
        empty_palette_presentation(package_id).1
    } else {
        match_score
    }
}

fn app_detail(needle: &str, app: &InstalledApp) -> String {
    let metadata = match app.category.as_deref() {
        Some(category) => format!("qOS · {category} · {}", app.package_id),
        None => format!("qOS · {} · {}", app.package_id, app.version),
    };
    if needle.is_empty() {
        format!(
            "{} · {metadata}",
            empty_palette_presentation(&app.package_id).0
        )
    } else {
        metadata
    }
}

fn catalogue_app_detail(needle: &str, app: &CatalogueApp) -> String {
    let metadata = match app.category.as_deref() {
        Some(category) => format!("qOS · {category} · {}", app.package_id),
        None => format!("qOS · {} · {}", app.package_id, app.version),
    };
    if needle.is_empty() {
        format!(
            "{} · {metadata}",
            empty_palette_presentation(&app.package_id).0
        )
    } else {
        metadata
    }
}

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
                None,
            ) else {
                continue;
            };
            let package_id = app.package_id.clone();
            let display_name = app.display_name.clone();
            // A declared category is a more useful second line than the version.
            let detail = app_detail(needle, &app);
            // Where the window will land leads the line, since it's the
            // part of a launch row that changes with context.
            let subtitle = format!("{} · {detail}", open_target.result_hint());
            out.push(SearchResult::nullary_fallible(
                kind.clone(),
                self.id(),
                app.package_id.clone(),
                format!("Open {}", app.display_name),
                Some(subtitle),
                presentation_score(needle, &app.package_id, score),
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

    fn dynamic_apps() -> Vec<DynamicLauncherRecord> {
        crate::portal::desktop_services::dynamic_launcher_records()
    }

    /// Reviewed `DynamicLauncher` entries are catalogue aliases, never command
    /// lines. Application entries start only their authenticated owner package;
    /// web entries pass their HTTPS target through the normal URI handler and
    /// workspace policy.
    fn dynamic_results_for(
        &self,
        needle: &str,
        records: Vec<DynamicLauncherRecord>,
        open_target: OpenTarget,
    ) -> Vec<SearchResult> {
        let needle_lower = needle.to_lowercase();
        let installed_gui: Vec<String> = launcher::gui_apps()
            .into_iter()
            .map(|app| app.package_id)
            .collect();
        records
            .into_iter()
            .filter_map(|record| {
                let name_lower = record.name.to_lowercase();
                let id_lower = record.id.to_lowercase();
                let score = match_score(&needle_lower, &name_lower, &id_lower, None, None)?;
                let valid = match record.launcher_type {
                    1 => record.target.is_none() && installed_gui.contains(&record.owner_app_id),
                    2 => record.target.as_deref().is_some_and(|target| {
                        target.starts_with("https://")
                            && crate::shell::uri_open::resource_for_uri(target).is_ok()
                    }),
                    _ => false,
                };
                if !valid
                    || record.name.trim().is_empty()
                    || record.name.chars().any(char::is_control)
                {
                    return None;
                }
                let name = record.name.clone();
                let owner = record.owner_app_id.clone();
                let target = record.target.clone();
                let launcher_type = record.launcher_type;
                Some(SearchResult::nullary_fallible(
                    ResourceKind::Application,
                    self.id(),
                    record.id,
                    format!("Open {}", record.name),
                    Some(format!("{} · Shortcut", open_target.result_hint())),
                    score,
                    move |_doc, _root, chrome: &Chrome| {
                        if launcher_type == 1 {
                            return launch_installed_app(chrome, &owner, &name);
                        }
                        let Some(target) = target.as_deref() else {
                            return Err(ActionError::Refused("Shortcut target is missing".into()));
                        };
                        crate::shell::uri_open::open_in_workspace(target, chrome)
                            .map(|_| ())
                            .map_err(|error| {
                                let message = format!("Couldn't open {name}: {error:?}");
                                let mut state = chrome.borrow_mut();
                                state.push_notification_full(
                                    Notification::new(
                                        format!("Couldn't open {name}"),
                                        // The URI resolver's Debug output may
                                        // contain package ids, paths, or
                                        // multiline backend details. Keep the
                                        // full value in the refusal returned
                                        // to the caller/log and give the
                                        // fixed-height toast a bounded recovery
                                        // instruction instead.
                                        "The shortcut could not be opened. Check its target and try again.",
                                    )
                                    .with_severity(NotificationSeverity::Error)
                                    .with_app_name("Applications"),
                                );
                                state.bump();
                                ActionError::Refused(message)
                            })
                    },
                ))
            })
            .collect()
    }

    /// Score not-yet-installed apps and build Store-detail rows. `installed`
    /// keeps an app from being offered twice between an install finishing and the
    /// catalogue refetching.
    pub(crate) fn catalogue_results_for(
        &self,
        needle: &str,
        apps: Vec<CatalogueApp>,
        installed: &[String],
        _open_target: OpenTarget,
    ) -> Vec<SearchResult> {
        self.catalogue_results_for_with_hosts(needle, apps, installed, &[], open_target)
    }

    fn catalogue_results_for_with_hosts(
        &self,
        needle: &str,
        apps: Vec<CatalogueApp>,
        installed: &[String],
        host_apps: &[crate::shell::host_apps::HostApp],
        open_target: OpenTarget,
    ) -> Vec<SearchResult> {
        let needle_lower = needle.to_lowercase();
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for app in apps {
            if installed.contains(&app.package_id)
                || host_apps
                    .iter()
                    .any(|host| catalogue_aliases_host(&app, host))
                || !seen.insert(app.package_id.clone())
            {
                continue;
            }
            let name_lower = app.display_name.to_lowercase();
            let id_lower = app.package_id.to_lowercase();
            let category_lower = app.category.as_deref().map(str::to_lowercase);
            let description_lower = app.description.to_lowercase();
            let Some(score) = match_score(
                &needle_lower,
                &name_lower,
                &id_lower,
                category_lower.as_deref(),
                Some(&description_lower),
            ) else {
                continue;
            };
            let package_id = app.package_id.clone();
            let display_name = app.display_name.clone();
            let detail = catalogue_app_detail(needle, &app);
            let trust = if app.trust.trim().is_empty() {
                "Registry verification pending".to_string()
            } else {
                app.trust.clone()
            };
            let subtitle = format!("App Store · {trust} · {detail}");
            out.push(SearchResult {
                kind: ResourceKind::Application,
                provider_id: self.id().to_string(),
                id: format!("app:{}", app.package_id),
                title: format!("View {} in App Store", app.display_name),
                subtitle: Some(subtitle),
                score: presentation_score(needle, &app.package_id, score)
                    .saturating_sub(NOT_INSTALLED_TIEBREAK),
                on_open: Rc::new(move |_doc, _root, chrome: &Chrome, _args: &ActionArgs| {
                    // A palette can be opened and activated repeatedly while
                    // catalogue installation is still running. Keep one
                    // in-flight operation and one acknowledgement; replacing
                    // the pending id would strand the first card forever.
                    if catalogue::pending_notice(&package_id).is_some() {
                        return Ok(());
                    }
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
                                launcher::launch_failure_title(&display_name),
                                launcher::LaunchError::NoWorkspace.notification_message(),
                            )
                            .with_severity(NotificationSeverity::Error)
                            .with_app_name("Applications")
                            .with_dismiss_after(launcher::LAUNCH_FAILURE_NOTICE_LIFETIME),
                        );
                        st.bump();
                        // The notification is how the person at the machine
                        // learns; this is how a caller with no screen learns —
                        // the same failure, rather than success reported for an
                        // app that was never started.
                        return Err(ActionError::Refused(format!(
                            "Couldn't open App Store for {display_name}: {}",
                            launcher::LaunchError::NoWorkspace.user_message()
                        )));
                    };
                    let uri = store_detail_uri(&package_id)?;
                    launcher::launch_uri_in_room(STORE_PACKAGE_ID, room, capabilities, &uri)
                        .map(|_| ())
                        .map_err(|error| {
                            let message = error.user_message();
                            let mut state = chrome.borrow_mut();
                            state.push_notification_full(
                                Notification::new(
                                    format!("Couldn't open App Store for {display_name}"),
                                    message.clone(),
                                )
                                .with_severity(NotificationSeverity::Error)
                                .with_app_name("Applications"),
                            );
                            state.bump();
                            ActionError::Refused(format!(
                                "Couldn't open App Store for {display_name}: {message}"
                            ))
                        })
                }),
                on_invoke: None,
                on_begin_invoke: None,
                params: Vec::new(),
                contract: None,
            });
        }
        out
    }
}

fn store_detail_uri(package_id: &str) -> Result<String, ActionError> {
    if package_id.is_empty()
        || package_id.len() > 255
        || package_id
            .chars()
            .any(|ch| !ch.is_ascii_alphanumeric() && ch != '.' && ch != '-')
    {
        return Err(ActionError::Refused(
            "The catalogue entry has an invalid stable package identity".into(),
        ));
    }
    Ok(format!("qos-store:app/{package_id}"))
}

fn catalogue_aliases_host(
    catalogue: &CatalogueApp,
    host: &crate::shell::host_apps::HostApp,
) -> bool {
    // Display names and reverse-DNS tails are presentation, not identity:
    // GNOME Calendar and qOS Calendar are two real applications, as are the
    // many unrelated programs named Notes or Terminal.  Until the signed
    // catalogue contract grows explicit aliases, only the complete canonical
    // ID is strong enough evidence to collapse a cross-provider row.
    catalogue.package_id.eq_ignore_ascii_case(&host.desktop_id)
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
    // The app is already being installed/launched. Repeated activation should
    // be idempotent: one worker, one pending notice, one eventual window.
    if catalogue::pending_notice(package_id).is_some() {
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
        // The preflight failed before a toplevel could claim the placement
        // note. Do not let a later app inherit this dead launch's target.
        launcher::clear_pending_placement(package_id);
        let message = err.user_message();
        tracing::error!(
            target: "qos::app_launch",
            package_id,
            display_name,
            detail = %message,
            "application launch was refused before spawn"
        );
        {
            let mut st = chrome.borrow_mut();
            // Claimed back rather than left to expire, or the next window
            // to paint would retire somebody else's notice.
            if let Some(id) = catalogue::take_pending_notice(package_id) {
                st.dismiss_notification(id);
            }
            st.push_notification_full(
                Notification::new(
                    launcher::launch_failure_title(display_name),
                    err.notification_message(),
                )
                .with_severity(NotificationSeverity::Error)
                .with_app_name("Applications")
                .with_dismiss_after(launcher::LAUNCH_FAILURE_NOTICE_LIFETIME),
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
        out.extend(self.dynamic_results_for(
            ctx.needle,
            Self::dynamic_apps(),
            ctx.chrome.open_target,
        ));
        out.extend(self.catalogue_results_for_with_hosts(
            ctx.needle,
            Self::catalogue_apps(),
            &installed_ids,
            &crate::shell::host_apps::host_apps(),
            ctx.chrome.open_target,
        ));
        out
    }
}

/// Case-insensitive, first matching band wins in the order written below:
/// name prefix 100, id in full or last segment 100, name substring 50, id
/// prefix 60, id substring 25, category substring 20, description 15,
/// fuzzy subsequence on the name 10. Multi-word queries may span metadata
/// fields, so `office spreadsheet` can find an app whose category and
/// description carry one word each. Empty needle matches everything at 0, so an empty palette
/// lists installed apps. Category ranks below the id because it's a bucket,
/// not an identity. Package ids score by how completely they were named,
/// not flat "contains" — `hex-editor` typed for `dev.qos.hex-editor` is the
/// app's identity in full and must outrank an incidental substring.
pub(crate) fn match_score(
    needle: &str,
    name: &str,
    package_id: &str,
    category: Option<&str>,
    description: Option<&str>,
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
    if description.is_some_and(|d| d.contains(needle)) {
        return Some(15);
    }
    let terms: Vec<&str> = needle.split_whitespace().collect();
    if terms.len() > 1
        && terms.iter().all(|term| {
            name.contains(term)
                || package_id.contains(term)
                || category.is_some_and(|value| value.contains(term))
                || description.is_some_and(|value| value.contains(term))
        })
    {
        return Some(15);
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
    #[serial_test::serial(launch_events)]
    fn repeated_activation_while_opening_is_idempotent() {
        let chrome = chrome_rc();
        let package_id = "dev.qos.repeated-open-fixture";
        let notice = chrome
            .borrow_mut()
            .push_notification("Opening Fixture", "Loading…");
        catalogue::note_pending_notice(package_id, notice);

        assert!(launch_installed_app(&chrome, package_id, "Fixture").is_ok());
        assert_eq!(
            chrome.borrow().notifications().len(),
            1,
            "a second activation must not create another worker or opening card"
        );
        assert_eq!(catalogue::take_pending_notice(package_id), Some(notice));
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
            Some("Opens beside · qOS · dev.qos.hello · 0.1.0"),
            "the row names the package it will launch"
        );
    }

    #[test]
    fn available_apps_are_discoverable_by_description_and_cross_field_terms() {
        let p = ApplicationsProvider::new();
        let mut candidate = offered("dev.qos.ledger", "Ledger", Some("Office"));
        candidate.description = "Fast collaborative spreadsheet".to_string();

        let by_description = p.catalogue_results_for(
            "spreadsheet",
            vec![candidate.clone()],
            &[],
            OpenTarget::Beside,
        );
        assert_eq!(
            by_description.first().map(|row| row.id.as_str()),
            Some("app:dev.qos.ledger")
        );

        let across_fields = p.catalogue_results_for(
            "office collaborative",
            vec![candidate],
            &[],
            OpenTarget::Beside,
        );
        assert_eq!(
            across_fields.first().map(|row| row.id.as_str()),
            Some("app:dev.qos.ledger")
        );
    }

    #[test]
    fn duplicate_registry_offers_produce_one_application_row() {
        let p = ApplicationsProvider::new();
        let app = offered("dev.qos.notes", "Notes", Some("Office"));
        let out = p.catalogue_results_for("notes", vec![app.clone(), app], &[], OpenTarget::Beside);
        assert_eq!(out.len(), 1, "registry mirrors must not duplicate an app");
    }

    #[test]
    fn an_exact_host_package_identity_hides_its_catalogue_alias() {
        let p = ApplicationsProvider::new();
        let hosts = vec![crate::shell::host_apps::HostApp {
            desktop_id: "dev.qos.notes".into(),
            name: "Notes".into(),
            source_label: "Flatpak",
            categories: Vec::new(),
            keywords: Vec::new(),
            window_identities: vec!["dev.qos.notes".into()],
        }];
        let out = p.catalogue_results_for_with_hosts(
            "notes",
            vec![offered("dev.qos.notes", "Notes", Some("Office"))],
            &[],
            &hosts,
            OpenTarget::Beside,
        );
        assert!(
            out.is_empty(),
            "opening an existing app must not offer a duplicate fetch"
        );
    }

    fn catalogue_rows_with_host(
        catalogue_id: &str,
        catalogue_name: &str,
        host_id: &str,
        host_name: &str,
    ) -> Vec<SearchResult> {
        let p = ApplicationsProvider::new();
        let hosts = vec![crate::shell::host_apps::HostApp {
            desktop_id: host_id.into(),
            name: host_name.into(),
            source_label: "System",
            categories: Vec::new(),
            keywords: Vec::new(),
            window_identities: vec![host_id.into()],
        }];
        p.catalogue_results_for_with_hosts(
            catalogue_name,
            vec![offered(catalogue_id, catalogue_name, None)],
            &[],
            &hosts,
            OpenTarget::Beside,
        )
    }

    #[test]
    fn gnome_calendar_does_not_hide_qos_calendar() {
        let rows = catalogue_rows_with_host(
            "dev.qos.calendar",
            "Calendar",
            "org.gnome.Calendar",
            "Calendar",
        );
        assert_eq!(rows.len(), 1);
        assert!(rows[0].subtitle.as_deref().unwrap().contains("qOS"));
    }

    #[test]
    fn host_terminal_does_not_hide_qos_terminal() {
        let rows = catalogue_rows_with_host(
            "dev.qos.terminal",
            "Terminal",
            "org.gnome.Console",
            "Terminal",
        );
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn a_generic_shared_name_is_not_cross_provider_identity() {
        let rows = catalogue_rows_with_host(
            "dev.qos.notes",
            "Notes",
            "org.example.PrivateNotes",
            "Notes",
        );
        assert_eq!(rows.len(), 1);
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
            Some("Opens beside · qOS · Office · dev.qos.sheets"),
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
    fn empty_needle_lists_every_app_with_a_useful_fresh_system_order() {
        let p = ApplicationsProvider::new();
        let out = p.results_for(
            "",
            vec![
                app("dev.qos.monitor", "System Monitor"),
                app("dev.qos.browser", "Browser"),
                app("dev.qos.notes", "Notes"),
                app("dev.qos.hello", "Hello"),
            ],
            OpenTarget::Beside,
        );
        assert_eq!(out.len(), 4);
        let score = |id: &str| out.iter().find(|row| row.id == id).unwrap().score;
        assert!(score("dev.qos.browser") > score("dev.qos.notes"));
        assert!(score("dev.qos.notes") > score("dev.qos.monitor"));
        assert!(score("dev.qos.monitor") > score("dev.qos.hello"));
        assert!(
            out.iter()
                .find(|row| row.id == "dev.qos.browser")
                .and_then(|row| row.subtitle.as_deref())
                .is_some_and(|subtitle| subtitle.contains("Start here")),
            "the ordering must also be explained on screen"
        );
    }

    #[test]
    fn typed_search_ignores_default_presentation_tiers() {
        let p = ApplicationsProvider::new();
        let out = p.results_for(
            "monitor",
            vec![
                app("dev.qos.browser", "Browser"),
                app("dev.qos.monitor", "System Monitor"),
            ],
            OpenTarget::Beside,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "dev.qos.monitor");
        assert_eq!(
            out[0].score, 100,
            "normal match scoring remains authoritative"
        );
        assert!(
            !out[0]
                .subtitle
                .as_deref()
                .unwrap_or_default()
                .contains("System tools"),
            "presentation bands are not search metadata or query noise"
        );
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

    #[test]
    fn an_uninstalled_app_opens_its_canonical_store_detail() {
        let p = ApplicationsProvider::new();
        let out = p.catalogue_results_for(
            "she",
            vec![offered("dev.qos.sheets", "Sheets", Some("Office"))],
            &[],
            OpenTarget::Beside,
        );
        assert_eq!(out.len(), 1, "the offered app must match: {out:?}");
        assert_eq!(
            out[0].title, "View Sheets in App Store",
            "palette activation must present a reviewable Store detail, not install"
        );
        assert_eq!(out[0].id, "app:dev.qos.sheets");
        assert_eq!(out[0].kind, ResourceKind::Application);
        assert!(
            out[0].subtitle.as_deref().is_some_and(
                |subtitle| subtitle.contains("App Store") && subtitle.contains("verified")
            ),
            "the palette must disclose the review surface and trust claim"
        );
        assert_eq!(
            out[0].provider_id, "applications",
            "one provider means one group header — a second would be a \
             visible 'other kind of app' section"
        );
    }

    #[test]
    fn palette_result_never_installs_and_store_uri_is_stable() {
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
        assert_eq!(
            store_detail_uri("dev.qos.sheets").unwrap(),
            "qos-store:app/dev.qos.sheets"
        );
        assert!(store_detail_uri("dev.qos.sheets?install=true").is_err());
    }

    #[test]
    fn uninstalled_rows_are_explicit_about_the_store_review_surface() {
        let p = ApplicationsProvider::new();
        let available = p.catalogue_results_for(
            "",
            vec![offered("dev.qos.sheets", "Sheets", Some("Office"))],
            &[],
            OpenTarget::Beside,
        );
        assert!(available[0].title.contains("App Store"));
        assert!(
            available[0]
                .subtitle
                .as_deref()
                .is_some_and(|subtitle| subtitle.contains("verified"))
        );
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
    #[serial_test::serial(launch_events)]
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
        let opened = (row.on_open)(&mut doc, &automerge::ROOT, &c, &ActionArgs::none());
        assert!(
            opened.is_err(),
            "no active workspace is no room to launch into — the caller must be told, \
             not answered as though the app had started",
        );
        let st = c.borrow();
        let n = st
            .notifications()
            .last()
            .expect("activation must never be silent");
        assert_eq!(n.title, "Couldn't open App Store for Sheets");
        assert_eq!(n.severity, NotificationSeverity::Error);
        assert_eq!(
            n.body,
            launcher::LaunchError::NoWorkspace.notification_message(),
            "the same failure gets the same words whichever list the app \
             came from"
        );
    }

    #[test]
    fn shell_query_includes_an_uninstalled_app_as_a_store_detail_row() {
        let p = ApplicationsProvider::new();
        crate::shell::catalogue::seed_for_test(vec![offered(
            "dev.qos.uninstalled-sheets",
            "Sheets",
            Some("Office"),
        )]);
        let c = chrome_rc();
        let state = c.borrow();
        let doc = AutoCommit::new();
        let results = p.query(SearchContext {
            needle: "sheets",
            wanted: &[ResourceKind::Application],
            chrome: &state,
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        let row = results
            .iter()
            .find(|row| row.id == "app:dev.qos.uninstalled-sheets")
            .expect("Win+K finds the uninstalled stable app identity");
        assert_eq!(row.title, "View Sheets in App Store");
        assert!(
            row.subtitle
                .as_deref()
                .is_some_and(|text| text.contains("verified"))
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

    #[test]
    fn reviewed_web_shortcut_is_searchable_but_malformed_records_are_hidden() {
        let p = ApplicationsProvider::new();
        let record = |id: &str, target: Option<&str>| DynamicLauncherRecord {
            id: id.into(),
            owner_app_id: "org.example.creator".into(),
            name: "Project board".into(),
            launcher_type: 2,
            target: target.map(str::to_string),
            icon_kind: "qos-web".into(),
            icon_bytes: None,
        };
        let out = p.dynamic_results_for(
            "project",
            vec![
                record("portal-valid", Some("https://example.test/board")),
                record("portal-invalid", Some("javascript:alert(1)")),
            ],
            OpenTarget::Beside,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "portal-valid");
        assert_eq!(out[0].title, "Open Project board");
        assert!(out[0].subtitle.as_deref().unwrap().contains("Shortcut"));
    }
}
