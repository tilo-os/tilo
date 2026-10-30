#![allow(
    clippy::significant_drop_tightening,
    reason = "tests hold the chrome borrow across the assertions that read it"
)]
use super::*;
use crate::shell::chrome_state::{ChromeState, PromptRequest};
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
        .room()
        .to_string()
}

/// The person at this device in the fixture's room: whom an admitted agent
/// acts for.
fn person(chrome: &Chrome) -> String {
    let state = chrome.borrow();
    let room = state
        .tree()
        .desktop
        .active_workspace()
        .expect("fixture has a room")
        .room();
    state.local_user_for_room(&room)
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
) -> tokio::sync::oneshot::Receiver<DoorDecision> {
    let (decide, decided) = tokio::sync::oneshot::channel();
    sender.send(DoorRequest { facts, decide }).unwrap();
    decided
}

/// Tasks makes the run of each admitted client, as the overlay asks it
/// (`ask_agent::ask_door_runs`): an issue with the title asked for, assigned
/// to a task agent minted for it. The titles of the issues made.
fn tasks_start_runs(chrome: &Chrome) -> Vec<String> {
    let asks = chrome.borrow_mut().take_door_runs();
    asks.into_iter()
        .map(|ask| {
            assert_eq!(
                ask.person,
                person(chrome),
                "the run acts for who admitted it"
            );
            let agent = format!("task-agent-{}", ask.title.len());
            let _ = ask.reply.send(Ok(DoorRun {
                agent,
                issue: format!("tasks.tracker/issues/{}", ask.title),
                presentation: ask.title.clone().into_bytes(),
            }));
            ask.title
        })
        .collect()
}

/// The client was let in under `label`, into the run Tasks made for it.
fn admitted(decided: &mut tokio::sync::oneshot::Receiver<DoorDecision>, label: &str) -> Admitted {
    let admitted = decided.try_recv().expect("decided").expect("let in");
    assert_eq!(admitted.label, label);
    admitted
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

/// Nobody is made a member by the door: a client works in its run.
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
    assert!(matches!(decided.try_recv(), Ok(Err(_))));
    assert!(tasks_start_runs(&chrome).is_empty(), "no run for a refusal");
    assert_eq!(consent.waiting(), 0);
}

#[test]
fn allowing_once_starts_a_run_of_its_own_and_remembers_nothing() {
    let chrome = chrome_with_room();
    let (sender, mut consent) = channel(None, None);
    let mut decided = ask(&sender, facts("/usr/bin/node"));
    consent.pump(&chrome);
    answer(&chrome, PromptChoice::Allow, None);
    consent.pump(&chrome);
    assert!(decided.try_recv().is_err(), "let in before its run went");
    assert_eq!(tasks_start_runs(&chrome), ["claude-code in this session"]);
    consent.pump(&chrome);
    let admitted = admitted(&mut decided, "claude-code");
    assert_eq!(admitted.room, room(&chrome));
    assert_eq!(admitted.person, person(&chrome));
    assert_eq!(admitted.agent, "task-agent-27", "the run's task agent");
    assert_eq!(admitted.presentation, b"claude-code in this session");
    assert!(agent_members(&chrome).is_empty(), "the door adds no member");
    // Once is once: the same client asks again next time.
    let mut again = ask(&sender, facts("/usr/bin/node"));
    consent.pump(&chrome);
    assert!(again.try_recv().is_err());
    assert_eq!(chrome.borrow().pending_grants.len(), 1);
}

/// The person's answer asks Tasks for the run; a run Tasks does not make
/// turns the client away with why, and nothing else is made.
#[test]
fn a_run_tasks_does_not_make_turns_the_client_away() {
    let chrome = chrome_with_room();
    let (sender, mut consent) = channel(None, None);
    let mut decided = ask(&sender, facts("/usr/bin/node"));
    consent.pump(&chrome);
    answer(&chrome, PromptChoice::Allow, None);
    consent.pump(&chrome);
    for ask in chrome.borrow_mut().take_door_runs() {
        let _ = ask.reply.send(Err("Tasks is not installed".into()));
    }
    consent.pump(&chrome);
    let refused = decided.try_recv().unwrap().unwrap_err();
    assert!(refused.contains("Tasks is not installed"), "{refused}");
    assert!(agent_members(&chrome).is_empty());
}

#[test]
fn the_person_names_what_a_client_works_under() {
    let chrome = chrome_with_room();
    let (sender, mut consent) = channel(None, None);
    let mut decided = ask(&sender, facts("/usr/bin/node"));
    consent.pump(&chrome);
    answer(
        &chrome,
        PromptChoice::Allow,
        Some(MemberChoice {
            agent: "  Scribe ".into(),
        }),
    );
    consent.pump(&chrome);
    assert_eq!(tasks_start_runs(&chrome), ["scribe in this session"]);
    consent.pump(&chrome);
    admitted(&mut decided, "scribe");
}

#[test]
fn a_session_answer_lets_the_same_client_in_again_and_no_other() {
    let chrome = chrome_with_room();
    let (sender, mut consent) = channel(None, None);
    let mut first = ask(&sender, facts("/usr/bin/node"));
    consent.pump(&chrome);
    answer(&chrome, PromptChoice::Session, None);
    consent.pump(&chrome);
    tasks_start_runs(&chrome);
    consent.pump(&chrome);
    admitted(&mut first, "claude-code");

    // Let in without asking, into a run of its own.
    let mut same = ask(&sender, facts("/usr/bin/node"));
    consent.pump(&chrome);
    assert!(chrome.borrow().pending_grants.is_empty(), "asked again");
    assert_eq!(tasks_start_runs(&chrome), ["claude-code in this session"]);
    consent.pump(&chrome);
    admitted(&mut same, "claude-code");

    let mut other = ask(&sender, facts("/opt/other/node"));
    consent.pump(&chrome);
    assert!(
        other.try_recv().is_err(),
        "another executable is another client"
    );
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
    tasks_start_runs(&chrome);
    consent.pump(&chrome);
    admitted(&mut decided, "claude-code");
    // The answer is written on the state writer.
    crate::shell::durable_writer::global().flush();
    let mode = std::fs::metadata(&store).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);

    // The next session reads it back.
    let (sender, mut consent) = channel(Some(store), None);
    let mut next = ask(&sender, facts("/usr/bin/node"));
    consent.pump(&chrome);
    tasks_start_runs(&chrome);
    consent.pump(&chrome);
    admitted(&mut next, "claude-code");
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

/// `pump` runs on the compositor's loop, which must never wait for the disk:
/// remembering an "Always" answer hands its write to the state writer.
#[cfg(target_os = "linux")]
#[test]
fn remembering_always_never_waits_for_the_disk() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("tilo/agent-door-bindings.json");
    crate::shell::disk_sync_guard::on_a_thread_that_cannot_sync(|| {
        let chrome = chrome_with_room();
        let (sender, mut consent) = channel(Some(store.clone()), None);
        let mut decided = ask(&sender, facts("/usr/bin/node"));
        consent.pump(&chrome);
        answer(&chrome, PromptChoice::Always, None);
        consent.pump(&chrome);
        tasks_start_runs(&chrome);
        consent.pump(&chrome);
        admitted(&mut decided, "claude-code");
    });
    crate::shell::durable_writer::global().flush();
    let stored: StoredBindings = serde_json::from_slice(&std::fs::read(&store).unwrap()).unwrap();
    assert_eq!(stored.bindings.len(), 1, "the answer is remembered");
    assert_eq!(stored.bindings[0].key.client, "Claude Code");
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
    use crate::shell::search::visibility::PolicyVisibility;
    let chrome = chrome_with_room();
    // The most an admitted agent can hold: it may change the room and ask to
    // reach outside it, and still never answers a prompt.
    {
        let state = chrome.borrow();
        let mut tree = state.tree_mut();
        let workspace = tree.desktop.active_workspace_mut().unwrap();
        let owner = tilo_action_policy::Subject::person("shell-fixture");
        workspace.admit_agent_preset(
            "claude-code",
            tilo_action_policy::roles::AgentPreset::Operator,
            &owner,
        );
    }
    let state = chrome.borrow();
    let tree = state.tree();
    let workspace = tree.desktop.active_workspace().unwrap();
    let agent =
        crate::shell::state::Actor::agent("claude-code").in_room(workspace.room().to_string());
    let visibility = PolicyVisibility::new(workspace, agent);
    for decision in ["allow-once", "allow-session", "allow-always", "deny"] {
        assert!(!visibility.can_see(
            &ResourceRef::new(PROVIDER_ID, format!("{decision}@{}", uuid::Uuid::new_v4())),
            &ResourceKind::Action
        ));
    }
}

/// What `t` and Claude Code look like at the door: both reach it through the
/// same relay executable, so only their own names and pids tell them apart.
fn relayed(client: &str, pid: u32) -> ClientFacts {
    ClientFacts {
        client: client.into(),
        version: None,
        pid,
        exe: Some(PathBuf::from("/usr/libexec/tilo/tilo-runtime-mcp")),
        host_app: None,
    }
}

/// The prompt for `client`, by what it shows the person.
fn prompt_for(chrome: &Chrome, client: &str) -> uuid::Uuid {
    let state = chrome.borrow();
    let prompts: Vec<_> = state
        .pending_grants
        .iter()
        .filter(|pending| pending.app_id.starts_with(&format!("{client} (")))
        .map(|pending| pending.id)
        .collect();
    assert_eq!(prompts.len(), 1, "{:?}", state.pending_grants);
    prompts[0]
}

/// Two clients wait at once. Each prompt is its own request with its own
/// identity and its own reply: the answer to one admits exactly that one, a
/// repeated, stale, unknown or out-of-turn answer admits nobody, and the
/// other client stays waiting until its own prompt is answered.
#[test]
#[allow(clippy::too_many_lines, reason = "one scenario read top to bottom")]
fn two_waiting_clients_are_each_decided_only_by_the_answer_to_their_own_prompt() {
    let chrome = chrome_with_room();
    let (sender, mut consent) = channel(None, None);
    let mut t = ask(&sender, relayed("t", 100));
    let mut claude = ask(&sender, relayed("claude-code", 200));
    consent.pump(&chrome);
    assert_eq!(consent.waiting(), 2);
    let t_prompt = prompt_for(&chrome, "t");
    let claude_prompt = prompt_for(&chrome, "claude-code");
    assert_ne!(t_prompt, claude_prompt);
    {
        let state = chrome.borrow();
        let shown: Vec<_> = state
            .pending_grants
            .iter()
            .map(|pending| {
                (
                    pending.app_id.clone(),
                    crate::shell::overlay_host::prompt_label(&pending.request),
                )
            })
            .collect();
        // Each prompt names its own client and the name it works under; a
        // process id means nothing to the person deciding.
        assert!(
            shown[0].0.starts_with("t (") && shown[0].1.contains("as t,"),
            "{shown:?}"
        );
        assert!(
            shown[1].0.starts_with("claude-code (") && shown[1].1.contains("as claude-code,"),
            "{shown:?}"
        );
        assert!(
            shown.iter().all(|(who, _)| !who.contains("pid")),
            "{shown:?}"
        );
    }

    // Out of turn: Claude Code's prompt is not the one on screen, so its
    // decision is refused and decides nothing.
    assert!(
        answer_pending_prompt_as(
            &mut chrome.borrow_mut(),
            claude_prompt,
            PromptChoice::Allow,
            None
        )
        .is_err()
    );
    // An answer to no prompt at all decides nothing either.
    assert!(
        answer_pending_prompt_as(
            &mut chrome.borrow_mut(),
            uuid::Uuid::new_v4(),
            PromptChoice::Allow,
            None
        )
        .is_err()
    );
    consent.pump(&chrome);
    assert!(t.try_recv().is_err() && claude.try_recv().is_err());
    assert!(tasks_start_runs(&chrome).is_empty());

    // The person allows `t`, the prompt on screen.
    answer_pending_prompt_as(
        &mut chrome.borrow_mut(),
        t_prompt,
        PromptChoice::Allow,
        None,
    )
    .expect("t's prompt is on screen");
    consent.pump(&chrome);
    assert_eq!(tasks_start_runs(&chrome), ["t in this session"]);
    consent.pump(&chrome);
    assert_eq!(admitted(&mut t, "t").room, room(&chrome));
    assert!(
        matches!(
            claude.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ),
        "Claude Code was decided by t's answer"
    );
    assert_eq!(consent.waiting(), 1);

    // The same answer again, now that Claude Code's prompt is on screen,
    // is stale and admits nobody.
    assert_eq!(front_id(&chrome), claude_prompt);
    for choice in [PromptChoice::Allow, PromptChoice::Session] {
        assert!(
            answer_pending_prompt_as(&mut chrome.borrow_mut(), t_prompt, choice, None).is_err(),
            "a repeated answer was taken"
        );
    }
    consent.pump(&chrome);
    assert!(claude.try_recv().is_err());
    assert!(tasks_start_runs(&chrome).is_empty());

    // Claude Code is decided only by its own prompt.
    answer_pending_prompt_as(
        &mut chrome.borrow_mut(),
        claude_prompt,
        PromptChoice::Deny,
        None,
    )
    .expect("Claude Code's prompt is on screen");
    consent.pump(&chrome);
    assert!(matches!(claude.try_recv(), Ok(Err(_))));
    assert!(tasks_start_runs(&chrome).is_empty());
    assert_eq!(consent.waiting(), 0);
    assert!(chrome.borrow().pending_grants.is_empty());
}

/// The person's click goes to the prompt on screen, which names its client.
/// When that client leaves between the prompt appearing and the click being
/// handled, the click is dropped: it never falls through to the next client
/// in the queue, whose prompt the person has not seen.
#[test]
fn a_click_admits_only_the_client_its_prompt_names_and_never_the_next_one() {
    use ui::synthetic_input::{cursor_moved, mouse_press};
    use ui::testing::dispatch;
    let chrome = chrome_with_room();
    let (sender, mut consent) = channel(None, None);
    let mut host =
        crate::shell::overlay_host::ShellOverlayHost::new(Box::new(ui::widget::Container::new()))
            .with_chrome(chrome.clone());
    let click_allow = |host: &mut crate::shell::overlay_host::ShellOverlayHost| -> String {
        let prompt = host
            .permission_prompt_mut_for_tests()
            .expect("a prompt is on screen");
        prompt.set_rect(vello::kurbo::Rect::new(0.0, 0.0, 600.0, 80.0));
        let center = prompt.allow_rect().center();
        dispatch(prompt, &cursor_moved(center.x, center.y));
        dispatch(prompt, &mouse_press());
        assert_eq!(prompt.pending_choice, Some(PromptChoice::Allow));
        prompt.app_id.clone()
    };

    // `t` asks first, Claude Code right behind it.
    let t = ask(&sender, relayed("t", 100));
    let mut claude = ask(&sender, relayed("claude-code", 200));
    consent.pump(&chrome);
    host.tick_permission_prompt_for_tests();

    // The person sees and clicks `t`'s prompt, but `t` leaves before the
    // click is handled.
    let clicked = click_allow(&mut host);
    assert!(clicked.starts_with("t ("), "{clicked}");
    drop(t);
    consent.pump(&chrome);
    host.tick_permission_prompt_for_tests();
    consent.pump(&chrome);
    assert!(
        matches!(
            claude.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ),
        "the click on t's prompt decided Claude Code"
    );
    assert!(tasks_start_runs(&chrome).is_empty());

    // Claude Code's own prompt is now on screen, naming it, and only a
    // click on it admits it.
    host.tick_permission_prompt_for_tests();
    let clicked = click_allow(&mut host);
    assert!(clicked.starts_with("claude-code ("), "{clicked}");
    host.tick_permission_prompt_for_tests();
    consent.pump(&chrome);
    assert_eq!(tasks_start_runs(&chrome), ["claude-code in this session"]);
    consent.pump(&chrome);
    admitted(&mut claude, "claude-code");
}
