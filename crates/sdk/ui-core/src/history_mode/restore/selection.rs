//! Schema-directed projection. Structural maps normalize absence; entity keys do not.
use super::{
    HistoryMapOverlay, HistoryRestoreNode, HistoryRestoreSchema, HistoryRootPattern, RestoreError,
    projection::{ContentProjection, Node, Properties, property},
    schema::selectors,
};
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(super) type IdentityPaths = BTreeSet<Vec<String>>;

pub(super) fn content(
    doc: &AutoCommit,
    schema: &HistoryRestoreSchema,
) -> Result<ContentProjection, RestoreError> {
    let mut result = BTreeMap::new();
    for selector in selectors(schema) {
        let keys = selector.pattern.keys(doc)?;
        if selector.required && keys.is_empty() {
            return Err(RestoreError::SchemaInvalid(
                "required family has no members".into(),
            ));
        }
        for key in keys {
            result.insert(
                key.clone(),
                selected(
                    doc,
                    Some(&ROOT),
                    &key,
                    selector.node,
                    selector.required,
                    &key,
                    0,
                )?,
            );
        }
    }
    Ok(ContentProjection(result))
}

fn selected(
    doc: &AutoCommit,
    parent: Option<&ObjId>,
    key: &str,
    selector: HistoryRestoreNode,
    required: bool,
    path: &str,
    depth: usize,
) -> Result<Vec<Node>, RestoreError> {
    match selector {
        HistoryRestoreNode::Whole { expected_type } => {
            let values = match parent {
                Some(parent) => property(doc, parent, key.into(), path, true, depth)?,
                None => Vec::new(),
            };
            if values.is_empty() && required {
                return Err(RestoreError::SchemaInvalid(format!(
                    "missing required property {path}"
                )));
            }
            if values
                .first()
                .is_some_and(|value| value.value_type() != expected_type)
            {
                return Err(RestoreError::SchemaInvalid(format!(
                    "wrong type for {path}"
                )));
            }
            Ok(values)
        }
        HistoryRestoreNode::MapOverlay(overlay) => {
            let object = map_object(doc, parent, key, required, path)?;
            let mut fields = BTreeMap::new();
            for field in overlay.fields {
                let child_path = format!("{path}/{}", field.key);
                fields.insert(
                    field.key.into(),
                    selected(
                        doc,
                        object.as_ref(),
                        field.key,
                        field.node,
                        field.required,
                        &child_path,
                        depth.saturating_add(1),
                    )?,
                );
            }
            if let (Some(entries), Some(object)) = (overlay.entries, object.as_ref()) {
                for child in doc.keys(object) {
                    if overlay.fields.iter().any(|field| field.key == child) {
                        continue;
                    }
                    if child.is_empty() || child.len() > entries.key_limit_bytes {
                        return Err(RestoreError::SchemaInvalid(format!(
                            "entity key exceeds declared bounds at {path}/{child}"
                        )));
                    }
                    let child_path = format!("{path}/{child}");
                    fields.insert(
                        child.clone(),
                        selected(
                            doc,
                            Some(object),
                            &child,
                            entries.node,
                            true,
                            &child_path,
                            depth.saturating_add(1),
                        )?,
                    );
                }
            }
            Ok(vec![Node::Map(ObjType::Map, fields)])
        }
    }
}

pub(super) fn map_object(
    doc: &AutoCommit,
    parent: Option<&ObjId>,
    key: &str,
    required: bool,
    path: &str,
) -> Result<Option<ObjId>, RestoreError> {
    let values = match parent {
        Some(parent) => doc
            .get_all(parent, key)
            .map_err(|err| RestoreError::read(path, &err))?,
        None => Vec::new(),
    };
    match values.as_slice() {
        [] if !required => Ok(None),
        [] => Err(RestoreError::SchemaInvalid(format!(
            "missing required map {path}"
        ))),
        [(Value::Object(ObjType::Map), id)] => Ok(Some(id.clone())),
        [_] => Err(RestoreError::SchemaInvalid(format!(
            "wrong map type at {path}"
        ))),
        _ => Err(RestoreError::UnsupportedConflict(path.into())),
    }
}

pub(super) fn map_values(values: &[Node]) -> Result<&Properties, RestoreError> {
    match values {
        [Node::Map(ObjType::Map, fields)] => Ok(fields),
        _ => Err(RestoreError::ContentPostconditionMismatch),
    }
}

/// Snapshot protected values and their operation/object identities. `target`
/// limits entity membership: intentionally deleted identities have no protected
/// descendants in the result. Newly admitted entities must contain no unselected
/// historical properties. Structural ancestors remain protected when they exist.
pub(super) fn protected(
    doc: &AutoCommit,
    schema: &HistoryRestoreSchema,
    target: &ContentProjection,
    identity_paths: Option<&IdentityPaths>,
) -> Result<(ContentProjection, IdentityPaths), RestoreError> {
    let selectors = selectors(schema);
    let mut result = BTreeMap::new();
    let mut identities = ProtectedIdentities {
        existing: identity_paths,
        seen: BTreeSet::new(),
    };
    for key in doc.keys(ROOT) {
        if !selectors
            .iter()
            .any(|selector| selector.pattern.matches(&key))
        {
            result.insert(
                key.clone(),
                property(doc, &ROOT, key.clone().into(), &key, false, 0)?,
            );
        }
    }
    for selector in selectors {
        let HistoryRestoreNode::MapOverlay(overlay) = selector.node else {
            continue;
        };
        let keys: Vec<String> = match selector.pattern {
            HistoryRootPattern::Exact(key) => vec![key.into()],
            pattern @ HistoryRootPattern::Affix { .. } => target
                .0
                .keys()
                .filter(|key| pattern.matches(key))
                .cloned()
                .collect(),
        };
        for key in keys {
            let values = target
                .0
                .get(&key)
                .ok_or(RestoreError::ContentPostconditionMismatch)?;
            let mut path = vec![key.clone()];
            let node = protected_map(
                doc,
                Some(&ROOT),
                &key,
                overlay,
                values,
                &mut path,
                &mut identities,
            )?;
            result.insert(key, vec![node]);
        }
    }
    Ok((ContentProjection(result), identities.seen))
}

struct ProtectedIdentities<'a> {
    existing: Option<&'a IdentityPaths>,
    seen: IdentityPaths,
}

fn protected_map(
    doc: &AutoCommit,
    parent: Option<&ObjId>,
    key: &str,
    overlay: &HistoryMapOverlay,
    target: &[Node],
    path: &mut Vec<String>,
    identities: &mut ProtectedIdentities<'_>,
) -> Result<Node, RestoreError> {
    let display = path.join("/");
    let object = map_object(doc, parent, key, false, &display)?;
    let target = map_values(target)?;
    let mut result = BTreeMap::new();
    if let Some(object) = &object {
        for child in doc.keys(object) {
            if overlay.fields.iter().any(|field| field.key == child) || overlay.entries.is_some() {
                continue;
            }
            result.insert(
                child.clone(),
                property(
                    doc,
                    object,
                    child.clone().into(),
                    &format!("{display}/{child}"),
                    false,
                    path.len(),
                )?,
            );
        }
    }
    for field in overlay.fields {
        if let HistoryRestoreNode::MapOverlay(child_overlay) = field.node {
            path.push(field.key.into());
            let values = target
                .get(field.key)
                .ok_or(RestoreError::ContentPostconditionMismatch)?;
            let child = protected_map(
                doc,
                object.as_ref(),
                field.key,
                child_overlay,
                values,
                path,
                identities,
            )?;
            path.pop();
            result.insert(field.key.into(), vec![child]);
        }
    }
    if let Some(entries) = overlay.entries
        && let HistoryRestoreNode::MapOverlay(child_overlay) = entries.node
    {
        for (child, values) in target {
            if overlay.fields.iter().any(|field| field.key == child) {
                continue;
            }
            path.push(child.clone());
            let node = protected_map(
                doc,
                object.as_ref(),
                child,
                child_overlay,
                values,
                path,
                identities,
            )?;
            path.pop();
            result.insert(child.clone(), vec![node]);
        }
    }
    let node = Node::Map(ObjType::Map, result);
    if let Some(object) = object
        && identities.existing.is_none_or(|paths| paths.contains(path))
    {
        identities.seen.insert(path.clone());
        return Ok(Node::Protected(object, Box::new(node)));
    }
    Ok(node)
}
