//! The index against an in-memory action host whose catalogue a test edits:
//! what it indexes, what leaves it, and what opening asks the host.

use std::sync::{Arc, Mutex};

use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueProvider, CatalogueQuery,
    CatalogueResource, CatalogueRow, InvocationReceipt, InvocationState, InvokeRequest, Pending,
    ReceiptStatus, workspace_provider,
};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, Availability, ConfirmationPolicy,
    ExecutionLocus, InputSpec, InvocationId, OperationKind, ResourcePresentation,
    ResourceViewDescriptor,
};

use crate::{EntryId, OpenError, OsSearch};

pub(crate) const ROOM: &str = "0b7d9a3e-5c1f-4a8e-9d2b-7f6e5a4c3b21";

#[derive(Default)]
pub(crate) struct Catalogue {
    pub(crate) rows: Vec<CatalogueRow>,
    unanswered: bool,
    invoked: Vec<(Caller, InvokeRequest)>,
    revision: u32,
}

/// An action host serving `Catalogue`, the way the broker pages rows.
#[derive(Clone, Default)]
pub(crate) struct Host(Arc<Mutex<Catalogue>>);

impl Host {
    pub(crate) fn edit(&self, change: impl FnOnce(&mut Catalogue)) {
        let mut catalogue = self.0.lock().unwrap();
        change(&mut catalogue);
        catalogue.revision = catalogue.revision.wrapping_add(1);
    }

    pub(crate) fn invoked(&self) -> Vec<(Caller, InvokeRequest)> {
        self.0.lock().unwrap().invoked.clone()
    }
}

impl ActionHost for Host {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        let (unanswered, revision, all) = {
            let catalogue = self.0.lock().unwrap();
            (
                catalogue.unanswered,
                catalogue.revision,
                catalogue.rows.clone(),
            )
        };
        if unanswered {
            return Pending::ready(Err(ActionHostError::Unavailable {
                reason: "test".into(),
            }));
        }
        let rows = all
            .iter()
            .filter(|row| query.provider.as_ref().is_none_or(|p| *p == row.provider))
            .cloned()
            .collect();
        let mut page = CataloguePage::from_rows(revision.to_string(), rows, &query);
        if query.provider.is_none() {
            let mut providers: Vec<CatalogueProvider> = Vec::new();
            for row in &all {
                if !providers.iter().any(|p| p.provider == row.provider) {
                    providers.push(CatalogueProvider {
                        provider: row.provider.clone(),
                        app_id: row.app_id.clone(),
                        room: ROOM.into(),
                        record_id: String::new(),
                        browsable: true,
                    });
                }
            }
            page.providers = providers;
        }
        Pending::ready(Ok(page))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let invocation = request.invocation;
        self.0
            .lock()
            .unwrap()
            .invoked
            .push((caller.clone(), request));
        Pending::ready(Ok(InvocationReceipt {
            invocation,
            status: ReceiptStatus::Accepted,
        }))
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionHostError>> {
        tilo_actions::remote::Pending::ready(Err(ActionHostError::UnknownInvocation))
    }
}

pub(crate) fn owner() -> Caller {
    Caller::local_person("owner").in_room(ROOM)
}

pub(crate) fn search(host: &Host) -> OsSearch {
    OsSearch::new(Arc::new(host.clone()), owner())
}

pub(crate) fn row(provider: &str, resource: &str, label: &str, action: &str) -> CatalogueRow {
    CatalogueRow {
        provider: provider.into(),
        app_id: "so.tilo.notes".into(),
        parent_resource: String::new(),
        resource: CatalogueResource {
            id: resource.into(),
            kind: "notes.page".into(),
            label: label.into(),
            presentation: None,
        },
        action: Some(ActionDescriptor {
            semantic_identity: None,
            address: ActionAddress::new(resource, action),
            resource_kind: "notes.page".into(),
            resource_label: label.into(),
            action_label: action.into(),
            description: None,
            locus: ExecutionLocus::ClientLocal,
            kind: OperationKind::View,
            contract: ActionContract::view(),
            input: InputSpec::None,
            params: Vec::new(),
            view: None,
        }),
        has_children: false,
        visibility: None,
    }
}

fn with_view(mut row: CatalogueRow, primary: bool) -> CatalogueRow {
    let action = row.action.as_mut().unwrap();
    action.view = Some(ResourceViewDescriptor {
        id: "editor".into(),
        label: "Editor".into(),
        description: None,
        open_action: action.address.action.clone(),
        primary,
        range: None,
        mount: None,
    });
    row
}

fn titles(search: &OsSearch, query: &str) -> Vec<String> {
    search
        .search(query, 10)
        .into_iter()
        .map(|entry| entry.title)
        .collect()
}

#[tokio::test]
async fn only_resources_with_a_quiet_opening_action_are_indexed() {
    let host = Host::default();
    host.edit(|catalogue| {
        let mut asks = row("7", "page:asks", "Asks for a name", "open");
        asks.action.as_mut().unwrap().input = InputSpec::Text {
            prompt: "Name".into(),
            placeholder: String::new(),
            collects: tilo_actions::Collects::Words,
        };
        let mut confirms = row("7", "page:confirms", "Confirms first", "open");
        confirms.action.as_mut().unwrap().contract.confirmation = ConfirmationPolicy::Confirm;
        let mut edits = row("7", "page:edits", "Edits", "open");
        edits.action.as_mut().unwrap().kind = OperationKind::Document;
        let mut disabled = row("7", "page:disabled", "Disabled", "open");
        disabled.action.as_mut().unwrap().contract.availability = Availability::Unavailable {
            code: "x".into(),
            reason: "x".into(),
        };
        let mut hidden = row("7", "page:hidden", "Offered to callers only", "open");
        hidden.action.as_mut().unwrap().contract.tags =
            vec![tilo_actions::AGENTS_AUDIENCE_TAG.into()];
        catalogue.rows = vec![
            row("7", "page:plain", "Plain view", "open"),
            row("7", "page:peek", "Peek only", "peek"),
            asks,
            confirms,
            edits,
            disabled,
            hidden,
            row(
                &workspace_provider(ROOM),
                "app.so.tilo.notes",
                "Notes",
                "open",
            ),
        ];
    });
    let search = search(&host);
    let changes = search.refresh().await.unwrap();
    let indexed: Vec<_> = changes.indexed.iter().map(|e| e.title.as_str()).collect();
    assert_eq!(indexed, ["Plain view"]);
    assert!(changes.removed.is_empty());
}

#[tokio::test]
async fn a_declared_primary_view_is_the_opening_action_and_trees_are_walked() {
    let host = Host::default();
    host.edit(|catalogue| {
        let mut folder = row("7", "folder:1", "Folder", "open");
        folder.action = None;
        folder.has_children = true;
        let mut child = with_view(row("7", "page:1", "Travel plans", "edit"), true);
        child.parent_resource = "folder:1".into();
        let mut child_open = row("7", "page:1", "Travel plans", "open");
        child_open.parent_resource = "folder:1".into();
        let mut secondary = with_view(row("7", "page:1", "Travel plans", "outline"), false);
        secondary.parent_resource = "folder:1".into();
        catalogue.rows = vec![folder, child_open, secondary, child];
    });
    let search = search(&host);
    let changes = search.refresh().await.unwrap();
    assert_eq!(changes.indexed.len(), 1, "{changes:?}");
    assert_eq!(
        changes.indexed[0].target.address,
        ActionAddress::new("page:1", "edit"),
        "the primary view outranks `open` and a secondary view"
    );
}

#[tokio::test]
async fn revoked_or_closed_resources_leave_the_index_and_open_nothing() {
    let host = Host::default();
    host.edit(|catalogue| {
        catalogue.rows = vec![
            row("7", "page:1", "Travel plans", "open"),
            row("7", "page:2", "Tax return", "open"),
        ];
    });
    let search = search(&host);
    search.refresh().await.unwrap();
    let tax = search.search("tax", 5).pop().unwrap();

    // The grant is revoked: the catalogue offers the owner one page less.
    host.edit(|catalogue| catalogue.rows.retain(|r| r.resource.id != "page:2"));
    // Before the next walk the index still names it, but opening rechecks.
    assert_eq!(
        search.open(&tax.id.to_string()).await,
        Err(OpenError::NotOffered)
    );
    assert!(host.invoked().is_empty(), "nothing ran");

    let changes = search.refresh().await.unwrap();
    assert_eq!(changes.removed, [tax.id]);
    assert!(changes.indexed.is_empty());
    assert!(titles(&search, "tax").is_empty());
    assert!(search.entry(&tax.id.to_string()).is_none());

    // The app closes without keeping its actions launchable.
    host.edit(|catalogue| catalogue.rows.clear());
    assert_eq!(search.refresh().await.unwrap().removed.len(), 1);
    assert!(titles(&search, "travel").is_empty());
}

#[tokio::test]
async fn opening_invokes_the_exact_action_as_the_owner_through_the_host() {
    let host = Host::default();
    host.edit(|catalogue| catalogue.rows = vec![row("7", "page:1", "Travel plans", "open")]);
    let search = search(&host);
    search.refresh().await.unwrap();
    let travel = search.search("travel", 5).pop().unwrap();

    // The app restarts as another publisher: nothing a host shows changed.
    host.edit(|catalogue| catalogue.rows = vec![row("9", "page:1", "Travel plans", "open")]);
    assert!(search.refresh().await.unwrap().is_empty());

    search.open(&travel.id.to_string()).await.unwrap();
    let invoked = host.invoked();
    assert_eq!(invoked.len(), 1);
    let (caller, request) = &invoked[0];
    assert_eq!(*caller, owner());
    assert_eq!(
        request.target.provider, "9",
        "the publisher offering it now"
    );
    assert_eq!(request.target.address, ActionAddress::new("page:1", "open"));
    assert_eq!(request.revision.as_deref(), Some("2"));
    assert!(request.confirmation.is_none(), "never fabricated");

    for foreign in [
        "",
        "page:1",
        "ffffffffffffffff",
        &travel.id.to_string().to_uppercase(),
    ] {
        assert_eq!(search.open(foreign).await, Err(OpenError::NotOffered));
    }
    assert_eq!(host.invoked().len(), 1);
}

#[tokio::test]
async fn host_searches_see_changes_to_what_they_show_only() {
    let host = Host::default();
    host.edit(|catalogue| catalogue.rows = vec![row("7", "page:1", "Travel plans", "open")]);
    let search = search(&host);
    let first = search.refresh().await.unwrap();
    assert!(search.refresh().await.unwrap().is_empty(), "unchanged");

    host.edit(|catalogue| {
        catalogue.rows[0].resource.label = "Summer travel".into();
        catalogue.rows[0].resource.presentation = Some(ResourcePresentation {
            detail: Some("Shared with Ada".into()),
            badge: None,
            preview: None,
        });
    });
    let renamed = search.refresh().await.unwrap();
    assert_eq!(renamed.indexed.len(), 1);
    assert_eq!(renamed.indexed[0].id, first.indexed[0].id, "same resource");
    assert_eq!(
        renamed.indexed[0].detail.as_deref(),
        Some("Shared with Ada")
    );
    assert_eq!(titles(&search, "summer"), ["Summer travel"]);
    assert_eq!(titles(&search, "ada"), ["Summer travel"]);
}

#[tokio::test]
async fn titles_rank_as_the_palette_ranks_them() {
    let host = Host::default();
    host.edit(|catalogue| {
        catalogue.rows = vec![
            row("7", "page:1", "Notes on atlas moths", "open"),
            row("7", "page:2", "Atlas", "open"),
            row("7", "page:3", "Groceries", "open"),
        ];
    });
    let search = search(&host);
    search.refresh().await.unwrap();
    assert_eq!(titles(&search, "atlas"), ["Atlas", "Notes on atlas moths"]);
    assert!(titles(&search, "   ").is_empty());
}

#[tokio::test]
async fn an_unanswered_walk_leaves_the_index_as_it_was() {
    let host = Host::default();
    host.edit(|catalogue| catalogue.rows = vec![row("7", "page:1", "Travel plans", "open")]);
    let search = search(&host);
    search.refresh().await.unwrap();
    host.edit(|catalogue| catalogue.unanswered = true);
    assert!(search.refresh().await.is_err());
    assert_eq!(titles(&search, "travel"), ["Travel plans"]);
    let id: EntryId = search.search("travel", 1)[0].id;
    assert!(matches!(
        search.open(&id.to_string()).await,
        Err(OpenError::Host(ActionHostError::Unavailable { .. }))
    ));
}
