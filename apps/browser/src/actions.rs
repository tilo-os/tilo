//! Replayable mutations for Browser's shared library and preferences.
//!
//! Navigation and the visit log remain local. Bookmarks and preferences are
//! durable decisions, so every visible control, Ctrl+K and agent uses this
//! deterministic document-action path.

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ROOT, ReadDoc, Value};
use serde::{Deserialize, Serialize};
use tilo_actions::{Action, ActionError, ActionRegistry, Doc, ExecutionLocus};

use crate::library::{Bookmark, Bookmarks};
use crate::settings::{ADBLOCK_PATH, CLEAR_ON_EXIT_PATH, HOMEPAGE_PATH, SEARCH_ENGINE_PATH};

pub const BROWSER_ACTION_TYPE: &str = "browser.action";
/// Prefix for V2's root-level add/tombstone keys. Keeping entries directly at
/// `ROOT` is deliberate: concurrent first actions must never contend on a
/// newly-created parent map object.
pub const BOOKMARKS_V2_PATH: &str = "browser.bookmarks.v2";
const ADD_PREFIX: &str = "browser.bookmarks.v2.add:";
const REMOVE_PREFIX: &str = "browser.bookmarks.v2.remove:";
const URL: &str = "url";
const TITLE: &str = "title";
const MAX_BOOKMARKS: usize = 10_000;
const MAX_WEB_URL_BYTES: usize = 8 * 1024;
const MAX_BOOKMARK_TITLE_BYTES: usize = 4 * 1024;

/// Search scope and resource id of the history section. A history entry's id
/// lies beneath it, so a search narrowed to History admits exactly its hits.
pub const HISTORY_SCOPE: &str = "browser.history";
/// Search scope and resource id of the bookmarks section.
pub const BOOKMARKS_SCOPE: &str = "browser.bookmarks";
/// Search scope and resource id of the downloads section.
pub const DOWNLOADS_SCOPE: &str = "browser.downloads";

/// Stable identity for a private history entry.  History is deliberately not
/// copied into the shared document (visited URLs are sensitive activity), but
/// it still needs the same canonical identity everywhere it is exposed: the
/// library tree, global search, and an agent's resource address.
pub fn history_resource_id(url: &str) -> String {
    format!(
        "{HISTORY_SCOPE}/{}",
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
    /// Turn tracker protection on or off for one exact origin
    /// (`https://example.org`); off is a site exception.
    SetSiteProtection {
        origin: String,
        enabled: bool,
    },
}

/// Prefix of the root-level keys naming each origin protection is off for.
const SITE_EXCEPTION_PREFIX: &str = "browser.site-exception:";
/// The most site exceptions the document keeps.
pub const MAX_SITE_EXCEPTIONS: usize = 1024;

/// The origins protection is off for.
pub fn site_exceptions_from_doc(doc: &Doc) -> std::collections::BTreeSet<String> {
    doc.keys(&ROOT)
        .filter_map(|key| key.strip_prefix(SITE_EXCEPTION_PREFIX).map(str::to_owned))
        .take(MAX_SITE_EXCEPTIONS)
        .collect()
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
        "{BOOKMARKS_SCOPE}/{}",
        blake3::hash(normalized.as_bytes()).to_hex()
    )
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
/// `None` means the document holds no bookmark yet, so callers may retain the
/// local cache until the first bookmark action.
pub fn bookmarks_from_doc(doc: &Doc) -> Option<Bookmarks> {
    if !doc.keys(&ROOT).any(|key| key.starts_with(ADD_PREFIX)) {
        return None;
    }
    let mut rows = Vec::new();
    for key in doc.keys(&ROOT) {
        let Some(tag) = key.strip_prefix(ADD_PREFIX) else {
            continue;
        };
        let tag = tag.to_string();
        if doc.get(&ROOT, remove_key(&tag)).ok().flatten().is_some() {
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
        rows.push(Bookmark {
            url,
            title,
            tags: vec![tag],
        });
    }
    rows.sort_by(|a, b| a.url.cmp(&b.url).then_with(|| a.title.cmp(&b.title)));
    let mut merged: Vec<Bookmark> = Vec::new();
    for row in rows {
        if let Some(last) = merged.last_mut()
            && last.url == row.url
        {
            last.tags.extend(row.tags);
            last.tags.sort();
            last.tags.dedup();
        } else {
            merged.push(row);
        }
    }
    rows = merged;
    rows.truncate(MAX_BOOKMARKS);
    Some(Bookmarks::from_entries(rows))
}

pub fn observed_bookmark_tags(doc: &Doc, url: &str) -> Vec<String> {
    let Some(url) = normalized_bookmark_url(url) else {
        return Vec::new();
    };
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
            Self::SetSiteProtection { origin, enabled } => {
                if crate::site_controls::origin(origin).as_deref() != Some(origin.as_str()) {
                    return Err(ActionError::Refused(
                        "a site exception names one exact http(s) origin".into(),
                    ));
                }
                let key = format!("{SITE_EXCEPTION_PREFIX}{origin}");
                if *enabled {
                    if doc.get(&ROOT, &key)?.is_some() {
                        doc.delete(&ROOT, key)?;
                    }
                } else if site_exceptions_from_doc(doc).len() < MAX_SITE_EXCEPTIONS {
                    doc.put(&ROOT, key, true)?;
                } else {
                    return Err(ActionError::Refused(
                        "too many site exceptions; turn protection back on for some".into(),
                    ));
                }
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

    /// Concurrent first bookmark actions from an empty document merge rows and tags.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn concurrent_first_bookmark_actions_merge_rows_and_tags() {
        // Phase `concurrent_first_bookmark_actions_from_empty_doc_merge_distinct_rows`.
        {
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

        // Phase `concurrent_remove_only_tombstones_observed_tags`.
        {
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

        // Phase `concurrent_adds_for_the_same_new_bookmark_preserve_both_tags`.
        {
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

    fn registry() -> ActionRegistry {
        ActionRegistry::with(register)
    }

    fn encode(action: &BrowserAction) -> tilo_actions::EncodedAction {
        tilo_actions::EncodedAction::encode(BROWSER_ACTION_TYPE, action).unwrap()
    }

    const RUST: &str = "https://rust-lang.org/";

    /// A library with two bookmarks, written through the same action the
    /// page's bookmark control dispatches.
    fn seeded_library() -> Doc {
        let mut doc = Doc::new();
        for (url, tag) in [
            (RUST, "bookmark:rust"),
            ("https://docs.rs/", "bookmark:docs"),
        ] {
            let action = add(url, tag);
            tilo_actions::apply_local(&mut doc, &action, encode(&action)).unwrap();
        }
        doc
    }

    /// The tree a headless browser window publishes over `doc`: the
    /// resource's own publication, joined by the window's live rows.
    fn published(doc: &Doc) -> Vec<tilo_actions::Resource> {
        use ui::tilo_resource::Resource as _;
        fn join(into: &mut Vec<tilo_actions::Resource>, rows: Vec<tilo_actions::Resource>) {
            for row in rows {
                match into.iter_mut().find(|child| child.id == row.id) {
                    Some(existing) => {
                        for action in row.actions {
                            if !existing.actions.iter().any(|have| have.id == action.id) {
                                existing.actions.push(action);
                            }
                        }
                        join(&mut existing.children, row.children);
                    }
                    None => into.push(row),
                }
            }
        }
        let state = crate::resource::Browser::read(doc).unwrap();
        let mut root = ui::tilo_resource::publish::<crate::resource::Browser>(
            crate::resource::Browser::KIND,
            "Browser",
            &state,
        );
        let mut widget = crate::browser_widget::BrowserWidget::new_headless();
        widget.present_state(&state);
        join(
            &mut root.children,
            widget
                .live_rows()
                .into_iter()
                .map(ui::framework::LiveRow::into_resource)
                .collect(),
        );
        vec![root]
    }

    fn removal(doc: &Doc, url: &str) -> BrowserAction {
        BrowserAction::RemoveBookmark {
            url: url.into(),
            observed_tags: observed_bookmark_tags(doc, url),
        }
    }

    ui::action_contract_tests! {
        seed: seeded_library(),
        registry: registry(),
        resources: |doc| published(doc),
        palette: ["Remove bookmark", "Set homepage", "Show history", "Open page"],
        replay: [
            remove_bookmark => |doc| encode(&removal(doc, RUST)),
            set_homepage => |_| encode(&BrowserAction::SetHomepage {
                url: "https://home.test/".into(),
            }),
        ],
        undo: [
            add_bookmark => |_| encode(&add("https://crates.io/", "bookmark:crates")),
            remove_bookmark => |doc| encode(&removal(doc, RUST)),
            set_adblock => |_| encode(&BrowserAction::SetAdblock { enabled: false }),
        ],
        encodes: [
            remove_bookmark => ("browser.bookmark", "remove", |doc, res| {
                let url = bookmarks_from_doc(doc)
                    .unwrap()
                    .entries()
                    .iter()
                    .map(|bookmark| bookmark.url.clone())
                    .find(|url| bookmark_resource_id(url) == res.id.as_str())
                    .unwrap();
                encode(&removal(doc, &url))
            }),
            set_homepage => ("browser.settings", "set-homepage", arg: "https://home.test/", |_, _| {
                encode(&BrowserAction::SetHomepage {
                    url: "https://home.test/".into(),
                })
            }),
        ],
    }
}
