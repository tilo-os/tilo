use super::*;
use crate::shell::chrome_state::PromptRequest;
use crate::shell::search::providers::permission_prompts::answer_pending_prompt_as;
use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};
use ui::widget::PromptChoice;

/// A session with a room this machine administers, as `agent.invite`'s own
/// tests set one up.
fn chrome_with_room() -> Chrome {
    use crate::shell::state::{FreshId as _, Workspace, WorkspaceId};
    let mut tree = WorkspaceTree::default();
    let id = WorkspaceId::new();
    let mut workspace = Workspace::new(id, "Demo");
    workspace.set_owner("shell-fixture");
    tree.desktop.workspaces.push(workspace);
    tree.desktop.active = id;
    let chrome = ChromeState::new_with_fixture_handle(
        WorkspaceTreeHandle::new(tree),
        crate::shell::hosted_inputs::HostedTileInputs::default(),
    );
    chrome.borrow_mut().test_only_owner_override = Some(true);
    chrome
}

fn room(chrome: &Chrome) -> String {
    chrome
        .borrow()
        .tree()
        .desktop
        .active_workspace()
        .expect("fixture has a room")
        .room
        .to_string()
}

fn facts(exe: &str) -> ClientFacts {
    ClientFacts {
        client: "Claude Code".into(),
        version: Some("2".into()),
        pid: 4242,
        exe: Some(PathBuf::from(exe)),
        host_app: Some("org.alacritty.Alacritty".into()),
    }
}

/// One client asking: what the door's worker hands over and then waits on.
fn ask(
    sender: &std::sync::mpsc::Sender<DoorRequest>,
    facts: ClientFacts,
) -> tokio::sync::oneshot::Receiver<Consent> {
    let (decide, decided) = tokio::sync::oneshot::channel();
    sender.send(DoorRequest { facts, decide }).unwrap();
    decided
}

fn front_id(chrome: &Chrome) -> uuid::Uuid {
    let state = chrome.borrow();
    let front = state.pending_grants.front().expect("a prompt is up");
    assert!(matches!(front.request, PromptRequest::AgentAdmission(_)));
    front.id
}

fn answer(chrome: &Chrome, choice: PromptChoice, member: Option<MemberChoice>) {
    let id = front_id(chrome);
    answer_pending_prompt_as(&mut chrome.borrow_mut(), id, choice, member).expect("answered");
}

fn agent_members(chrome: &Chrome) -> Vec<String> {
    use crate::shell::state::WorkspaceAuthority as _;
    chrome
        .borrow()
        .tree()
        .desktop
        .active_workspace()
        .expect("fixture has a room")
        .agent_members()
        .into_iter()
        .map(|(agent, _)| agent)
        .collect()
}

#[test]
fn a_client_is_proposed_under_its_own_name_as_a_member_id() {
    assert_eq!(proposed_agent_id("Claude Code"), "claude-code");
    assert_eq!(proposed_agent_id("  codex/cli 2 "), "codex-cli-2");
    assert_eq!(proposed_agent_id("!!!"), "agent");
}

#[test]
fn nothing_is_decided_until_the_person_answers_and_a_refusal_admits_nobody() {
    let chrome = chrome_with_room();
    let (sender, mut consent) = channel(None, None);
    let mut decided = ask(&sender, facts("/usr/bin/node"));
    consent.pump(&chrome);
    consent.pump(&chrome);
    assert!(
        decided.try_recv().is_err(),
        "decided before anyone answered"
    );
    let subject = chrome
        .borrow()
        .pending_grants
        .front()
        .unwrap()
        .app_id
        .clone();
    assert!(
        subject.contains("Claude Code") && subject.contains("org.alacritty.Alacritty"),
        "{subject}"
    );
    answer(&chrome, PromptChoice::Deny, None);
    consent.pump(&chrome);
    assert!(matches!(decided.try_recv(), Ok(Consent::Refuse(_))));
    assert!(agent_members(&chrome).is_empty());
    assert_eq!(consent.waiting(), 0);
}

#[test]
fn allowing_once_adds_the_proposed_agent_and_remembers_nothing() {
    let chrome = chrome_with_room();
    let (sender, mut consent) = channel(None, None);
    let mut decided = ask(&sender, facts("/usr/bin/node"));
    consent.pump(&chrome);
    answer(&chrome, PromptChoice::Allow, None);
    consent.pump(&chrome);
    assert_eq!(
        decided.try_recv().unwrap(),
        Consent::Admit {
            room: room(&chrome),
            agent: "claude-code".into()
        }
    );
    assert_eq!(agent_members(&chrome), ["claude-code"]);
    // Once is once: the same client asks again next time.
    let mut again = ask(&sender, facts("/usr/bin/node"));
    consent.pump(&chrome);
    assert!(again.try_recv().is_err());
    assert_eq!(chrome.borrow().pending_grants.len(), 1);
}

#[test]
fn the_person_names_which_agent_and_preset_a_client_is_admitted_as() {
    let chrome = chrome_with_room();
    let (sender, mut consent) = channel(None, None);
    let mut decided = ask(&sender, facts("/usr/bin/node"));
    consent.pump(&chrome);
    answer(
        &chrome,
        PromptChoice::Allow,
        Some(MemberChoice {
            agent: "  Scribe ".into(),
            preset: Some(AgentAuthorityPreset::Operator),
        }),
    );
    consent.pump(&chrome);
    assert!(matches!(
        decided.try_recv().unwrap(),
        Consent::Admit { agent, .. } if agent == "scribe"
    ));
    let state = chrome.borrow();
    let tree = state.tree();
    let workspace = tree.desktop.active_workspace().unwrap();
    assert!(
        workspace
            .capabilities
            .iter()
            .any(|cap| cap.id == "agent-member:scribe:operator"),
        "the membership is the ordinary admission grant"
    );
}

#[test]
fn a_session_answer_lets_the_same_client_in_again_and_no_other() {
    let chrome = chrome_with_room();
    let (sender, mut consent) = channel(None, None);
    let mut first = ask(&sender, facts("/usr/bin/node"));
    consent.pump(&chrome);
    answer(&chrome, PromptChoice::Session, None);
    consent.pump(&chrome);
    assert!(matches!(first.try_recv(), Ok(Consent::Admit { .. })));

    let mut same = ask(&sender, facts("/usr/bin/node"));
    consent.pump(&chrome);
    assert!(
        matches!(same.try_recv(), Ok(Consent::Admit { agent, .. }) if agent == "claude-code"),
        "the remembered client was asked again"
    );
    assert!(chrome.borrow().pending_grants.is_empty());

    let mut other = ask(&sender, facts("/opt/other/node"));
    consent.pump(&chrome);
    assert!(
        other.try_recv().is_err(),
        "another executable is another client"
    );
    assert_eq!(chrome.borrow().pending_grants.len(), 1);
}

#[test]
fn a_remembered_client_is_asked_again_once_its_agent_is_removed() {
    let chrome = chrome_with_room();
    let (sender, mut consent) = channel(None, None);
    let _first = ask(&sender, facts("/usr/bin/node"));
    consent.pump(&chrome);
    answer(&chrome, PromptChoice::Session, None);
    consent.pump(&chrome);
    {
        let state = chrome.borrow();
        let mut tree = state.tree_mut();
        let workspace = tree.desktop.active_workspace_mut().unwrap();
        workspace
            .capabilities
            .retain(|cap| !cap.id.starts_with("agent-member:claude-code"));
    }
    assert!(agent_members(&chrome).is_empty());
    let mut again = ask(&sender, facts("/usr/bin/node"));
    consent.pump(&chrome);
    assert!(again.try_recv().is_err(), "a revoked agent was let in");
    assert_eq!(chrome.borrow().pending_grants.len(), 1);
}

#[test]
fn always_is_kept_privately_for_the_next_session_and_only_an_owner_gives_it() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("tilo/agent-door-bindings.json");
    let chrome = chrome_with_room();
    let (sender, mut consent) = channel(Some(store.clone()), None);
    let mut decided = ask(&sender, facts("/usr/bin/node"));
    consent.pump(&chrome);

    chrome.borrow_mut().test_only_owner_override = Some(false);
    let id = front_id(&chrome);
    assert!(
        answer_pending_prompt_as(&mut chrome.borrow_mut(), id, PromptChoice::Always, None).is_err(),
        "only an owner remembers a client always"
    );
    chrome.borrow_mut().test_only_owner_override = Some(true);
    answer(&chrome, PromptChoice::Always, None);
    consent.pump(&chrome);
    assert!(matches!(decided.try_recv(), Ok(Consent::Admit { .. })));
    let mode = std::fs::metadata(&store).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);

    // The next session reads it back.
    let (sender, mut consent) = channel(Some(store), None);
    let mut next = ask(&sender, facts("/usr/bin/node"));
    consent.pump(&chrome);
    assert!(matches!(next.try_recv(), Ok(Consent::Admit { .. })));
    let mut renamed = ask(
        &sender,
        ClientFacts {
            client: "Not Claude".into(),
            ..facts("/usr/bin/node")
        },
    );
    consent.pump(&chrome);
    assert!(
        renamed.try_recv().is_err(),
        "another client name is another client"
    );
}

#[test]
fn a_client_that_leaves_withdraws_its_prompt() {
    let chrome = chrome_with_room();
    let (sender, mut consent) = channel(None, None);
    let decided = ask(&sender, facts("/usr/bin/node"));
    consent.pump(&chrome);
    assert_eq!(chrome.borrow().pending_grants.len(), 1);
    drop(decided);
    consent.pump(&chrome);
    assert_eq!(consent.waiting(), 0);
    assert!(
        chrome
            .borrow()
            .pending_grants
            .front()
            .is_some_and(PendingPrompt::is_retired),
        "the overlay's next pump drops a retired prompt"
    );
}

#[test]
fn an_agent_is_never_shown_the_decisions_on_its_own_admission() {
    use crate::shell::search::ResourceKind;
    use crate::shell::search::action_api::{ActionVisibility, ResourceRef};
    use crate::shell::search::providers::permission_prompts::PROVIDER_ID;
    use crate::shell::search::visibility::CapabilityVisibility;
    let chrome = chrome_with_room();
    let state = chrome.borrow();
    let tree = state.tree();
    let workspace = tree.desktop.active_workspace().unwrap();
    let agent =
        crate::shell::state::Actor::agent("claude-code").in_room(workspace.room.to_string());
    let visibility = CapabilityVisibility::new(workspace, agent);
    for decision in ["allow-once", "allow-session", "allow-always", "deny"] {
        assert!(!visibility.can_see(
            &ResourceRef::new(PROVIDER_ID, format!("{decision}@{}", uuid::Uuid::new_v4())),
            &ResourceKind::Action
        ));
    }
}
