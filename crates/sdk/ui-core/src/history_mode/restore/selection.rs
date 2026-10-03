//! Schema-directed projection. Structural maps normalize absence; entity keys do not.
use super::{
    HistoryRestoreNode, HistoryRestoreSchema, HistoryRootPattern, RestoreError,
    projection::{
        Alternatives, ContentProjection, Node, NodeId, ProjectionBuilder, Properties, property,
    },
    schema::selectors,
};
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(super) type IdentityPaths = BTreeSet<Vec<String>>;

pub(super) fn content(
    doc: &AutoCommit,
    schema: &HistoryRestoreSchema,
) -> Result<ContentProjection, RestoreError> {
    let mut builder = ProjectionBuilder::new();
    let mut result = BTreeMap::new();
    for selector in selectors(schema) {
        let keys = selector.pattern.keys(doc)?;
        if selector.required && keys.is_empty() {
            return Err(RestoreError::SchemaInvalid(
                "required family has no members".into(),
            ));
        }
        for key in keys {
            let values = selected(
                &mut builder,
                doc,
                Some(&ROOT),
                &key,
                selector.node,
                selector.required,
                &key,
                0,
            )?;
            result.insert(key, values);
        }
    }
    builder.build(result)
}

struct SelectEval {
    parent: Option<ObjId>,
    key: String,
    selector: HistoryRestoreNode,
    required: bool,
    path: String,
    depth: usize,
    output: usize,
}

enum SelectTask {
    Eval(SelectEval),
    FinishMap {
        output: usize,
        children: Vec<(String, usize)>,
    },
}

fn output_slot(outputs: &mut Vec<Option<Alternatives>>) -> usize {
    let slot = outputs.len();
    outputs.push(None);
    slot
}

fn selected(
    builder: &mut ProjectionBuilder,
    doc: &AutoCommit,
    parent: Option<&ObjId>,
    key: &str,
    selector: HistoryRestoreNode,
    required: bool,
    path: &str,
    depth: usize,
) -> Result<Alternatives, RestoreError> {
    let mut outputs = vec![None];
    let mut tasks = vec![SelectTask::Eval(SelectEval {
        parent: parent.cloned(),
        key: key.into(),
        selector,
        required,
        path: path.into(),
        depth,
        output: 0,
    })];
    while let Some(task) = tasks.pop() {
        match task {
            SelectTask::Eval(eval) => {
                let SelectEval {
                    parent,
                    key,
                    selector,
                    required,
                    path,
                    depth,
                    output,
                } = eval;
                match selector {
                    HistoryRestoreNode::Whole { expected_type } => {
                        let values = match parent.as_ref() {
                            Some(parent) => {
                                property(builder, doc, parent, key.into(), &path, true, depth)?
                            }
                            None => Vec::new(),
                        };
                        if values.is_empty() && required {
                            return Err(RestoreError::SchemaInvalid(format!(
                                "missing required property {path}"
                            )));
                        }
                        if values.first().is_some_and(|value| {
                            !builder
                                .value_type(*value)
                                .is_ok_and(|found| expected_type.accepts(found))
                        }) {
                            return Err(RestoreError::SchemaInvalid(format!(
                                "wrong type for {path}"
                            )));
                        }
                        outputs[output] = Some(values);
                    }
                    HistoryRestoreNode::MapContentExcept { preserved_fields } => {
                        let object = map_object(doc, parent.as_ref(), &key, required, &path)?;
                        let mut fields = BTreeMap::new();
                        if let Some(object) = &object {
                            for child in doc.keys(object) {
                                if preserved_fields.contains(&child.as_str()) {
                                    continue;
                                }
                                let child_path = format!("{path}/{child}");
                                fields.insert(
                                    child.clone(),
                                    property(
                                        builder,
                                        doc,
                                        object,
                                        child.into(),
                                        &child_path,
                                        true,
                                        depth.saturating_add(1),
                                    )?,
                                );
                            }
                        }
                        outputs[output] = Some(vec![builder.push(Node::Map(ObjType::Map, fields))]);
                    }
                    HistoryRestoreNode::MapOverlay(overlay) => {
                        let object = map_object(doc, parent.as_ref(), &key, required, &path)?;
                        let mut children = Vec::new();
                        let mut child_evals = Vec::new();
                        for field in overlay.fields {
                            let child_output = output_slot(&mut outputs);
                            children.push((field.key.into(), child_output));
                            child_evals.push(SelectEval {
                                parent: object.clone(),
                                key: field.key.into(),
                                selector: field.node,
                                required: field.required,
                                path: format!("{path}/{}", field.key),
                                depth: depth.saturating_add(1),
                                output: child_output,
                            });
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
                                let child_output = output_slot(&mut outputs);
                                children.push((child.clone(), child_output));
                                child_evals.push(SelectEval {
                                    parent: Some(object.clone()),
                                    key: child.clone(),
                                    selector: entries.node,
                                    required: true,
                                    path: format!("{path}/{child}"),
                                    depth: depth.saturating_add(1),
                                    output: child_output,
                                });
                            }
                        }
                        tasks.push(SelectTask::FinishMap { output, children });
                        tasks.extend(child_evals.into_iter().rev().map(SelectTask::Eval));
                    }
                }
            }
            SelectTask::FinishMap { output, children } => {
                let mut fields = BTreeMap::new();
                for (key, child_output) in children {
                    fields.insert(
                        key,
                        outputs[child_output]
                            .take()
                            .ok_or(RestoreError::ContentPostconditionMismatch)?,
                    );
                }
                outputs[output] = Some(vec![builder.push(Node::Map(ObjType::Map, fields))]);
            }
        }
    }
    outputs[0]
        .take()
        .ok_or(RestoreError::ContentPostconditionMismatch)
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

pub(super) fn map_values<'a>(
    projection: &'a ContentProjection,
    values: &[NodeId],
) -> Result<&'a Properties, RestoreError> {
    match values {
        [node] => match projection.node(*node)? {
            Node::Map(ObjType::Map, fields) => Ok(fields),
            _ => Err(RestoreError::ContentPostconditionMismatch),
        },
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
    let mut builder = ProjectionBuilder::new();
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
                property(&mut builder, doc, &ROOT, key.clone().into(), &key, false, 0)?,
            );
        }
    }
    for selector in selectors {
        if !structural(selector.node) {
            continue;
        }
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
            let node = protected_map(
                &mut builder,
                doc,
                ProtectedEval::at_root(&key, selector.node, values),
                target,
                &mut identities,
            )?;
            result.insert(key, vec![node]);
        }
    }
    Ok((builder.build(result)?, identities.seen))
}

struct ProtectedIdentities<'a> {
    existing: Option<&'a IdentityPaths>,
    seen: IdentityPaths,
}

fn node_output_slot(outputs: &mut Vec<Option<NodeId>>) -> usize {
    let slot = outputs.len();
    outputs.push(None);
    slot
}

struct ProtectedEval {
    parent: Option<ObjId>,
    key: String,
    selector: HistoryRestoreNode,
    target: Alternatives,
    path: Vec<String>,
    output: usize,
}

impl ProtectedEval {
    /// The first task of a walk: a root key, written to output slot 0.
    fn at_root(key: &str, selector: HistoryRestoreNode, target: &[NodeId]) -> Self {
        Self {
            parent: Some(ROOT),
            key: key.into(),
            selector,
            target: target.to_vec(),
            path: vec![key.into()],
            output: 0,
        }
    }
}

enum ProtectedTask {
    Eval(ProtectedEval),
    Finish {
        object: Option<ObjId>,
        path: Vec<String>,
        output: usize,
        fields: Properties,
        children: Vec<(String, usize)>,
    },
}

fn protected_map(
    builder: &mut ProjectionBuilder,
    doc: &AutoCommit,
    root: ProtectedEval,
    target_projection: &ContentProjection,
    identities: &mut ProtectedIdentities<'_>,
) -> Result<NodeId, RestoreError> {
    let mut outputs = vec![None];
    let mut tasks = vec![ProtectedTask::Eval(root)];
    while let Some(task) = tasks.pop() {
        match task {
            ProtectedTask::Eval(eval) => {
                let display = eval.path.join("/");
                let object = map_object(doc, eval.parent.as_ref(), &eval.key, false, &display)?;
                let target_fields = map_values(target_projection, &eval.target)?.clone();
                let mut fields = BTreeMap::new();
                let mut children = Vec::new();
                let mut child_evals = Vec::new();
                match eval.selector {
                    HistoryRestoreNode::MapContentExcept { preserved_fields } => {
                        for child in preserved_fields {
                            let values = match &object {
                                Some(object) => property(
                                    builder,
                                    doc,
                                    object,
                                    (*child).into(),
                                    &format!("{display}/{child}"),
                                    false,
                                    eval.path.len(),
                                )?,
                                None => Vec::new(),
                            };
                            fields.insert((*child).into(), values);
                        }
                    }
                    HistoryRestoreNode::MapOverlay(overlay) => {
                        if let Some(object) = &object {
                            for child in doc.keys(object) {
                                if overlay.fields.iter().any(|field| field.key == child)
                                    || overlay.entries.is_some()
                                {
                                    continue;
                                }
                                fields.insert(
                                    child.clone(),
                                    property(
                                        builder,
                                        doc,
                                        object,
                                        child.clone().into(),
                                        &format!("{display}/{child}"),
                                        false,
                                        eval.path.len(),
                                    )?,
                                );
                            }
                        }
                        for field in overlay.fields {
                            if structural(field.node) {
                                let child_output = node_output_slot(&mut outputs);
                                children.push((field.key.into(), child_output));
                                let mut child_path = eval.path.clone();
                                child_path.push(field.key.into());
                                child_evals.push(ProtectedEval {
                                    parent: object.clone(),
                                    key: field.key.into(),
                                    selector: field.node,
                                    target: target_fields
                                        .get(field.key)
                                        .ok_or(RestoreError::ContentPostconditionMismatch)?
                                        .clone(),
                                    path: child_path,
                                    output: child_output,
                                });
                            }
                        }
                        if let Some(entries) = overlay.entries
                            && structural(entries.node)
                        {
                            for (child, values) in &target_fields {
                                if overlay.fields.iter().any(|field| field.key == child) {
                                    continue;
                                }
                                let child_output = node_output_slot(&mut outputs);
                                children.push((child.clone(), child_output));
                                let mut child_path = eval.path.clone();
                                child_path.push(child.clone());
                                child_evals.push(ProtectedEval {
                                    parent: object.clone(),
                                    key: child.clone(),
                                    selector: entries.node,
                                    target: values.clone(),
                                    path: child_path,
                                    output: child_output,
                                });
                            }
                        }
                    }
                    HistoryRestoreNode::Whole { .. } => {
                        return Err(RestoreError::ContentPostconditionMismatch);
                    }
                }
                tasks.push(ProtectedTask::Finish {
                    object,
                    path: eval.path,
                    output: eval.output,
                    fields,
                    children,
                });
                tasks.extend(child_evals.into_iter().rev().map(ProtectedTask::Eval));
            }
            ProtectedTask::Finish {
                object,
                path,
                output,
                mut fields,
                children,
            } => {
                for (key, child_output) in children {
                    fields.insert(
                        key,
                        vec![
                            outputs[child_output]
                                .take()
                                .ok_or(RestoreError::ContentPostconditionMismatch)?,
                        ],
                    );
                }
                let map = builder.push(Node::Map(ObjType::Map, fields));
                let node = if let Some(object) = object
                    && identities
                        .existing
                        .is_none_or(|paths| paths.contains(&path))
                {
                    identities.seen.insert(path);
                    builder.push(Node::Protected(object, map))
                } else {
                    map
                };
                outputs[output] = Some(node);
            }
        }
    }
    outputs[0]
        .take()
        .ok_or(RestoreError::ContentPostconditionMismatch)
}

fn structural(node: HistoryRestoreNode) -> bool {
    matches!(
        node,
        HistoryRestoreNode::MapOverlay(_) | HistoryRestoreNode::MapContentExcept { .. }
    )
}
