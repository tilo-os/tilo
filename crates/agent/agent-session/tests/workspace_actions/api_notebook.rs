//! An agent rearranges a notebook through the API, and a person entering the
//! workspace afterwards on a new page reads it as the agent left it: every
//! write the host made to the one record reaches the page, those after the
//! host folded the record's history included.

use super::api::ISSUER;
use super::api_on_server::{accept_invitation, api_on_server, approved_key, headers, json_of};
use super::resource_support::open_admitted_peer;
use super::*;
use serde_json::{Value, json};

/// What an agent brings into the notebook: `count` Markdown files under a few
/// folders, by their path, each a few paragraphs long.
fn markdown_files(count: usize) -> Vec<(String, String)> {
    (0..count)
        .map(|file| {
            let name = format!("docs/area-{}/note-{file}.md", file % 4);
            let paragraph = format!("Paragraph of note {file}, written to be moved. ");
            (name, format!("# Note {file}\n\n{}\n", paragraph.repeat(20)))
        })
        .collect()
}

/// `POST path` with `body` under the key `token`, answered `200`.
async fn post(api: &Arc<tilo_api::Server>, token: &str, path: &str, body: &Value) -> Value {
    let bearer = format!("Bearer {token}");
    let pairs = [
        ("authorization", bearer.as_str()),
        ("content-type", "application/json"),
    ];
    let (status, answer) = json_of(
        api.dispatch(
            &hyper::Method::POST,
            path,
            "",
            &headers(&pairs),
            body.to_string().as_bytes(),
        )
        .await,
    )
    .await;
    assert!((200..300).contains(&status), "{path} {body}: {answer}");
    answer
}

/// Each note of the notebook `reader` holds, by id: its title and the note it
/// sits under.
fn notes_held(
    reader: &Browser,
    workspace: &str,
) -> Option<BTreeMap<String, (String, Option<String>)>> {
    let parts = super::resource_support::notes_parts(reader.peer.engine(), workspace);
    if parts.is_empty() {
        return None;
    }
    Some(
        notes::Pages::of(&parts)
            .iter()
            .map(|(id, note)| {
                (
                    id.to_string(),
                    (
                        note.title.clone(),
                        note.parent.as_ref().map(ToString::to_string),
                    ),
                )
            })
            .collect(),
    )
}

/// An agent brings a folder of Markdown into a workspace's notebook through
/// the API, one note per file titled by its path; the host folds the record's
/// history while the agent reads it. Then the agent files every note under
/// folder notes named after its directories and renames it to its file's
/// name: over a hundred more writes to the one record, each by the host. A
/// person entering the workspace afterwards on a new page reads the notebook
/// as the agent left it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_page_reads_a_notebook_an_agent_rearranged_through_the_api() {
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let token = approved_key(&on_server, &ada, &page, "Mover").await;
    let made = post(&api, &token, "/v1/workspaces", &json!({"name": "Company"})).await;
    let workspace = made["id"].as_str().unwrap().to_owned();
    let base = format!("/v1/workspaces/{workspace}");
    post(
        &api,
        &token,
        &format!("{base}/resources/notes/actions/open"),
        &json!({}),
    )
    .await;
    let notebook = format!("{base}/resources/notes/Notes/actions");
    let act = |action: &str, args: Value| {
        let path = format!("{notebook}/{action}");
        let (api, token) = (&api, &token);
        async move { post(api, token, &path, &args).await }
    };

    let files = markdown_files(40);
    let started = Instant::now();
    for (name, markdown) in &files {
        act(
            "import-markdown",
            json!({"title": name, "markdown": markdown, "images": []}),
        )
        .await;
    }
    eprintln!("imported {} notes in {:?}", files.len(), started.elapsed());
    // The agent reads what it brought in before it files it: the record is quiet
    // meanwhile, long enough for the host to fold its history.
    tokio::time::sleep(Duration::from_secs(5)).await;
    let mut folders = BTreeMap::<String, String>::new();
    let mut expected = BTreeMap::<String, String>::new();
    for (name, _) in &files {
        let found = act("find-page", json!({"title": name})).await;
        let note = found["result"]["pages"][0]["page"]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: {found}"))
            .to_owned();
        let (directory, file) = name.rsplit_once('/').unwrap();
        let mut parent: Option<String> = None;
        let mut at = String::new();
        for part in directory.split('/') {
            at = if at.is_empty() {
                part.to_owned()
            } else {
                format!("{at}/{part}")
            };
            if let Some(folder) = folders.get(&at) {
                parent = Some(folder.clone());
                continue;
            }
            let made = act("new-note", json!({"title": part, "parent": parent})).await;
            let folder = made["result"]["page"]
                .as_str()
                .unwrap_or_else(|| panic!("{at}: {made}"))
                .to_owned();
            folders.insert(at.clone(), folder.clone());
            parent = Some(folder);
        }
        act("move-note", json!({"page": note, "parent": parent})).await;
        let title = file.trim_end_matches(".md");
        act("rename", json!({"page": note, "title": title})).await;
        expected.insert(note, title.to_owned());
    }
    eprintln!(
        "filed {} notes under {} folders in {:?}",
        expected.len(),
        folders.len(),
        started.elapsed()
    );

    // A person enters the workspace on a new page.
    let invited = post(
        &api,
        &token,
        &format!("{base}/members/invitations"),
        &json!({"role": "editor"}),
    )
    .await;
    let link = invited["link"]
        .as_str()
        .unwrap_or_else(|| panic!("{invited}"))
        .to_owned();
    let (rosa, _) = tilo_user_identity::UserIdentity::generate("Rosa", 1);
    let (rosa_page, _) = accept_invitation(&on_server, &link, &rosa, "Rosa").await;
    let joined = Instant::now();
    let reader =
        open_admitted_peer(&on_server.hosted, &rosa_page, &on_server.host, &workspace).await;
    let deadline = joined + Duration::from_secs(60);
    loop {
        let left: Vec<String> = notes_held(&reader, &workspace).map_or_else(
            || vec!["the notebook".into()],
            |notes| {
                expected
                    .iter()
                    .filter(|(id, title)| {
                        notes
                            .get(*id)
                            .is_none_or(|(held, parent)| held != *title || parent.is_none())
                    })
                    .map(|(_, title)| title.clone())
                    .collect()
            },
        );
        if left.is_empty() {
            eprintln!(
                "the new page read the notebook {:?} after joining",
                joined.elapsed()
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{} of {} notes still read as before the agent filed them on the new page, {:?} \
             after it joined: {:?}",
            left.len(),
            expected.len(),
            joined.elapsed(),
            &left[..left.len().min(5)]
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    reader.peer.close();
}

/// A page that joins after an agent wrote into a workspace's notebook, and
/// then opens Notes on the workspace's own record — a record it does not
/// hold — asks its host for it, as a window opening Notes does. The host
/// answers with the record's frames, and the page reads what the agent put
/// there.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fresh_page_opens_notes_and_reads_what_the_agent_wrote() {
    use tilo_session_client::{Cmd, Event};

    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let token = approved_key(&on_server, &ada, &page, "Writer").await;
    let made = post(&api, &token, "/v1/workspaces", &json!({"name": "Company"})).await;
    let workspace = made["id"].as_str().unwrap().to_owned();
    let base = format!("/v1/workspaces/{workspace}");
    post(
        &api,
        &token,
        &format!("{base}/resources/notes/actions/open"),
        &json!({}),
    )
    .await;
    post(
        &api,
        &token,
        &format!("{base}/resources/notes/Notes/actions/new-note"),
        &json!({"title": "Supplier contract"}),
    )
    .await;

    // A person enters the workspace on a page of their own; nobody opened
    // Notes there.
    let invited = post(
        &api,
        &token,
        &format!("{base}/members/invitations"),
        &json!({"role": "editor"}),
    )
    .await;
    let link = invited["link"]
        .as_str()
        .unwrap_or_else(|| panic!("{invited}"))
        .to_owned();
    let (rosa, _) = tilo_user_identity::UserIdentity::generate("Rosa", 1);
    let (rosa_page, _) = accept_invitation(&on_server, &link, &rosa, "Rosa").await;
    let reader =
        open_admitted_peer(&on_server.hosted, &rosa_page, &on_server.host, &workspace).await;

    // What a window opening Notes asks for: the workspace's own record of
    // the app, which this page does not hold.
    let ask = tilo_peer_state::document_frame::DocumentRoute {
        app_id: "so.tilo.notes".into(),
        cluster_id: workspace.clone(),
        document: String::new(),
    };
    reader
        .peer
        .send(Cmd::OpenDocumentAt(
            WorkspaceId::parse(&workspace).unwrap(),
            ask.clone(),
        ))
        .unwrap();
    // And the newest part of its notes, which the window's chain asks for by
    // the chain's head.
    reader
        .peer
        .send(Cmd::OpenDocumentAt(
            WorkspaceId::parse(&workspace).unwrap(),
            tilo_peer_state::document_frame::DocumentRoute {
                document: notes::part::chain().chain_head().as_str().to_owned(),
                ..ask.clone()
            },
        ))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut frames = None;
    loop {
        for event in reader.events.drain() {
            if let Event::DocumentOpened {
                route: Some(route),
                answer: tilo_session_client::DocumentAnswer::Sent { frames: sent },
                ..
            } = event
                && route == ask
            {
                frames = Some(sent);
            }
        }
        let held = notes_held(&reader, &workspace).is_some_and(|notes| {
            notes
                .values()
                .any(|(title, _)| title == "Supplier contract")
        });
        if frames.is_some() && held {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the page never read the agent's note: the host answered {frames:?}, the notebook held it: {held}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        frames.is_some_and(|sent| sent > 0),
        "the host sent the notebook's frames: {frames:?}"
    );
    reader.peer.close();
}

/// Note `title`'s body as the notebook `reader` holds it, as Markdown.
fn note_body(reader: &Browser, workspace: &str, title: &str) -> Option<String> {
    let docs = super::resource_support::notes_part_docs(reader.peer.engine(), workspace);
    let (page, doc) = docs.iter().find_map(|(_, doc)| {
        let part = <notes::PagePart as ui::tilo_resource::Resource>::read(doc).ok()?;
        let (id, _) = part.pages.iter().find(|(_, note)| note.title == title)?;
        Some((notes::notebook::page_of(id)?, doc))
    })?;
    let body = notes::part::body_object(doc, page)?;
    let containers = tilo_doc::model::open(doc, &body).ok()?;
    let markdown = tilo_doc_codecs::provider_for_ext("md")?
        .export(
            tilo_doc::DocRef {
                doc,
                c: &containers,
            },
            &tilo_content::NoContent,
        )
        .ok()?;
    Some(String::from_utf8_lossy(&markdown).into_owned())
}

/// An agent rewrites a note's whole body with `write-note`, as the host does
/// when the agent wrote the note's `.md` file in the workspace's folder: the
/// note reads as the agent wrote it, on a page that joins afterwards. The
/// workspace takes a change only of a verb the app's committed surface
/// publishes, so a verb missing there is a change no page ever reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agents_write_note_changes_the_notes_body() {
    let on_server = api_on_server(ISSUER).await;
    let api = Arc::clone(&on_server.api);
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let token = approved_key(&on_server, &ada, &page, "Writer").await;
    let made = post(&api, &token, "/v1/workspaces", &json!({"name": "Company"})).await;
    let workspace = made["id"].as_str().unwrap().to_owned();
    let base = format!("/v1/workspaces/{workspace}");
    post(
        &api,
        &token,
        &format!("{base}/resources/notes/actions/open"),
        &json!({}),
    )
    .await;
    let notebook = format!("{base}/resources/notes/Notes/actions");
    let made = post(
        &api,
        &token,
        &format!("{notebook}/new-note"),
        &json!({"title": "Terms"}),
    )
    .await;
    let note = made["result"]["page"]
        .as_str()
        .unwrap_or_else(|| panic!("{made}"))
        .to_owned();
    post(
        &api,
        &token,
        &format!("{notebook}/append-to-note"),
        &json!({"title": "Terms", "markdown": "Payment due net 30.\n\nLate fees apply."}),
    )
    .await;
    post(
        &api,
        &token,
        &format!("{notebook}/write-note"),
        &json!({
            "page": note,
            "markdown": "Payment due net 30.\n\nLate fees apply after 10 days.\n",
        }),
    )
    .await;

    let invited = post(
        &api,
        &token,
        &format!("{base}/members/invitations"),
        &json!({"role": "editor"}),
    )
    .await;
    let link = invited["link"]
        .as_str()
        .unwrap_or_else(|| panic!("{invited}"))
        .to_owned();
    let (rosa, _) = tilo_user_identity::UserIdentity::generate("Rosa", 1);
    let (rosa_page, _) = accept_invitation(&on_server, &link, &rosa, "Rosa").await;
    let reader =
        open_admitted_peer(&on_server.hosted, &rosa_page, &on_server.host, &workspace).await;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let body = note_body(&reader, &workspace, "Terms");
        if body
            .as_deref()
            .is_some_and(|body| body.contains("Late fees apply after 10 days."))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the note never read as the agent wrote it: {body:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    reader.peer.close();
}
