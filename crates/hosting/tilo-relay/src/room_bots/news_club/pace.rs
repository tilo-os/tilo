//! When the club speaks, and as whom. Nothing here reads the room or asks a
//! model: it is told what the threads hold and when it is, and answers
//! what to do next, so its rhythm is tested without either.
//!
//! The rhythm is a person's, not a feed's: a new story every hour or two,
//! a reply some minutes after the line it answers, often none at all, and a
//! thread that quietens as it grows. A person who writes in a thread is
//! answered more often and sooner than the club answers itself.

use std::collections::{BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use super::hn::Story;
use super::personas::{self, PERSONAS};

/// How the club paces itself, in seconds and chances.
#[derive(Clone, Debug, PartialEq)]
pub struct Pace {
    /// Between two new stories.
    pub story_gap: (i64, i64),
    /// Before the first answer to an opening post.
    pub first_reply: (i64, i64),
    /// Before the club answers itself.
    pub club_reply: (i64, i64),
    /// Before the club answers a person.
    pub person_reply: (i64, i64),
    /// That an opening post is answered at all.
    pub first_reply_chance: f64,
    /// That the club answers itself, in a thread it has not posted in yet;
    /// it falls as the club posts more there.
    pub club_reply_chance: f64,
    /// That a person's line is answered.
    pub person_reply_chance: f64,
    /// The most the club posts in one thread, opening included.
    pub thread_posts: u32,
    /// How long a thread is followed after it was opened.
    pub thread_life: i64,
    /// The most threads followed at once.
    pub live_threads: usize,
    /// The most the club posts in any hour.
    pub hourly_posts: usize,
}

impl Default for Pace {
    fn default() -> Self {
        Self {
            story_gap: (70 * 60, 160 * 60),
            first_reply: (4 * 60, 18 * 60),
            club_reply: (3 * 60, 25 * 60),
            person_reply: (45, 6 * 60),
            first_reply_chance: 0.9,
            club_reply_chance: 0.6,
            person_reply_chance: 0.9,
            thread_posts: 9,
            thread_life: 10 * 60 * 60,
            live_threads: 3,
            hourly_posts: 12,
        }
    }
}

/// Where the club's randomness comes from.
pub trait Dice {
    /// A number in `low..=high`.
    fn between(&mut self, low: i64, high: i64) -> i64;
    /// Whether something that happens with `chance` happens.
    fn chance(&mut self, chance: f64) -> bool;
    /// An index below `len` (`len` > 0).
    fn pick(&mut self, len: usize) -> usize;
}

/// The process's own randomness.
pub struct Random;

impl Dice for Random {
    fn between(&mut self, low: i64, high: i64) -> i64 {
        if high <= low {
            return low;
        }
        let span = u64::try_from(high - low).unwrap_or(0).saturating_add(1);
        low + i64::try_from(rand::random::<u64>() % span).unwrap_or(0)
    }

    fn chance(&mut self, chance: f64) -> bool {
        rand::random::<f64>() < chance
    }

    fn pick(&mut self, len: usize) -> usize {
        usize::try_from(rand::random::<u64>() % u64::try_from(len.max(1)).unwrap_or(1)).unwrap_or(0)
    }
}

/// A line of a thread as the pace sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    /// The message's id.
    pub id: String,
    /// The persona that wrote it, when the club did.
    pub persona: Option<String>,
    /// What it says.
    pub text: String,
}

/// A thread the club opened and follows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    /// The opening message's id: every reply names it as its parent.
    pub root: String,
    /// The story it is about.
    pub story: Story,
    /// Who opened it.
    pub opener: String,
    /// When, seconds since the epoch.
    pub opened_at: i64,
    /// How many posts the club made in it, opening included.
    pub posts: u32,
    /// The replies already taken in.
    pub seen: BTreeSet<String>,
    /// The personas that posted in it, most recent last.
    pub voices: Vec<String>,
}

/// A reply the club means to post.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Planned {
    /// The thread, by its root.
    pub root: String,
    /// Who answers.
    pub persona: String,
    /// When it is due, seconds since the epoch.
    pub due: i64,
    /// The message it answers, if not the thread as a whole.
    pub answering: Option<String>,
}

/// Everything the club keeps between wakes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Club {
    /// Stories already opened or passed over, newest last.
    pub taken: VecDeque<u64>,
    /// The threads it follows.
    pub threads: Vec<Thread>,
    /// The replies it means to post.
    pub planned: Vec<Planned>,
    /// When the next story may be opened; 0 for at once.
    pub next_story: i64,
    /// When the club posted, newest last, for the hourly cap.
    pub posted: VecDeque<i64>,
}

/// What the club does now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next {
    /// Answer in a thread.
    Reply(Planned),
    /// Open a thread on the next story, as this persona.
    Open {
        /// Who opens it.
        persona: String,
    },
    /// Nothing yet.
    Wait,
}

/// How many taken story ids are kept.
const TAKEN_KEPT: usize = 500;

impl Club {
    /// Whether story `id` was opened or passed over.
    #[must_use]
    pub fn took(&self, id: u64) -> bool {
        self.taken.contains(&id)
    }

    /// Mark story `id` opened or passed over.
    pub fn take(&mut self, id: u64) {
        if !self.took(id) {
            self.taken.push_back(id);
        }
        while self.taken.len() > TAKEN_KEPT {
            self.taken.pop_front();
        }
    }

    fn posted_last_hour(&self, now: i64) -> usize {
        self.posted.iter().filter(|at| now - **at < 3600).count()
    }

    /// Take in the lines of thread `root` (oldest first), planning an
    /// answer to what is new.
    pub fn take_in(
        &mut self,
        root: &str,
        lines: &[Seen],
        now: i64,
        pace: &Pace,
        dice: &mut dyn Dice,
    ) {
        let Some(at) = self.threads.iter().position(|thread| thread.root == root) else {
            return;
        };
        for line in lines {
            let thread = &mut self.threads[at];
            if !thread.seen.insert(line.id.clone()) {
                continue;
            }
            if let Some(persona) = &line.persona {
                if thread.voices.last() != Some(persona) {
                    thread.voices.push(persona.clone());
                }
            }
            let thread = self.threads[at].clone();
            if thread.posts >= pace.thread_posts {
                continue;
            }
            let by_person = line.persona.is_none();
            let pending = self.planned.iter().position(|plan| plan.root == root);
            match (pending, by_person) {
                // One answer is planned per thread; a person's line takes
                // its place, sooner.
                (Some(index), true) => {
                    if dice.chance(pace.person_reply_chance) {
                        self.planned[index] = plan_reply(&thread, Some(line), now, pace, dice);
                    }
                }
                (Some(_), false) => {}
                (None, true) => {
                    if dice.chance(pace.person_reply_chance) {
                        self.planned
                            .push(plan_reply(&thread, Some(line), now, pace, dice));
                    }
                }
                (None, false) => {
                    // The club answers itself less as it says more.
                    let said = f64::from(thread.posts.saturating_sub(1));
                    let room = 1.0 - said / f64::from(pace.thread_posts.max(1));
                    if dice.chance(pace.club_reply_chance * room) {
                        self.planned
                            .push(plan_reply(&thread, Some(line), now, pace, dice));
                    }
                }
            }
        }
    }

    /// Stop following threads past their life, with what was planned in
    /// them.
    pub fn retire(&mut self, now: i64, pace: &Pace) {
        self.threads
            .retain(|thread| now - thread.opened_at < pace.thread_life);
        let live: BTreeSet<String> = self.threads.iter().map(|t| t.root.clone()).collect();
        self.planned.retain(|plan| live.contains(&plan.root));
    }

    /// What to do now: the earliest reply that is due, else a new story
    /// when one is due and there is room for it, else nothing. At most one
    /// post per wake, so even a backlog comes out spaced.
    pub fn next(&mut self, now: i64, pace: &Pace, dice: &mut dyn Dice) -> Next {
        if self.posted_last_hour(now) >= pace.hourly_posts {
            return Next::Wait;
        }
        let due = self
            .planned
            .iter()
            .enumerate()
            .filter(|(_, plan)| plan.due <= now)
            .min_by_key(|(_, plan)| plan.due)
            .map(|(index, _)| index);
        if let Some(index) = due {
            return Next::Reply(self.planned.remove(index));
        }
        if now >= self.next_story && self.threads.len() < pace.live_threads {
            let recent: Vec<&str> = self.threads.iter().map(|t| t.opener.as_str()).collect();
            let persona = pick_persona(&recent, None, dice);
            return Next::Open { persona };
        }
        Next::Wait
    }

    /// The next story is due a while from `now`.
    pub fn story_later(&mut self, now: i64, pace: &Pace, dice: &mut dyn Dice) {
        self.next_story = now + dice.between(pace.story_gap.0, pace.story_gap.1);
    }

    /// The club opened thread `root` on `story` as `persona`.
    pub fn opened(
        &mut self,
        (root, story, persona): (String, Story, String),
        now: i64,
        pace: &Pace,
        dice: &mut dyn Dice,
    ) {
        self.take(story.id);
        self.posted.push_back(now);
        let thread = Thread {
            root: root.clone(),
            story,
            opener: persona.clone(),
            opened_at: now,
            posts: 1,
            seen: BTreeSet::new(),
            voices: vec![persona],
        };
        if dice.chance(pace.first_reply_chance) {
            let mut plan = plan_reply(&thread, None, now, pace, dice);
            plan.due = now + dice.between(pace.first_reply.0, pace.first_reply.1);
            self.planned.push(plan);
        }
        self.threads.push(thread);
        self.story_later(now, pace, dice);
    }

    /// The club posted in thread `root` as `persona`. The post itself is
    /// taken in with the thread's other lines, so another persona may
    /// answer it.
    pub fn replied(&mut self, root: &str, persona: &str, now: i64) {
        self.posted.push_back(now);
        while self.posted.front().is_some_and(|at| now - *at >= 3600) {
            self.posted.pop_front();
        }
        if let Some(thread) = self.threads.iter_mut().find(|thread| thread.root == root) {
            thread.posts += 1;
            if thread.voices.last().map(String::as_str) != Some(persona) {
                thread.voices.push(persona.to_owned());
            }
        }
    }
}

/// Who answers `line` (or the opening post) in `thread`, and when.
fn plan_reply(
    thread: &Thread,
    line: Option<&Seen>,
    now: i64,
    pace: &Pace,
    dice: &mut dyn Dice,
) -> Planned {
    let by_person = line.is_some_and(|line| line.persona.is_none());
    // A person who names a persona is answered by it.
    let named = line
        .filter(|_| by_person)
        .and_then(|line| personas::addressed_in(&line.text))
        .map(|persona| persona.name.to_owned());
    let recent: Vec<&str> = thread
        .voices
        .iter()
        .rev()
        .take(2)
        .map(String::as_str)
        .collect();
    let persona = named.unwrap_or_else(|| {
        // Someone already in the thread answers half the time: threads are
        // conversations, not a queue of strangers.
        let author = line.and_then(|line| line.persona.as_deref());
        let returning: Vec<&str> = thread
            .voices
            .iter()
            .map(String::as_str)
            .filter(|voice| Some(*voice) != author && !recent.contains(voice))
            .collect();
        if !returning.is_empty() && dice.chance(0.5) {
            returning[dice.pick(returning.len())].to_owned()
        } else {
            pick_persona(&recent, author, dice)
        }
    });
    let (low, high) = if by_person {
        pace.person_reply
    } else {
        pace.club_reply
    };
    Planned {
        root: thread.root.clone(),
        persona,
        due: now + dice.between(low, high),
        answering: line.map(|line| line.id.clone()),
    }
}

/// A persona other than `author` and those in `recent`.
fn pick_persona(recent: &[&str], author: Option<&str>, dice: &mut dyn Dice) -> String {
    let free: Vec<&str> = PERSONAS
        .iter()
        .map(|persona| persona.name)
        .filter(|name| Some(*name) != author && !recent.contains(name))
        .collect();
    free[dice.pick(free.len())].to_owned()
}

#[cfg(test)]
mod tests;
