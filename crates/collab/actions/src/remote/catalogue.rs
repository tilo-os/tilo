//! Browsing what a host offers: a paged, filtered catalogue query.
//!
//! A host may offer many actions, some behind applications that publish a
//! live, revision-bound directory rather than a flat list. A caller never
//! pulls the whole corpus: it asks for one bounded [`CataloguePage`] at a
//! time — everything, one provider, one application's rows, the children of
//! one resource, rows matching a needle, or exactly one target re-materialized
//! at the revision it was chosen from — and follows the opaque `next_cursor`.

use serde::{Deserialize, Serialize};

use crate::contract::ActionContract;
use crate::door::{ActionAddress, ActionDescriptor};
use crate::resource::ResourcePresentation;

use super::RemoteActionTarget;

/// Largest page a host returns, whatever a query asks for.
pub const MAX_CATALOGUE_PAGE: u32 = 256;

const fn default_page() -> u32 {
    64
}

/// One exact existing resource whose inert local-view metadata is requested.
/// The transport supplies identity and current room authority independently.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewResourceQuery {
    /// Record-local resource address, never a display label.
    pub id: String,
    /// Expected resource kind; a different publisher cannot satisfy the lookup.
    pub kind: String,
}

/// A host-issued navigation locator. It carries no access grant or credentials.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceViewLocator {
    /// Host-configured HTTPS listener origin.
    pub origin: String,
    /// Current governed room, supplied by the host.
    pub room: String,
    /// Certified host device key as hexadecimal.
    #[serde(rename = "hostDevice")]
    pub host_device: String,
    /// Exact existing record provider checked by the host.
    pub provider: String,
    /// The web entry base (`https://tilo.so/`, ending in `/`) the operator serves
    /// the browser bundle under; links to this host's resources are minted
    /// under it. Absent until a compatible bundle is hosted.
    #[serde(default, rename = "webEntry", skip_serializing_if = "Option::is_none")]
    pub web_entry: Option<String>,
}

/// Exact document route whose broker-owned live publisher is requested.
/// This selects custody, not permission: ordinary caller authorization still applies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalActionRoute {
    pub room: String,
    pub app_id: String,
    /// Empty names the package-primary record, never any record.
    pub record_id: String,
}

/// One bounded request for catalogue rows. Every field narrows; the empty
/// query lists the first page of everything offered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogueQuery {
    /// Provider-only lookup of one exact route's custodian. Cannot accompany
    /// a provider, cursor, target, view lookup, or row filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_route: Option<CanonicalActionRoute>,
    /// Only rows from this provider (a token a previous page issued).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Only rows published by this application. It narrows the rows a host
    /// builds and pages — before the page bound — while the providers the
    /// caller may browse are listed either way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    /// Read-only view discovery, separate from the document action catalogue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_resource: Option<ViewResourceQuery>,
    /// Only the direct children of this resource; `None` is the top level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_resource: Option<String>,
    /// Only rows whose labels, ids or parameters contain this text
    /// (case-insensitive).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub needle: String,
    /// Only top-level resources this names: their kind, or their label
    /// ignoring ASCII case. A host answers it whole, wherever the resource
    /// would fall in a paged listing; it takes no cursor, needle or parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_name: Option<String>,
    /// Only this exact action, re-resolved against the provider now: the
    /// check to make immediately before invoking a row from an older page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<ActionAddress>,
    /// The revision `cursor` (or `target`) was obtained at. A host refuses a
    /// cursor from another revision as stale rather than skipping rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// Opaque continuation from the previous page; empty starts over.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cursor: Vec<u8>,
    /// Upper bound on rows, clamped to [`MAX_CATALOGUE_PAGE`]. With no
    /// provider named, zero asks only for [`CataloguePage::providers`].
    #[serde(default = "default_page")]
    pub max_results: u32,
    /// Optional upper bounds on the work a provider spends answering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<CatalogueLimits>,
}

impl Default for CatalogueQuery {
    fn default() -> Self {
        Self {
            canonical_route: None,
            provider: None,
            app_id: None,
            view_resource: None,
            parent_resource: None,
            needle: String::new(),
            resource_name: None,
            target: None,
            revision: None,
            cursor: Vec::new(),
            max_results: default_page(),
            limits: None,
        }
    }
}

impl CatalogueQuery {
    /// A query that re-resolves exactly `target` at `revision`.
    #[must_use]
    pub fn resolve(target: &RemoteActionTarget, revision: impl Into<String>) -> Self {
        Self {
            provider: Some(target.provider.clone()),
            target: Some(target.address.clone()),
            revision: Some(revision.into()),
            max_results: 1,
            ..Self::default()
        }
    }

    /// The page size a host honours for this query.
    #[must_use]
    pub fn page_size(&self) -> usize {
        usize::try_from(self.max_results.clamp(1, MAX_CATALOGUE_PAGE)).unwrap_or(1)
    }
}

/// Bounds on how much work a provider spends on one page. Each is an upper
/// bound a host may lower, never a hint to exceed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogueLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_work: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u16>,
}

/// A resource as a catalogue row describes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogueResource {
    pub id: String,
    pub kind: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<ResourcePresentation>,
}

impl CatalogueResource {
    /// Whether `name` names this resource: its kind, or its label ignoring
    /// ASCII case.
    #[must_use]
    pub fn is_named(&self, name: &str) -> bool {
        self.kind == name || self.label.eq_ignore_ascii_case(name)
    }
}

/// One row: an action on a resource, or a resource that contains more.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CatalogueRow {
    /// Host-issued token naming the publisher; copy it into an invocation.
    pub provider: String,
    /// Stable id of the publishing application.
    pub app_id: String,
    /// The resource this row sits under; empty at the top level.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub parent_resource: String,
    pub resource: CatalogueResource,
    /// The action this row offers; `None` for a resource-only row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<ActionDescriptor>,
    /// Whether querying with this resource as the parent yields more rows.
    #[serde(default)]
    pub has_children: bool,
    /// For a resource-only row, the permission-bearing action that
    /// discloses it, when the provider declared one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<RowVisibility>,
}

impl CatalogueRow {
    /// A top-level row offering `action` on the resource it addresses.
    #[must_use]
    pub fn for_action(
        provider: impl Into<String>,
        app_id: impl Into<String>,
        action: ActionDescriptor,
    ) -> Self {
        Self {
            provider: provider.into(),
            app_id: app_id.into(),
            parent_resource: String::new(),
            resource: CatalogueResource {
                id: action.address.resource.clone(),
                kind: action.resource_kind.clone(),
                label: action.resource_label.clone(),
                presentation: None,
            },
            action: Some(action),
            has_children: false,
            visibility: None,
        }
    }

    /// The target that invokes this row's action.
    #[must_use]
    pub fn target(&self) -> Option<RemoteActionTarget> {
        self.action.as_ref().map(|action| RemoteActionTarget {
            provider: self.provider.clone(),
            address: action.address.clone(),
        })
    }

    fn matches(&self, needle: &str) -> bool {
        if needle.is_empty() {
            return true;
        }
        let needle = needle.to_lowercase();
        let mut fields = vec![
            self.resource.label.as_str(),
            self.resource.id.as_str(),
            self.app_id.as_str(),
        ];
        if let Some(action) = &self.action {
            fields.extend([action.action_label.as_str(), action.address.action.as_str()]);
            for param in &action.params {
                fields.extend([param.name.as_ref(), param.description.as_ref()]);
            }
        }
        fields
            .iter()
            .any(|field| field.to_lowercase().contains(&needle))
    }
}

/// The action that discloses a resource-only row, and its contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowVisibility {
    pub action: String,
    pub contract: ActionContract,
    /// The operation whose contract discloses this row. Missing on legacy
    /// publications; brokers must not infer that an unknown Read is public.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub governing_kind: Option<crate::OperationKind>,
}

/// A publisher the caller may browse or invoke.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogueProvider {
    pub provider: String,
    pub app_id: String,
    /// The workspace room the publisher belongs to.
    pub room: String,
    /// The publisher's own record, when it names one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub record_id: String,
    /// Whether it publishes a live directory worth browsing by parent.
    #[serde(default)]
    pub browsable: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CataloguePage {
    /// Opaque revision the page, its cursor and its targets belong to.
    pub revision: String,
    pub rows: Vec<CatalogueRow>,
    /// Present only on a fresh authorized view lookup with a routable host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<ResourceViewLocator>,
    /// For a query naming no provider: the publishers the caller may use.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<CatalogueProvider>,
    /// Continuation for the next page; empty when there is none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_cursor: Vec<u8>,
    /// No rows remain past this page.
    #[serde(default)]
    pub exhausted: bool,
    /// The query's cursor belonged to another revision; start over.
    #[serde(default)]
    pub stale_cursor: bool,
    /// The provider stopped at a work limit, not at the end.
    #[serde(default)]
    pub budget_exhausted: bool,
}

impl CataloguePage {
    /// Filter and page rows a host holds in memory: the publishing
    /// application, parent, needle, exact target and the offset cursor, all
    /// against `revision`. Every filter narrows before the page bound, so a
    /// wanted row is never left out because another row came first.
    #[must_use]
    pub fn from_rows(revision: String, rows: Vec<CatalogueRow>, query: &CatalogueQuery) -> Self {
        if !query.cursor.is_empty() && query.revision.as_deref() != Some(revision.as_str()) {
            return Self {
                revision,
                stale_cursor: true,
                ..Self::default()
            };
        }
        let parent = query.parent_resource.as_deref().unwrap_or("");
        let mut matching = rows.into_iter().filter(|row| {
            row.parent_resource == parent
                && query.app_id.as_deref().is_none_or(|app| row.app_id == app)
                && query.view_resource.as_ref().is_none_or(|wanted| {
                    row.resource.id == wanted.id
                        && row.resource.kind == wanted.kind
                        && row
                            .action
                            .as_ref()
                            .is_some_and(|action| action.kind == crate::OperationKind::View)
                })
                && row.matches(&query.needle)
                && query
                    .resource_name
                    .as_deref()
                    .is_none_or(|name| row.resource.is_named(name))
                && query.target.as_ref().is_none_or(|target| {
                    row.action
                        .as_ref()
                        .is_some_and(|action| action.address == *target)
                })
        });
        let offset = offset_of(&query.cursor);
        let size = query.page_size();
        let rows: Vec<_> = matching.by_ref().skip(offset).take(size).collect();
        let more = matching.next().is_some();
        let next = offset.saturating_add(rows.len());
        Self {
            revision,
            rows,
            locator: None,
            next_cursor: if more {
                u64::try_from(next)
                    .unwrap_or(u64::MAX)
                    .to_le_bytes()
                    .to_vec()
            } else {
                Vec::new()
            },
            exhausted: !more,
            ..Self::default()
        }
    }
}

fn offset_of(cursor: &[u8]) -> usize {
    <[u8; 8]>::try_from(cursor)
        .map(u64::from_le_bytes)
        .ok()
        .and_then(|offset| usize::try_from(offset).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExecutionLocus, InputSpec, OperationKind};

    fn row(resource: &str, action: Option<&str>, parent: &str) -> CatalogueRow {
        CatalogueRow {
            provider: "7".into(),
            app_id: "notes".into(),
            parent_resource: parent.into(),
            resource: CatalogueResource {
                id: resource.into(),
                kind: "notes.note".into(),
                label: resource.into(),
                presentation: None,
            },
            action: action.map(|action| ActionDescriptor {
                semantic_identity: None,
                address: ActionAddress::new(resource, action),
                resource_kind: "notes.note".into(),
                resource_label: resource.into(),
                action_label: action.into(),
                description: None,
                locus: ExecutionLocus::ClientLocal,
                kind: OperationKind::Document,
                contract: ActionContract::default(),
                input: InputSpec::None,
                params: Vec::new(),
                view: None,
            }),
            has_children: action.is_none(),
            visibility: None,
        }
    }

    #[test]
    fn in_memory_rows_page_filter_and_refuse_stale_cursors() {
        let rows = (0..5)
            .map(|index| row(&format!("note:{index}"), Some("archive"), ""))
            .chain([
                row("folder:a", None, ""),
                row("note:a1", Some("open"), "folder:a"),
            ])
            .collect::<Vec<_>>();
        let first = CataloguePage::from_rows(
            "r1".into(),
            rows.clone(),
            &CatalogueQuery {
                max_results: 4,
                ..CatalogueQuery::default()
            },
        );
        assert_eq!(first.rows.len(), 4);
        assert!(!first.exhausted);
        let second = CataloguePage::from_rows(
            "r1".into(),
            rows.clone(),
            &CatalogueQuery {
                max_results: 4,
                cursor: first.next_cursor.clone(),
                revision: Some("r1".into()),
                ..CatalogueQuery::default()
            },
        );
        assert_eq!(second.rows.len(), 2, "the remaining top-level rows");
        assert!(second.exhausted);
        let stale = CataloguePage::from_rows(
            "r2".into(),
            rows.clone(),
            &CatalogueQuery {
                cursor: first.next_cursor,
                revision: Some("r1".into()),
                ..CatalogueQuery::default()
            },
        );
        assert!(stale.stale_cursor && stale.rows.is_empty());

        let named = |name: &str| {
            CataloguePage::from_rows(
                "r1".into(),
                rows.clone(),
                &CatalogueQuery {
                    resource_name: Some(name.into()),
                    max_results: 4,
                    ..CatalogueQuery::default()
                },
            )
        };
        let by_label = named("NOTE:4");
        assert_eq!(by_label.rows.len(), 1, "a label past the first page");
        assert_eq!(by_label.rows[0].resource.id, "note:4");
        assert!(named("note:a1").rows.is_empty(), "only the top level");
        assert_eq!(named("notes.note").rows.len(), 4, "a kind names each");

        let children = CataloguePage::from_rows(
            "r1".into(),
            rows.clone(),
            &CatalogueQuery {
                parent_resource: Some("folder:a".into()),
                ..CatalogueQuery::default()
            },
        );
        assert_eq!(children.rows.len(), 1);
        let needle = CataloguePage::from_rows(
            "r1".into(),
            rows.clone(),
            &CatalogueQuery {
                needle: "NOTE:3".into(),
                ..CatalogueQuery::default()
            },
        );
        assert_eq!(needle.rows.len(), 1);
        let exact = CataloguePage::from_rows(
            "r1".into(),
            rows,
            &CatalogueQuery::resolve(
                &RemoteActionTarget {
                    provider: "7".into(),
                    address: ActionAddress::new("note:2", "archive"),
                },
                "r1",
            ),
        );
        assert_eq!(exact.rows.len(), 1);
        assert_eq!(
            exact.rows[0].target().map(|target| target.address),
            Some(ActionAddress::new("note:2", "archive"))
        );
    }

    /// The row `resource` of application `app`, with a `run` action.
    fn app_row(app: &str, resource: &str) -> CatalogueRow {
        let mut row = row(resource, Some("run"), "");
        row.app_id = app.into();
        row
    }

    #[test]
    fn an_app_filter_narrows_before_the_page_bound() {
        let wanted = app_row("so.tilo.shortcuts", "shortcuts.shortcut:tidy");
        let rows = (0..MAX_CATALOGUE_PAGE)
            .map(|index| app_row("so.tilo.notes", &format!("note:{index}")))
            .chain([wanted.clone()])
            .collect::<Vec<_>>();
        let query = |app_id: Option<&str>| CatalogueQuery {
            app_id: app_id.map(str::to_owned),
            max_results: MAX_CATALOGUE_PAGE,
            ..CatalogueQuery::default()
        };
        let scoped =
            CataloguePage::from_rows("r1".into(), rows.clone(), &query(Some("so.tilo.shortcuts")));
        assert_eq!(scoped.rows, [wanted], "the one row of the app asked for");
        assert!(scoped.exhausted && scoped.next_cursor.is_empty());

        let unfiltered = CataloguePage::from_rows("r1".into(), rows, &query(None));
        assert_eq!(
            unfiltered.rows.len(),
            usize::try_from(MAX_CATALOGUE_PAGE).unwrap_or(usize::MAX)
        );
        assert!(
            unfiltered
                .rows
                .iter()
                .all(|row| row.app_id == "so.tilo.notes"),
            "the wanted row sits past the first page when nothing narrows it"
        );
        assert!(!unfiltered.exhausted);
    }

    #[test]
    fn an_app_filter_is_optional_on_the_wire() {
        let legacy: CatalogueQuery =
            serde_json::from_value(serde_json::json!({ "max_results": 4 })).unwrap();
        assert_eq!(legacy.app_id, None);
        let scoped = CatalogueQuery {
            app_id: Some("so.tilo.shortcuts".into()),
            ..CatalogueQuery::default()
        };
        assert_eq!(
            serde_json::to_value(&scoped).unwrap()["app_id"],
            "so.tilo.shortcuts"
        );
        assert!(
            serde_json::to_value(CatalogueQuery::default())
                .unwrap()
                .get("app_id")
                .is_none(),
            "a query with no app filter says nothing about one"
        );
    }

    #[test]
    fn a_needle_matches_parameter_names_and_descriptions() {
        let mut format = row("sheet:1", Some("format-cells"), "");
        if let Some(action) = format.action.as_mut() {
            action.params = vec![crate::ActionParam::optional(
                "bold",
                crate::ParamType::Boolean,
                "Heavier text",
            )];
        }
        let rows = vec![row("sheet:1", Some("sort-rows"), ""), format];
        for needle in ["BOLD", "heavier"] {
            let page = CataloguePage::from_rows(
                "r1".into(),
                rows.clone(),
                &CatalogueQuery {
                    needle: needle.into(),
                    ..CatalogueQuery::default()
                },
            );
            let actions: Vec<_> = page
                .rows
                .iter()
                .filter_map(|row| row.action.as_ref().map(|a| a.address.action.as_str()))
                .collect();
            assert_eq!(actions, ["format-cells"], "{needle}");
        }
    }
}
