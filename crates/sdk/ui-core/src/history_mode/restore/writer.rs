//! Checked writes into the caller-owned pending transaction. No commits or merges.
use super::{
    RestoreError, RestoreValueType,
    projection::{ContentProjection, Node, SemanticMark},
};
use automerge::{AutoCommit, ObjId, Prop, ROOT, marks::ExpandMark, transaction::Transactable};

pub(super) fn write(
    doc: &mut AutoCommit,
    schema: &super::HistoryRestoreSchema,
    current: &ContentProjection,
    target: &ContentProjection,
) -> Result<(), RestoreError> {
    for selector in super::schema::selectors(schema) {
        let keys: std::collections::BTreeSet<_> = current
            .0
            .keys()
            .chain(target.0.keys())
            .filter(|key| selector.pattern.matches(key))
            .collect();
        for key in keys {
            let old = current.0.get(key).map_or(&[][..], Vec::as_slice);
            let new = target.0.get(key).map_or(&[][..], Vec::as_slice);
            // Family membership is authoritative, including mixed entity roots.
            if new.is_empty()
                && !old.is_empty()
                && matches!(selector.pattern, super::HistoryRootPattern::Affix { .. })
            {
                doc.delete(ROOT, key.as_str())
                    .map_err(|err| RestoreError::write(key, &err))?;
            } else {
                write_selected(doc, &ROOT, key, selector.node, old, new, key)?;
            }
        }
    }
    Ok(())
}

fn write_selected(
    doc: &mut AutoCommit,
    parent: &ObjId,
    key: &str,
    selector: super::HistoryRestoreNode,
    current: &[Node],
    target: &[Node],
    path: &str,
) -> Result<(), RestoreError> {
    if current == target {
        return Ok(());
    }
    match selector {
        super::HistoryRestoreNode::Whole { .. } => match target {
            [] => doc
                .delete(parent, key)
                .map_err(|err| RestoreError::write(path, &err)),
            [node] => write_node(doc, parent, key.into(), node, path),
            _ => Err(RestoreError::UnsupportedConflict(path.into())),
        },
        super::HistoryRestoreNode::MapOverlay(overlay) => {
            let empty = std::collections::BTreeMap::new();
            let old = if current.is_empty() {
                &empty
            } else {
                super::selection::map_values(current)?
            };
            let new = super::selection::map_values(target)?;
            let object = match super::selection::map_object(doc, Some(parent), key, false, path)? {
                Some(object) => object,
                None => doc
                    .put_object(parent, key, automerge::ObjType::Map)
                    .map_err(|err| RestoreError::write(path, &err))?,
            };
            for field in overlay.fields {
                let before = old.get(field.key).map_or(&[][..], Vec::as_slice);
                let after = new
                    .get(field.key)
                    .ok_or(RestoreError::ContentPostconditionMismatch)?;
                write_selected(
                    doc,
                    &object,
                    field.key,
                    field.node,
                    before,
                    after,
                    &format!("{path}/{}", field.key),
                )?;
            }
            if let Some(entries) = overlay.entries {
                let keys: std::collections::BTreeSet<_> = old.keys().chain(new.keys()).collect();
                for key in keys {
                    if overlay.fields.iter().any(|field| field.key == key) {
                        continue;
                    }
                    let child_path = format!("{path}/{key}");
                    match (old.get(key), new.get(key)) {
                        (Some(_), None) => doc
                            .delete(&object, key.as_str())
                            .map_err(|err| RestoreError::write(&child_path, &err))?,
                        (before, Some(after)) => {
                            if before.is_none()
                                && entries.on_create == super::PreservedOnCreate::Reject
                            {
                                return Err(RestoreError::SchemaInvalid(format!(
                                    "entity resurrection refused at {child_path}"
                                )));
                            }
                            write_selected(
                                doc,
                                &object,
                                key,
                                entries.node,
                                before.map_or(&[][..], Vec::as_slice),
                                after,
                                &child_path,
                            )?;
                        }
                        (None, None) => {}
                    }
                }
            }
            Ok(())
        }
    }
}

fn write_marks(
    doc: &mut AutoCommit,
    object: &ObjId,
    marks: &[SemanticMark],
    path: &str,
) -> Result<(), RestoreError> {
    for mark in marks {
        // Public Mark exposes current ranges, not historic expansion boundaries.
        doc.mark(object, mark.to_mark(), ExpandMark::None)
            .map_err(|err| RestoreError::write(path, &err))?;
    }
    Ok(())
}

fn write_node(
    doc: &mut AutoCommit,
    parent: &ObjId,
    prop: Prop,
    node: &Node,
    path: &str,
) -> Result<(), RestoreError> {
    if let Node::Scalar(value) = node {
        match prop {
            Prop::Map(key) => doc.put(parent, key, value.0.clone()),
            Prop::Seq(index) => doc.insert(parent, index, value.0.clone()),
        }
        .map_err(|err| RestoreError::write(path, &err))?;
        return Ok(());
    }
    let RestoreValueType::Object(kind) = node.value_type() else {
        return Err(RestoreError::UnsupportedContent(path.into()));
    };
    let object = match prop {
        Prop::Map(key) => doc.put_object(parent, key, kind),
        Prop::Seq(index) => doc.insert_object(parent, index, kind),
    }
    .map_err(|err| RestoreError::write(path, &err))?;
    match node {
        Node::Map(_, values) => {
            for (key, alternatives) in values {
                let child_path = format!("{path}/{key}");
                let [child] = alternatives.as_slice() else {
                    return Err(RestoreError::UnsupportedConflict(child_path));
                };
                write_node(doc, &object, key.as_str().into(), child, &child_path)?;
            }
        }
        Node::List(elements, marks) => {
            for (index, alternatives) in elements.iter().enumerate() {
                let child_path = format!("{path}/{index}");
                let [child] = alternatives.as_slice() else {
                    return Err(RestoreError::UnsupportedConflict(child_path));
                };
                write_node(doc, &object, index.into(), child, &child_path)?;
            }
            write_marks(doc, &object, marks, path)?;
        }
        Node::Text {
            text,
            marks,
            protected_elements: None,
        } => {
            doc.splice_text(&object, 0, 0, text)
                .map_err(|err| RestoreError::write(path, &err))?;
            write_marks(doc, &object, marks, path)?;
        }
        _ => {
            return Err(RestoreError::UnsupportedContent(format!(
                "{path}: cannot write protected projection"
            )));
        }
    }
    Ok(())
}
