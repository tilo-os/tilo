//! Checked writes into the caller-owned pending transaction. No commits or merges.
use super::{
    HistoryRestoreNode, PreservedOnCreate, RestoreError, RestoreValueType,
    projection::{Alternatives, ContentProjection, Node, NodeId, Properties, SemanticMark},
};
use automerge::{
    AutoCommit, ObjId, ObjType, Prop, ROOT, ReadDoc, Value, marks::ExpandMark,
    transaction::Transactable,
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn write(
    doc: &mut AutoCommit,
    schema: &super::HistoryRestoreSchema,
    current: &ContentProjection,
    target: &ContentProjection,
) -> Result<(), RestoreError> {
    for selector in super::schema::selectors(schema) {
        let keys: BTreeSet<_> = current
            .0
            .keys()
            .chain(target.0.keys())
            .filter(|key| selector.pattern.matches(key))
            .cloned()
            .collect();
        for key in keys {
            let old = current.0.get(&key).cloned().unwrap_or_default();
            let new = target.0.get(&key).cloned().unwrap_or_default();
            if new.is_empty()
                && !old.is_empty()
                && matches!(selector.pattern, super::HistoryRootPattern::Affix { .. })
            {
                doc.delete(ROOT, key.as_str())
                    .map_err(|err| RestoreError::write(&key, &err))?;
            } else {
                run_tasks(
                    doc,
                    current,
                    target,
                    Task::Selected {
                        parent: ROOT,
                        key: key.clone(),
                        selector: selector.node,
                        current: old,
                        target: new,
                        path: key,
                    },
                )?;
            }
        }
    }
    Ok(())
}

enum Task {
    Selected {
        parent: ObjId,
        key: String,
        selector: HistoryRestoreNode,
        current: Alternatives,
        target: Alternatives,
        path: String,
    },
    ReconcileProperty {
        parent: ObjId,
        key: String,
        current: Alternatives,
        target: Alternatives,
        path: String,
    },
    ReconcileProperties {
        object: ObjId,
        current: Properties,
        target: Properties,
        path: String,
    },
    WriteNode {
        parent: ObjId,
        prop: Prop,
        node: NodeId,
        path: String,
    },
    WriteMarks {
        object: ObjId,
        marks: Vec<SemanticMark>,
        path: String,
    },
    Delete {
        parent: ObjId,
        key: String,
        path: String,
    },
}

fn run_tasks(
    doc: &mut AutoCommit,
    current_projection: &ContentProjection,
    target_projection: &ContentProjection,
    first: Task,
) -> Result<(), RestoreError> {
    let mut tasks = vec![first];
    while let Some(task) = tasks.pop() {
        match task {
            Task::Selected {
                parent,
                key,
                selector,
                current,
                target,
                path,
            } => {
                if current_projection.alternatives_equal(&current, target_projection, &target) {
                    continue;
                }
                match selector {
                    HistoryRestoreNode::Whole { .. } => {
                        tasks.push(Task::ReconcileProperty {
                            parent,
                            key,
                            current,
                            target,
                            path,
                        });
                    }
                    HistoryRestoreNode::MapContentExcept { preserved_fields } => {
                        let old = if current.is_empty() {
                            BTreeMap::new()
                        } else {
                            super::selection::map_values(current_projection, &current)?.clone()
                        };
                        let new = super::selection::map_values(target_projection, &target)?.clone();
                        if preserved_fields
                            .iter()
                            .any(|field| old.contains_key(*field) || new.contains_key(*field))
                        {
                            return Err(RestoreError::ContentPostconditionMismatch);
                        }
                        let object = match super::selection::map_object(
                            doc,
                            Some(&parent),
                            &key,
                            false,
                            &path,
                        )? {
                            Some(object) => object,
                            None => doc
                                .put_object(&parent, key, ObjType::Map)
                                .map_err(|error| RestoreError::write(&path, &error))?,
                        };
                        tasks.push(Task::ReconcileProperties {
                            object,
                            current: old,
                            target: new,
                            path,
                        });
                    }
                    HistoryRestoreNode::MapOverlay(overlay) => {
                        let old = if current.is_empty() {
                            BTreeMap::new()
                        } else {
                            super::selection::map_values(current_projection, &current)?.clone()
                        };
                        let new = super::selection::map_values(target_projection, &target)?.clone();
                        let object = match super::selection::map_object(
                            doc,
                            Some(&parent),
                            &key,
                            false,
                            &path,
                        )? {
                            Some(object) => object,
                            None => doc
                                .put_object(&parent, key, ObjType::Map)
                                .map_err(|err| RestoreError::write(&path, &err))?,
                        };
                        let mut actions = Vec::new();
                        for field in overlay.fields {
                            actions.push(Task::Selected {
                                parent: object.clone(),
                                key: field.key.into(),
                                selector: field.node,
                                current: old.get(field.key).cloned().unwrap_or_default(),
                                target: new
                                    .get(field.key)
                                    .cloned()
                                    .ok_or(RestoreError::ContentPostconditionMismatch)?,
                                path: format!("{path}/{}", field.key),
                            });
                        }
                        if let Some(entries) = overlay.entries {
                            let keys: BTreeSet<_> = old.keys().chain(new.keys()).cloned().collect();
                            for child in keys {
                                if overlay.fields.iter().any(|field| field.key == child) {
                                    continue;
                                }
                                let child_path = format!("{path}/{child}");
                                match (old.get(&child), new.get(&child)) {
                                    (Some(_), None) => actions.push(Task::Delete {
                                        parent: object.clone(),
                                        key: child,
                                        path: child_path,
                                    }),
                                    (before, Some(after)) => {
                                        if before.is_none()
                                            && entries.on_create == PreservedOnCreate::Reject
                                        {
                                            return Err(RestoreError::SchemaInvalid(format!(
                                                "entity resurrection refused at {child_path}"
                                            )));
                                        }
                                        actions.push(Task::Selected {
                                            parent: object.clone(),
                                            key: child,
                                            selector: entries.node,
                                            current: before.cloned().unwrap_or_default(),
                                            target: after.clone(),
                                            path: child_path,
                                        });
                                    }
                                    (None, None) => {}
                                }
                            }
                        }
                        tasks.extend(actions.into_iter().rev());
                    }
                }
            }
            Task::ReconcileProperty {
                parent,
                key,
                current,
                target,
                path,
            } => {
                if current_projection.alternatives_equal(&current, target_projection, &target) {
                    continue;
                }
                let same_kind_maps = match (current.as_slice(), target.as_slice()) {
                    ([current], [target]) => match (
                        current_projection.node(*current)?,
                        target_projection.node(*target)?,
                    ) {
                        (
                            Node::Map(current_kind @ (ObjType::Map | ObjType::Table), current),
                            Node::Map(target_kind, target),
                        ) if current_kind == target_kind => {
                            Some((*current_kind, current.clone(), target.clone()))
                        }
                        _ => None,
                    },
                    _ => None,
                };
                if let Some((kind, current, target)) = same_kind_maps {
                    let values = doc
                        .get_all(&parent, key.as_str())
                        .map_err(|error| RestoreError::read(&path, &error))?;
                    let object = match values.as_slice() {
                        [(Value::Object(live_kind), object)] if *live_kind == kind => {
                            object.clone()
                        }
                        [] | [_] => return Err(RestoreError::ContentPostconditionMismatch),
                        _ => return Err(RestoreError::UnsupportedConflict(path)),
                    };
                    tasks.push(Task::ReconcileProperties {
                        object,
                        current,
                        target,
                        path,
                    });
                } else {
                    match target.as_slice() {
                        [] => doc
                            .delete(&parent, key.as_str())
                            .map_err(|error| RestoreError::write(&path, &error))?,
                        [node] => tasks.push(Task::WriteNode {
                            parent,
                            prop: key.into(),
                            node: *node,
                            path,
                        }),
                        _ => return Err(RestoreError::UnsupportedConflict(path)),
                    }
                }
            }
            Task::ReconcileProperties {
                object,
                current,
                target,
                path,
            } => {
                let keys: BTreeSet<_> = current.keys().chain(target.keys()).cloned().collect();
                let actions = keys.into_iter().map(|key| Task::ReconcileProperty {
                    parent: object.clone(),
                    path: format!("{path}/{key}"),
                    current: current.get(&key).cloned().unwrap_or_default(),
                    target: target.get(&key).cloned().unwrap_or_default(),
                    key,
                });
                tasks.extend(actions.rev());
            }
            Task::WriteNode {
                parent,
                prop,
                node,
                path,
            } => match target_projection.node(node)? {
                Node::Scalar(value) => {
                    match prop {
                        Prop::Map(key) => doc.put(&parent, key, value.0.clone()),
                        Prop::Seq(index) => doc.insert(&parent, index, value.0.clone()),
                    }
                    .map_err(|err| RestoreError::write(&path, &err))?;
                }
                node_value => {
                    let RestoreValueType::Object(kind) = target_projection.value_type(node)? else {
                        return Err(RestoreError::UnsupportedContent(path));
                    };
                    let object = match prop {
                        Prop::Map(key) => doc.put_object(&parent, key, kind),
                        Prop::Seq(index) => doc.insert_object(&parent, index, kind),
                    }
                    .map_err(|err| RestoreError::write(&path, &err))?;
                    match node_value {
                        Node::Map(_, values) => {
                            let mut actions = Vec::new();
                            for (key, alternatives) in values {
                                let [child] = alternatives.as_slice() else {
                                    return Err(RestoreError::UnsupportedConflict(format!(
                                        "{path}/{key}"
                                    )));
                                };
                                actions.push(Task::WriteNode {
                                    parent: object.clone(),
                                    prop: key.as_str().into(),
                                    node: *child,
                                    path: format!("{path}/{key}"),
                                });
                            }
                            tasks.extend(actions.into_iter().rev());
                        }
                        Node::List(elements, marks) => {
                            tasks.push(Task::WriteMarks {
                                object: object.clone(),
                                marks: marks.clone(),
                                path: path.clone(),
                            });
                            let mut actions = Vec::new();
                            for (index, alternatives) in elements.iter().enumerate() {
                                let [child] = alternatives.as_slice() else {
                                    return Err(RestoreError::UnsupportedConflict(format!(
                                        "{path}/{index}"
                                    )));
                                };
                                actions.push(Task::WriteNode {
                                    parent: object.clone(),
                                    prop: index.into(),
                                    node: *child,
                                    path: format!("{path}/{index}"),
                                });
                            }
                            tasks.extend(actions.into_iter().rev());
                        }
                        Node::Text {
                            text,
                            marks,
                            protected_elements: None,
                        } => {
                            doc.splice_text(&object, 0, 0, text)
                                .map_err(|err| RestoreError::write(&path, &err))?;
                            tasks.push(Task::WriteMarks {
                                object,
                                marks: marks.clone(),
                                path,
                            });
                        }
                        Node::Protected(..) | Node::Text { .. } | Node::Scalar(..) => {
                            return Err(RestoreError::UnsupportedContent(format!(
                                "{path}: cannot write protected projection"
                            )));
                        }
                    }
                }
            },
            Task::WriteMarks {
                object,
                marks,
                path,
            } => {
                for mark in marks {
                    doc.mark(&object, mark.to_mark(), ExpandMark::None)
                        .map_err(|err| RestoreError::write(&path, &err))?;
                }
            }
            Task::Delete { parent, key, path } => doc
                .delete(&parent, key)
                .map_err(|err| RestoreError::write(&path, &err))?,
        }
    }
    Ok(())
}
