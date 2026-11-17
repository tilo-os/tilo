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
            parent,
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
                parent: Some(child.clone()),
            }))
            .is_err(),
        "a parent cannot move under its own sub-issue"
    );
    assert!(
        board
            .apply(IssuePart::CREATE.with(NewIssue {
                parent: Some(Id::from_raw("gone")),
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
                parent: Some(first.clone()),
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
                parent: Some(last),
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
            before: Some(one),
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
            project: Some(launch.clone()),
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
                project: Some(Id::from_raw("nope")),
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
        "the record a change is written to is no larger: {record_small} then {record_large} bytes"
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
