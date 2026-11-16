//! A person sets a workspace up once on the relay, as tilo.sh hosts it, and
//! branches it per customer through the API: the branch holds every note with
//! its history and the files they name, leaves the workspace's conversations
//! and its other people behind, is listed among the person's workspaces only
//! once it holds everything, and goes on apart from its source. A retry answers
//! the same branch; a key for one workspace, someone else's key and a copy that
//! fails make none.

use super::api::{ISSUER, Mcp};
use super::api_on_relay::{
    OnRelay, accept_invitation, api_on_relay, approved_key, headers, json_of,
};
use super::hosted_documents::application_pdf;
use super::*;
use http_body_util::BodyExt as _;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;
use tilo_api::Server;
use tilo_relay::workspaces::{BranchContent, Branched, Workspaces};

/// A key's calls of `/v1`.
struct Key<'a> {
    api: &'a Arc<Server>,
    token: String,
}

impl Key<'_> {
    async fn send(
        &self,
        method: hyper::Method,
        path: &str,
        body: &[u8],
        extra: &[(&'static str, &str)],
    ) -> tilo_api::Reply {
        let bearer = format!("Bearer {}", self.token);
        let mut pairs = vec![
            ("authorization", bearer.as_str()),
            ("content-type", "application/json"),
        ];
        pairs.extend_from_slice(extra);
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        self.api
            .dispatch(&method, path, query, &headers(&pairs), body)
            .await
    }

    async fn get(&self, path: &str) -> (u16, Value) {
        json_of(self.send(hyper::Method::GET, path, b"", &[]).await).await
    }

    async fn post(&self, path: &str, body: &Value, extra: &[(&'static str, &str)]) -> (u16, Value) {
        let body = body.to_string();
        json_of(
            self.send(hyper::Method::POST, path, body.as_bytes(), extra)
                .await,
        )
        .await
    }

    /// `POST path` with `body`, which must succeed: its answer.
    async fn did(&self, path: &str, body: &Value) -> Value {
        let (status, answer) = self.post(path, body, &[]).await;
        assert!((200..300).contains(&status), "{path} {body}: {answer}");
        answer
    }
}

/// A person on `on_relay`, their page and a key reaching all their workspaces.
async fn person(on_relay: &OnRelay, name: &str) -> (SigningKey, String) {
    let (root, _) = tilo_user_identity::UserIdentity::generate(name, 1);
    let page = enrol(
        &root,
        &format!("{name}'s browser"),
        DeviceCapabilities::member(),
    );
    let token = approved_key(on_relay, &root, &page, &format!("{name}'s backend")).await;
    (root, token)
}

/// The notebook of `workspace` as its host keeps it, loaded.
async fn notebook(workspaces: &Workspaces, workspace: &str) -> Option<automerge::AutoCommit> {
    let bytes = workspaces
        .record_document(workspace, "so.tilo.notes", "")
        .await
        .ok()??;
    automerge::AutoCommit::load(&bytes).ok()
}

/// The titles of the notes in `workspace`, as its host keeps them.
async fn titles(workspaces: &Workspaces, workspace: &str) -> BTreeSet<String> {
    let Some(doc) = notebook(workspaces, workspace).await else {
        return BTreeSet::new();
    };
    <notes::notebook::Notebook as ui::tilo_resource::Resource>::read(&doc)
        .map(|notebook| {
            notebook
                .pages
                .values()
                .map(|note| note.title.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Wait until `workspace`'s host keeps a note titled `title`.
async fn holds_note(workspaces: &Workspaces, workspace: &str, title: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let held = titles(workspaces, workspace).await;
        if held.contains(title) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{workspace} never held the note {title:?}: {held:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The conversations `workspace`'s host keeps: Chat's records that are not the
/// workspace's own (its emoji, its agents).
async fn conversations(workspaces: &Workspaces, workspace: &str) -> Vec<(String, String)> {
    workspaces
        .records(workspace)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|(app, record)| {
            app == "so.tilo.chat" && collab_types::workspace_record(app, record).is_none()
        })
        .collect()
}

/// The people `workspace` has now, but the relay's operator, who governs it.
async fn people(
    workspaces: &Workspaces,
    on_relay: &OnRelay,
    workspace: &str,
) -> BTreeSet<[u8; 32]> {
    let operator = on_relay.operator.verifying_key().to_bytes();
    workspaces
        .summary(workspace)
        .await
        .unwrap()
        .members
        .iter()
        .filter(|member| !member.removed)
        .filter_map(collab_types::Member::root)
        .filter(|root| *root != operator)
        .collect()
}

/// The ids of the person's workspaces named `name`, as `key` lists them.
async fn named(key: &Key<'_>, name: &str) -> Vec<String> {
    let (status, listed) = key.get("/v1/workspaces").await;
    assert_eq!(status, 200, "{listed}");
    listed["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["name"] == name)
        .filter_map(|row| row["id"].as_str().map(str::to_owned))
        .collect()
}

/// A workspace of `key`'s person with a notebook whose one note was renamed
/// once: its id.
async fn template(key: &Key<'_>, name: &str) -> String {
    let (status, made) = key
        .post("/v1/workspaces", &json!({"name": name}), &[])
        .await;
    assert_eq!(status, 201, "{made}");
    let workspace = made["id"].as_str().unwrap().to_owned();
    let notes = format!("/v1/workspaces/{workspace}/resources/notes");
    key.did(&format!("{notes}/actions/open"), &json!({})).await;
    let note = key
        .did(
            &format!("{notes}/Notebook/actions/new-note"),
            &json!({"title": "Checklist draft"}),
        )
        .await["result"]["page"]
        .as_str()
        .unwrap()
        .to_owned();
    key.did(
        &format!("{notes}/Notebook/actions/rename"),
        &json!({"page": note, "title": "Checklist"}),
    )
    .await;
    workspace
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_branch_holds_the_workspace_and_goes_on_apart() {
    // The story builds a host, its API and a workspace's sessions; its future goes on the
    // heap rather than into the frames of the test thread that polls it.
    Box::pin(a_customers_branch_holds_the_template_and_goes_on_apart()).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "one template's story, from its making to a customer's branch of it going its own way"
)]
async fn a_customers_branch_holds_the_template_and_goes_on_apart() {
    let on_relay = api_on_relay(ISSUER).await;
    let api = Arc::clone(&on_relay.api);
    let workspaces = on_relay.hosted.relay.workspaces().unwrap().clone();
    let (ada, token) = person(&on_relay, "Ada").await;
    let backend = Key { api: &api, token };
    let ada = ada.verifying_key().to_bytes();

    // The template: a note written and renamed, a form filed, a conversation
    // in Chat, and a reviewer invited in.
    let source = template(&backend, "Onboarding template").await;
    let base = format!("/v1/workspaces/{source}");
    let pdf = application_pdf();
    let (status, put) = json_of(
        backend
            .send(
                hyper::Method::POST,
                &format!("{base}/uploads"),
                &pdf,
                &[("content-type", "application/pdf")],
            )
            .await,
    )
    .await;
    assert_eq!(status, 201, "{put}");
    let form = put["content"].as_str().unwrap().to_owned();
    backend
        .did(
            &format!("{base}/resources/pdf-viewer/actions/open"),
            &json!({}),
        )
        .await;
    backend
        .did(
            &format!("{base}/resources/pdf-viewer/actions/import-pdf-content"),
            &json!({"content": form, "name": "application.pdf"}),
        )
        .await;
    // A conversation, as the workspace's governor keeps one: a part of Chat's
    // messages.
    workspaces
        .author_workspace_record(
            &source,
            workspaces.serving_certificate(),
            ("so.tilo.chat", chat::parts::part(0).route()),
            |doc| {
                automerge::transaction::Transactable::put(
                    doc,
                    automerge::ROOT,
                    "said",
                    "Only the template's people read this",
                )
                .map_err(|error| error.to_string())
            },
        )
        .await
        .unwrap();
    let invited = backend
        .did(
            &format!("{base}/members/invitations"),
            &json!({"role": "editor"}),
        )
        .await;
    let (rosa, _) = tilo_user_identity::UserIdentity::generate("Rosa", 1);
    accept_invitation(&on_relay, invited["link"].as_str().unwrap(), &rosa, "Rosa").await;
    let rosa = rosa.verifying_key().to_bytes();
    holds_note(&workspaces, &source, "Checklist").await;
    let deadline = Instant::now() + Duration::from_secs(60);
    while conversations(&workspaces, &source).await.is_empty() {
        assert!(
            Instant::now() < deadline,
            "the template never held its conversation"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        people(&workspaces, &on_relay, &source).await,
        BTreeSet::from([ada, rosa])
    );
    let history: Vec<automerge::ChangeHash> = notebook(&workspaces, &source)
        .await
        .unwrap()
        .get_changes(&[])
        .iter()
        .map(automerge::Change::hash)
        .collect();

    // A customer's branch of it, made once however often it is asked for.
    let mut made = workspaces.branches();
    let new = json!({"name": "Onboarding: Acme", "labels": {"customer": "acme"}});
    let retry = [("idempotency-key", "branch-acme")];
    let (status, branched) = backend
        .post(&format!("{base}/branches"), &new, &retry)
        .await;
    assert_eq!(status, 201, "{branched}");
    let branch = branched["id"].as_str().unwrap().to_owned();
    assert_ne!(branch, source);
    assert_eq!(branched["name"], "Onboarding: Acme", "{branched}");
    assert_eq!(
        branched["labels"],
        json!({"customer": "acme"}),
        "{branched}"
    );
    let replayed = backend
        .send(
            hyper::Method::POST,
            &format!("{base}/branches"),
            new.to_string().as_bytes(),
            &retry,
        )
        .await;
    assert_eq!(replayed.headers()["idempotent-replayed"], "true");
    let (_, again) = json_of(replayed).await;
    assert_eq!(
        again["id"], branched["id"],
        "a retry answers the same branch"
    );
    assert_eq!(
        named(&backend, "Onboarding: Acme").await,
        std::slice::from_ref(&branch)
    );
    let (status, labelled) = backend.get("/v1/workspaces?label[customer]=acme").await;
    assert_eq!(status, 200, "{labelled}");
    assert_eq!(labelled["workspaces"][0]["id"], json!(branch), "{labelled}");
    assert_eq!(labelled["workspaces"][0]["role"], "owner", "{labelled}");
    assert_eq!(
        labelled["workspaces"].as_array().unwrap().len(),
        1,
        "{labelled}"
    );
    // The machine the workspace runs on hears of it.
    let heard = tokio::time::timeout(Duration::from_secs(5), made.recv())
        .await
        .expect("a branch made is heard of")
        .unwrap();
    assert_eq!(
        heard,
        Branched {
            source: source.clone(),
            branch: branch.clone(),
            person: ada,
        }
    );

    // The note came along with every change of its history.
    assert!(titles(&workspaces, &branch).await.contains("Checklist"));
    let kept = notebook(&workspaces, &branch).await.unwrap();
    for change in &history {
        assert!(
            kept.clone().get_change_by_hash(change).is_some(),
            "the branch's notebook lacks the template's change {change}"
        );
    }
    // The form's bytes read in the branch, byte for byte.
    let reply = backend
        .send(
            hyper::Method::GET,
            &format!("/v1/workspaces/{branch}/content/{form}"),
            b"",
            &[],
        )
        .await;
    assert_eq!(reply.status(), 200);
    let body = reply.into_body().collect().await.unwrap().to_bytes();
    assert!(
        body.as_ref() == pdf.as_slice(),
        "the form reads in the branch"
    );
    // The conversation stayed behind, and so did the reviewer: Ada alone is
    // in it, and it is hers.
    assert_eq!(conversations(&workspaces, &branch).await, Vec::new());
    assert_eq!(
        people(&workspaces, &on_relay, &branch).await,
        BTreeSet::from([ada])
    );
    assert!(workspaces.owns(&ada, &branch));

    // From here on each goes its own way.
    let in_branch = format!("/v1/workspaces/{branch}/resources/notes/Notebook/actions/new-note");
    backend
        .did(&in_branch, &json!({"title": "Acme's contacts"}))
        .await;
    backend
        .did(
            &format!("{base}/resources/notes/Notebook/actions/new-note"),
            &json!({"title": "Next customer's checklist"}),
        )
        .await;
    holds_note(&workspaces, &branch, "Acme's contacts").await;
    holds_note(&workspaces, &source, "Next customer's checklist").await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let (branch_notes, source_notes) = (
        titles(&workspaces, &branch).await,
        titles(&workspaces, &source).await,
    );
    assert!(
        !branch_notes.contains("Next customer's checklist"),
        "{branch_notes:?}"
    );
    assert!(
        !source_notes.contains("Acme's contacts"),
        "{source_notes:?}"
    );

    // A key minted for the branch reaches it, and not the template.
    let (status, minted) = backend
        .post(
            "/v1/keys",
            &json!({"workspace": branch, "name": "Acme's agent"}),
            &[],
        )
        .await;
    assert_eq!(status, 201, "{minted}");
    let agent = Key {
        api: &api,
        token: minted["token"].as_str().unwrap().to_owned(),
    };
    let (status, read) = agent
        .get(&format!("/v1/workspaces/{branch}/resources/notes/Notebook"))
        .await;
    assert_eq!(status, 200, "{read}");
    assert!(read.to_string().contains("Checklist"), "{read}");
    let (status, refused) = agent.get(&format!("{base}/resources")).await;
    assert_eq!(
        (status, &refused["error"]["code"]),
        (403, &json!("beyond_scope")),
        "{refused}"
    );
}

/// What keeps content for a host whose disk refuses everything.
struct Refusing;

impl BranchContent for Refusing {
    fn copy(&self, _: &str, _: &str, _: &[((String, String), Vec<u8>)]) -> Result<(), String> {
        Err("the disk refused it".into())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_its_owner_branches_a_workspace_and_a_failed_branch_leaves_nothing() {
    Box::pin(only_its_owners_key_branches_and_a_failed_branch_leaves_nothing()).await;
}

#[expect(
    clippy::too_many_lines,
    reason = "every way of asking for a branch that makes one, or none"
)]
async fn only_its_owners_key_branches_and_a_failed_branch_leaves_nothing() {
    let on_relay = api_on_relay(ISSUER).await;
    let api = Arc::clone(&on_relay.api);
    let workspaces = on_relay.hosted.relay.workspaces().unwrap().clone();
    let (_, token) = person(&on_relay, "Ada").await;
    let backend = Key {
        api: &api,
        token: token.clone(),
    };
    let source = template(&backend, "Onboarding template").await;
    let base = format!("/v1/workspaces/{source}");
    holds_note(&workspaces, &source, "Checklist").await;

    // Over MCP, under an id the caller chose: asked twice, one branch.
    let mut account = Mcp {
        api: &api,
        path: "/mcp".into(),
        token,
        session: None,
    };
    assert_eq!(account.initialize().await.0, 200);
    let id = "0c6b1f0e-3d1a-4c8e-9a51-6f1f4b7d2e10";
    let ask = json!({"workspace": source, "name": "Onboarding: Globex", "id": id});
    let first = account.did("account/workspaces#branch", ask.clone()).await;
    let second = account.did("account/workspaces#branch", ask).await;
    assert_eq!(first["workspace"]["id"], id, "{first}");
    assert_eq!(second["workspace"]["id"], id, "{second}");
    assert_eq!(named(&backend, "Onboarding: Globex").await, [id.to_owned()]);
    holds_note(&workspaces, id, "Checklist").await;

    // A key for one workspace is refused as it is refused making one.
    let (status, minted) = backend
        .post("/v1/keys", &json!({"workspace": source}), &[])
        .await;
    assert_eq!(status, 201, "{minted}");
    let narrow = Key {
        api: &api,
        token: minted["token"].as_str().unwrap().to_owned(),
    };
    let (status, refused) = narrow
        .post(
            &format!("{base}/branches"),
            &json!({"name": "Onboarding: Initech"}),
            &[],
        )
        .await;
    let (made_status, made_refused) = narrow
        .post(
            "/v1/workspaces",
            &json!({"name": "Onboarding: Initech"}),
            &[],
        )
        .await;
    assert_eq!(status, 403, "{refused}");
    assert_eq!(
        (status, &refused["error"]["code"]),
        (made_status, &made_refused["error"]["code"]),
        "{refused} {made_refused}"
    );

    // Someone else, invited into it, is refused as for any workspace not
    // theirs: it is no workspace of theirs.
    let invited = backend
        .did(
            &format!("{base}/members/invitations"),
            &json!({"role": "editor"}),
        )
        .await;
    let (rosa, rosa_token) = person(&on_relay, "Rosa").await;
    accept_invitation(&on_relay, invited["link"].as_str().unwrap(), &rosa, "Rosa").await;
    let reviewer = Key {
        api: &api,
        token: rosa_token,
    };
    let (status, refused) = reviewer
        .post(
            &format!("{base}/branches"),
            &json!({"name": "Rosa's copy"}),
            &[],
        )
        .await;
    let (deleting, not_hers) =
        json_of(reviewer.send(hyper::Method::DELETE, &base, b"", &[]).await).await;
    assert_eq!(
        (status, &refused["error"]["code"]),
        (404, &json!("not_found")),
        "{refused}"
    );
    assert_eq!(
        (status, &refused["error"]["code"]),
        (deleting, &not_hers["error"]["code"]),
        "{not_hers}"
    );
    assert!(named(&reviewer, "Rosa's copy").await.is_empty());

    // A branch that could not take everything is deleted again: the call
    // names what did not come, and nobody lists it.
    let mut made = workspaces.branches();
    workspaces.branch_content(Arc::new(Refusing));
    let (status, failed) = backend
        .post(
            &format!("{base}/branches"),
            &json!({"name": "Onboarding: Umbrella"}),
            &[],
        )
        .await;
    // The caller tells this refusal from every other by its code, and reads
    // which part of the template did not come; why stays in the host's log.
    assert_eq!(
        (status, &failed["error"]["code"]),
        (422, &json!("branch_incomplete")),
        "{failed}"
    );
    let said = failed["error"]["message"].as_str().unwrap_or_default();
    assert!(
        said.contains("wasn't made") && said.contains("the files its records name"),
        "{failed}"
    );
    assert!(
        failed["error"]["next_steps"]
            .as_array()
            .is_some_and(|steps| !steps.is_empty()),
        "{failed}"
    );
    assert!(named(&backend, "Onboarding: Umbrella").await.is_empty());
    assert!(
        !workspaces
            .list()
            .await
            .iter()
            .any(|summary| summary.name == "Onboarding: Umbrella"),
        "the relay governs nothing of it"
    );
    assert!(made.try_recv().is_err(), "no branch was made");
    // The template is as it was.
    assert!(titles(&workspaces, &source).await.contains("Checklist"));
}
