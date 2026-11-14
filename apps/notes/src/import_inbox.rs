//! Add the pages a migration left for this app: Outline's collections and documents.
//!
//! The migration writes each page it brings, with its pictures, into this app's own data area
//! (`personal-data-incoming/pages`). Opening the notebook folds them in through its own actions:
//! `import-markdown` keeps a page's words and pictures under an id its source decides, and
//! `move-note` puts it under its parent. A page the notebook already holds is left as the person
//! has it, and an entry is removed once its page is in, so a deleted page is never revived.

use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use serde::Deserialize;
use ui::application::HostContext;
use ui::tilo_resource::{Bytes, Id, Invoke, Replica};

use crate::notebook::{Notebook, notebook, page_id};

const INBOX: &str = "personal-data-incoming/pages";
const VERSION: u32 = 1;
const MAX_ENTRY_BYTES: usize = 64 * 1024 * 1024;

/// One page as the migration wrote it.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Entry {
    version: u32,
    source: String,
    key: String,
    title: String,
    markdown: String,
    parent: Option<String>,
    /// Base64 of each picture the Markdown shows as `asset:<digest>`.
    #[serde(default)]
    pictures: Vec<String>,
}

impl Entry {
    fn note(&self) -> Id {
        note_for(&self.source, &self.key)
    }
}

/// The note a source's page is kept under: the same on every device and every run.
fn note_for(source: &str, key: &str) -> Id {
    let digest = blake3::hash(format!("{source}\u{0}{key}").as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(digest.as_bytes().get(..16).unwrap_or(&[0; 16]));
    page_id(uuid::Builder::from_random_bytes(bytes).into_uuid())
}

pub(crate) fn import(context: &HostContext) -> Result<(), String> {
    let files = context.files();
    let data = files.data().map_err(|error| error.to_string())?;
    let Ok(inbox) = tilo_files::VPath::new(INBOX) else {
        return Ok(());
    };
    let Ok(listed) = tilo_files::blocking::list(&data, &inbox) else {
        return Ok(());
    };
    let mut entries = Vec::new();
    let mut names = Vec::new();
    for entry in listed {
        if entry.kind != tilo_files::Kind::File
            || !std::path::Path::new(&entry.name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
        {
            continue;
        }
        let Ok(name) = tilo_files::VPath::new(&entry.name) else {
            continue;
        };
        let path = inbox.join(&name);
        let Ok(Some(bytes)) = tilo_files::blocking::read_bounded(&data, &path, MAX_ENTRY_BYTES)
        else {
            continue;
        };
        let Ok(parsed) = serde_json::from_slice::<Entry>(&bytes) else {
            continue;
        };
        if parsed.version != VERSION {
            continue;
        }
        entries.push(parsed);
        names.push(path);
    }
    if entries.is_empty() {
        return Ok(());
    }
    let store = context
        .content()
        .cloned()
        .unwrap_or_else(tilo_content::ContentStore::memory);
    let author = context.author().0.clone();
    context.migrate_record(move |doc| apply(doc, entries, &store, &author))?;
    for name in names {
        let _ = tilo_files::blocking::remove(&data, &name);
    }
    Ok(())
}

/// Add `entries` the notebook does not hold yet, parents and link targets first.
pub(crate) fn apply(
    doc: &mut automerge::AutoCommit,
    entries: Vec<Entry>,
    content: &tilo_content::ContentStore,
    author: &str,
) -> Result<bool, String> {
    let mut replica = Replica::<Notebook>::from_doc(doc.clone()).as_author(author);
    let held: BTreeSet<Id> = replica
        .state()
        .map_err(|error| error.to_string())?
        .pages
        .ids()
        .cloned()
        .collect();
    let steps = invokes(entries, &held, &mut |bytes| {
        Bytes::keep(content, &bytes).map_err(|error| error.to_string())
    })?;
    if steps.is_empty() {
        return Ok(false);
    }
    for step in steps {
        replica
            .apply_confirmed(step)
            .map_err(|error| error.to_string())?;
    }
    *doc = replica.doc_mut().clone();
    Ok(true)
}

/// The notebook's own actions that add `entries` it does not hold (`held`): `import-markdown`
/// for each page with its pictures, kept by `keep`, then `move-note` under its parent.
pub(crate) fn invokes(
    entries: Vec<Entry>,
    held: &BTreeSet<Id>,
    keep: &mut dyn FnMut(Vec<u8>) -> Result<Bytes, String>,
) -> Result<Vec<Invoke<Notebook>>, String> {
    let mut present = held.clone();
    let ordered = ordered(
        entries
            .into_iter()
            .filter(|entry| !held.contains(&entry.note()))
            .collect(),
    );
    let engine = base64::engine::general_purpose::STANDARD;
    let mut steps = Vec::new();
    for entry in ordered {
        let note = entry.note();
        let mut images = Vec::new();
        for picture in &entry.pictures {
            let Ok(bytes) = engine.decode(picture) else {
                continue;
            };
            if crate::notebook::check_picture(&bytes).is_err() {
                continue;
            }
            images.push(keep(bytes)?);
        }
        steps.push(Notebook::IMPORT_MARKDOWN.with(notebook::ImportMarkdown {
            title: entry.title.clone(),
            markdown: entry.markdown.clone(),
            images,
            note: Some(note.clone()),
            path: None,
            origin: None,
        }));
        present.insert(note.clone());
        let parent = entry
            .parent
            .as_deref()
            .map(|parent| note_for(&entry.source, parent))
            .filter(|parent| present.contains(parent));
        if let Some(parent) = parent {
            steps.push(Notebook::MOVE_NOTE.with(notebook::MoveNote {
                page: note,
                parent: Some(parent),
                before: None,
            }));
        }
    }
    Ok(steps)
}

/// What the `migrated` screenshot shows: an Outline team's handbook as a migration lands it.
#[cfg(feature = "samples")]
pub(crate) fn migrated_scenario() -> Vec<Invoke<Notebook>> {
    let picture = base64::engine::general_purpose::STANDARD.encode(crate::scenario::IMAGE);
    let digest = blake3::hash(crate::scenario::IMAGE).to_hex();
    let page = |key: &str, title: &str, markdown: String, parent: Option<&str>| Entry {
        version: VERSION,
        source: "outline".into(),
        key: key.into(),
        title: title.into(),
        markdown,
        parent: parent.map(str::to_owned),
        pictures: Vec::new(),
    };
    let mut onboarding = page(
        "doc-onboarding",
        "Onboarding checklist",
        format!(
            "Read [[Welcome]] first, then work through the list.\n\n- [ ] Get your laptop\n- [ ] Set up your Tilo identity\n- [ ] Join the team workspace\n\n![Settings, the way your desk starts](asset:{digest})\n\nQuestions go to [[Security basics]] or the team chat."
        ),
        Some("doc-welcome"),
    );
    onboarding.pictures = vec![picture];
    let entries = vec![
        page(
            "collection:handbook",
            "Handbook",
            "How we work.".into(),
            None,
        ),
        page(
            "doc-welcome",
            "Welcome",
            "Glad you are here. Start with the [[Onboarding checklist]].".into(),
            Some("collection:handbook"),
        ),
        onboarding,
        page(
            "doc-holidays",
            "Holidays",
            "Ask a week ahead.".into(),
            Some("collection:handbook"),
        ),
        page(
            "doc-security",
            "Security basics",
            "Lock your screen.".into(),
            Some("collection:handbook"),
        ),
        page(
            "collection:product",
            "Product",
            "What we build.".into(),
            None,
        ),
        page(
            "doc-roadmap",
            "Roadmap 2027",
            "## Next\n\n- Sync everywhere".into(),
            Some("collection:product"),
        ),
    ];
    invokes(entries, &BTreeSet::new(), &mut |bytes| {
        Ok(Bytes::to_keep(bytes))
    })
    .unwrap_or_default()
}

/// `entries` with every page after its parent, and, where the links allow, after the pages its
/// `[[title]]` links name: a link to a page that exists when its page is made leads there.
fn ordered(entries: Vec<Entry>) -> Vec<Entry> {
    let keys: BTreeSet<String> = entries.iter().map(|entry| entry.key.clone()).collect();
    let by_title: BTreeMap<String, String> = entries
        .iter()
        .map(|entry| (entry.title.clone(), entry.key.clone()))
        .collect();
    let mut placed = BTreeSet::new();
    let mut ordered = Vec::with_capacity(entries.len());
    let mut rest = entries;
    let waits_for = |entry: &Entry, placed: &BTreeSet<String>, links: bool| {
        let parent = entry
            .parent
            .as_ref()
            .is_some_and(|parent| keys.contains(parent) && !placed.contains(parent));
        parent
            || (links
                && linked_titles(&entry.markdown).iter().any(|title| {
                    by_title
                        .get(title)
                        .is_some_and(|key| key != &entry.key && !placed.contains(key))
                }))
    };
    while !rest.is_empty() {
        let mut ready = None;
        for links in [true, false] {
            if let Some(index) = rest
                .iter()
                .position(|entry| !waits_for(entry, &placed, links))
            {
                ready = Some(index);
                break;
            }
        }
        let index = ready.unwrap_or(0);
        let entry = rest.remove(index);
        placed.insert(entry.key.clone());
        ordered.push(entry);
    }
    ordered
}

/// The titles `markdown` links to as `[[title]]`.
fn linked_titles(markdown: &str) -> Vec<String> {
    let mut titles = Vec::new();
    let mut rest = markdown;
    while let Some(start) = rest.find("[[") {
        let after = rest.get(start + 2..).unwrap_or_default();
        let Some(end) = after.find("]]") else {
            break;
        };
        titles.push(after.get(..end).unwrap_or_default().trim().to_owned());
        rest = after.get(end + 2..).unwrap_or_default();
    }
    titles
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, title: &str, markdown: &str, parent: Option<&str>) -> Entry {
        Entry {
            version: VERSION,
            source: "outline".into(),
            key: key.into(),
            title: title.into(),
            markdown: markdown.into(),
            parent: parent.map(str::to_owned),
            pictures: Vec::new(),
        }
    }

    #[test]
    fn migrated_pages_become_notes_under_their_parents_once() {
        // A one-pixel PNG, as the migration keeps an Outline picture.
        let png: &[u8] = &[
            0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0, 0, 13, b'I', b'H', b'D', b'R',
            0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89, 0, 0, 0, 13, b'I', b'D',
            b'A', b'T', 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0, 0x05, 0, 0x01, 0xff,
            0x89, 0x99, 0x3d, 0x1d, 0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82,
        ];
        let digest = blake3::hash(png).to_hex().to_string();
        let mut child = entry(
            "doc-child",
            "Onboarding checklist",
            &format!("Read [[Welcome]] first.\n\n![Team](asset:{digest})\n"),
            Some("doc-welcome"),
        );
        child.pictures = vec![base64::engine::general_purpose::STANDARD.encode(png)];
        // Listed child first, as a directory listing may: the parent still lands first.
        let entries = vec![
            child,
            entry("doc-welcome", "Welcome", "Hello.", Some("collection:c1")),
            entry("collection:c1", "Handbook", "How we work", None),
        ];
        let mut replica = Replica::<Notebook>::new();
        let mut doc = replica.doc_mut().clone();
        let content = tilo_content::ContentStore::memory();
        assert!(apply(&mut doc, entries.clone(), &content, "ada").unwrap());
        let notebook = <Notebook as ui::tilo_resource::Resource>::read(&doc).unwrap();
        let note = |key: &str| {
            notebook
                .pages
                .get(&note_for("outline", key))
                .unwrap()
                .clone()
        };
        let handbook = note("collection:c1");
        assert_eq!(handbook.title, "Handbook");
        assert_eq!(handbook.parent, None);
        assert_eq!(
            note("doc-welcome").parent,
            Some(note_for("outline", "collection:c1"))
        );
        let checklist = note("doc-child");
        assert_eq!(checklist.parent, Some(note_for("outline", "doc-welcome")));
        assert_eq!(
            notebook.images.len(),
            1,
            "the picture is kept with the notebook"
        );
        assert!(notebook.images.contains_key(&digest));
        let blocks = format!("{:?}", checklist.blocks());
        assert!(blocks.contains(&format!("asset:{digest}")), "{blocks}");

        // Opening again with the same entries adds nothing.
        assert!(!apply(&mut doc, entries, &content, "ada").unwrap());
        let again = <Notebook as ui::tilo_resource::Resource>::read(&doc).unwrap();
        assert_eq!(again.pages.len(), 3);
    }

    #[test]
    fn link_targets_are_made_before_the_pages_linking_to_them() {
        let entries = ordered(vec![
            entry("a", "Alpha", "See [[Beta]].", None),
            entry("b", "Beta", "Plain.", None),
        ]);
        let keys: Vec<&str> = entries.iter().map(|entry| entry.key.as_str()).collect();
        assert_eq!(keys, ["b", "a"]);
        let cycle = ordered(vec![
            entry("a", "Alpha", "See [[Beta]].", None),
            entry("b", "Beta", "See [[Alpha]].", None),
        ]);
        assert_eq!(cycle.len(), 2);
    }
}
