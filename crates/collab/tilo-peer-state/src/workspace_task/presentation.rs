//! Incremental storage for replacement provider snapshots. Authority remains in
//! the authenticated command reducer; this module only projects accepted state.
use automerge::{
    AutoCommit, ObjType, ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable,
};
use collab_types::task_presentation::TaskPresentation;
use std::collections::BTreeMap;

const KEY: &str = "task_presentations_v1";

fn map(
    doc: &mut AutoCommit,
    parent: &automerge::ObjId,
    key: &str,
) -> Result<automerge::ObjId, String> {
    match doc.get(parent, key).map_err(|e| e.to_string())? {
        Some((Value::Object(ObjType::Map), id)) => Ok(id),
        Some(_) => Err("task presentation object is not a map".into()),
        None => doc
            .put_object(parent, key, ObjType::Map)
            .map_err(|e| e.to_string()),
    }
}

pub(super) fn load(
    doc: &AutoCommit,
    task: &str,
) -> Result<Option<BTreeMap<String, TaskPresentation>>, String> {
    let root = match doc.get(ROOT, KEY).map_err(|e| e.to_string())? {
        Some((Value::Object(ObjType::Map), id)) => id,
        Some(_) => return Err("task presentation root is not a map".into()),
        None => return Ok(None),
    };
    let runs = match doc.get(root, task).map_err(|e| e.to_string())? {
        Some((Value::Object(ObjType::Map), id)) => id,
        Some(_) => return Err("task presentations are not a map".into()),
        None => return Ok(None),
    };
    let mut result = BTreeMap::new();
    for run in doc.keys(&runs) {
        let Some((Value::Object(ObjType::Map), entry)) =
            doc.get(&runs, &run).map_err(|e| e.to_string())?
        else {
            return Err("task presentation entry is not a map".into());
        };
        let mut fields = serde_json::Map::new();
        fields.insert("run".into(), serde_json::Value::String(run.clone()));
        for field in [
            "sequence",
            "received_at",
            "finish",
            "text",
            "reasoning",
            "detail",
        ] {
            let value = if matches!(field, "text" | "reasoning" | "detail") {
                let Some((Value::Object(ObjType::Text), id)) =
                    doc.get(&entry, field).map_err(|e| e.to_string())?
                else {
                    return Err("task presentation text is missing".into());
                };
                serde_json::Value::String(doc.text(id).map_err(|e| e.to_string())?)
            } else {
                let Some((Value::Scalar(value), _)) =
                    doc.get(&entry, field).map_err(|e| e.to_string())?
                else {
                    return Err("task presentation field is missing".into());
                };
                let ScalarValue::Str(value) = value.as_ref() else {
                    return Err("task presentation field is not encoded JSON".into());
                };
                serde_json::from_str(value).map_err(|e| e.to_string())?
            };
            fields.insert(field.into(), value);
        }
        result.insert(
            run,
            serde_json::from_value(serde_json::Value::Object(fields)).map_err(|e| e.to_string())?,
        );
    }
    Ok(Some(result))
}

pub(super) fn project(
    doc: &mut AutoCommit,
    task: &str,
    presentations: &BTreeMap<String, TaskPresentation>,
) -> Result<(), String> {
    let root = map(doc, &ROOT, KEY)?;
    let runs = map(doc, &root, task)?;
    let removed: Vec<_> = doc
        .keys(&runs)
        .filter(|run| !presentations.contains_key(run))
        .collect();
    for run in removed {
        doc.delete(&runs, run).map_err(|e| e.to_string())?;
    }
    for (run, presentation) in presentations {
        let entry = map(doc, &runs, run)?;
        for (field, text) in [
            ("text", &presentation.text),
            ("reasoning", &presentation.reasoning),
            ("detail", &presentation.detail),
        ] {
            let id = match doc.get(&entry, field).map_err(|e| e.to_string())? {
                Some((Value::Object(ObjType::Text), id)) => id,
                Some(_) => return Err("task presentation text has wrong type".into()),
                None => doc
                    .put_object(&entry, field, ObjType::Text)
                    .map_err(|e| e.to_string())?,
            };
            if doc.text(&id).map_err(|e| e.to_string())? != *text {
                // Automerge diffs on Unicode scalar boundaries, retaining unchanged
                // prefixes and suffixes rather than tombstoning the full snapshot.
                doc.update_text(&id, text).map_err(|e| e.to_string())?;
            }
        }
        for (field, value) in [
            ("sequence", serde_json::to_string(&presentation.sequence)),
            (
                "received_at",
                serde_json::to_string(&presentation.received_at),
            ),
            ("finish", serde_json::to_string(&presentation.finish)),
        ] {
            let value = value.map_err(|e| e.to_string())?;
            put_changed(doc, &entry, field, ScalarValue::from(value))?;
        }
    }
    Ok(())
}

pub(super) fn put_changed(
    doc: &mut AutoCommit,
    map: &automerge::ObjId,
    key: &str,
    value: ScalarValue,
) -> Result<(), String> {
    if doc
        .get(map, key)
        .map_err(|e| e.to_string())?
        .is_some_and(|(old, _)| old == Value::from(value.clone()))
    {
        return Ok(());
    }
    doc.put(map, key, value).map_err(|e| e.to_string())
}
