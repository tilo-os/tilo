//! Replayable mutations for Browser's shared library and preferences.
//!
//! Navigation and the visit log remain local. Bookmarks and preferences are
//! durable decisions, so every visible control, Ctrl+K and agent uses this
//! deterministic document-action path.

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ROOT, ReadDoc, Value};
use serde::{Deserialize, Serialize};
use tilo_actions::{Action, ActionContext, ActionError, ActionRegistry, Doc, ExecutionLocus};

use crate::library::{Bookmark, Bookmarks};
use crate::settings::{ADBLOCK_PATH, CLEAR_ON_EXIT_PATH, HOMEPAGE_PATH, SEARCH_ENGINE_PATH};

pub const BROWSER_ACTION_TYPE: &str = "browser.action";
/// Legacy scalar JSON; read once inside an action and migrated into `V2`.
const LEGACY_BOOKMARKS_PATH: &str = "browser.bookmarks";
/// Prefix for V2's root-level add/tombstone keys. Keeping entries directly at
/// `ROOT` is deliberate: concurrent first actions must never contend on a
/// newly-created parent map object.
pub const BOOKMARKS_V2_PATH: &str = "browser.bookmarks.v2";
const MIGRATED_PATH: &str = "browser.bookmarks.v2.migrated";
const ADD_PREFIX: &str = "browser.bookmarks.v2.add:";
const REMOVE_PREFIX: &str = "browser.bookmarks.v2.remove:";
const URL: &str = "url";
const TITLE: &str = "title";
const MAX_BOOKMARKS: usize = 10_000;
const MAX_WEB_URL_BYTES: usize = 8 * 1024;
const MAX_BOOKMARK_TITLE_BYTES: usize = 4 * 1024;

/// Stable identity for a private history entry.  History is deliberately not
/// copied into the shared document (visited URLs are sensitive activity), but
/// it still needs the same canonical identity everywhere it is exposed: the
/// library tree, global search, and an agent's resource address.
pub fn history_resource_id(url: &str) -> String {
    format!(
        "browser.history.{}",
        blake3::hash(url.trim().as_bytes()).to_hex()
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BrowserAction {
    /// `tag` is minted when the action descriptor is made, never during
    /// replay. This makes the per-bookmark observed-remove set merge-safe.
    AddBookmark {
        url: String,
        title: String,
        tag: String,
    },
    /// Only observed tags are tombstoned, so a concurrent add is never lost.
    RemoveBookmark {
        url: String,
        observed_tags: Vec<String>,
    },
    SetAdblock {
        enabled: bool,
    },
    SetClearOnExit {
        enabled: bool,
    },
    SetHomepage {
        url: String,
    },
    SetSearchEngine {
        template: String,
    },
}

pub fn normalized_bookmark_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() || url.len() > MAX_WEB_URL_BYTES {
        return None;
    }
    let parsed = url::Url::parse(url).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| parsed.into())
}

/// Stable identity is based on the normalized URL, not transient title text.
pub fn bookmark_resource_id(url: &str) -> String {
    let normalized = normalized_bookmark_url(url).unwrap_or_else(|| url.trim().to_string());
    format!(
        "browser.bookmark.{}",
        blake3::hash(normalized.as_bytes()).to_hex()
    )
}

fn bookmark_key(url: &str) -> String {
    bookmark_resource_id(url)
        .strip_prefix("browser.bookmark.")
        .unwrap_or_default()
        .to_string()
}

/// Context attached to durable Browser actions in workspace history.
pub fn history_context(action: &BrowserAction) -> ActionContext {
    let mut context = ActionContext::for_action("so.tilo.browser", "Browser", action);
    let (label, id, kind, resource_label) = match action {
        BrowserAction::AddBookmark { url, title, .. } => (
            "Add bookmark",
            bookmark_resource_id(url),
            "browser.bookmark",
            if title.trim().is_empty() {
                url.clone()
            } else {
                title.clone()
            },
        ),
        BrowserAction::RemoveBookmark { url, .. } => (
            "Remove bookmark",
            bookmark_resource_id(url),
            "browser.bookmark",
            url.clone(),
        ),
        BrowserAction::SetAdblock { .. } => settings_context("Change ad blocking"),
        BrowserAction::SetClearOnExit { .. } => settings_context("Change clear-on-exit"),
        BrowserAction::SetHomepage { .. } => settings_context("Set homepage"),
        BrowserAction::SetSearchEngine { .. } => settings_context("Set search engine"),
    };
    context.action_label = label.into();
    context.resource = Some(tilo_actions::ActionResource {
        id,
        kind: kind.into(),
        label: resource_label,
    });
    context
}

fn settings_context(label: &'static str) -> (&'static str, String, &'static str, String) {
    (
        label,
        "browser.settings".into(),
        "browser.settings",
        "Settings".into(),
    )
}

fn ensure_v2_migrated(doc: &mut Doc) -> Result<(), ActionError> {
    // Migration is action-owned, so it is replayable and not an untracked
    // bootstrap write. Legacy rows get stable deterministic tags.
    if doc.get(&ROOT, MIGRATED_PATH)?.is_none() {
        let legacy = doc
            .get(&ROOT, LEGACY_BOOKMARKS_PATH)?
            .and_then(|(value, _)| value.into_scalar().ok())
            .and_then(|value| value.as_str().map(str::to_string))
            .map_or_else(Bookmarks::default, |json| Bookmarks::from_json(&json));
        for entry in legacy.entries() {
            let Some(url) = normalized_bookmark_url(&entry.url) else {
                continue;
            };
            let key = bookmark_key(&url);
            let tag = format!("legacy:{key}");
            put_add(doc, &ROOT, &tag, &url, &entry.title)?;
        }
        doc.put(&ROOT, MIGRATED_PATH, true)?;
    }
    Ok(())
}

/// Adds and tombstones live directly below the shared V2 map. Tags are
/// globally unique per add, so this avoids a contested per-URL map object
/// when two replicas bookmark the same new URL concurrently.
fn add_key(tag: &str) -> String {
    format!("{ADD_PREFIX}{tag}")
}

fn remove_key(tag: &str) -> String {
    format!("{REMOVE_PREFIX}{tag}")
}

fn put_add(
    doc: &mut Doc,
    root: &ObjId,
    tag: &str,
    url: &str,
    title: &str,
) -> Result<(), ActionError> {
    let key = add_key(tag);
    if doc.get(root, &key)?.is_none() {
        let add = doc.put_object(root, key, ObjType::Map)?;
        doc.put(&add, URL, url)?;
        doc.put(&add, TITLE, title)?;
    }
    Ok(())
}

fn read_string(doc: &Doc, parent: &ObjId, key: &str) -> Option<String> {
    doc.get(parent, key)
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_scalar().ok())
        .and_then(|value| value.as_str().map(str::to_string))
}

/// Project the CRDT OR-set into the compact view model used by the chrome.
/// `None` means the document has not migrated yet, so callers may retain the
/// local legacy cache until the first bookmark action.
pub fn bookmarks_from_doc(doc: &Doc) -> Option<Bookmarks> {
    let has_v2 = doc.get(&ROOT, MIGRATED_PATH).ok().flatten().is_some()
        || doc.keys(&ROOT).any(|key| key.starts_with(ADD_PREFIX));
    if !has_v2 {
        return None;
    }
    let mut rows = Vec::new();
    for key in doc.keys(&ROOT) {
        let Some(tag) = key.strip_prefix(ADD_PREFIX) else {
            continue;
        };
        if doc.get(&ROOT, remove_key(tag)).ok().flatten().is_some() {
            continue;
        }
        let Some((Value::Object(ObjType::Map), add)) = doc.get(&ROOT, key).ok().flatten() else {
            continue;
        };
        let Some(url) = read_string(doc, &add, URL) else {
            continue;
        };
        let Some(title) = read_string(doc, &add, TITLE) else {
            continue;
        };
        rows.push(Bookmark { url, title });
    }
    rows.sort_by(|a, b| a.url.cmp(&b.url).then_with(|| a.title.cmp(&b.title)));
    rows.dedup_by(|a, b| a.url == b.url);
    rows.truncate(MAX_BOOKMARKS);
    Some(Bookmarks::from_entries(rows))
}

pub fn observed_bookmark_tags(doc: &Doc, url: &str) -> Vec<String> {
    let Some(url) = normalized_bookmark_url(url) else {
        return Vec::new();
    };
    let has_v2 = doc.get(&ROOT, MIGRATED_PATH).ok().flatten().is_some()
        || doc.keys(&ROOT).any(|key| key.starts_with(ADD_PREFIX));
    if !has_v2 {
        let legacy = doc
            .get(&ROOT, LEGACY_BOOKMARKS_PATH)
            .ok()
            .flatten()
            .and_then(|(value, _)| value.into_scalar().ok())
            .and_then(|value| value.as_str().map(str::to_string))
            .map_or_else(Bookmarks::default, |json| Bookmarks::from_json(&json));
        return if legacy
            .entries()
            .iter()
            .any(|entry| normalized_bookmark_url(&entry.url).as_deref() == Some(url.as_str()))
        {
            vec![format!("legacy:{}", bookmark_key(&url))]
        } else {
            Vec::new()
        };
    }
    let mut tags: Vec<_> = doc
        .keys(&ROOT)
        .filter_map(|key| key.strip_prefix(ADD_PREFIX).map(str::to_string))
        .filter(|tag| {
            doc.get(&ROOT, remove_key(tag)).ok().flatten().is_none()
                && doc
                    .get(&ROOT, add_key(tag))
                    .ok()
                    .flatten()
                    .and_then(|(value, id)| {
                        matches!(value, Value::Object(ObjType::Map)).then_some(id)
                    })
                    .and_then(|add| read_string(doc, &add, URL))
                    .as_deref()
                    == Some(url.as_str())
        })
        .collect();
    tags.sort();
    tags
}

fn replace_text(doc: &mut Doc, key: &str, text: &str) -> Result<(), ActionError> {
    if let Some((Value::Object(ObjType::Text), object)) = doc.get(&ROOT, key)? {
        doc.splice_text(&object, 0, doc.length(&object) as isize, text)?;
    } else {
        let object = doc.put_object(&ROOT, key, ObjType::Text)?;
        if !text.is_empty() {
            doc.splice_text(&object, 0, 0, text)?;
        }
    }
    Ok(())
}

fn valid_web_url(value: &str, field: &str) -> Result<(), ActionError> {
    normalized_bookmark_url(value)
        .filter(|url| !url.trim().is_empty())
        .map(|_| ())
        .ok_or_else(|| ActionError::Refused(format!("{field} must be an absolute http(s) URL")))
}

impl Action for BrowserAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            Self::AddBookmark { url, title, tag } => {
                let url = normalized_bookmark_url(url).ok_or_else(|| {
                    ActionError::Refused("bookmark must be an absolute http(s) URL".into())
                })?;
                if tag.trim().is_empty() {
                    return Err(ActionError::Refused("bookmark tag is required".into()));
                }
                if tag.len() > 256 || title.len() > MAX_BOOKMARK_TITLE_BYTES {
                    return Err(ActionError::Refused(
                        "bookmark metadata exceeds the safety limit".into(),
                    ));
                }
                ensure_v2_migrated(doc)?;
                let title = if title.trim().is_empty() {
                    url.clone()
                } else {
                    title.trim().to_string()
                };
                put_add(doc, &ROOT, tag, &url, &title)?;
            }
            Self::RemoveBookmark { url, observed_tags } => {
                normalized_bookmark_url(url).ok_or_else(|| {
                    ActionError::Refused("bookmark must be an absolute http(s) URL".into())
                })?;
                ensure_v2_migrated(doc)?;
                for tag in observed_tags.iter().filter(|tag| !tag.trim().is_empty()) {
                    doc.put(&ROOT, remove_key(tag), true)?;
                }
            }
            Self::SetAdblock { enabled } => doc.put(&ROOT, ADBLOCK_PATH, *enabled)?,
            Self::SetClearOnExit { enabled } => doc.put(&ROOT, CLEAR_ON_EXIT_PATH, *enabled)?,
            Self::SetHomepage { url } => {
                valid_web_url(url, "homepage")?;
                replace_text(doc, HOMEPAGE_PATH, url)?;
            }
            Self::SetSearchEngine { template } => {
                valid_web_url(template, "search engine")?;
                replace_text(doc, SEARCH_ENGINE_PATH, template)?;
            }
        }
        Ok(())
    }
    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

pub fn register(registry: &mut ActionRegistry) {
    registry.register::<BrowserAction>(BROWSER_ACTION_TYPE);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_automerge::ApplyIncoming;
    fn add(url: &str, tag: &str) -> BrowserAction {
        BrowserAction::AddBookmark {
            url: url.into(),
            title: "Example".into(),
            tag: tag.into(),
        }
    }

    #[test]
    fn concurrent_first_bookmark_actions_from_empty_doc_merge_distinct_rows() {
        let mut seed = Doc::new();
        seed.put(&ROOT, "seed", true).unwrap();
        seed.commit();
        let mut left = Doc::load(&seed.save()).unwrap();
        let mut right = Doc::load(&seed.save()).unwrap();
        add("https://one.example", "a").apply(&mut left).unwrap();
        left.commit();
        add("https://two.example", "b").apply(&mut right).unwrap();
        right.commit();
        left.merge_incoming(&mut right).unwrap();
        let rows = bookmarks_from_doc(&left).unwrap();
        assert!(rows.contains("https://one.example/"));
        assert!(rows.contains("https://two.example/"));
    }

    #[test]
    fn concurrent_remove_only_tombstones_observed_tags() {
        let mut doc = Doc::new();
        add("https://example.test", "old").apply(&mut doc).unwrap();
        doc.commit();
        let mut left = Doc::load(&doc.save()).unwrap();
        let mut right = Doc::load(&doc.save()).unwrap();
        let tags = observed_bookmark_tags(&left, "https://example.test");
        BrowserAction::RemoveBookmark {
            url: "https://example.test".into(),
            observed_tags: tags,
        }
        .apply(&mut left)
        .unwrap();
        left.commit();
        add("https://example.test", "new")
            .apply(&mut right)
            .unwrap();
        right.commit();
        left.merge_incoming(&mut right).unwrap();
        assert!(
            bookmarks_from_doc(&left)
                .unwrap()
                .contains("https://example.test/")
        );
    }

    #[test]
    fn concurrent_adds_for_the_same_new_bookmark_preserve_both_tags() {
        let mut seed = Doc::new();
        seed.put(&ROOT, "seed", true).unwrap();
        seed.commit();
        let mut left = Doc::load(&seed.save()).unwrap();
        let mut right = Doc::load(&seed.save()).unwrap();

        add("https://example.test", "left")
            .apply(&mut left)
            .unwrap();
        left.commit();
        add("https://example.test", "right")
            .apply(&mut right)
            .unwrap();
        right.commit();
        left.merge_incoming(&mut right).unwrap();

        assert_eq!(
            observed_bookmark_tags(&left, "https://example.test"),
            vec!["left", "right"],
            "a later observed-remove must be able to tombstone every concurrent add"
        );
    }

    #[test]
    fn concurrent_first_actions_migrate_legacy_rows_and_merge_distinct_urls() {
        let mut seed = Doc::new();
        seed.put(
            &ROOT,
            LEGACY_BOOKMARKS_PATH,
            r#"[{"url":"https://legacy.example","title":"Legacy"}]"#,
        )
        .unwrap();
        seed.commit();
        let mut left = Doc::load(&seed.save()).unwrap();
        let mut right = Doc::load(&seed.save()).unwrap();

        add("https://one.example", "left").apply(&mut left).unwrap();
        left.commit();
        add("https://two.example", "right")
            .apply(&mut right)
            .unwrap();
        right.commit();
        left.merge_incoming(&mut right).unwrap();

        let rows = bookmarks_from_doc(&left).unwrap();
        assert!(rows.contains("https://legacy.example/"));
        assert!(rows.contains("https://one.example/"));
        assert!(rows.contains("https://two.example/"));
    }

    #[test]
    fn concurrent_first_actions_migrate_legacy_rows_and_merge_same_url_tags() {
        let mut seed = Doc::new();
        seed.put(
            &ROOT,
            LEGACY_BOOKMARKS_PATH,
            r#"[{"url":"https://legacy.example","title":"Legacy"}]"#,
        )
        .unwrap();
        seed.commit();
        let mut left = Doc::load(&seed.save()).unwrap();
        let mut right = Doc::load(&seed.save()).unwrap();

        add("https://same.example", "left")
            .apply(&mut left)
            .unwrap();
        left.commit();
        add("https://same.example", "right")
            .apply(&mut right)
            .unwrap();
        right.commit();
        left.merge_incoming(&mut right).unwrap();

        assert_eq!(
            observed_bookmark_tags(&left, "https://same.example"),
            vec!["left", "right"]
        );
        assert!(
            bookmarks_from_doc(&left)
                .unwrap()
                .contains("https://legacy.example/")
        );
    }

    #[test]
    fn invalid_preference_is_refused_before_history_can_record_success() {
        assert!(matches!(
            BrowserAction::SetHomepage {
                url: "not a url".into()
            }
            .apply(&mut Doc::new()),
            Err(ActionError::Refused(_))
        ));
    }
}
