#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion"
)]

use std::rc::Rc;

use ui::tilo_resource::{Id, Invoke, NonBlank, Replica, Resource as _};

use crate::issue_list::{IssueList, Issues, issue_list};
use crate::model::{Issue, NewIssue, Status, Tracker, tracker};
use ui::framework::PartLimits;

/// Issues a part holds in these tests before the next one starts: fewer
/// than the framework's, so a few hundred issues show what thousands would.
pub(crate) const PART_ISSUES: usize = 500;

/// A tracker and its parts as one device holds them: every action goes to
/// the part the framework places it in, read against the tracker and the parts
/// held, as a window carries it out.
pub(crate) struct Board {
    pub(crate) tracker: Replica<Tracker>,
    parts: Vec<(u64, Replica<IssueList>)>,
    /// Each part's state as last written, as a window follows them.
    states: Vec<(u64, Rc<IssueList>)>,
    author: String,
    now_ms: i64,
    _limits: PartLimits,
}

impl Board {
    pub(crate) fn new(author: &str) -> Self {
        let mut tracker = Replica::<Tracker>::new().as_author(author);
        tracker.set_now_ms(1_000);
        Self {
            tracker,
            parts: Vec::new(),
            states: Vec::new(),
            author: author.to_owned(),
            now_ms: 1_000,
            _limits: PartLimits::scoped(PART_ISSUES, usize::MAX),
        }
    }

    pub(crate) fn states(&self) -> Vec<(u64, Rc<IssueList>)> {
        self.states.clone()
    }

    /// Follow every part again, after a sync wrote into them.
    fn reread(&mut self) {
        self.states = self
            .parts
            .iter()
            .map(|(index, part)| (*index, Rc::new(part.state().unwrap())))
            .collect();
    }

    /// Follow part `index` after a write into it.
    fn follow(&mut self, index: u64) {
        let state = Rc::new(self.part(index).live().unwrap().clone());
        match self.states.iter_mut().find(|(held, _)| *held == index) {
            Some((_, held)) => *held = state,
            None => self.states.push((index, state)),
        }
    }

    fn part(&mut self, index: u64) -> &mut Replica<IssueList> {
        if !self.parts.iter().any(|(held, _)| *held == index) {
            let carried = self
                .parts
                .last()
                .map(|(_, part)| crate::issue_list::carried(&part.state().unwrap()));
            let mut part = Replica::<IssueList>::new().as_author(&self.author);
            part.set_now_ms(self.now_ms);
            if let Some(before) = carried {
                part.apply(IssueList::START.with(issue_list::Start { before }))
                    .unwrap();
            }
            self.parts.push((index, part));
        }
        &mut self
            .parts
            .iter_mut()
            .find(|(held, _)| *held == index)
            .unwrap()
            .1
    }

    pub(crate) fn apply(&mut self, invoke: impl Into<Invoke<IssueList>>) -> Result<u64, String> {
        let mut action = invoke.into().build(self.now_ms)?;
        let parts = self.states();
        crate::issue_list::prepare(
            std::slice::from_mut(&mut action),
            &self.tracker.state().unwrap(),
            &parts,
        )?;
        let index = ui::framework::placed::<IssueList>(&action, &parts);
        self.part(index)
            .apply_action(&action)
            .map_err(|error| error.to_string())?;
        self.follow(index);
        Ok(index)
    }

    pub(crate) fn create(&mut self, title: &str, parent: Option<Id>) -> Id {
        self.apply(IssueList::CREATE.with(NewIssue {
            parent: parent.map(ui::tilo_resource::items::Ref::from),
            ..NewIssue::titled(nb(title))
        }))
        .unwrap();
        self.id_of(title)
    }

    fn id_of(&self, title: &str) -> Id {
        let parts = self.states();
        Issues::of(&parts)
            .iter()
            .find(|(_, issue)| issue.title == title)
            .map(|(id, _)| id.clone())
            .expect("the issue is there")
    }

    fn issue(&self, id: &Id) -> Option<Issue> {
        let parts = self.states();
        Issues::of(&parts).get(id).cloned()
    }

    fn titles(&self) -> Vec<String> {
        let parts = self.states();
        Issues::of(&parts)
            .iter()
            .map(|(_, issue)| issue.title.clone())
            .collect()
    }
}

/// `text` as the non-blank text an action takes.
fn nb(text: &str) -> NonBlank {
    NonBlank::parse(text).expect("the test's text says something")
}

#[test]
fn issues_are_numbered_nested_and_never_under_themselves() {
    let mut board = Board::new("ada");
    let parent = board.create("Ship it", None);
    let child = board.create("Write notes", Some(parent.clone()));
    assert_eq!(board.issue(&parent).unwrap().number, 1);
    assert_eq!(board.issue(&child).unwrap().number, 2);
    assert_eq!(
        board.issue(&child).unwrap().author,
        Some(ui::tilo_resource::MemberId::person("ada"))
    );
    let parts = board.states();
    assert_eq!(Issues::of(&parts).progress(&parent), (0, 1));
    assert!(
        board
            .apply(IssueList::MOVE_UNDER.with(issue_list::MoveUnder {
                issue: parent.clone(),
                parent: Some(ui::tilo_resource::items::Ref::from(child.clone())),
            }))
            .is_err(),
        "a parent cannot move under its own sub-issue"
    );
    assert!(
        board
            .apply(IssueList::CREATE.with(NewIssue {
                parent: Some(ui::tilo_resource::items::Ref::from("gone")),
                ..NewIssue::titled(nb("Orphan"))
            }))
            .is_err(),
        "a parent that is not an issue is refused"
    );
    // Deleting the parent leaves its sub-issue at the top level.
    board
        .apply(IssueList::DELETE.with(issue_list::Delete { issue: parent }))
        .unwrap();
    let parts = board.states();
    let issues = Issues::of(&parts);
    let (top, _) = issues.children(None).next().unwrap();
    assert_eq!(top, &child);
}

#[test]
fn a_full_part_starts_the_next_and_numbers_go_on_across_them() {
    let mut board = Board::new("ada");
    for index in 0..=PART_ISSUES {
        board
            .apply(IssueList::CREATE.with(NewIssue::titled(nb(&format!("Issue {index}")))))
            .unwrap();
    }
    assert_eq!(
        board.parts.len(),
        2,
        "the issue past a full part starts another"
    );
    let last = board.id_of(&format!("Issue {PART_ISSUES}"));
    let issue = board.issue(&last).unwrap();
    assert_eq!(issue.number, u64::try_from(PART_ISSUES).unwrap() + 1);
    let parts = board.states();
    let issues = Issues::of(&parts);
    assert_eq!(issues.part_of(&last), Some(1));
    assert_eq!(
        issues.iter().last().map(|(id, _)| id),
        Some(&last),
        "a new issue goes after every other, whichever part keeps them"
    );

    // A change to an issue of the first part is written there, and an issue
    // nests under one kept in another part.
    let first = board.id_of("Issue 0");
    assert_eq!(
        board
            .apply(IssueList::MOVE_UNDER.with(issue_list::MoveUnder {
                issue: last.clone(),
                parent: Some(ui::tilo_resource::items::Ref::from(first.clone())),
            }))
            .unwrap(),
        1
    );
    assert_eq!(
        board
            .apply(IssueList::RENAME.with(issue_list::Rename {
                issue: first.clone(),
                title: nb("First"),
            }))
            .unwrap(),
        0
    );
    let parts = board.states();
    let issues = Issues::of(&parts);
    assert_eq!(issues.progress(&first), (0, 1));
    assert!(issues.is_under(&last, &first));
    assert!(
        board
            .apply(IssueList::MOVE_UNDER.with(issue_list::MoveUnder {
                issue: first,
                parent: Some(ui::tilo_resource::items::Ref::from(last)),
            }))
            .is_err(),
        "a cycle across parts is refused too"
    );
}

#[test]
fn numbers_given_after_a_merge_are_past_both_sides() {
    let mut left = Board::new("ada");
    left.create("First", None);
    let mut right = Board::new("grace");
    right.parts = vec![(0, left.parts[0].1.fork().as_author("grace"))];
    right.reread();
    left.create("Left", None);
    right.create("Right", None);
    left.parts[0].1.sync_with(&mut right.parts[0].1).unwrap();
    left.reread();
    let after = left.create("After", None);
    assert_eq!(left.issue(&after).unwrap().number, 4);
}

#[test]
fn placing_on_the_board_sets_status_and_order_in_one_step() {
    let mut board = Board::new("ada");
    for title in ["One", "Two", "Three"] {
        board.create(title, None);
    }
    let one = board.id_of("One");
    let three = board.id_of("Three");
    board
        .apply(IssueList::PLACE.with(issue_list::Place {
            issue: three.clone(),
            status: Status::Todo,
            before: Some(ui::tilo_resource::items::Ref::from(one)),
            rank: None,
        }))
        .unwrap();
    assert_eq!(board.titles(), ["Three", "One", "Two"]);
    assert_eq!(board.issue(&three).unwrap().status, Status::Todo);
    board
        .apply(IssueList::PLACE.with(issue_list::Place {
            issue: three.clone(),
            status: Status::Done,
            before: None,
            rank: None,
        }))
        .unwrap();
    assert_eq!(board.titles(), ["One", "Two", "Three"]);
    assert_eq!(board.issue(&three).unwrap().status, Status::Done);
}

#[test]
fn a_project_holds_its_tasks_and_a_deleted_one_holds_none() {
    let mut board = Board::new("ada");
    board
        .tracker
        .apply(Tracker::CREATE_AREA.with(tracker::CreateArea {
            name: nb("Launch"),
            kind: None,
        }))
        .unwrap();
    let launch = board
        .tracker
        .state()
        .unwrap()
        .areas
        .iter()
        .next()
        .map(|(id, _)| id.clone())
        .unwrap();
    board
        .apply(IssueList::CREATE.with(NewIssue {
            project: Some(ui::tilo_resource::items::Ref::from(launch.clone())),
            ..NewIssue::titled(nb("Write the post"))
        }))
        .unwrap();
    let post = board.id_of("Write the post");
    assert_eq!(
        board.issue(&post).unwrap().area.map(|area| area.label),
        Some("Launch".to_owned())
    );
    assert!(
        board
            .apply(IssueList::SET_PROJECT.with(issue_list::SetProject {
                issue: post.clone(),
                project: Some(ui::tilo_resource::items::Ref::from("nope")),
                goal: None,
            }))
            .is_err(),
        "a project the tracker does not have is refused"
    );
    {
        let parts = board.states();
        assert_eq!(Issues::of(&parts).in_area(&launch).count(), 1);
    }
    board
        .tracker
        .apply_confirmed(Tracker::DELETE_AREA.with(tracker::DeleteArea { area: launch }))
        .unwrap();
    let tracker = board.tracker.state().unwrap();
    let issue = board.issue(&post).unwrap();
    assert_eq!(
        Issues::goal_of(&tracker, &issue),
        None,
        "an issue of a deleted project works towards none"
    );
}

#[test]
fn every_part_action_publishes_what_it_reads() {
    let mut board = Board::new("ada");
    board.create("One", None);
    let spec = board.parts[0].1.published().unwrap();
    assert!(
        spec.children
            .iter()
            .any(|child| child.id.as_str() == crate::model::issue_resource(&board.id_of("One")))
    );
    assert_eq!(IssueList::KIND, crate::model::ISSUES_KIND);
}

/// Giving an issue to an agent asks it, for whoever gave it; the agent's own
/// message in the thread answers that ask, and a person writing in the thread
/// afterwards asks it again.
#[test]
fn giving_an_issue_to_an_agent_asks_it_until_it_answers() {
    use ui::tilo_resource::MemberId;
    let asks =
        |part: &Replica<IssueList>| crate::model::agent_asks(part.state().unwrap().issues.iter());
    let mut ada = Replica::<IssueList>::new().as_author("ada");
    ada.set_now_ms(1_000);
    ada.apply(IssueList::CREATE.with(NewIssue::titled(nb("Plan the launch"))))
        .unwrap();
    let issue = ada.state().unwrap().issues.iter().next().unwrap().0.clone();
    assert!(asks(&ada).is_empty(), "nobody asked yet");

    ada.apply(IssueList::ASSIGN.with(issue_list::Assign {
        issue: issue.clone(),
        assignee: Some("agent:claude".into()),
    }))
    .unwrap();
    let asked = asks(&ada);
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].agent, "claude");
    assert_eq!(
        asked[0].person,
        MemberId::person("ada"),
        "it works for whoever gave it the issue"
    );
    assert_eq!(asked[0].issue, issue);

    let mut agent = ada.fork().as_agent_for("claude", "ada");
    agent.set_now_ms(2_000);
    agent
        .apply(IssueList::COMMENT.with(issue_list::Comment {
            issue: issue.clone(),
            text: nb("The plan is in the launch note"),
            origin: None,
        }))
        .unwrap();
    ada.sync_with(&mut agent).unwrap();
    assert!(asks(&ada).is_empty(), "its message answers the ask");

    ada.set_now_ms(3_000);
    ada.apply(IssueList::COMMENT.with(issue_list::Comment {
        issue: issue.clone(),
        text: nb("Add the press list too"),
        origin: None,
    }))
    .unwrap();
    let asked = asks(&ada);
    assert_eq!(
        asked.len(),
        1,
        "a person's later message asks again: {asked:?}"
    );
    assert_eq!(asked[0].words, "Add the press list too");

    // Given to a person, it asks no agent.
    ada.apply(IssueList::ASSIGN.with(issue_list::Assign {
        issue,
        assignee: Some(crate::model::person_assignee("jane").0),
    }))
    .unwrap();
    assert!(asks(&ada).is_empty());
}

#[test]
fn an_issue_and_a_project_are_named_by_their_names() {
    use ui::tilo_resource::items::Ref;
    let mut board = Board::new("ada");
    board
        .tracker
        .apply(Tracker::CREATE_AREA.with(tracker::CreateArea {
            name: nb("Launch"),
            kind: None,
        }))
        .unwrap();
    // An issue that works towards the project by the name it is read under.
    board
        .apply(IssueList::CREATE.with(NewIssue {
            project: Some(Ref::from("Launch")),
            ..NewIssue::titled(nb("Write the post"))
        }))
        .unwrap();
    let post = board.id_of("Write the post");
    assert_eq!(
        board
            .issue(&post)
            .unwrap()
            .area
            .as_ref()
            .map(|area| area.label.as_str()),
        Some("Launch")
    );
    // A project nothing is called says what there is, never that it went away.
    let why = board
        .apply(IssueList::CREATE.with(NewIssue {
            project: Some(Ref::from("Nowhere")),
            ..NewIssue::titled(nb("Lost"))
        }))
        .expect_err("no project is named Nowhere");
    // The model's catalogue owns the words: what it refuses with names the
    // project asked for and the ones there are.
    let expected = ui::tilo_resource::word!(
        "tasks-areas-not-found",
        catalogue = "../tasks-model/locales",
        asked = "Nowhere".to_owned(),
        holds = "Launch".to_owned()
    )
    .read();
    assert_eq!(why, expected);
    // An issue's parent is named the same way, by the title it is read under.
    board
        .apply(IssueList::CREATE.with(NewIssue {
            parent: Some(Ref::from("Write the post")),
            ..NewIssue::titled(nb("Reply"))
        }))
        .unwrap();
    let reply = board.id_of("Reply");
    let parts = board.states();
    let issues = Issues::of(&parts);
    assert_eq!(
        issues
            .get(&reply)
            .and_then(|issue| issues.parent_of(issue))
            .map(Id::as_str),
        Some(post.as_str())
    );
    // A parent nothing is called says the issues there are.
    let why = board
        .apply(IssueList::CREATE.with(NewIssue {
            parent: Some(Ref::from("Nothing")),
            ..NewIssue::titled(nb("Orphan"))
        }))
        .expect_err("no issue is named Nothing");
    assert!(why.contains("no issue named Nothing"), "{why}");
    assert!(
        why.contains("Write the post"),
        "what there is follows: {why}"
    );
}

#[test]
fn an_issue_is_named_by_its_title_its_number_or_its_id() {
    let mut board = Board::new("ada");
    board.create("Write the post", None);
    let id = board.id_of("Write the post");
    // By the title it is read under, wherever its part is.
    board
        .apply(IssueList::RENAME.with(issue_list::Rename {
            issue: Id::from_raw("Write the post"),
            title: nb("Write it"),
        }))
        .unwrap();
    // By the number it reads as.
    board
        .apply(IssueList::RENAME.with(issue_list::Rename {
            issue: Id::from_raw("#1"),
            title: nb("Write the post"),
        }))
        .unwrap();
    // And by its id, which no rename changes.
    board
        .apply(IssueList::RENAME.with(issue_list::Rename {
            issue: id,
            title: nb("Ship it"),
        }))
        .unwrap();
    assert_eq!(board.titles(), ["Ship it"]);
    // A reference to no issue says which there are.
    let why = board
        .apply(IssueList::RENAME.with(issue_list::Rename {
            issue: Id::from_raw("Nothing"),
            title: nb("Never"),
        }))
        .expect_err("no issue is named Nothing");
    assert!(why.contains("no issue named Nothing"), "{why}");
    assert!(why.contains("Ship it"), "what there is follows: {why}");
}

/// A day, in milliseconds.
const DAY_MS: i64 = 86_400_000;

/// What long work on an issue is tested with: the person who gives it, the
/// agent it is given to, and the issue.
struct Task {
    ada: Replica<IssueList>,
    agent: Replica<IssueList>,
    issue: Id,
}

impl Task {
    /// Ada files "Write the market analysis" and gives it to the agent
    /// `claude` at 1 000 ms; the agent holds the same document.
    fn given() -> Self {
        let mut ada = Replica::<IssueList>::new().as_author("ada");
        ada.set_now_ms(1_000);
        ada.apply(IssueList::CREATE.with(NewIssue {
            assignee: Some("agent:claude".into()),
            ..NewIssue::titled(nb("Write the market analysis"))
        }))
        .unwrap();
        let issue = ada.state().unwrap().issues.iter().next().unwrap().0.clone();
        let agent = ada.fork().as_agent_for("claude", "ada");
        Self { ada, agent, issue }
    }

    /// The agent does `invoke` at `now_ms`, and Ada's document takes it.
    fn agent_does(
        &mut self,
        now_ms: i64,
        invoke: impl Into<Invoke<IssueList>>,
    ) -> Result<(), String> {
        self.ada.sync_with(&mut self.agent).unwrap();
        self.agent.set_now_ms(now_ms);
        self.agent
            .apply(invoke)
            .map(drop)
            .map_err(|error| error.to_string())?;
        self.ada.sync_with(&mut self.agent).unwrap();
        Ok(())
    }

    /// Ada does `invoke` at `now_ms`.
    fn ada_does(
        &mut self,
        now_ms: i64,
        invoke: impl Into<Invoke<IssueList>>,
    ) -> Result<(), String> {
        self.ada.sync_with(&mut self.agent).unwrap();
        self.ada.set_now_ms(now_ms);
        self.ada
            .apply(invoke)
            .map(drop)
            .map_err(|error| error.to_string())
    }

    fn says(&mut self, now_ms: i64, text: &str) {
        let issue = self.issue.clone();
        self.agent_does(
            now_ms,
            IssueList::COMMENT.with(issue_list::Comment {
                issue,
                text: nb(text),
                origin: None,
            }),
        )
        .unwrap();
    }

    fn begins(&mut self, now_ms: i64) {
        let issue = self.issue.clone();
        self.agent_does(
            now_ms,
            IssueList::SESSION_BEGAN.with(issue_list::SessionBegan { issue }),
        )
        .unwrap();
    }

    fn ends(&mut self, now_ms: i64, cost_micros: u64, worked_ms: u64) {
        let issue = self.issue.clone();
        self.agent_does(
            now_ms,
            IssueList::SESSION_ENDED.with(issue_list::SessionEnded {
                issue,
                cost_micros,
                worked_ms,
            }),
        )
        .unwrap();
    }

    fn held(&self) -> Issue {
        self.ada
            .state()
            .unwrap()
            .issues
            .get(&self.issue)
            .unwrap()
            .clone()
    }

    fn work(&self) -> crate::model::Work {
        self.held().work.unwrap_or_default()
    }

    fn task_state(&self) -> crate::model::TaskState {
        crate::model::task_state(&self.held())
    }

    /// What asks the agent at `now_ms`, read from the document alone, as a server
    /// that just started reads it.
    fn asks(&self, now_ms: i64) -> Vec<crate::model::IssueAsk> {
        crate::model::agent_asks_at(self.ada.state().unwrap().issues.iter(), now_ms)
    }
}

fn reply_wait() -> crate::model::WaitFor {
    crate::model::WaitFor {
        kind: crate::model::WaitKind::Reply,
        what: "a reply from the supplier".into(),
        on: Some("mail-thread:supplier-17".into()),
        event: Some("e-reply".into()),
    }
}

/// A task that waits two days for a reply costs nothing while it waits, is
/// asked again when the reply comes, and in a second telling when the two
/// days are over; a reader that holds only the document reads the same asks.
#[test]
fn a_task_that_waits_is_asked_again_by_what_it_waited_for_or_by_its_deadline() {
    use crate::model::{Asked, TaskState};
    let until = 2_000 + 2 * DAY_MS;
    let waiting = || {
        let mut task = Task::given();
        assert_eq!(task.asks(1_500).len(), 1, "given, it is asked");
        task.begins(2_000);
        let issue = task.issue.clone();
        task.agent_does(
            2_100,
            IssueList::PLACE.with(issue_list::Place {
                issue: issue.clone(),
                status: Status::InProgress,
                before: None,
                rank: None,
            }),
        )
        .unwrap();
        task.agent_does(
            2_200,
            IssueList::WAIT.with(issue_list::Wait {
                issue,
                waits: vec![reply_wait()],
                until_ms: until,
            }),
        )
        .unwrap();
        task.says(2_300, "I wrote to the supplier and wait for the reply.");
        task.ends(2_400, 500, 400);
        task
    };

    // On the reply.
    let mut task = waiting();
    assert_eq!(
        task.task_state(),
        TaskState::Waiting {
            what: vec!["a reply from the supplier".into()],
            until_ms: until
        }
    );
    for now in [2_500, 2_000 + DAY_MS, until - 1] {
        assert!(task.asks(now).is_empty(), "nothing is asked while it waits");
    }
    let issue = task.issue.clone();
    task.agent_does(
        2_000 + DAY_MS,
        IssueList::RESUME.with(issue_list::Resume {
            issue,
            cause: nb("the supplier replied"),
        }),
    )
    .unwrap();
    let asked = task.asks(2_001 + DAY_MS);
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].asked, Asked::Resumed);
    assert_eq!(asked[0].words, "the supplier replied");
    assert_eq!(asked[0].person, ui::tilo_resource::MemberId::person("ada"));
    assert_eq!(
        task.task_state(),
        TaskState::Working,
        "it no longer reads as waiting"
    );
    task.begins(2_002 + DAY_MS);
    task.says(2_003 + DAY_MS, "The supplier's prices are in the sheet.");
    task.ends(2_004 + DAY_MS, 700, 300);
    assert!(
        task.asks(until + DAY_MS).is_empty(),
        "answered, and the old deadline wakes nothing"
    );
    let work = task.work();
    assert_eq!(
        (work.sessions, work.spent_micros, work.worked_ms),
        (2, 1_200, 700),
        "both sessions are counted"
    );

    // On the deadline, in a second telling.
    let mut task = waiting();
    assert!(task.asks(until - 1).is_empty());
    let asked = task.asks(until);
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].asked, Asked::WaitEnded);
    assert_eq!(asked[0].cause, format!("waited-{until}"));
    task.begins(until + 5);
    assert_eq!(
        task.task_state(),
        TaskState::Working,
        "the session ended the wait"
    );
    assert_eq!(task.work().until_ms, None);
    task.says(until + 6, "No reply came: I wrote again.");
    assert!(task.asks(until + DAY_MS).is_empty(), "answered once");
}

/// A session that was lost is asked again from the document alone, and one lost
/// while the task waited does not start it early. After the most lost sessions
/// in a row the task stops and says so: it never reads as done.
#[test]
fn a_task_whose_worker_was_lost_is_asked_again_then_stops_and_never_reads_as_done() {
    use crate::model::{Asked, MOST_LOST_SESSIONS, StoppedWhy, TaskState};
    // Lost after it began to wait: the wait still holds.
    let mut task = Task::given();
    task.begins(2_000);
    let issue = task.issue.clone();
    task.agent_does(
        2_100,
        IssueList::WAIT.with(issue_list::Wait {
            issue,
            waits: vec![reply_wait()],
            until_ms: 9_000,
        }),
    )
    .unwrap();
    assert!(
        task.asks(3_000).is_empty(),
        "what was asked before the wait is not asked again while it waits"
    );
    assert_eq!(task.asks(9_000)[0].asked, Asked::WaitEnded);

    // Lost while it worked: asked again, each new session counting the one before.
    let mut task = Task::given();
    let issue = task.issue.clone();
    for lost in 0..=MOST_LOST_SESSIONS {
        let asked = task.asks(2_000);
        assert_eq!(asked.len(), 1, "session {lost} is asked again: {asked:?}");
        task.begins(2_000 + i64::try_from(lost).unwrap());
        assert_eq!(task.work().lost, lost, "the sessions lost in a row");
    }
    assert!(task.work().lost >= MOST_LOST_SESSIONS);
    task.agent_does(
        3_000,
        IssueList::STOP.with(issue_list::Stop {
            issue: issue.clone(),
            why: StoppedWhy::LostWorker,
            words: nb("I stopped: my work on this was interrupted three times."),
        }),
    )
    .unwrap();
    let read = task.task_state();
    assert!(
        matches!(&read, TaskState::Stopped { why: StoppedWhy::LostWorker, words } if words.contains("interrupted")),
        "{read:?}"
    );
    assert!(!read.resolved(), "work that lost its worker is not done");
    assert!(
        task.asks(4_000).is_empty(),
        "stopped, it is not started again by what was asked before"
    );
    // Whatever its status is set to, it does not read as done.
    task.agent_does(
        3_500,
        IssueList::PLACE.with(issue_list::Place {
            issue: issue.clone(),
            status: Status::Done,
            before: None,
            rank: None,
        }),
    )
    .unwrap();
    assert!(matches!(task.task_state(), TaskState::Stopped { .. }));
    task.agent_does(
        3_600,
        IssueList::PLACE.with(issue_list::Place {
            issue: issue.clone(),
            status: Status::InProgress,
            before: None,
            rank: None,
        }),
    )
    .unwrap();

    // Its person says something new: it is asked again and works again.
    task.ada_does(
        5_000,
        IssueList::COMMENT.with(issue_list::Comment {
            issue,
            text: nb("Try again"),
            origin: None,
        }),
    )
    .unwrap();
    let asked = task.asks(5_001);
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].asked, Asked::Message);
    task.begins(5_002);
    assert_eq!(task.task_state(), TaskState::Working);
}

/// A budget is its person's to raise. Once it is spent the task stops with a
/// plain line, does not read as done, and goes on only after it was raised.
#[test]
fn a_spent_budget_stops_a_task_until_its_person_raises_it() {
    use crate::model::{Asked, BudgetBy, StoppedWhy, TaskState};
    let mut task = Task::given();
    let issue = task.issue.clone();
    let budget = |money_micros: Option<u64>, time_ms: Option<u64>| {
        IssueList::SET_BUDGET.with(issue_list::SetBudget {
            issue: issue.clone(),
            money_micros,
            time_ms,
        })
    };
    // The agent proposes one where none is set, and holds itself to it.
    task.agent_does(1_100, budget(Some(1_000_000), None))
        .unwrap();
    assert_eq!(task.work().budget_by, BudgetBy::Agent);
    // The person sets it: from then on only a person raises it.
    task.ada_does(1_200, budget(Some(2_000_000), None)).unwrap();
    assert_eq!(task.work().budget_by, BudgetBy::Person);
    let refused = task
        .agent_does(1_300, budget(Some(5_000_000), None))
        .unwrap_err();
    assert!(
        refused.contains("Only a person raises a budget a person set"),
        "{refused}"
    );
    assert_eq!(task.work().budget_micros, Some(2_000_000));
    // It may add a bound of its own beside it, and lower one.
    task.agent_does(1_400, budget(None, Some(600_000))).unwrap();
    assert_eq!(task.work().budget_by, BudgetBy::Person);
    assert!(
        task.ada_does(1_450, budget(None, None)).is_err(),
        "a budget names money or time"
    );

    task.begins(2_000);
    task.ends(2_500, 2_300_000, 400);
    assert_eq!(task.work().used_up(), Some(StoppedWhy::Budget));
    task.agent_does(
        2_600,
        IssueList::STOP.with(issue_list::Stop {
            issue: issue.clone(),
            why: StoppedWhy::Budget,
            words: nb("I stopped: this task's budget of $2.00 is spent."),
        }),
    )
    .unwrap();
    task.says(2_700, "I stopped: this task's budget of $2.00 is spent.");
    let read = task.task_state();
    assert!(
        matches!(&read, TaskState::Stopped { why: StoppedWhy::Budget, words } if words.contains("$2.00")),
        "{read:?}"
    );
    assert!(
        !read.resolved(),
        "work that stopped at its budget is not done"
    );
    assert!(
        task.asks(3_000).is_empty(),
        "nothing is asked past the budget"
    );

    let resume = || {
        IssueList::RESUME.with(issue_list::Resume {
            issue: issue.clone(),
            cause: nb("go on"),
        })
    };
    let refused = task.ada_does(3_100, resume()).unwrap_err();
    assert!(
        refused.contains("Its budget of $2.00 is spent: raise it first"),
        "{refused}"
    );
    task.ada_does(3_200, budget(Some(5_000_000), None)).unwrap();
    task.ada_does(3_300, resume()).unwrap();
    let asked = task.asks(3_400);
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].asked, Asked::Resumed);
    assert!(!matches!(task.task_state(), TaskState::Stopped { .. }));
}

/// An agent gives an issue for the one person it acts for; an issue it filed
/// for itself is a proposal until its person gives it.
#[test]
fn an_agent_gives_an_issue_for_its_person_and_what_it_filed_itself_waits_for_them() {
    use ui::tilo_resource::MemberId;
    let asks =
        |part: &Replica<IssueList>| crate::model::agent_asks(part.state().unwrap().issues.iter());
    let mut ada = Replica::<IssueList>::new().as_author("ada");
    let mut agent = ada.fork().as_agent_for("claude", "ada");
    agent.set_now_ms(1_000);
    agent
        .apply(IssueList::CREATE.with(NewIssue {
            assignee: Some("agent:claude".into()),
            ..NewIssue::titled(nb("Look at the renewal dates"))
        }))
        .unwrap();
    let proposal = agent
        .state()
        .unwrap()
        .issues
        .iter()
        .next()
        .unwrap()
        .0
        .clone();
    assert!(asks(&agent).is_empty(), "nobody asked for it yet");
    assert_eq!(
        crate::model::task_state(agent.state().unwrap().issues.get(&proposal).unwrap()),
        crate::model::TaskState::UpNext
    );
    // The nod: its person gives it to the agent it is with.
    let mut ada = agent.fork().as_author("ada");
    ada.set_now_ms(2_000);
    ada.apply(IssueList::ASSIGN.with(issue_list::Assign {
        issue: proposal.clone(),
        assignee: Some("agent:claude".into()),
    }))
    .unwrap();
    let asked = asks(&ada);
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].person, MemberId::person("ada"));
    // Giving it again asks nothing new.
    ada.set_now_ms(3_000);
    ada.apply(IssueList::ASSIGN.with(issue_list::Assign {
        issue: proposal,
        assignee: Some("agent:claude".into()),
    }))
    .unwrap();
    assert_eq!(asks(&ada)[0].cause, "assigned-2000");

    // The agent hands a piece on: the other agent works on it for the same person.
    let mut agent = Replica::<IssueList>::new().as_agent_for("claude", "ada");
    agent.set_now_ms(1_000);
    agent
        .apply(IssueList::CREATE.with(NewIssue::titled(nb("Read the price pages"))))
        .unwrap();
    let piece = agent
        .state()
        .unwrap()
        .issues
        .iter()
        .next()
        .unwrap()
        .0
        .clone();
    agent
        .apply(IssueList::ASSIGN.with(issue_list::Assign {
            issue: piece,
            assignee: Some("agent:scout".into()),
        }))
        .unwrap();
    let asked = asks(&agent);
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].agent, "scout");
    assert_eq!(asked[0].person, MemberId::person("ada"));

    // An agent that acts for nobody gives nobody's work to anyone.
    let mut lone = Replica::<IssueList>::new().as_member(MemberId::agent("lone"));
    lone.set_now_ms(1_000);
    lone.apply(IssueList::CREATE.with(NewIssue::titled(nb("Nobody's"))))
        .unwrap();
    let issue = lone
        .state()
        .unwrap()
        .issues
        .iter()
        .next()
        .unwrap()
        .0
        .clone();
    lone.apply(IssueList::ASSIGN.with(issue_list::Assign {
        issue,
        assignee: Some("agent:scout".into()),
    }))
    .unwrap();
    assert!(asks(&lone).is_empty());
}

/// Only the agent an issue is with counts its sessions, a closed issue takes no
/// wait and no stop, and a place is a folder's path and a note.
#[test]
fn sessions_are_the_assigned_agents_and_a_closed_issue_neither_waits_nor_stops() {
    use crate::model::StoppedWhy;
    let mut task = Task::given();
    let issue = task.issue.clone();
    let began = || {
        IssueList::SESSION_BEGAN.with(issue_list::SessionBegan {
            issue: issue.clone(),
        })
    };
    let refused = task.ada_does(1_100, began()).unwrap_err();
    assert!(
        refused.contains("Only the agent an issue is assigned to counts its sessions"),
        "{refused}"
    );
    let mut other = task.ada.fork().as_agent_for("scout", "ada");
    assert!(other.apply(began()).is_err(), "another agent is refused");

    task.agent_does(
        1_200,
        IssueList::SET_PLACE.with(issue_list::SetPlace {
            issue: issue.clone(),
            folder: " Tasks / 1 Write the market analysis ".into(),
            note: Some("notes.notebook.pages/pages/7f".into()),
        }),
    )
    .unwrap();
    let work = task.work();
    assert_eq!(work.folder, "Tasks/1 Write the market analysis");
    assert_eq!(work.note, "notes.notebook.pages/pages/7f");
    assert!(
        task.agent_does(
            1_250,
            IssueList::SET_PLACE.with(issue_list::SetPlace {
                issue: issue.clone(),
                folder: "../outside".into(),
                note: None,
            }),
        )
        .is_err(),
        "a place is a folder of the workspace"
    );
    let wait = |until_ms: i64| {
        IssueList::WAIT.with(issue_list::Wait {
            issue: issue.clone(),
            waits: Vec::new(),
            until_ms,
        })
    };
    let refused = task.agent_does(1_300, wait(1_300)).unwrap_err();
    assert!(refused.contains("A wait ends later than now"), "{refused}");

    task.ada_does(
        1_400,
        IssueList::PLACE.with(issue_list::Place {
            issue: issue.clone(),
            status: Status::Done,
            before: None,
            rank: None,
        }),
    )
    .unwrap();
    let refused = task.ada_does(1_500, wait(9_000)).unwrap_err();
    assert!(refused.contains("This issue is closed"), "{refused}");
    assert!(
        task.ada_does(
            1_600,
            IssueList::STOP.with(issue_list::Stop {
                issue,
                why: StoppedWhy::Person,
                words: nb("Not needed any more"),
            }),
        )
        .is_err()
    );
}

/// What `issues` says of the tracker, read through every question the views ask of it.
fn reading(issues: &Issues) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for (id, issue) in issues.iter() {
        let children: Vec<u64> = issues
            .children(Some(id))
            .map(|(_, child)| child.number)
            .collect();
        let _ = writeln!(
            out,
            "{} {} {:?} under {:?} {:?} {:?}",
            issue.number,
            issue.title,
            issue.status,
            issues
                .parent_of(issue)
                .map(|parent| issues.get(parent).map(|p| p.number)),
            children,
            issues.progress(id),
        );
    }
    let top: Vec<u64> = issues
        .children(None)
        .map(|(_, issue)| issue.number)
        .collect();
    let labels: Vec<(String, usize)> = issues
        .labels()
        .map(|(label, count)| (label.to_owned(), count))
        .collect();
    let todo = issues.count_kinds(|issue| !issue.status.started());
    let unassigned = issues.count_kinds(|issue| issue.assignee.is_none());
    let _ = writeln!(
        out,
        "top {top:?}\nlabels {labels:?}\ntodo {todo} unassigned {unassigned} of {}",
        issues.len()
    );
    out
}

#[test]
fn the_kept_issues_read_only_what_changed_and_read_as_the_parts_do() {
    let mut board = Board::new("ada");
    let mut kept = Issues::default();
    let follow = |board: &Board, kept: &mut Issues| -> Vec<u64> {
        let parts: Rc<[(u64, Rc<IssueList>)]> = board.states().into();
        let before = kept.revision();
        kept.sync(&parts);
        assert_eq!(
            reading(kept),
            reading(&Issues::of(&parts)),
            "kept from the changes, the issues read as the parts read afresh"
        );
        let mut touched = kept.touched_since(before).expect("one revision back");
        touched.sort_unstable();
        touched.dedup();
        touched
    };
    // Enough issues for three parts, every tenth a parent of the nine after it.
    let mut parent = None;
    let mut ids = Vec::new();
    for number in 1..=1_100_u64 {
        let first = number % 10 == 1;
        let id = board.create(
            &format!("Issue {number}"),
            if first { None } else { parent.clone() },
        );
        if first {
            parent = Some(id.clone());
        }
        ids.push(id);
    }
    follow(&board, &mut kept);

    // One change touches that issue, and its parent whose progress moved.
    board
        .apply(IssueList::PLACE.with(issue_list::Place {
            issue: ids[4].clone(),
            status: Status::Done,
            before: None,
            rank: None,
        }))
        .unwrap();
    assert_eq!(follow(&board, &mut kept), vec![1, 5]);

    board
        .apply(IssueList::ADD_LABEL.with(issue_list::AddLabel {
            issue: ids[600].clone(),
            label: nb("launch"),
        }))
        .unwrap();
    board
        .apply(IssueList::ASSIGN.with(issue_list::Assign {
            issue: ids[1_050].clone(),
            assignee: Some("agent:claude".to_owned()),
        }))
        .unwrap();
    follow(&board, &mut kept);

    // A parent moved under another, a parent deleted (its sub-issues stand at
    // the top until they are moved), an issue made in the newest part.
    board
        .apply(IssueList::MOVE_UNDER.with(issue_list::MoveUnder {
            issue: ids[20].clone(),
            parent: Some(ui::tilo_resource::items::Ref::from(&ids[1_090])),
        }))
        .unwrap();
    follow(&board, &mut kept);
    board
        .apply(IssueList::DELETE.with(issue_list::Delete {
            issue: ids[30].clone(),
        }))
        .unwrap();
    follow(&board, &mut kept);
    board.create("One more", Some(ids[0].clone()));
    let touched = follow(&board, &mut kept);
    assert_eq!(
        touched,
        vec![1, 1_101],
        "the new issue and its parent, and nothing else"
    );
}

/// An automation going on tells exactly the tasks that wait for it to go on: one given for its
/// own person, still waiting, that names it. It tells no other, and never a task given for
/// another person.
#[test]
fn an_automation_tells_only_the_task_that_waits_for_it_to_go_on() {
    let mut task = Task::given();
    let issue = task.issue.clone();
    task.agent_does(
        2_000,
        IssueList::WAIT.with(issue_list::Wait {
            issue: issue.clone(),
            waits: vec![reply_wait()],
            until_ms: 2_000 + 2 * DAY_MS,
        }),
    )
    .unwrap();
    let waiting = |task: &Task, person: &str, automation: &str| {
        crate::model::waiting_for(task.ada.state().unwrap().issues.iter(), person, automation)
    };
    assert_eq!(waiting(&task, "ada", "e-reply"), vec![issue.clone()]);
    assert!(
        waiting(&task, "grace", "e-reply").is_empty(),
        "another person's automation ends no wait of a task given for Ada"
    );
    assert!(
        waiting(&task, "ada", "e-other").is_empty(),
        "an automation it does not name"
    );
    assert!(waiting(&task, "ada", "").is_empty(), "no automation at all");

    // Told to go on, it waits no more: the automation coming again tells nothing.
    task.ada_does(
        3_000,
        IssueList::RESUME.with(issue_list::Resume {
            issue,
            cause: nb("the supplier replied"),
        }),
    )
    .unwrap();
    assert!(waiting(&task, "ada", "e-reply").is_empty());
}

/// Say how many of `issue`'s pieces are in work at once.
fn pieces_at_once(issue: &Id, at_once: Option<u64>) -> impl Into<Invoke<IssueList>> {
    IssueList::SET_PIECES_AT_ONCE.with(issue_list::SetPiecesAtOnce {
        issue: issue.clone(),
        at_once,
    })
}

/// A task with three pieces under it, each given to the agent, of which one is in work at a
/// time, and the task waiting for them: the task, and its pieces in the tracker's order.
fn in_three_pieces() -> (Task, Vec<Id>) {
    use crate::model::{WaitFor, WaitKind};
    let mut task = Task::given();
    let whole = task.issue.clone();
    task.begins(1_100);
    let mut pieces = Vec::new();
    for (at, title) in [(1_200, "North"), (1_201, "South"), (1_202, "West")] {
        task.ada_does(
            at,
            IssueList::CREATE.with(NewIssue {
                assignee: Some("agent:claude".into()),
                parent: Some(ui::tilo_resource::items::Ref::from(whole.clone())),
                ..NewIssue::titled(nb(title))
            }),
        )
        .unwrap();
        let held = task.ada.state().unwrap();
        let piece = held.issues.iter().find(|(_, issue)| issue.title == title);
        pieces.push(piece.unwrap().0.clone());
    }
    task.agent_does(1_300, pieces_at_once(&whole, Some(1)))
        .unwrap();
    task.agent_does(
        1_400,
        IssueList::WAIT.with(issue_list::Wait {
            issue: whole,
            waits: vec![WaitFor {
                kind: WaitKind::Pieces,
                what: "its three pieces".into(),
                on: None,
                event: None,
            }],
            until_ms: 1_400 + 2 * DAY_MS,
        }),
    )
    .unwrap();
    task.says(1_450, "I split it into three pieces and wait for them.");
    task.ends(1_500, 0, 0);
    (task, pieces)
}

/// How many of an issue's pieces are in work at once is said by a person or by the agent the
/// issue is given to: no other agent says it, and none at a time is no bound.
#[test]
fn only_a_person_or_its_own_agent_says_how_many_pieces_are_in_work_at_once() {
    let (mut task, _) = in_three_pieces();
    let whole = task.issue.clone();
    assert!(
        task.ada_does(1_600, pieces_at_once(&whole, Some(0)))
            .is_err(),
        "none at a time"
    );
    let refused = task
        .ada
        .fork()
        .as_agent_for("other", "ada")
        .apply(pieces_at_once(&whole, Some(9)))
        .map(drop)
        .map_err(|error| error.to_string())
        .unwrap_err();
    assert!(refused.contains("its own agent"), "{refused}");
    task.ada_does(1_700, pieces_at_once(&whole, Some(2)))
        .unwrap();
    assert_eq!(task.work().pieces_at_once, Some(2));
    task.ada_does(1_800, pieces_at_once(&whole, None)).unwrap();
    assert_eq!(task.work().pieces_at_once, None);
}

/// A task split into pieces: of the pieces under an issue that says how many are in work at
/// once, only so many are begun, in the tracker's order, and the next is begun when one is
/// through. The issue itself, waiting for its pieces, is asked again once all of them are.
#[test]
fn pieces_are_begun_so_many_at_once_and_their_issue_goes_on_when_they_are_through() {
    use crate::model::{Asked, StoppedWhy};
    let (mut task, pieces) = in_three_pieces();
    let whole = task.issue.clone();
    let asked = |task: &Task, now_ms: i64| -> Vec<(Id, Asked)> {
        task.asks(now_ms)
            .into_iter()
            .map(|ask| (ask.issue, ask.asked))
            .collect()
    };
    let began = |issue: &Id| {
        IssueList::SESSION_BEGAN.with(issue_list::SessionBegan {
            issue: issue.clone(),
        })
    };
    let placed = |issue: &Id, status| {
        IssueList::PLACE.with(issue_list::Place {
            issue: issue.clone(),
            status,
            before: None,
            rank: None,
        })
    };

    // One at a time: only the first piece is begun, and nothing else while it is in work.
    assert_eq!(
        asked(&task, 2_000),
        vec![(pieces[0].clone(), Asked::Assigned)]
    );
    task.agent_does(2_100, began(&pieces[0])).unwrap();
    let said = IssueList::COMMENT.with(issue_list::Comment {
        issue: pieces[0].clone(),
        text: nb("I am on it."),
        origin: None,
    });
    task.agent_does(2_150, said).unwrap();
    assert!(asked(&task, 2_200).is_empty(), "one piece is in work");

    // It is ready for review: the second is begun. That one stops: the third is begun.
    task.agent_does(2_300, placed(&pieces[0], Status::InReview))
        .unwrap();
    assert_eq!(
        asked(&task, 2_400),
        vec![(pieces[1].clone(), Asked::Assigned)]
    );
    task.agent_does(2_500, began(&pieces[1])).unwrap();
    let stopped = IssueList::STOP.with(issue_list::Stop {
        issue: pieces[1].clone(),
        why: StoppedWhy::Failed,
        words: nb("I could not reach the site."),
    });
    task.agent_does(2_600, stopped).unwrap();
    assert_eq!(
        asked(&task, 2_700),
        vec![(pieces[2].clone(), Asked::Assigned)]
    );

    // Its person calls the third off: every piece is through, and the issue is asked again
    // with how they ended. A stopped piece is not counted as done.
    task.ada_does(2_800, placed(&pieces[2], Status::Cancelled))
        .unwrap();
    let asks = task.asks(2_900);
    assert_eq!(asks.len(), 1, "{asks:?}");
    assert_eq!(
        (&asks[0].issue, asks[0].asked),
        (&whole, Asked::PiecesEnded)
    );
    assert_eq!(
        asks[0].words,
        "3 pieces: 1 ready for review, 0 done, 1 stopped, 1 cancelled"
    );
}

/// How many `#[model(demanded)]` fields (an issue's description and thread) `scopes` read.
fn demanded_reads(scopes: &[ui::tilo_resource::profiling::Sample]) -> usize {
    scopes
        .iter()
        .filter(|scope| scope.phase == "resource.read_demanded")
        .map(|scope| scope.calls)
        .sum()
}

/// A part of `count` issues, each with a description and a comment, and its issues' ids in the
/// order they were made.
fn commented_part(count: usize) -> (Replica<IssueList>, Vec<Id>) {
    let mut part = Replica::<IssueList>::new().as_author("ada");
    for at in 0..count {
        part.apply(IssueList::CREATE.with(NewIssue {
            body: Some(format!("Issue {at}: what it is about, a paragraph of it.")),
            rank: Some(format!("{at:08}")),
            ..NewIssue::titled(nb(&format!("Issue {at}")))
        }))
        .unwrap();
    }
    let state = part.state().unwrap();
    let mut ids: Vec<(String, Id)> = state
        .issues
        .iter()
        .map(|(id, issue)| (issue.rank.clone(), id.clone()))
        .collect();
    ids.sort();
    let ids: Vec<Id> = ids.into_iter().map(|(_, id)| id).collect();
    for id in &ids {
        part.apply(IssueList::COMMENT.with(issue_list::Comment {
            issue: id.clone(),
            text: nb("Looked at it"),
            origin: None,
        }))
        .unwrap();
    }
    (part, ids)
}

/// What a window's reader reads of a part: none of its issues' descriptions or threads, at 20
/// issues as at 400, and the one issue a view demands whole, however many there are.
#[test]
fn a_windows_reading_of_its_issues_reads_no_description_or_thread_until_one_is_demanded() {
    fn read(count: usize) -> (usize, usize) {
        let (part, ids) = commented_part(count);
        let (reader, scopes) = ui::tilo_resource::profiling::observe(|| {
            ui::tilo_resource::Reader::<IssueList>::read(part.doc()).unwrap()
        });
        let opened = demanded_reads(&scopes);
        let state = Rc::clone(reader.state());
        assert_eq!(state.issues.len(), count);
        assert!(
            state
                .issues
                .iter()
                .all(|(_, issue)| issue.body.text().is_empty() && issue.comments.is_empty()),
            "a list's reading holds no description or thread"
        );
        drop(state);
        let mut reader = reader;
        let shown = &ids[count / 2];
        let ((), scopes) = ui::tilo_resource::profiling::observe(|| {
            assert!(
                reader
                    .demand(
                        part.doc(),
                        ui::tilo_resource::Demand::none().with_keys([shown.as_str()]),
                    )
                    .unwrap()
            );
        });
        let issue = reader.state().issues.get(shown).unwrap();
        assert!(
            issue.body.text().contains("what it is about"),
            "{:?}",
            issue.body.text()
        );
        assert_eq!(issue.comments.len(), 1);
        (opened, demanded_reads(&scopes))
    }
    // The description and the thread of one issue: two fields read, at any size.
    assert_eq!(read(20), (0, 2));
    assert_eq!(read(400), (0, 2));
}

/// The issue a view demanded stays read as it changes, and a change to another issue reads what
/// it touched of that one alone; the reading of every field still sees them all.
#[test]
fn a_demanded_issue_follows_its_changes_and_the_others_stay_unread() {
    let (mut part, ids) = commented_part(30);
    let mut reader = ui::tilo_resource::Reader::<IssueList>::read(part.doc()).unwrap();
    let shown = ids[3].clone();
    reader
        .demand(
            part.doc(),
            ui::tilo_resource::Demand::none().with_keys([shown.as_str()]),
        )
        .unwrap();
    for issue in [&shown, &ids[7]] {
        part.apply(IssueList::COMMENT.with(issue_list::Comment {
            issue: issue.clone(),
            text: nb("And again"),
            origin: None,
        }))
        .unwrap();
    }
    let ((), scopes) = ui::tilo_resource::profiling::observe(|| {
        reader.sync(part.doc()).unwrap();
    });
    assert!(demanded_reads(&scopes) <= 2, "{scopes:?}");
    let state = reader.state();
    assert_eq!(state.issues.get(&shown).unwrap().comments.len(), 2);
    assert!(
        state
            .issues
            .get(&ids[3])
            .unwrap()
            .body
            .text()
            .contains("Issue 3"),
        "the demanded issue keeps its description"
    );
    assert!(state.issues.get(&ids[10]).unwrap().comments.is_empty());
    let every = ui::tilo_resource::Reader::<IssueList>::read_all(part.doc()).unwrap();
    assert!(
        every
            .state()
            .issues
            .iter()
            .all(|(_, issue)| !issue.comments.is_empty() && !issue.body.text().is_empty())
    );
}
