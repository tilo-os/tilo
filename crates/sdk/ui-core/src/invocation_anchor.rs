//! Inert app projectors for the exact encoded action at checked dispatch.
//!
//! A projector supplies a pre-operation semantic target, never authority,
//! an applied outcome, a current activity status, or presentation evidence.
use automerge::AutoCommit;
use tilo_actions::{
    ActionAddress, EncodedAction, InteractionAnchor, MAX_ANCHOR_ITEMS, MAX_ID_BYTES,
};

pub const MAX_PROJECTION_BYTES: usize = 64 * 1024;
const MAX_RETAINED_ANCHOR_BYTES: usize = 8 * 1024;
const MAX_PROVIDERS: usize = 256;
const MAX_DEPTH: usize = 32;

pub type AnchorProjection =
    fn(&AutoCommit, &ActionAddress, &EncodedAction) -> Option<InteractionAnchor>;

/// App-owned pure decoder over the newly encoded action and its exact loan.
/// The SDK alone qualifies the result against original invocation custody.
pub struct RegisteredInvocationAnchor {
    operation_type: &'static str,
    project: AnchorProjection,
    applied: Option<AnchorProjection>,
}

impl RegisteredInvocationAnchor {
    pub const fn new(operation_type: &'static str, project: AnchorProjection) -> Self {
        Self {
            operation_type,
            project,
            applied: None,
        }
    }
    /// Optional post-operation decoder. The SDK invokes it only against the
    /// actual AppliedLocal envelope and its committed record loan.
    #[must_use]
    pub const fn with_applied(mut self, project: AnchorProjection) -> Self {
        self.applied = Some(project);
        self
    }
}

/// Only the already resolved owner subtree participates. Ambiguity, excessive
/// traversal, nested owners and structural chrome never donate a registration.
fn registration(
    widget: &dyn crate::widget::Widget,
    type_id: &str,
) -> Option<&'static RegisteredInvocationAnchor> {
    struct Search<'a> {
        type_id: &'a str,
        remaining: usize,
        found: Option<&'static RegisteredInvocationAnchor>,
        invalid: bool,
    }
    fn visit(widget: &dyn crate::widget::Widget, depth: usize, search: &mut Search<'_>) {
        if search.invalid {
            return;
        }
        if depth > MAX_DEPTH || search.remaining == 0 {
            search.invalid = true;
            return;
        }
        search.remaining = search.remaining.saturating_sub(1);
        if widget.resets_callback_binding()
            || (depth != 0
                && (widget.has_separate_document_owner()
                    || widget.legacy_callback_boundary().is_some()))
        {
            return;
        }
        if let Some(provider) = crate::widget::capability::capability::<
            dyn crate::resources::PublishesResources,
        >(widget)
            // A forwarding wrapper cannot donate a descendant capability
            // before traversal has checked that descendant's owner boundary.
            && (provider as *const dyn crate::resources::PublishesResources).cast::<()>()
                == (widget as *const dyn crate::widget::Widget).cast::<()>()
            && provider.concrete_type_name() == widget.concrete_type_name()
        {
            let table = provider.invocation_anchor_projectors();
            if table.len() > MAX_ANCHOR_ITEMS {
                search.invalid = true;
                return;
            }
            for entry in table
                .iter()
                .filter(|entry| entry.operation_type == search.type_id)
            {
                if entry.operation_type.is_empty() || search.found.is_some() {
                    search.invalid = true;
                    return;
                }
                search.found = Some(entry);
            }
        }
        widget.for_each_child(&mut |child| visit(child, depth.saturating_add(1), search));
    }
    let mut search = Search {
        type_id,
        remaining: MAX_PROVIDERS,
        found: None,
        invalid: false,
    };
    visit(widget, 0, &mut search);
    (!search.invalid).then_some(search.found).flatten()
}

pub(crate) fn project(
    widget: &dyn crate::widget::Widget,
    document: &AutoCommit,
    address: &ActionAddress,
    encoded: &EncodedAction,
) -> Option<InteractionAnchor> {
    if encoded.payload.len() > MAX_PROJECTION_BYTES {
        return None;
    }
    let entry = registration(widget, &encoded.type_id)?;
    let anchor = (entry.project)(document, address, encoded)?;
    bounded(&anchor).then_some(anchor)
}

pub(crate) fn project_applied(
    widget: &dyn crate::widget::Widget,
    document: &AutoCommit,
    address: &ActionAddress,
    encoded: &EncodedAction,
) -> Option<InteractionAnchor> {
    if encoded.payload.len() > MAX_PROJECTION_BYTES {
        return None;
    }
    let entry = registration(widget, &encoded.type_id)?;
    let anchor = (entry.applied?)(document, address, encoded)?;
    bounded(&anchor).then_some(anchor)
}

// Reuse the existing protocol vocabulary and its public limits. This local
// target is not a fabricated SemanticInteraction and gets no transport stamp.
fn bounded(anchor: &InteractionAnchor) -> bool {
    fn id(value: &str) -> bool {
        !value.is_empty() && value.len() <= MAX_ID_BYTES && !value.chars().any(char::is_control)
    }
    fn count(value: usize) -> bool {
        value > 0 && value <= MAX_ANCHOR_ITEMS
    }
    let valid = match anchor {
        InteractionAnchor::Resource => true,
        InteractionAnchor::SemanticControl { id: value } => id(value),
        InteractionAnchor::Text { region, ranges } => {
            id(region)
                && count(ranges.len())
                && ranges
                    .iter()
                    .all(|range| id(&range.block) && range.start <= range.end)
        }
        InteractionAnchor::Grid { sheet, ranges } => {
            id(sheet)
                && count(ranges.len())
                && ranges.iter().all(|range| {
                    range.start_col <= range.end_col && range.start_row <= range.end_row
                })
        }
        InteractionAnchor::Objects { ids } | InteractionAnchor::ListItems { ids } => {
            count(ids.len()) && ids.iter().all(|value| id(value))
        }
        InteractionAnchor::Timeline {
            track,
            start_ms,
            end_ms,
        } => track.as_deref().is_none_or(id) && start_ms <= end_ms,
    };
    // Shape limits above bound serialization work, including escaped IDs.
    valid && serde_json::to_vec(anchor).is_ok_and(|bytes| bytes.len() <= MAX_RETAINED_ANCHOR_BYTES)
}
