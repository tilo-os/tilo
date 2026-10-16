//! Bringing the notebooks of earlier Notes windows into the room's notebook.
//!
//! Notes used to keep a notebook per window: each window the launcher opened
//! got a record of its own (`derived:default@tab:<window>`), so a new window
//! showed an empty notebook and the pages written in another were reachable
//! only by restoring that exact window. Notes now keeps one notebook per
//! room ([`ResourceApp::ROOM_RECORD`](ui::framework::ResourceApp::ROOM_RECORD)).
//! When a room's notebook is first created, its seed copies the notes, tags
//! and pictures of every per-window notebook this device holds in the room
//! into it ([`adoption`]).
//!
//! The copy is deterministic: notebooks are taken in record id order, notes
//! keep their ids (so links between them still resolve) and their order, and
//! a note id met again in a later notebook is kept as the first had it. The
//! per-window records are only read; they stay on disk as they were, so the
//! migration can be undone by hand.
//!
//! Why a copy and not [`RecordCheckout::formerly`](ui::record::RecordCheckout::formerly):
//! a former identity hands the new record *one* earlier document, history
//! and all, and claims it so no other successor gets it. A room usually has
//! several window notebooks with notes (each window someone wrote in), and
//! independent Automerge documents cannot be merged into one: each created
//! its own `pages` collection, so merging would keep one window's notes and
//! hide the others' as conflicts. Copying the notes, by id, keeps all of
//! them; what is lost is their per-keystroke history, which stays readable
//! in the untouched old records.

use std::collections::HashSet;

use ui::tilo_resource::{Id, Invoke, Resource};

use crate::actions;
use crate::model::{PageId, TagId};
use crate::notebook::{self, Notebook};

/// The seed that copies `records` (a room's window notebooks, as
/// [`SeedCx::window_records`](ui::framework::SeedCx::window_records) reads
/// them) into a new notebook: its notes with their trees, bodies and tags,
/// then the pictures they show, which `content` (this device's content
/// store) serves when a notebook holds only a reference to one. Empty when
/// no record holds a note or a tag. A record that does not read as a Notes
/// notebook is left out and logged; so is a picture whose bytes are not
/// here, which is then missing from its notes as it was in the old window.
pub fn adoption(
    records: &[(ui::record::RecordId, ui::tilo_resource::Doc)],
    content: Option<&tilo_content::ContentStore>,
) -> Vec<Invoke<Notebook>> {
    let mut seed = Vec::new();
    let mut pages = HashSet::<PageId>::new();
    let mut tags = HashSet::<TagId>::new();
    let mut images = HashSet::<String>::new();
    for (record, doc) in records {
        // A window notebook written before Notes kept a notebook record is
        // read as the record it migrates to; the window's own record stays
        // as it was.
        let mut doc = doc.clone();
        if let Err(error) = ui::tilo_resource::evolution::migrate::<Notebook>(&mut doc) {
            tracing::warn!(%record, %error, "notes: not adopting an unreadable window notebook");
            continue;
        }
        let notebook = match Notebook::read(&doc) {
            Ok(notebook) => notebook,
            Err(error) => {
                tracing::warn!(%record, %error, "notes: not adopting an unreadable window notebook");
                continue;
            }
        };
        adopt_tags(&notebook, &mut tags, &mut seed);
        adopt_pages(&notebook, &tags, &mut pages, &mut seed);
        adopt_images(&notebook, content, &mut images, &mut seed);
    }
    seed
}

fn adopt_tags(notebook: &Notebook, tags: &mut HashSet<TagId>, seed: &mut Vec<Invoke<Notebook>>) {
    for (id, tag) in notebook.tags.iter() {
        let Some(tag_id) = notebook::page_of(id).filter(|tag| !tag.is_nil()) else {
            continue;
        };
        let name = tag.name.0.trim();
        if name.is_empty() || !tags.insert(tag_id) {
            continue;
        }
        push(seed, &actions::new_tag_action(tag_id, name));
    }
}

fn adopt_pages(
    notebook: &Notebook,
    tags: &HashSet<TagId>,
    pages: &mut HashSet<PageId>,
    seed: &mut Vec<Invoke<Notebook>>,
) {
    let named = |id: &Id| notebook::page_of(id).filter(|page| !page.is_nil());
    for (id, page) in notebook.pages.iter() {
        let Some(page_id) = named(id) else {
            continue;
        };
        if !pages.insert(page_id) {
            continue;
        }
        // A note whose parent is not in the notebook (it was left out)
        // becomes a top-level note rather than dangling.
        let parent = page
            .parent
            .as_ref()
            .and_then(named)
            .filter(|parent| notebook.pages.contains(&notebook::page_id(*parent)));
        push(
            seed,
            &actions::create_page_action(page_id, parent, &page.title),
        );
        let blocks = page.blocks();
        if !blocks.is_empty() {
            push(seed, &actions::write_body_action(page_id, &blocks));
        }
        for tag in page.tags.iter().filter_map(named) {
            if tags.contains(&tag) {
                push(
                    seed,
                    &actions::set_tag_assignment_action(page_id, tag, true),
                );
            }
        }
    }
}

fn adopt_images(
    notebook: &Notebook,
    content: Option<&tilo_content::ContentStore>,
    images: &mut HashSet<String>,
    seed: &mut Vec<Invoke<Notebook>>,
) {
    for (digest, stored) in notebook.images.iter() {
        if images.contains(digest) {
            continue;
        }
        let bytes = match stored.inline() {
            Some(bytes) => Some(bytes.to_vec()),
            None => content
                .and_then(|content| content.read(digest))
                .filter(|bytes| tilo_content::verify(digest, bytes)),
        };
        let Some(bytes) = bytes else {
            tracing::warn!(%digest, "notes: an adopted picture is not on this device");
            continue;
        };
        images.insert(digest.clone());
        push(seed, &actions::keep_image_action(bytes));
    }
}

/// Add `action` to `seed`; an action that does not encode is left out, which
/// never happens for the plain data the builders take.
fn push(seed: &mut Vec<Invoke<Notebook>>, action: &<Notebook as Resource>::Action) {
    match Invoke::apply(action) {
        Ok(invoke) => seed.push(invoke),
        Err(error) => tracing::warn!(%error, "notes: an adopted change did not encode"),
    }
}
