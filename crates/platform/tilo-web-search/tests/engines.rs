//! Each engine's reader against a result page recorded from it (or, where
//! the live engine refuses this client, written in its shape), and the merge
//! of several engines' answers.
#![allow(clippy::unwrap_used, reason = "in a test these are assertions")]

use std::path::Path;

use tilo_web_search::merge::merge;
use tilo_web_search::{Engine, EngineError, EngineResult};

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

fn parse(engine: Engine, name: &str) -> Vec<EngineResult> {
    let parsed = engine.parse(&fixture(name));
    assert!(parsed.is_ok(), "{engine}: {parsed:?}");
    parsed.unwrap()
}

fn urls(results: &[EngineResult]) -> Vec<&str> {
    results.iter().map(|r| r.url.as_str()).collect()
}

#[test]
fn bing_results_unwrap_its_click_tracking_links() {
    let results = parse(Engine::Bing, "bing.html");
    assert_eq!(
        urls(&results),
        [
            "https://rust-lang.org/",
            "https://en.wikipedia.org/wiki/Rust_(programming_language)",
            "https://rust-lang.org/tools/install/",
        ]
    );
    for result in &results {
        assert!(!result.title.is_empty(), "{result:?}");
        assert!(result.snippet.len() > 20, "{result:?}");
        assert!(!result.snippet.contains('<'), "{result:?}");
    }
}

#[test]
fn brave_results_skip_standalone_boxes() {
    let results = parse(Engine::Brave, "brave.html");
    assert_eq!(results.len(), 3, "{results:#?}");
    assert_eq!(results[0].title, "Rust Programming Language");
    assert_eq!(results[1].title, "Rust (programming language) - Wikipedia");
    assert!(
        results[1]
            .url
            .starts_with("https://en.wikipedia.org/wiki/Rust")
    );
    for result in &results {
        assert!(!result.snippet.is_empty(), "{result:?}");
    }
}

#[test]
fn duckduckgo_results_unwrap_its_redirect_links() {
    let results = parse(Engine::DuckDuckGo, "duckduckgo.html");
    assert_eq!(
        urls(&results),
        [
            "https://rust-lang.org/",
            "https://en.wikipedia.org/wiki/Rust_(programming_language)",
            "https://rust-lang.org/tools/install/",
        ]
    );
    assert_eq!(results[2].title, "Install Rust - Rust Programming Language");
    assert!(results.iter().all(|r| !r.snippet.is_empty()));
}

#[test]
fn google_results_resolve_its_redirect_links() {
    let results = parse(Engine::Google, "google.html");
    assert_eq!(
        urls(&results),
        [
            "https://www.rust-lang.org/",
            "https://en.wikipedia.org/wiki/Rust_(programming_language)",
        ]
    );
    assert_eq!(
        results[0].snippet,
        "A language empowering everyone to build reliable and efficient software."
    );
}

#[test]
fn marginalia_results_skip_entries_without_a_description() {
    let results = parse(Engine::Marginalia, "marginalia.html");
    assert_eq!(
        urls(&results),
        [
            "https://doc.rust-lang.org/book/",
            "http://www.example.net/rust/notes.html"
        ]
    );
    assert_eq!(
        results[1].title,
        "Notes on learning Rust & its borrow checker"
    );
}

#[test]
fn a_page_of_another_shape_is_a_layout_error_not_an_empty_answer() {
    for engine in Engine::ALL {
        let page = "<html><head><title>Hello</title></head><body><p>Nothing here</p></body></html>";
        assert!(
            matches!(engine.parse(page), Err(EngineError::Layout(_))),
            "{engine}"
        );
    }
}

#[test]
fn pages_several_engines_found_rank_first() {
    let answers = [
        (Engine::Bing, parse(Engine::Bing, "bing.html")),
        (Engine::Brave, parse(Engine::Brave, "brave.html")),
        (
            Engine::DuckDuckGo,
            parse(Engine::DuckDuckGo, "duckduckgo.html"),
        ),
    ];
    let merged = merge(&answers);
    // Bing and DuckDuckGo rank rust-lang.org first (2.0); Brave links its
    // /en-US/ page instead, so Wikipedia, second on all three, follows with
    // 0.5 + 0.625 + 0.5.
    assert_eq!(merged[0].url, "https://rust-lang.org/");
    assert_eq!(merged[0].engines, [Engine::Bing, Engine::DuckDuckGo]);
    let wikipedia = &merged[1];
    assert!(wikipedia.url.contains("wikipedia.org"), "{merged:#?}");
    assert_eq!(
        wikipedia.engines,
        [Engine::Brave, Engine::Bing, Engine::DuckDuckGo]
    );
    assert!((wikipedia.score - 1.625).abs() < 1e-9);
    let mut keys: Vec<String> = merged
        .iter()
        .map(|r| tilo_web_search::urls::dedup_key(&r.url))
        .collect();
    let count = keys.len();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), count, "every page appears once");
    let json = serde_json::to_value(&merged).unwrap();
    assert_eq!(
        json[1]["engines"],
        serde_json::json!(["brave", "bing", "duckduckgo"])
    );
    assert!(json[0].get("score").is_none());
}
