//! Host applications provider: everything already installed on the
//! machine that qOS did not package.
//!
//! The sibling of [`ApplicationsProvider`], and deliberately a separate
//! provider under a separate [`ResourceKind`] rather than more rows in
//! that one. A qOS app runs sandboxed, in the workspace you are looking
//! at, with one writable directory. A host app runs unconfined, in no
//! room, with the user's own files. Both belong in the palette; putting
//! them in the same group would make the difference invisible at
//! exactly the moment it matters, which is the moment you press Enter.
//!
//! So they get their own chip ("Host Apps"), their own group header,
//! and a row that names the source and says "unconfined" outright.
//!
//! [`ApplicationsProvider`]: super::applications::ApplicationsProvider
//! [`ResourceKind`]: crate::shell::search::ResourceKind

use crate::shell::chrome_state::Chrome;
use crate::shell::host_apps::{self, HostApp};
use crate::shell::notification::{Notification, NotificationSeverity};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

/// Chip label / group header. Mirrors
/// [`ResourceKind::HostApplication`]'s label.
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

    /// The host applications this provider offers. Split out so tests
    /// can score against a known list without depending on what
    /// happens to be installed.
    fn apps(&self) -> Vec<HostApp> {
        host_apps::host_apps()
    }

    fn results_for(&self, needle: &str, apps: Vec<HostApp>) -> Vec<SearchResult> {
        // A qOS package and a host app can name the same program — a
        // packaged terminal and the distribution's, say. When they do,
        // the confined one is the answer, so the host row is dropped
        // rather than offered as a near-identical second choice the
        // user has to tell apart by its chip.
        self.results_against(needle, apps, &crate::shell::launcher::gui_apps())
    }

    /// [`Self::results_for`] with the installed-package list supplied.
    ///
    /// Split out so a test can decide what is installed. Reading the
    /// real registry made the scoring tests depend on what happened to
    /// be installed on the machine running them, which is how a test
    /// starts passing or failing for reasons unrelated to the code.
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
                // The subtitle says the thing the user needs before
                // they press Enter, not the thing that is easiest to
                // print. "Unconfined" is not decoration: this app is
                // about to get the user's real home directory.
                Some(format!(
                    "{} · unconfined host app · {}",
                    app.source_label, app.desktop_id
                )),
                score,
                move |_doc, _root, chrome: &Chrome| {
                    // Where the window goes is decided now, while the
                    // layout the user was looking at is still true —
                    // the window itself maps seconds later, from a
                    // process qOS does not own. A foreign toplevel
                    // announces an app-id qOS did not mint, so the note
                    // is keyed on every identity the entry declared.
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
                    // No capability publication and no workspace id:
                    // there is no sandbox for either to act on. See
                    // `crate::shell::host_apps`.
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

/// Whether an installed qOS package already answers for this app.
///
/// Matched on the identity the two namespaces can actually share: the
/// trailing component of the reverse-DNS id, and the display name. Both
/// have to be exact — a host app called "Files" must not be hidden by a
/// qOS package called "File Explorer" just because the words overlap.
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

/// Same scale as the applications provider, one step lower at each
/// rung so that when a qOS app and a host app match equally well the
/// confined one sorts first. Categories and `Keywords=` are matched
/// too — that is what `Keywords` is for, and it is how typing "browser"
/// finds Firefox.
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
    fn id(&self) -> &str {
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
        self.results_for(ctx.needle, self.apps())
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

    /// The row has to say what it is before the user presses Enter.
    /// "Unconfined" in the subtitle is the whole point of separating
    /// these from qOS applications.
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

    /// Host apps sit under their own kind, so the palette gives them
    /// their own chip and group header rather than mixing them into
    /// "Applications".
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

    /// Equal-quality matches must rank the confined app first, because
    /// Enter takes the top row.
    #[test]
    fn a_host_app_scores_below_an_equally_good_qos_match() {
        use crate::shell::search::providers::applications::ApplicationsProvider;
        let host = HostApplicationsProvider::new();
        let qos = ApplicationsProvider::new();
        // Different name and different id tail from the qOS package
        // below, so what this measures is the scoring scale rather than
        // the shadowing rule.
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
            // The destination only words the row; it does not enter the
            // score, which is what this comparison is about.
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
        let out = p.results_against(
            "",
            vec![app("a.b.One", "One"), app("a.b.Two", "Two")],
            &[],
        );
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

    /// Opening a row for an app that is not installed must surface a
    /// visible error rather than failing silently. Drives the real
    /// `on_open` closure the palette fires on Enter.
    #[test]
    fn a_failed_launch_pushes_an_error_notification() {
        let p = HostApplicationsProvider::new();
        let c = chrome_rc();
        let mut doc = AutoCommit::new();
        let out = p.results_against(
            "ghost",
            vec![app("dev.qos.ghost-host-app", "Ghost")],
            &[],
        );
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

    /// Two rows for the same program, one confined and one not, is a
    /// choice the user should not have to make from a chip label.
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
