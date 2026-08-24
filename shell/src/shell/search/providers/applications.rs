//! Applications provider: apps you can open, launchable from the
//! palette.
//!
//! Typing an app's name into Ctrl+K offers `Open <App>`; picking the row
//! starts the package as a real sandboxed process through
//! [`crate::shell::launcher`]. The row's `on_open` is the only launch
//! trigger in the shell today, so everything a launch needs to report —
//! "no app with that id", "no display yet", "qos-launcher missing" —
//! surfaces here as a notification.
//!
//! ## Apps that are not here yet
//!
//! What this provider offers is installed apps **plus** whatever the
//! machine's configured registries describe and it does not have (see
//! [`crate::shell::catalogue`]). Both live in one group, under one
//! header, ranked against each other — a second provider would mean a
//! second section headed "here is the other kind of app", and the
//! sections would compete rather than rank.
//!
//! But the two rows are not the same row, because the two acts are not
//! the same act. Opening an app that is here takes a moment and
//! changes nothing about the machine. Opening one that is not means
//! going out to a registry, pulling a package down, checking who
//! signed it, and unpacking it — seconds of waiting, over the network,
//! after which this machine has software it did not have before. A row
//! that reads identically to a local one asks the user to consent to
//! all of that without telling them any of it is going to happen.
//!
//! So a row for an app that is not here says two more things than one
//! for an app that is, and no more: its verb is **Fetch and open**
//! rather than Open, and it carries the registry it would come from in
//! [`SearchResult::fetch_source`], which the palette renders as a small
//! trailing chip. Everything else — the group, the glyph, the
//! subtitle's shape, the ranking scale, the activation path — is
//! identical, so the distinction is one the eye can find and not one
//! that reorganises the list.
//!
//! Neither string says *install*. Nobody asked to install anything;
//! they asked to open an app, and fetching it is how that gets done.
//!
//! The wait is then accounted for rather than designed around. An open
//! that needs a fetch — and only that one — raises a standing
//! indication that reports where the fetch has got to (see
//! [`catalogue::FetchPhase`]), retired when the app's window appears,
//! or replaced by the reason it could not.

use std::rc::Rc;

use crate::shell::catalogue::{self, CatalogueApp};
use crate::shell::chrome_state::{Chrome, OpenTarget};
use crate::shell::i18n;
use crate::shell::launcher::{self, InstalledApp};
use crate::shell::notification::{Notification, NotificationProgress, NotificationSeverity};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};
use crate::shell::search::action_api::{ActionArgs, ActionError};

/// Chip label / group header for launchable apps. Mirrors
/// [`ResourceKind::Application`]'s label; kept as a constant because
/// the provider's `name()` is the group header too.
pub const APPLICATIONS_KIND: &str = "Applications";

/// How much an app the machine does not have yet gives up against one
/// it does.
///
/// Exactly one point: enough that an app you have wins a *tie* against
/// one you don't, and nothing more. Every band in [`match_score`] is at
/// least five points apart, so this can never drag a better match below
/// a worse one — typing an available app's name outright still beats a
/// vague fuzzy hit on an installed one, which is what naming an app
/// means. The two alternatives are both worse: ranking the lists
/// separately, or pushing not-installed apps below every installed one,
/// each make "the app I actually named" lose to "an app I happen to
/// have".
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

    /// The installed GUI apps this provider offers. Split out so tests
    /// can exercise scoring against a known list without an installed
    /// registry.
    fn apps(&self) -> Vec<InstalledApp> {
        launcher::gui_apps()
    }

    /// Score `apps` against `needle`. `pub(crate)` so the host-app
    /// provider can assert its own scale ranks below this one — the
    /// two scales only mean anything relative to each other, and a
    /// test that reimplemented either would not notice them drifting.
    ///
    /// `open_target` is only used to word the row — the launch reads the
    /// live target again at activation time, because the user can retarget
    /// the palette (or dismiss and reopen it) between the query that built
    /// this row and the Enter that fires it.
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
            // The category, when the publisher declared one, is the more
            // useful second line than the version: it tells you what kind
            // of app the row will open.
            // Where the window will land leads the line. It is the one
            // part of a launch row that changes with context, and a user
            // who opened the palette from a specific group's "+" needs to
            // see that the pick honours that group before they commit.
            let detail = match app.category.as_deref() {
                Some(cat) => format!("{cat} · {}", app.package_id),
                None => format!("{} · {}", app.package_id, app.version),
            };
            let subtitle = i18n::t_args(
                "palette-app-subtitle",
                &[
                    i18n::arg("hint", open_target.result_hint()),
                    i18n::arg("detail", detail.as_str()),
                ],
            );
            out.push(SearchResult::nullary_fallible(
                kind.clone(),
                self.id(),
                app.package_id.clone(),
                i18n::t_args(
                    "palette-open-app",
                    &[i18n::arg("app", app.display_name.as_str())],
                ),
                Some(subtitle),
                score,
                move |_doc, _root, chrome: &Chrome| {
                    launch_installed_app(chrome, &package_id, &display_name)
                },
            ));
        }
        out
    }

    /// The apps the configured registries offer that are not installed
    /// here. Answered from cache — see [`catalogue::available_apps`].
    fn catalogue_apps(&self) -> Vec<CatalogueApp> {
        catalogue::available_apps()
    }

    /// Score not-yet-installed apps against `needle` and build their
    /// rows.
    ///
    /// Structurally a mirror of [`Self::results_for`] — same group,
    /// same glyph, same score bands, same subtitle shape — differing in
    /// exactly the two places where the truth differs: the verb says
    /// the row will fetch, and [`SearchResult::fetch_source`] names the
    /// registry it would fetch from. See the module header for why that
    /// is two things and not zero or five.
    ///
    /// `installed` is the ids already present, so an app cannot be
    /// offered twice in the window between an install finishing and the
    /// catalogue being refetched.
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
            if installed.iter().any(|id| *id == app.package_id) {
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
            let registry = app.registry.clone();
            // The same fields an installed row shows, plus where it
            // would come from. The registry goes at the *end* because
            // the palette elides a subtitle from the middle: the part
            // that says this is not a local app is the part that has to
            // survive a narrow window.
            let detail = match app.category.as_deref() {
                Some(cat) => format!("{cat} · {}", app.package_id),
                None => format!("{} · {}", app.package_id, app.version),
            };
            let subtitle = i18n::t_args(
                "palette-app-subtitle-remote",
                &[
                    i18n::arg("hint", open_target.result_hint()),
                    i18n::arg("detail", detail.as_str()),
                    i18n::arg("registry", app.registry.as_str()),
                ],
            );
            out.push(SearchResult {
                kind: ResourceKind::Application,
                provider_id: self.id().to_string(),
                id: app.package_id.clone(),
                title: i18n::t_args(
                    "palette-fetch-and-open-app",
                    &[i18n::arg("app", app.display_name.as_str())],
                ),
                subtitle: Some(subtitle),
                score: score - NOT_INSTALLED_TIEBREAK,
                fetch_source: Some(app.registry.clone()),
                on_open: Rc::new(move |_doc, _root, chrome: &Chrome, _args: &ActionArgs| {
                    // Everything decided on the frame thread is decided
                    // *now*, while the user's selection and the layout
                    // they were looking at are both still true. The
                    // window may be a minute away — long enough for the
                    // active workspace to have changed — so the room
                    // and the grants are captured here and carried to
                    // the launch rather than read again when it
                    // happens. Opening an app somewhere means opening
                    // it where you asked.
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
                        // No active workspace is no room and no grant
                        // to launch into — reported exactly as an
                        // installed app's launch reports it, because it
                        // is exactly the same failure.
                        let mut st = chrome.borrow_mut();
                        st.push_notification_full(
                            Notification::new(
                                i18n::t_args(
                                    "notice-open-failed",
                                    &[i18n::arg("app", display_name.as_str())],
                                ),
                                launcher::LaunchError::NoWorkspace.user_message(),
                            )
                            .with_severity(NotificationSeverity::Error)
                            .with_app_name("Applications"),
                        );
                        st.bump();
                        return Ok(());
                    };
                    // The palette closes the instant this returns, so
                    // this is the only thing standing between the user
                    // and an apparently dead Enter key — and the wait
                    // is seconds, not milliseconds.
                    //
                    // Raised *before* the work starts, so the
                    // acknowledgement is on screen on the same frame as
                    // the keystroke rather than whenever the first byte
                    // lands. Its body is rewritten each frame from the
                    // fetch's own reported phase, so it says what is
                    // actually happening — contacting the registry,
                    // this much of that much downloaded, unpacking —
                    // rather than a fixed word that could equally mean
                    // "wedged". It is retired by the app's window
                    // appearing, not by a timer, so the indication
                    // covers the whole wait and ends on something the
                    // user can see.
                    let notice = {
                        let mut st = chrome.borrow_mut();
                        let id = st.push_notification_full(
                            Notification::new(
                                i18n::t_args(
                                    "notice-opening-app",
                                    &[i18n::arg("app", display_name.as_str())],
                                ),
                                i18n::t_args(
                                    "notice-fetch-connecting",
                                    &[i18n::arg("registry", registry.as_str())],
                                ),
                            )
                            .with_app_name("Applications")
                            .with_progress(NotificationProgress::Indeterminate)
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

/// Launch an installed package into the workspace the user is looking
/// at, reporting a pre-fork failure as a notification.
///
/// Shared by the Applications row and by the catalog's post-install
/// launch, so "the app opens" means one thing: the same placement, the
/// same grants, the same room. The app opens into the active workspace —
/// that workspace's grants become its sandbox, and its id becomes the
/// collaboration room it joins — so opening the same app in two
/// workspaces gives two independent sessions.
///
/// The grants are published immediately before the launch rather than
/// cached: a grant made a moment ago in the permissions UI must reach
/// this launch, and a revoked one must not.
/// The package id of the file manager qOS ships as a system app.
pub(crate) const SYSTEM_FILE_MANAGER: &str = "dev.qos.file-explorer";

/// Give the system file manager the user's home before it starts.
///
/// Every app gets one directory of its own, with `HOME` pointed inside
/// it, and nothing else it was not granted. That is right for an app
/// that edits its own documents and wrong for the one whose entire
/// subject is the user's files: it opens on a folder containing nothing
/// but its own dotfiles and reports, accurately, zero items.
///
/// The grant is not a hole in that model, it is the model being used.
/// It goes through the same [`GrantStoreHandle`](crate::portal::GrantStoreHandle)
/// the permission prompt and the file-chooser portal write to, so it is
/// a capability on the workspace like any other: it appears in the
/// permissions UI, it can be revoked there, and it expires with the
/// session rather than persisting. What it changes is who decides —
/// the OS, for the file manager it ships, instead of a dialog asking the
/// user to approve the file manager's access to files on first sight of
/// an empty window.
///
/// It names one package id rather than a rule about categories or
/// manifests deliberately. A rule that could be satisfied by a
/// third-party package would be a way to ask for the user's home by
/// declaring the right thing about yourself; qOS ships exactly one file
/// manager, and naming it is the narrower statement.
///
/// Write, not read: renaming, deleting and creating folders are what a
/// file manager is for, and a read-only grant would leave its toolbar
/// full of affordances that fail.
fn grant_user_home_to_file_manager(chrome: &Chrome, package_id: &str) {
    let ws_id = {
        let st = chrome.borrow();
        if st.tree().active_workspace().is_none() {
            return;
        }
        st.tree().active
    };
    let granted = {
        let mut st = chrome.borrow_mut();
        let mut tree = st.tree_mut();
        grant_file_manager_home(&mut tree, ws_id, package_id)
    };
    if granted {
        chrome.borrow_mut().bump();
    }
}

/// [`grant_user_home_to_file_manager`] against a tree, for the launches
/// that do not come from the palette.
///
/// The file manager is started by more than one path — the palette, and
/// [`crate::shell::peer_windows`] when a peer's window says the room has
/// one open. Only the first used to mint the grant, so a file manager
/// started on the room's behalf opened on its own sandbox directory and
/// reported, accurately, that it was empty. The grant is a precondition
/// of the app being usable at all, so it belongs with the launch rather
/// than with the palette that happened to be the first caller.
///
/// Returns whether a grant was added, so a caller with a repaint to do
/// can skip it when there was nothing to change.
///
/// # Minted per machine, and why that is not yet enough
///
/// Every member mints their own, because the path names *this* machine's
/// home and is meaningless on anybody else's. A member who only received
/// a copy of somebody else's still got no `QOS_USER_HOME`
/// (`packaging/launcher/src/sandbox.rs`, `push_user_home`, which exports
/// it only when the local `$HOME` is covered by a granted bind) and so
/// still opened on their sandbox directory.
///
/// This deliberately does **not** mirror the grant into the room
/// document the way `GrantStoreHandle::insert` does. That is not
/// sufficient to keep it off the wire, and the remaining hole is worth
/// stating plainly rather than leaving to be rediscovered: the workspace
/// projection writes `ws.capabilities` wholesale
/// (`WorkspaceTree`'s doc write, `state.rs`), so the grant reaches every
/// peer on the next persist regardless. A peer's sandbox then honours a
/// path naming *this* machine's home directory.
///
/// Closing it needs a capability the projection knows not to ship — and
/// one the doc-driven rebuild of `ws.capabilities` knows not to sweep,
/// since that rebuild replaces the list with whatever the document holds.
/// Both halves are a change to the capability model, not to this
/// function, so it is recorded here rather than half-done.
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
    // Asks the capability model rather than re-deriving prefix logic,
    // exactly as the portal's own check does.
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

pub(crate) fn launch_installed_app(
    chrome: &Chrome,
    package_id: &str,
    display_name: &str,
) -> Result<(), ActionError> {
    grant_user_home_to_file_manager(chrome, package_id);
    // The room may already have this app open here. If it does, the
    // window the person is asking for is on their screen already — put
    // it in front of them rather than starting a second copy of it
    // beside the first. See `peer_windows::opened_for_the_room` for why
    // that is not the same as refusing to open an app twice.
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
    // Say that the app is opening, before the work starts, so the
    // acknowledgement is on screen on the same frame as the keystroke.
    //
    // The window is seconds away — it is a process to start, a wayland
    // connection to make and a first frame to draw — and it is not shown
    // until it has something in it, precisely so nobody is handed an
    // empty frame to look at. That leaves this notification as the only
    // thing standing between the user and an apparently dead Enter key,
    // which is why a plain launch raises it and not just the path that
    // has to fetch the app first.
    //
    // It is retired by the window appearing rather than by a timer
    // (`ShellOverlayHost::retire_notices_for_opened_windows`), so the
    // indication covers exactly the wait and ends on something the
    // person can see.
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
        // Where the window goes is decided here, while the user's
        // selection and the layout they were looking at are both still
        // true — the window itself maps seconds later, on another
        // process. See `crate::shell::placement`.
        //
        // An explicit target beats the rule: the "+" on a group's tab
        // strip names the destination, and that is a stronger statement
        // of intent than any inference from what happens to be selected.
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
    // The launch itself is non-blocking: everything past resolution
    // happens on a worker thread. Only the pre-fork failures land here;
    // the rest arrive as launch events the chrome drains each frame.
    //
    // No active workspace means there is no room and no grant to launch
    // into, so there is nothing to start — reported the same way a
    // resolution failure is.
    let outcome = launched.unwrap_or(Err(launcher::LaunchError::NoWorkspace));
    if let Err(err) = outcome {
        let message = err.user_message();
        {
            let mut st = chrome.borrow_mut();
            // Nothing is opening after all, so stop saying it is. The
            // notice is claimed back rather than left to expire, or the
            // next window to paint would retire somebody else's.
            if let Some(id) = catalogue::take_pending_notice(package_id) {
                st.dismiss_notification(id);
            }
            st.push_notification_full(
                Notification::new(
                    i18n::t_args("notice-open-failed", &[i18n::arg("app", display_name)]),
                    message.clone(),
                )
                .with_severity(NotificationSeverity::Error)
                .with_app_name("Applications"),
            );
            st.bump();
        }
        // The notification is how the person at the machine learns. This
        // is how a caller that cannot see the screen learns — without it
        // an invocation that started nothing answers exactly like one
        // that started an app.
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
    fn id(&self) -> &str {
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
        let installed = self.apps();
        let installed_ids: Vec<String> =
            installed.iter().map(|a| a.package_id.clone()).collect();
        let mut out = self.results_for(ctx.needle, installed, ctx.chrome.open_target);
        out.extend(self.catalogue_results_for(
            ctx.needle,
            self.catalogue_apps(),
            &installed_ids,
            ctx.chrome.open_target,
        ));
        out
    }
}

/// Case-insensitive scoring, same scale as the actions provider:
/// prefix on the name → 100, substring on the name → 50, substring on
/// the package id → 25, substring on the declared category → 20, fuzzy
/// subsequence on the name → 10. An empty needle matches everything at
/// score 0 so an empty palette lists the installed apps.
///
/// Category ranks below the package id because it is a bucket, not an
/// identity: "office" should offer every office app, but never above an
/// app whose own name or id the user actually typed.
///
/// Package ids are scored by how completely they were named, not as one
/// flat "contains". `dev.qos.hex-editor` typed as `hex-editor` is the
/// app's identity spelled out in full and has to score like a name
/// match: the display name is "Hex Editor", so the hyphenated id is
/// often the only exact handle a user - or a script - has for it.
/// Scoring that the same 25 as an incidental substring let a file called
/// `hex-editor.rs` outrank the app itself.
pub(crate) fn match_score(needle: &str, name: &str, package_id: &str, category: Option<&str>) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    if name.starts_with(needle) {
        return Some(100);
    }
    // The id in full, or its last segment - `dev.qos.hex-editor` named
    // either as itself or as `hex-editor`. Both identify exactly one app.
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

    /// Pin the locale for a test that asserts on exact wording.
    ///
    /// These rows are translated now, so the strings depend on the
    /// developer's `LANG` unless the test says otherwise — which is how
    /// a German desktop turned the whole suite red. Paired with
    /// `#[serial_test::serial(locale)]` because the active locale is
    /// process-global.
    fn english() {
        i18n::set_locale("en-US");
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
        }
    }

    #[test]
    #[serial_test::serial(locale)]
    fn matching_app_offers_an_open_row() {
        english();
        let p = ApplicationsProvider::new();
        let out = p.results_for("hel", vec![app("dev.qos.hello", "Hello")], OpenTarget::Beside);
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
    #[serial_test::serial(locale)]
    fn declared_category_is_matchable_and_replaces_the_version_subtitle() {
        english();
        let p = ApplicationsProvider::new();
        let out = p.results_for("office", vec![
                app_in("dev.qos.sheets", "Sheets", Some("Office")),
                app_in("dev.qos.calculator", "Calculator", Some("Utility")),
            ], OpenTarget::Beside);
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
        let out = p.results_for("system", vec![
                app_in("dev.qos.monitor", "Monitor", Some("System")),
                app_in("dev.qos.sysinfo", "System Info", Some("Utility")),
            ], OpenTarget::Beside);
        assert_eq!(out.len(), 2);
        let score = |id: &str| out.iter().find(|r| r.id == id).unwrap().score;
        assert_eq!(score("dev.qos.sysinfo"), 100, "prefix on the display name");
        assert_eq!(score("dev.qos.monitor"), 20, "category-only match");
        assert!(score("dev.qos.sysinfo") > score("dev.qos.monitor"));
    }

    #[test]
    fn package_id_matches_score_below_name_matches() {
        let p = ApplicationsProvider::new();
        let out = p.results_for("dev.qos", vec![app("dev.qos.hello", "Hello"), app("org.x.zed", "Zed")], OpenTarget::Beside);
        assert_eq!(out.len(), 1, "only the matching package: {out:?}");
        // A shared id *prefix* names a publisher, not an app, so it
        // stays below a name match. It is no longer lumped in with an
        // incidental substring, hence 60 rather than the old flat 25.
        assert_eq!(out[0].score, 60);
        let by_name = p.results_for("hello", vec![app("dev.qos.hello", "Hello")], OpenTarget::Beside);
        assert!(
            by_name[0].score > out[0].score,
            "naming the app must still beat naming its publisher"
        );
    }

    /// `hex-editor` is the whole identity of `dev.qos.hex-editor`, and
    /// the only exact handle for it - the display name is "Hex Editor",
    /// which no amount of typing the id will prefix-match.
    ///
    /// This came out of a sweep: the app scored 25 on a bare `contains`
    /// while a source file called `hex-editor.rs` scored the top of the
    /// files provider's scale, so Enter opened the file and the app
    /// never launched.
    #[test]
    fn the_id_last_segment_is_an_identity_match() {
        let p = ApplicationsProvider::new();
        let out = p.results_for("hex-editor", vec![app("dev.qos.hex-editor", "Hex Editor")], OpenTarget::Beside);
        assert_eq!(out.len(), 1, "the app must match at all: {out:?}");
        assert_eq!(
            out[0].score, 100,
            "naming an app's id segment identifies it as exactly as \
             naming the app does"
        );
        // And the full id, spelled out.
        let full = p.results_for("dev.qos.hex-editor", vec![app("dev.qos.hex-editor", "Hex Editor")], OpenTarget::Beside);
        assert_eq!(full[0].score, 100);
    }

    #[test]
    fn non_matching_needle_returns_nothing() {
        let p = ApplicationsProvider::new();
        let out = p.results_for("zzzqqq", vec![app("dev.qos.hello", "Hello")], OpenTarget::Beside);
        assert!(out.is_empty());
    }

    #[test]
    fn empty_needle_lists_every_app() {
        let p = ApplicationsProvider::new();
        let out = p.results_for("", vec![app("dev.qos.hello", "Hello"), app("org.x.zed", "Zed")], OpenTarget::Beside);
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

    /// A launch that cannot start has to reach both audiences: the
    /// person looking at the screen gets a notification, and the caller
    /// that invoked the row by address gets a refusal. Drives the real
    /// `on_open` closure the palette fires on Enter.
    ///
    /// The refusal is the half that is easy to lose. Without it the
    /// control socket answers a failed launch exactly as it answers a
    /// successful one, so a script — or an agent — is told the app
    /// opened and goes on to wait for a window that will never arrive.
    #[test]
    #[serial_test::serial(locale)]
    fn a_failed_launch_notifies_the_user_and_refuses_the_caller() {
        english();
        use crate::shell::search::action_api::ActionError;

        let p = ApplicationsProvider::new();
        let c = chrome_rc();
        let mut doc = AutoCommit::new();
        // The row is built against an app that the registry does not
        // actually contain, so `launcher::launch` refuses it.
        let out = p.results_for("ghost", vec![app("dev.qos.ghost", "Ghost")], OpenTarget::Beside);
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

    // -----------------------------------------------------------------
    // Apps that are not installed here yet.
    //
    // Two properties are load-bearing here and they pull against each
    // other. A row for an app the machine has to fetch must be
    // *findable* — activating it goes to the network and leaves this
    // machine with software it did not have, and a row that reads
    // identically to a local one asks for that without saying so. And
    // it must stay *structurally identical* — same group, same glyph,
    // same subtitle shape, same ranking — or the palette grows a "here
    // is the other kind of app" section that competes with its own
    // results instead of ranking against them.
    //
    // So these assert on the exact strings in both directions: what
    // must differ, and what must not.
    // -----------------------------------------------------------------

    #[test]
    #[serial_test::serial(locale)]
    fn an_uninstalled_app_offers_a_row_that_says_it_will_fetch() {
        english();
        let p = ApplicationsProvider::new();
        let out = p.catalogue_results_for(
            "she",
            vec![offered("dev.qos.sheets", "Sheets", Some("Office"))],
            &[],
            OpenTarget::Beside,
        );
        assert_eq!(out.len(), 1, "the offered app must match: {out:?}");
        assert_eq!(
            out[0].title, "Fetch and open Sheets",
            "the verb has to say what activating the row actually does"
        );
        assert_eq!(out[0].id, "dev.qos.sheets");
        assert_eq!(out[0].kind, ResourceKind::Application);
        assert_eq!(
            out[0].provider_id, "applications",
            "one provider means one group header — a second would be a \
             visible 'other kind of app' section"
        );
    }

    /// The distinguishing fact travels on the row, so the palette (and
    /// anything else reading rows) does not have to work out for itself
    /// whether an app is here.
    #[test]
    fn only_an_uninstalled_row_carries_a_fetch_source() {
        let p = ApplicationsProvider::new();
        let installed = p.results_for(
            "sheets",
            vec![app_in("dev.qos.sheets", "Sheets", Some("Office"))],
            OpenTarget::Beside,
        );
        assert_eq!(
            installed[0].fetch_source, None,
            "an app that is here is not fetched from anywhere"
        );

        let available = p.catalogue_results_for(
            "sheets",
            vec![offered("dev.qos.sheets", "Sheets", Some("Office"))],
            &[],
            OpenTarget::Beside,
        );
        assert_eq!(
            available[0].fetch_source.as_deref(),
            Some("qOS main"),
            "the row names the registry it would come from, not merely \
             that it would come from one"
        );
    }

    /// The wording is honest about the network and silent about
    /// installing: the user asked to open an app, not to acquire one,
    /// and "install" is a word nobody typed.
    #[test]
    #[serial_test::serial(locale)]
    fn an_uninstalled_row_never_says_install() {
        english();
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
        for word in ["install", "download", "purchase", "buy "] {
            assert!(
                !lowered.contains(word),
                "{word:?} must not appear in a row the user reads as opening \
                 an app: {visible:?}"
            );
        }
        assert!(
            lowered.contains("fetch"),
            "and it must say the one true thing that distinguishes it: \
             {visible:?}"
        );
    }

    /// Same shape as an installed row, plus the provenance. Everything
    /// the two rows share has to keep being shared, or the difference
    /// stops being one difference and becomes a different list.
    #[test]
    #[serial_test::serial(locale)]
    fn an_uninstalled_subtitle_is_the_installed_one_plus_its_registry() {
        english();
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
        let mine = installed[0].subtitle.clone().expect("installed subtitle");
        let theirs = available[0].subtitle.clone().expect("available subtitle");
        assert_eq!(mine, "Opens beside · Office · dev.qos.sheets");
        assert_eq!(
            theirs, "Opens beside · Office · dev.qos.sheets · from qOS main",
            "the fields an installed row shows, then where this one comes from"
        );
        assert!(
            theirs.starts_with(&mine),
            "the shared part has to stay shared, or the two rows stop \
             reading as the same kind of thing: {mine:?} vs {theirs:?}"
        );
        // The registry goes last because the palette elides a subtitle
        // from its middle: the part that says this is not local is the
        // part that has to survive a narrow window.
        assert!(
            theirs.ends_with("from qOS main"),
            "provenance must be at the tail, where elision keeps it: {theirs:?}"
        );

        // And with no category declared, where the detail falls back to
        // id + version.
        let installed = p.results_for("", vec![app("dev.qos.plain", "Plain")], OpenTarget::Beside);
        let available = p.catalogue_results_for(
            "",
            vec![offered("dev.qos.plain", "Plain", None)],
            &[],
            OpenTarget::Beside,
        );
        let mine = installed[0].subtitle.clone().expect("installed subtitle");
        let theirs = available[0].subtitle.clone().expect("available subtitle");
        assert!(theirs.starts_with(&mine), "{mine:?} vs {theirs:?}");
        assert!(theirs.ends_with("from qOS main"));
    }

    /// Structure is what must not move: a difference the eye can find
    /// is the goal, a reorganised list is not.
    #[test]
    fn an_uninstalled_row_keeps_the_group_glyph_and_ranking_of_an_installed_one() {
        let p = ApplicationsProvider::new();
        let installed = p.results_for(
            "sheets",
            vec![app_in("dev.qos.sheets", "Sheets", Some("Office"))],
            OpenTarget::Beside,
        );
        let available = p.catalogue_results_for(
            "sheets",
            vec![offered("dev.qos.sheets", "Sheets", Some("Office"))],
            &[],
            OpenTarget::Beside,
        );
        assert_eq!(
            installed[0].kind, available[0].kind,
            "same kind means same group header and same glyph"
        );
        assert_eq!(installed[0].provider_id, available[0].provider_id);
        assert_eq!(
            installed[0].score - available[0].score,
            NOT_INSTALLED_TIEBREAK,
            "the two are ranked on one scale a single point apart, not on \
             two scales that cannot be compared"
        );
    }

    /// The ranking rule, stated as the two cases that matter.
    #[test]
    fn an_app_you_have_wins_a_tie_but_not_a_better_match() {
        let p = ApplicationsProvider::new();
        // Same name, same match quality: the one already here wins.
        let mine = p.results_for("notes", vec![app("dev.qos.notes", "Notes")], OpenTarget::Beside);
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
            // "sheets" is a fuzzy subsequence of "Some Header Editor
            // Extra Tools" — the weakest band there is.
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

    /// Between an install finishing and the catalogue being refetched,
    /// the same app is in both lists. It must produce one row, not two.
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

    /// Activating with no active workspace has to say so, in the same
    /// words an installed app's launch uses — not fail silently while
    /// the palette closes. Drives the real `on_open` closure.
    #[test]
    #[serial_test::serial(locale)]
    fn activating_with_no_workspace_reports_it_the_same_way() {
        english();
        let p = ApplicationsProvider::new();
        let c = chrome_rc();
        let mut doc = AutoCommit::new();
        // A default WorkspaceTree has no active workspace.
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

    /// Pressing Enter on an app whose bytes are not here must put
    /// something on screen *on that frame*, before any of the work
    /// starts. The wait is measured in seconds; silence across it is
    /// indistinguishable from a keystroke that did nothing.
    #[test]
    #[serial_test::serial(locale)]
    fn activating_an_app_that_has_to_load_says_so_immediately() {
        english();
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
        // Something has to be *moving*, or a slow fetch and a wedged
        // one look the same. Nothing is known about the size yet, so
        // the honest indication is indeterminate.
        assert_eq!(
            n.progress,
            Some(NotificationProgress::Indeterminate),
            "an open in flight has to carry a live progress indication"
        );
        // And it says where the bytes are coming from, because that is
        // the true and useful thing about this particular wait.
        assert_eq!(
            n.body, "Contacting qOS main…",
            "the body names the registry the app is being fetched from"
        );
        // It still never says install: nobody asked to install
        // anything.
        let visible = format!("{} {}", n.title, n.body).to_lowercase();
        assert!(
            !visible.contains("install"),
            "{:?} / {:?} must not read as an install",
            n.title,
            n.body
        );

        // The same open is readable by a caller that cannot see the
        // screen. An agent driving the palette over the control socket
        // has the same right to know the state of the thing it asked
        // for. (Where that state then goes — bytes, then unpacking,
        // then a launch or a legible failure — is proved end to end
        // against a real subprocess in `catalogue`.)
        assert!(
            catalogue::fetch_phase("dev.qos.sheets").is_some(),
            "an activated open must report a phase, not merely paint one"
        );
        catalogue::clear_fetch_phase("dev.qos.sheets");
    }

    /// The notice is not a sentence written once at the keystroke: it
    /// is rewritten from the fetch's own reports, so it keeps saying
    /// something newly true across the seconds it is up.
    #[test]
    #[serial_test::serial(locale)]
    fn the_notice_follows_the_fetch_through_its_phases() {
        use crate::shell::catalogue::FetchPhase;
        use crate::shell::overlay_host::progress_body;
        english();

        assert_eq!(
            progress_body(&FetchPhase::Connecting),
            None,
            "the opening sentence already names the registry; re-deriving \
             it without the name would say less"
        );
        assert_eq!(
            progress_body(&FetchPhase::Downloading {
                received: 512 * 1024,
                total: Some(4 * 1024 * 1024),
            })
            .as_deref(),
            Some("Downloading — 512 KB of 4.0 MB"),
            "a sized download says how much of how much — the number that \
             distinguishes a slow network from a stalled one"
        );
        assert_eq!(
            progress_body(&FetchPhase::Downloading {
                received: 512 * 1024,
                total: None,
            })
            .as_deref(),
            Some("Downloading — 512 KB"),
            "an unsized one says what has arrived rather than inventing a \
             total"
        );
        assert_eq!(
            progress_body(&FetchPhase::Unpacking).as_deref(),
            Some("Checking and unpacking…")
        );
        assert_eq!(
            progress_body(&FetchPhase::Starting).as_deref(),
            Some("Starting…")
        );
        // A failure reads as its own reason, not as a generic stop.
        assert_eq!(
            progress_body(&FetchPhase::Failed {
                detail: "the download failed: connection refused".to_string(),
            })
            .as_deref(),
            Some("the download failed: connection refused")
        );

        for phase in [
            FetchPhase::Downloading {
                received: 1,
                total: Some(2),
            },
            FetchPhase::Unpacking,
            FetchPhase::Starting,
        ] {
            let body = progress_body(&phase).expect("a phase the user waits through says something");
            assert!(
                !body.to_lowercase().contains("install"),
                "{phase:?} must not read as an install: {body:?}"
            );
        }
    }

    /// An app that is already here opens without a loading state — it
    /// does not need one, and a state that flashes for a hundred
    /// milliseconds is noise rather than feedback.
    #[test]
    #[serial_test::serial(locale)]
    fn an_app_that_is_already_here_does_not_raise_a_loading_state() {
        english();
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
