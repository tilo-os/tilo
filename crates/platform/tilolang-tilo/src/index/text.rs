//! The words of a resource's texts, and finding resources by them.
//!
//! A text is cut into words at every character that is not a letter or a digit, each word
//! folded to lower case. The index posts each word of each text of a resource under the
//! resource, with a weight: a resource's first text (what names it) weighs more than its
//! others. A query finds the resources holding every word of it, its last word as typed so
//! far (a prefix), heaviest first.

use std::collections::{BTreeMap, BTreeSet};

use super::{IndexError, Snapshot};

/// What a word weighs in the text that names its resource, and in any other.
pub const NAMING: u8 = 2;
pub const OTHER: u8 = 1;

/// The words of `text`, each once.
#[must_use]
pub fn words(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// One resource a query found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub resource: String,
    pub weight: u32,
}

/// The resources of `snapshot` holding every word of `query` (the last as a prefix), heaviest
/// first, then by path: the first `limit` of them.
///
/// # Errors
/// The index cannot be read or is corrupt.
pub fn search(snapshot: &Snapshot, query: &str, limit: usize) -> Result<Vec<Found>, IndexError> {
    let folded = query.to_lowercase();
    let mut parts: Vec<&str> = folded
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    // A query that ends in a word being typed matches it as a prefix; one that ends at a space
    // matches its last word whole.
    let last_is_prefix = folded.chars().last().is_some_and(char::is_alphanumeric);
    let Some(last) = parts.pop() else {
        return Ok(Vec::new());
    };
    let mut found: Option<BTreeMap<String, u32>> = None;
    let mut postings = Vec::new();
    for word in &parts {
        postings.push(snapshot.postings(word, true)?);
    }
    postings.push(snapshot.postings(last, !last_is_prefix)?);
    // The rarest word first: the others narrow what it found.
    postings.sort_by_key(Vec::len);
    for posting in postings {
        let mut weights: BTreeMap<String, u32> = BTreeMap::new();
        for (resource, weight) in posting {
            let held = weights.entry(resource).or_default();
            *held = (*held).max(u32::from(weight));
        }
        found = Some(match found {
            None => weights,
            Some(before) => before
                .into_iter()
                .filter_map(|(resource, weight)| {
                    let more = weights.get(&resource)?;
                    Some((resource, weight.saturating_add(*more)))
                })
                .collect(),
        });
    }
    let mut out: Vec<Found> = found
        .unwrap_or_default()
        .into_iter()
        .map(|(resource, weight)| Found { resource, weight })
        .collect();
    out.sort_by(|a, b| {
        b.weight
            .cmp(&a.weight)
            .then_with(|| a.resource.cmp(&b.resource))
    });
    out.truncate(limit);
    Ok(out)
}
