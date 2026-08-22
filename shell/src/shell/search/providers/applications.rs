//! Applications provider: installed apps, launchable from the palette.
//!
//! Typing an app's name into Ctrl+K offers `Open <App>`; picking the row
//! starts the package as a real sandboxed process through
//! [`crate::shell::launcher`]. The row's `on_open` is the only launch
//! trigger in the shell today, so everything a launch needs to report —
//! "not installed", "no display yet", "qos-launcher missing" — surfaces
//! here as a notification.

use std::rc::Rc;

use crate::shell::chrome_state::{Chrome, OpenTarget};
use crate::shell::launcher::{self, InstalledApp};
use crate::shell::notification::{Notification, NotificationSeverity};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

/// Chip label / group header for launchable apps. Mirrors
/// [`ResourceKind::Application`]'s label; kept as a constant because
/// the provider's `name()` is the group header too.
pub const APPLICATIONS_KIND: &str = "Applications";

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
            let subtitle = format!("{} · {detail}", open_target.result_hint());
            out.push(SearchResult {
                kind: kind.clone(),
                provider_id: self.id().to_string(),
                id: app.package_id.clone(),
                title: format!("Open {}", app.display_name),
                subtitle: Some(subtitle),
                score,
                on_open: Rc::new(move |_doc, _root, chrome: &Chrome| {
                    launch_installed_app(chrome, &package_id, &display_name);
                }),
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
pub(crate) fn launch_installed_app(chrome: &Chrome, package_id: &str, display_name: &str) {
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
        let mut st = chrome.borrow_mut();
        st.push_notification_full(
            Notification::new(format!("Couldn't open {display_name}"), err.user_message())
                .with_severity(NotificationSeverity::Error)
                .with_app_name("Applications"),
        );
        st.bump();
    }
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
        self.results_for(ctx.needle, self.apps(), ctx.chrome.open_target)
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

    fn app(package_id: &str, display_name: &str) -> InstalledApp {
        app_in(package_id, display_name, None)
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
    fn matching_app_offers_an_open_row() {
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
    fn declared_category_is_matchable_and_replaces_the_version_subtitle() {
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

    /// Opening a row for a package that isn't installed must surface a
    /// visible error rather than failing silently. Drives the real
    /// `on_open` closure the palette fires on Enter.
    #[test]
    fn failed_launch_pushes_an_error_notification() {
        let p = ApplicationsProvider::new();
        let c = chrome_rc();
        let mut doc = AutoCommit::new();
        // The row is built against an app that the registry does not
        // actually contain, so `launcher::launch` refuses it.
        let out = p.results_for("ghost", vec![app("dev.qos.ghost", "Ghost")], OpenTarget::Beside);
        let row = out.first().expect("a row for the ghost app");
        assert!(c.borrow().notifications().is_empty());
        (row.on_open)(&mut doc, &automerge::ROOT, &c);
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
}
