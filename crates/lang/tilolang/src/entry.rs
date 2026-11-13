//! What the text of an entry will make, read from the create action it runs: the action's declared
//! parameters say what it takes out of the text, and their types say how.
//!
//! An entry ([`std.add_entry`]) is handed the action it commits — a call of an action with one
//! `Text` parameter, which the call left for the text (`new_task(tasks, p: project)`). The words
//! before the entry type say what the parameters take:
//!
//! - a `Date` or `Time` parameter takes a day or a moment, as people say it ([`crate::natural`]):
//!   "tomorrow", "fri 10am", "in 2 weeks", "3 nov";
//! - a `Principal` parameter takes `@name`, matched against the workspace's members by the name they
//!   go by;
//! - a parameter of a record type takes `#title`, matched against the records of that type by its
//!   title field.
//!
//! A parameter the call already gave is a default: what the words name fills it. Where the words say
//! nothing of it, the value the call gave stands. Everything the parse is not sure of stays in the
//! title: two members of one name, a title two records share, a word that only looks like a date.

use crate::calendar::{DAY, day_text, time_text};
use crate::natural::{Language, day_label, scan, words};

/// What a create action's parameter takes out of the text an entry is typed into.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Takes {
    Entry,
    Day,
    Time,
    Who,
    /// A record this workspace holds, from "#title".
    What {
        app: String,
        ty: String,
    },
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    pub takes: Takes,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Filled {
    Day(String),
    Time(String),
    Who(String),
    Record(String),
}

/// One thing the entry takes out of the text: shown as a chip while it is typed, and removed from the
/// text when the chip is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    /// What it says: "Fri 30 Oct", "10:00", "@Bo", "#Move flat".
    pub label: String,
    pub icon: &'static str,
    /// What is typed once it is removed: the text without its words.
    pub without: String,
}

/// What the text says the action will make.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    /// The title: the words no other parameter took, and empty when the text was nothing but parts and
    /// space: then the entry makes nothing.
    pub title: String,
    pub fills: Vec<(usize, Filled)>,
    /// What the entry shows as parts of what will be made, in the order they stand in it.
    pub parts: Vec<Part>,
}

/// What the text's names can be matched against: who is in the workspace and the records its
/// collections hold.
pub trait Look {
    fn people(&self) -> Vec<(String, String)>;
    fn records(&self, app: &str, ty: &str) -> Vec<(String, String)>;
}

/// The longest run of words after a `#` that names a record: a title of several words ("#Move flat") is
/// read whole before the shorter read is tried.
const LONGEST_TITLE: usize = 5;

/// Reads what `text` says the action of `params` will make, typed at `now` (seconds since 1970, UTC) by
/// someone whose clock is `offset` seconds ahead of UTC, typing in `lang`.
#[must_use]
pub fn read(
    text: &str,
    params: &[Param],
    look: &dyn Look,
    now: i64,
    offset: i64,
    lang: Language,
) -> Parsed {
    let local = now.saturating_add(offset);
    let today = local.div_euclid(DAY);
    let words = words(text);
    let mut used = vec![false; words.len()];
    let mut fills: Vec<(usize, Filled)> = Vec::new();
    // The parts, with the word each starts at, so they stand in the text's order.
    let mut parts: Vec<(usize, Part)> = Vec::new();

    // When: a day to the day parameter, a time to the time one; a time alone fills the day too when the
    // action has one, as "at 9" means the next nine.
    let mut day_slot = params.iter().position(|p| p.takes == Takes::Day);
    let mut time_slot = params.iter().position(|p| p.takes == Takes::Time);
    // The day the text said last: a time after it ("fri 10am") is at that day.
    let mut said: Option<i64> = None;
    for phrase in scan(&words, today, lang) {
        said = phrase.day.or(said);
        let mut label = String::new();
        let mut icon = "calendar";
        let mut filled = false;
        if let (Some(slot), Some(day)) = (day_slot, phrase.day) {
            fills.push((slot, Filled::Day(day_text(day))));
            day_slot = None;
            label = day_label(day, today, lang);
            filled = true;
        }
        if let Some(minutes) = phrase.minutes {
            // The moment it is at: the day the text said, else the next time to come.
            let day = said.unwrap_or_else(|| {
                if minutes * 60 > local.rem_euclid(DAY) {
                    today
                } else {
                    today + 1
                }
            });
            let clock = format!("{:02}:{:02}", minutes / 60, minutes % 60);
            if let Some(slot) = time_slot {
                fills.push((
                    slot,
                    Filled::Time(time_text(day * DAY + minutes * 60 - offset)),
                ));
                time_slot = None;
                icon = "clock";
                label = if label.is_empty() {
                    clock
                } else {
                    format!("{label} {clock}")
                };
                filled = true;
            } else if let (Some(slot), false) = (day_slot, filled) {
                fills.push((slot, Filled::Day(day_text(day))));
                day_slot = None;
                icon = "clock";
                label = clock;
                filled = true;
            }
        }
        if !filled {
            continue;
        }
        let taken: Vec<usize> = (phrase.from..phrase.from + phrase.words).collect();
        for i in &taken {
            used[*i] = true;
        }
        let without = without(&words, &taken);
        parts.push((
            phrase.from,
            Part {
                label,
                icon,
                without,
            },
        ));
    }

    // Who: "@name" once for each principal, by the name they go by or their id.
    if let Some(slot) = params.iter().position(|p| p.takes == Takes::Who) {
        let people = look.people();
        for (i, word) in words.iter().enumerate() {
            if used[i] {
                continue;
            }
            let Some(name) = word.text.strip_prefix('@').filter(|n| !n.is_empty()) else {
                continue;
            };
            let mut found: Vec<&str> = people
                .iter()
                .filter(|(name_of, id)| {
                    name_of.eq_ignore_ascii_case(name) || id.eq_ignore_ascii_case(name)
                })
                .map(|(_, id)| id.as_str())
                .collect();
            found.sort_unstable();
            found.dedup();
            let [id] = found.as_slice() else {
                continue;
            };
            let name_of = people
                .iter()
                .find(|(_, person)| person == id)
                .map_or(name, |(name_of, _)| name_of.as_str());
            used[i] = true;
            fills.push((slot, Filled::Who((*id).to_owned())));
            parts.push((
                i,
                Part {
                    label: format!("@{name_of}"),
                    icon: "user",
                    without: without(&words, &[i]),
                },
            ));
        }
    }

    // What: "#title" against the records of each record parameter's collection, the longest run of
    // words that names exactly one of them.
    let record_params: Vec<usize> = params
        .iter()
        .enumerate()
        .filter(|(_, p)| matches!(p.takes, Takes::What { .. }))
        .map(|(i, _)| i)
        .collect();
    if !record_params.is_empty() {
        let titles: Vec<(usize, Vec<(String, String)>)> = record_params
            .iter()
            .map(|&slot| {
                let found = match &params[slot].takes {
                    Takes::What { app, ty } => look.records(app, ty),
                    _ => Vec::new(),
                };
                (slot, found)
            })
            .collect();
        let mut at = 0;
        while at < words.len() {
            if used[at] {
                at += 1;
                continue;
            }
            let Some(first) = words[at].text.strip_prefix('#') else {
                at += 1;
                continue;
            };
            let mut best: Option<(usize, usize, String, String)> = None;
            for n in 1..=LONGEST_TITLE {
                if at + n > words.len() || used[at..at + n].iter().any(|u| *u) {
                    break;
                }
                let mut run = String::from(first);
                for word in &words[at + 1..at + n] {
                    if !run.is_empty() {
                        run.push(' ');
                    }
                    run.push_str(&word.text);
                }
                let mut found = titles
                    .iter()
                    .flat_map(|(slot, records)| {
                        records
                            .iter()
                            .filter(|(title, _)| title.to_lowercase() == run)
                            .map(|(title, id)| (*slot, title.clone(), id.clone()))
                    })
                    .collect::<Vec<_>>();
                found.dedup();
                match found.as_slice() {
                    [_, _, ..] => {
                        best = None;
                        break;
                    }
                    [(slot, title, id)] => best = Some((*slot, n, title.clone(), id.clone())),
                    [] => {}
                }
            }
            let Some((slot, n, title, id)) = best else {
                at += 1;
                continue;
            };
            fills.push((slot, Filled::Record(id)));
            let taken: Vec<usize> = (at..at + n).collect();
            for i in &taken {
                used[*i] = true;
            }
            parts.push((
                at,
                Part {
                    label: format!("#{title}"),
                    icon: "hash",
                    without: without(&words, &taken),
                },
            ));
            at += n;
        }
    }

    // The title: the words no parameter took. An entry that is only parts names nothing: there is
    // nothing to make of it.
    let title = words
        .iter()
        .zip(&used)
        .filter(|(_, used)| !**used)
        .map(|(w, _)| w.raw.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    parts.sort_by_key(|(from, _)| *from);
    Parsed {
        title,
        fills,
        parts: parts.into_iter().map(|(_, part)| part).collect(),
    }
}

fn without(words: &[crate::natural::Word], taken: &[usize]) -> String {
    words
        .iter()
        .enumerate()
        .filter(|(i, _)| !taken.contains(i))
        .map(|(_, w)| w.raw.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::{Filled, Look, Param, Takes, read};
    use crate::natural::Language;

    const NOW: i64 = 1_793_257_200;
    const BERLIN: i64 = 3600;

    struct Fixed {
        people: Vec<(String, String)>,
        records: Vec<(String, String)>,
    }

    impl Look for Fixed {
        fn people(&self) -> Vec<(String, String)> {
            self.people.clone()
        }

        fn records(&self, _app: &str, _ty: &str) -> Vec<(String, String)> {
            self.records.clone()
        }
    }

    fn look() -> Fixed {
        Fixed {
            people: vec![("Bo".into(), "bo".into()), ("Ada".into(), "ada".into())],
            records: vec![
                ("Move flat".into(), "tasks:Move flat".into()),
                ("Move office".into(), "tasks:Move office".into()),
            ],
        }
    }

    fn tasks() -> Vec<Param> {
        vec![
            Param {
                takes: Takes::Other,
            },
            Param { takes: Takes::Day },
            Param { takes: Takes::Time },
            Param { takes: Takes::Who },
            Param {
                takes: Takes::What {
                    app: "tasks".into(),
                    ty: "Project".into(),
                },
            },
            Param {
                takes: Takes::Entry,
            },
        ]
    }

    fn story(text: &str, params: &[Param], look: &dyn Look) -> super::Parsed {
        read(text, params, look, NOW, BERLIN, Language::English)
    }

    #[test]
    fn the_story_reads_every_parameter_of_the_create_action() {
        let parsed = story("call landlord fri 10am @bo #move flat", &tasks(), &look());
        assert_eq!(parsed.title, "call landlord");
        assert_eq!(
            parsed.fills,
            vec![
                (1, Filled::Day("2026-10-30".into())),
                (2, Filled::Time("2026-10-30T09:00:00Z".into())),
                (3, Filled::Who("bo".into())),
                (4, Filled::Record("tasks:Move flat".into())),
            ]
        );
        let labels: Vec<&str> = parsed.parts.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels, vec!["Tomorrow", "10:00", "@Bo", "#Move flat"]);
    }

    #[test]
    fn a_part_leaves_the_title_and_can_be_taken_out() {
        let parsed = story("call landlord fri 10am @bo", &tasks(), &look());
        let due = &parsed.parts[0];
        assert_eq!(due.without, "call landlord 10am @bo");
        let who = &parsed.parts[2];
        assert_eq!(who.without, "call landlord fri 10am");
    }

    #[test]
    fn what_is_only_parts_or_space_names_nothing() {
        for text in ["fri 10am @bo", "   ", ""] {
            let parsed = story(text, &tasks(), &look());
            assert_eq!(parsed.title, "", "`{text}` names nothing");
        }
        assert_eq!(story("fri 10am @bo", &tasks(), &look()).parts.len(), 3);
        assert_eq!(story(" @bo ", &tasks(), &look()).title, "");
        assert_eq!(story(" milk @bo ", &tasks(), &look()).title, "milk");
    }

    #[test]
    fn what_is_not_sure_stays_in_the_title() {
        let parsed = story("call landlord @zoe", &tasks(), &look());
        assert_eq!(parsed.title, "call landlord @zoe");
        assert!(parsed.parts.is_empty());
        let mut two = look();
        two.people.push(("Bo".into(), "bo-2".into()));
        let parsed = story("call @bo", &tasks(), &two);
        assert_eq!(parsed.title, "call @bo");
        assert!(parsed.parts.is_empty());
        let parsed = story("call #nowhere", &tasks(), &look());
        assert_eq!(parsed.title, "call #nowhere");
        let parsed = story("read 3 chapters", &tasks(), &look());
        assert_eq!(parsed.title, "read 3 chapters");
        assert!(parsed.parts.is_empty());
    }

    #[test]
    fn a_time_alone_fills_the_day_it_falls_on() {
        let mut params = tasks();
        params.remove(2);
        let parsed = story("call landlord at 9", &params, &look());
        assert_eq!(parsed.fills, vec![(1, Filled::Day("2026-10-29".into()))]);
        assert_eq!(parsed.parts[0].label, "09:00");
    }

    #[test]
    fn german_says_its_parts_in_its_own_words() {
        let done = read(
            "Vermieter anrufen morgen 10 uhr @bo",
            &tasks(),
            &look(),
            NOW,
            BERLIN,
            Language::German,
        );
        assert_eq!(done.title, "Vermieter anrufen");
        assert_eq!(
            done.fills,
            vec![
                (1, Filled::Day("2026-10-30".into())),
                (2, Filled::Time("2026-10-30T09:00:00Z".into())),
                (3, Filled::Who("bo".into())),
            ]
        );
        assert_eq!(done.parts[0].label, "Morgen");
        assert_eq!(done.parts[1].label, "10:00");
    }
}
