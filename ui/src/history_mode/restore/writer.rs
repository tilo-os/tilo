//! Checked writes into the caller-owned pending transaction. No commits or merges.
use super::{
    RestoreError, RestoreValueType,
    projection::{ContentProjection, Node, SemanticMark},
};
use automerge::{AutoCommit, ObjId, Prop, ROOT, marks::ExpandMark, transaction::Transactable};

pub(super) fn write(
    doc: &mut AutoCommit,
    current: &ContentProjection,
    target: &ContentProjection,
) -> Result<(), RestoreError> {
    for (key, values) in &target.0 {
        if current.0.get(key) == Some(values) {
            continue;
        }
        match values.as_slice() {
            [] => doc
                .delete(ROOT, key.as_str())
                .map_err(|err| RestoreError::write(key, &err))?,
            [node] => write_node(doc, &ROOT, key.as_str().into(), node, key)?,
            _ => return Err(RestoreError::UnsupportedConflict(key.clone())),
        }
    }
    Ok(())
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
