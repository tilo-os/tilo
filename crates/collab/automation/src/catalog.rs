//! Finding the published action a step names.
//!
//! The checker and the interpreter resolve a step the same way, against
//! catalogue rows a host returned from `query`: the checker against rows
//! gathered when the shortcut is saved, the interpreter against rows it
//! fetches immediately before each invocation.

use serde_json::{Map, Value};
use tilo_actions::params::ActionParam;
use tilo_actions::remote::CatalogueRow;

/// A step's resource, once any binding in its selector is evaluated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceTarget<'a> {
    /// Exactly this resource id.
    Id(&'a str),
    /// The first resource whose kind is this kind or a sub-kind of it.
    Kind(&'a str),
    /// Any resource that offers the action. Only the checker uses this, for
    /// a resource address that is not known until the run.
    Any,
}

/// Whether resource kind `got` is `want` or one of its sub-kinds:
/// `files.entry` matches `files.entry` and `files.entry.image`, never
/// `files.entryway`.
#[must_use]
pub fn kind_matches(want: &str, got: &str) -> bool {
    got == want
        || got
            .strip_prefix(want)
            .is_some_and(|rest| rest.starts_with(['.', ':', '/']))
}

/// The input through which a shortcut whose `run` takes `params` runs as a
/// quick action on a resource of kind `kind`: its first parameter, when that
/// takes resources of `kind` or a parent kind
/// ([`ActionParam::resource_kind`]) and nothing else is required.
#[must_use]
pub fn quick_action_input<'p>(params: &'p [ActionParam], kind: &str) -> Option<&'p ActionParam> {
    let (first, rest) = params.split_first()?;
    let wanted = first.resource_kind()?;
    (kind_matches(wanted, kind) && rest.iter().all(|param| !param.required)).then_some(first)
}

/// The arguments that run a quick action with `resource` bound to `input`.
#[must_use]
pub fn quick_action_arguments(input: &ActionParam, resource: &str) -> Map<String, Value> {
    let mut arguments = Map::new();
    arguments.insert(input.name.to_string(), Value::String(resource.to_string()));
    arguments
}

/// The first row among `rows` that offers `action` on `app_id` at `target`.
pub(crate) fn find_row<'r>(
    rows: impl IntoIterator<Item = &'r CatalogueRow>,
    app_id: &str,
    target: ResourceTarget<'_>,
    action: &str,
) -> Option<&'r CatalogueRow> {
    rows.into_iter().find(|row| {
        row.app_id == app_id
            && row
                .action
                .as_ref()
                .is_some_and(|descriptor| descriptor.address.action == action)
            && match target {
                ResourceTarget::Id(id) => row.resource.id == id,
                ResourceTarget::Kind(kind) => kind_matches(kind, &row.resource.kind),
                ResourceTarget::Any => true,
            }
    })
}

/// Descriptors known at save time: the rows one or more `query` pages
/// returned, as the invoking person sees them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActionCatalog {
    rows: Vec<CatalogueRow>,
}

impl ActionCatalog {
    #[must_use]
    pub fn new(rows: Vec<CatalogueRow>) -> Self {
        Self { rows }
    }

    /// Add the rows of one more catalogue page.
    pub fn extend(&mut self, rows: impl IntoIterator<Item = CatalogueRow>) {
        self.rows.extend(rows);
    }

    #[must_use]
    pub fn rows(&self) -> &[CatalogueRow] {
        &self.rows
    }

    /// The row offering `action` on `app_id` at `target`.
    #[must_use]
    pub fn find(
        &self,
        app_id: &str,
        target: ResourceTarget<'_>,
        action: &str,
    ) -> Option<&CatalogueRow> {
        find_row(&self.rows, app_id, target, action)
    }

    /// Every row offering `action` on `app_id`, whatever the resource.
    pub fn offering<'s>(
        &'s self,
        app_id: &'s str,
        action: &'s str,
    ) -> impl Iterator<Item = &'s CatalogueRow> + 's {
        self.rows.iter().filter(move |row| {
            row.app_id == app_id
                && row
                    .action
                    .as_ref()
                    .is_some_and(|descriptor| descriptor.address.action == action)
        })
    }
}
