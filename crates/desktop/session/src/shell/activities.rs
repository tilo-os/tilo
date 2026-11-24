//! The apps' live activities as the shell's host reads them: the board every app shows its live
//! activity on, what one app publishes there (the clocks app's running timer, which the shell's
//! clock reads), and the due events an app wants announced.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ui::live_activity::Published;

use crate::platform::notice::{Notice, Notices as Told};

/// The app whose live activity is the clocks app's when the shell reads the running timer.
pub use ui::apps::CLOCKS as CLOCKS_APP;

/// What the board published, parsed, at the revision it was read at.
type Parsed = (u64, Rc<Vec<(String, Published)>>);

/// The apps' live activities on the one board they show them on, and the announcements already
/// told.
pub(crate) struct Activities {
    board: Arc<runtime_collab::LiveActivities>,
    /// What the board last published, parsed, with the revision it was read at: the pass reads it
    /// every frame, and the activities are JSON.
    shown: RefCell<Option<Parsed>>,
    /// The announcements posted, by `app:id` .
    announced: RefCell<std::collections::BTreeSet<String>>,
}

impl Activities {
    pub(crate) fn new(board: Arc<runtime_collab::LiveActivities>) -> Self {
        Self {
            board,
            shown: RefCell::default(),
            announced: RefCell::default(),
        }
    }

    /// The board the apps show their live activities on.
    pub(crate) fn board(&self) -> &Arc<runtime_collab::LiveActivities> {
        &self.board
    }

    /// What one app publishes now.
    pub(crate) fn published(&self, app: &str) -> Option<Published> {
        let activity = self.board.activity(app)?;
        serde_json::from_value(activity)
            .inspect_err(|error| tracing::warn!(%error, app, "a live activity does not read back"))
            .ok()
    }

    /// The due events the apps want announced, each told once.
    pub(crate) fn announce(&self, notices: &dyn Told) {
        for (app, published) in self.read().iter() {
            let Some(announcement) = &published.announce else {
                continue;
            };
            let key = format!("{app}:{}", announcement.id);
            if !self.announced.borrow_mut().insert(key) {
                continue;
            }
            let title = if announcement.title.is_empty() {
                crate::i18n::t("clock-due-title")
            } else {
                announcement.title.clone()
            };
            let body = if announcement.body.is_empty() {
                crate::i18n::t("clock-due-body")
            } else {
                announcement.body.clone()
            };
            notices.show(&Notice::new(String::new(), 1, title, body));
        }
    }

    /// What every app publishes now, by app, parsed once per board revision.
    fn read(&self) -> Rc<Vec<(String, Published)>> {
        let revision = self.board.revision();
        if let Some((at, shown)) = self.shown.borrow().as_ref()
            && *at == revision
        {
            return Rc::clone(shown);
        }
        let shown: Rc<Vec<(String, Published)>> = Rc::new(
            self.board
                .activities()
                .into_iter()
                .filter_map(|(app, activity)| {
                    serde_json::from_value(activity)
                        .inspect_err(|error| {
                            tracing::warn!(%error, app, "a live activity does not read back");
                        })
                        .ok()
                        .map(|published| (app, published))
                })
                .collect(),
        );
        self.shown.replace(Some((revision, Rc::clone(&shown))));
        shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn publish(board: &runtime_collab::LiveActivities, app: &str, published: Published) {
        board.show(app, Some(serde_json::to_value(published).expect("JSON")));
    }

    /// A due event is announced once, in the app's own words when it has them and in the desktop's
    /// otherwise.
    #[test]
    fn a_due_event_is_announced_once() {
        let board = Arc::new(runtime_collab::LiveActivities::default());
        let activities = Activities::new(Arc::clone(&board));
        let notices = crate::notifications::told::Told::default();
        let announced = || notices.told();
        publish(
            &board,
            CLOCKS_APP,
            Published::announcing(ui::live_activity::Announcement {
                id: "due-1".into(),
                title: "Bread".into(),
                body: "Out of the oven".into(),
            }),
        );
        activities.announce(&notices);
        let live = announced();
        assert_eq!(
            live,
            [(String::from("Bread"), String::from("Out of the oven"))]
        );

        // The same due event, read again, is not announced again.
        activities.announce(&notices);
        assert_eq!(announced().len(), 1, "still one");

        // An app that names its own due event in no words gets the desktop's.
        board.show(CLOCKS_APP, None);
        publish(
            &board,
            CLOCKS_APP,
            Published::announcing(ui::live_activity::Announcement {
                id: "due-2".into(),
                title: String::new(),
                body: String::new(),
            }),
        );
        activities.announce(&notices);
        let live = announced();
        assert_eq!(live.len(), 2, "the next one is its own announcement");
        assert_eq!(live[1].0, crate::i18n::t("clock-due-title"));
        assert_eq!(live[1].1, crate::i18n::t("clock-due-body"));
    }
}
