//! Outline as a migration source: its collections and documents, through Outline's API
//! (`POST /api/<method>` with a bearer key, answers as `{ "data": … }`).
//!
//! A collection becomes a top-level page and every document a page under its collection, or
//! under the document Outline nests it in. Pictures a document shows are fetched and kept with
//! its page, and a link to another document becomes a link to that document's page.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use super::http::{self, Key};
use super::landing::{IncomingPage, Picture};
use super::{ReviewGroup, ReviewItem, ServiceError, ServiceReview};

const PAGE: u64 = 100;
const COLLECTION: &str = "collection:";

fn call(server: &str, key: &str, method: &str, body: &Value) -> Result<Value, ServiceError> {
    http::post_json(&format!("{server}/api/{method}"), body, key, Key::Bearer)
}

/// The team the key belongs to, as Outline names it.
pub fn check(server: &str, key: &str) -> Result<String, ServiceError> {
    let answer = call(server, key, "auth.info", &json!({}))?;
    let team = text(answer.pointer("/data/team").unwrap_or(&Value::Null), "name");
    let user = text(answer.pointer("/data/user").unwrap_or(&Value::Null), "name");
    Ok(if team.is_empty() {
        format!("Outline ({user})")
    } else {
        format!("Outline: {team}")
    })
}

/// Every `data` entry of a paged `method`.
fn listed(server: &str, key: &str, method: &str, body: &Value) -> Result<Vec<Value>, ServiceError> {
    let mut all = Vec::new();
    let mut offset = 0_u64;
    loop {
        let mut request = body.clone();
        request["limit"] = json!(PAGE);
        request["offset"] = json!(offset);
        let answer = call(server, key, method, &request)?;
        let data = answer
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let count = data.len() as u64;
        all.extend(data);
        if all.len() > super::MAX_REVIEW_ITEMS {
            return Err(ServiceError::TooLarge);
        }
        if count < PAGE {
            return Ok(all);
        }
        offset += PAGE;
    }
}

struct Source {
    collections: Vec<Value>,
    documents: BTreeMap<String, Vec<Value>>,
}

fn read(server: &str, key: &str) -> Result<Source, ServiceError> {
    let collections = listed(server, key, "collections.list", &json!({}))?;
    let mut documents = BTreeMap::new();
    for collection in &collections {
        let id = text(collection, "id");
        let listed = listed(
            server,
            key,
            "documents.list",
            &json!({ "collectionId": id }),
        )?;
        documents.insert(id.to_owned(), parents_first(listed));
    }
    Ok(Source {
        collections,
        documents,
    })
}

/// Documents ordered so a document comes after the one it is nested in.
fn parents_first(documents: Vec<Value>) -> Vec<Value> {
    let ids: BTreeSet<String> = documents.iter().map(|d| text(d, "id").to_owned()).collect();
    let mut placed = BTreeSet::new();
    let mut ordered = Vec::with_capacity(documents.len());
    let mut rest = documents;
    while !rest.is_empty() {
        let (ready, waiting): (Vec<Value>, Vec<Value>) = rest.into_iter().partition(|document| {
            parent_document(document)
                .is_none_or(|parent| placed.contains(parent) || !ids.contains(parent))
        });
        if ready.is_empty() {
            // A cycle the source should never hold: keep the rest in the order listed.
            ordered.extend(waiting);
            break;
        }
        for document in &ready {
            placed.insert(text(document, "id").to_owned());
        }
        ordered.extend(ready);
        rest = waiting;
    }
    ordered
}

fn parent_document(document: &Value) -> Option<&str> {
    document
        .get("parentDocumentId")
        .and_then(Value::as_str)
        .filter(|parent| !parent.is_empty())
}

/// The collections as groups, each listing itself and its documents.
pub fn review(server: &str, key: &str) -> Result<Vec<ReviewGroup>, ServiceError> {
    let source = read(server, key)?;
    let titles: HashMap<&str, &str> = source
        .documents
        .values()
        .flatten()
        .map(|document| (text(document, "id"), text(document, "title")))
        .collect();
    Ok(source
        .collections
        .iter()
        .map(|collection| {
            let id = text(collection, "id");
            let name = text(collection, "name");
            let mut items = vec![ReviewItem {
                key: format!("{COLLECTION}{id}"),
                title: name.to_owned(),
                facts: Vec::new(),
            }];
            items.extend(
                source
                    .documents
                    .get(id)
                    .into_iter()
                    .flatten()
                    .map(|document| ReviewItem {
                        key: text(document, "id").to_owned(),
                        title: titled(text(document, "title")),
                        facts: std::iter::once(name.to_owned())
                            .chain(
                                parent_document(document)
                                    .and_then(|parent| titles.get(parent))
                                    .map(ToString::to_string),
                            )
                            .collect(),
                    }),
            );
            ReviewGroup {
                key: id.to_owned(),
                title: name.to_owned(),
                items,
                chosen: true,
            }
        })
        .collect())
}

pub struct Pages {
    pub pages: Vec<IncomingPage>,
    pub not_reviewed: usize,
    pub gone: Vec<String>,
}

/// The reviewed, chosen pages as Outline holds them now, parents before their children.
pub fn pages(
    server: &str,
    key: &str,
    review: &ServiceReview,
    chosen: &BTreeSet<String>,
    cancel: &AtomicBool,
) -> Result<Pages, ServiceError> {
    let source = read(server, key)?;
    let mut titles = HashMap::new();
    for document in source.documents.values().flatten() {
        let title = titled(text(document, "title"));
        titles.insert(text(document, "id").to_owned(), title.clone());
        let url_id = text(document, "urlId");
        if !url_id.is_empty() {
            titles.insert(url_id.to_owned(), title);
        }
    }
    let chosen_groups: BTreeSet<&str> = review
        .groups
        .iter()
        .filter(|group| group.chosen)
        .map(|group| group.key.as_str())
        .collect();
    let mut found = BTreeSet::new();
    let mut not_reviewed = 0;
    let mut pages = Vec::new();
    for collection in &source.collections {
        let id = text(collection, "id");
        let collection_key = format!("{COLLECTION}{id}");
        if chosen.contains(&collection_key) {
            found.insert(collection_key.clone());
            pages.push(IncomingPage {
                source: "outline".into(),
                key: collection_key.clone(),
                title: titled(text(collection, "name")),
                markdown: text(collection, "description").to_owned(),
                parent: None,
                url: format!("{server}{}", text(collection, "url")),
                pictures: Vec::new(),
            });
        }
        for document in source.documents.get(id).into_iter().flatten() {
            if cancel.load(Ordering::Relaxed) {
                return Err(ServiceError::Cancelled);
            }
            let document_id = text(document, "id");
            if !chosen.contains(document_id) {
                if chosen_groups.contains(id) {
                    not_reviewed += 1;
                }
                continue;
            }
            found.insert(document_id.to_owned());
            let (markdown, pictures) = rewrite(text(document, "text"), server, key, &titles);
            pages.push(IncomingPage {
                source: "outline".into(),
                key: document_id.to_owned(),
                title: titled(text(document, "title")),
                markdown,
                parent: Some(
                    parent_document(document)
                        .filter(|parent| chosen.contains(*parent))
                        .map_or_else(|| collection_key.clone(), str::to_owned),
                )
                .filter(|parent| chosen.contains(parent)),
                url: format!("{server}{}", text(document, "url")),
                pictures,
            });
        }
    }
    let gone = chosen
        .iter()
        .filter(|key| !found.contains(*key))
        .cloned()
        .collect();
    Ok(Pages {
        pages,
        not_reviewed,
        gone,
    })
}

/// `markdown` with every picture it shows kept as `asset:<digest>`, every link to another
/// Outline document as that document's `[[title]]`, and every other link on the Outline host
/// made absolute so it still leads there.
pub fn rewrite(
    markdown: &str,
    server: &str,
    key: &str,
    titles: &HashMap<String, String>,
) -> (String, Vec<Picture>) {
    let mut pictures: Vec<Picture> = Vec::new();
    let mut output = String::with_capacity(markdown.len());
    let mut rest = markdown;
    while let Some(open) = rest.find('[') {
        let picture = open > 0 && rest.as_bytes().get(open - 1) == Some(&b'!');
        let start = if picture { open - 1 } else { open };
        let Some((text_end, destination_start, end)) = link_at(rest, open) else {
            output.push_str(part(rest, 0, open + 1));
            rest = part(rest, open + 1, rest.len());
            continue;
        };
        output.push_str(part(rest, 0, start));
        let label = part(rest, open + 1, text_end);
        let target = part(rest, destination_start, end).trim();
        let destination = target
            .split_once(char::is_whitespace)
            .map_or(target, |(destination, _)| destination)
            .trim_matches(['<', '>']);
        if picture {
            if let Some(bytes) = http::picture(destination, server, key, Key::Bearer) {
                let digest = blake3::hash(&bytes).to_hex().to_string();
                if !pictures.iter().any(|kept| kept.digest == digest) {
                    pictures.push(Picture {
                        digest: digest.clone(),
                        bytes,
                    });
                }
                output.push_str(&format!("![{label}](asset:{digest})"));
            } else {
                output.push_str(&format!("![{label}]({})", absolute(destination, server)));
            }
        } else if let Some(title) = document_link(destination, server).and_then(|id| titles.get(id))
        {
            output.push_str(&format!("[[{title}]]"));
        } else {
            output.push_str(&format!("[{label}]({})", absolute(destination, server)));
        }
        rest = part(rest, end + 1, rest.len());
    }
    output.push_str(rest);
    (output, pictures)
}

/// `text` from byte `from` to byte `to`, both found at ASCII delimiters.
fn part(text: &str, from: usize, to: usize) -> &str {
    text.get(from..to).unwrap_or_default()
}

/// For `[text](destination)` opening at `open`: the end of its text, the start of its
/// destination and the closing parenthesis.
fn link_at(text: &str, open: usize) -> Option<(usize, usize, usize)> {
    let after = text.get(open + 1..)?;
    let close = after.find(']')?;
    if after.get(..close)?.contains('\n') {
        return None;
    }
    let text_end = open + 1 + close;
    if text.as_bytes().get(text_end + 1) != Some(&b'(') {
        return None;
    }
    let destination_start = text_end + 2;
    let mut depth = 0_usize;
    for (offset, byte) in text.get(destination_start..)?.bytes().enumerate() {
        match byte {
            b'\n' => return None,
            b'(' => depth += 1,
            b')' if depth == 0 => {
                return Some((text_end, destination_start, destination_start + offset));
            }
            b')' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// The document id or `urlId` a link to an Outline document names.
fn document_link<'a>(destination: &'a str, server: &str) -> Option<&'a str> {
    let path = destination
        .strip_prefix(server)
        .unwrap_or(destination)
        .split(['#', '?'])
        .next()?;
    let slug = path.strip_prefix("/doc/")?.trim_end_matches('/');
    Some(slug.rsplit_once('-').map_or(slug, |(_, id)| id))
}

fn absolute(destination: &str, server: &str) -> String {
    if destination.starts_with('/') && !destination.starts_with("//") {
        format!("{server}{destination}")
    } else {
        destination.to_owned()
    }
}

fn titled(title: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        "Untitled".to_owned()
    } else {
        title.to_owned()
    }
}

fn text<'a>(value: &'a Value, name: &str) -> &'a str {
    value.get(name).and_then(Value::as_str).unwrap_or_default()
}
