//! Changes other people made to a notebook, as their replicas send them:
//! what a host merges to show a History with other authors in it.

/// A change someone else made to a notebook, for screenshots of its History:
/// who made it (`actor`, the document actor their replica writes as), what
/// they said they did (`purpose`), the paragraph it added to the notebook's
/// first note, and when (seconds since the Unix epoch).
pub struct PeerEdit<'a> {
    pub actor: &'a [u8],
    pub purpose: &'a str,
    pub paragraph: &'a str,
    pub at: i64,
}

/// The changes `edits` make to `notebook`, in order, each as the incremental
/// bytes the author's replica sends: what a record merges when they arrive.
/// Stops at the first edit that cannot be made (a notebook with no note).
#[must_use]
pub fn peer_edit_changes(notebook: &automerge::AutoCommit, edits: &[PeerEdit<'_>]) -> Vec<Vec<u8>> {
    let mut doc = notebook.clone();
    let mut changes = Vec::with_capacity(edits.len());
    for edit in edits {
        let before = doc.get_heads();
        let mut fork = doc.fork().with_actor(automerge::ActorId::from(edit.actor));
        let Some(body) = first_note_body(&fork) else {
            break;
        };
        let appended = tilo_document::RichTextAction::AppendParagraph {
            text: edit.paragraph.to_owned(),
        }
        .apply_under(&mut fork, &body);
        if appended.is_err() {
            break;
        }
        let context = tilo_actions::ActionContext {
            purpose: Some(edit.purpose.to_owned()),
            ..tilo_actions::ActionContext::default()
        };
        if ui::tilo_resource::history::commit_stamped_at_with_message(
            &mut fork,
            edit.at,
            context.change_message(),
        )
        .is_none()
        {
            break;
        }
        let bytes = fork.save_after(&before);
        if tilo_automerge::ApplyIncoming::load_incoming(&mut doc, &bytes).is_err() {
            break;
        }
        changes.push(bytes);
    }
    changes
}

/// The body of the first note the notebook stores.
fn first_note_body(doc: &automerge::AutoCommit) -> Option<automerge::ObjId> {
    use automerge::{ObjType, ReadDoc, Value};
    fn find(
        doc: &automerge::AutoCommit,
        obj: &automerge::ObjId,
        depth: usize,
    ) -> Option<automerge::ObjId> {
        if depth > 6 {
            return None;
        }
        let children: Vec<(String, ObjType, automerge::ObjId)> = match doc.object_type(obj).ok()? {
            ObjType::Map | ObjType::Table => doc
                .keys(obj)
                .filter_map(|key| match doc.get(obj, key.as_str()).ok()? {
                    Some((Value::Object(kind), id)) => Some((key, kind, id)),
                    _ => None,
                })
                .collect(),
            ObjType::List => (0..doc.length(obj))
                .filter_map(|index| match doc.get(obj, index).ok()? {
                    Some((Value::Object(kind), id)) => Some((String::new(), kind, id)),
                    _ => None,
                })
                .collect(),
            ObjType::Text => Vec::new(),
        };
        for (key, kind, id) in &children {
            if key == "body" && *kind == ObjType::Map {
                return Some(id.clone());
            }
        }
        children
            .iter()
            .filter(|(_, kind, _)| *kind != ObjType::Text)
            .find_map(|(_, _, id)| find(doc, id, depth + 1))
    }
    let (_, pages) = doc.get(automerge::ROOT, "pages").ok()??;
    find(doc, &pages, 0)
}
