//! drivews, docws and the content server, server side.

use std::time::SystemTime;

use serde_json::{Value, json};
use test_http::{Reply, Request};
use unicode_normalization::UnicodeNormalization;

use super::{FakeFault, Node, State, has_web_session, json_reply, query};
use crate::api::split_id;

fn body(request: &Request) -> Value {
    serde_json::from_slice(&request.body).unwrap_or(Value::Null)
}

fn folder_id(zone: &str, doc: &str) -> String {
    format!("FOLDER::{zone}::{doc}")
}

fn file_id(zone: &str, doc: &str) -> String {
    format!("FILE::{zone}::{doc}")
}

fn drivewsid(doc: &str, node: &Node) -> String {
    if node.folder {
        folder_id(&node.zone, doc)
    } else {
        file_id(&node.zone, doc)
    }
}

/// The docwsid of a live node named by a drivewsid.
fn live(state: &State, id: &str) -> Option<String> {
    let doc = split_id(id).2;
    reachable(state, doc).then(|| doc.to_owned())
}

/// A node that exists and is not (below) a trashed folder.
fn reachable(state: &State, doc: &str) -> bool {
    let mut current = doc.to_owned();
    for _ in 0..256 {
        let Some(node) = state.nodes.get(&current) else {
            return false;
        };
        if node.trashed {
            return false;
        }
        if node.parent.is_empty() {
            return true;
        }
        current.clone_from(&node.parent);
    }
    false
}

/// Live children of folder `doc`, in name order.
pub fn children(state: &State, doc: &str) -> Vec<String> {
    let mut out: Vec<(String, String)> = state
        .nodes
        .iter()
        .filter(|(_, node)| node.parent == doc && !node.trashed)
        .map(|(child, node)| (node.name.clone(), child.clone()))
        .collect();
    out.sort();
    out.into_iter().map(|(_, child)| child).collect()
}

/// The docwsid at `path` below the root.
pub fn lookup(state: &State, path: &str) -> Option<String> {
    let mut current = "root".to_owned();
    for part in path.split('/').filter(|part| !part.is_empty()) {
        current = children(state, &current)
            .into_iter()
            .find(|child| state.nodes.get(child).is_some_and(|node| node.name == part))?;
    }
    Some(current)
}

fn taken(state: &State, parent: &str, name: &str, except: Option<&str>) -> bool {
    let wanted = name.to_lowercase();
    children(state, parent).iter().any(|child| {
        Some(child.as_str()) != except
            && state
                .nodes
                .get(child)
                .is_some_and(|node| node.name.to_lowercase() == wanted)
    })
}

fn bump(state: &mut State, doc: &str) {
    let etag = state.next("e");
    if let Some(node) = state.nodes.get_mut(doc) {
        node.etag = etag;
    }
}

/// A folder changed: its own etag moves (direct children only).
fn touch_folder(state: &mut State, doc: &str) {
    bump(state, doc);
    if let Some(node) = state.nodes.get_mut(doc) {
        node.modified = SystemTime::now();
    }
}

/// The item as drivews describes it. Names come back decomposed (NFD), as
/// Apple's do.
fn item_json(state: &State, doc: &str) -> Value {
    let Some(node) = state.nodes.get(doc) else {
        return Value::Null;
    };
    let full: String = node.name.nfd().collect();
    let (name, extension) = match full.rsplit_once('.') {
        Some((stem, extension)) if !node.folder && !stem.is_empty() => {
            (stem.to_owned(), Some(extension.to_owned()))
        }
        _ => (full, None),
    };
    let parent = state
        .nodes
        .get(&node.parent)
        .map(|parent| folder_id(&parent.zone, &node.parent));
    let mut value = json!({
        "drivewsid": drivewsid(doc, node),
        "docwsid": doc,
        "zone": node.zone,
        "name": name,
        "etag": node.etag,
        "type": if node.folder { "FOLDER" } else { "FILE" },
        "dateCreated": crate::time::format(node.created),
        "dateModified": crate::time::format(node.modified),
        "dateChanged": crate::time::format(node.modified),
    });
    if let Value::Object(map) = &mut value {
        if let Some(extension) = extension {
            map.insert("extension".into(), Value::String(extension));
        }
        if let Some(parent) = parent {
            map.insert("parentId".into(), Value::String(parent));
        }
        if !node.folder {
            map.insert("size".into(), json!(node.bytes.len()));
        }
    }
    value
}

fn with_children(state: &State, doc: &str) -> Value {
    let mut value = item_json(state, doc);
    let kids: Vec<Value> = children(state, doc)
        .iter()
        .map(|child| item_json(state, child))
        .collect();
    if let Value::Object(map) = &mut value {
        map.insert("numberOfItems".into(), json!(kids.len()));
        map.insert("directChildrenCount".into(), json!(kids.len()));
        map.insert("items".into(), Value::Array(kids));
        map.insert("status".into(), json!("OK"));
    }
    value
}

fn not_found(id: &str) -> Value {
    json!({"drivewsid": id, "status": "ID_INVALID"})
}

fn with_status(mut value: Value, status: &str) -> Value {
    if let Value::Object(map) = &mut value {
        map.insert("status".into(), json!(status));
    }
    value
}

fn session_refused(state: &State, request: &Request) -> Option<Reply> {
    if !has_web_session(state, request) {
        return Some(json_reply(421, &json!({"error": "Invalid global session"})));
    }
    if state.options.advanced_data_protection {
        return Some(json_reply(423, &json!({"error": "Missing PCS cookies"})));
    }
    None
}

pub fn drivews(state: &mut State, request: &Request, route: &str) -> Reply {
    if let Some(refused) = session_refused(state, request) {
        return refused;
    }
    let payload = body(request);
    match route {
        "retrieveItemDetailsInFolders" => {
            state.stats.listings = state.stats.listings.saturating_add(1);
            let answer: Vec<Value> = payload
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|entry| entry["drivewsid"].as_str())
                .map(|id| match live(state, id) {
                    Some(doc) if state.nodes.get(&doc).is_some_and(|node| node.folder) => {
                        with_children(state, &doc)
                    }
                    Some(doc) => item_json(state, &doc),
                    None => not_found(id),
                })
                .collect();
            json_reply(200, &Value::Array(answer))
        }
        "retrieveItemDetails" => {
            let answer: Vec<Value> = payload
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|group| group["items"].as_array().cloned().unwrap_or_default())
                .filter_map(|entry| entry["drivewsid"].as_str().map(ToOwned::to_owned))
                .map(|id| match live(state, &id) {
                    Some(doc) => item_json(state, &doc),
                    None => not_found(&id),
                })
                .collect();
            json_reply(200, &Value::Array(answer))
        }
        "createFolders" => create_folders(state, &payload),
        "renameItems" => per_item(state, &payload, |state, doc, entry| {
            let Some(name) = entry["name"].as_str() else {
                return Err("INVALID_NAME");
            };
            let parent = state
                .nodes
                .get(doc)
                .map(|node| node.parent.clone())
                .unwrap_or_default();
            if taken(state, &parent, name, Some(doc)) {
                return Err("NAME_ALREADY_EXISTS");
            }
            if let Some(node) = state.nodes.get_mut(doc) {
                name.nfc().collect::<String>().clone_into(&mut node.name);
            }
            touch_folder(state, &parent);
            Ok(())
        }),
        "moveItems" => {
            let Some(destination) = payload["destinationDrivewsId"]
                .as_str()
                .and_then(|id| live(state, id))
                .filter(|doc| state.nodes.get(doc).is_some_and(|node| node.folder))
            else {
                return json_reply(400, &json!({"error": "bad destination"}));
            };
            per_item(state, &payload, |state, doc, _| {
                let Some(node) = state.nodes.get(doc) else {
                    return Err("ID_INVALID");
                };
                let (old_parent, name) = (node.parent.clone(), node.name.clone());
                if taken(state, &destination, &name, Some(doc)) {
                    return Err("NAME_ALREADY_EXISTS");
                }
                if let Some(node) = state.nodes.get_mut(doc) {
                    node.parent.clone_from(&destination);
                }
                touch_folder(state, &old_parent);
                touch_folder(state, &destination);
                Ok(())
            })
        }
        "moveItemsToTrash" => per_item(state, &payload, |state, doc, _| {
            let parent = state
                .nodes
                .get(doc)
                .map(|node| node.parent.clone())
                .unwrap_or_default();
            if let Some(node) = state.nodes.get_mut(doc) {
                node.trashed = true;
            }
            touch_folder(state, &parent);
            Ok(())
        }),
        _ => Reply::status(404),
    }
}

/// Apply `change` to each `items[]` entry after its etag check; answer
/// `{"items": [...]}` with a per-item status.
fn per_item(
    state: &mut State,
    payload: &Value,
    mut change: impl FnMut(&mut State, &str, &Value) -> Result<(), &'static str>,
) -> Reply {
    let mut answers = Vec::new();
    for entry in payload["items"].as_array().into_iter().flatten() {
        let id = entry["drivewsid"].as_str().unwrap_or_default();
        let Some(doc) = live(state, id) else {
            answers.push(not_found(id));
            continue;
        };
        let current = state
            .nodes
            .get(&doc)
            .map(|node| node.etag.clone())
            .unwrap_or_default();
        if entry["etag"].as_str() != Some(current.as_str()) {
            answers.push(with_status(item_json(state, &doc), "ETAG_CONFLICT"));
            continue;
        }
        match change(state, &doc, entry) {
            Ok(()) => {
                bump(state, &doc);
                answers.push(with_status(item_json(state, &doc), "OK"));
            }
            Err(status) => answers.push(with_status(item_json(state, &doc), status)),
        }
    }
    json_reply(200, &json!({"items": answers}))
}

fn create_folders(state: &mut State, payload: &Value) -> Reply {
    let Some(parent) = payload["destinationDrivewsId"]
        .as_str()
        .and_then(|id| live(state, id))
        .filter(|doc| state.nodes.get(doc).is_some_and(|node| node.folder))
    else {
        return json_reply(400, &json!({"error": "bad destination"}));
    };
    let zone = state
        .nodes
        .get(&parent)
        .map(|node| node.zone.clone())
        .unwrap_or_default();
    let mut answers = Vec::new();
    for folder in payload["folders"].as_array().into_iter().flatten() {
        let name: String = folder["name"].as_str().unwrap_or_default().nfc().collect();
        if name.is_empty() || taken(state, &parent, &name, None) {
            answers.push(json!({"clientId": folder["clientId"], "status": "NAME_ALREADY_EXISTS"}));
            continue;
        }
        let doc = uuid::Uuid::new_v4().to_string().to_uppercase();
        let etag = state.next("e");
        let now = SystemTime::now();
        state.nodes.insert(
            doc.clone(),
            Node {
                folder: true,
                zone: zone.clone(),
                name,
                parent: parent.clone(),
                etag,
                bytes: Vec::new(),
                created: now,
                modified: now,
                trashed: false,
            },
        );
        touch_folder(state, &parent);
        let mut value = with_status(item_json(state, &doc), "OK");
        if let Value::Object(map) = &mut value {
            map.insert("clientId".into(), folder["clientId"].clone());
        }
        answers.push(value);
    }
    json_reply(200, &json!({"folders": answers}))
}

pub fn docws(state: &mut State, request: &Request, route: &str) -> Reply {
    if let Some(refused) = session_refused(state, request) {
        return refused;
    }
    let Some(rest) = route.strip_prefix("ws/") else {
        return Reply::status(404);
    };
    let Some((zone, action)) = rest.split_once('/') else {
        return Reply::status(404);
    };
    match action {
        "download/by_id" => {
            let doc = query(request, "document_id").unwrap_or_default();
            if !reachable(state, &doc) || state.nodes.get(&doc).is_none_or(|node| node.folder) {
                return json_reply(404, &json!({"error": "not found"}));
            }
            let token = state.next("dl-");
            state.downloads.insert(token.clone(), doc.clone());
            let url = format!("{}/content/{token}", state.base);
            json_reply(
                200,
                &json!({
                    "document_id": doc,
                    "item_id": doc,
                    "owner_dsid": 12345,
                    "data_token": {"url": url, "token": token, "signature": "sig", "wrapping_key": "wk"},
                    "double_etag": "x::y"
                }),
            )
        }
        "upload/web" => {
            if query(request, "token").is_none() {
                return json_reply(401, &json!({"error": "missing token"}));
            }
            let payload = body(request);
            if payload["filename"].as_str().is_none_or(str::is_empty)
                || payload["content_type"].as_str().is_none_or(str::is_empty)
            {
                return json_reply(400, &json!({"error": "bad upload request"}));
            }
            let doc = uuid::Uuid::new_v4().to_string().to_uppercase();
            state.slots.insert(doc.clone(), zone.to_owned());
            let url = format!("{}/content/upload/{doc}", state.base);
            json_reply(
                200,
                &json!([{"document_id": doc, "url": url, "owner": "12345"}]),
            )
        }
        "update/documents" => update_documents(state, zone, &body(request)),
        _ => Reply::status(404),
    }
}

fn doc_status(code: i64) -> Value {
    json!({"status_code": code, "error_message": if code == 0 { "" } else { "refused" }})
}

fn update_documents(state: &mut State, zone: &str, payload: &Value) -> Reply {
    let refuse = |code: i64| json_reply(200, &json!({"status": doc_status(code), "results": []}));
    let document = payload["document_id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let signature = payload["data"]["signature"].as_str().unwrap_or_default();
    let Some(bytes) = state.uploads.get(signature).cloned() else {
        return refuse(2);
    };
    if payload["data"]["size"].as_u64() != Some(bytes.len() as u64) {
        return refuse(3);
    }
    let parent = payload["path"]["starting_document_id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let name: String = payload["path"]["path"]
        .as_str()
        .unwrap_or_default()
        .nfc()
        .collect();
    if !reachable(state, &parent) || name.is_empty() || name.contains('/') {
        return refuse(4);
    }
    let now = SystemTime::now();
    let target = if state.slots.remove(&document).is_some() {
        // A fresh document: link it in, renaming on a clash as Apple does
        // with `allow_conflict`.
        let mut final_name = name.clone();
        let mut counter = 2_u32;
        while taken(state, &parent, &final_name, None) {
            final_name = match name.rsplit_once('.') {
                Some((stem, extension)) => format!("{stem} {counter}.{extension}"),
                None => format!("{name} {counter}"),
            };
            counter = counter.saturating_add(1);
        }
        let etag = state.next("e");
        state.nodes.insert(
            document.clone(),
            Node {
                folder: false,
                zone: zone.to_owned(),
                name: final_name,
                parent: parent.clone(),
                etag,
                bytes,
                created: now,
                modified: now,
                trashed: false,
            },
        );
        touch_folder(state, &parent);
        document
    } else if reachable(state, &document)
        && state.nodes.get(&document).is_some_and(|node| !node.folder)
    {
        if !state.options.replace_in_place {
            return refuse(13);
        }
        if let Some(node) = state.nodes.get_mut(&document) {
            node.bytes = bytes;
            node.modified = now;
        }
        bump(state, &document);
        touch_folder(state, &parent);
        document
    } else {
        return refuse(5);
    };
    let Some(node) = state.nodes.get(&target) else {
        return refuse(6);
    };
    json_reply(
        200,
        &json!({
            "status": doc_status(0),
            "results": [{
                "status": doc_status(0),
                "operation_id": null,
                "document": {
                    "document_id": target,
                    "item_id": target,
                    "etag": node.etag,
                    "parent_id": node.parent,
                    "name": node.name,
                    "type": "FILE",
                    "size": node.bytes.len(),
                    "zone": node.zone,
                    "deleted": false
                }
            }]
        }),
    )
}

/// `bytes=a-b` → `a..b+1`, clamped to `len`.
fn range(header: &str, len: usize) -> Option<(usize, usize)> {
    let spec = header.trim().strip_prefix("bytes=")?;
    let (start, end) = spec.split_once('-')?;
    let start: usize = start.trim().parse().ok()?;
    let end: usize = if end.trim().is_empty() {
        len.checked_sub(1)?
    } else {
        end.trim().parse().ok()?
    };
    let end = end.min(len.checked_sub(1)?);
    (start <= end).then_some((start, end.saturating_add(1)))
}

pub fn content(state: &mut State, request: &Request, path: &str) -> Reply {
    if let Some(doc) = path.strip_prefix("/content/upload/") {
        return upload(state, request, doc);
    }
    let token = path.trim_start_matches("/content/");
    let Some(doc) = state.downloads.get(token).cloned() else {
        return Reply::status(404);
    };
    let Some(bytes) = state.nodes.get(&doc).map(|node| node.bytes.clone()) else {
        return Reply::status(404);
    };
    let truncate = state
        .faults
        .iter()
        .position(|fault| *fault == FakeFault::TruncatedBody)
        .and_then(|position| state.faults.remove(position))
        .is_some();
    let requested = request.header("range").and_then(|h| range(h, bytes.len()));
    let (status, served, content_range) = if let Some((start, end)) = requested {
        state.stats.ranged_downloads = state.stats.ranged_downloads.saturating_add(1);
        let served = bytes.get(start..end).unwrap_or_default().to_vec();
        let content_range = format!("bytes {start}-{}/{}", end.saturating_sub(1), bytes.len());
        (206, served, Some(content_range))
    } else {
        state.stats.full_downloads = state.stats.full_downloads.saturating_add(1);
        (200, bytes, None)
    };
    let mut reply = Reply::status(status).header("Content-Type", "application/octet-stream");
    if let Some(content_range) = content_range {
        reply = reply.header("Content-Range", &content_range);
    }
    if truncate {
        let half = served.len() / 2;
        return reply
            .without_content_length()
            .header("Content-Length", &served.len().to_string())
            .body(served.get(..half).unwrap_or_default().to_vec());
    }
    reply.body(served)
}

/// The file part of a `multipart/form-data` body.
fn multipart_file(request: &Request) -> Option<Vec<u8>> {
    let content_type = request.header("content-type")?;
    let boundary = content_type
        .split(';')
        .find_map(|part| part.trim().strip_prefix("boundary="))?
        .trim_matches('"');
    let body = &request.body;
    let opening = format!("--{boundary}\r\n");
    let closing = format!("\r\n--{boundary}--");
    if !body.starts_with(opening.as_bytes()) {
        return None;
    }
    let head_end = body
        .windows(4)
        .position(|window| window == b"\r\n\r\n")?
        .checked_add(4)?;
    let tail = body
        .windows(closing.len())
        .rposition(|window| window == closing.as_bytes())?;
    body.get(head_end..tail).map(<[u8]>::to_vec)
}

fn upload(state: &mut State, request: &Request, doc: &str) -> Reply {
    if !state.slots.contains_key(doc) {
        return Reply::status(404);
    }
    let Some(bytes) = multipart_file(request) else {
        return json_reply(400, &json!({"error": "not multipart"}));
    };
    state.stats.uploads = state.stats.uploads.saturating_add(1);
    let checksum = state.next("ck-");
    let size = bytes.len();
    state.uploads.insert(checksum.clone(), bytes);
    let mut single = json!({
        "referenceChecksum": format!("ref-{checksum}"),
        "fileChecksum": checksum,
        "wrappingKey": "wrapping-key",
        "size": size,
    });
    // Apple leaves the receipt out for empty files.
    if size > 0
        && let Value::Object(map) = &mut single
    {
        map.insert("receipt".into(), json!(format!("receipt-{checksum}")));
    }
    json_reply(200, &json!({"singleFile": single}))
}
