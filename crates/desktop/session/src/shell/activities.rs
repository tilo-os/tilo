//! The apps' live activities as the shell's host reads them: the board every app shows its live
//! activity on, what one app publishes there (the clocks app's running timer, which the shell's
//! clock reads), and the due events an app wants announced.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ui::live_activity::{Published, Summary};

use crate::platform::notice::{Notice, Notices as Told};

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
    /// What the person put away that its app cannot end from outside, by `app:id`, with the id
    /// and title it had then: it stays away until the app shows something else.
    hidden: RefCell<std::collections::BTreeMap<String, (String, String)>>,
}

impl Activities {
    pub(crate) fn new(board: Arc<runtime_collab::LiveActivities>) -> Self {
        Self {
            board,
            shown: RefCell::default(),
            announced: RefCell::default(),
            hidden: RefCell::default(),
        }
    }

    /// The board the apps show their live activities on.
    pub(crate) fn board(&self) -> &Arc<runtime_collab::LiveActivities> {
        &self.board
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

    /// Every activity the apps show now and the person did not put away, by app, with whether
    /// its app asks for it to stand over the windows as a card.
    pub(crate) fn summaries(&self) -> Vec<(String, Summary, bool)> {
        self.read()
            .iter()
            .filter_map(|(app, published)| {
                let summary = published.summary.as_ref()?;
                self.shows(app, summary)
                    .then(|| (app.clone(), summary.clone(), published.overlay))
            })
            .collect()
    }

    /// A control the person pressed on an activity reaches its app.
    pub(crate) fn press(&self, app: &str, activity: &str, command: &str) {
        self.board.press(
            app,
            serde_json::json!({
                "command": command,
                "activity_id": (!activity.is_empty()).then_some(activity),
            }),
        );
    }

    /// The person dragged an activity's progress bar to `fraction` of it, from 0 to 1: its app
    /// goes there, through the same press a control makes.
    pub(crate) fn seek(&self, app: &str, activity: &str, fraction: f64) {
        self.board.press(
            app,
            serde_json::json!({
                "command": "seek",
                "activity_id": (!activity.is_empty()).then_some(activity),
                "fraction": fraction.clamp(0.0, 1.0),
            }),
        );
    }

    /// The person ended or put away an activity: an app that offers a close is asked to end it,
    /// and one that cannot be stopped from outside goes away until it shows something else.
    pub(crate) fn close(&self, app: &str, activity: &str) {
        let Some(summary) = self
            .read()
            .iter()
            .find(|(shown, _)| shown == app)
            .and_then(|(_, published)| published.summary.clone())
            .filter(|summary| summary.id == activity)
        else {
            return;
        };
        if let Some(close) = summary.close_control() {
            self.press(app, activity, &close.id);
        } else {
            self.hidden
                .borrow_mut()
                .insert(format!("{app}:{activity}"), (summary.id, summary.title));
        }
    }

    /// Whether `summary` shows: an activity the person put away stays away until the app shows
    /// another one.
    fn shows(&self, app: &str, summary: &Summary) -> bool {
        let key = format!("{app}:{}", summary.id);
        let hidden = self.hidden.borrow().get(&key).cloned();
        match hidden {
            Some(put_away) if put_away == (summary.id.clone(), summary.title.clone()) => false,
            Some(_) => {
                self.hidden.borrow_mut().remove(&key);
                true
            }
            None => true,
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
    use ui::apps::CLOCKS as CLOCKS_APP;

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
