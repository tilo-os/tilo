//! What one person sees of a workspace's layout: the strip and the group in front, the tab in
//! front of each group, how wide each window is and which ones are as large as their display.
//!
//! It is the person's, not the workspace's and not a device's: it is kept in the workspace, so
//! every device of theirs shows the layout the way they left it, and the people they work with
//! see where they are looking ([`PersonView::front_strip`]). What a display shows — its windows
//! and whether one of them covers it — is the device's, and stays on it.

use std::collections::{BTreeMap, BTreeSet};

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ObjId, ROOT, ReadDoc as _, Value};
use serde::{Deserialize, Serialize};

use crate::ids::{GroupId, StripId, TabId, WorkspaceId};
use crate::serde_ids::deserialize_id_btree_map;
use crate::tree::Layout;
use crate::width::WindowWidth;

/// The record of [`crate::ownership::DESKTOP_APP`] holding `person`'s view of a workspace's
/// layout.
#[must_use]
pub fn record_of(person: &str) -> String {
    format!("view-of-{person}")
}

/// The person whose view of the layout record `record` holds, on [`crate::ownership::DESKTOP_APP`].
#[must_use]
pub fn person_of(record: &str) -> Option<&str> {
    record.strip_prefix("view-of-")
}

/// The keys a view is kept under.
const FRONT_STRIP: &str = "front_strip";
const FRONT_GROUP: &str = "front_group";
const FRONT_TAB: &str = "front_tab";
const WIDTHS: &str = "widths";
const MAXIMIZED: &str = "maximized";

/// What one person sees of one workspace's layout.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonView {
    /// The strip in front.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub front_strip: Option<StripId>,
    /// The group in front of each strip.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "deserialize_id_btree_map"
    )]
    pub front_group: BTreeMap<StripId, GroupId>,
    /// The tab in front of each group.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "deserialize_id_btree_map"
    )]
    pub front_tab: BTreeMap<GroupId, TabId>,
    /// How wide each group is.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "deserialize_id_btree_map"
    )]
    pub widths: BTreeMap<GroupId, WindowWidth>,
    /// The groups made as large as their display.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub maximized: BTreeSet<GroupId>,
}

impl PersonView {
    /// What the workspace `id` of `tree` shows the person looking at it.
    #[must_use]
    pub fn of(tree: &Layout, id: WorkspaceId) -> Self {
        let mut view = Self {
            front_strip: tree.active_strip.get(&id).copied(),
            ..Self::default()
        };
        let Some(workspace) = tree.workspaces.iter().find(|held| held.id() == id) else {
            return view;
        };
        for strip in &workspace.strips {
            if let Some(group) = tree.active_group.get(&strip.id) {
                view.front_group.insert(strip.id, *group);
            }
            for group in &strip.window_groups {
                view.front_tab.insert(group.id, group.active_tab);
                view.widths.insert(group.id, group.width);
                if group.maximized {
                    view.maximized.insert(group.id);
                }
            }
        }
        view
    }

    /// Show the workspace `id` of `tree` as this view says. A strip, a group or a tab the view names and the
    /// layout no longer holds is left out: what it named is gone. Whether anything changed.
    pub fn applied(&self, tree: &mut Layout, id: WorkspaceId) -> bool {
        let mut changed = false;
        let Some(workspace) = tree.workspaces.iter_mut().find(|held| held.id() == id) else {
            return false;
        };
        if let Some(strip) = self.front_strip
            && workspace.strips.iter().any(|held| held.id == strip)
            && tree.active_strip.get(&id) != Some(&strip)
        {
            tree.active_strip.insert(id, strip);
            changed = true;
        }
        for strip in &mut workspace.strips {
            if let Some(group) = self.front_group.get(&strip.id)
                && strip.window_groups.iter().any(|held| held.id == *group)
                && tree.active_group.get(&strip.id) != Some(group)
            {
                tree.active_group.insert(strip.id, *group);
                changed = true;
            }
            for group in &mut strip.window_groups {
                if let Some(tab) = self.front_tab.get(&group.id)
                    && group.active_tab != *tab
                    && group.tabs.iter().any(|held| held.id == *tab)
                {
                    group.active_tab = *tab;
                    changed = true;
                }
                if let Some(width) = self.widths.get(&group.id)
                    && group.width != *width
                {
                    group.width = *width;
                    changed = true;
                }
                let maximized = self.maximized.contains(&group.id);
                if group.maximized != maximized {
                    group.maximized = maximized;
                    changed = true;
                }
            }
        }
        changed
    }

    /// This view, read from the record, laid over `here`, the device's own view, which showed
    /// `written` before the person changed it here: what they changed, and what the record says
    /// nothing of, stays; everything else takes the record's.
    #[must_use]
    pub fn over(&self, here: &Self, written: &Self) -> Self {
        fn each<K: Ord + Copy, V: Clone + PartialEq>(
            record: &BTreeMap<K, V>,
            here: &BTreeMap<K, V>,
            written: &BTreeMap<K, V>,
        ) -> BTreeMap<K, V> {
            let mut kept = record.clone();
            for (key, value) in here {
                if written.get(key) != Some(value) || !record.contains_key(key) {
                    kept.insert(*key, value.clone());
                }
            }
            kept
        }
        let mut maximized = self.maximized.clone();
        maximized.extend(here.maximized.difference(&written.maximized));
        maximized
            .retain(|group| here.maximized.contains(group) || !written.maximized.contains(group));
        Self {
            front_strip: if here.front_strip == written.front_strip {
                self.front_strip
            } else {
                here.front_strip
            },
            front_group: each(&self.front_group, &here.front_group, &written.front_group),
            front_tab: each(&self.front_tab, &here.front_tab, &written.front_tab),
            widths: each(&self.widths, &here.widths, &written.widths),
            maximized,
        }
    }

    /// The view `doc` holds.
    #[must_use]
    pub fn read(doc: &AutoCommit) -> Self {
        Self {
            front_strip: field(doc, FRONT_STRIP),
            front_group: field(doc, FRONT_GROUP).unwrap_or_default(),
            front_tab: field(doc, FRONT_TAB).unwrap_or_default(),
            widths: field(doc, WIDTHS).unwrap_or_default(),
            maximized: field(doc, MAXIMIZED).unwrap_or_default(),
        }
    }

    /// Keep this view in `doc`, writing only what differs. Whether anything was written.
    ///
    /// # Errors
    /// The document refused a write.
    pub fn write(&self, doc: &mut AutoCommit) -> Result<bool, automerge::AutomergeError> {
        let ops = doc.pending_ops();
        for (key, value) in [
            (FRONT_STRIP, serde_json::to_value(self.front_strip)),
            (FRONT_GROUP, serde_json::to_value(&self.front_group)),
            (FRONT_TAB, serde_json::to_value(&self.front_tab)),
            (WIDTHS, serde_json::to_value(&self.widths)),
            (MAXIMIZED, serde_json::to_value(&self.maximized)),
        ] {
            let value = value.unwrap_or(serde_json::Value::Null);
            tilo_automerge::json::write(doc, &ObjId::Root, key, &value).map_err(
                |error| match error {
                    tilo_automerge::json::JsonError::Automerge(error) => error,
                    shape @ tilo_automerge::json::JsonError::Shape { .. } => {
                        automerge::AutomergeError::InvalidValueType {
                            expected: "a JSON value".to_owned(),
                            unexpected: shape.to_string(),
                        }
                    }
                },
            )?;
        }
        Ok(doc.pending_ops() > ops)
    }
}

/// The value `doc` keeps under `key`.
fn field<T: serde::de::DeserializeOwned>(doc: &AutoCommit, key: &str) -> Option<T> {
    let (value, id) = doc.get(ROOT, key).ok().flatten()?;
    let json = match &value {
        Value::Object(automerge::ObjType::Map | automerge::ObjType::List) => {
            tilo_automerge::json::read_value(doc, &value, &id).ok()?
        }
        Value::Scalar(scalar) => tilo_automerge::json::scalar_json(scalar.as_ref()).ok()?,
        Value::Object(_) => return None,
    };
    serde_json::from_value(json).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Strip, Tab, TileContent, WindowGroup};
    use crate::ownership::SharedDesktop;
    use crate::shared_record;
    use crate::{Workspace, WorkspaceId};

    fn tab(id: u64, label: &str) -> Tab {
        Tab {
            id,
            title: label.into(),
            tile: TileContent::LocalApp {
                app_id: format!("org.example.{label}"),
                label: label.into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        }
    }

    fn group(id: u64, tabs: Vec<Tab>) -> WindowGroup {
        let active_tab = tabs[0].id;
        WindowGroup {
            id,
            tabs,
            active_tab,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }
    }

    /// A workspace of two strips, the first holding two windows.
    fn workspace() -> Workspace {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Garden");
        let mut first = Strip::new(StripId(11), "A");
        first.window_groups = vec![
            group(21, vec![tab(31, "Notes"), tab(32, "Files")]),
            group(22, vec![tab(33, "Chat")]),
        ];
        let mut second = Strip::new(StripId(12), "B");
        second.window_groups = vec![group(23, vec![tab(34, "Music")])];
        workspace.strips = vec![first, second];
        workspace
    }

    /// The layout the workspace shares, as its record gives it back.
    fn layout() -> Layout {
        let mut doc = AutoCommit::new();
        let shared = SharedDesktop {
            tree: Layout {
                workspaces: vec![workspace()],
                ..Layout::default()
            },
            ..SharedDesktop::default()
        };
        assert!(
            shared_record::write(&mut doc, &shared).expect("a document that takes the layout"),
            "the record holds the layout"
        );
        shared_record::read(&doc, None).expect("the layout").tree
    }

    tilo_test_env::scenario! {
        fn a_persons_view_of_the_shared_layout;

        phase one_layout_and_each_person_their_own_strip_in_front;
        // Two people in one workspace see one layout, and each their own strip in front.
        {
            let shared = layout();
            let mut hers = shared.clone();
            let mut his = shared;

            let her_view = PersonView {
                front_strip: Some(StripId(12)),
                front_group: BTreeMap::from([(StripId(12), 23)]),
                front_tab: BTreeMap::from([(21, 32)]),
                widths: BTreeMap::from([(21, WindowWidth::Fraction(700))]),
                maximized: BTreeSet::from([23]),
            };
            assert!(her_view.applied(&mut hers, WorkspaceId::from_u128(1)));
            let his_view = PersonView {
                front_strip: Some(StripId(11)),
                ..PersonView::default()
            };
            assert!(his_view.applied(&mut his, WorkspaceId::from_u128(1)));

            assert_eq!(
                hers.workspaces[0].strips.len(),
                his.workspaces[0].strips.len(),
                "one layout, whoever looks at it"
            );
            assert_eq!(hers.active_strip.get(&WorkspaceId::from_u128(1)), Some(&StripId(12)));
            assert_eq!(his.active_strip.get(&WorkspaceId::from_u128(1)), Some(&StripId(11)));
            assert_eq!(
                hers.active_group.get(&StripId(12)),
                Some(&23),
                "the group in front is the person's"
            );
            assert_eq!(
                hers.workspaces[0].strips[0].window_groups[0].active_tab, 32,
                "and the tab in front of a group"
            );
            assert_eq!(
                his.workspaces[0].strips[0].window_groups[0].active_tab, 31,
                "which the other person did not move"
            );
            assert_eq!(
                hers.workspaces[0].strips[0].window_groups[0].width,
                WindowWidth::Fraction(700)
            );
            assert!(hers.workspaces[0].strips[1].window_groups[0].maximized);
            assert!(!his.workspaces[0].strips[1].window_groups[0].maximized);
        }

        phase a_view_reads_back_from_the_record_of_the_person;
        // What one device of the person kept, the other reads: the view is the person's.
        {
            let tree = layout();
            let view = PersonView {
                front_strip: Some(StripId(12)),
                ..PersonView::of(&tree, WorkspaceId::from_u128(1))
            };
            let mut doc = AutoCommit::new();
            assert!(
                view.write(&mut doc)
                    .expect("a document that takes the view")
            );
            assert!(
                !view.write(&mut doc).expect("the same view again"),
                "a view that did not move writes nothing"
            );
            let saved = AutoCommit::load(&doc.save()).expect("the view's record");
            assert_eq!(PersonView::read(&saved), view);

            let mut elsewhere = layout();
            let mut moved = view.clone();
            moved.front_tab.insert(21, 32);
            assert!(moved.applied(&mut elsewhere, WorkspaceId::from_u128(1)));
            assert_eq!(PersonView::of(&elsewhere, WorkspaceId::from_u128(1)), moved);
        }

        phase a_view_of_windows_that_are_gone_leaves_the_layout_alone;
        // A view naming windows the layout no longer holds shows the layout as it is.
        {
            let mut tree = layout();
            let view = PersonView {
                front_strip: Some(StripId(99)),
                front_group: BTreeMap::from([(StripId(11), 99)]),
                front_tab: BTreeMap::from([(21, 99)]),
                ..PersonView::default()
            };
            assert!(!view.applied(&mut tree, WorkspaceId::from_u128(1)));
            assert_eq!(tree.active_strip.get(&WorkspaceId::from_u128(1)), None);
            assert_eq!(PersonView::of(&tree, WorkspaceId::from_u128(1)).front_tab.len(), 3);
        }
    }

    /// A view reads back as it was written, the groups made as large as their display with it.
    #[test]
    fn a_view_reads_back_as_written() {
        let view = PersonView {
            front_strip: Some(StripId(11)),
            widths: [(21, WindowWidth::Fraction(480))].into_iter().collect(),
            maximized: [22].into_iter().collect(),
            ..PersonView::default()
        };
        let mut doc = AutoCommit::new();
        assert!(view.write(&mut doc).expect("the view is written"));
        assert_eq!(PersonView::read(&doc), view);
    }

    /// A tab the person brought to the front here, before this device wrote it, stays in front
    /// when another device's view arrives; what this device did not change takes the record's.
    #[test]
    fn what_the_person_changed_here_stays_over_another_devices_view() {
        let view = |tabs: &[(u64, u64)], strip: u64| PersonView {
            front_strip: Some(StripId(strip)),
            front_tab: tabs.iter().copied().collect(),
            ..PersonView::default()
        };
        let written = view(&[(1, 10), (2, 20)], 7);
        let here = view(&[(1, 11), (2, 20)], 7);
        let record = view(&[(1, 10), (2, 21)], 8);
        assert_eq!(
            record.over(&here, &written),
            view(&[(1, 11), (2, 21)], 8),
            "the tab chosen here stays; the other device's choices and strip come in"
        );
        assert_eq!(
            record.over(&written, &written),
            record,
            "nothing changed here"
        );
    }
}
