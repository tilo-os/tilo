//! Catalog provider: apps that are **not** installed, installable from
//! the palette.
//!
//! Typing an app's name offers `Install & open <App>` for every catalog
//! entry that matches and is not already on this machine. Picking the
//! row downloads the `.qpkg`, has `qos-install` verify and cache it, and
//! launches it into the workspace the user is looking at — the same
//! launch the Applications row performs, one download earlier. The
//! user never visits a store, and never "installs" anything in the
//! traditional sense: the package is cached, and can be pinned later.
//!
//! Rows share the [`ResourceKind::Application`] group with installed
//! apps but always rank below them: when both match, Enter opens what is
//! already here rather than fetching a second copy of it.

use std::rc::Rc;

use crate::shell::app_catalog::{self, CatalogApp, CatalogHandle, InstallEvent, InstallEventKind};
use crate::shell::chrome_state::{Chrome, OpenTarget};
use crate::shell::launcher;
use crate::shell::notification::{Notification, NotificationSeverity};
use crate::shell::search::providers::applications::match_score;
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

/// What picking a row does. Real rows start a download; tests swap in a
/// recorder so a unit test never reaches the network or `qos-install`.
pub type Installer = dyn Fn(&CatalogApp) -> bool;

pub struct CatalogProvider {
    kinds: Vec<ResourceKind>,
    catalog: CatalogHandle,
    installer: Rc<Installer>,
}

impl CatalogProvider {
    /// A provider over `catalog`, installing through
    /// [`app_catalog::install`].
    pub fn new(catalog: CatalogHandle) -> Self {
        Self {
            kinds: vec![ResourceKind::Application],
            catalog,
            installer: Rc::new(|app: &CatalogApp| app_catalog::install(app.clone())),
        }
    }

    /// Replace the install step. For tests.
    pub fn with_installer(mut self, installer: Rc<Installer>) -> Self {
        self.installer = installer;
        self
    }

    /// Score `apps` against `needle`, skipping anything in
    /// `installed_ids`.
    ///
    /// An empty needle offers nothing: the installed provider already
    /// lists every app on an empty palette, and a catalog with hundreds
    /// of entries would bury them. The catalog answers a name, not a
    /// browse — the Store is the browse.
    pub(crate) fn results_for(
        &self,
        needle: &str,
        apps: Vec<CatalogApp>,
        installed_ids: &[String],
        open_target: OpenTarget,
    ) -> Vec<SearchResult> {
        let needle_lower = needle.trim().to_lowercase();
        if needle_lower.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for app in apps {
            if installed_ids.iter().any(|id| *id == app.package_id) {
                continue;
            }
            let name_lower = app.display_name.to_lowercase();
            let id_lower = app.package_id.to_lowercase();
            let category_lower = app.category().map(str::to_lowercase);
            let Some(score) = match_score(
                &needle_lower,
                &name_lower,
                &id_lower,
                category_lower.as_deref(),
            ) else {
                continue;
            };
            // Same scale as the installed rows, one step below: an
            // installed app that matches as well as a catalog app wins
            // the top row, and therefore Enter.
            let score = score - 1;
            let detail = match app.category() {
                Some(cat) => format!("{cat} · {}", app.package_id),
                None => format!("{} · {}", app.package_id, app.version),
            };
            let subtitle = format!(
                "Not installed · {} · {detail} · from {}",
                open_target.result_hint(),
                app.registry_name
            );
            let installer = Rc::clone(&self.installer);
            let row_app = app.clone();
            out.push(SearchResult {
                kind: ResourceKind::Application,
                provider_id: self.id().to_string(),
                id: app.package_id.clone(),
                title: format!("Install & open {}", app.display_name),
                subtitle: Some(subtitle),
                score,
                on_open: Rc::new(move |_doc, _root, chrome: &Chrome| {
                    start_install(chrome, &row_app, installer.as_ref());
                }),
            });
        }
        out
    }
}

/// Kick off the install and tell the user it started. The install
/// itself runs on a worker thread; the chrome hears back through
/// [`app_catalog::drain_events`], and the launch happens then.
fn start_install(chrome: &Chrome, app: &CatalogApp, installer: &Installer) {
    let mut st = chrome.borrow_mut();
    if !installer(app) {
        // Already in flight: the first Enter is still working. Say so
        // rather than stay silent — a second press with no feedback
        // reads as "nothing happened".
        st.push_notification_full(
            Notification::new(
                format!("Already installing {}", app.display_name),
                "It opens as soon as the download finishes.",
            )
            .with_severity(NotificationSeverity::Info)
            .with_app_name("Applications"),
        );
        st.bump();
        return;
    }
    let size = if app.qpkg_size_bytes > 0 {
        format!(" · {}", human_size(app.qpkg_size_bytes))
    } else {
        String::new()
    };
    st.push_notification_full(
        Notification::new(
            format!("Installing {}…", app.display_name),
            format!("Fetching {} from {}{size}", app.version, app.registry_name),
        )
        .with_severity(NotificationSeverity::Info)
        .with_app_name("Applications"),
    );
    st.bump();
}

fn human_size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// What the chrome does with one finished install, once per frame.
///
/// Success means `qos-install` verified and cached the package, so the
/// registry now lists it: drop the launcher's memoised enumeration and
/// launch exactly as the Applications row would have. Failure is quoted
/// back to the user with `qos-install`'s own last line.
pub(crate) fn apply_install_event(chrome: &Chrome, event: InstallEvent) {
    match event.kind {
        InstallEventKind::Installed => {
            launcher::invalidate_app_cache();
            crate::shell::search::providers::applications::launch_installed_app(
                chrome,
                &event.package_id,
                &event.display_name,
            );
        }
        InstallEventKind::Failed { detail } => {
            let mut st = chrome.borrow_mut();
            st.push_notification_full(
                Notification::new(format!("Couldn't install {}", event.display_name), detail)
                    .with_severity(NotificationSeverity::Error)
                    .with_app_name("Applications"),
            );
            st.bump();
        }
    }
}

impl SearchProvider for CatalogProvider {
    fn id(&self) -> &str {
        "catalog"
    }
    fn name(&self) -> &str {
        "Applications"
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
        let installed: Vec<String> = launcher::installed_apps()
            .unwrap_or_default()
            .into_iter()
            .map(|a| a.package_id)
            .collect();
        self.results_for(
            ctx.needle,
            self.catalog.apps(),
            &installed,
            ctx.chrome.open_target,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};
    use automerge::AutoCommit;
    use parking_lot::RwLock;
    use std::cell::RefCell;
    use std::sync::Arc;

    fn chrome_rc() -> Chrome {
        let handle: WorkspaceTreeHandle = Arc::new(RwLock::new(WorkspaceTree::default()));
        ChromeState::new_with_handle(handle)
    }

    fn app(package_id: &str, display_name: &str, category: Option<&str>) -> CatalogApp {
        CatalogApp {
            package_id: package_id.to_string(),
            display_name: display_name.to_string(),
            categories: category.map(|c| vec![c.to_string()]).unwrap_or_default(),
            version: "2026.08".to_string(),
            qpkg_url: format!("http://registry.test/{package_id}.qpkg"),
            qpkg_size_bytes: 2 * 1024 * 1024,
            registry_name: "local".to_string(),
        }
    }

    fn provider() -> CatalogProvider {
        CatalogProvider::new(CatalogHandle::empty())
    }

    #[test]
    fn a_matching_catalog_app_offers_an_install_and_open_row() {
        let out = provider().results_for(
            "word",
            vec![app("dev.qos.word-processor", "Word Processor", Some("Office"))],
            &[],
            OpenTarget::Beside,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Install & open Word Processor");
        assert_eq!(out[0].id, "dev.qos.word-processor");
        assert_eq!(out[0].kind, ResourceKind::Application);
        assert_eq!(
            out[0].subtitle.as_deref(),
            Some("Not installed · Opens beside · Office · dev.qos.word-processor · from local"),
            "the row says it is not installed, where it opens, and where it comes from"
        );
    }

    /// The catalog shares a group with installed apps, so the same
    /// needle must rank the installed copy first: Enter opens what is
    /// here rather than fetching it again.
    #[test]
    fn a_catalog_row_ranks_one_below_the_same_installed_match() {
        let installed = crate::shell::search::providers::ApplicationsProvider::new();
        let here = installed.results_for(
            "word",
            vec![{
                let mut a = crate::shell::launcher::InstalledApp {
                    package_id: "dev.qos.word-processor".into(),
                    display_name: "Word Processor".into(),
                    category: None,
                    version: "1".into(),
                    manifest_path: Default::default(),
                    payload_root: Default::default(),
                    has_gui: true,
                    opens_extensions: Vec::new(),
                };
                a.category = Some("Office".into());
                a
            }],
            OpenTarget::Beside,
        );
        let there = provider().results_for(
            "word",
            vec![app("dev.qos.other-word-thing", "Word Processor", Some("Office"))],
            &[],
            OpenTarget::Beside,
        );
        assert_eq!(here[0].score, 100);
        assert_eq!(there[0].score, 99);
    }

    #[test]
    fn an_installed_package_is_never_offered_for_install() {
        let out = provider().results_for(
            "word",
            vec![app("dev.qos.word-processor", "Word Processor", None)],
            &["dev.qos.word-processor".to_string()],
            OpenTarget::Beside,
        );
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn an_empty_needle_offers_nothing() {
        let out = provider().results_for(
            "   ",
            vec![app("dev.qos.word-processor", "Word Processor", None)],
            &[],
            OpenTarget::Beside,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn non_matching_needle_offers_nothing() {
        let out = provider().results_for(
            "zzzqqq",
            vec![app("dev.qos.word-processor", "Word Processor", None)],
            &[],
            OpenTarget::Beside,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn unrequested_kind_returns_empty() {
        let p = CatalogProvider::new(CatalogHandle::with_apps(vec![app(
            "dev.qos.word-processor",
            "Word Processor",
            None,
        )]));
        let c = chrome_rc();
        let cs = c.borrow();
        let doc = AutoCommit::new();
        let out = p.query(SearchContext {
            needle: "word",
            wanted: &[ResourceKind::File],
            chrome: &cs,
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        assert!(out.is_empty());
    }

    /// Enter on the row starts exactly one install and tells the user
    /// it started — driving the real `on_open` the palette fires.
    #[test]
    fn picking_the_row_starts_the_install_and_notifies() {
        let started: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&started);
        let p = provider().with_installer(Rc::new(move |a: &CatalogApp| {
            sink.borrow_mut().push(a.package_id.clone());
            true
        }));
        let out = p.results_for(
            "word",
            vec![app("dev.qos.word-processor", "Word Processor", Some("Office"))],
            &[],
            OpenTarget::Beside,
        );
        let c = chrome_rc();
        let mut doc = AutoCommit::new();
        (out[0].on_open)(&mut doc, &automerge::ROOT, &c);
        assert_eq!(started.borrow().as_slice(), ["dev.qos.word-processor"]);
        let st = c.borrow();
        let n = st.notifications().last().expect("the user is told the install started");
        assert_eq!(n.title, "Installing Word Processor…");
        assert_eq!(n.body, "Fetching 2026.08 from local · 2.0 MB");
        assert_eq!(n.severity, NotificationSeverity::Info);
    }

    #[test]
    fn a_second_pick_while_in_flight_does_not_start_another_install() {
        let p = provider().with_installer(Rc::new(|_: &CatalogApp| false));
        let out = p.results_for(
            "word",
            vec![app("dev.qos.word-processor", "Word Processor", None)],
            &[],
            OpenTarget::Beside,
        );
        let c = chrome_rc();
        let mut doc = AutoCommit::new();
        (out[0].on_open)(&mut doc, &automerge::ROOT, &c);
        let st = c.borrow();
        let n = st.notifications().last().expect("a refused second pick still says something");
        assert_eq!(n.title, "Already installing Word Processor");
    }

    #[test]
    fn a_failed_install_event_becomes_an_error_notification() {
        let c = chrome_rc();
        apply_install_event(
            &c,
            InstallEvent {
                package_id: "dev.qos.word-processor".into(),
                display_name: "Word Processor".into(),
                kind: InstallEventKind::Failed {
                    detail: "signature: publisher key not trusted".into(),
                },
            },
        );
        let st = c.borrow();
        let n = st.notifications().last().expect("a failed install must notify");
        assert_eq!(n.title, "Couldn't install Word Processor");
        assert_eq!(n.body, "signature: publisher key not trusted");
        assert_eq!(n.severity, NotificationSeverity::Error);
    }

    /// A finished install goes straight into the real launch path. With
    /// nothing actually installed in this test process the launch is
    /// refused pre-fork — and that refusal is the proof the path ran:
    /// the notification is the Applications row's, not a catalog one.
    #[test]
    #[serial_test::serial(process_env)]
    fn an_installed_event_launches_through_the_applications_path() {
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: guarded by the `process_env` serial group.
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", tmp.path()) };
        launcher::invalidate_app_cache();
        let handle: WorkspaceTreeHandle = Arc::new(RwLock::new(WorkspaceTree::bootstrap_default()));
        let c = ChromeState::new_with_handle(handle);
        apply_install_event(
            &c,
            InstallEvent {
                package_id: "dev.qos.word-processor".into(),
                display_name: "Word Processor".into(),
                kind: InstallEventKind::Installed,
            },
        );
        let st = c.borrow();
        let n = st.notifications().last().expect("the launch attempt reports");
        assert_eq!(n.title, "Couldn't open Word Processor");
        assert!(n.body.contains("not installed"), "{}", n.body);
    }
}
