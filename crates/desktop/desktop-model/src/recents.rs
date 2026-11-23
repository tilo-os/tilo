//! What a person opened lately, read off the history the workspace's layout record already keeps.
//!
//! A window that shows a resource names it in its entry of the shared layout record
//! (`windows.<id>.record`, [`crate::shared_resource`]), and every change of that record says who
//! made it and when. Each change that points a window at a resource it did not show before is an
//! open: by the change's author, at the change's time. Nothing else is written for it; recents
//! are a reading of that history, and a person's "clear" is a mark in their own view of the
//! layout ([`crate::person_view::hide_recents`]), never a change of the workspace's history.

use std::collections::{HashMap, HashSet};

use automerge::legacy::{Key, ObjectId, OpType};
use automerge::{AutoCommit, ChangeHash, ScalarValue};

use crate::shared_resource;

/// The field of a window's entry naming the resource it shows.
const RECORD: &str = "record";

/// One resource put in a window: what it is, who put it there and when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    /// The resource's address, as the window names it.
    pub resource: String,
    /// The person whose change put it there.
    pub by: String,
    /// When, in seconds since the epoch, as the change says.
    pub at: i64,
}

/// The opens of one layout record, read once and then followed change by change: a look reads
/// only the changes that arrived since the last.
#[derive(Clone, Debug, Default)]
pub struct Opens {
    heads: Vec<ChangeHash>,
    /// What each window entry showed after the changes read so far.
    showing: HashMap<ObjectId, String>,
    opened: Vec<Opened>,
}

impl Opens {
    /// Read what `doc` gained since the last read. Whether anything was opened in it.
    pub fn follow(&mut self, doc: &mut AutoCommit) -> bool {
        let heads = doc.get_heads();
        if heads == self.heads {
            return false;
        }
        let before = self.opened.len();
        for change in doc.get_changes(&self.heads) {
            let Some(by) = shared_resource::change_author(change.message()) else {
                continue;
            };
            let at = change.timestamp();
            for op in change.decode().operations {
                let (Key::Map(key), OpType::Put(ScalarValue::Str(resource))) =
                    (&op.key, &op.action)
                else {
                    continue;
                };
                if key != RECORD || resource.is_empty() {
                    continue;
                }
                let resource = resource.to_string();
                if self.showing.get(&op.obj) == Some(&resource) {
                    continue;
                }
                self.showing.insert(op.obj.clone(), resource.clone());
                self.opened.push(Opened {
                    resource,
                    by: by.clone(),
                    at,
                });
            }
        }
        self.heads = heads;
        // Changes arrive in causal order; recency is the time each says it was made.
        self.opened.sort_by_key(|opened| opened.at);
        self.opened.len() != before
    }

    /// Every open read so far, oldest first.
    #[must_use]
    pub fn opened(&self) -> &[Opened] {
        &self.opened
    }
}

/// A resource in a person's recents: when it was last opened, by whom, and whether by them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recent {
    pub resource: String,
    pub by: String,
    pub at: i64,
    /// Whether the person looking opened it last themselves.
    pub mine: bool,
}

/// The recents of `person` among `opened` (oldest first): each resource once, at its latest open
/// that `keep` takes; what they opened themselves first, then what others did, each newest
/// first; at most `limit`.
#[must_use]
pub fn recent(
    opened: &[Opened],
    person: &str,
    keep: impl Fn(&Opened) -> bool,
    limit: usize,
) -> Vec<Recent> {
    let mut seen = HashSet::new();
    let mut mine = Vec::new();
    let mut others = Vec::new();
    // A resource's own latest open counts, whoever made it; an earlier one by the person does not
    // lift it above the others.
    for open in opened.iter().rev() {
        if !keep(open) || !seen.insert(open.resource.as_str()) {
            continue;
        }
        let own = open.by == person;
        let entry = Recent {
            resource: open.resource.clone(),
            by: open.by.clone(),
            at: open.at,
            mine: own,
        };
        if own {
            mine.push(entry);
        } else {
            others.push(entry);
        }
        if mine.len() >= limit {
            break;
        }
    }
    mine.extend(others);
    mine.truncate(limit);
    mine
}

/// What a recents list is narrowed to: everything, one app's resources, or one type's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    All,
    App(String),
    Kind(String),
}

impl Scope {
    /// The scope an app and a kind name, the app first; both empty is everything.
    #[must_use]
    pub fn of(app: Option<&str>, kind: Option<&str>) -> Self {
        fn named(text: Option<&str>) -> Option<&str> {
            text.map(str::trim).filter(|text| !text.is_empty())
        }
        match (named(app), named(kind)) {
            (Some(app), _) => Self::App(app.to_owned()),
            (None, Some(kind)) => Self::Kind(kind.to_owned()),
            (None, None) => Self::All,
        }
    }

    /// The key a person's clear of this scope is kept under.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::All => String::new(),
            Self::App(app) => format!("app:{app}"),
            Self::Kind(kind) => format!("kind:{kind}"),
        }
    }
}

/// The time before which `person`'s recents hide a resource of `app` and `kind`, by the clears
/// `hidden` keeps ([`crate::person_view::recents_hidden`]): the latest of everything's, the
/// app's and the type's.
#[must_use]
pub fn hidden_before(
    hidden: &std::collections::BTreeMap<String, i64>,
    app: &str,
    kind: &str,
) -> i64 {
    [
        Scope::All,
        Scope::App(app.to_owned()),
        Scope::Kind(kind.to_owned()),
    ]
    .iter()
    .filter_map(|scope| hidden.get(&scope.key()).copied())
    .max()
    .unwrap_or(i64::MIN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Strip, Tab, TileContent, WindowGroup};
    use crate::ownership::SharedDesktop;
    use crate::{Layout, StripId, WindowWidth, Workspace, WorkspaceId};

    fn desktop(shows: &[(u64, &str)]) -> SharedDesktop {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Garden");
        let mut strip = Strip::new(StripId(11), "A");
        strip.window_groups = shows
            .iter()
            .map(|(id, _)| WindowGroup {
                // The group's id: one no tab of the test has.
                id: id | 0x1_0000,
                tabs: vec![Tab {
                    id: *id,
                    title: "Video".into(),
                    tile: TileContent::LocalApp {
                        app_id: "so.tilo.tube".into(),
                        label: "Video".into(),
                        note: String::new(),
                    },
                    app_title: None,
                    view: String::new(),
                    pinned: false,
                }],
                active_tab: *id,
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            })
            .collect();
        workspace.strips = vec![strip];
        SharedDesktop {
            tree: Layout {
                workspaces: vec![workspace],
                ..Layout::default()
            },
            records: shows
                .iter()
                .map(|(id, record)| (*id, (*record).to_owned()))
                .collect(),
            ..SharedDesktop::default()
        }
    }

    fn write(doc: &mut AutoCommit, shows: &[(u64, &str)], by: &str, at: i64) {
        assert!(shared_resource::write(doc, &desktop(shows)).expect("the record takes it"));
        shared_resource::commit_change(doc, by, at, None);
    }

    tilo_test_env::scenario! {
        fn recents_are_read_off_the_layouts_history;

        phase each_resource_a_window_comes_to_show_is_an_open;
        {
            let mut doc = AutoCommit::new();
            let mut opens = Opens::default();
            write(&mut doc, &[(1, "tube:a")], "ada", 10);
            write(&mut doc, &[(1, "tube:a"), (2, "tube:b")], "ada", 20);
            // The first window shows another video now: a third open, and the window that kept
            // showing `b` opens nothing again.
            write(&mut doc, &[(1, "tube:c"), (2, "tube:b")], "bob", 30);
            assert!(opens.follow(&mut doc));
            let read: Vec<_> = opens
                .opened()
                .iter()
                .map(|open| (open.resource.as_str(), open.by.as_str(), open.at))
                .collect();
            assert_eq!(
                read,
                [("tube:a", "ada", 10), ("tube:b", "ada", 20), ("tube:c", "bob", 30)]
            );
            // A look with nothing new reads nothing.
            assert!(!opens.follow(&mut doc));
            // Closing a window is no open, and opening `a` again is the latest of `a`.
            write(&mut doc, &[(2, "tube:b")], "ada", 40);
            write(&mut doc, &[(2, "tube:b"), (3, "tube:a")], "ada", 50);
            assert!(opens.follow(&mut doc));
            let mine = recent(opens.opened(), "ada", |_| true, 10);
            let listed: Vec<_> = mine
                .iter()
                .map(|r| (r.resource.as_str(), r.mine))
                .collect();
            assert_eq!(
                listed,
                [("tube:a", true), ("tube:b", true), ("tube:c", false)],
                "her own opens first, newest first, each resource once"
            );
        }

        phase ten_thousand_opens_read_newest_first_and_once_each;
        {
            let opened: Vec<Opened> = (0..10_000)
                .map(|n| Opened {
                    resource: format!("tube:{}", n % 400),
                    by: if n % 3 == 0 { "ada" } else { "bob" }.to_owned(),
                    at: n,
                })
                .collect();
            let started = std::time::Instant::now();
            let all = recent(&opened, "ada", |_| true, usize::MAX);
            assert!(started.elapsed() < std::time::Duration::from_millis(50));
            assert_eq!(all.len(), 400, "each resource once");
            assert!(all.windows(2).all(|pair| pair[0].mine >= pair[1].mine));
            let hers: Vec<_> = all.iter().filter(|r| r.mine).collect();
            assert!(hers.windows(2).all(|pair| pair[0].at > pair[1].at));
            assert_eq!(hers.first().map(|r| r.resource.as_str()), Some("tube:399"));
            let first = recent(&opened, "ada", |open| open.at < 5_000, 20);
            assert_eq!(first.len(), 20);
            assert!(first.iter().all(|r| r.at < 5_000));
        }

        phase a_clear_hides_by_the_widest_mark_that_covers_it;
        {
            let mut hidden = std::collections::BTreeMap::new();
            assert_eq!(hidden_before(&hidden, "so.tilo.tube", "tube.video"), i64::MIN);
            hidden.insert(Scope::of(Some("so.tilo.tube"), None).key(), 20);
            hidden.insert(Scope::of(None, Some("tube.video")).key(), 30);
            assert_eq!(hidden_before(&hidden, "so.tilo.tube", "tube.video"), 30);
            assert_eq!(hidden_before(&hidden, "so.tilo.tube", "tube.channel"), 20);
            assert_eq!(hidden_before(&hidden, "so.tilo.notes", "note"), i64::MIN);
            hidden.insert(Scope::All.key(), 40);
            assert_eq!(hidden_before(&hidden, "so.tilo.notes", "note"), 40);
        }

        phase a_clear_is_kept_in_the_persons_view_and_outlives_its_writes;
        {
            use crate::person_view::{PersonView, hide_recents, recents_hidden};
            let mut view = AutoCommit::new();
            let tube = Scope::of(Some("so.tilo.tube"), None);
            assert!(hide_recents(&mut view, &tube, 50).expect("the view takes it"));
            assert!(
                !hide_recents(&mut view, &tube, 40).expect("the view takes it"),
                "an earlier clear leaves the later mark"
            );
            // The view's own writes keep the mark: it is a key they do not know.
            PersonView {
                front_strip: Some(StripId(11)),
                ..PersonView::default()
            }
            .write(&mut view)
            .expect("the view is written");
            view.commit();
            let reopened = AutoCommit::load(&view.save()).expect("the view reads");
            assert_eq!(
                recents_hidden(&reopened),
                std::collections::BTreeMap::from([("app:so.tilo.tube".to_owned(), 50)])
            );
        }
    }
}
