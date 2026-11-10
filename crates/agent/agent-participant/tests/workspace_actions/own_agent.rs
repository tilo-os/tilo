//! A person's own agent on a computer of theirs (`tilo login`, then
//! `tilo mcp`): its session says it runs an agent, as `tilo login` makes it
//! say, and from then on the host serves it as that agent acting for the
//! person, within an agent's bounds. What it lists is what it can run;
//! what people alone are offered it is not; what it writes is by the agent,
//! for the person; and an action that asks first goes to the person.
use super::*;
use serde_json::{Value, json};
use tilo_runtime::mcp::{McpServer, Surface};
use tilo_session_host::session_actions::SessionActions;

const NOTES: &str = "so.tilo.notes";
const TASKS: &str = "so.tilo.tasks";

/// What a tool call answers, and whether it failed.
async fn tool(server: &McpServer<SessionActions>, name: &str, arguments: Value) -> (String, bool) {
    let answer = server
        .handle(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                       "params": {"name": name, "arguments": arguments}}))
        .await
        .expect("a call is answered");
    let result = &answer["result"];
    let text = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (
        if text.is_empty() {
            answer.to_string()
        } else {
            text
        },
        result["isError"] == true || answer.get("error").is_some(),
    )
}

/// The action ids a `tilo_ls` listing names (`#<action>` before its
/// parameters), in order.
fn listed_actions(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|line| line.split_once('#'))
        .map(|(_, rest)| {
            rest.chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect::<String>()
        })
        .filter(|action| !action.is_empty())
        .collect()
}

/// Who `bytes`' changes recorded acting, from the actions they carry.
fn invokers(changes: &[([u8; 32], Vec<u8>)]) -> Vec<tilo_actions::ActionInvoker> {
    let mut doc = automerge::AutoCommit::new();
    for (_, bytes) in changes {
        let _ = tilo_automerge::ApplyIncoming::load_incoming(&mut doc, bytes);
    }
    doc.get_changes(&[])
        .iter()
        .filter_map(|change| change.message())
        .filter_map(tilo_actions::ActionContext::from_change_message)
        .filter_map(|context| context.invoker)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_persons_own_agent_runs_what_it_lists_as_an_agent_and_asks_them_first() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let laptop_device = enrol(&root, "laptop", DeviceCapabilities::primary());
    let computer = enrol(
        &root,
        "Claude Code on build-box",
        DeviceCapabilities::member(),
    );
    let state = support::scratch_dir();
    let shell_state = tempfile::tempdir().unwrap();
    let hosted = harness::hosted::start(
        &root,
        &host,
        state.path(),
        Options {
            shell: Some(
                tilo_relay::agents::host_shells(shell_state.path().join("shells")).unwrap(),
            ),
            ..Options::offering(common::catalogue, ModelChoice::Scripted)
        },
    )
    .await;
    let _participant = participant::Participant::start(&hosted.relay, &root).await;
    // The person's laptop answers what the host asks them about the agent.
    let _asked =
        harness::hosted::Laptop::new(["Claude Code".to_owned()], harness::hosted::Unlisted::Deny)
            .spawn(&hosted.relay, root.clone(), &laptop_device);
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let notes = browser.create(NOTES);
    let tasks = browser.create(TASKS);
    let books = browser.create(EBOOK);
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();

    // The agent's computer enters as a device of the person's and says, as
    // `tilo login` makes it, that its session runs the agent.
    let agent = Browser::open(&hosted, &computer, &host).await;
    let _ = tilo_session_host::device::say_here(
        &agent.peer,
        agent.workspace,
        "Claude Code",
        Vec::new(),
        true,
    )
    .await;
    let collab_types::Principal::Person(person) = collab_types::Principal::person_from_key(
        collab_types::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) else {
        panic!("a key names a person")
    };
    let server = McpServer::new(
        SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key),
        WORKSPACE.into(),
        person.clone(),
    )
    .with_surface(Surface::Agent)
    .with_agent_run("run-3", "Claude Code");

    // Terminal: what it lists, it runs, as listed; a session first, then
    // that session's own listing.
    let (ran, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": "terminal#run", "args": {"command": "echo first"}}),
    )
    .await;
    assert!(!failed && ran.contains("first"), "{ran}");
    let (terminal, failed) = tool(&server, "tilo_ls", json!({"path": "terminal"})).await;
    assert!(!failed, "{terminal}");
    let (session, failed) = tool(
        &server,
        "tilo_ls",
        json!({"path": "terminal/Terminal session"}),
    )
    .await;
    assert!(!failed, "{session}");
    eprintln!("tilo_ls terminal/Terminal session:\n{session}");
    assert!(
        listed_actions(&session)
            .iter()
            .any(|action| action == "run"),
        "{session}"
    );
    // As listed, on the session, and as the app's own `run`.
    for path in ["terminal/Terminal session#run", "terminal#run"] {
        let (again, failed) = tool(
            &server,
            "tilo_do",
            json!({"path": path, "args": {"command": "echo listed"}}),
        )
        .await;
        assert!(
            !failed && again.contains("listed"),
            "{path} as listed: {again}"
        );
    }

    // Tasks: what people alone are offered (planning an issue) is not
    // listed to the agent, asked over its own session.
    let session = SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key);
    let anyone = tilo_actions::remote::Caller::local_person("unused");
    let offered = tilo_actions::remote::ActionHost::query(
        &session,
        &anyone,
        CatalogueQuery {
            provider: Some(record_provider(&tasks)),
            max_results: 500,
            ..CatalogueQuery::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    let actions: Vec<_> = offered
        .rows
        .iter()
        .filter_map(|row| row.action.as_ref())
        .collect();
    assert!(
        !actions.is_empty(),
        "the agent is listed the tracker's actions"
    );
    assert!(
        actions
            .iter()
            .all(|action| action.contract.reaches_agents()),
        "nothing for people alone is listed to an agent: {:?}",
        actions
            .iter()
            .filter(|action| !action.contract.reaches_agents())
            .map(|action| &action.address)
            .collect::<Vec<_>>()
    );

    // Notes: every action it lists on the notebook is one the host runs
    // for it, and the new note it makes is by the agent, for the person.
    let (notes_listing, failed) = tool(&server, "tilo_ls", json!({"path": "notes"})).await;
    assert!(!failed, "{notes_listing}");
    eprintln!("tilo_ls notes:\n{notes_listing}");
    assert!(
        listed_actions(&notes_listing)
            .iter()
            .any(|action| action == "new-note"),
        "{notes_listing}"
    );
    let (made, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": "notes#new-note", "args": {"title": "Launch plan"}}),
    )
    .await;
    assert!(!failed, "notes#new-note as listed: {made}");
    eprintln!("notes#new-note: {made}");
    let (wrote, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": "notes#append-to-note",
               "args": {"title": "Launch plan", "markdown": "Ship it."}}),
    )
    .await;
    assert!(!failed, "notes#append-to-note as listed: {wrote}");
    let deadline = tilo_test_env::hang_deadline();
    loop {
        browser.pump();
        let by = invokers(browser.documents.get(&notes).map_or(&[][..], Vec::as_slice));
        if by.iter().any(|invoker| {
            invoker.kind == tilo_actions::ActorKind::Agent
                && invoker.actor == "agent:Claude Code"
                && invoker.on_behalf_of.as_deref() == Some(person.as_str())
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the note never arrived as the agent's, for the person: {by:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Removing a book asks first: the agent's own word does not do, and the
    // person, asked on their laptop, allows it.
    let books_provider = record_provider(&books);
    let remove = || {
        request(
            &books_provider,
            ActionAddress::new("", "remove-from-library"),
            InvokeArgument::Json(json!({"book": "book:dusk"}).as_object().unwrap().clone()),
        )
    };
    let own_word = SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key);
    let caller = tilo_actions::remote::Caller::local_person("unused");
    let refused = tilo_actions::remote::ActionHost::invoke(
        &own_word,
        &caller,
        InvokeRequest {
            confirmation: Some(Confirmation::Confirmed),
            ..remove()
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(&refused, Err(ActionHostError::Refused { reason }) if reason.contains("person")),
        "an agent confirms nothing by its own word: {refused:?}"
    );
    let forged = tilo_actions::remote::ActionHost::invoke(
        &own_word,
        &caller,
        InvokeRequest {
            confirmation: Some(Confirmation::Witnessed("x".into())),
            ..remove()
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(&forged, Err(ActionHostError::Refused { .. })),
        "a token the host did not issue confirms nothing: {forged:?}"
    );
    let asked = remove();
    let accepted = tilo_actions::remote::ActionHost::invoke(&own_word, &caller, asked.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(accepted.status, ReceiptStatus::Accepted),
        "it waits for the person: {accepted:?}"
    );
    // Asked again under its id, the host answers how it stands.
    loop {
        let now = tilo_actions::remote::ActionHost::invoke(&own_word, &caller, asked.clone())
            .await
            .unwrap();
        match now {
            Ok(receipt) if matches!(receipt.status, ReceiptStatus::Completed { .. }) => break,
            Ok(receipt) if matches!(receipt.status, ReceiptStatus::Accepted) => {}
            other => panic!("the person allowed it, yet it did not run: {other:?}"),
        }
        assert!(
            Instant::now() < deadline,
            "the person's answer never ran it"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
