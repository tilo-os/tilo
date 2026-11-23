//! A view someone asked to be shown on a workspace's layout, kept in the layout's own document
//! until a device that holds the layout puts it there.
//!
//! Nothing waits for a window: an action does its work, and the view it attaches is asked for
//! here. A device that holds the layout places it at once ([`crate::place`]); one that does not
//! (the device serving the workspace, before the person's page has shown it the layout) writes the
//! ask into the layout's document instead, and the person's page places every view asked of their
//! layout when it holds it: at once, or when it next comes up. Each ask is its own root entry
//! (`asked:<id>`), so asks written by devices that never saw each other's copy of the document
//! merge without one replacing another.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, ScalarValue, Value};

use crate::ownership::OpenedBy;
use crate::place::{NotPlaced, Opener, View, place_view};
use tilo_automerge::ApplyIncoming as _;

/// The prefix of a root key that holds an asked view.
const ASKED: &str = "asked:";
const APP: &str = "app";
const TITLE: &str = "title";
const DOCUMENT: &str = "document";
const PERSON: &str = "person";
const SESSION: &str = "session";
const SESSION_LABEL: &str = "session_label";
const SESSION_TITLE: &str = "session_title";
const SESSION_AGENT: &str = "session_agent";

/// A view asked to be shown on the layout of `person`, the person it is shown to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskedView {
    pub app_id: String,
    pub title: String,
    /// The document the view shows, in the wire spelling pages note their windows' documents in:
    /// empty for the app's primary document.
    pub document: String,
    pub person: String,
    /// The agent session that asked for it, which names the window it opens.
    pub opened_by: Option<OpenedBy>,
}

/// Ask for `view` to be shown, under `id` (one ask per id: asking again replaces it).
///
/// # Errors
///
/// The document refused the write.
pub fn ask(
    doc: &mut AutoCommit,
    id: &str,
    view: &AskedView,
) -> Result<(), automerge::AutomergeError> {
    let entry = doc.put_object(ROOT, format!("{ASKED}{id}"), ObjType::Map)?;
    doc.put(&entry, APP, view.app_id.as_str())?;
    doc.put(&entry, TITLE, view.title.as_str())?;
    doc.put(&entry, DOCUMENT, view.document.as_str())?;
    doc.put(&entry, PERSON, view.person.as_str())?;
    if let Some(opened_by) = &view.opened_by {
        let OpenedBy {
            run: session,
            agent,
            label,
            title,
        } = opened_by;
        doc.put(&entry, SESSION_AGENT, agent.as_str())?;
        doc.put(&entry, SESSION, session.as_str())?;
        doc.put(&entry, SESSION_LABEL, label.as_str())?;
        doc.put(&entry, SESSION_TITLE, title.as_str())?;
    }
    Ok(())
}

/// The views asked for in `doc`, each with the id it was asked under.
#[must_use]
pub fn asked(doc: &AutoCommit) -> Vec<(String, AskedView)> {
    doc.keys(ROOT)
        .filter_map(|key| {
            let id = key.strip_prefix(ASKED)?.to_owned();
            let Ok(Some((Value::Object(ObjType::Map), entry))) = doc.get(ROOT, key.as_str()) else {
                return None;
            };
            let text = |field: &str| match doc.get(&entry, field) {
                Ok(Some((Value::Scalar(scalar), _))) => match scalar.as_ref() {
                    ScalarValue::Str(text) => Some(text.to_string()),
                    _ => None,
                },
                _ => None,
            };
            let opened_by = text(SESSION).map(|session| OpenedBy {
                run: session,
                agent: text(SESSION_AGENT).unwrap_or_default(),
                label: text(SESSION_LABEL).unwrap_or_default(),
                title: text(SESSION_TITLE).unwrap_or_default(),
            });
            Some((
                id,
                AskedView {
                    app_id: text(APP)?,
                    title: text(TITLE).unwrap_or_default(),
                    document: text(DOCUMENT).unwrap_or_default(),
                    person: text(PERSON)?,
                    opened_by,
                },
            ))
        })
        .collect()
}

/// Put every view asked for `person` on the layout `doc` holds, and take each ask out once its
/// window is there (or a window shows its document already). An ask is left for later while `doc`
/// holds no layout to place it on. Says whether `doc` changed.
///
/// # Errors
///
/// Why a view could not be placed on the layout `doc` holds, other than its holding none yet.
pub fn place_asked(doc: &mut AutoCommit, person: &str) -> Result<bool, NotPlaced> {
    let mut changed = false;
    for (id, view) in asked(doc) {
        if view.person != person {
            continue;
        }
        let placed = place_view(
            doc,
            &Opener {
                person: &view.person,
                opened_by: view.opened_by.clone(),
            },
            &View {
                app_id: &view.app_id,
                title: &view.title,
                document: &view.document,
                beside: None,
            },
        );
        match placed {
            Ok(Some(change)) => {
                doc.load_incoming(&change)
                    .map_err(|error| NotPlaced::Unwritten(error.to_string()))?;
            }
            Ok(None) => {}
            Err(NotPlaced::NoLayout) => continue,
            Err(other) => return Err(other),
        }
        doc.delete(ROOT, format!("{ASKED}{id}").as_str())
            .map_err(|error| NotPlaced::Unwritten(error.to_string()))?;
        changed = true;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ownership::SharedDesktop;
    use crate::{Layout, Strip, StripId, Workspace, WorkspaceId, shared_resource};

    fn layout() -> AutoCommit {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Workspace");
        workspace.strips = vec![Strip::new(StripId(10), "Main")];
        let desktop = SharedDesktop {
            tree: Layout {
                workspaces: vec![workspace],
                ..Layout::default()
            },
            ..SharedDesktop::default()
        };
        let mut doc = AutoCommit::new();
        shared_resource::write(&mut doc, &desktop).unwrap();
        doc.commit();
        doc
    }

    fn notes(person: &str) -> AskedView {
        AskedView {
            app_id: "so.tilo.notes".into(),
            title: "Notes".into(),
            document: String::new(),
            person: person.into(),
            opened_by: Some(OpenedBy {
                run: "agent-session:1".into(),
                agent: "claude-code-0a1b2c3d4e".into(),
                label: "Claude Code".into(),
                title: String::new(),
            }),
        }
    }

    /// A device without the layout asks in a document of its own; the person's page, holding the
    /// layout, takes the ask in and puts the window there, named as the agent session's, and the
    /// ask is gone.
    #[test]
    fn an_ask_from_a_device_without_the_layout_is_placed_by_the_page_that_holds_it() {
        let mut asking = AutoCommit::new();
        ask(&mut asking, "notes", &notes("ada")).unwrap();
        asking.commit();
        let mut page = layout();
        page.merge_incoming(&mut asking).unwrap();
        assert_eq!(asked(&page).len(), 1);
        assert!(place_asked(&mut page, "ada").unwrap());
        page.commit();
        let desktop = shared_resource::read(&page, None).unwrap();
        let [(tab, shown)] = desktop.records.as_slice() else {
            panic!("one window: {desktop:?}");
        };
        assert_eq!(shown, "");
        assert_eq!(
            desktop
                .opened_by
                .get(tab)
                .map(|session| session.label.as_str()),
            Some("Claude Code")
        );
        assert!(asked(&page).is_empty(), "the ask is taken out once placed");
        assert!(
            !place_asked(&mut page, "ada").unwrap(),
            "nothing left to place"
        );
    }

    /// Someone else's page leaves the ask for the person's own; a device with no layout yet keeps
    /// it for when it has one.
    #[test]
    fn an_ask_waits_for_its_persons_layout() {
        let mut page = layout();
        ask(&mut page, "notes", &notes("ada")).unwrap();
        assert!(!place_asked(&mut page, "bo").unwrap());
        assert_eq!(asked(&page).len(), 1, "Bo's page leaves Ada's ask");
        let mut bare = AutoCommit::new();
        ask(&mut bare, "notes", &notes("ada")).unwrap();
        assert!(!place_asked(&mut bare, "ada").unwrap());
        assert_eq!(asked(&bare).len(), 1, "kept until a layout is there");
    }
}
