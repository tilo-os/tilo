//! The query a canvas place shows while an app answers it: the state it settles in from the app's
//! own search invocation result, and the resource and actions a place stands as (shell.md §8).

use tilo_actions::{ResourceSpec, ResourceViewSpec};

use super::published::Query;
use super::{Item, LayoutQueryReading, LayoutRefReading, QUERY_KIND, SearchOutcome, Target, Words};

pub(super) fn query_state_from_outcome(outcome: SearchOutcome) -> (String, String) {
    match outcome {
        Ok(()) => ("searching".to_owned(), String::new()),
        Err(reason) => ("failed".to_owned(), reason),
    }
}

pub(super) fn settle(state: &str, matches: &[LayoutRefReading]) -> (String, String) {
    let kind = matches
        .first()
        .map(|one| one.kind.clone())
        .unwrap_or_default();
    // A failure keeps its own state whatever it found (nothing, by definition): the app said it
    // could not answer, which is not the same as a search that matched nothing.
    if state == "failed" {
        return ("failed".to_owned(), kind);
    }
    let state = match matches.len() {
        0 => "none",
        1 => "one",
        _ => "several",
    };
    (state.to_owned(), kind)
}

/// The query `query` publishes as a resource of its own, through the view a place shows it by
/// (`query_view`): what a place stands as until its app answers, and what several or no matches
/// leave as they are (shell.md §8).
pub(super) fn query_resource(query: &LayoutQueryReading, _words: &Words) -> ResourceSpec {
    tilo_resource::LiveRow::typed::<Query>(Item::Query.resource(&query.key), query.text.clone())
        .view(ResourceViewSpec::new("page", query.text.clone(), "open"))
        .into_resource()
        // A query is what a place stands as while its app answers: the Shell's search, as
        // search and a script have always read it.
        .with_type(tilo_actions::ResourceType {
            noun: tilo_actions::Noun::new("search", "searches"),
            app: tilo_actions::Noun::new("Shell", "Shell"),
            root: false,
        })
}

/// What a place showing a query reads it as, in the catalogue: the app the shell draws it with,
/// its one `page` view, and the words a search goes by.
pub(super) fn query_target(query: &LayoutQueryReading) -> Target {
    Target {
        app: "so.tilo.shell".to_owned(),
        views: vec!["page".to_owned()],
        title: query.text.clone(),
        kind: QUERY_KIND.to_owned(),
        ..Target::default()
    }
}
