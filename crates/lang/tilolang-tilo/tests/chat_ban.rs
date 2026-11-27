//! Chat's moderators ban a person from the workspace through the workspace's own `ban`, and
//! overturning the ban on the Reports page lifts it through its `unban`: Chat keeps no ban list,
//! only the decided report.

use std::cell::RefCell;
use std::rc::Rc;

use tilolang_runtime::delegate::Invocation;
use tilolang_runtime::{Actor, InvokeOutcome, UiNode, Value};

use crate::fixtures::{Replica, Sent, Workspace};

const CHAT: &str = include_str!("../../../../apps/tl/chat/app.tl");
const CHAT_IDS: &str = include_str!("../../../../apps/tl/chat/app.ids");
const APP: &str = "so.tilo.chat";

/// The workspace verbs sent, each with the person it names.
fn workspace_calls(sent: &Rc<RefCell<Vec<Invocation>>>) -> Vec<(String, Option<Value>)> {
    sent.borrow()
        .iter()
        .filter_map(|invocation| match invocation {
            Invocation::Action {
                app, name, args, ..
            } if app.as_str() == "tilo.workspace" => Some((
                name.clone(),
                args.iter()
                    .find(|(named, _)| named == "person")
                    .map(|(_, value)| value.clone()),
            )),
            _ => None,
        })
        .collect()
}

fn texts(node: &UiNode) -> Vec<String> {
    crate::fixtures::nodes(node)
        .into_iter()
        .flat_map(|node| node.props.values())
        .filter_map(|value| match value {
            Value::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn person(name: &str) -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person(name))
}

fn view_id(r: &Replica, name: &str) -> String {
    let meta = r.host.package(APP).unwrap().meta;
    meta.views
        .iter()
        .find(|view| view.name == name)
        .unwrap_or_else(|| panic!("no view {name}"))
        .id
        .clone()
}

/// The workspace as Chat's replica `name` reads it: ada owns it, ben and cy edit, mo moderates.
fn chat(name: &str) -> Replica {
    let (replica, _) = Replica::new(name, Workspace::new(), &[(CHAT, CHAT_IDS)]);
    crate::fixtures::stage_members(
        &replica.host,
        vec![
            tilolang_runtime::members::Member::person("ada", tilo_actions::policy::Role::Owner),
            tilolang_runtime::members::Member::person("ben", tilo_actions::policy::Role::Editor),
            tilolang_runtime::members::Member::person("cy", tilo_actions::policy::Role::Editor),
            tilolang_runtime::members::Member::person("mo", tilo_actions::policy::Role::Moderator),
        ],
        None,
    );
    replica
}

#[test]
fn a_moderator_bans_through_the_workspace_and_overturning_lets_them_back() {
    let ada = chat("ada");
    let root = ada.host.root_of(APP).unwrap();
    let all = Value::resource(&root);
    ada.commit();
    let sent = Rc::new(RefCell::new(Vec::new()));
    ada.host.runtime().set_invoker(Sent(Rc::clone(&sent)));
    let meta = ada.host.package(APP).unwrap().meta;
    let as_who = |who: &str, action: &str, args: Vec<Value>| {
        ada.host.runtime().invoke_action(
            &person(who),
            (crate::fixtures::action_id(&meta, action).as_str(), args),
        )
    };
    let done = |result: Result<InvokeOutcome, _>| match result {
        Ok(InvokeOutcome::Done(value)) => value,
        other => panic!("{other:?}"),
    };

    // The people page offers its moderators a Ban on everyone else.
    let people_view = view_id(&ada, "people");
    let mut people = ada.host.open_resource(&root, Some(&people_view)).unwrap();
    people.poll_patches().unwrap();
    let bans = people
        .tree()
        .of_kind("ui.Button")
        .into_iter()
        .filter(|button| texts(button).contains(&"Ban".to_owned()))
        .count();
    assert_eq!(
        bans,
        3,
        "ben, cy and mo, not ada herself: {:?}",
        texts(people.tree())
    );

    // Banning someone asks the workspace, naming the person; nobody bans themselves.
    assert!(
        format!(
            "{:?}",
            as_who(
                "mo",
                "ban",
                vec![all.clone(), Value::Actor(person("mo")), Value::text("x")]
            )
        )
        .contains("You can't ban yourself")
    );
    let banned = done(as_who(
        "mo",
        "ban",
        vec![
            all.clone(),
            Value::Actor(person("ben")),
            Value::text("Spam in every channel"),
        ],
    ));
    ada.commit();
    assert_eq!(
        workspace_calls(&sent),
        vec![("ban".to_owned(), Some(Value::Actor(person("ben"))))]
    );

    // A report about someone is settled by banning them from its row.
    let report = done(as_who(
        "ben",
        "report_person",
        vec![all, Value::Actor(person("cy")), Value::text("Threats")],
    ));
    ada.commit();
    done(as_who("mo", "ban_reported", vec![report.clone()]));
    ada.commit();
    assert_eq!(
        workspace_calls(&sent).last(),
        Some(&("ban".to_owned(), Some(Value::Actor(person("cy")))))
    );
    assert!(format!("{:?}", as_who("mo", "ban_reported", vec![report])).contains("banned already"));

    // Both stand on the moderators' Reports page as decided, and overturning the ban lifts it.
    let reports_view = view_id(&ada, "reports");
    let mut page = ada.host.open_resource(&root, Some(&reports_view)).unwrap();
    page.poll_patches().unwrap();
    let shown = texts(page.tree());
    assert_eq!(
        shown
            .iter()
            .filter(|text| *text == "Decided: Banned")
            .count(),
        2,
        "{shown:?}"
    );
    done(as_who(
        "mo",
        "overturn",
        vec![banned, Value::text("Talked it through")],
    ));
    ada.commit();
    assert_eq!(
        workspace_calls(&sent).last(),
        Some(&("unban".to_owned(), Some(Value::Actor(person("ben")))))
    );
}

#[test]
fn a_member_who_does_not_moderate_is_offered_no_ban() {
    let ben = chat("ben");
    let root = ben.host.root_of(APP).unwrap();
    ben.commit();
    let people_view = view_id(&ben, "people");
    let mut people = ben.host.open_resource(&root, Some(&people_view)).unwrap();
    people.poll_patches().unwrap();
    let shown = texts(people.tree());
    assert!(shown.contains(&"Message".to_owned()), "{shown:?}");
    assert!(!shown.contains(&"Ban".to_owned()), "{shown:?}");
}

/// A Shortcut's unattended run is the automation acting for its person, and Chat's checks of who
/// may do what read the run, not the person: a run of a moderator's Shortcut, saved by them or by
/// their agent, settles no report and edits none of their messages. What the run writes is still
/// addressed as theirs: it writes in their own conversation.
#[test]
fn a_moderators_shortcut_run_moderates_nothing_and_writes_in_their_conversation() {
    let ada = chat("ada");
    let root = ada.host.root_of(APP).unwrap();
    let all = Value::resource(&root);
    ada.commit();
    let meta = ada.host.package(APP).unwrap().meta;
    let invoke = |who: &Actor, action: &str, args: Vec<Value>| {
        let outcome = ada.host.runtime().invoke_action(
            who,
            (crate::fixtures::action_id(&meta, action).as_str(), args),
        );
        ada.commit();
        outcome
    };
    let done = |result: Result<InvokeOutcome, _>| match result {
        Ok(InvokeOutcome::Done(value)) => value,
        other => panic!("{other:?}"),
    };
    let mo = person("mo");
    let report = done(invoke(
        &person("ben"),
        "report_person",
        vec![
            all.clone(),
            Value::Actor(person("cy")),
            Value::text("Threats"),
        ],
    ));
    let written = done(invoke(
        &mo,
        "write_to",
        vec![
            all.clone(),
            Value::Actor(person("ben")),
            rich("See you at four"),
        ],
    ));

    for (saved_by, saver) in [
        ("mo", tilo_actions::remote::Caller::own_person("mo")),
        (
            "mo's agent",
            tilo_actions::remote::Caller::local_agent("scout", "mo"),
        ),
    ] {
        let caller = shortcut_run(&ada, saver, saved_by);
        let run = caller.actor.clone();
        let refuses = |action: &str, args: Vec<Value>, says: &str| {
            let refused = format!("{:?}", invoke(&run, action, args));
            assert!(
                refused.contains(says),
                "a run of a Shortcut {saved_by} saved may not {action}: {refused}"
            );
        };

        // It moderates nothing, and edits none of mo's messages.
        let settle = vec![report.clone(), Value::text("Warned")];
        refuses(
            "decide",
            settle,
            "Only the workspace's moderators settle a report",
        );
        let edit = vec![written.clone(), rich("See you at five")];
        refuses("edit", edit, "Only its author can edit a message");

        // What it writes to ben is in mo's conversation with him, the one mo opened, written
        // for mo as the host writes a run's call.
        let made = {
            let _for_mo = ada
                .host
                .store()
                .acting_for(caller.on_behalf_of.clone(), caller.acting_for());
            done(invoke(
                &run,
                "write_to",
                vec![
                    all.clone(),
                    Value::Actor(person("ben")),
                    rich("Time to call ben"),
                ],
            ))
        };
        // The message is mo's, written by the run: the run edits none of it, mo does.
        let edit = vec![made.clone(), rich("Time to call ben now")];
        refuses("edit", edit, "Only its author can edit a message");
        done(invoke(
            &mo,
            "edit",
            vec![made, rich("Time to call ben now")],
        ));
        let with_ben = |who: &Actor| {
            let ben = vec![all.clone(), Value::Actor(person("ben"))];
            done(invoke(who, "conversation_with", ben))
        };
        let (theirs, the_runs) = (with_ben(&mo), with_ben(&run));
        assert_eq!(
            theirs, the_runs,
            "a run of a Shortcut {saved_by} saved finds mo's conversation with ben"
        );
        assert_ne!(theirs, Value::none(), "mo has a conversation with ben");
    }
    // The moderator settles it themselves.
    done(invoke(&mo, "decide", vec![report, Value::text("Warned")]));
}

/// The run of a Shortcut `saver` saved, admitted to `r`'s host as its host admits one.
fn shortcut_run(
    r: &Replica,
    saver: tilo_actions::remote::Caller,
    saved_by: &str,
) -> tilolang_tilo::publish::Caller {
    let run = saver.running_shortcut(format!("reminder-{}", saved_by.len()));
    let caller =
        tilolang_tilo::publish::Caller::from(&tilo_actions::ActionInvoker::admitted(run.acting()));
    r.host.admit_agent(&caller);
    caller
}

fn rich(text: &str) -> Value {
    Value::Rich(tilolang_runtime::rich::RichValue::from_text(text))
}
