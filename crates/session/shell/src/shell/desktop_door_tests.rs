//! The desktop's layout actions driven through the broker, the door an
//! agent uses: an agent the room admitted makes eight named strips, moves a
//! window onto each, lays one out side by side, reads the windows back by
//! id, and takes the whole layout back in one undo. Each step is admitted
//! and recorded by the broker like any action; one an agent the room never
//! admitted asks is refused before anything changes.

use std::sync::Arc;

use serde_json::{Value, json};
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{Caller, InvokeArgument, InvokeRequest, RemoteActionTarget};
use tilo_desktop_model::commands::{RESOURCE, layout};

use crate::shell::overlay_host::ShellOverlayHost;
use crate::shell::state::{
    AppKind, GroupId, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
    WorkspaceTree,
};

const OWNER: &str = "owner";
const AGENT: &str = "planner";

struct Fixture {
    _home: tilo_test_env::TestHome,
    chrome: crate::shell::chrome_state::Chrome,
    host: ShellOverlayHost,
    broker: tilo_action_broker::collab_broker::AppActionBroker,
    room: String,
    /// The windows already on the desktop, in its first strip.
    windows: Vec<GroupId>,
}

/// A workspace whose first strip holds three windows, with an agent the
/// owner admitted to change it.
fn fixture() -> Fixture {
    let home = tilo_test_env::TestHome::new().unwrap();
    let mut workspace = Workspace::new(WorkspaceId(42), "Hiring");
    workspace.set_owner(OWNER);
    workspace.admit_agent_preset(
        AGENT,
        tilo_action_policy::roles::AgentPreset::Contributor,
        &tilo_action_policy::Subject::person(OWNER.to_owned()),
    );
    // The person lets it arrange their desktop, as Settings → Agents does.
    workspace.grant(tilo_action_policy::Grant::new(
        "planner-desktop",
        tilo_action_policy::Subject::agent(AGENT),
        tilo_action_policy::TreePath::root()
            .any(tilo_action_policy::Level::Room)
            .app(tilo_desktop_model::commands::APP_ID),
        tilo_action_policy::KindSet::READ_UNDOABLE,
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Subject::person(OWNER.to_owned()),
    ));
    let room = workspace.room().to_string();
    let mut tree = WorkspaceTree::default();
    tree.desktop.active = workspace.id;
    tree.desktop.workspaces.push(workspace);
    let main = crate::shell::state::StripId(tree.mint_id());
    tree.desktop.workspaces[0]
        .strips
        .push(crate::shell::state::Strip {
            id: main,
            name: "Main".into(),
            window_groups: Vec::new(),
        });
    let mut windows = Vec::new();
    for title in ["Mail", "CV", "Draft"] {
        let (group, tab) = (tree.mint_id(), tree.mint_id());
        windows.push(group);
        let strip = &mut tree.desktop.workspaces[0].strips[0];
        strip.window_groups.push(WindowGroup {
            id: group,
            tabs: vec![Tab {
                id: tab,
                title: title.into(),
                tile: TileContent::Stub {
                    kind: AppKind::Other(title.into()),
                    label: title.into(),
                },
                app_title: None,
            }],
            active_tab: tab,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
    }
    tree.seed_doc_from_cache();
    let chrome = crate::shell::chrome_state::ChromeState::new(tree);
    let handle = chrome.borrow().tree_handle();
    let broker =
        tilo_action_broker::collab_broker_capnp::CapnpBrokerState::test_broker_with_authority(
            Arc::new(crate::shell::broker_services::NativeWorkspaceAuthority(
                handle,
            )),
            tilo_action_broker::services::HostServices {
                host_steps: vec![super::broker_door()],
                ..Default::default()
            },
        );
    let host =
        ShellOverlayHost::new(Box::new(ui::widget::Container::new())).with_chrome(chrome.clone());
    Fixture {
        _home: home,
        chrome,
        host,
        broker,
        room,
        windows,
    }
}

impl Fixture {
    fn agent(&self) -> Caller {
        Caller::local_agent(AGENT).in_room(self.room.clone())
    }

    /// Invoke `action` with `arguments` as `caller` and run it on the
    /// desktop's thread, as the loop does when the broker wakes it; how it
    /// ended.
    fn run(&mut self, caller: &Caller, action: &str, arguments: Value) -> InvocationUpdate {
        let Value::Object(arguments) = arguments else {
            panic!("arguments are an object");
        };
        let request = InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: layout::PROVIDER.into(),
                address: tilo_actions::ActionAddress::new(RESOURCE, action),
            },
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
        };
        let admitted = self
            .broker
            .state()
            .action_invoke(caller, None, None, &request)
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .expect("admission answers at once");
        if let Err(error) = admitted {
            return InvocationUpdate::Failed {
                invocation: request.invocation,
                error: tilo_actions::ActionFailure {
                    code: "refused".into(),
                    message: error.to_string(),
                    details: None,
                    retryable: false,
                },
            };
        }
        // The desktop's loop runs on every frame, woken by the broker and by
        // what launches report: run it until the invocation ends.
        let deadline = tilo_test_env::hang_deadline();
        loop {
            self.host.serve_desktop_jobs();
            if let Some(observed) = self
                .broker
                .state()
                .action_observe(caller, &request.invocation)
                && observed.update.is_terminal()
            {
                return observed.update;
            }
            assert!(std::time::Instant::now() < deadline, "the invocation moves");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// `action`'s answer, which must be a success.
    fn answer(&mut self, action: &str, arguments: Value) -> Value {
        let agent = self.agent();
        match self.run(&agent, action, arguments) {
            InvocationUpdate::Succeeded { output, .. } => output.value.unwrap_or(Value::Null),
            other => panic!("{action} did not succeed: {other:?}"),
        }
    }

    /// The workspace's strips as (id, name, window groups, their widths).
    fn strips(&self) -> Vec<(StripId, String, Vec<GroupId>, Vec<WindowWidth>)> {
        let state = self.chrome.borrow();
        let tree = state.tree();
        tree.desktop.workspaces[0]
            .strips
            .iter()
            .map(|strip| {
                (
                    strip.id,
                    strip.name.clone(),
                    strip.window_groups.iter().map(|g| g.id).collect(),
                    strip.window_groups.iter().map(|g| g.width).collect(),
                )
            })
            .collect()
    }
}

fn id_of(value: &Value) -> u64 {
    value
        .as_str()
        .and_then(|text| text.parse().ok())
        .unwrap_or_else(|| panic!("an id as decimal text, not {value}"))
}

#[test]
#[serial_test::serial]
fn an_agent_lays_out_eight_strips_by_id_and_one_undo_takes_it_all_back() {
    let mut fixture = fixture();
    let before = fixture.strips();
    let focused = fixture.chrome.borrow().focused_strip_id();
    let names = [
        "Ada Lovelace",
        "Grace Hopper",
        "Alan Turing",
        "Edsger Dijkstra",
        "Barbara Liskov",
        "Ken Thompson",
        "Margaret Hamilton",
        "Donald Knuth",
    ];
    let mut strips = Vec::new();
    for name in names {
        let made = fixture.answer(layout::CREATE_STRIP, json!({ "name": name }));
        assert_eq!(made["name"], name);
        strips.push(StripId(id_of(&made["strip"])));
    }
    let now = fixture.strips();
    assert_eq!(now.len(), 9, "eight strips beside the first");
    for (strip, name) in strips.iter().zip(names) {
        assert!(
            now.iter()
                .any(|(id, named, _, _)| id == strip && named == name),
            "{name} is strip {}",
            strip.raw()
        );
    }
    assert_eq!(
        fixture.chrome.borrow().focused_strip_id(),
        focused,
        "an agent's strips leave the person where they are"
    );

    let renamed = fixture.answer(
        layout::RENAME_STRIP,
        json!({ "strip": strips[7].raw().to_string(), "name": "Donald E. Knuth" }),
    );
    assert_eq!(renamed["name"], "Donald E. Knuth");

    // One window onto each of the first three candidates' strips, then the
    // first strip's last two side by side with it.
    for (window, strip) in fixture.windows.clone().iter().zip(&strips) {
        let moved = fixture.answer(
            layout::MOVE_GROUP,
            json!({ "group": window.to_string(), "strip": strip.raw().to_string() }),
        );
        assert_eq!(moved["moved"], true);
    }
    let windows = fixture.windows.clone();
    for (index, window) in windows.iter().enumerate().skip(1) {
        fixture.answer(
            layout::MOVE_GROUP,
            json!({
                "group": window.to_string(),
                "strip": strips[0].raw().to_string(),
                "index": index,
            }),
        );
    }
    let first = fixture
        .strips()
        .into_iter()
        .find(|(id, ..)| *id == strips[0])
        .unwrap();
    assert_eq!(
        first.2, windows,
        "the three windows, in order, on Ada's strip"
    );

    let arranged = fixture.answer(
        layout::ARRANGE,
        json!({ "strip": strips[0].raw().to_string(), "layout": "columns" }),
    );
    assert_eq!(arranged["widths"].as_array().map(Vec::len), Some(3));
    let first = fixture
        .strips()
        .into_iter()
        .find(|(id, ..)| *id == strips[0])
        .unwrap();
    assert_eq!(first.3, vec![WindowWidth::Fraction(333); 3], "side by side");
    let rows = fixture.run(
        &fixture.agent(),
        layout::ARRANGE,
        json!({ "strip": strips[0].raw().to_string(), "layout": "rows" }),
    );
    assert!(
        matches!(&rows, InvocationUpdate::Failed { error, .. } if error.message.contains("side by side")),
        "a strip has no rows, and says what to do instead: {rows:?}"
    );

    let listed = fixture.answer(layout::LIST_WINDOWS, json!({}));
    let listed = listed["windows"].as_array().unwrap();
    assert_eq!(listed.len(), 3);
    for (window, row) in windows.iter().zip(listed) {
        assert_eq!(id_of(&row["group"]), *window);
        assert_eq!(id_of(&row["strip"]), strips[0].raw());
        assert_eq!(row["strip_name"], "Ada Lovelace");
        assert_eq!(row["shown"], true);
        assert!(row["title"].is_string());
    }

    let reversed: Vec<String> = windows.iter().rev().map(ToString::to_string).collect();
    fixture.answer(
        layout::ARRANGE,
        json!({ "strip": strips[0].raw().to_string(), "order": reversed }),
    );
    let first = fixture
        .strips()
        .into_iter()
        .find(|(id, ..)| *id == strips[0])
        .unwrap();
    let mut back = windows.clone();
    back.reverse();
    assert_eq!(first.2, back, "the columns in the order asked");

    let undone = fixture.answer(layout::UNDO, json!({}));
    assert_eq!(undone["kept"], json!([]), "{undone}");
    assert_eq!(
        undone["undone"], 17,
        "8 strips, a rename, 7 moves and an arrangement, as one entry"
    );
    let after = fixture.strips();
    assert_eq!(
        after, before,
        "the desktop is as it was before the agent's run"
    );
    let again = fixture.run(&fixture.agent(), layout::UNDO, json!({}));
    assert!(
        matches!(again, InvocationUpdate::Failed { .. }),
        "the run was taken back once"
    );
}

#[test]
#[serial_test::serial]
fn an_agent_the_room_never_admitted_changes_nothing() {
    let mut fixture = fixture();
    let before = fixture.strips();
    let stranger = Caller::local_agent("intruder").in_room(fixture.room.clone());
    let refused = fixture.run(&stranger, layout::CREATE_STRIP, json!({ "name": "Mine" }));
    assert!(
        matches!(refused, InvocationUpdate::Failed { .. }),
        "{refused:?}"
    );
    assert_eq!(fixture.strips(), before);
}

#[test]
#[serial_test::serial]
fn opening_what_nothing_here_opens_says_why_and_holds_no_place() {
    let mut fixture = fixture();
    let before = fixture.strips();
    let agent = fixture.agent();
    for (target, why) in [
        ("https://github.com/ada", "web addresses"),
        ("mail.message:work:INBOX:42", "publishes"),
        ("not an address", "not a resource address"),
    ] {
        let ended = fixture.run(&agent, layout::OPEN, json!({ "target": target }));
        assert!(
            matches!(&ended, InvocationUpdate::Failed { error, .. } if error.message.contains(why)),
            "{target}: {ended:?}"
        );
    }
    let ended = fixture.run(
        &agent,
        layout::OPEN,
        json!({ "target": "https://example.com", "place": "tab-of" }),
    );
    assert!(
        matches!(&ended, InvocationUpdate::Failed { error, .. } if error.message.contains("group")),
        "{ended:?}"
    );
    assert_eq!(fixture.strips(), before);
}

/// A web address an agent opens is browsed as any visitor would, unless it
/// asks for the person's sign-ins; one the person opens is theirs. Only a
/// web address has sign-ins to ask for.
#[test]
fn an_agent_opens_a_web_address_as_a_visitor_unless_it_asks_for_the_persons_sign_ins() {
    use crate::shell::desktop_layout::Target;
    let open = |arguments: Value, agent: bool| {
        let Value::Object(arguments) = arguments else {
            panic!("arguments are an object");
        };
        super::bind(layout::OPEN, &arguments, agent).map(|command| match command {
            super::Command::Open { target, .. } => target,
            other => panic!("open binds an open, not {other:?}"),
        })
    };
    let url = "https://github.com/ada";
    let visit = Ok(Target::Visit(url.to_owned()));
    let own = Ok(Target::Url(url.to_owned()));
    assert_eq!(open(json!({ "target": url }), true), visit);
    assert_eq!(open(json!({ "target": url, "signed_in": true }), true), own);
    assert_eq!(open(json!({ "target": url }), false), own);
    assert_eq!(
        open(json!({ "target": url, "signed_in": false }), false),
        visit
    );
    let refused = open(
        json!({ "target": "mail.message:work:INBOX:42", "signed_in": true }),
        true,
    );
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.contains("web address")),
        "{refused:?}"
    );
    assert!(open(json!({ "target": url, "signed_in": "yes" }), true).is_err());
    assert_eq!(
        open(json!({ "target": "mail.message:work:INBOX:42" }), true),
        Ok(Target::Resource("mail.message:work:INBOX:42".into())),
        "a resource opens as it is"
    );
}

/// A package `package_id` installed under `dir`, opening `mime` and the
/// resource kinds `resources`.
pub(super) fn install(dir: &std::path::Path, package_id: &str, mime: &[&str], resources: &[&str]) {
    use std::fmt::Write as _;
    let payload = dir.join("payloads").join(package_id);
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    let manifest = payload.join("manifest.toml");
    let quoted = |values: &[&str]| {
        values
            .iter()
            .map(|value| format!("\"{value}\""))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let (mime, resources) = (quoted(mime), quoted(resources));
    std::fs::write(
        &manifest,
        crate::shell::test_support::canonical_manifest(
            &format!("id = \"{package_id}\"\nname = \"{package_id}\"\nversion = \"1.0.0\"\nkind = \"app\""),
            &format!(
                "[entrypoint]\nbin = \"bin/app\"\ntype = \"gui\"\n\
                 [providers]\nmime = [{mime}]\next = []\nresources = [{resources}]\n"
            ),
        ),
    )
    .unwrap();
    let registry = dir.join("registry.toml");
    let mut entries = std::fs::read_to_string(&registry).unwrap_or_default();
    write!(
        entries,
        "[[entry]]\nid = '{package_id}'\npayload_root = {:?}\nmanifest_path = {:?}\n",
        payload.display().to_string(),
        manifest.display().to_string()
    )
    .unwrap();
    std::fs::write(&registry, entries).unwrap();
}

/// An open of an address whose kind an installed app declares it opens
/// launches that app; when the launch fails, the open is answered failed,
/// never opened, and the place it held goes.
#[test]
#[serial_test::serial(process_env)]
fn an_open_whose_launch_fails_is_answered_failed_and_holds_no_place() {
    const NOTES: &str = "org.example.notes";
    let mut fixture = fixture();
    fixture.chrome.borrow_mut().act_as(OWNER.into());
    crate::shell::action_authority::follow_tree(&fixture.chrome.borrow().tree_handle());
    let tmp = tempfile::tempdir().unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    let apps = tmp.path().join("apps");
    install(&apps, NOTES, &[], &["notes.page"]);
    env.set("TILO_APPS_DATA_DIR", &apps);
    // The launcher is not there: the launch fails to start.
    crate::shell::launcher::set_test_execution_program(Some(tmp.path().join("missing-launcher")));
    crate::shell::launcher::set_wayland_socket_path(Some(tmp.path().join("wayland-0")));
    crate::shell::launcher::invalidate_app_cache();
    crate::shell::launcher::clear_pending_placements();
    fixture.host.attach_open_door(
        crate::shell::search::providers::app_content::OpenDoor::fixture(fixture.broker.clone()),
    );
    let before = fixture.strips();
    let agent = fixture.agent();
    let ended = fixture.run(&agent, layout::OPEN, json!({ "target": "notes.page:1" }));
    assert!(
        matches!(&ended, InvocationUpdate::Failed { error, .. } if error.message.contains("did not open")),
        "{ended:?}"
    );
    assert_eq!(
        fixture.strips(),
        before,
        "the place the launch held is gone"
    );
    crate::shell::launcher::set_test_execution_program(None);
    crate::shell::launcher::set_wayland_socket_path(None);
    crate::shell::launcher::clear_pending_placements();
}

#[path = "desktop_mail_tests.rs"]
mod mail_review;
