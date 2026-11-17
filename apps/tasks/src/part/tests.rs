#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion"
)]

use std::rc::Rc;

use ui::tilo_resource::{Id, Invoke, NonBlank, Replica, Resource as _};

use crate::model::{Issue, NewIssue, Status, Tracker, tracker};
use crate::part::{IssuePart, Issues, PART_ISSUES, issue_part};

/// A tracker and its parts as one device holds them: every action goes to
/// the part the chain sends it to, read against the tracker and the parts
/// held, as a window carries it out.
struct Board {
    tracker: Replica<Tracker>,
    parts: Vec<(u64, Replica<IssuePart>)>,
    /// Each part's state as last written, as a window follows them.
    states: Vec<(u64, Rc<IssuePart>)>,
    author: String,
    now_ms: i64,
}

impl Board {
    fn new(author: &str) -> Self {
        let mut tracker = Replica::<Tracker>::new().as_author(author);
        tracker.set_now_ms(1_000);
        Self {
            tracker,
            parts: Vec::new(),
            states: Vec::new(),
            author: author.to_owned(),
            now_ms: 1_000,
        }
    }

    fn states(&self) -> Vec<(u64, Rc<IssuePart>)> {
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

    fn part(&mut self, index: u64) -> &mut Replica<IssuePart> {
        if !self.parts.iter().any(|(held, _)| *held == index) {
            let carried = self
                .parts
                .last()
                .map(|(_, part)| crate::part::carried(&part.state().unwrap()));
            let mut part = Replica::<IssuePart>::new().as_author(&self.author);
            part.set_now_ms(self.now_ms);
            if let Some(before) = carried {
                part.apply(IssuePart::START.with(issue_part::Start { before }))
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

    fn apply(&mut self, invoke: impl Into<Invoke<IssuePart>>) -> Result<u64, String> {
        let mut action = invoke.into().build(self.now_ms)?;
        let parts = self.states();
        crate::part::prepare(&mut action, &self.tracker.state().unwrap(), &parts)?;
        let index = crate::part::part_for(&action, &parts)?;
        self.part(index)
            .apply_action(&action)
            .map_err(|error| error.to_string())?;
        self.follow(index);
        Ok(index)
    }

    fn create(&mut self, title: &str, parent: Option<Id>) -> Id {
        self.apply(IssuePart::CREATE.with(NewIssue {
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
            .apply(IssuePart::MOVE_UNDER.with(issue_part::MoveUnder {
                issue: parent.clone(),
                parent: Some(ui::tilo_resource::items::Ref::from(child.clone())),
            }))
            .is_err(),
        "a parent cannot move under its own sub-issue"
    );
    assert!(
        board
            .apply(IssuePart::CREATE.with(NewIssue {
                parent: Some(ui::tilo_resource::items::Ref::from("gone")),
                ..NewIssue::titled(nb("Orphan"))
            }))
            .is_err(),
        "a parent that is not an issue is refused"
    );
    // Deleting the parent leaves its sub-issue at the top level.
    board
        .apply(IssuePart::DELETE.with(issue_part::Delete { issue: parent }))
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
            .apply(IssuePart::CREATE.with(NewIssue::titled(nb(&format!("Issue {index}")))))
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
            .apply(IssuePart::MOVE_UNDER.with(issue_part::MoveUnder {
                issue: last.clone(),
                parent: Some(ui::tilo_resource::items::Ref::from(first.clone())),
            }))
            .unwrap(),
        1
    );
    assert_eq!(
        board
            .apply(IssuePart::RENAME.with(issue_part::Rename {
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
            .apply(IssuePart::MOVE_UNDER.with(issue_part::MoveUnder {
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
        .apply(IssuePart::PLACE.with(issue_part::Place {
            issue: three.clone(),
            status: Status::Todo,
            before: Some(ui::tilo_resource::items::Ref::from(one)),
            rank: None,
        }))
        .unwrap();
    assert_eq!(board.titles(), ["Three", "One", "Two"]);
    assert_eq!(board.issue(&three).unwrap().status, Status::Todo);
    board
        .apply(IssuePart::PLACE.with(issue_part::Place {
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
        .apply(IssuePart::CREATE.with(NewIssue {
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
            .apply(IssuePart::SET_PROJECT.with(issue_part::SetProject {
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
    assert_eq!(IssuePart::KIND, crate::model::ISSUES_KIND);
}

/// What one more change costs a tracker does not grow with the issues it
/// already has: the part it writes holds at most a part's worth, so writing
/// into a tracker ten times the size costs the same time and bytes.
#[test]
#[ignore = "writes ten thousand issues, minutes in a debug build; run it with --ignored in a release build"]
fn a_change_costs_the_same_in_a_tracker_ten_times_the_size() {
    /// Fill the tracker up to `count` issues, a part at a time: written
    /// straight into the parts, as a sync brings them.
    fn fill(board: &mut Board, count: usize) {
        let mut made: usize = board
            .parts
            .iter()
            .map(|(_, part)| part.state().unwrap().issues.len())
            .sum();
        while made < count {
            let index = u64::try_from(made / PART_ISSUES).unwrap();
            let part = board.part(index);
            let end = count.min((made / PART_ISSUES + 1) * PART_ISSUES);
            for at in made..end {
                part.apply(IssuePart::CREATE.with(NewIssue {
                    body: Some(format!("Issue {at}: what it is about, a line or two.")),
                    rank: Some(format!("{at:08}")),
                    ..NewIssue::titled(nb(&format!("Issue {at}")))
                }))
                .unwrap();
            }
            made = end;
        }
        board.reread();
    }
    /// The bytes one comment on the first issue writes, the size of the
    /// record it is written to, saved, and the time the part took it.
    fn comment(board: &mut Board) -> (usize, usize, std::time::Duration) {
        let first = board.id_of("Issue 0");
        let began = std::time::Instant::now();
        let index = board
            .apply(IssuePart::COMMENT.with(issue_part::Comment {
                issue: first,
                text: nb("Looked at it"),
                origin: None,
            }))
            .unwrap();
        let took = began.elapsed();
        let part = board.part(index);
        let change = part
            .doc_mut()
            .get_last_local_change()
            .map(|change| change.raw_bytes().len())
            .unwrap();
        (change, part.save().len(), took)
    }
    let small = PART_ISSUES * 2;
    let mut board = Board::new("ada");
    fill(&mut board, small);
    let (change_small, record_small, took_small) = comment(&mut board);
    fill(&mut board, small * 10);
    let (change_large, record_large, took_large) = comment(&mut board);
    eprintln!(
        "a comment at {small} issues: {change_small} bytes into a {record_small}-byte part \
         in {took_small:?}; at {} issues: {change_large} bytes into a {record_large}-byte part \
         in {took_large:?}",
        small * 10
    );
    assert_eq!(board.parts.len(), 20);
    assert!(
        change_large <= change_small + 16,
        "a change is the same size: {change_small} then {change_large} bytes"
    );
    assert!(
        record_large <= record_small + record_small / 10,
        "the resource a change is written to is no larger: {record_small} then {record_large} bytes"
    );
}

/// Giving an issue to an agent asks it, for whoever gave it; the agent's own
/// message in the thread answers that ask, and a person writing in the thread
/// afterwards asks it again.
#[test]
fn giving_an_issue_to_an_agent_asks_it_until_it_answers() {
    use ui::tilo_resource::MemberId;
    let asks =
        |part: &Replica<IssuePart>| crate::model::agent_asks(part.state().unwrap().issues.iter());
    let mut ada = Replica::<IssuePart>::new().as_author("ada");
    ada.set_now_ms(1_000);
    ada.apply(IssuePart::CREATE.with(NewIssue::titled(nb("Plan the launch"))))
        .unwrap();
    let issue = ada.state().unwrap().issues.iter().next().unwrap().0.clone();
    assert!(asks(&ada).is_empty(), "nobody asked yet");

    ada.apply(IssuePart::ASSIGN.with(issue_part::Assign {
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
        .apply(IssuePart::COMMENT.with(issue_part::Comment {
            issue: issue.clone(),
            text: nb("The plan is in the launch note"),
            origin: None,
        }))
        .unwrap();
    ada.sync_with(&mut agent).unwrap();
    assert!(asks(&ada).is_empty(), "its message answers the ask");

    ada.set_now_ms(3_000);
    ada.apply(IssuePart::COMMENT.with(issue_part::Comment {
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
    ada.apply(IssuePart::ASSIGN.with(issue_part::Assign {
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
        .apply(IssuePart::CREATE.with(NewIssue {
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
        .apply(IssuePart::CREATE.with(NewIssue {
            project: Some(Ref::from("Nowhere")),
            ..NewIssue::titled(nb("Lost"))
        }))
        .expect_err("no project is named Nowhere");
    assert!(why.contains("no project or area named Nowhere"), "{why}");
    assert!(why.contains("Launch"), "what there is follows: {why}");
    // An issue's parent is named the same way, by the title it is read under.
    board
        .apply(IssuePart::CREATE.with(NewIssue {
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
        .apply(IssuePart::CREATE.with(NewIssue {
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
        .apply(IssuePart::RENAME.with(issue_part::Rename {
            issue: Id::from_raw("Write the post"),
            title: nb("Write it"),
        }))
        .unwrap();
    // By the number it reads as.
    board
        .apply(IssuePart::RENAME.with(issue_part::Rename {
            issue: Id::from_raw("#1"),
            title: nb("Write the post"),
        }))
        .unwrap();
    // And by its id, which no rename changes.
    board
        .apply(IssuePart::RENAME.with(issue_part::Rename {
            issue: id,
            title: nb("Ship it"),
        }))
        .unwrap();
    assert_eq!(board.titles(), ["Ship it"]);
    // A reference to no issue says which there are.
    let why = board
        .apply(IssuePart::RENAME.with(issue_part::Rename {
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
    ada: Replica<IssuePart>,
    agent: Replica<IssuePart>,
    issue: Id,
}

impl Task {
    /// Ada files "Write the market analysis" and gives it to the agent
    /// `claude` at 1 000 ms; the agent holds the same record.
    fn given() -> Self {
        let mut ada = Replica::<IssuePart>::new().as_author("ada");
        ada.set_now_ms(1_000);
        ada.apply(IssuePart::CREATE.with(NewIssue {
            assignee: Some("agent:claude".into()),
            ..NewIssue::titled(nb("Write the market analysis"))
        }))
        .unwrap();
        let issue = ada.state().unwrap().issues.iter().next().unwrap().0.clone();
        let agent = ada.fork().as_agent_for("claude", "ada");
        Self { ada, agent, issue }
    }

    /// The agent does `invoke` at `now_ms`, and Ada's record takes it.
    fn agent_does(
        &mut self,
        now_ms: i64,
        invoke: impl Into<Invoke<IssuePart>>,
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
        invoke: impl Into<Invoke<IssuePart>>,
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
            IssuePart::COMMENT.with(issue_part::Comment {
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
            IssuePart::RUN_BEGAN.with(issue_part::RunBegan { issue }),
        )
        .unwrap();
    }

    fn ends(&mut self, now_ms: i64, cost_micros: u64, worked_ms: u64) {
        let issue = self.issue.clone();
        self.agent_does(
            now_ms,
            IssuePart::RUN_ENDED.with(issue_part::RunEnded {
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

    /// What asks the agent at `now_ms`, read from the record alone, as a host
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
/// days are over; a reader that holds only the record reads the same asks.
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
            IssuePart::PLACE.with(issue_part::Place {
                issue: issue.clone(),
                status: Status::InProgress,
                before: None,
                rank: None,
            }),
        )
        .unwrap();
        task.agent_does(
            2_200,
            IssuePart::WAIT.with(issue_part::Wait {
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
        assert!(task.asks(now).is_empty(), "nothing runs while it waits");
    }
    let issue = task.issue.clone();
    task.agent_does(
        2_000 + DAY_MS,
        IssuePart::RESUME.with(issue_part::Resume {
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
        (work.runs, work.spent_micros, work.worked_ms),
        (2, 1_200, 700),
        "both runs are counted"
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
        "the run ended the wait"
    );
    assert_eq!(task.work().until_ms, None);
    task.says(until + 6, "No reply came: I wrote again.");
    assert!(task.asks(until + DAY_MS).is_empty(), "answered once");
}

/// A run that was lost is asked again from the record alone, and one lost
/// while the task waited does not start it early. After the most lost runs
/// in a row the task stops and says so: it never reads as done.
#[test]
fn a_task_whose_worker_was_lost_is_run_again_then_stops_and_never_reads_as_done() {
    use crate::model::{Asked, MOST_LOST_RUNS, StoppedWhy, TaskState};
    // Lost after it began to wait: the wait still holds.
    let mut task = Task::given();
    task.begins(2_000);
    let issue = task.issue.clone();
    task.agent_does(
        2_100,
        IssuePart::WAIT.with(issue_part::Wait {
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

    // Lost while it worked: asked again, each new run counting the one before.
    let mut task = Task::given();
    let issue = task.issue.clone();
    for run in 0..=MOST_LOST_RUNS {
        let asked = task.asks(2_000);
        assert_eq!(asked.len(), 1, "run {run} is asked again: {asked:?}");
        task.begins(2_000 + i64::try_from(run).unwrap());
        assert_eq!(task.work().lost, run, "the runs lost in a row");
    }
    assert!(task.work().lost >= MOST_LOST_RUNS);
    task.agent_does(
        3_000,
        IssuePart::STOP.with(issue_part::Stop {
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
        IssuePart::PLACE.with(issue_part::Place {
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
        IssuePart::PLACE.with(issue_part::Place {
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
        IssuePart::COMMENT.with(issue_part::Comment {
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
        IssuePart::SET_BUDGET.with(issue_part::SetBudget {
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
        IssuePart::STOP.with(issue_part::Stop {
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
    assert!(task.asks(3_000).is_empty(), "nothing runs past the budget");

    let resume = || {
        IssuePart::RESUME.with(issue_part::Resume {
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
        |part: &Replica<IssuePart>| crate::model::agent_asks(part.state().unwrap().issues.iter());
    let mut ada = Replica::<IssuePart>::new().as_author("ada");
    let mut agent = ada.fork().as_agent_for("claude", "ada");
    agent.set_now_ms(1_000);
    agent
        .apply(IssuePart::CREATE.with(NewIssue {
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
    ada.apply(IssuePart::ASSIGN.with(issue_part::Assign {
        issue: proposal.clone(),
        assignee: Some("agent:claude".into()),
    }))
    .unwrap();
    let asked = asks(&ada);
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].person, MemberId::person("ada"));
    // Giving it again asks nothing new.
    ada.set_now_ms(3_000);
    ada.apply(IssuePart::ASSIGN.with(issue_part::Assign {
        issue: proposal,
        assignee: Some("agent:claude".into()),
    }))
    .unwrap();
    assert_eq!(asks(&ada)[0].cause, "assigned-2000");

    // The agent hands a piece on: the other agent works on it for the same person.
    let mut agent = Replica::<IssuePart>::new().as_agent_for("claude", "ada");
    agent.set_now_ms(1_000);
    agent
        .apply(IssuePart::CREATE.with(NewIssue::titled(nb("Read the price pages"))))
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
        .apply(IssuePart::ASSIGN.with(issue_part::Assign {
            issue: piece,
            assignee: Some("agent:scout".into()),
        }))
        .unwrap();
    let asked = asks(&agent);
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].agent, "scout");
    assert_eq!(asked[0].person, MemberId::person("ada"));

    // An agent that acts for nobody gives nobody's work to anyone.
    let mut lone = Replica::<IssuePart>::new().as_member(MemberId::agent("lone"));
    lone.set_now_ms(1_000);
    lone.apply(IssuePart::CREATE.with(NewIssue::titled(nb("Nobody's"))))
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
    lone.apply(IssuePart::ASSIGN.with(issue_part::Assign {
        issue,
        assignee: Some("agent:scout".into()),
    }))
    .unwrap();
    assert!(asks(&lone).is_empty());
}

/// Only the agent an issue is with counts its runs, a closed issue takes no
/// wait and no stop, and a place is a folder's path and a note.
#[test]
fn runs_are_the_assigned_agents_and_a_closed_issue_neither_waits_nor_stops() {
    use crate::model::StoppedWhy;
    let mut task = Task::given();
    let issue = task.issue.clone();
    let began = || {
        IssuePart::RUN_BEGAN.with(issue_part::RunBegan {
            issue: issue.clone(),
        })
    };
    let refused = task.ada_does(1_100, began()).unwrap_err();
    assert!(
        refused.contains("Only the agent an issue is assigned to counts its runs"),
        "{refused}"
    );
    let mut other = task.ada.fork().as_agent_for("scout", "ada");
    assert!(other.apply(began()).is_err(), "another agent is refused");

    task.agent_does(
        1_200,
        IssuePart::SET_PLACE.with(issue_part::SetPlace {
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
            IssuePart::SET_PLACE.with(issue_part::SetPlace {
                issue: issue.clone(),
                folder: "../outside".into(),
                note: None,
            }),
        )
        .is_err(),
        "a place is a folder of the workspace"
    );
    let wait = |until_ms: i64| {
        IssuePart::WAIT.with(issue_part::Wait {
            issue: issue.clone(),
            waits: Vec::new(),
            until_ms,
        })
    };
    let refused = task.agent_does(1_300, wait(1_300)).unwrap_err();
    assert!(refused.contains("A wait ends later than now"), "{refused}");

    task.ada_does(
        1_400,
        IssuePart::PLACE.with(issue_part::Place {
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
            IssuePart::STOP.with(issue_part::Stop {
                issue,
                why: StoppedWhy::Person,
                words: nb("Not needed any more"),
            }),
        )
        .is_err()
    );
}
