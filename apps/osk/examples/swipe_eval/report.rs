//! Aggregate evaluation of public donated gestures; donor identities stay absent.
use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::Write;

use osk::{Candidate, GeometricSwipeModel};
use serde::Serialize;

use super::Gesture;

fn add(count: &mut u32, amount: u32) -> std::io::Result<()> {
    *count = count
        .checked_add(amount)
        .ok_or_else(|| std::io::Error::other("evaluation counter overflow"))?;
    Ok(())
}

pub fn lexical(word: &str) -> String {
    word.chars()
        .flat_map(char::to_lowercase)
        .map(osk::dict::fold_char)
        .filter(char::is_ascii_alphabetic)
        .collect()
}

pub struct Case<'a> {
    pub gesture: &'a Gesture,
    pub target: String,
    pub known: [bool; 2],
    pub candidates: &'a [Candidate],
}

impl<'a> Case<'a> {
    pub fn new(
        gesture: &'a Gesture,
        model: &GeometricSwipeModel,
        lexicon: &HashSet<String>,
        candidates: &'a [Candidate],
    ) -> Self {
        let target = lexical(&gesture.word);
        let known = [
            model.dictionary().contains(&gesture.word),
            lexicon.contains(&target),
        ];
        Self {
            gesture,
            target,
            known,
            candidates,
        }
    }
}

#[derive(Default, Serialize)]
pub struct Accuracy {
    pub eligible: u32,
    pub in_vocabulary: u32,
    pub top1_correct: u32,
    pub top3_correct: u32,
    pub top8_correct: u32,
    pub top64_correct: u32,
}

impl Accuracy {
    fn observe(
        &mut self,
        target: &str,
        known: bool,
        predictions: &[String],
    ) -> std::io::Result<()> {
        add(&mut self.eligible, 1)?;
        add(&mut self.in_vocabulary, u32::from(known))?;
        for (count, n) in [
            (&mut self.top1_correct, 1),
            (&mut self.top3_correct, 3),
            (&mut self.top8_correct, 8),
            (&mut self.top64_correct, 64),
        ] {
            add(
                count,
                u32::from(predictions.iter().take(n).any(|word| word == target)),
            )?;
        }
        Ok(())
    }
}

#[derive(Default, Serialize)]
pub struct Stats {
    pub records: u32,
    pub literal: Accuracy,
    pub lexical: Accuracy,
    pub no_candidates: u32,
    pub known_target_missing_from_top64: u32,
}

impl Stats {
    pub fn observe(&mut self, case: &Case<'_>) -> std::io::Result<()> {
        add(&mut self.records, 1)?;
        if case.gesture.points.len() < 2 || case.gesture.word.chars().count() < 2 {
            return Ok(());
        }
        let predictions: Vec<_> = case.candidates.iter().map(|c| c.word.clone()).collect();
        self.literal
            .observe(&case.gesture.word, case.known[0], &predictions)?;
        if case.target.len() < 2 {
            return Ok(());
        }
        let predictions: Vec<_> = predictions.iter().map(|word| lexical(word)).collect();
        self.lexical
            .observe(&case.target, case.known[1], &predictions)?;
        add(&mut self.no_candidates, u32::from(predictions.is_empty()))?;
        add(
            &mut self.known_target_missing_from_top64,
            u32::from(case.known[1] && !predictions.iter().any(|word| word == &case.target)),
        )?;
        Ok(())
    }
}

#[derive(Default, Serialize)]
pub struct Report {
    pub overall: Stats,
    pub groups: BTreeMap<String, Stats>,
    pub orientations: BTreeMap<String, Stats>,
    pub word_lengths: BTreeMap<String, Stats>,
    pub sessions: BTreeMap<u32, Stats>,
    pub out_of_vocabulary: BTreeMap<String, u32>,
    pub confusions: BTreeMap<String, u32>,
}

impl Report {
    pub fn observe(&mut self, case: &Case<'_>, errors: &mut File) -> std::io::Result<()> {
        self.overall.observe(case)?;
        self.groups
            .entry(case.gesture.group.clone())
            .or_default()
            .observe(case)?;
        self.orientations
            .entry(case.gesture.orientation.clone())
            .or_default()
            .observe(case)?;
        let length = match case.target.len() {
            0..=3 => "<=3",
            4..=6 => "4-6",
            _ => "7+",
        };
        self.word_lengths
            .entry(length.into())
            .or_default()
            .observe(case)?;
        if let Some(session) = case.gesture.session {
            self.sessions.entry(session).or_default().observe(case)?;
        }
        if case.gesture.group != "analysis"
            || case.target.len() < 2
            || case.gesture.points.len() < 2
        {
            return Ok(());
        }
        if !case.known[1] {
            add(
                self.out_of_vocabulary
                    .entry(case.target.clone())
                    .or_default(),
                1,
            )?;
        }
        let predicted = case.candidates.first().map_or("", |c| c.word.as_str());
        if lexical(predicted) != case.target {
            add(
                self.confusions
                    .entry(format!("{} -> {predicted}", case.target))
                    .or_default(),
                1,
            )?;
            // Only diagnostic donors' public target words/candidates, never paths,
            // sentence text, timestamps, original IDs or qualification errors.
            writeln!(
                errors,
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "target":case.target, "known":case.known[1],
                    "width":case.gesture.width,"height":case.gesture.height,
                    "points":case.gesture.points.len(),"candidates":case.candidates.iter()
                        .map(|c| serde_json::json!({"word":c.word,"score":c.score})).collect::<Vec<_>>(),
                }))?
            )?;
        }
        Ok(())
    }
}
