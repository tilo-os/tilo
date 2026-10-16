//! Applications of the manifest registry this host cannot run ("Tilo OS
//! only" on a Mac), listed in search and the launcher's browse set with the
//! reason instead of being missing. Ranking and presentation are the portable
//! desktop search's, so the browser desktop lists the same rows.

use tilo_desktop_search::Source as _;
use tilo_desktop_search::availability::unavailable_on;
use tilo_desktop_search::providers::Applications;

use crate::shell::search::action_api::ActionError;
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

/// The platforms this build of the shell provides, as manifests name them.
fn host_platforms() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        &["macos"]
    } else {
        &["tilo-os", "linux"]
    }
}

pub struct UnavailableApplicationsProvider {
    apps: Applications,
}

impl UnavailableApplicationsProvider {
    /// The registry's apps this host cannot run.
    #[must_use]
    pub fn for_host() -> Self {
        Self::on(host_platforms())
    }

    /// The registry's apps that run on none of `host`.
    #[must_use]
    pub fn on(host: &[&str]) -> Self {
        Self {
            apps: Applications::new(unavailable_on(host, &[])),
        }
    }
}

impl SearchProvider for UnavailableApplicationsProvider {
    fn id(&self) -> &'static str {
        "unavailable-applications"
    }

    fn name(&self) -> &'static str {
        "Applications"
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Application]
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Application) {
            return Vec::new();
        }
        self.apps
            .query(ctx.needle)
            .into_iter()
            .map(|hit| {
                let reason = match &hit.target {
                    tilo_desktop_search::Target::Unavailable { reason, .. } => reason.clone(),
                    _ => String::new(),
                };
                let title = hit.title.clone();
                SearchResult::nullary_fallible(
                    ResourceKind::Application,
                    self.id(),
                    hit.id,
                    hit.title,
                    hit.subtitle,
                    hit.score,
                    move |_doc, _root, _chrome| {
                        Err(ActionError::Refused(format!(
                            "{title} is not available here: {reason}"
                        )))
                    },
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::search::action_api::ActionArgs;
    use crate::shell::state::WorkspaceTree;

    fn query(provider: &UnavailableApplicationsProvider, needle: &str) -> Vec<SearchResult> {
        let chrome = ChromeState::new(WorkspaceTree::bootstrap_default());
        let doc = automerge::AutoCommit::new();
        provider.query(SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle,
            wanted: &[ResourceKind::Application],
            chrome: &chrome.borrow(),
            doc: &doc,
            doc_root: &automerge::ROOT,
        })
    }

    #[test]
    fn a_mac_lists_tilo_os_apps_as_unavailable_and_opening_one_says_why() {
        let os_only = tilo_desktop_search::availability::catalog()
            .iter()
            .find(|app| app.platforms == ["tilo-os"])
            .expect("the registry has a Tilo-OS-only app");
        let desktop_only = tilo_desktop_search::availability::catalog()
            .iter()
            .find(|app| app.platforms == ["linux", "macos"])
            .expect("the registry has a desktop-only app");
        let provider = UnavailableApplicationsProvider::on(&["macos"]);
        let browse = query(&provider, "");
        assert!(browse.iter().any(|row| row.id == os_only.id));
        assert!(browse.iter().all(|row| row.id != desktop_only.id));
        let row = query(&provider, os_only.name)
            .into_iter()
            .find(|row| row.id == os_only.id)
            .expect("found by name");
        assert_eq!(row.title, os_only.name);
        assert!(
            row.subtitle.as_deref().unwrap().starts_with("Tilo OS only"),
            "{:?}",
            row.subtitle
        );
        let chrome = ChromeState::new(WorkspaceTree::bootstrap_default());
        let mut doc = automerge::AutoCommit::new();
        let error = (row.on_open)(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &chrome,
            &ActionArgs::none(),
        )
        .expect_err("an unavailable app does not open");
        assert!(error.to_string().contains("Tilo OS only"), "{error}");
    }

    #[test]
    fn tilo_os_runs_every_app_in_the_registry() {
        let provider = UnavailableApplicationsProvider::on(&["tilo-os", "linux"]);
        let web_only: Vec<_> = tilo_desktop_search::availability::catalog()
            .iter()
            .filter(|app| app.platforms == ["web"])
            .map(|app| app.id)
            .collect();
        assert!(
            query(&provider, "")
                .iter()
                .all(|row| web_only.contains(&row.id.as_str()))
        );
    }
}
