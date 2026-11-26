//! Browsing what a provider offers: a paged, filtered catalogue query.
//!
//! A provider may offer many actions, some behind applications that publish a
//! live, revision-bound directory rather than a flat list. A caller never
//! pulls the whole corpus: it asks for one bounded [`CataloguePage`] at a
//! time — everything, one provider, one application's rows, the children of
//! one resource, rows matching a needle, or exactly one target re-materialized
//! at the revision it was chosen from — and follows the opaque `next_cursor`.

use serde::{Deserialize, Serialize};

use crate::api::{ActionAddress, ActionDescriptor};
use crate::contract::ActionContract;
use crate::resource::ResourcePresentation;

use super::RemoteActionTarget;

/// Largest page a provider returns, whatever a query asks for.
pub const MAX_CATALOGUE_PAGE: u32 = 256;

/// The most rows a page of a listing taken a resource at a time
/// ([`CatalogueQuery::by_resource`]) answers: a resource's rows travel as one
/// there, so a page this long still holds a couple of hundred resources.
pub const MAX_BY_RESOURCE_PAGE: u32 = 16_384;

const fn default_page() -> u32 {
    64
}

/// One exact existing resource whose inert local-view metadata is requested.
/// The transport supplies identity and current workspace authority independently.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewResourceQuery {
    /// Record-local resource address, never a display label.
    pub id: String,
    /// Expected resource kind; a different publisher cannot satisfy the lookup.
    pub kind: String,
}

/// A provider-issued navigation locator. It carries no access grant or credentials.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceViewLocator {
    /// Provider-configured HTTPS listener origin.
    pub origin: String,
    /// Current governed workspace, supplied by the provider.
    pub workspace: String,
    /// Certified provider device key as hexadecimal.
    #[serde(rename = "hostDevice")]
    pub host_device: String,
    /// Exact existing record provider checked by the provider.
    pub provider: String,
    /// The web entry base (`https://tilo.so/`, ending in `/`) the operator serves
    /// the browser bundle under; links to this provider's resources are minted
    /// under it. Absent until a compatible bundle is hosted.
    #[serde(default, rename = "webEntry", skip_serializing_if = "Option::is_none")]
    pub web_entry: Option<String>,
}

/// Exact document route whose broker-owned live publisher is requested.
/// This selects ownership, not permission: ordinary caller authorization still applies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalActionRoute {
    pub workspace: String,
    pub app_id: String,
    /// Empty names the package-primary record, never any record.
    pub document_id: String,
}

/// One bounded request for catalogue rows. Every field narrows; the empty
/// query lists the first page of everything offered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogueQuery {
    /// Provider-only lookup of one exact route's owner. Cannot accompany
    /// a provider, cursor, target, view lookup, or row filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_route: Option<CanonicalActionRoute>,
    /// Only rows from this provider (a token a previous page issued).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Only rows published by this application. It narrows the rows a provider
    /// builds and pages — before the page bound — while the providers the
    /// caller may browse are listed either way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    /// Read-only view discovery, separate from the document action catalogue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_resource: Option<ViewResourceQuery>,
    /// Only the direct children of this resource; `None` is the top level
    /// for a listing, and every level for a query that names words
    /// ([`Self::needle`]): a search finds a row wherever in the tree it
    /// stands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_resource: Option<String>,
    /// Only rows whose labels, ids or parameters contain this text
    /// (case-insensitive).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub needle: String,
    /// Only resources this names: their kind, or their label ignoring ASCII
    /// case, at the top level or, with [`Self::parent_resource`], among that
    /// resource's direct children. A provider answers it whole, wherever the
    /// resource would fall in a paged listing; it takes no cursor or needle.
    /// A provider from before parents were named answers with top-level rows,
    /// whose parent says so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_name: Option<String>,
    /// Only this exact action, re-resolved against the provider now: the
    /// check to make immediately before invoking a row from an older page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<ActionAddress>,
    /// Only the action with this id, wherever the provider publishes it: the
    /// lookup of an action named by its app alone. An agent hears here of one
    /// only the person at the publishing device takes
    /// ([`crate::Availability::shown_here`]), which no listing offers it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// The revision `cursor` (or `target`) was obtained at. A provider refuses a
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
    /// The rows each resource offers sent as one ([`CatalogueRow::offered`],
    /// [`compact`]), and a page up to [`MAX_BY_RESOURCE_PAGE`] rows long:
    /// what a listing of resources asks, which shows each resource's actions
    /// by their ids. Cursors still count the rows each stands for.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub by_resource: bool,
    /// Upper bound on the resources a page holds rows of, as well as on its
    /// rows: the page ends after the last row of that many resources. What a
    /// caller listing a number of resources asks, so that a provider builds
    /// those and not as many rows as it guessed they offer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_resources: Option<u32>,
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
            action: None,
            revision: None,
            cursor: Vec::new(),
            max_results: default_page(),
            limits: None,
            by_resource: false,
            max_resources: None,
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

    /// The resources a page may hold rows of ([`Self::max_resources`]), at
    /// least one: a page always moves the listing on.
    #[must_use]
    pub fn resource_bound(&self) -> Option<usize> {
        self.max_resources
            .map(|most| usize::try_from(most).unwrap_or(usize::MAX).max(1))
    }

    /// The page size a provider honours for this query.
    #[must_use]
    pub fn page_size(&self) -> usize {
        let most = if self.by_resource {
            MAX_BY_RESOURCE_PAGE
        } else {
            MAX_CATALOGUE_PAGE
        };
        usize::try_from(self.max_results.clamp(1, most)).unwrap_or(1)
    }
}

/// Bounds on how much work a provider spends on one page. Each is an upper
/// bound a provider may lower, never a hint to exceed.
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogueResource {
    pub id: String,
    pub kind: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<ResourcePresentation>,
}

impl CatalogueResource {
    /// Whether `name` names this resource: its id, the item part of its id
    /// (an item's id is `<kind>/<field>/<item>`, and `<item>` names it as
    /// its own id does), its kind, or its label ignoring ASCII case. An id
    /// is matched exactly, where a label is matched in any case.
    #[must_use]
    pub fn is_named(&self, name: &str) -> bool {
        let item_of_id = self.id.rsplit_once('/').map(|(_, item)| item);
        self.id == name
            || item_of_id == Some(name)
            || self.kind == name
            || self.label.eq_ignore_ascii_case(name)
    }
}

/// One row: an action on a resource, or a resource that contains more.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CatalogueRow {
    /// Provider-issued token naming the publisher; copy it into an invocation.
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
    /// For an application, the surface its signed package publishes, in the
    /// form it is kept by (`collab_types::surface::canonical`): what an agent
    /// generates its typed client from, without the app having to open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    /// The actions the resource offers, in place of a row each, on a page of
    /// a listing taken a resource at a time ([`CatalogueQuery::by_resource`]);
    /// `action` is then none. [`expand`] gives the rows back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offered: Option<Box<Offered>>,
}

/// What one resource offers, as a row of a listing taken a resource at a
/// time carries it ([`CatalogueRow::offered`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Offered {
    /// The ids of its actions, in the order its rows stood.
    pub actions: Vec<String>,
    /// The resource's semantic identity, as each of its actions carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_identity: Option<crate::ResourceSemanticIdentity>,
    /// Its actions whole that no row before it on the page described for a
    /// resource of its kind: the rows of that kind after it name them by id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub described: Vec<ActionDescriptor>,
}

impl CatalogueRow {
    /// How many rows of its listing this row stands for: one, or each action
    /// of a resource sent as one ([`compact`]). What a cursor counts.
    #[must_use]
    pub fn weight(&self) -> usize {
        self.offered
            .as_ref()
            .map_or(1, |offered| offered.actions.len().max(1))
    }
}

/// The rows `rows` stand for, each counted as its [`CatalogueRow::weight`].
#[must_use]
pub fn rows_weight(rows: &[CatalogueRow]) -> usize {
    rows.iter().map(CatalogueRow::weight).sum()
}

/// `rows` cut to the first `count` rows they stand for: a resource sent as one
/// row keeps the actions before the cut.
pub fn truncate_rows(rows: &mut Vec<CatalogueRow>, count: usize) {
    let mut kept = 0usize;
    let mut cut = rows.len();
    for (at, row) in rows.iter_mut().enumerate() {
        let left = count.saturating_sub(kept);
        if left == 0 {
            cut = at;
            break;
        }
        let weight = row.weight();
        if weight > left
            && let Some(offered) = row.offered.as_mut()
        {
            offered.actions.truncate(left);
        }
        kept = kept.saturating_add(weight.min(left));
    }
    rows.truncate(cut);
}

/// `rows` cut after the rows of their first `count` resources, each
/// resource's rows next to each other: whether any were cut.
pub fn truncate_resources(rows: &mut Vec<CatalogueRow>, count: usize) -> bool {
    let mut seen = 0usize;
    let mut last: Option<&str> = None;
    let mut cut = rows.len();
    for (at, row) in rows.iter().enumerate() {
        let id = row.resource.id.as_str();
        if last != Some(id) {
            if seen == count {
                cut = at;
                break;
            }
            seen = seen.saturating_add(1);
            last = Some(id);
        }
    }
    let cutting = cut < rows.len();
    rows.truncate(cut);
    cutting
}

/// `rows` with each resource's consecutive action rows sent as one row
/// ([`CatalogueRow::offered`]): an action is described whole the first time a
/// resource of its kind offers it on the page, and by its id after that. A row
/// naming no action is kept as it is. [`expand`] undoes it.
#[must_use]
pub fn compact(rows: Vec<CatalogueRow>) -> Vec<CatalogueRow> {
    let mut described: std::collections::HashSet<(String, String, String)> =
        std::collections::HashSet::new();
    let mut out: Vec<CatalogueRow> = Vec::new();
    for mut row in rows {
        let Some(action) = row.action.take() else {
            out.push(row);
            continue;
        };
        let first = (action.resource_kind.clone(), action.address.action.clone());
        let named = (row.provider.clone(), first.0, first.1);
        let fresh = described.insert(named);
        if let Some(last) = out.last_mut()
            && let Some(offered) = last.offered.as_mut()
            && last.resource.id == row.resource.id
            && last.parent_resource == row.parent_resource
            && last.provider == row.provider
            && last.app_id == row.app_id
        {
            last.has_children |= row.has_children;
            offered.actions.push(action.address.action.clone());
            if fresh {
                offered.described.push(action);
            }
            continue;
        }
        row.offered = Some(Box::new(Offered {
            actions: vec![action.address.action.clone()],
            semantic_identity: action.semantic_identity.clone(),
            described: if fresh { vec![action] } else { Vec::new() },
        }));
        out.push(row);
    }
    out
}

/// The rows [`compact`] sent as one, a row per action again, each action
/// described as the page first described it for a resource of its kind and
/// placed on this one. A row whose action the page never described is kept
/// as one row with no action, so nothing it lists is lost.
#[must_use]
pub fn expand(rows: Vec<CatalogueRow>) -> Vec<CatalogueRow> {
    if rows.iter().all(|row| row.offered.is_none()) {
        return rows;
    }
    let mut described: std::collections::HashMap<(String, String, String), ActionDescriptor> =
        std::collections::HashMap::new();
    let mut out = Vec::with_capacity(rows.len());
    for mut row in rows {
        let Some(offered) = row.offered.take() else {
            out.push(row);
            continue;
        };
        let Offered {
            actions,
            semantic_identity,
            described: whole,
        } = *offered;
        for action in whole {
            described.insert(
                (
                    row.provider.clone(),
                    action.resource_kind.clone(),
                    action.address.action.clone(),
                ),
                action,
            );
        }
        let mut any = false;
        for id in actions {
            let key = (row.provider.clone(), row.resource.kind.clone(), id);
            let Some(like) = described.get(&key) else {
                continue;
            };
            let mut action = like.clone();
            action.address = ActionAddress::new(row.resource.id.as_str(), &key.2);
            action.resource_label.clone_from(&row.resource.label);
            action.semantic_identity.clone_from(&semantic_identity);
            let mut each = row.clone();
            each.action = Some(action);
            out.push(each);
            any = true;
        }
        if !any {
            out.push(row);
        }
    }
    out
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
            offered: None,
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
            surface: None,
        }
    }

    /// A row offering `action` in the row it stands in: the resource of the
    /// level above it, and whether its own resource holds rows below it, so a
    /// reader that asks a level at a time knows where the listing continues.
    #[must_use]
    pub fn for_placed_action(
        provider: impl Into<String>,
        app_id: impl Into<String>,
        placed: crate::api::PlacedAction,
    ) -> Self {
        let crate::api::PlacedAction {
            parent,
            has_children,
            action,
        } = placed;
        Self {
            parent_resource: parent,
            has_children,
            ..Self::for_action(provider, app_id, action)
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

    /// Whether this row is worth returning for `needle`. A provider narrows a
    /// query to one word here; whatever it keeps is then matched more
    /// closely, so this keeps every row a word begins in any of the row's
    /// names — the action's id, label and description, its parameters, the
    /// resource's label and kind — rather than only those whose text holds
    /// the needle verbatim. A caller asking for "draw" is after
    /// "Draw a pointer path", and one asking for "scrolling" after a
    /// "scroll" the description spells as "Scroll down with `PageDown`".
    fn matches(&self, needle: &str) -> bool {
        if needle.is_empty() {
            return true;
        }
        let needle = needle.to_lowercase();
        // The same word in another inflection names the same action: a
        // caller asking for "scrolling" is after the row that spells
        // "scroll", and one asking for "draw" after "drawing".
        let stem = crate::remote::stem_word(&needle);
        let mut fields = vec![
            self.resource.label.as_str(),
            self.resource.kind.as_str(),
            self.resource.id.as_str(),
            self.app_id.as_str(),
        ];
        if let Some(action) = &self.action {
            fields.extend([action.action_label.as_str(), action.address.action.as_str()]);
            fields.extend(action.keywords.iter().map(String::as_str));
            fields.extend(action.description.as_deref());
            for param in &action.params {
                fields.extend([param.name.as_ref(), param.description.as_ref()]);
            }
        }
        fields.iter().any(|field| {
            let field = field.to_lowercase();
            field.contains(&needle)
                || field.split(|c: char| !c.is_alphanumeric()).any(|word| {
                    word.starts_with(&needle)
                        || (word.chars().count() >= 3 && crate::remote::stem_word(word) == stem)
                })
        })
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogueProvider {
    pub provider: String,
    pub app_id: String,
    /// The workspace the publisher belongs to.
    pub workspace: String,
    /// The publisher's own record, when it names one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub document_id: String,
    /// Whether it publishes a live directory worth browsing by parent.
    #[serde(default)]
    pub browsable: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CataloguePage {
    /// Opaque revision the page, its cursor and its targets belong to.
    pub revision: String,
    pub rows: Vec<CatalogueRow>,
    /// Present only on a fresh authorized view lookup with a routable provider.
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
    /// Why the app the query named could not be listed: what it keeps did not
    /// mount on this device, or the caller may not read it now. Such a page holds
    /// no rows, and a caller tells the reason rather than taking the app for one
    /// that lists nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

impl CataloguePage {
    /// Filter and page rows a provider holds in memory: the publishing
    /// application, parent, needle, exact target and the offset cursor, all
    /// against `revision`. Every filter narrows before the page bound, so a
    /// wanted row is never left out because another row came first.
    #[must_use]
    pub fn from_rows(revision: String, rows: Vec<CatalogueRow>, query: &CatalogueQuery) -> Self {
        if !query.cursor.is_empty() && query.revision.as_deref() != Some(revision.as_str()) {
            return Self::stale(revision);
        }
        let mut matching = rows.into_iter().filter(|row| query.admits(row));
        let rows: Vec<_> = matching
            .by_ref()
            .skip(query.offset())
            .take(query.page_size())
            .collect();
        let mut rows = rows;
        let mut more = matching.next().is_some();
        if let Some(most) = query.resource_bound() {
            more |= truncate_resources(&mut rows, most);
        }
        Self::window(revision, rows, query, more)
    }

    /// The page `query` asks of the actions the tree `resources` publishes, offered as
    /// `provider`: what a provider that serves one app's published tree answers. Its revision
    /// names what the tree offers, so a cursor read before the tree changed starts again.
    #[must_use]
    pub fn published(
        provider: &str,
        resources: &[crate::ResourceSpec],
        query: &CatalogueQuery,
    ) -> Self {
        let rows: Vec<CatalogueRow> = crate::api::survey_placed(resources)
            .into_iter()
            .map(|placed| CatalogueRow::for_placed_action(provider, provider, placed))
            .collect();
        let offered = serde_json::to_vec(&rows).unwrap_or_default();
        let revision = format!("{provider}:{}", blake3::hash(&offered).to_hex());
        Self::from_rows(revision, rows, query)
    }

    /// A page at `revision` whose cursor was read at another: the caller
    /// starts again from the first page.
    #[must_use]
    pub fn stale(revision: String) -> Self {
        Self {
            revision,
            stale_cursor: true,
            ..Self::default()
        }
    }

    /// A page at `revision` whose app `reason` says could not be listed:
    /// no rows, and the reason in their place.
    #[must_use]
    pub fn unavailable(revision: String, reason: String) -> Self {
        Self {
            revision,
            unavailable: Some(reason),
            ..Self::default()
        }
    }

    /// The page `query` asks for, from `rows`: the rows it admits
    /// ([`CatalogueQuery::admits`]) from its cursor on ([`CatalogueQuery::offset`]),
    /// at most [`CatalogueQuery::page_size`] of them, and whether `more` follow.
    /// What a provider that pages its rows where they are kept answers with, so
    /// that a page costs its rows and not every row there is.
    #[must_use]
    pub fn window(
        revision: String,
        rows: Vec<CatalogueRow>,
        query: &CatalogueQuery,
        more: bool,
    ) -> Self {
        let next = query.offset().saturating_add(rows_weight(&rows));
        // A listing taken a resource at a time is sent so; its cursor still
        // counts the rows each resource stands for.
        let rows = if query.by_resource {
            compact(rows)
        } else {
            rows
        };
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

impl CatalogueQuery {
    /// Whether `row` is one this query lists: under the parent asked for,
    /// of the app, view, words, name and action it names. An exact action
    /// is found wherever it stands, since its address names it alone, and so
    /// is a row whose words hold a needle asked for with no parent named: a
    /// search ranges over the whole tree, where a browse names one place.
    #[must_use]
    pub fn admits(&self, row: &CatalogueRow) -> bool {
        let named_parent = self.parent_resource.as_deref().unwrap_or("");
        let searched = self.parent_resource.is_none() && !self.needle.is_empty();
        (self.target.is_some()
            || self.action.is_some()
            || searched
            || row.parent_resource == named_parent)
            && self.app_id.as_deref().is_none_or(|app| row.app_id == app)
            && self.view_resource.as_ref().is_none_or(|wanted| {
                row.resource.id == wanted.id
                    && row.resource.kind == wanted.kind
                    && row
                        .action
                        .as_ref()
                        .is_some_and(|action| action.kind == crate::OperationKind::View)
            })
            && row.matches(&self.needle)
            && self
                .resource_name
                .as_deref()
                .is_none_or(|name| row.resource.is_named(name))
            && self
                .action
                .as_ref()
                .is_none_or(|id| row.action.as_ref().is_some_and(|a| a.address.action == *id))
            && self.target.as_ref().is_none_or(|target| {
                row.action
                    .as_ref()
                    .is_some_and(|action| action.address == *target)
            })
    }

    /// How many of the rows it admits this query's cursor is past.
    #[must_use]
    pub fn offset(&self) -> usize {
        offset_of(&self.cursor)
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
            offered: None,
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
                did: None,
                state: crate::ActionState::default(),
                semantic_identity: None,
                address: ActionAddress::new(resource, action),
                resource_kind: "notes.note".into(),
                resource_label: resource.into(),
                action_label: action.into(),
                keywords: Vec::new(),
                icon: None,
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
            surface: None,
        }
    }

    /// A page bounded in resources ends after the last row of the last one
    /// it may hold, says more follow, and its cursor goes on at the next
    /// resource's first row: three resources of two actions each, asked two
    /// at a time.
    #[test]
    fn a_page_bounded_in_resources_ends_after_its_last_resource() {
        let rows: Vec<_> = ["note:a", "note:b", "note:c"]
            .iter()
            .flat_map(|note| [row(note, Some("open"), ""), row(note, Some("archive"), "")])
            .collect();
        let query = CatalogueQuery {
            max_results: 64,
            max_resources: Some(2),
            ..CatalogueQuery::default()
        };
        let first = CataloguePage::from_rows("r1".into(), rows.clone(), &query);
        let listed: Vec<_> = first
            .rows
            .iter()
            .map(|row| row.resource.id.as_str())
            .collect();
        assert_eq!(listed, ["note:a", "note:a", "note:b", "note:b"]);
        assert!(!first.exhausted, "a third resource follows");
        let second = CataloguePage::from_rows(
            "r1".into(),
            rows,
            &CatalogueQuery {
                cursor: first.next_cursor,
                revision: Some("r1".into()),
                ..query
            },
        );
        let listed: Vec<_> = second
            .rows
            .iter()
            .map(|row| row.resource.id.as_str())
            .collect();
        assert_eq!(listed, ["note:c", "note:c"]);
        assert!(second.exhausted);
    }

    /// The row `resource` of application `app`, with a `run` action.
    fn app_row(app: &str, resource: &str) -> CatalogueRow {
        let mut row = row(resource, Some("run"), "");
        row.app_id = app.into();
        row
    }

    tilo_test_env::scenario! {
        fn a_catalogue_pages_filters_and_matches_its_rows;

        phase in_memory_rows_page_filter_and_refuse_stale_cursors;
        {
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

        phase an_exact_action_is_found_below_the_top_level;
        {
            let rows = vec![
                row("folder:a", None, ""),
                row("note:a1", Some("open"), "folder:a"),
            ];
            let target = RemoteActionTarget {
                provider: "7".into(),
                address: ActionAddress::new("note:a1", "open"),
            };
            let page =
                CataloguePage::from_rows("r1".into(), rows, &CatalogueQuery::resolve(&target, "r1"));
            assert_eq!(page.rows.len(), 1);
        }

        phase an_id_names_its_resource_and_an_items_id_part_names_the_item;
        {
            let mut item = row("tasks.issues/issues/7", Some("delete"), "");
            item.resource.label = "Fix the roof".into();
            let rows = vec![row("tracker", None, ""), item];
            let named = |name: &str| {
                CataloguePage::from_rows(
                    "r1".into(),
                    rows.clone(),
                    &CatalogueQuery {
                        resource_name: Some(name.into()),
                        ..CatalogueQuery::default()
                    },
                )
            };
            let whole = named("tasks.issues/issues/7");
            assert_eq!(whole.rows.len(), 1, "the id names the item");
            let part = named("7");
            assert_eq!(part.rows.len(), 1, "the item part of the id names it");
            assert_eq!(part.rows[0].resource.id, "tasks.issues/issues/7");
            assert!(named("issue").rows.is_empty(), "not the id's middle");
        }

        phase an_app_filter_narrows_before_the_page_bound;
        {
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

        phase an_app_filter_is_optional_on_the_wire;
        {
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

        phase a_needle_matches_an_inflection_of_a_described_word;
        {
            let mut scroll = row("page:1", Some("press-page"), "");
            if let Some(action) = scroll.action.as_mut() {
                action.description =
                    Some("Scroll down the page with PageDown, or Space for a screenful.".into());
            }
            let mut draw = row("page:1", Some("pointer-path"), "");
            if let Some(action) = draw.action.as_mut() {
                action.description = Some("Moves the pointer as drawing a signature does.".into());
            }
            let rows = vec![draw, scroll];
            let actions = |needle: &str| {
                CataloguePage::from_rows(
                    "r1".into(),
                    rows.clone(),
                    &CatalogueQuery {
                        needle: needle.into(),
                        ..CatalogueQuery::default()
                    },
                )
                .rows
                .iter()
                .filter_map(|row| row.action.as_ref().map(|a| a.address.action.clone()))
                .collect::<Vec<_>>()
            };
            // The word the caller types and the word the description spells are
            // the same word in another inflection.
            assert_eq!(actions("scrolling"), ["press-page"]);
            assert_eq!(actions("draw"), ["pointer-path"]);
            assert_eq!(actions("drawing"), ["pointer-path"]);
            // A word nothing spells still matches nothing.
            assert!(actions("printing").is_empty());
        }

        phase a_needle_matches_parameter_names_and_descriptions;
        {
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

        phase a_needle_searches_every_level_of_the_tree_and_a_listing_names_one;
        {
            // A row standing under a parent, as a workspace's people name
            // themselves on the tree's own members path.
            let nested = row("workspace/members", Some("name"), "workspace");
            let root = row("workspace", Some("publish"), "");
            let rows = vec![root, nested];
            let listed = |query: CatalogueQuery| {
                CataloguePage::from_rows("r1".into(), rows.clone(), &query)
                    .rows
                    .iter()
                    .filter_map(|row| row.action.as_ref().map(|a| a.address.action.clone()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                listed(CatalogueQuery::default()),
                ["publish"],
                "a listing with nothing named is the top level"
            );
            assert_eq!(
                listed(CatalogueQuery {
                    parent_resource: Some("workspace".into()),
                    ..CatalogueQuery::default()
                }),
                ["name"],
                "a listing that names a place is that place's children"
            );
            assert_eq!(
                listed(CatalogueQuery {
                    needle: "nam".into(),
                    ..CatalogueQuery::default()
                }),
                ["name"],
                "a search finds a row under a parent with no parent named"
            );
            assert_eq!(
                listed(CatalogueQuery {
                    needle: "publish".into(),
                    parent_resource: Some("workspace".into()),
                    ..CatalogueQuery::default()
                }),
                Vec::<String>::new(),
                "a needle asked inside a named place keeps to that place"
            );
        }

        phase a_listing_by_resource_sends_each_resource_once_and_counts_its_rows;
        {
            // Three notes of one kind, each offering the same two actions, and a
            // folder that names none.
            let mut rows = Vec::new();
            for note in ["note:1", "note:2", "note:3"] {
                rows.push(row(note, Some("rename"), ""));
                rows.push(row(note, Some("archive"), ""));
            }
            rows.push(row("folder:a", None, ""));
            let sent = compact(rows.clone());
            assert_eq!(sent.len(), 4, "a row per resource");
            assert_eq!(rows_weight(&sent), rows.len(), "counting every row");
            let described: Vec<usize> = sent
                .iter()
                .map(|row| row.offered.as_ref().map_or(0, |offered| offered.described.len()))
                .collect();
            assert_eq!(described, [2, 0, 0, 0], "each action described once a page");
            assert_eq!(expand(sent.clone()), rows, "the rows come back as they were");

            // A page cut inside a resource keeps the actions before the cut.
            let mut cut = sent;
            truncate_rows(&mut cut, 3);
            assert_eq!(rows_weight(&cut), 3);
            assert_eq!(expand(cut), rows[..3].to_vec());

            // A page asked a resource at a time is sent so, and its cursor
            // counts the rows: the next page starts where these end.
            let query = CatalogueQuery {
                max_results: 4,
                by_resource: true,
                ..CatalogueQuery::default()
            };
            let page = CataloguePage::from_rows("r1".into(), rows.clone(), &query);
            assert_eq!(page.rows.len(), 2);
            let next = CataloguePage::from_rows(
                "r1".into(),
                rows.clone(),
                &CatalogueQuery {
                    cursor: page.next_cursor.clone(),
                    revision: Some("r1".into()),
                    ..query
                },
            );
            let mut both = expand(page.rows);
            both.extend(expand(next.rows));
            assert_eq!(both, rows, "the two pages hold every row once");
        }
    }
}
