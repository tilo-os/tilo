//! "Add \<name\> to apps" for each downloaded program running now that has no
//! entry in the person's apps ([`crate::shell::downloaded_apps`]): the way
//! to add one after closing the question its first run asked, or after
//! saying no to it.
//!
//! Ids: `add-downloaded-app:<surface id>`, the window it showed.

use std::rc::Rc;

use crate::shell::add_app_sheet::Answer;
use crate::shell::chrome_state::Chrome;
use crate::shell::downloaded_apps;
use crate::shell::i18n::{t, t_args};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

pub const PROVIDER_ID: &str = "downloaded-programs";
const ADD_PREFIX: &str = "add-downloaded-app:";

#[derive(Default)]
pub struct DownloadedProgramsProvider {
    /// The rows' names, indexed while they stay the same.
    ranker: ui::search::ListIndex,
}

fn row(surface_id: u64, name: &str) -> SearchResult {
    SearchResult {
        facts: Vec::new(),
        shortcut: None,
        kind: ResourceKind::Action,
        operation_kind: tilo_actions::OperationKind::Effect,
        provider_id: PROVIDER_ID.into(),
        id: format!("{ADD_PREFIX}{surface_id}"),
        title: t_args("downloaded-app-add-row", &[("name", name.into())]),
        subtitle: None,
        badge: None,
        score: 0,
        params: Vec::new(),
        contract: Some(tilo_actions::ActionContract {
            tags: vec!["apps".into(), "downloaded".into()],
            ..tilo_actions::ActionContract::default()
        }),
        on_open: Rc::new(move |_doc, _root, _chrome: &Chrome, _args| {
            downloaded_apps::answer(&Answer::Add {
                surface_id,
                move_to_applications: false,
            });
            Ok(())
        }),
        on_invoke: None,
        on_begin_invoke: None,
    }
}

impl SearchProvider for DownloadedProgramsProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "Downloaded programs"
    }

    fn display_name(&self) -> String {
        t("search-provider-downloaded-programs")
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Action) {
            return Vec::new();
        }
        let mut rows = Vec::new();
        let mut fields = Vec::new();
        for (surface_id, candidate) in downloaded_apps::candidates_snapshot() {
            let found = row(surface_id, &candidate.identity.name);
            fields.push(
                [
                    candidate.identity.name.as_str(),
                    found.title.as_str(),
                    found.id.as_str(),
                ]
                .iter()
                .map(|name| ((*name).to_owned(), ui::search::FieldWeight::TITLE))
                .collect(),
            );
            rows.push(found);
        }
        crate::shell::search::rank_rows(&self.ranker, ctx.needle.trim(), rows, &fields)
    }
}
