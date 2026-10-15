//! Asks the real engines and fetches a real page. Needs the network, and the
//! engines change their pages and their tolerance without notice, so it only
//! runs when asked: `cargo test -p tilo-web-search --test live -- --ignored`.

use tilo_web_search::{Engine, EngineStatus, FetchLimits, SearchLimits, client, fetch, search};

#[test]
#[ignore = "needs the network and live search engines"]
fn the_default_engines_answer_and_a_result_can_be_read() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let client = client().unwrap();
    let response = runtime
        .block_on(search(
            &client,
            "rust programming language",
            &Engine::ALL,
            &SearchLimits::default(),
        ))
        .unwrap();
    for report in &response.engines {
        eprintln!("{report:?}");
    }
    assert!(
        response
            .engines
            .iter()
            .any(|r| r.status == EngineStatus::Ok && Engine::DEFAULT.contains(&r.engine)),
        "at least one default engine answers"
    );
    assert!(!response.results.is_empty());
    let page = runtime
        .block_on(fetch(
            &client,
            "https://www.rust-lang.org/",
            &FetchLimits::default(),
        ))
        .unwrap();
    assert!(page.text.contains("Rust"), "{}", page.text);
    assert!(page.title.is_some());
}
