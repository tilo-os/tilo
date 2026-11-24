//! Who is in a channel is who the workspace lets read it: Chat's members panel lists the channel's
//! readers (`members(c)`) and changes them through the workspace's own grant and revoke actions on
//! the channel, never a list of its own. A moderator the workspace lets grant brings Ben in and
//! takes him out again, and his read follows; one it does not is refused.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_action_policy::reading::published_node;
use tilo_action_policy::roles::{self, governance};
use tilo_action_policy::{
    ActionKind, Answer, Authority, Change, Context, Grant, Grantee, KindSet, MemberAccess,
    PersonId, Role, WorkspacePolicy,
};
use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    WORKSPACE_READ_GRANT, WORKSPACE_READ_REVOKE, workspace_governance_child,
    workspace_governance_operation,
};
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::{Actor, InvokeOutcome, UiNode, Value, ViewInstance};
use tilolang_tilo::call::Callees;

use crate::fixtures::{Replica, Workspace, target};

const CHAT: &str = include_str!("../../../../apps/tl/chat/app.tl");
const CHAT_IDS: &str = include_str!("../../../../apps/tl/chat/app.ids");
const APP: &str = "so.tilo.chat";
const WORKSPACE: &str = "team";

/// The workspace's policy, which every replica decides by and the workspace's governance changes.
type Shared = Rc<RefCell<WorkspacePolicy>>;

/// The workspace's governance as its host answers a call the session sends it: the call as the
/// device's dispatch builds it from the invocation ([`Callees::call`]), the operation its tree
/// publishes at that address, and the change [`WorkspacePolicy::reading_change`] decides for the
/// caller, written into the policy every member decides by.
struct WorkspaceActions {
    policy: Shared,
    callees: Callees,
    caller: String,
}

impl Invoker for WorkspaceActions {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        let call = match self.callees.call(invocation) {
            Ok(call) => call,
            Err(why) => return Invoked::Failed(format!("{why:?}")),
        };
        assert_eq!(call["app"], "tilo.workspace", "{call}");
        let child = call["resource"]
            .as_str()
            .and_then(|resource| resource.strip_prefix("workspace/"))
            .expect("a call on one of the workspace's own roots");
        let address = ActionAddress::new(
            workspace_governance_child(WORKSPACE, Some(child)),
            call["action"].as_str().expect("an action"),
        );
        let (_, operation) =
            workspace_governance_operation(&address).expect("an operation the workspace publishes");
        let reads = match operation {
            WORKSPACE_READ_GRANT => true,
            WORKSPACE_READ_REVOKE => false,
            other => panic!("not a reading change: {other}"),
        };
        let arguments = &call["arguments"];
        let node = published_node(
            WORKSPACE,
            arguments["resource"].as_str().expect("the resource"),
        )
        .expect("a resource of an app of the workspace");
        let member: tilo_action_policy::MemberId =
            serde_json::from_value(arguments["member"].clone()).expect("the member");
        let cx = Context::at_time(tilo_clock::wall());
        let change = {
            let policy = self.policy.borrow();
            policy.reading_change(
                &Authority::person(policy.party(&self.caller)),
                Grantee::member(member),
                &node,
                reads,
                Grantee::person(PersonId::from(self.caller.as_str())),
                &cx,
            )
        };
        match change {
            Err(refusal) => Invoked::Refused(refusal.to_string()),
            Ok(change) => {
                if let Some(change) = change {
                    write(&self.policy, change);
                }
                Invoked::Done(Value::Unit)
            }
        }
    }
}

/// Write `change` into the workspace's policy, as its host writes a change it decided.
fn write(policy: &Shared, change: Change) {
    let cx = Context::at_time(tilo_clock::wall());
    let mut next = policy.borrow().clone();
    let owner = next.clone();
    next.apply(
        "ada",
        &Authority::person(owner.party("ada")),
        change,
        &cx,
        false,
    )
    .expect("the host writes the change");
    *policy.borrow_mut() = next;
}

/// Ada's workspace: Mo moderates and the owner let him grant and revoke, Cy moderates without
/// that, and Ben edits.
fn workspace() -> Shared {
    let mut policy = WorkspacePolicy::new(WORKSPACE, "ada");
    policy.members.insert("mo".to_owned(), Role::Moderator);
    policy.members.insert("cy".to_owned(), Role::Moderator);
    policy.members.insert("ben".to_owned(), Role::Editor);
    for action in [governance::GRANT, governance::REVOKE] {
        policy.grants.push(Grant::new(
            format!("mo-{action}"),
            Grantee::person("mo"),
            roles::governance_action(WORKSPACE, action),
            KindSet::of(ActionKind::Outside),
            Answer::Allow,
            Grantee::person("ada"),
        ));
    }
    Rc::new(RefCell::new(policy))
}

/// `name`'s device in the workspace, deciding by the policy as it stands at each read, its calls
/// into the workspace answered by its governance.
fn member(name: &str, doc: Workspace, policy: &Shared) -> Replica {
    let following = Rc::clone(policy);
    let person = name.to_owned();
    let (replica, _) = Replica::following(name, doc, &[(CHAT, CHAT_IDS)], move || {
        Some(MemberAccess::person(following.borrow().clone(), &person))
    });
    let packages = [
        (
            APP.to_owned(),
            replica.host.package(APP).expect("Chat").meta,
        ),
        (
            "tilo.workspace".to_owned(),
            tilolang_tilo::platform_apps::platform_package("tilo.workspace")
                .expect("the workspace's package"),
        ),
    ];
    replica
        .host
        .runtime()
        .set_foreign_invoker(WorkspaceActions {
            policy: Rc::clone(policy),
            callees: Callees {
                apps: packages.into_iter().collect(),
                published: ["tilo.workspace".to_owned()].into_iter().collect(),
                ..Callees::default()
            },
            caller: name.to_owned(),
        });
    replica.host.follow_members();
    replica
}

fn texts(node: &UiNode) -> Vec<String> {
    crate::fixtures::nodes(node)
        .into_iter()
        .filter_map(
            |node| match node.props.get("label").or(node.props.get("value")) {
                Some(Value::Text(text)) => Some(text.clone()),
                _ => None,
            },
        )
        .collect()
}

/// The people the panel lists under `section`, in order.
fn listed(view: &mut ViewInstance, section: &str) -> Vec<String> {
    view.poll_patches().unwrap();
    view.tree()
        .of_kind("ui.Section")
        .into_iter()
        .find(|node| node.props.get("title") == Some(&Value::text(section)))
        .map(|node| {
            node.of_kind("ui.Text")
                .into_iter()
                .filter_map(|text| match text.props.get("value") {
                    Some(Value::Text(name)) => Some(name.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Press the button labelled `label` on the row of `person` in the panel.
fn press(view: &mut ViewInstance, person: &str, label: &str) -> Result<InvokeOutcome, String> {
    view.poll_patches().unwrap();
    let key = view
        .tree()
        .of_kind("row")
        .into_iter()
        .filter(|row| {
            row.of_kind("ui.Text")
                .iter()
                .any(|text| text.props.get("value") == Some(&Value::text(person)))
        })
        .flat_map(|row| row.of_kind("ui.Button"))
        .find(|button| button.props.get("label") == Some(&Value::text(label)))
        .unwrap_or_else(|| panic!("no {label} for {person}: {:?}", texts(view.tree())))
        .key
        .clone();
    view.press(&key).map_err(|error| format!("{error:?}"))
}

fn view_id(r: &Replica, name: &str) -> String {
    r.host
        .package(APP)
        .unwrap()
        .meta
        .views
        .iter()
        .find(|view| view.name == name)
        .unwrap_or_else(|| panic!("the view {name}"))
        .id
        .clone()
}

/// Whether Ben's device reads the channel: what a read by him gets, from the grants as they stand.
fn reads(ben: &Replica, channel: &tilolang_runtime::ResourceRef) -> bool {
    ben.host.follow_members();
    ben.host
        .runtime()
        .may_read(&ben.host.viewer().clone(), channel)
}

#[test]
fn a_moderator_lets_ben_into_a_channel_and_takes_him_out_again() {
    let policy = workspace();
    let mo = member("mo", Workspace::new(), &policy);
    let all = Value::resource(&mo.host.root_of(APP).unwrap());
    mo.commit();
    let made = {
        let meta = mo.host.package(APP).unwrap().meta;
        match mo
            .host
            .runtime()
            .invoke_action(
                &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("mo")),
                (
                    crate::fixtures::action_id(&meta, "new_channel").as_str(),
                    vec![all, Value::text("design")],
                ),
            )
            .unwrap()
        {
            InvokeOutcome::Done(made) => made,
            other => panic!("Mo makes the channel: {other:?}"),
        }
    };
    mo.commit();
    let channel = target(&made);

    // Ben starts outside the channel: the owner kept him out of it.
    let node = published_node(WORKSPACE, &format!("tl:{APP}:{}", channel.id_string())).unwrap();
    let kept_out = {
        let held = policy.borrow();
        held.reading_change(
            &Authority::person(held.party("ada")),
            Grantee::person("ben"),
            &node,
            false,
            Grantee::person("ada"),
            &Context::at_time(tilo_clock::wall()),
        )
    };
    write(&policy, kept_out.unwrap().expect("a change"));
    // Mo's window follows the workspace's grants, as a window does each frame.
    mo.host.follow_members();
    let ben = member("ben", mo.doc.fork(), &policy);
    assert!(!reads(&ben, &channel), "Ben does not read the channel");

    // The channel's menu opens its members.
    let mut page = mo.host.open_resource(&channel, None).unwrap();
    page.poll_patches().unwrap();
    assert!(
        page.tree()
            .of_kind("ui.MenuItem")
            .iter()
            .any(|item| item.props.get("label") == Some(&Value::text("Members"))),
        "the channel's menu offers its members"
    );
    let mut panel = mo
        .host
        .open_resource(&channel, Some(&view_id(&mo, "channel_members")))
        .unwrap();
    assert!(
        !listed(&mut panel, "Members").contains(&"ben".to_owned()),
        "{:?}",
        texts(panel.tree())
    );
    assert!(
        listed(&mut panel, "Not in the channel").contains(&"ben".to_owned()),
        "{:?}",
        texts(panel.tree())
    );

    // Mo lets him in: the workspace's grant, and the panel lists him from the readers.
    let added = press(&mut panel, "ben", "Add");
    assert!(matches!(added, Ok(InvokeOutcome::Done(_))), "{added:?}");
    mo.host.follow_members();
    assert!(
        listed(&mut panel, "Members").contains(&"ben".to_owned()),
        "{:?}",
        texts(panel.tree())
    );
    assert!(reads(&ben, &channel), "Ben reads the channel");
    assert!(
        policy
            .borrow()
            .grants
            .iter()
            .all(|grant| !grant.id.as_str().starts_with("reads:")),
        "letting him back took the owner's Deny away rather than adding to it"
    );

    // Mo takes him out again: gone from the panel, and his read is refused.
    let removed = press(&mut panel, "ben", "Remove");
    assert!(matches!(removed, Ok(InvokeOutcome::Done(_))), "{removed:?}");
    mo.host.follow_members();
    assert!(
        !listed(&mut panel, "Members").contains(&"ben".to_owned()),
        "{:?}",
        texts(panel.tree())
    );
    assert!(!reads(&ben, &channel), "Ben's read is refused");

    // Cy moderates too, but the workspace never let her grant: her Add is refused, and Ben stays
    // out.
    let cy = member("cy", mo.doc.fork(), &policy);
    let mut theirs = cy
        .host
        .open_resource(&channel, Some(&view_id(&cy, "channel_members")))
        .unwrap();
    let refused = press(&mut theirs, "ben", "Add");
    assert!(
        format!("{refused:?}").contains("not allowed")
            || matches!(refused, Ok(InvokeOutcome::Denied)),
        "Cy may not grant: {refused:?}"
    );
    assert!(!reads(&ben, &channel), "Ben is still kept out");
}
