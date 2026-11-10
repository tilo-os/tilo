#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! A verb marked `#[action(by_app)]` is the app's own bookkeeping of its
//! record: its code applies it, and no caller is offered it. It has no
//! declaration, so it is not published, nothing builds it by its id, and a
//! caller's value of it is refused through `apply` and `group`.

use tilo_resource::{Invoke, Model, Replica, Resource, resource};

/// The pages someone went to: the app notes each visit itself, and anyone
/// may star a page.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Visits {
    pub noted: Vec<String>,
    pub starred: Vec<String>,
}

#[resource(kind = "test.visits", label = "Visits")]
impl Visits {
    /// Note a visit.
    #[action(housekeeping, by_app)]
    pub fn note(&mut self, page: String) {
        self.noted.push(page);
    }

    /// Star a page.
    #[action]
    pub fn star(&mut self, page: String) {
        self.starred.push(page);
    }
}

/// The visit to `page`, as the app's own code builds it.
fn noting(page: &str) -> <Visits as Resource>::Action {
    Visits::NOTE
        .with(page)
        .build(0)
        .expect("the app builds what it keeps itself")
}

#[test]
fn a_by_app_verb_is_declared_and_published_to_no_caller() {
    let declared: Vec<&str> = Visits::declarations().iter().map(|decl| decl.id).collect();
    assert_eq!(declared, ["star"]);

    let published = Replica::<Visits>::new().published().unwrap();
    let offered: Vec<String> = published
        .actions
        .iter()
        .map(|spec| spec.id.clone())
        .collect();
    assert!(offered.iter().any(|id| id == "star"), "{offered:?}");
    assert!(!offered.iter().any(|id| id == "note"), "{offered:?}");

    // What an app that is not running is known by names it to nobody
    // either, and nor does the value `apply` takes.
    let surface = Visits::surface();
    let verbs: Vec<&str> = surface.verbs.iter().map(|verb| verb.id.as_str()).collect();
    assert_eq!(verbs, ["star"]);
    let schema = Visits::action_schema().unwrap().to_string();
    assert!(schema.contains("\"star\""), "{schema}");
    assert!(!schema.contains("\"note\""), "{schema}");
}

#[test]
fn a_caller_invokes_a_by_app_verb_by_no_name_value_or_group() {
    let mut replica = Replica::<Visits>::new().as_agent_for("agent:eve", "ada");

    // By its id, the door finds no such action.
    let named = replica.apply(Visits::NOTE.with("forged")).map(drop);
    assert!(
        matches!(named, Err(tilo_actions::DoorError::Unknown(_))),
        "{named:?}"
    );
    // As a value of the generic `apply`, confirmed or not.
    let value = replica.apply_action(&noting("forged")).map(drop);
    assert!(value.is_err(), "a value of it is refused");
    // As a member of a group, which then applies none of its members.
    let grouped = replica
        .apply(Invoke::group([
            Visits::STAR.with("beside"),
            Visits::NOTE.with("forged"),
        ]))
        .map(drop);
    assert!(grouped.is_err(), "a group holding it is refused");
    let state = replica.state().unwrap();
    assert!(state.noted.is_empty(), "{state:?}");
    assert!(state.starred.is_empty(), "{state:?}");

    // The same caller invokes what is declared.
    replica.apply(Visits::STAR.with("tilo.so")).unwrap();
    assert_eq!(replica.state().unwrap().starred, ["tilo.so"]);
}

#[test]
fn the_apps_own_code_builds_and_applies_a_by_app_verb() {
    let mut replica = Replica::<Visits>::new();
    let action = noting("tilo.so");
    assert!(Visits::by_app(&action));
    assert!(!Visits::by_app(
        &Visits::STAR.with("tilo.so").build(0).unwrap()
    ));
    // It is still the change it says it is.
    assert_eq!(
        tilo_actions::Action::change_kind(&action),
        tilo_actions::ChangeKind::Housekeeping
    );
    tilo_actions::Action::apply(&action, replica.doc_mut()).unwrap();
    let state = replica.state().unwrap();
    assert_eq!(state.noted, ["tilo.so"]);
    assert!(state.starred.is_empty());
}
