//! Merging several engines' rankings into one.
//!
//! Ported from metasearch2's `merge_engine_responses` in
//! `src/engines/ranking.rs` (CC0-1.0, see `PROVENANCE.md`): a result at
//! position `n` on an engine scores `weight / n`, the same page found by
//! several engines adds their scores, and the page keeps the title and
//! snippet of the most trusted engine that found it. Pages are the same when
//! their [`crate::urls::dedup_key`] is.

use std::collections::HashMap;

use serde::Serialize;

use crate::engines::{Engine, EngineResult};

/// One merged result.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
    /// The engines that found it, most trusted first.
    pub engines: Vec<Engine>,
    #[serde(skip)]
    pub score: f64,
}

/// Merge each engine's ranked results, best first.
pub fn merge(responses: &[(Engine, Vec<EngineResult>)]) -> Vec<SearchResult> {
    let mut merged: Vec<SearchResult> = Vec::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    for (engine, results) in responses {
        let weight = engine.weight();
        for (rank, result) in results.iter().enumerate() {
            let position = f64::from(u32::try_from(rank).unwrap_or(u32::MAX)) + 1.0;
            let score = weight / position;
            let key = crate::urls::dedup_key(&result.url);
            if let Some(existing) = by_key.get(&key).and_then(|&i| merged.get_mut(i)) {
                if existing.engines.contains(engine) {
                    // An engine listing one page twice ranks it once.
                    continue;
                }
                let best = existing
                    .engines
                    .iter()
                    .map(|e| e.weight())
                    .fold(0.0, f64::max);
                if weight > best {
                    existing.title.clone_from(&result.title);
                    existing.snippet.clone_from(&result.snippet);
                }
                existing.engines.push(*engine);
                existing.score += score;
            } else {
                by_key.insert(key, merged.len());
                merged.push(SearchResult {
                    title: result.title.clone(),
                    url: result.url.clone(),
                    snippet: result.snippet.clone(),
                    engines: vec![*engine],
                    score,
                });
            }
        }
    }
    for result in &mut merged {
        result
            .engines
            .sort_by(|a, b| b.weight().total_cmp(&a.weight()).then(a.cmp(b)));
    }
    // Stable, so equal scores keep the order the engines were listed in.
    merged.sort_by(|a, b| b.score.total_cmp(&a.score));
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(url: &str, title: &str) -> EngineResult {
        EngineResult {
            url: url.into(),
            title: title.into(),
            snippet: format!("about {title}"),
        }
    }

    #[test]
    fn agreement_between_engines_outranks_one_engines_top_hit() {
        let merged = merge(&[
            (
                Engine::Bing,
                vec![
                    result("https://only-bing.example/", "Only Bing"),
                    result("https://www.shared.example/page/", "Shared (Bing)"),
                ],
            ),
            (
                Engine::Brave,
                vec![
                    result("https://shared.example/page", "Shared (Brave)"),
                    result("https://only-brave.example/", "Only Brave"),
                ],
            ),
        ]);
        let urls: Vec<&str> = merged.iter().map(|r| r.url.as_str()).collect();
        // Shared: 1.0/2 + 1.25/1 = 1.75; Only Bing 1.0; Only Brave 0.625.
        assert_eq!(
            urls,
            [
                "https://www.shared.example/page/",
                "https://only-bing.example/",
                "https://only-brave.example/"
            ]
        );
        let shared = &merged[0];
        assert_eq!(shared.engines, [Engine::Brave, Engine::Bing]);
        assert_eq!(
            shared.title, "Shared (Brave)",
            "the more trusted engine's title wins"
        );
        assert!((shared.score - 1.75).abs() < 1e-9);
    }

    #[test]
    fn an_engine_repeating_a_page_counts_it_once() {
        let merged = merge(&[(
            Engine::DuckDuckGo,
            vec![
                result("https://a.example/", "A"),
                result("https://a.example/#again", "A again"),
            ],
        )]);
        assert_eq!(merged.len(), 1);
        assert!((merged[0].score - 1.0).abs() < 1e-9);
    }
}
