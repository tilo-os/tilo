//! A window's search, and the host that answers for it: the entries an app published into the
//! session's index, which is what an app in a process of its own gives the desk.

use super::*;
use crate::model::{Hit, Source, Target};

/// A window's app, as a session reads it: what it published into the session's index.
struct Conversations;

impl Source for Conversations {
    fn id(&self) -> &'static str {
        "app-content"
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        let needle = needle.trim().to_lowercase();
        if needle.is_empty() || !"voice".starts_with(&needle) {
            return Vec::new();
        }
        vec![Hit {
            row: None,
            preview: None,
            faces: Vec::new(),
            outcome: None,
            kind: ResourceKind::Custom("Conversations".into()),
            provider: self.id().into(),
            id: "chat.workspace/channels/c_voice".into(),
            title: "voice".into(),
            subtitle: None,
            facts: Vec::new(),
            score: ui::search::system::MAX_PROVIDER_SCORE,
            icon: None,
            target: Target::Window {
                tab: 0,
                app: Some("so.tilo.chat".into()),
            },
        }]
    }
}

/// A window's search asks what its host adds for it: the entries the app published into the
/// session's index, which is everything an app in a process of its own gives the desk. The
/// desk's own rows are not the window's.
#[test]
fn a_windows_search_finds_what_the_host_adds_for_the_app() {
    let mut search = DesktopSearch::new();
    search.open(SearchData {
        scope: Scope::InApp(ui::search::AppScope::of("so.tilo.chat")),
        sources: vec![Held::new(Conversations)],
        ..SearchData::default()
    });
    search.set_query("voice");
    let deadline = ui::time::now()
        .checked_add(crate::tests::SETTLE)
        .unwrap_or_else(ui::time::now);
    while search.refresh() || search.answering() {
        assert!(ui::time::now() < deadline, "the search never settled");
    }
    let titles: Vec<&str> = search
        .results()
        .iter()
        .map(|hit| hit.title.as_str())
        .collect();
    assert_eq!(
        titles,
        ["voice"],
        "the app's own conversation is found in its window: {titles:?}"
    );
}
