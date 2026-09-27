//! Name/keyword ranking for [`Catalogue::search`] and [`Catalogue::suggest`].

use crate::{Catalogue, Emoji};

/// Folded search text for one emoji, built once per catalogue.
#[derive(Debug)]
pub(crate) struct Key {
    name: String,
    name_words: Vec<String>,
    keywords: Vec<String>,
    keyword_words: Vec<String>,
}

impl Key {
    pub(crate) fn new(emoji: &Emoji) -> Self {
        let name = fold(emoji.name());
        let name_words = words(&name).map(str::to_owned).collect();
        let keywords: Vec<String> = emoji.keywords().map(fold).collect();
        let keyword_words = keywords
            .iter()
            .flat_map(|k| words(k))
            .map(str::to_owned)
            .collect();
        Self {
            name,
            name_words,
            keywords,
            keyword_words,
        }
    }

    /// How well one query term matches; 0 is no match.
    fn term(&self, t: &str) -> u32 {
        if self.name.starts_with(t) {
            60
        } else if self.name_words.iter().any(|w| w == t) {
            50
        } else if self.name_words.iter().any(|w| w.starts_with(t)) {
            40
        } else if self.keywords.iter().any(|k| k == t) {
            35
        } else if self.keyword_words.iter().any(|w| w.starts_with(t)) {
            25
        } else if self.name.contains(t) {
            10
        } else if self.keywords.iter().any(|k| k.contains(t)) {
            5
        } else {
            0
        }
    }
}

/// Lowercase and strip the Latin diacritics, ligatures and typographic
/// quotes CLDR names use, so "cote" finds "Côte d’Ivoire", "coeur" finds
/// "cœur" and "strasse" finds "Straße".
fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => out.push('a'),
            'ç' => out.push('c'),
            'è' | 'é' | 'ê' | 'ë' => out.push('e'),
            'ì' | 'í' | 'î' | 'ï' => out.push('i'),
            'ñ' => out.push('n'),
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' => out.push('o'),
            'ù' | 'ú' | 'û' | 'ü' => out.push('u'),
            'ý' | 'ÿ' => out.push('y'),
            'œ' => out.push_str("oe"),
            'æ' => out.push_str("ae"),
            'ß' => out.push_str("ss"),
            '’' | '‘' => out.push('\''),
            c => out.push(c),
        }
    }
    out
}

fn words(s: &str) -> impl Iterator<Item = &str> {
    s.split(|c: char| !(c.is_alphanumeric() || c == '+' || c == '\''))
        .filter(|w| !w.is_empty())
}

fn ranked(cat: &Catalogue, mut scored: Vec<(u32, usize)>, limit: usize) -> Vec<&Emoji> {
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored
        .into_iter()
        .filter_map(|(_, i)| cat.emoji.get(i))
        .take(limit)
        .collect()
}

/// Every emoji whose `keys` match all of `terms`, with its score.
fn matches(keys: &[Key], query: &str, terms: &[&str]) -> Vec<(u32, usize)> {
    keys.iter()
        .enumerate()
        .filter_map(|(i, key)| {
            let mut total: u32 = if key.name == query { 100 } else { 0 };
            for t in terms {
                match key.term(t) {
                    0 => return None,
                    s => total = total.saturating_add(s),
                }
            }
            Some((total, i))
        })
        .collect()
}

pub(crate) fn search<'a>(cat: &'a Catalogue, query: &str, limit: usize) -> Vec<&'a Emoji> {
    let query = fold(query.trim());
    let terms: Vec<&str> = query.split_whitespace().collect();
    if terms.is_empty() {
        return Vec::new();
    }
    let native = matches(&cat.keys, &query, &terms);
    let mut seen: std::collections::HashSet<usize> = native.iter().map(|(_, i)| *i).collect();
    let mut found = ranked(cat, native, limit);
    let english = Catalogue::english();
    if found.len() < limit && !std::ptr::eq(cat, english) {
        // Every catalogue shares one structure, so an index names the same
        // emoji in both; English only adds what the native names missed.
        let extra = matches(&english.keys, &query, &terms)
            .into_iter()
            .filter(|(_, i)| seen.insert(*i))
            .collect();
        found.extend(ranked(cat, extra, limit.saturating_sub(found.len())));
    }
    found
}

pub(crate) fn suggest<'a>(cat: &'a Catalogue, word: &str, limit: usize) -> Vec<&'a Emoji> {
    let word = fold(word.trim());
    if word.chars().count() < 3 || word.contains(char::is_whitespace) {
        return Vec::new();
    }
    let score = |w: &str| -> Vec<(u32, usize)> {
        cat.keys
            .iter()
            .enumerate()
            .filter_map(|(i, key)| {
                let s = if key.name == w {
                    3
                } else if key.keywords.iter().any(|k| k == w) {
                    2
                } else if key.name_words.iter().any(|n| n == w) {
                    1
                } else {
                    return None;
                };
                Some((s, i))
            })
            .collect()
    };
    let mut scored = score(&word);
    if scored.is_empty()
        && let Some(singular) = word.strip_suffix('s')
    {
        scored = score(singular);
    }
    ranked(cat, scored, limit)
}
