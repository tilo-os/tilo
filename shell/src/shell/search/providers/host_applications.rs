//! Host applications provider: everything already installed on the machine
//! that qOS did not package.
//!
//! Deliberately a separate provider under a separate [`ResourceKind`]
//! rather than more rows under [`ApplicationsProvider`]: a qOS app runs
//! sandboxed with one writable directory, a host app runs unconfined with
//! the user's own files. Own chip ("Host Apps"), own group header, and a
//! row that says "unconfined" outright.
//!
//! [`ApplicationsProvider`]: super::applications::ApplicationsProvider

use crate::shell::chrome_state::Chrome;
use crate::shell::host_apps::{self, HostApp};
use crate::shell::notification::{Notification, NotificationSeverity};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

/// Chip label / group header, mirroring [`ResourceKind::HostApplication`].
pub const HOST_APPLICATIONS_KIND: &str = "Host Apps";

pub struct HostApplicationsProvider {
    kinds: Vec<ResourceKind>,
}

impl HostApplicationsProvider {
    pub fn new() -> Self {
        Self {
            kinds: vec![ResourceKind::HostApplication],
        }
    }

    /// Split out so tests can score against a known list.
    fn apps() -> Vec<HostApp> {
        host_apps::host_apps()
    }

    fn results_for(&self, needle: &str, apps: Vec<HostApp>) -> Vec<SearchResult> {
        // When a qOS package and a host app name the same program, the
        // confined one is the answer, so the host row is dropped.
        self.results_against(needle, apps, &crate::shell::launcher::gui_apps())
    }

    /// [`Self::results_for`] with the installed-package list supplied, so a
    /// test's scoring doesn't depend on what happens to be installed.
    fn results_against(
        &self,
        needle: &str,
        apps: Vec<HostApp>,
        installed: &[crate::shell::launcher::InstalledApp],
    ) -> Vec<SearchResult> {
        let needle_lower = needle.to_lowercase();
        let mut out = Vec::new();
        for app in apps {
            if shadowed_by_a_qos_package(&app, installed) {
                continue;
            }
            let name_lower = app.name.to_lowercase();
            let id_lower = app.desktop_id.to_lowercase();
            let Some(score) = match_score(&needle_lower, &name_lower, &id_lower, &app) else {
                continue;
            };
            let desktop_id = app.desktop_id.clone();
            let display_name = app.name.clone();
            let notify_name = display_name.clone();
            out.push(SearchResult::nullary(
                ResourceKind::HostApplication,
                self.id(),
                app.desktop_id.clone(),
                format!("Open {}", app.name),
                // "Unconfined" is not decoration: this app is about to get
                // the user's real home directory.
                Some(format!(
                    "{} · unconfined host app · {}",
                    app.source_label, app.desktop_id
                )),
                score,
                move |_doc, _root, chrome: &Chrome| {
                    // Decided now, while the layout is still true; a
                    // foreign toplevel announces an app-id qOS did not
                    // mint, so the note is keyed on every declared identity.
                    if let Some(app) = host_apps::find(&desktop_id) {
                        let st = chrome.borrow();
                        let tree = st.tree();
                        crate::shell::placement::note_placement_for_launch_identities(
                            &app.desktop_id,
                            &app.window_identities,
                            &tree,
                            st.strip_viewport(),
                        );
                    }
                    // No capability publication or workspace id: no
                    // sandbox for either to act on.
                    if let Err(err) = host_apps::launch(&desktop_id) {
                        let mut st = chrome.borrow_mut();
                        st.push_notification_full(
                            Notification::new(
                                format!("Couldn't open {notify_name}"),
                                err.user_message(),
                            )
                            .with_severity(NotificationSeverity::Error)
                            .with_app_name(HOST_APPLICATIONS_KIND),
                        );
                        st.bump();
                    }
                },
            ));
        }
        out
    }
}

/// Whether an installed qOS package already answers for this app. Matched
/// on the trailing reverse-DNS segment or the display name, both exact —
/// overlapping words alone must not hide a host app.
fn shadowed_by_a_qos_package(
    app: &HostApp,
    installed: &[crate::shell::launcher::InstalledApp],
) -> bool {
    let host_tail = app
        .desktop_id
        .rsplit('.')
        .next()
        .unwrap_or(&app.desktop_id)
        .to_ascii_lowercase();
    let host_name = app.name.to_ascii_lowercase();
    installed.iter().any(|pkg| {
        let pkg_tail = pkg
            .package_id
            .rsplit('.')
            .next()
            .unwrap_or(&pkg.package_id)
            .to_ascii_lowercase();
        pkg.package_id.eq_ignore_ascii_case(&app.desktop_id)
            || pkg_tail == host_tail
            || pkg.display_name.to_ascii_lowercase() == host_name
    })
}

/// Same scale as the applications provider, one step lower at each rung so
/// a confined qOS app sorts first at an equal match. `Keywords=` and
/// categories are matched too — how typing "browser" finds Firefox.
fn match_score(needle: &str, name: &str, desktop_id: &str, app: &HostApp) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    if name.starts_with(needle) {
        return Some(95);
    }
    // The full id, or its last segment — `org.gnome.TextEditor` named
    // either way identifies exactly one application.
    if desktop_id == needle || desktop_id.rsplit('.').next() == Some(needle) {
        return Some(95);
    }
    if name.contains(needle) {
        return Some(45);
    }
    if desktop_id.starts_with(needle) {
        return Some(55);
    }
    if desktop_id.contains(needle) {
        return Some(24);
    }
    if app
        .keywords
        .iter()
        .any(|k| k.to_lowercase().contains(needle))
    {
        return Some(22);
    }
    if app
        .categories
        .iter()
        .any(|c| c.to_lowercase().contains(needle))
    {
        return Some(19);
    }
    if fuzzy_subsequence(needle, name) {
        return Some(9);
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

impl Default for HostApplicationsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchProvider for HostApplicationsProvider {
    fn id(&self) -> &'static str {
        "host-applications"
    }
    fn name(&self) -> &str {
        HOST_APPLICATIONS_KIND
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &self.kinds
    }
    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx
            .wanted
            .iter()
            .any(|k| matches!(k, ResourceKind::HostApplication))
        {
            return Vec::new();
        }
        self.results_for(ctx.needle, Self::apps())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};
    use automerge::AutoCommit;
    use parking_lot::RwLock;
    use std::sync::Arc;

    fn chrome_rc() -> Chrome {
        let handle: WorkspaceTreeHandle = Arc::new(RwLock::new(WorkspaceTree::default()));
        ChromeState::new_with_handle(handle)
    }

    fn app(desktop_id: &str, name: &str) -> HostApp {
        HostApp {
            desktop_id: desktop_id.to_string(),
            name: name.to_string(),
            source_label: "Flatpak",
            categories: Vec::new(),
            keywords: Vec::new(),
            window_identities: vec![desktop_id.to_string()],
        }
    }

    #[test]
    fn a_matching_host_app_offers_an_open_row() {
        let p = HostApplicationsProvider::new();
        let out = p.results_against("spot", vec![app("com.spotify.Client", "Spotify")], &[]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Open Spotify");
        assert_eq!(out[0].id, "com.spotify.Client");
        assert_eq!(out[0].kind, ResourceKind::HostApplication);
    }

    /// "Unconfined" in the subtitle is the whole point of the separate kind.
    #[test]
    fn the_row_names_the_source_and_says_it_is_unconfined() {
        let p = HostApplicationsProvider::new();
        let out = p.results_against("spot", vec![app("com.spotify.Client", "Spotify")], &[]);
        let subtitle = out[0].subtitle.as_deref().expect("a subtitle");
        assert!(subtitle.contains("Flatpak"), "{subtitle}");
        assert!(
            subtitle.contains("unconfined"),
            "the user must be able to tell a confined qOS app from an \
             unconfined host app without opening anything: {subtitle}"
        );
        assert!(subtitle.contains("com.spotify.Client"), "{subtitle}");
    }

    #[test]
    fn host_apps_are_a_separate_kind_from_qos_applications() {
        let p = HostApplicationsProvider::new();
        assert_eq!(p.resource_kinds(), &[ResourceKind::HostApplication]);
        assert_eq!(ResourceKind::HostApplication.label(), "Host Apps");
        assert_ne!(
            ResourceKind::HostApplication.label(),
            ResourceKind::Application.label()
        );
    }

    /// Equal-quality matches rank the confined app first — Enter takes the top row.
    #[test]
    fn a_host_app_scores_below_an_equally_good_qos_match() {
        use crate::shell::search::providers::applications::ApplicationsProvider;
        let host = HostApplicationsProvider::new();
        let qos = ApplicationsProvider::new();
        // Different name/id tail, so this measures the scale, not shadowing.
        let host_rows =
            host.results_against("term", vec![app("org.x.Terminator", "Terminator")], &[]);
        let qos_rows = qos.results_for(
            "term",
            vec![crate::shell::launcher::InstalledApp {
                package_id: "dev.qos.terminal".into(),
                display_name: "Terminal".into(),
                category: None,
                version: "1".into(),
                manifest_path: std::path::PathBuf::from("/x/manifest.toml"),
                payload_root: std::path::PathBuf::from("/x"),
                has_gui: true,
                opens_extensions: Vec::new(),
            }],
            crate::shell::chrome_state::OpenTarget::Beside,
        );
        assert!(
            qos_rows[0].score > host_rows[0].score,
            "qos {} must outrank host {}",
            qos_rows[0].score,
            host_rows[0].score
        );
    }

    #[test]
    fn keywords_and_categories_are_matchable() {
        let p = HostApplicationsProvider::new();
        let mut firefox = app("org.mozilla.firefox", "Firefox");
        firefox.keywords = vec!["Internet".into(), "WWW".into()];
        firefox.categories = vec!["Network".into(), "WebBrowser".into()];
        let by_keyword = p.results_against("internet", vec![firefox.clone()], &[]);
        assert_eq!(by_keyword.len(), 1, "{by_keyword:?}");
        let by_category = p.results_against("webbrowser", vec![firefox], &[]);
        assert_eq!(by_category.len(), 1, "{by_category:?}");
        assert!(
            by_keyword[0].score > by_category[0].score,
            "a declared keyword is a stronger signal than a category bucket"
        );
    }

    #[test]
    fn an_empty_needle_lists_every_host_app() {
        let p = HostApplicationsProvider::new();
        let out = p.results_against("", vec![app("a.b.One", "One"), app("a.b.Two", "Two")], &[]);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|r| r.score == 0));
    }

    #[test]
    fn a_non_matching_needle_returns_nothing() {
        let p = HostApplicationsProvider::new();
        assert!(
            p.results_against("zzzqqq", vec![app("com.spotify.Client", "Spotify")], &[])
                .is_empty()
        );
    }

    #[test]
    fn unrequested_kind_returns_empty() {
        let p = HostApplicationsProvider::new();
        let c = chrome_rc();
        let cs = c.borrow();
        let doc = AutoCommit::new();
        let out = p.query(SearchContext {
            needle: "spotify",
            wanted: &[ResourceKind::Application],
            chrome: &cs,
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        assert!(
            out.is_empty(),
            "the Applications chip must not pull in host apps — that is \
             the distinction the separate kind exists to draw"
        );
    }

    /// Must surface a visible error, not fail silently. Drives the real
    /// `on_open` closure.
    #[test]
    fn a_failed_launch_pushes_an_error_notification() {
        let p = HostApplicationsProvider::new();
        let c = chrome_rc();
        let mut doc = AutoCommit::new();
        let out = p.results_against("ghost", vec![app("dev.qos.ghost-host-app", "Ghost")], &[]);
        let row = out.first().expect("a row for the ghost app");
        assert!(c.borrow().notifications().is_empty());
        (row.on_open)(
            &mut doc,
            &automerge::ROOT,
            &c,
            &crate::shell::search::action_api::ActionArgs::none(),
        )
        .expect("launching takes no arguments");
        let st = c.borrow();
        let n = st
            .notifications()
            .last()
            .expect("a failed launch must notify the user");
        assert_eq!(n.title, "Couldn't open Ghost");
        assert_eq!(n.severity, NotificationSeverity::Error);
        assert!(!n.body.is_empty());
    }

    #[test]
    fn a_qos_package_of_the_same_name_hides_the_host_row() {
        let installed = vec![crate::shell::launcher::InstalledApp {
            package_id: "dev.qos.terminal".into(),
            display_name: "Terminal".into(),
            category: None,
            version: "1".into(),
            manifest_path: std::path::PathBuf::from("/x/manifest.toml"),
            payload_root: std::path::PathBuf::from("/x"),
            has_gui: true,
            opens_extensions: Vec::new(),
        }];
        // Same trailing id segment.
        assert!(shadowed_by_a_qos_package(
            &app("org.x.terminal", "Konsole"),
            &installed
        ));
        // Same display name.
        assert!(shadowed_by_a_qos_package(
            &app("org.x.whatever", "Terminal"),
            &installed
        ));
        // Merely overlapping words must NOT hide it.
        assert!(!shadowed_by_a_qos_package(
            &app("org.x.termite", "Terminal Emulator"),
            &installed
        ));
    }
}
