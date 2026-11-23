//! A feed's articles listed through the action API, from the parts its chain
//! keeps them in.
//!
//! The feed keeps its articles in numbered parts beside its own small resource.
//! An article landed through the action API the host serves is written into the
//! chain's head part, and the API lists it under the chain at once, with the
//! actions a member runs on it.

use tilo_actions::MemberId;
use tilo_actions::api::ActionAddress;
use tilo_agent_session::{Replica, ReplicaActor, Seed};

/// The subscription the articles come from.
const FEED: &str = "feed-1";

/// The address the feed publishes its articles under.
fn articles() -> String {
    ui::resource::chain_resource_id(&feeds::article_list::chain())
}

/// A feed with its own record.
fn founded() -> Replica {
    crate::common::open(
        "so.tilo.feeds",
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap()
}

/// One article, as a refresh brings it.
fn fetched(at: usize) -> serde_json::Value {
    serde_json::json!({
        "title": format!("Article {at}"),
        "url": format!("https://example.test/{at}"),
        "summary": "The service keeps its work beside its peers. ".repeat(20),
    })
}

/// Land article `at` through the chain's own `receive`.
fn land(page: &mut Replica, at: usize) {
    page.invoke(
        &ActionAddress::new(articles(), "receive"),
        serde_json::json!({"feed": FEED, "articles": [fetched(at)]})
            .as_object()
            .unwrap(),
        false,
    )
    .expect("the article lands");
}

#[test]
fn an_article_landed_through_the_api_is_listed_with_what_runs_on_it() {
    let mut page = founded();
    land(&mut page, 0);
    let surveyed = page.survey_rows();
    let under: Vec<String> = surveyed
        .iter()
        .map(|row| {
            format!(
                "{} | {} | {:?}",
                row.parent_resource,
                row.resource.id,
                row.action.as_ref().map(|action| &action.address.action)
            )
        })
        .collect();
    let item_action = surveyed.iter().any(|row| {
        row.parent_resource == articles()
            && row
                .action
                .as_ref()
                .is_some_and(|action| action.address.action == "set-starred")
    });
    assert!(
        item_action,
        "the landed article is listed with its star: {under:#?}"
    );

    // A caller lists a level at a time, as a page and an agent's ls do: the
    // chain is a level with children, and its level lists the article.
    let mut under: Vec<Option<String>> = vec![None];
    let mut walked = Vec::new();
    let mut visited = std::collections::HashSet::new();
    while let Some(parent) = under.pop() {
        if !visited.insert(parent.clone()) {
            continue;
        }
        let query = tilo_actions::remote::CatalogueQuery {
            parent_resource: parent.clone(),
            max_results: 500,
            ..tilo_actions::remote::CatalogueQuery::default()
        };
        for row in page.survey_page(&query, (false, (0, 500))).1 {
            if row.has_children {
                under.push(Some(row.resource.id.clone()));
            }
            walked.push(format!(
                "{:?} | {} | {} | {:?}",
                parent,
                row.parent_resource,
                row.resource.id,
                row.action.as_ref().map(|action| &action.address.action)
            ));
        }
        assert!(walked.len() < 5_000, "the levels end");
    }
    assert!(
        walked
            .iter()
            .any(|row| row.contains(&format!("| {} |", articles()))
                && row.ends_with("Some(\"set-starred\")")),
        "a level at a time lists the landed article with its star: {walked:#?}"
    );
}
