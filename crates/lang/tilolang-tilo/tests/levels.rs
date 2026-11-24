//! A level of what a workspace publishes, listed a page at a time, is what the whole publication
//! lists there: the same rows in the same order, each saying whether its resource holds more,
//! however the pages fall. Only the page's resources are published for it, so a caller listing a
//! record of thousands a level at a time pays for each page's rows, not the record.

use std::rc::Rc;
use std::sync::Arc;

use tilo_actions::api::{PlacedAction, survey_placed};
use tilolang_runtime::Actor;
use tilolang_tilo::host::Host;
use tilolang_tilo::publish::{PlanRequest, Planner, Publications, resource_id};

use crate::fixtures::{Replica, Workspace};

const CHAT: (&str, &str) = (
    include_str!("../../../../apps/tl/chat/app.tl"),
    include_str!("../../../../apps/tl/chat/app.ids"),
);
const APP: &str = "so.tilo.chat";

/// The page sizes listed: one row, a size that splits a resource's rows, and the CLI's.
const PAGES: [usize; 3] = [1, 7, 40];

struct Refusing;

impl Planner for Refusing {
    fn plan(&self, _: &PlanRequest<'_>) -> Result<tilolang_tilo::action::DynAction, String> {
        Err("nothing runs while this test publishes".into())
    }
}

/// The seeded Chat workspace: its channels and their messages, as its first scenario makes them,
/// and the published id of its root.
fn seeded_chat() -> (Replica, String) {
    let (ada, loaded) = Replica::new("ada", Workspace::new(), &[CHAT]);
    let meta = loaded.last().unwrap().meta.clone();
    let root = ada.host.root_of(APP).unwrap();
    ada.commit();
    let scenario = meta.scenarios.first().expect("Chat declares a scenario");
    ada.host
        .runtime()
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            (scenario.action.as_str(), vec![]),
        )
        .unwrap();
    ada.commit();
    (ada, resource_id(APP, &root.id_string()))
}

/// The rows the whole publication lists under `parent` (`""` for the top), in its order.
fn whole(host: &Host, planner: &Arc<dyn Planner>, parent: &str) -> Vec<PlacedAction> {
    let trees: Vec<_> = Publications::new(host)
        .everything(host, planner)
        .into_iter()
        .map(Rc::unwrap_or_clone)
        .collect();
    survey_placed(&trees)
        .into_iter()
        .filter(|row| row.parent == parent)
        .collect()
}

/// The pages `admit` admits of the level under `parent`, `size` rows a page, each listed on its
/// own as a caller asks for it: from the offset the pages before it reached.
fn pages(
    host: &Host,
    planner: &Arc<dyn Planner>,
    parent: Option<&str>,
    size: usize,
    every: bool,
    admit: &dyn Fn(&PlacedAction) -> bool,
) -> Vec<Vec<PlacedAction>> {
    let mut publications = Publications::new(host);
    let mut out = Vec::new();
    let mut offset = 0;
    loop {
        let level = publications.level(host, planner, parent);
        let (rows, more) = level.page(host, (offset, size), every, &mut |row| {
            admit(&row).then_some(row)
        });
        assert!(rows.len() <= size, "a page holds at most {size} rows");
        assert!(
            !more || rows.len() == size,
            "a page short of its size is the last"
        );
        offset += rows.len();
        out.push(rows);
        if !more {
            return out;
        }
    }
}

/// The level under `parent` listed at every page size equals the whole publication's rows there,
/// page by page.
fn lists_as_whole(host: &Host, planner: &Arc<dyn Planner>, parent: Option<&str>) {
    let expected = whole(host, planner, parent.unwrap_or(""));
    assert!(
        !expected.is_empty(),
        "the level under {parent:?} holds rows"
    );
    for size in PAGES {
        let listed = pages(host, planner, parent, size, true, &|_| true);
        for (at, page) in listed.iter().enumerate() {
            let from = (at * size).min(expected.len());
            let to = (from + size).min(expected.len());
            assert_eq!(
                page,
                &expected[from..to],
                "page {at} of {size} under {parent:?}"
            );
        }
        assert_eq!(
            listed.concat().len(),
            expected.len(),
            "every row under {parent:?} at {size} a page"
        );

        // Rows narrowed by what they say are counted among the ones admitted, from the level's
        // start, and land on the same pages.
        let opens = |row: &PlacedAction| row.action.address.action.contains("open");
        let narrowed: Vec<PlacedAction> =
            expected.iter().filter(|row| opens(row)).cloned().collect();
        let listed = pages(host, planner, parent, size, false, &opens);
        assert_eq!(
            listed.concat(),
            narrowed,
            "the rows under {parent:?} that open, at {size} a page"
        );
    }
}

#[test]
fn a_page_of_a_level_lists_what_the_whole_publication_lists_there() {
    let (replica, root) = seeded_chat();
    let host = &replica.host;
    let planner: Arc<dyn Planner> = Arc::new(Refusing);

    // The top: the workspace's documents.
    lists_as_whole(host, &planner, None);

    // Under the root: its channels, each holding messages.
    let channels = whole(host, &planner, &root);
    let holding: Vec<&PlacedAction> = channels.iter().filter(|row| row.has_children).collect();
    assert!(
        !holding.is_empty(),
        "a channel under the root holds messages"
    );
    lists_as_whole(host, &planner, Some(&root));

    // A level further down: a channel's messages.
    let channel = holding[0].action.address.resource.clone();
    lists_as_whole(host, &planner, Some(&channel));

    // A resource the publication holds none as has nothing under it.
    let level = Publications::new(host).level(host, &planner, Some(&resource_id(APP, "nothing")));
    let (rows, more) = level.page(host, (0, 40), true, &mut |row: PlacedAction| Some(row));
    assert!(
        rows.is_empty() && !more,
        "nothing is listed under no resource"
    );
}
