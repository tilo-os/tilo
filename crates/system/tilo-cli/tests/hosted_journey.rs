//! A person or their agent working in a hosted workspace through the `tilo` command itself: the
//! key they were given names the workspace, files go up and come back, an action runs, and the
//! people and the record are read. Every command is the binary a person types, against a
//! scripted `/v1` server, and each request it makes is checked.

use std::process::Command;

use serde_json::{Value, json};
use tilo_test_env::SpawnUnderEnvLock as _;

const WORKSPACE: &str = "6f1c2b9e-1c1f-4f0e-9a51-3c4d5e6f7a8b";
const BRANCH: &str = "0b5e7c1d-2e3f-4a5b-8c6d-7e8f9a0b1c2d";

/// The scripted `/v1` server: the hosted commands' routes, answering as the API does. It keeps
/// how many files a folder's list named, so the open that puts it in the tree answers the count
/// Files answers.
fn answer() -> impl Fn(&test_http::Request) -> Option<test_http::Reply> + Send + Sync + 'static {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    let listed = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&listed);
    move |request: &test_http::Request| answer_with(request, &counted)
}

fn answer_with(
    request: &test_http::Request,
    listed: &std::sync::atomic::AtomicUsize,
) -> Option<test_http::Reply> {
    use std::sync::atomic::Ordering;
    let json_reply = |status: u16, body: Value| {
        Some(
            test_http::Reply::status(status)
                .header("content-type", "application/json")
                .body(body.to_string()),
        )
    };
    let route = request.target.as_str();
    match (request.method.as_str(), route) {
        ("POST", "/v1/resources/account/workspaces/actions/list") => json_reply(
            200,
            json!({"status": "ok", "result": {"workspaces": [{"id": WORKSPACE,
                "name": "Applicant: Jane Doe", "host": "eu1.tilo.sh", "role": "owner"}]}}),
        ),
        ("POST", "/v1/resources/account/workspaces/actions/create") => json_reply(
            200,
            json!({"status": "ok", "result": {"workspace": {"id": WORKSPACE,
                "name": "Applicant: Jane Doe", "host": "eu1.tilo.sh", "role": "owner"}}}),
        ),
        ("POST", "/v1/resources/account/workspaces/actions/branch") => json_reply(
            200,
            json!({"status": "ok", "result": {"workspace": {"id": BRANCH,
                "name": "Applicant: Jane Doe, second try", "host": "eu1.tilo.sh",
                "role": "owner", "labels": {"customer_id": "cus_8121"}}}}),
        ),
        ("POST", route) if route.ends_with("/actions/unattended") => json_reply(
            200,
            json!({"status": "ok", "path": "workspace/unattended#set",
                "result": {"workspace": WORKSPACE, "unattended": true}}),
        ),
        ("POST", route) if route.ends_with("/contents/have") => {
            let asked: Value = serde_json::from_slice(&request.body).unwrap_or_default();
            let count = asked
                .get("hashes")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            json_reply(200, json!({"have": vec![false; count]}))
        }
        ("PUT", route) if route.ends_with("/contents") => {
            let asked: Value = serde_json::from_slice(&request.body).unwrap_or_default();
            let count = asked
                .get("files")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            json_reply(200, json!({"contents": vec!["tilo.content:abc"; count]}))
        }
        ("POST", route) if route.ends_with("/uploads") => {
            if let Ok(files) = serde_json::from_slice::<Value>(&request.body)
                && let Some(files) = files.as_array()
            {
                listed.store(files.len(), Ordering::SeqCst);
            }
            json_reply(
                201,
                json!({"content": "tilo.content:abc", "size": request.body.len()}),
            )
        }
        ("POST", route) if route.ends_with("/actions/open") => {
            let asked: Value = serde_json::from_slice(&request.body).unwrap_or_default();
            let count = asked
                .get("files")
                .and_then(Value::as_array)
                .map_or_else(|| listed.load(Ordering::SeqCst), Vec::len);
            json_reply(
                200,
                json!({"status": "ok", "path": "files#open", "invocation": "inv-1",
                    "result": {"count": count}}),
            )
        }
        ("GET", route) if route.contains("/content/") => {
            Some(test_http::Reply::status(200).body(b"hello".to_vec()))
        }
        ("POST", route) if route.ends_with("/resources/invocations/actions/list") => json_reply(
            200,
            json!({"status": "ok", "path": "invocations#list", "result": {
                "items": [{"id": "run-2", "status": "waiting", "changes": 1,
                    "by": {"name": "Jane Doe's agent", "kind": "agent"}}],
                "total": 2, "waiting": 1, "next_cursor": "17:s1"}}),
        ),
        ("GET", route) if route.contains("/resources?path=") => json_reply(
            200,
            json!({"path": "notes", "kind": "notes.notebook", "rows": []}),
        ),
        ("GET", route) if route.contains("/resources/") => json_reply(
            200,
            json!({"path": "notes/Notebook/Todo", "fields": {"markdown": "# Todo"}}),
        ),
        ("POST", route) if route.contains("/actions/") => json_reply(
            200,
            json!({"status": "ok", "path": "acted#one", "invocation": "inv-2", "result": {}}),
        ),
        _ => json_reply(
            404,
            json!({"error": {"code": "not_found", "message": "no such route"}}),
        ),
    }
}

/// The `tilo` a person runs, with the key they were given and the server it reaches.
struct Person {
    home: tempfile::TempDir,
    api: test_http::Responder,
}

impl Person {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
            api: test_http::Responder::handle(answer()),
        }
    }

    /// Run `tilo <args>` as they would, and fail unless it succeeded: its answer, and the
    /// requests that answer asked for.
    fn run(&self, args: &[&str]) -> (String, Vec<test_http::Request>) {
        self.api.drain_requests();
        let out = Command::new(tilo_test_env::package_binary!("tilo"))
            .args(args)
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join("config"))
            .env("XDG_STATE_HOME", self.home.path().join("state"))
            .env("XDG_RUNTIME_DIR", self.home.path().join("run"))
            .env("TILO_API_KEY", "tilo_sk_test")
            .env("TILO_API_URL", self.api.url())
            .env_remove("TILO_IDENTITY_ROOT")
            .env_remove("TILO_AGENT_SOCKET")
            .output_locked()
            .unwrap();
        let stdout = String::from_utf8(out.stdout).expect("an answer");
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success(), "tilo {args:?}: {stderr}");
        (stdout, self.api.drain_requests())
    }

    /// The answer as the typed JSON `--json` asks for.
    fn typed(&self, args: &[&str]) -> (Value, Vec<test_http::Request>) {
        let mut with_json = vec!["--json"];
        with_json.extend_from_slice(args);
        let (stdout, sent) = self.run(&with_json);
        (
            serde_json::from_str(&stdout).unwrap_or_else(|why| panic!("{why}: {stdout}")),
            sent,
        )
    }
}

/// The one request among `sent` whose target ends with `ending`.
fn sent_ending<'a>(sent: &'a [test_http::Request], ending: &str) -> &'a test_http::Request {
    sent.iter()
        .find(|request| request.target.ends_with(ending))
        .unwrap_or_else(|| panic!("no request ending {ending}: {:?}", targets(sent)))
}

fn targets(sent: &[test_http::Request]) -> Vec<&str> {
    sent.iter().map(|request| request.target.as_str()).collect()
}

tilo_test_env::scenario! {
    fn a_person_works_in_a_workspace_on_a_server_through_the_command;

    phase a_workspace_is_made_branched_listed_and_named;
    {
        let person = Person::new();

        // A workspace is made as the account's `workspaces#create`, once per idempotency key, its
        // labels as create takes them, and the answer names its id.
        let (made, sent) = person.typed(&[
            "workspace",
            "create",
            "Applicant: Jane Doe",
            "--label",
            "customer_id=cus_8121",
        ]);
        assert_eq!(made["workspace"]["id"], WORKSPACE, "{made}");
        assert_eq!(sent.len(), 1, "{:?}", targets(&sent));
        assert_eq!(sent[0].method, "POST");
        assert_eq!(
            sent[0].target,
            "/v1/resources/account/workspaces/actions/create"
        );
        assert_eq!(sent[0].header("authorization"), Some("Bearer tilo_sk_test"));
        assert!(
            sent[0].header("idempotency-key").is_some(),
            "a call that makes something names its invocation"
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&sent[0].body).unwrap(),
            json!({"name": "Applicant: Jane Doe", "labels": {"customer_id": "cus_8121"}})
        );

        // A branch is the account's `workspaces#branch`, its source named by a read of the list.
        let (branched, sent) = person.typed(&[
            "workspace",
            "branch",
            "Applicant: Jane Doe",
            "Applicant: Jane Doe, second try",
            "--label",
            "customer_id=cus_8121",
        ]);
        assert_eq!(branched["workspace"]["id"], BRANCH, "{branched}");
        assert_eq!(sent.len(), 2, "the source is named by the list first");
        let branch = sent.last().expect("the branch");
        assert_eq!(
            branch.target,
            "/v1/resources/account/workspaces/actions/branch"
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&branch.body).unwrap(),
            json!({"workspace": WORKSPACE, "name": "Applicant: Jane Doe, second try",
                "labels": {"customer_id": "cus_8121"}})
        );

        // Running commands unattended is the account's `workspaces#unattended`, run in the workspace
        // and held while the person decides.
        let (_, sent) = person.typed(&["workspace", "unattended", "Applicant: Jane Doe"]);
        let asked = sent.last().expect("the setting");
        assert_eq!(
            asked.target,
            format!("/v1/workspaces/{WORKSPACE}/resources/account/workspaces/actions/unattended")
        );
        assert_eq!(asked.header("prefer"), Some("wait=60"));
        assert_eq!(
            serde_json::from_slice::<Value>(&asked.body).unwrap(),
            json!({"workspace": WORKSPACE, "on": true})
        );

        // Listing by a label names the labels as `workspaces#list` takes them.
        let (_, listed) = person.typed(&["workspace", "ls", "--label", "customer_id=cus_8121"]);
        assert_eq!(
            listed.last().expect("the list").target,
            "/v1/resources/account/workspaces/actions/list"
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&listed.last().expect("the list").body).unwrap(),
            json!({"labels": {"customer_id": "cus_8121"}})
        );
    }

    phase a_persons_files_go_up_whole_and_come_back;
    {
        let person = Person::new();
        // A file goes in as one upload, asked about by its address first, and the reference it
        // answers is what the workspace puts in its tree.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("passport.png");
        std::fs::write(&file, b"hello").unwrap();
        let (_, put) = person.typed(&[
            "-w",
            "Applicant: Jane Doe",
            "files",
            "put",
            file.to_str().unwrap(),
        ]);
        let asked = sent_ending(&put, "/contents/have");
        let asked: Value = serde_json::from_slice(&asked.body).unwrap();
        let hashes = asked["hashes"].as_array().expect("an address each");
        assert_eq!(hashes.len(), 1);
        assert!(hashes[0].as_str().is_some_and(|hash| hash.len() == 64));
        let upload = sent_ending(&put, "/contents");
        assert_eq!(
            upload.target,
            format!("/v1/workspaces/{WORKSPACE}/contents")
        );
        assert_eq!(upload.method, "PUT");
        assert_eq!(
            serde_json::from_slice::<Value>(&upload.body).unwrap(),
            json!({"files": ["aGVsbG8="]}),
            "the file goes up whole in one call, base64 as the route takes it"
        );
        assert!(
            put.iter()
                .all(|request| !request.target.ends_with("/uploads")),
            "the file does not go up one request to a call"
        );
        let placed = sent_ending(&put, "/actions/open");
        assert_eq!(
            placed.target,
            format!("/v1/workspaces/{WORKSPACE}/resources/files/actions/open")
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&placed.body).unwrap(),
            json!({"files": [{"content": "tilo.content:abc", "name": "passport.png", "dir": ""}]})
        );

        // A folder goes up whole, each file named by its path under the folder it came from, in one
        // change of the workspace's tree.
        let folder = dir.path().join("contracts");
        std::fs::create_dir_all(folder.join("2026")).unwrap();
        for name in ["nda.pdf", "2026/plan.md", "2026/signed.pdf"] {
            std::fs::write(folder.join(name), b"hello").unwrap();
        }
        let (_, folder_put) = person.typed(&[
            "-w",
            "Applicant: Jane Doe",
            "files",
            "put",
            folder.to_str().unwrap(),
            "--to",
            "docs",
        ]);
        let uploads: Vec<&test_http::Request> = folder_put
            .iter()
            .filter(|request| request.target.ends_with("/uploads"))
            .collect();
        assert_eq!(uploads.len(), 1, "the list goes up as content, once");
        let contents: Vec<&test_http::Request> = folder_put
            .iter()
            .filter(|request| request.target.ends_with("/contents"))
            .collect();
        assert_eq!(contents.len(), 1, "the folder's files go up in one call");
        let contents: Value = serde_json::from_slice(&contents[0].body).unwrap();
        assert_eq!(contents["files"].as_array().map(Vec::len), Some(3));
        let listed = uploads
            .iter()
            .find(|request| request.body.starts_with(b"["))
            .expect("the list of files goes up as content");
        assert_eq!(
            serde_json::from_slice::<Value>(&listed.body).unwrap(),
            json!([
                {"content": "tilo.content:abc", "name": "plan.md", "dir": "docs/contracts/2026"},
                {"content": "tilo.content:abc", "name": "signed.pdf", "dir": "docs/contracts/2026"},
                {"content": "tilo.content:abc", "name": "nda.pdf", "dir": "docs/contracts"},
            ]),
            "each file keeps its place under the folder it came from"
        );
        let opened: Vec<&test_http::Request> = folder_put
            .iter()
            .filter(|request| request.target.ends_with("/actions/open"))
            .collect();
        assert_eq!(opened.len(), 1, "the folder is one change of the tree");
        assert_eq!(
            serde_json::from_slice::<Value>(&opened[0].body).unwrap(),
            json!({"files": "tilo.content:abc"}),
            "the open names the list, however many files it holds"
        );
    }

    phase a_person_acts_reads_and_sees_the_people_and_the_record;
    {
        let person = Person::new();
        // An action is `POST …/resources/{path}/actions/{action}`: its arguments are the body, its
        // run is named, and it is held while the person decides.
        let (_, sent) = person.typed(&["-w", "Applicant: Jane Doe", "do", "pdf-viewer#open"]);
        assert_eq!(
            sent.len(),
            2,
            "the workspace is named, then the action runs"
        );
        let action = sent.last().expect("the action");
        assert_eq!(
            action.target,
            format!("/v1/workspaces/{WORKSPACE}/resources/pdf-viewer/actions/open")
        );
        assert_eq!(action.header("prefer"), Some("wait=60"));
        assert_eq!(
            action.header("tilo-invocation-title"),
            Some("pdf-viewer#open")
        );
        assert!(
            action
                .header("tilo-invocation")
                .is_some_and(|id| id == action.header("idempotency-key").unwrap_or_default()),
            "one invocation, one idempotency key"
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&action.body).unwrap(),
            json!({})
        );

        // A listing and a read are the routes `tilo_ls` and `tilo_read` answer, a page naming the
        // cursor it ended with.
        let (_, listed) = person.typed(&["-w", "Applicant: Jane Doe", "ls", "notes/Todo"]);
        assert_eq!(
            listed.last().expect("the listing").target,
            format!("/v1/workspaces/{WORKSPACE}/resources?path=notes%2FTodo")
        );
        let (_, paged) = person.typed(&[
            "-w",
            "Applicant: Jane Doe",
            "ls",
            "notes/Todo",
            "--cursor",
            "12",
        ]);
        assert_eq!(
            paged.last().expect("the listing").target,
            format!("/v1/workspaces/{WORKSPACE}/resources?path=notes%2FTodo&cursor=12")
        );
        let (_, read) = person.typed(&[
            "-w",
            "Applicant: Jane Doe",
            "read",
            "notes/Notebook/Todo",
            "--cursor",
            "12",
        ]);
        assert_eq!(
            read.last().expect("the read").target,
            format!("/v1/workspaces/{WORKSPACE}/resources/notes/Notebook/Todo?cursor=12")
        );

        // A file comes back by the reference, its bytes as they were.
        let out = person.home.path().join("passport.back");
        let (_, got) = person.typed(&[
            "-w",
            "Applicant: Jane Doe",
            "files",
            "get",
            "tilo.content:abc",
            "--output",
            out.to_str().unwrap(),
        ]);
        assert_eq!(std::fs::read(&out).unwrap(), b"hello");
        assert_eq!(
            got.last().expect("the read").target,
            format!("/v1/workspaces/{WORKSPACE}/content/tilo.content%3Aabc")
        );

        // A fellow administrator is invited by the role the API reads.
        let (_, invited) = person.typed(&[
            "-w",
            "Applicant: Jane Doe",
            "workspace",
            "people",
            "invite",
            "--role",
            "administrator",
            "--email",
            "ops@example.com",
        ]);
        let invitation = invited.last().expect("the invitation");
        assert_eq!(
            invitation.target,
            format!("/v1/workspaces/{WORKSPACE}/resources/workspace/invitations/actions/invite")
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&invitation.body).unwrap(),
            json!({"role": "administrator", "invitee_name": "ops@example.com"})
        );

        // Someone in is moved by their key, and removed by it.
        let ada = "ab".repeat(32);
        let (_, moved) = person.typed(&[
            "-w",
            "Applicant: Jane Doe",
            "workspace",
            "people",
            "role",
            &ada,
            "editor",
        ]);
        let moved = moved.last().expect("the move");
        assert_eq!(
            moved.target,
            format!("/v1/workspaces/{WORKSPACE}/resources/workspace/members/{ada}/actions/role")
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&moved.body).unwrap(),
            json!({"role": "editor"})
        );
        let (_, removed) = person.typed(&[
            "-w",
            "Applicant: Jane Doe",
            "workspace",
            "people",
            "rm",
            &ada,
        ]);
        let removed = removed.last().expect("the removal");
        assert_eq!(
            removed.target,
            format!("/v1/workspaces/{WORKSPACE}/resources/workspace/members/{ada}/actions/remove")
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&removed.body).unwrap(),
            json!({})
        );

        // A workspace is named again with the account's `workspaces#rename`.
        let (_, renamed) = person.typed(&["workspace", "rename", "Applicant: Jane Doe", "Jane Doe"]);
        let renamed = renamed.last().expect("the rename");
        assert_eq!(
            renamed.target,
            "/v1/resources/account/workspaces/actions/rename"
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&renamed.body).unwrap(),
            json!({"workspace": WORKSPACE, "name": "Jane Doe"})
        );

        // A workspace's invocations are listed page by page, and the answer says how to read on.
        let (_, sent) = person.typed(&[
            "-w",
            "Applicant: Jane Doe",
            "invocations",
            "--limit",
            "1",
            "--cursor",
            "17:s0",
        ]);
        let listing = sent.last().expect("the listing");
        assert_eq!(
            listing.target,
            format!("/v1/workspaces/{WORKSPACE}/resources/invocations/actions/list")
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&listing.body).unwrap(),
            json!({"limit": 1, "cursor": "17:s0"})
        );
        let (said, _) = person.run(&["-w", "Applicant: Jane Doe", "invocations"]);
        assert!(
            said.starts_with(concat!(
                "2 invocations, newest first; 1 waiting for the person\n",
                "run-2 waiting by Jane Doe's agent (agent): 1 change"
            )),
            "{said}"
        );
        assert!(
            said.contains("\nolder: tilo invocations --cursor 17:s1"),
            "{said}"
        );
    }
}

/// A server that says it is briefly unavailable answers `503`; the same call is asked again under
/// the id it first went with, so the host runs one invocation however often it is retried.
#[test]
fn a_server_that_did_not_answer_is_asked_again_under_the_same_idempotency_key() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let asked = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&asked);
    let api = test_http::Responder::handle(move |request: &test_http::Request| {
        let reply = |status: u16, body: Value| {
            Some(
                test_http::Reply::status(status)
                    .header("content-type", "application/json")
                    .body(body.to_string()),
            )
        };
        if request.target.ends_with("/list") {
            return reply(
                200,
                json!({"status": "ok", "result": {"keys": [{"grant": "grant-1", "name": "Nightly"}]}}),
            );
        }
        let first = counted.fetch_add(1, Ordering::SeqCst) == 0;
        if first {
            reply(
                503,
                json!({"error": {"code": "unavailable", "message": "still running"}}),
            )
        } else {
            reply(
                200,
                json!({"status": "ok", "result": {"revoked": "grant-1"}}),
            )
        }
    });
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(tilo_test_env::package_binary!("tilo"))
        .args(["--json", "keys", "revoke", "grant-1"])
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_STATE_HOME", home.path().join("state"))
        .env("XDG_RUNTIME_DIR", home.path().join("run"))
        .env("TILO_API_KEY", "tilo_sk_test")
        .env("TILO_API_URL", api.url())
        .env_remove("TILO_IDENTITY_ROOT")
        .output_locked()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let sent: Vec<_> = api
        .drain_requests()
        .into_iter()
        .filter(|request| request.target.ends_with("/revoke"))
        .collect();
    assert_eq!(
        sent.len(),
        2,
        "the unavailable answer is retried, not reported"
    );
    assert_eq!(sent[1].target, "/v1/resources/account/keys/actions/revoke");
    assert_eq!(
        sent[0].header("idempotency-key"),
        sent[1].header("idempotency-key")
    );
}
