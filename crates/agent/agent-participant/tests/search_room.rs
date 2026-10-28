//! `search_room` through `ModelPlanner` with a scripted stand-in for the
//! model: it searches the room's records by what they hold, then plans to
//! open the record the search found, by the id the answer showed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::{Arc, Mutex};

use tilo_agent::surface::Catalogue;
use tilo_agent::tools::{Answer, Revealed};
use tilo_agent_participant::search::{self, SearchDoc};
use tilo_agent_participant::{
    Briefing, ModelPlanner, PlanContext, PlanModel, Planner, ReadView, StepAction,
};
use tilo_browser_peer::DocumentRoute;

const SLIDES: &str = "so.tilo.slides";
const WRITER: &str = "so.tilo.word-processor";
const MAIL: &str = "so.tilo.mail";

/// Searches for the words it was built with, then plans to open the first
/// record the answer offers to open, the rest left to `then`.
struct Searches {
    query: &'static str,
    answered: Arc<Mutex<Option<String>>>,
}

impl Searches {
    fn new(query: &'static str) -> (Self, Arc<Mutex<Option<String>>>) {
        let answered = Arc::new(Mutex::new(None));
        (
            Self {
                query,
                answered: Arc::clone(&answered),
            },
            answered,
        )
    }
}

impl PlanModel for Searches {
    fn choose(&mut self, _: &str, _: &Catalogue, _: usize) -> Result<Answer, String> {
        Err("this model plans only when briefed".into())
    }

    fn choose_briefed(
        &mut self,
        _: &str,
        briefing: &Briefing,
        _: &Catalogue,
        _: usize,
        _: &mut ReadView<'_>,
    ) -> Result<Answer, String> {
        let mut revealed = Revealed::new();
        let found = search::answer(
            &serde_json::json!({"query": self.query}),
            &briefing.searchable,
            &mut revealed,
        );
        let open = found
            .split_whitespace()
            .find(|word| word.starts_with("record.") && word.ends_with("#open"))
            .ok_or_else(|| format!("nothing to open in {found}"))?
            .to_owned();
        if !revealed.contains(&open) {
            return Err(format!("{open} was shown but not revealed"));
        }
        *self.answered.lock().unwrap() = Some(found);
        Ok(Answer {
            actions: vec![open],
            arguments: vec![serde_json::Map::new()],
            why: String::new(),
            then: Some("make the deck from it".into()),
            reply: None,
            wait: None,
            ..Answer::default()
        })
    }
}

fn route(app: &str, record: &str) -> DocumentRoute {
    DocumentRoute {
        app_id: app.into(),
        cluster_id: "room".into(),
        record: record.into(),
    }
}

fn doc(app: &str, record: &str, title: Option<&str>, text: &str, changed: u64) -> SearchDoc {
    SearchDoc {
        route: route(app, record),
        title: title.map(str::to_owned),
        text: text.into(),
        changed: Some(changed),
        open: false,
    }
}

/// A room where the brief for the Q3 partner review is one of many
/// documents and not named by its title: a stale version, a decoy sharing
/// its words, a mail thread about it and filler.
fn room() -> PlanContext {
    let mut searchable = vec![
        doc(
            WRITER,
            "uuid:brief-v1",
            Some("Partner brief v1"),
            "Q3 partner review: draft agenda. Superseded by v2.",
            2,
        ),
        doc(
            WRITER,
            "uuid:review-checklist",
            Some("Review checklist"),
            "Code review: partner API changes need two approvals.",
            9,
        ),
        doc(
            MAIL,
            "uuid:inbox",
            None,
            "From: Dana\nSubject: deck\nPlease make slides from the partner brief v2.",
            11,
        ),
        doc(
            WRITER,
            "uuid:brief-v2",
            Some("Partner brief v2"),
            "Q3 partner review with Northwind, final agenda: revenue share, joint roadmap, \
             renewal terms.",
            10,
        ),
    ];
    for at in 0..20 {
        searchable.push(doc(
            WRITER,
            &format!("uuid:filler-{at}"),
            Some(&format!("Weekly sync {at}")),
            "Weekly sync: action items and owners.",
            at,
        ));
    }
    PlanContext {
        known: searchable
            .iter()
            .map(|doc| (doc.route.clone(), doc.title.clone()))
            .collect(),
        apps: vec![SLIDES.into(), WRITER.into(), MAIL.into()],
        searchable,
        room: "room".into(),
        ..PlanContext::default()
    }
}

#[test]
fn a_record_found_by_its_text_is_opened_by_the_plan() {
    let context = room();
    let (model, answered) = Searches::new("Q3 partner review final agenda");
    let steps = ModelPlanner::new(Box::new(model))
        .plan("make a deck for the Q3 partner review", &context)
        .unwrap();
    let answered = answered.lock().unwrap().clone().unwrap();
    assert!(
        answered.contains("1. so.tilo.word-processor \"Partner brief v2\" (record uuid:brief-v2"),
        "the current brief first: {answered}"
    );
    assert!(answered.contains("Northwind"), "{answered}");
    assert!(!answered.contains("Weekly sync"), "filler is not found");
    assert_eq!(steps[0].app, WRITER);
    assert_eq!(steps[0].record.as_deref(), Some("uuid:brief-v2"));
    assert_eq!(steps[0].action, StepAction::Open);
    assert!(matches!(&steps[1].action, StepAction::Continue(rest) if rest.contains("deck")));
}

#[test]
fn a_mail_thread_is_searched_like_any_record() {
    let context = room();
    let (model, _) = Searches::new("Dana deck");
    let steps = ModelPlanner::new(Box::new(model))
        .plan("do what Dana asked", &context)
        .unwrap();
    assert_eq!(steps[0].app, MAIL);
    assert_eq!(steps[0].record.as_deref(), Some("uuid:inbox"));
}
