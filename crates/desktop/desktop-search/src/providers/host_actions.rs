//! The actions the workspace's action catalogue offers, as the shell's search lists them: the
//! same rows `tilo do` and a page's catalogue query read, so a person finds on their screen what
//! an agent finds over the wire.
//!
//! A device's own sources cover what this machine holds: the actions its open windows publish and
//! those of the apps it runs. An app that runs on the workspace's host (a hosted workspace's Linux
//! apps), or a record the workspace keeps for every app of it, is not in any of those, though the
//! workspace's catalogue offers it. This source asks that catalogue, over the session the device
//! already holds, and lists what it answers under the row the action's own app would give it.
//!
//! The answer comes over the wire, so a query asks and returns at once; the answer is kept for
//! that query, wakes the shell, and the next pass lists it ([`Source::poll`]).
//!
//! Rows the device's own sources already list are left out: the catalogue speaks for the whole
//! workspace, and an action this device publishes twice is one row.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, CatalogueRow, Pending,
    application_provider, application_provider_resource, provider_workspace,
};
use ui::search::system::ResourceKind;
use ui::time::{Duration, Instant};

use crate::model::{Hit, InvokeTarget, Source, Target};
use crate::rank;

/// How long a search waits on the catalogue's answer before it no longer counts it as coming.
pub const PATIENCE: Duration = Duration::from_secs(2);

/// How many rows of the catalogue one query asks for.
const MAX_ROWS: u32 = 40;

/// What the source last asked, and the catalogue's answer to it.
#[derive(Default)]
struct Asked {
    /// The query asked, as the source was asked it.
    needle: String,
    /// When it was asked.
    at: Option<Instant>,
    /// The catalogue's rows, once they came: empty when it refused.
    rows: Option<Vec<CatalogueRow>>,
    /// Whether the answer came since the palette last looked.
    fresh: bool,
}

/// The workspace's action catalogue, as one of a device's search sources.
pub struct HostActions {
    catalogue: Arc<dyn ActionProvider + Send + Sync>,
    caller: Caller,
    workspace: String,
    /// The addresses this device's own sources already list, so the catalogue's copy is not a
    /// second row.
    listed: Arc<HashSet<ActionAddress>>,
    /// What asks the device's shell for the pass that lists an answer.
    wake: Arc<dyn Fn() + Send + Sync>,
    asked: Arc<Mutex<Asked>>,
}

impl HostActions {
    /// `catalogue`, the action provider of `workspace`, asked as `caller`; `wake` asks for the
    /// pass that lists its answer, and `listed` are the addresses other sources already list.
    #[must_use]
    pub fn new(
        catalogue: Arc<dyn ActionProvider + Send + Sync>,
        caller: Caller,
        workspace: impl Into<String>,
        listed: Arc<HashSet<ActionAddress>>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            catalogue,
            caller,
            workspace: workspace.into(),
            listed,
            wake,
            asked: Arc::default(),
        }
    }

    fn asked(&self) -> std::sync::MutexGuard<'_, Asked> {
        self.asked.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The row of `row`, when it names an action a person may run: it runs the action at its own
    /// address in the app that publishes it, as `tilo do` does, and reads as the app's own row.
    fn hit(&self, needle: &str, row: &CatalogueRow) -> Option<Hit> {
        let action = row.action.as_ref()?;
        if !action.contract.a_person_can_take_now() || self.listed.contains(&action.address) {
            return None;
        }
        let score = rank::action_match_score(
            needle,
            &action.action_label,
            &action.keywords,
            &row.resource.label,
        )?;
        // The app and the workspace the action belongs to: the provider names both, and its
        // record when the action lives in one the app keeps for the workspace.
        let (workspace, app) = application_provider(&row.provider)
            .map(|(workspace, app)| (workspace.to_owned(), app.to_owned()))
            .or_else(|| {
                provider_workspace(&row.provider)
                    .map(|workspace| (workspace.to_owned(), row.app_id.clone()))
            })
            .unwrap_or_else(|| (self.workspace.clone(), row.app_id.clone()));
        let record = application_provider_resource(&row.provider).unwrap_or_default();
        let target = InvokeTarget {
            record: record.to_owned(),
            descriptor: Some(action.clone()),
            arguments: serde_json::Map::new(),
            ..InvokeTarget::action(
                &app,
                &workspace,
                row.resource.id.as_str(),
                action.address.action.as_str(),
            )
        };
        Some(Hit {
            row: None,
            preview: None,
            faces: Vec::new(),
            outcome: None,
            kind: ResourceKind::Action,
            provider: self.id().into(),
            id: format!("{}\u{1f}{}", row.provider, action.address.encode()),
            title: action.action_label.clone(),
            subtitle: crate::availability::name_for(&row.app_id).map(str::to_owned),
            facts: Vec::new(),
            score,
            icon: action
                .icon
                .as_deref()
                .and_then(tilo_ui_base::icons::Icon::from_name),
            target: Target::Invoke(Box::new(target)),
        })
    }
}

impl Source for HostActions {
    fn id(&self) -> &'static str {
        "workspace-actions"
    }

    fn name(&self) -> String {
        crate::i18n::t("search-source-workspace-actions")
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        let needle = needle.trim();
        if needle.is_empty() {
            return Vec::new();
        }
        let mut asked = self.asked();
        if asked.needle == needle {
            return asked
                .rows
                .iter()
                .flatten()
                .filter_map(|row| self.hit(needle, row))
                .collect();
        }
        *asked = Asked {
            needle: needle.to_owned(),
            at: Some(ui::time::now()),
            rows: None,
            fresh: false,
        };
        drop(asked);
        let answer = Arc::clone(&self.asked);
        let wake = Arc::clone(&self.wake);
        let asking = needle.to_owned();
        let query = CatalogueQuery {
            needle: needle.to_owned(),
            max_results: MAX_ROWS,
            ..CatalogueQuery::default()
        };
        let asked_now: Pending<Result<CataloguePage, ActionCallError>> =
            self.catalogue.query(&self.caller, query);
        asked_now.then(
            move |page: Option<Result<CataloguePage, ActionCallError>>| {
                let mut asked = answer.lock().unwrap_or_else(PoisonError::into_inner);
                // An answer to a query typed over since is no row of the one shown.
                if asked.needle != asking {
                    return;
                }
                asked.rows = Some(
                    page.and_then(Result::ok)
                        .map(|page| page.rows)
                        .unwrap_or_default(),
                );
                asked.fresh = true;
                drop(asked);
                wake();
            },
        );
        Vec::new()
    }

    fn poll(&self) -> bool {
        std::mem::take(&mut self.asked().fresh)
    }

    fn waiting(&self) -> bool {
        let asked = self.asked();
        asked.rows.is_none()
            && asked
                .at
                .is_some_and(|at| ui::time::now().saturating_duration_since(at) < PATIENCE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::remote::{
        ActionCallError, CatalogueResource, InvocationReceipt, InvocationState, InvokeRequest,
        Pending, item_param,
    };
    use tilo_actions::{
        ActionKind, ActionParam, ExecutionLocus, InvocationId, OperationKind, ParamType,
    };

    /// A catalogue that answers one page of `rows`, and invokes nothing.
    struct Standing(Vec<CatalogueRow>);

    impl ActionProvider for Standing {
        fn query(
            &self,
            _: &Caller,
            query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionCallError>> {
            Pending::ready(Ok(CataloguePage::from_rows(
                "0".to_owned(),
                self.0.clone(),
                &query,
            )))
        }

        fn invoke(
            &self,
            _: &Caller,
            _: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
            Pending::ready(Err(ActionCallError::Unavailable {
                reason: "this test invokes nothing".to_owned(),
            }))
        }

        fn observe(&self, _: &Caller, _: &InvocationId) -> Option<InvocationState> {
            None
        }

        fn cancel(&self, _: &Caller, _: &InvocationId) -> Pending<Result<(), ActionCallError>> {
            Pending::ready(Ok(()))
        }
    }

    /// Terminal's machine-secrets root as the workspace's catalogue offers its `add_secret`,
    /// with its two parameters, labelled as the app's own code labels them.
    fn machine_secrets() -> CatalogueRow {
        let offer = tilo_actions::published::Offer {
            resource: "tl:so.tilo.terminal:root-tl.so.tilo.terminal.MachineSecrets",
            resource_kind: "tl.so.tilo.terminal.MachineSecrets",
            action: "add_secret",
            label: "Add secret",
            description: None,
            kind: ActionKind::Undoable,
            operation: OperationKind::Document,
            confirm: false,
            asks: Vec::new(),
            keywords: vec!["secret", "machine secret", "vault"],
            icon: None,
        };
        let mut published = tilo_actions::published::PublishedAction::offered(&offer, "Terminal");
        published.params = vec![
            ActionParam::required("variable", ParamType::Text, "Variable"),
            item_param("item", "Vault item"),
        ];
        CatalogueRow {
            provider: "application:ws-1:so.tilo.terminal".to_owned(),
            app_id: "so.tilo.terminal".to_owned(),
            parent_resource: String::new(),
            resource: CatalogueResource {
                id: "tl:so.tilo.terminal:root-tl.so.tilo.terminal.MachineSecrets".to_owned(),
                kind: "tl.so.tilo.terminal.MachineSecrets".to_owned(),
                label: "Machine secrets".to_owned(),
                presentation: None,
            },
            action: Some(published.descriptor(ExecutionLocus::ClientLocal)),
            has_children: false,
            visibility: None,
            surface: None,
            offered: None,
        }
    }

    fn source(rows: Vec<CatalogueRow>, listed: HashSet<ActionAddress>) -> HostActions {
        HostActions::new(
            Arc::new(Standing(rows)),
            Caller::local_person("ada").in_workspace("ws-1"),
            "ws-1",
            Arc::new(listed),
            Arc::new(|| {}),
        )
    }

    #[test]
    fn the_workspace_catalogue_offers_add_secret_with_its_labelled_params() {
        let source = source(vec![machine_secrets()], HashSet::new());
        assert!(
            source.query("secret").is_empty(),
            "the catalogue is asked, and its answer lists on the next pass"
        );
        assert!(source.poll(), "the answer woke the shell");
        let hits = source.query("secret");
        assert_eq!(hits.len(), 1, "one row: {hits:?}");
        let hit = &hits[0];
        assert_eq!(hit.title, "Add secret");
        let Target::Invoke(target) = &hit.target else {
            panic!("the row runs the action by address");
        };
        assert_eq!(target.app, "so.tilo.terminal");
        assert_eq!(target.workspace, "ws-1");
        assert_eq!(
            target.address.resource.as_str(),
            "tl:so.tilo.terminal:root-tl.so.tilo.terminal.MachineSecrets"
        );
        assert_eq!(target.address.action.as_str(), "add_secret");
        let descriptor = target
            .descriptor
            .as_ref()
            .expect("the row carries what the action asks");
        let labelled: Vec<(&str, &str)> = descriptor
            .params
            .iter()
            .map(|param| (param.name.as_ref(), param.description.as_ref()))
            .collect();
        assert_eq!(
            labelled,
            vec![("variable", "Variable"), ("item", "Vault item")],
            "each parameter is labelled for the person filling it"
        );
    }

    #[test]
    fn a_query_by_any_of_its_words_finds_it() {
        for word in ["machine secret", "vault", "add secret"] {
            let source = source(vec![machine_secrets()], HashSet::new());
            source.query(word);
            assert!(source.poll());
            assert_eq!(source.query(word).len(), 1, "found by {word:?}");
        }
    }

    #[test]
    fn a_row_this_device_already_lists_is_not_a_second_one() {
        let row = machine_secrets();
        let address = row.action.as_ref().expect("an action").address.clone();
        let source = source(vec![row], HashSet::from([address]));
        source.query("secret");
        assert!(source.poll());
        assert!(
            source.query("secret").is_empty(),
            "the local source already lists it"
        );
    }
}
