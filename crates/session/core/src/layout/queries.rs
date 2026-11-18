//! The query a canvas place shows while an app answers it: the state it settles in from the app's
//! own search invocation result, and the resource and actions a place stands as (shell.md §8).

use tilo_actions::{ActionSpec, Noun, ParamType, ResourceSpec, ResourceType, ResourceViewSpec};

use super::published::{action, count_of, required, text};
use super::{
    Item, LayoutAction, LayoutQueryReading, LayoutRefReading, QUERY_KIND, SearchOutcome, Target,
    Words,
};

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
pub(super) fn query_resource(query: &LayoutQueryReading, words: &Words) -> ResourceSpec {
    query_actions(&query.key, words).into_iter().fold(
        ResourceSpec::new(
            Item::Query.resource(&query.key),
            QUERY_KIND,
            query.text.clone(),
        )
        .with_view(ResourceViewSpec::new("page", query.text.clone(), "open"))
        .with_type(ResourceType {
            noun: Noun::new("search", "searches"),
            app: Noun::new("Shell", "Shell"),
            root: false,
        }),
        ResourceSpec::with_action,
    )
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

/// A query's own actions, its place's id bound into each: a press on a row shows that match in the
/// same place (`Show`), and Retry asks the app again.
pub(super) fn query_actions(key: &str, words: &Words) -> Vec<ActionSpec> {
    let shown = key.to_owned();
    vec![
        action(
            words,
            "show-match",
            "Show a match of the search in this place",
            vec![required(
                words,
                "show-match",
                "resource",
                ParamType::Resource,
                "The resource the row names",
            )],
            move |args| LayoutAction::Show {
                place: count_of(&shown),
                resource: text(args, "resource"),
                view: String::new(),
            },
        ),
        action(words, "retry", "Look for it again", Vec::new(), {
            let retried = key.to_owned();
            move |_| LayoutAction::RetryQuery {
                place: count_of(&retried),
            }
        }),
    ]
}
