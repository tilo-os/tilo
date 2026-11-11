//! A person's own agent on a computer of theirs (`tilo login`, then
//! `tilo mcp`): its session says it runs an agent, as `tilo login` makes it
//! say, and from then on the host serves it as that agent acting for the
//! person, within an agent's bounds. What it lists is what it can run;
//! what people alone are offered it is not; what it writes is by the agent,
//! for the person; and an action that asks first goes to the person.
use super::*;
use serde_json::{Value, json};
use tilo_runtime::mcp::{McpServer, Surface};
use tilo_session_client::Outbox;
use tilo_session_core::remote::SessionActions;

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
    let _ = tilo_session_client::device::run_as_agent(&agent.peer, "Claude Code").await;
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

/// The resources a `tilo_ls` listing names, by the name a path takes: a row
/// is `Notebook  notes.notebook  [26 actions]`; the app's own head row
/// (`notes  (so.tilo.notes)`) and its actions name none.
pub(super) fn listed_resources(listing: &str) -> Vec<String> {
    listing
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let columns: Vec<&str> = line
                .split("  ")
                .filter(|column| !column.is_empty())
                .collect();
            (columns.len() >= 2 && !columns[1].trim().starts_with('('))
                .then(|| columns[0].trim().trim_end_matches('/').to_owned())
        })
        .collect()
}

/// The note as the page holds it: the text of every paragraph of the note
/// titled `title`, in order, in each Notes record the page received.
pub(super) fn page_paragraphs(browser: &mut Browser, title: &str) -> Vec<Vec<String>> {
    browser.pump();
    let routes: Vec<DocumentRoute> = browser
        .documents
        .keys()
        .filter(|route| route.app_id == NOTES)
        .cloned()
        .collect();
    routes
        .iter()
        .map(|route| {
            browser
                .mount(route)
                .read(|doc| {
                    notes::read::list_pages(doc)
                        .into_iter()
                        .filter(|page| page.title == title)
                        .flat_map(|page| notes::read::page_block_snapshots(doc, page.id))
                        .map(|(_, text)| text)
                        .filter(|text| !text.is_empty())
                        .collect()
                })
                .unwrap()
        })
        .collect()
}

/// The work group each entry the agent's changes open carries (a write
/// continuing the one before it, as a batch's later steps do, says it once).
fn agent_groups(changes: &[([u8; 32], Vec<u8>)]) -> Vec<Option<tilo_actions::WorkGroup>> {
    let mut doc = automerge::AutoCommit::new();
    for (_, bytes) in changes {
        let _ = tilo_automerge::ApplyIncoming::load_incoming(&mut doc, bytes);
    }
    doc.get_changes(&[])
        .iter()
        .filter_map(|change| change.message())
        .filter_map(tilo_actions::ActionContext::from_change_message)
        .filter(|context| {
            context
                .invoker
                .as_ref()
                .is_some_and(|invoker| invoker.kind == tilo_actions::ActorKind::Agent)
        })
        .map(|context| context.group)
        .collect()
}

/// The agent as the workspace's listing leads it, on an app nobody started:
/// it starts the app as the listing says to (and asking again once it runs
/// still answers), writes in a note with one batched call and then one more,
/// reads the note back, and everything it wrote is one run of the agent's,
/// the connection's, which History lists with its changes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_persons_own_agent_starts_a_stopped_app_writes_and_reads_as_one_run() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let computer = enrol(
        &root,
        "Claude Code on build-box",
        DeviceCapabilities::member(),
    );
    let state = support::scratch_dir();
    let hosted = harness::hosted::start(
        &root,
        &host,
        state.path(),
        Options::offering(common::catalogue, ModelChoice::Scripted),
    )
    .await;
    let _participant = participant::Participant::start(&hosted.relay, &root).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let agent = Browser::open(&hosted, &computer, &host).await;
    let _ = tilo_session_client::device::run_as_agent(&agent.peer, "Claude Code").await;
    let collab_types::Principal::Person(person) = collab_types::Principal::person_from_key(
        collab_types::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) else {
        panic!("a key names a person")
    };
    let server = McpServer::new(
        SessionActions::new(&agent.peer, agent.workspace, host.cert.device_public_key),
        WORKSPACE.into(),
        person,
    )
    .with_surface(Surface::Agent)
    .with_agent_run("run-7", "Claude Code");

    // Nobody started Notes: the listing says how to, and that works.
    let (stopped, failed) = tool(&server, "tilo_ls", json!({"path": "notes"})).await;
    assert!(
        !failed && stopped.contains("not running") && stopped.contains("notes#open"),
        "{stopped}"
    );
    let (opened, failed) = tool(&server, "tilo_do", json!({"path": "notes#open"})).await;
    assert!(!failed, "notes#open as the listing said: {opened}");
    // Said again once it runs, the same call still answers what is there.
    let (again, failed) = tool(&server, "tilo_do", json!({"path": "notes#open"})).await;
    assert!(!failed, "notes#open on running notes: {again}");

    // A note, found down the app's listing as an agent reads it.
    let (made, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": "notes#new-note", "args": {"title": "Launch notes"}}),
    )
    .await;
    assert!(!failed, "notes#new-note: {made}");
    let (listing, failed) = tool(&server, "tilo_ls", json!({"path": "notes"})).await;
    assert!(!failed, "{listing}");
    let notebook = listed_resources(&listing)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("notes lists a notebook: {listing}"));
    let (pages, failed) = tool(
        &server,
        "tilo_ls",
        json!({"path": format!("notes/{notebook}")}),
    )
    .await;
    assert!(!failed, "{pages}");
    let page = listed_resources(&pages)
        .into_iter()
        .find(|page| page.contains("Launch notes"))
        .unwrap_or_else(|| panic!("the notebook lists the note: {pages}"));
    let note = format!("notes/{notebook}/{page}");
    let (parts, failed) = tool(&server, "tilo_ls", json!({"path": note})).await;
    assert!(!failed, "{parts}");
    let mut write_at = None;
    for at in std::iter::once(note.clone()).chain(
        listed_resources(&parts)
            .into_iter()
            .map(|part| format!("{note}/{part}")),
    ) {
        let (actions, _) = tool(&server, "tilo_ls", json!({"path": at, "kind": "actions"})).await;
        if listed_actions(&actions)
            .iter()
            .any(|action| action == "append-paragraph")
        {
            write_at = Some(at);
            break;
        }
    }
    let write_at = write_at.unwrap_or_else(|| panic!("the note publishes a write: {parts}"));
    let write = format!("{write_at}#append-paragraph");

    // One batched call writing three paragraphs, then one more call.
    let lines = [
        "Launch notes, from Claude Code.",
        "Each line lands in the workspace as I write it.",
        "Following me puts this window in front of you.",
    ];
    let (batched, failed) = tool(
        &server,
        "tilo_do",
        json!({"calls": lines
            .iter()
            .map(|text| json!({"path": write, "args": {"text": text}}))
            .collect::<Vec<_>>()}),
    )
    .await;
    assert!(!failed, "the batched write: {batched}");
    let (single, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": write, "args": {"text": "A second invocation, a moment later."}}),
    )
    .await;
    assert!(!failed, "the single write: {single}");
    let (read, failed) = tool(&server, "tilo_read", json!({"path": note})).await;
    assert!(!failed, "the note read back: {read}");
    for line in lines
        .iter()
        .copied()
        .chain(["A second invocation, a moment later."])
    {
        assert!(read.contains(line), "the note reads {line:?}: {read}");
    }
    // What the host answered `ok` reaches the person's page too, every line
    // in the order it was written: an answer is a promise about the record,
    // not about the host's own copy of it.
    let sent: Vec<&str> = lines
        .iter()
        .copied()
        .chain(["A second invocation, a moment later."])
        .collect();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let seen = page_paragraphs(&mut browser, "Launch notes");
        if seen.iter().any(|texts| texts == &sent) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the page never read the note as written ({sent:?}): {seen:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Every change it made is the connection's run, as History groups an
    // agent's runs.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        browser.pump();
        let groups: Vec<_> = browser
            .documents
            .iter()
            .filter(|(route, _)| route.app_id == NOTES)
            .flat_map(|(_, changes)| agent_groups(changes))
            .collect();
        if groups.len() >= 3 {
            assert!(
                groups
                    .iter()
                    .all(|group| group.as_ref().is_some_and(|group| {
                        group.kind == "agent-run"
                            && group.id.starts_with("agent-run:")
                            && group.label == "Claude Code"
                    })),
                "every change the agent made is its run: {groups:?}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the agent's writes never arrived: {groups:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
