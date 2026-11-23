//! A type that says who reads its records: each record goes to a document sealed to exactly its
//! readers, which reaches every device of the workspace and opens only on theirs. Another member
//! lists, finds, counts and reads none of it; a reader's other device has it all; and a record
//! whose readers change moves to the new ones, but never back from every member.

use tilo_resource_store::wire::{Command, Reply};
use tilo_resource_store::{Filter, Query};
use tilolang_runtime::{Actor, ResourceRef, Value};
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::publish::Caller;
use tilolang_tilo::runtime::ids::MemberId;

use crate::fixtures::{Replica, Workspace, action_id, target};

const DESK: &str = r#"app test.desk "Desk"

type Desk {
  drafts: Collection<Draft>
  invites: Collection<Invite>
}

type Draft (readers: author until sent) {
  subject: Text
  sent: Bool = false
}

type Invite (readers: guests) {
  subject: Text
  guests: Set<Actor>
}

root Desk

default view desk for Desk

view desk(d: Desk) {
  std.page(d) {
    for draft in d.drafts {
      ui.Text(draft.subject)
    }
  }
}

action write(d: Desk, subject: Text) -> Draft {
  return d.drafts.insert(Draft(subject: subject))
}

action retitle(draft: Draft, subject: Text) {
  draft.subject = subject
}

action send(draft: Draft, sent: Bool) {
  draft.sent = sent
}

action invite(d: Desk, subject: Text, host: Actor, guest: Actor) -> Invite {
  let invite = d.invites.insert(Invite(subject: subject))
  invite.guests.add(host)
  invite.guests.add(guest)
  return invite
}
"#;

const APP: &str = "test.desk";

/// What `replica`'s store answers of the records of `kind`: how many it lists, how many it counts,
/// how many mention `word`, and whether it reads `record`.
fn found(
    replica: &Replica,
    kind: &str,
    word: &str,
    record: &ResourceRef,
) -> (usize, usize, usize, bool) {
    let page = |query| match replica.doc.command(Command::Query(query)) {
        Reply::Page(page) => page.resources.len(),
        other => panic!("a query answered {other:?}"),
    };
    let counted = match replica.doc.command(Command::Count {
        kind: kind.to_owned(),
        filters: Vec::new(),
    }) {
        Reply::Count(count) => count,
        other => panic!("a count answered {other:?}"),
    };
    let read = match replica.doc.command(Command::Get {
        kind: kind.to_owned(),
        id: record.id_string(),
    }) {
        Reply::Resource(found) => found.is_some(),
        other => panic!("a read answered {other:?}"),
    };
    let search = Filter::Contains("subject".to_owned(), word.to_owned());
    (
        page(Query::of(kind, 10)),
        counted,
        page(Query::of(kind, 10).filter(search)),
        read,
    )
}

/// The subjects of the drafts on `replica`'s desk, as its runtime lists them.
fn subjects(replica: &Replica, desk: &ResourceRef) -> Vec<String> {
    let Value::List(drafts) = replica.read(desk, "drafts") else {
        panic!("a desk's drafts are a list");
    };
    drafts
        .iter()
        .map(|draft| match replica.read(&target(draft), "subject") {
            Value::Text(subject) => subject,
            other => panic!("a subject is text, got {other:?}"),
        })
        .collect()
}

fn person(name: &str) -> Actor {
    Actor::from(MemberId::person(name))
}

/// The caller an agent of `person` works as, through the app's published action API.
fn agent(person: Option<&str>) -> Caller {
    Caller {
        actor: Actor::from(MemberId::agent("helper")),
        agent: true,
        on_behalf_of: person.map(str::to_owned),
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one walk of a record across four devices"
)]
fn a_record_reaches_the_devices_of_its_readers_and_no_other_members() {
    let (ada, packages) = Replica::new("ada", Workspace::new(), &[(DESK, "")]);
    let meta = packages[0].meta.clone();
    let ids = packages[0].ids.clone();
    let desk = ada.root(APP);
    // Every device runs the same build of the package: the ids its first compile locked.
    let [laptop, ben, cy] = ["ada", "ben", "cy"].map(|name| {
        let replica = Replica::new(name, Workspace::new(), &[(DESK, &ids)]).0;
        ada.send_to(&replica);
        replica
    });
    // Ada acts, and her writes reach every device of the workspace, as sync carries them.
    let act = |action: &str, args: Vec<Value>| {
        let outcome = ada
            .host
            .runtime()
            .invoke_action(&person("ada"), (action_id(&meta, action).as_str(), args))
            .unwrap();
        ada.commit();
        for other in [&laptop, &ben, &cy] {
            ada.send_to(other);
        }
        match outcome {
            tilolang_runtime::InvokeOutcome::Done(Value::Ref(made)) => Some((*made.target).clone()),
            _ => None,
        }
    };

    let kind = meta.kind_of("Draft");
    let draft = act(
        "write",
        vec![Value::resource(&desk), Value::text("Quarterly numbers")],
    )
    .unwrap();
    assert_eq!(
        found(&ben, &kind, "quarterly", &draft),
        (0, 0, 0, false),
        "another member lists, counts, finds and reads none of Ada's draft"
    );
    assert!(
        subjects(&ben, &desk).is_empty(),
        "Ben's desk shows no draft of Ada's"
    );
    assert_eq!(
        found(&laptop, &kind, "quarterly", &draft),
        (1, 1, 1, true),
        "Ada's other device has her draft"
    );
    assert_eq!(subjects(&laptop, &desk), ["Quarterly numbers"]);

    // Her edit changes the draft where it is, sealed to her alone.
    act(
        "retitle",
        vec![
            Value::resource(&draft),
            Value::text("Quarterly numbers, final"),
        ],
    );
    assert_eq!(subjects(&laptop, &desk), ["Quarterly numbers, final"]);
    assert_eq!(
        found(&ben, &kind, "final", &draft),
        (0, 0, 0, false),
        "an edit does not hand the draft to every member"
    );

    // Sent, it is every member's, and it stays so: taking it back is refused.
    act("send", vec![Value::resource(&draft), Value::Bool(true)]);
    assert_eq!(
        found(&ben, &kind, "final", &draft),
        (1, 1, 1, true),
        "a sent draft is every member's"
    );
    assert_eq!(subjects(&ben, &desk), ["Quarterly numbers, final"]);
    let taken_back = ada.host.runtime().invoke_action(
        &person("ada"),
        (
            action_id(&meta, "send").as_str(),
            vec![Value::resource(&draft), Value::Bool(false)],
        ),
    );
    let refused = ada
        .host
        .store()
        .take_refusal()
        .or_else(|| taken_back.err().map(|error| error.to_string()));
    assert!(
        refused
            .as_ref()
            .is_some_and(|refused| refused.contains("stays so")),
        "a draft handed to every member is not taken back, got {refused:?}"
    );
    for other in [&laptop, &ben] {
        ada.send_to(other);
        assert_eq!(found(other, &kind, "final", &draft), (1, 1, 1, true));
    }

    // An invite is read by the people it names, Ada and Ben, and not by Cy.
    let kind = meta.kind_of("Invite");
    let invite = act(
        "invite",
        vec![
            Value::resource(&desk),
            Value::text("Dinner"),
            Value::Actor(person("ada")),
            Value::Actor(person("ben")),
        ],
    )
    .unwrap();
    assert_eq!(
        found(&ben, &kind, "dinner", &invite),
        (1, 1, 1, true),
        "the guest reads it"
    );
    assert_eq!(
        found(&laptop, &kind, "dinner", &invite),
        (1, 1, 1, true),
        "so does its host"
    );
    assert_eq!(
        found(&cy, &kind, "dinner", &invite),
        (0, 0, 0, false),
        "who it does not name does not"
    );
}

#[test]
fn a_record_an_agent_makes_is_the_person_it_acts_for() {
    let (ada, packages) = Replica::new("ada", Workspace::new(), &[(DESK, "")]);
    let meta = packages[0].meta.clone();
    let ids = packages[0].ids.clone();
    let desk = ada.root(APP);
    let [laptop, ben] = ["ada", "ben"].map(|name| {
        let replica = Replica::new(name, Workspace::new(), &[(DESK, &ids)]).0;
        ada.send_to(&replica);
        replica
    });
    // The action runs where the API publishes it, as an agent of Ada's.
    let draft_for = |caller: Caller| {
        let args = serde_json::json!({ "subject": "Drafted for Ada" });
        ada.carry(
            APP,
            &meta.kind_of("Desk"),
            &action_id(&meta, "write"),
            Some(&tilolang_tilo::publish::resource_id(APP, &desk.id_string())),
            args.as_object().expect("an object"),
            Some(&caller),
            None,
        )
    };
    draft_for(agent(Some("ada"))).expect("an agent drafts for the person it acts for");
    let made = ada.host.store().take_made();
    let [draft] = made.as_slice() else {
        panic!("the action made one draft: {made:?}");
    };
    ada.commit();
    for other in [&laptop, &ben] {
        ada.send_to(other);
    }
    let kind = meta.kind_of("Draft");
    assert_eq!(
        found(&laptop, &kind, "drafted", draft),
        (1, 1, 1, true),
        "the person's other device reads what their agent made for them"
    );
    assert_eq!(
        found(&ben, &kind, "drafted", draft),
        (0, 0, 0, false),
        "another member lists, counts, finds and reads none of it"
    );
    assert!(
        subjects(&ben, &desk).is_empty(),
        "Ben's desk shows no draft an agent made for Ada"
    );

    // An agent acting for nobody is no person's record: the store refuses it.
    let refused = draft_for(agent(None))
        .err()
        .or_else(|| ada.host.store().take_refusal())
        .expect("an agent that acts for nobody makes no record under a reader rule");
    assert!(refused.contains("no person made it"), "{refused}");
}
