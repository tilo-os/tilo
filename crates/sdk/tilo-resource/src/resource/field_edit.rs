//! The generic field edit a record publishes: one action taking any edit
//! of its fields, judged per edit for risk and for whether Undo takes it
//! back.

use super::{
    ActionParam, ActionSpec, Arc, EDIT_PARAM, Edit, ExecutionLocus, FIELD_EDIT, ParamType,
    Vocabulary, encoded, origin_now_ms, refused,
};

/// The field edit at paths under `prefix`, described by `schema`.
pub(super) fn rooted_edit_spec(
    vocabulary: &'static Vocabulary,
    (prefix, root): (Vec<crate::Segment>, Option<crate::Shape>),
    schema: Arc<serde_json::Value>,
    label: String,
    description: &'static str,
    now: Vec<(String, String)>,
) -> ActionSpec {
    let mut tags = vec![tilo_action_policy::edits::FIELD_EDIT_TAG.to_owned()];
    if let Some(root) = root {
        // Any door reads the edit without this record's code: it is a field
        // edit, these collections' removals ask nothing, these fields' every
        // edit asks, and these fields' edits making them weaker than they
        // are now.
        tags.extend(
            root.quiet_removals()
                .into_iter()
                .map(|path| format!("{}{path}", tilo_action_policy::edits::QUIET_REMOVAL_TAG)),
        );
        tags.extend(root.guarded_fields().into_iter().map(|(path, trust)| {
            if let crate::Trust::Weakens(weaker) = trust {
                let at = now.iter().find(|(field, _)| *field == path);
                tilo_action_policy::edits::weakening_tag(
                    &path,
                    at.map(|(_, now)| now.as_str()),
                    weaker,
                )
            } else {
                format!("{}{path}", tilo_action_policy::edits::GUARDED_FIELD_TAG)
            }
        }));
    }
    let judged = root.map(|root| (root, FieldValues::from(now)));
    let mut spec = edit_spec(vocabulary, prefix, schema, label, description, judged);
    spec.contract.tags.extend(tags);
    let takes_reactions = vocabulary
        .record_table
        .is_some_and(|table| table.holds(<crate::Reactions as crate::Model>::TABLE));
    if !takes_reactions {
        return spec;
    }
    // A reaction is placed below the reactions field, wherever it is, as
    // the room places it: someone who may only comment may react, though
    // no other edit of the record is theirs. The edit is offered to whoever
    // may react, and each invocation is what it does.
    let edit = spec.contract.need();
    let reaction = tilo_actions::ActionNeed::reactions();
    spec.with_need(reaction.clone()).with_need_of(move |args| {
        if reacts(args) {
            reaction.clone()
        } else {
            edit.clone()
        }
    })
}

/// Whether the field edit in `args` reacts, or takes a reaction back.
fn reacts(args: &tilo_actions::ActionArgs) -> bool {
    let Some(tilo_actions::ArgValue::Object(value)) = args.get(EDIT_PARAM) else {
        return false;
    };
    value
        .get("op")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|op| op == "react" || op == "unreact")
}

/// The values of fields, as JSON text by dotted path.
type FieldValues = Arc<[(String, String)]>;

/// The field edit at paths under `prefix`, described by `schema`; with
/// `judged`, the shape at `prefix` and the values now of its fields that
/// weaken one way, by which an edit's risk is judged.
fn edit_spec(
    vocabulary: &'static Vocabulary,
    prefix: Vec<crate::Segment>,
    schema: Arc<serde_json::Value>,
    label: String,
    description: &'static str,
    judged: Option<(crate::Shape, FieldValues)>,
) -> ActionSpec {
    let read = move |value: &serde_json::Map<String, serde_json::Value>| {
        let edit = Edit::<()>::from_json(serde_json::Value::Object(value.clone()))?;
        let mut path = prefix.clone();
        path.extend(edit.path().iter().cloned());
        match (vocabulary.edit)(Edit::new(path, edit.op().clone())) {
            Some(action) => Ok(action),
            None => Err(format!("{} has no field edits", vocabulary.label)),
        }
    };
    let read = std::sync::Arc::new(read);
    let risk_read = std::sync::Arc::clone(&read);
    let exposing_shape = judged.as_ref().map(|(shape, _)| *shape);
    ActionSpec::typed(
        FIELD_EDIT,
        label,
        "Field edit as JSON",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required_owned(EDIT_PARAM, ParamType::Object, description)
                .with_shared_schema(schema),
        ],
        move |args| {
            let Some(tilo_actions::ArgValue::Object(value)) = args.get(EDIT_PARAM) else {
                return refused("missing edit");
            };
            match read(value) {
                Ok(mut action) => {
                    action.stamp(origin_now_ms());
                    encoded(&action)
                }
                Err(reason) => refused(&reason),
            }
        },
    )
    .with_description(
        "Change one field: {path: [{field: name} | {item: id} | {key: k} | {cell: {row, column}}], op: set | splice | replace | add | insert | remove | ..., ...its arguments}. Address a collection's items by what they hold, never by position: {path: [{field: name}], op: \"upsert-item\", match: {field: value}, value: {fields}} updates the one item that matches, else appends one; insert-item and move-to place by at: \"end\" | {after: id} | {before: id}.",
    )
    // Controls and agents edit fields; a person picks named actions or edits
    // the field in place.
    .for_audience(tilo_actions::Audience::Agents)
    // History records the edit itself, not `field-edit`, and Undo puts the
    // field's value back.
    .carrying()
    .of_kind(tilo_actions::ActionKind::Undoable)
    // Removing a collection item asks, as a delete verb does, and so does
    // any edit of a field that is not ordinary, but an edit of one that
    // weakens one way only when it makes it weaker than it is now.
    .with_risk(move |args| {
        let Some(tilo_actions::ArgValue::Object(value)) = args.get(EDIT_PARAM) else {
            return tilo_actions::RiskLevel::Low;
        };
        let Ok(action) = risk_read(value) else {
            return tilo_actions::RiskLevel::Low;
        };
        let Some((shape, now)) = judged.as_ref().filter(|(_, now)| !now.is_empty()) else {
            return action.risk();
        };
        match Edit::<()>::from_json(serde_json::Value::Object(value.clone())) {
            Ok(edit) => crate::path::edit_risk(*shape, edit.path(), edit.op(), Some(now)),
            Err(_) => action.risk(),
        }
    })
    // Undo puts a field's value back, but not what others saw of a field
    // that exposes the person: such an edit asks first, even though Undo
    // restores the value.
    .with_undo_of(move |args| {
        let Some(shape) = exposing_shape else {
            return true;
        };
        let Some(tilo_actions::ArgValue::Object(value)) = args.get(EDIT_PARAM) else {
            return true;
        };
        Edit::<()>::from_json(serde_json::Value::Object(value.clone()))
            .map_or(true, |edit| shape.trust_at(edit.path()) != crate::Trust::Sensitive)
    })
}
