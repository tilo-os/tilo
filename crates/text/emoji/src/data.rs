//! Reader for the checked-in data files written by `tools/regenerate.py`.
//!
//! The files are tab-separated records keyed by their first field; `#` lines
//! are comments. The files are generated, so a malformed record is skipped
//! rather than trusted.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use crate::{Catalogue, Emoji, Gender, Group, SkinTone, Subgroup};

const VS16: char = '\u{FE0F}';

/// Lookup key: the text without emoji presentation selectors, so fully- and
/// minimally-qualified spellings resolve to the same entry.
pub(crate) fn key(text: &str) -> String {
    text.chars().filter(|&c| c != VS16).collect()
}

/// Put `tone` after every modifier base, replacing a VS16 that follows it.
fn apply_tone(text: &str, tone: SkinTone, bases: &HashSet<char>) -> String {
    let mut out = String::with_capacity(text.len().saturating_add(8));
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if bases.contains(&c) {
            out.push(tone.modifier());
            chars.next_if_eq(&VS16);
        }
    }
    out
}

struct Names {
    labels: HashMap<&'static str, &'static str>,
    entries: HashMap<&'static str, (&'static str, &'static str)>,
}

fn read_names(text: &'static str) -> Names {
    let mut names = Names {
        labels: HashMap::new(),
        entries: HashMap::new(),
    };
    for line in text.lines() {
        let mut f = line.split('\t');
        match (f.next(), f.next(), f.next(), f.next()) {
            (Some("G"), Some(id), Some(label), _) => {
                names.labels.insert(id, label);
            }
            (Some("E"), Some(emoji), Some(name), keywords) => {
                names
                    .entries
                    .insert(emoji, (name, keywords.unwrap_or_default()));
            }
            _ => {}
        }
    }
    names
}

#[allow(
    clippy::too_many_lines,
    reason = "one pass over the structure file; each record kind is a short arm"
)]
pub(crate) fn build(
    locale: &'static str,
    structure: &'static str,
    names_text: &'static str,
    fallback_text: Option<&'static str>,
) -> Catalogue {
    let names = read_names(names_text);
    let fallback = fallback_text.map(read_names);
    let label_of = |id: &'static str| -> &'static str {
        names
            .labels
            .get(id)
            .or_else(|| fallback.as_ref()?.labels.get(id))
            .copied()
            .unwrap_or(id)
    };
    let entry_of = |text: &'static str| -> (&'static str, &'static str) {
        names
            .entries
            .get(text)
            .or_else(|| fallback.as_ref()?.entries.get(text))
            .copied()
            .unwrap_or((text, ""))
    };
    let mut emoji: Vec<Emoji> = Vec::new();
    let mut groups: Vec<Group> = Vec::new();
    let mut subgroups: Vec<Subgroup> = Vec::new();
    let mut bases: HashSet<char> = HashSet::new();
    let mut overrides: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut triples: Vec<(&str, &str, &str)> = Vec::new();

    for line in structure.lines() {
        let mut f = line.split('\t');
        match (f.next(), f.next()) {
            (Some("M"), Some(list)) => {
                bases.extend(
                    list.split(' ')
                        .filter_map(|h| u32::from_str_radix(h, 16).ok())
                        .filter_map(char::from_u32),
                );
            }
            (Some("G"), Some(id)) => {
                let at = emoji.len();
                let sub_at = subgroups.len();
                groups.push(Group {
                    id,
                    label: label_of(id),
                    first: "",
                    subgroups: sub_at..sub_at,
                    emoji: at..at,
                });
            }
            (Some("S"), Some(id)) => {
                let Some(group) = groups.len().checked_sub(1) else {
                    continue;
                };
                let at = emoji.len();
                subgroups.push(Subgroup {
                    id,
                    group,
                    emoji: at..at,
                });
                if let Some(g) = groups.last_mut() {
                    g.subgroups.end = subgroups.len();
                }
            }
            (Some("E"), Some(text)) => {
                let (Some(group), Some(subgroup)) =
                    (groups.len().checked_sub(1), subgroups.len().checked_sub(1))
                else {
                    continue;
                };
                let (name, keywords) = entry_of(text);
                emoji.push(Emoji {
                    text,
                    name,
                    keywords,
                    group,
                    subgroup,
                    takes_skin_tone: f.next().is_some_and(|flags| flags.contains('t')),
                    gender: None,
                });
                if let Some(g) = groups.last_mut() {
                    g.emoji.end = emoji.len();
                    if g.first.is_empty() {
                        g.first = text;
                    }
                }
                if let Some(s) = subgroups.last_mut() {
                    s.emoji.end = emoji.len();
                }
            }
            (Some("T"), Some(text)) => {
                overrides.insert(text, f.collect());
            }
            (Some("V"), Some(neutral)) => {
                if let (Some(man), Some(woman)) = (f.next(), f.next()) {
                    triples.push((neutral, man, woman));
                }
            }
            _ => {}
        }
    }

    let mut index: HashMap<String, (usize, Option<SkinTone>)> = HashMap::new();
    let mut toned: HashMap<usize, [String; 5]> = HashMap::new();
    for (i, e) in emoji.iter_mut().enumerate() {
        index.insert(key(e.text), (i, None));
        if !e.takes_skin_tone {
            continue;
        }
        let forms = SkinTone::ALL.map(|tone| match overrides.get(e.text) {
            Some(list) => list
                .get(tone.index())
                .map_or_else(|| e.text.to_owned(), |s| (*s).to_owned()),
            None => apply_tone(e.text, tone, &bases),
        });
        for (tone, form) in SkinTone::ALL.iter().zip(&forms) {
            index.insert(key(form), (i, Some(*tone)));
        }
        toned.insert(i, forms);
    }

    let at = |text: &str| -> Option<usize> {
        if text.is_empty() {
            return None;
        }
        index.get(&key(text)).map(|(i, _)| *i)
    };
    let mut genders = HashMap::new();
    let mut neutral_of = HashMap::new();
    let mut marks = Vec::new();
    for (neutral, man, woman) in triples {
        let Some(n) = at(neutral) else { continue };
        let (m, w) = (at(man), at(woman));
        genders.insert(n, (m, w));
        marks.push((n, Gender::Neutral));
        for (g, gender) in [(m, Gender::Man), (w, Gender::Woman)] {
            if let Some(g) = g {
                neutral_of.insert(g, n);
                marks.push((g, gender));
            }
        }
    }
    for (i, gender) in marks {
        if let Some(e) = emoji.get_mut(i) {
            e.gender = Some(gender);
        }
    }

    Catalogue {
        locale,
        emoji,
        groups,
        subgroups,
        index,
        toned,
        genders,
        neutral_of,
        search: OnceLock::new(),
    }
}
