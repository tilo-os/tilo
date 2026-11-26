//! The patches of a workspace change in a person's words: the fields a comparison changed, what
//! each held either side, and which detail a selection came from. Resource-native renderers can
//! still use the complete before/after documents when opened.

use automerge::{Prop, ReadDoc};

use super::{HistorySelectionDetail, counted, field_change, t, t_args};

pub(super) fn detail_provenance(detail: &HistorySelectionDetail) -> String {
    let people = detail.authors.join(", ");
    let resources = detail
        .resources
        .iter()
        .map(|resource| resource.label.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let facets = detail
        .facets
        .iter()
        .map(|facet| format!("{}: {}", facet.key, facet.value))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{}\n{}\n{}{}",
        t_args(
            "activity-detail-action-count",
            &[("count", detail.entry_ids.len().into())],
        ),
        people,
        if resources.is_empty() {
            t("activity-no-resource")
        } else {
            resources
        },
        if facets.is_empty() {
            String::new()
        } else {
            format!("\n{facets}")
        },
    )
}

/// Turn Automerge's structural patch vocabulary into a small, stable set of phrases suitable for
/// the human-facing comparison preview. Resource-native renderers can still use the complete
/// before/after documents when opened. The fields a comparison changed, with what each held either
/// side, read from the before and after documents the comparison kept.
pub fn comparison_field_changes(
    comparison: &ui::history_source::HistoryComparison,
) -> Vec<crate::desktop::activity::vocabulary::FieldChange> {
    let before = automerge::AutoCommit::load(&comparison.before_document).ok();
    let after = automerge::AutoCommit::load(&comparison.after_document).ok();
    let (Some(before), Some(after)) = (before, after) else {
        return Vec::new();
    };
    let mut changes: Vec<crate::desktop::activity::vocabulary::FieldChange> = Vec::new();
    for patch in &comparison.patches {
        let automerge::PatchAction::PutMap { key, .. } = &patch.action else {
            continue;
        };
        let field = humanize_field(key);
        if changes.iter().any(|change| change.field == field) {
            continue;
        }
        changes.push(crate::desktop::activity::vocabulary::FieldChange {
            before: map_value(&before, &patch.obj, key),
            after: map_value(&after, &patch.obj, key),
            field,
        });
    }
    changes
}

/// What `key` of the map `object` holds in `doc` , as a person reads it.
fn map_value(doc: &automerge::AutoCommit, object: &automerge::ObjId, key: &str) -> String {
    match doc.get(object, key).ok().flatten() {
        None => String::new(),
        Some((automerge::Value::Scalar(value), _)) => match value.as_ref() {
            automerge::ScalarValue::Str(text) => text.to_string(),
            automerge::ScalarValue::Int(number) => number.to_string(),
            automerge::ScalarValue::Uint(number) => number.to_string(),
            automerge::ScalarValue::F64(number) => format!("{number}"),
            automerge::ScalarValue::Boolean(flag) => {
                if *flag {
                    "yes".into()
                } else {
                    "no".into()
                }
            }
            automerge::ScalarValue::Null => String::new(),
            other => format!("{other:?}"),
        },
        // A list or a map reads as what it holds, not as its identity.
        Some(_) => t("activity-change-content"),
    }
}

pub(super) fn comparison_change_labels(patches: &[automerge::Patch]) -> Vec<String> {
    let mut labels = Vec::new();
    for patch in patches {
        let label = match &patch.action {
            automerge::PatchAction::PutMap { key, conflict, .. } => field_change(
                if *conflict {
                    "activity-change-resolved-field"
                } else {
                    "activity-change-updated-field"
                },
                humanize_field(key),
            ),
            automerge::PatchAction::DeleteMap { key } => {
                field_change("activity-change-removed-field", humanize_field(key))
            }
            automerge::PatchAction::PutSeq { conflict, .. } => t(if *conflict {
                "activity-change-resolved-list"
            } else {
                "activity-change-updated-list-item"
            }),
            automerge::PatchAction::Insert { values, .. } => {
                counted("activity-change-added-items", values.len())
            }
            automerge::PatchAction::SpliceText { .. } => t("activity-change-edited-text"),
            automerge::PatchAction::Increment { prop, value } => {
                let field = match prop {
                    Prop::Map(key) => humanize_field(key),
                    Prop::Seq(_) => t("activity-change-a-counter"),
                };
                field_change(
                    if *value < 0 {
                        "activity-change-decreased"
                    } else {
                        "activity-change-increased"
                    },
                    field,
                )
            }
            automerge::PatchAction::Conflict { prop } => match prop {
                Prop::Map(key) => {
                    field_change("activity-change-concurrent-field", humanize_field(key))
                }
                Prop::Seq(_) => t("activity-change-concurrent-list"),
            },
            automerge::PatchAction::DeleteSeq { length, .. } => {
                counted("activity-change-removed-items", *length)
            }
            automerge::PatchAction::Mark { .. } => t("activity-change-formatting"),
        };
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    labels
}

fn humanize_field(field: &str) -> String {
    let words = field.replace(['_', '-'], " ");
    if words.trim().is_empty() {
        t("activity-change-content")
    } else {
        words
    }
}
