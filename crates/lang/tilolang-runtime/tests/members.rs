//! Who is in the workspace, end to end: `workspace.members`, `members(r)`, `p.kind()` and
//! `p.role()` read what the host says the membership is, follow it live, and feed a rich
//! input's `@` drawer.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use std::rc::Rc;

    use super::common::*;
    use tilolang_runtime::members::Member;
    use tilolang_runtime::{AmStore, Principal, ResourceRef, Runtime, SimpleAuthority, Value};

    const SOURCE: &str = r#"app so.tilo.workspaces "Workspaces"

type Workspace {
    topic: Text
    lines: Collection<Line>
}

type Line {
    text: Text
    author: Principal
}

default view workspace for Workspace

action say(r: Workspace, text: Text) {
    r.lines.insert(Line(text: text, author: invoker))
}

fn kind_of(p: Principal) -> Text {
    if p.kind() == .agent { "agent" } else { "person" }
}

view workspace(r: Workspace) {
    state draft: RichText = RichText("")
    page {
        for p in workspace.members {
            text("in {p.name()} {kind_of(p)} {p.role() ?? "no role"}")
        }
        for p in members(r) {
            text("reads {p.name()}")
        }
        rich_input(bind: draft, speakers: speakers(r.lines))
    }
}

/// The same conversation's people, at most the two newest: a view says how many it offers.
view two(r: Workspace) {
    state draft: RichText = RichText("")
    page {
        rich_input(bind: draft, speakers: speakers(r.lines, 2))
    }
}
"#;

    fn ada() -> Principal {
        Principal::new("ada")
    }

    fn bo() -> Principal {
        Principal::new("bo")
    }

    fn planner() -> Principal {
        Principal::new("agent:planner")
    }

    /// Two people and an agent, as a workspace's membership records them.
    fn membership() -> Vec<Member> {
        vec![
            Member::person("ada", "owner"),
            Member::person("bo", "editor"),
            Member::agent("agent:planner"),
        ]
    }

    fn setup() -> (Runtime, App, ResourceRef, ResourceRef) {
        let app = App::compile(SOURCE, &[]);
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        let open = rt.create("Workspace").unwrap();
        let private = rt.create("Workspace").unwrap();
        (rt, app, open, private)
    }

    fn texts(v: &tilolang_runtime::ViewInstance, prefix: &str) -> Vec<String> {
        of_kind(v.tree(), "ui.Text")
            .into_iter()
            .map(|t| text_prop(t, "value"))
            .filter(|t| t.starts_with(prefix))
            .collect()
    }

    fn people(v: &tilolang_runtime::ViewInstance, prop: &str) -> Vec<String> {
        let inputs = of_kind(v.tree(), "ui.RichInput");
        let input = inputs.first().expect("the view has its rich input");
        match input.props.get(prop) {
            Some(Value::List(items)) => items
                .iter()
                .map(|item| match item {
                    Value::Principal(who) => who.0.clone(),
                    other => panic!("{prop} holds {other:?}"),
                })
                .collect(),
            other => panic!("{prop} is {other:?}"),
        }
    }

    #[test]
    fn the_workspaces_members_are_its_people_and_agents_with_their_kind_and_role() {
        let (rt, app, open, _) = setup();
        rt.set_members(membership(), None);
        let v = rt
            .open_view(&ada(), &app.view("workspace"), vec![Value::resource(&open)])
            .unwrap();
        assert_eq!(
            texts(&v, "in "),
            [
                "in ada person owner",
                "in bo person editor",
                "in agent:planner agent no role",
            ]
        );
        assert_eq!(
            texts(&v, "reads "),
            ["reads ada", "reads bo", "reads agent:planner"],
            "without grants that narrow it, every member reads the workspace's resources"
        );
    }

    #[test]
    fn members_of_a_resource_are_those_its_grants_let_read_it() {
        let (rt, app, open, private) = setup();
        let hidden = private.clone();
        rt.set_members(
            membership(),
            Some(Rc::new(move |who: &Principal, r: &ResourceRef| {
                *r != hidden || who.0 == "ada"
            })),
        );
        let on = |r: &ResourceRef| {
            rt.open_view(&ada(), &app.view("workspace"), vec![Value::resource(r)])
                .unwrap()
        };
        assert_eq!(texts(&on(&open), "reads ").len(), 3);
        assert_eq!(texts(&on(&private), "reads "), ["reads ada"]);
        assert_eq!(
            people(&on(&private), "people"),
            ["ada"],
            "the drawer offers only who can read what the mention lands in"
        );
    }

    #[test]
    fn a_view_follows_membership_as_it_changes() {
        let (rt, app, open, _) = setup();
        rt.set_members(membership()[..1].to_vec(), None);
        let mut v = rt
            .open_view(&ada(), &app.view("workspace"), vec![Value::resource(&open)])
            .unwrap();
        assert_eq!(texts(&v, "in "), ["in ada person owner"]);
        rt.set_members(membership(), None);
        v.poll_patches().unwrap();
        assert_eq!(texts(&v, "in ").len(), 3, "a member admitted shows at once");
        assert_eq!(people(&v, "people"), ["ada", "bo", "agent:planner"]);
        let mut changed = membership();
        changed.remove(1);
        if let Some(ada) = changed.first_mut() {
            ada.role = Some("editor".to_owned());
        }
        rt.set_members(changed, None);
        v.poll_patches().unwrap();
        assert_eq!(
            texts(&v, "in "),
            ["in ada person editor", "in agent:planner agent no role"],
            "a removal and a role change show at once"
        );
    }

    #[test]
    fn the_drawer_offers_the_conversations_people_newest_first() {
        let (rt, app, open, _) = setup();
        rt.set_members(membership(), None);
        let say = app.action("say");
        let mut v = rt
            .open_view(&ada(), &app.view("workspace"), vec![Value::resource(&open)])
            .unwrap();
        for who in [bo(), planner(), ada(), bo()] {
            rt.invoke_action(
                &who,
                (
                    say.as_str(),
                    vec![Value::resource(&open), Value::text("hello")],
                ),
            )
            .unwrap();
        }
        v.poll_patches().unwrap();
        let speakers = people(&v, "speakers");
        assert_eq!(
            speakers.get(..3),
            Some(
                &[
                    "bo".to_owned(),
                    "ada".to_owned(),
                    "agent:planner".to_owned()
                ][..]
            ),
            "newest first, each once: {speakers:?}"
        );
        assert_eq!(
            people(&v, "people"),
            ["ada", "bo", "agent:planner"],
            "and the members under them"
        );
    }

    #[test]
    fn the_drawer_offers_as_many_people_as_the_view_asks_for() {
        let (rt, app, open, _) = setup();
        rt.set_members(membership(), None);
        let say = app.action("say");
        for who in [ada(), bo(), planner()] {
            rt.invoke_action(
                &who,
                (
                    say.as_str(),
                    vec![Value::resource(&open), Value::text("hello")],
                ),
            )
            .unwrap();
        }
        let v = rt
            .open_view(&ada(), &app.view("two"), vec![Value::resource(&open)])
            .unwrap();
        assert_eq!(
            people(&v, "speakers"),
            ["agent:planner", "bo"],
            "the two newest, though three spoke"
        );
    }

    #[test]
    fn who_spoke_is_read_from_a_bounded_window_of_the_conversations_items() {
        let (rt, app, open, _) = setup();
        rt.set_members(membership(), None);
        let say = app.action("say");
        let mut v = rt
            .open_view(&ada(), &app.view("workspace"), vec![Value::resource(&open)])
            .unwrap();
        rt.invoke_action(
            &bo(),
            (
                say.as_str(),
                vec![Value::resource(&open), Value::text("the first word")],
            ),
        )
        .unwrap();
        for _ in 0..60 {
            rt.invoke_action(
                &ada(),
                (
                    say.as_str(),
                    vec![Value::resource(&open), Value::text("and on")],
                ),
            )
            .unwrap();
        }
        v.poll_patches().unwrap();
        assert_eq!(
            people(&v, "speakers"),
            ["ada"],
            "who spoke longer ago than the last items is a member like any other: the drawer's \
             cost is the window, never the length of the conversation"
        );
    }
}
