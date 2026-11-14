use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde_json::{Value, json};
use test_http::{Reply, Request, Responder};
use zeroize::Zeroizing;

use crate::migration_store::MigrationStore;
use migration_sources::service::*;

const OUTLINE_KEY: &str = "ol_api_secretkey";
const LINEAR_KEY: &str = "lin_api_secretkey";
const PASSWORD: &str = "mail-password-typed-once";

/// A one-pixel PNG, as an Outline attachment answers.
const PNG: &[u8] = &[
    0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0, 0, 13, b'I', b'H', b'D', b'R', 0, 0,
    0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89,
];

fn json_reply(value: &Value) -> Option<Reply> {
    Some(
        Reply::status(200)
            .header("content-type", "application/json")
            .body(value.to_string()),
    )
}

fn body(request: &Request) -> Value {
    serde_json::from_str(&request.body_text()).unwrap_or(Value::Null)
}

/// Outline as its API answers: `POST /api/<method>`, bearer key, `{ "data": … }` paged by
/// `limit` and `offset`, attachments behind `/api/attachments.redirect`.
fn outline(documents: Arc<std::sync::Mutex<Vec<Value>>>) -> Responder {
    Responder::handle(move |request| {
        if request.target.starts_with("/api/attachments.redirect") {
            assert_eq!(
                request.header("authorization"),
                Some(format!("Bearer {OUTLINE_KEY}").as_str())
            );
            return Some(
                Reply::status(200)
                    .header("content-type", "image/png")
                    .body(PNG),
            );
        }
        if request.header("authorization") != Some(format!("Bearer {OUTLINE_KEY}").as_str()) {
            return Some(Reply::status(401));
        }
        let asked = body(request);
        let offset = asked["offset"].as_u64().unwrap_or(0);
        match request.target.as_str() {
            "/api/auth.info" => json_reply(&json!({
                "data": {"user": {"name": "Ada"}, "team": {"name": "Acme"}}
            })),
            "/api/collections.list" => json_reply(&json!({
                "data": if offset == 0 { json!([
                    {"id": "col-handbook", "name": "Handbook", "description": "How we work", "url": "/collection/handbook-c1"},
                    {"id": "col-archive", "name": "Archive", "description": "", "url": "/collection/archive-c2"}
                ]) } else { json!([]) },
                "pagination": {"limit": 100, "offset": offset}
            })),
            "/api/documents.list" => {
                let collection = asked["collectionId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                let data: Vec<Value> = if offset == 0 {
                    documents
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|document| document["collectionId"] == collection.as_str())
                        .cloned()
                        .collect()
                } else {
                    Vec::new()
                };
                json_reply(&json!({"data": data, "pagination": {"limit": 100, "offset": offset}}))
            }
            _ => Some(Reply::status(404)),
        }
    })
}

fn outline_documents(server: &str) -> Vec<Value> {
    vec![
        json!({
            "id": "doc-child", "urlId": "Ch1ld", "title": "Onboarding checklist",
            "text": "Read [the welcome](/doc/welcome-W3lc0me) first.\n\n![Team photo](/api/attachments.redirect?id=pic-1 \"=320x200\")\n",
            "collectionId": "col-handbook", "parentDocumentId": "doc-welcome",
            "url": "/doc/onboarding-checklist-Ch1ld"
        }),
        json!({
            "id": "doc-welcome", "urlId": "W3lc0me", "title": "Welcome",
            "text": format!("# Welcome\n\nSee [the checklist]({server}/doc/onboarding-checklist-Ch1ld) and [our site](https://acme.test).\n"),
            "collectionId": "col-handbook", "parentDocumentId": null,
            "url": "/doc/welcome-W3lc0me"
        }),
        json!({
            "id": "doc-old", "urlId": "0ld", "title": "Old plans", "text": "Gone by now.",
            "collectionId": "col-archive", "parentDocumentId": null, "url": "/doc/old-plans-0ld"
        }),
    ]
}

struct Fixture {
    _temporary: tempfile::TempDir,
    home: std::path::PathBuf,
    config: std::path::PathBuf,
    store: MigrationStore,
}

fn fixture() -> Fixture {
    let temporary = tempfile::tempdir().unwrap();
    let home = temporary.path().join("home");
    std::fs::create_dir(&home).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let config = home.join(".config/tilo");
    let store = MigrationStore::open_at(&temporary.path().join("migrations")).unwrap();
    Fixture {
        _temporary: temporary,
        home,
        config,
        store,
    }
}

fn read_inbox(directory: &Path) -> Vec<Value> {
    let mut entries: Vec<Value> = std::fs::read_dir(directory)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
                .map(|entry| serde_json::from_slice(&std::fs::read(entry.path()).unwrap()).unwrap())
                .collect()
        })
        .unwrap_or_default();
    entries.sort_by(|a, b| a["key"].as_str().cmp(&b["key"].as_str()));
    entries
}

fn run_saved(
    fixture: &Fixture,
    journal: &mut ServiceJournal,
    cancel: &AtomicBool,
) -> Result<ServiceReport, ServiceError> {
    let mut landing = InboxLanding::new(&fixture.home);
    run(
        &fixture.config,
        journal,
        &mut landing,
        &mut |journal| fixture.store.save_service(journal),
        cancel,
    )
}

fn assert_no_secret(directory: &Path, secret: &str) {
    for entry in walk(directory) {
        let bytes = std::fs::read(&entry).unwrap();
        let name = entry.to_string_lossy();
        if name.contains("vault") {
            continue;
        }
        assert!(
            !String::from_utf8_lossy(&bytes).contains(secret),
            "{name} holds the secret"
        );
    }
}

fn walk(directory: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}

#[test]
fn outline_review_choose_and_land_pages_with_hierarchy_pictures_and_links() {
    let documents = Arc::new(std::sync::Mutex::new(Vec::new()));
    let server = outline(Arc::clone(&documents));
    *documents.lock().unwrap() = outline_documents(&server.url());
    let fixture = fixture();

    let refused = connect(
        &fixture.config,
        ServiceAddress {
            kind: ServiceKind::Outline,
            server: server.url(),
            login: String::new(),
            secret: Zeroizing::new("wrong".into()),
        },
    );
    assert_eq!(refused, Err(ServiceError::Refused));

    let account = connect(
        &fixture.config,
        ServiceAddress {
            kind: ServiceKind::Outline,
            server: format!("{}/", server.url()),
            login: String::new(),
            secret: Zeroizing::new(OUTLINE_KEY.into()),
        },
    )
    .unwrap();
    assert_eq!(
        connected(&fixture.config),
        vec![(
            account.clone(),
            ServiceKind::Outline,
            "Outline: Acme".to_owned()
        )]
    );

    let mut reviewed = review(&fixture.config, account.as_str()).unwrap();
    assert_eq!(reviewed.groups.len(), 2);
    let handbook = &reviewed.groups[0];
    assert_eq!(handbook.title, "Handbook");
    let titles: Vec<&str> = handbook
        .items
        .iter()
        .map(|item| item.title.as_str())
        .collect();
    assert_eq!(titles, ["Handbook", "Welcome", "Onboarding checklist"]);
    assert_eq!(handbook.items[2].facts, ["Handbook", "Welcome"]);
    reviewed.choose("col-archive", false).unwrap();
    assert_eq!(reviewed.chosen_count(), 3);

    let mut journal = ServiceJournal::new(reviewed);
    fixture.store.save_service(&journal).unwrap();
    let report = run_saved(&fixture, &mut journal, &AtomicBool::new(false)).unwrap();
    assert_eq!(report.landed, 3);
    assert_eq!(journal.state, JournalState::Complete);

    let inbox = InboxLanding::new(&fixture.home).inbox("so.tilo.notes", "pages");
    let pages = read_inbox(&inbox);
    assert_eq!(pages.len(), 3, "the archive was left out");
    let by_key = |key: &str| {
        pages
            .iter()
            .find(|page| page["key"] == key)
            .unwrap()
            .clone()
    };
    let collection = by_key("collection:col-handbook");
    assert_eq!(collection["title"], "Handbook");
    assert_eq!(collection["parent"], Value::Null);
    let welcome = by_key("doc-welcome");
    assert_eq!(welcome["parent"], "collection:col-handbook");
    assert!(
        welcome["markdown"]
            .as_str()
            .unwrap()
            .contains("See [[Onboarding checklist]] and [our site](https://acme.test)."),
        "{welcome}"
    );
    let child = by_key("doc-child");
    assert_eq!(child["parent"], "doc-welcome");
    let digest = blake3::hash(PNG).to_hex().to_string();
    let markdown = child["markdown"].as_str().unwrap();
    assert!(markdown.contains("Read [[Welcome]] first."), "{markdown}");
    assert!(
        markdown.contains(&format!("![Team photo](asset:{digest})")),
        "{markdown}"
    );
    use base64::Engine as _;
    assert_eq!(
        child["pictures"][0].as_str().unwrap(),
        base64::engine::general_purpose::STANDARD.encode(PNG)
    );
    assert_eq!(
        child["url"],
        format!("{}/doc/onboarding-checklist-Ch1ld", server.url())
    );

    // A second run follows the journal: nothing lands twice.
    let again = run_saved(&fixture, &mut journal, &AtomicBool::new(false)).unwrap();
    assert_eq!((again.landed, again.already_there), (0, 3));

    // A document added since the review waits for a new review.
    documents.lock().unwrap().push(json!({
        "id": "doc-new", "urlId": "N3w", "title": "New page", "text": "Fresh.",
        "collectionId": "col-handbook", "parentDocumentId": null, "url": "/doc/new-page-N3w"
    }));
    let later = run_saved(&fixture, &mut journal, &AtomicBool::new(false)).unwrap();
    assert_eq!((later.landed, later.not_reviewed), (0, 1));
    let mut fresh = review(&fixture.config, account.as_str()).unwrap();
    fresh.choose("col-archive", false).unwrap();
    journal.reviewed_again(fresh);
    let reviewed = run_saved(&fixture, &mut journal, &AtomicBool::new(false)).unwrap();
    assert_eq!((reviewed.landed, reviewed.already_there), (1, 3));

    let saved = fixture.store.load_services().unwrap();
    assert_eq!(saved, vec![journal]);
    assert_no_secret(&fixture.home, OUTLINE_KEY);
    assert_no_secret(fixture.store.root(), OUTLINE_KEY);
}

/// Linear as its GraphQL API answers: `POST /graphql`, the key raw in `Authorization`,
/// issues paged by cursor.
fn linear() -> Responder {
    Responder::handle(|request| {
        assert_eq!(request.target, "/graphql");
        if request.header("authorization") != Some(LINEAR_KEY) {
            return json_reply(&json!({
                "errors": [{"message": "Authentication required", "extensions": {"code": "AUTHENTICATION_ERROR"}}]
            }));
        }
        let asked = body(request);
        let query = asked["query"].as_str().unwrap_or_default();
        if query.contains("organization") {
            return json_reply(
                &json!({"data": {"viewer": {"name": "Ada"}, "organization": {"name": "Acme"}}}),
            );
        }
        let after = asked["variables"]["after"].as_str();
        let issue = |id: &str,
                     identifier: &str,
                     title: &str,
                     team: (&str, &str),
                     state: &str,
                     extra: Value| {
            let mut issue = json!({
                "id": id, "identifier": identifier, "title": title,
                "description": format!("About {title}."),
                "url": format!("https://tracker.example/acme/issue/{identifier}"),
                "createdAt": "2026-01-02T03:04:05.000Z",
                "team": {"id": team.0, "key": team.1, "name": team.1},
                "state": {"name": state, "type": state},
                "assignee": null, "labels": {"nodes": []}, "parent": null,
                "relations": {"nodes": []}, "comments": {"nodes": []}
            });
            if let (Some(issue), Value::Object(extra)) = (issue.as_object_mut(), extra) {
                issue.extend(extra);
            }
            issue
        };
        let page = match after {
            None => json!({
                "pageInfo": {"hasNextPage": true, "endCursor": "c1"},
                "nodes": [
                    issue("i-1", "ENG-1", "Ship the migration", ("t-eng", "ENG"), "started", json!({
                        "assignee": {"name": "Ada Lovelace"},
                        "labels": {"nodes": [{"name": "Migration"}, {"name": "P1"}]},
                        "relations": {"nodes": [{"relatedIssue": {"id": "i-3"}}]},
                        "comments": {"nodes": [{"body": "On it.", "createdAt": "2026-01-03T00:00:00.000Z", "user": {"name": "Grace"}}]}
                    })),
                    issue("i-2", "ENG-2", "Write the review page", ("t-eng", "ENG"), "unstarted", json!({"parent": {"id": "i-1"}})),
                ]
            }),
            Some(_) => json!({
                "pageInfo": {"hasNextPage": false, "endCursor": null},
                "nodes": [
                    issue("i-3", "ENG-3", "Old importer gone", ("t-eng", "ENG"), "completed", json!({})),
                    issue("i-4", "OPS-1", "Renew certificates", ("t-ops", "OPS"), "canceled", json!({})),
                ]
            }),
        };
        json_reply(&json!({"data": {"issues": page}}))
    })
}

#[test]
fn linear_issues_land_with_state_labels_assignee_comments_links_and_source() {
    let server = linear();
    let fixture = fixture();
    let account = connect(
        &fixture.config,
        ServiceAddress {
            kind: ServiceKind::Linear,
            server: server.url(),
            login: String::new(),
            secret: Zeroizing::new(LINEAR_KEY.into()),
        },
    )
    .unwrap();
    let mut reviewed = review(&fixture.config, account.as_str()).unwrap();
    let teams: Vec<(&str, usize)> = reviewed
        .groups
        .iter()
        .map(|group| (group.title.as_str(), group.items.len()))
        .collect();
    assert_eq!(teams, [("ENG", 3), ("OPS", 1)]);
    assert_eq!(reviewed.groups[0].items[0].facts, ["ENG-1", "started"]);
    reviewed.choose("t-ops", false).unwrap();

    let mut journal = ServiceJournal::new(reviewed);
    // Stop after the first issue lands: the journal keeps it, and the next run resumes.
    let cancel = AtomicBool::new(false);
    let saves = AtomicUsize::new(0);
    let mut landing = InboxLanding::new(&fixture.home);
    let stopped = run(
        &fixture.config,
        &mut journal,
        &mut landing,
        &mut |journal| {
            if saves.fetch_add(1, Ordering::Relaxed) == 1 {
                cancel.store(true, Ordering::Relaxed);
            }
            fixture.store.save_service(journal)
        },
        &cancel,
    );
    assert_eq!(stopped, Err(ServiceError::Cancelled));
    assert_eq!(journal.state, JournalState::Interrupted);
    assert_eq!(journal.landed.len(), 1);
    let resumed = run_saved(&fixture, &mut journal, &AtomicBool::new(false)).unwrap();
    assert_eq!((resumed.landed, resumed.already_there), (2, 1));

    let inbox = InboxLanding::new(&fixture.home).inbox("so.tilo.tasks", "issues");
    let issues = read_inbox(&inbox);
    assert_eq!(issues.len(), 3, "OPS was left out");
    let first = &issues[0];
    assert_eq!(first["identifier"], "ENG-1");
    assert_eq!(first["status"], "in-progress");
    assert_eq!(first["assignee"], "person:Ada Lovelace");
    assert_eq!(first["labels"], json!(["Migration", "P1"]));
    assert_eq!(
        first["links"],
        json!(["https://tracker.example/acme/issue/ENG-3"])
    );
    assert_eq!(first["comments"][0]["author"], "Grace");
    assert_eq!(first["url"], "https://tracker.example/acme/issue/ENG-1");
    assert_eq!(
        issues[1]["parent"],
        "https://tracker.example/acme/issue/ENG-1"
    );
    assert_eq!(issues[1]["status"], "todo");
    assert_eq!(issues[2]["status"], "done");
    assert_no_secret(&fixture.home, LINEAR_KEY);
    assert_no_secret(fixture.store.root(), LINEAR_KEY);
}

#[test]
fn a_mail_account_waits_for_review_then_reaches_mail_with_its_vaulted_password() {
    let fixture = fixture();
    let account = connect(
        &fixture.config,
        ServiceAddress {
            kind: ServiceKind::Mail,
            server: String::new(),
            login: "Ops@Acme.test".into(),
            secret: Zeroizing::new(PASSWORD.into()),
        },
    )
    .unwrap();
    assert_eq!(account.as_str(), "ops@acme.test");
    let pending = |config: &Path| {
        let store = accounts::AccountStore::load_in(config);
        let stored = store.get(&account).unwrap().clone();
        assert_eq!(stored.type_id.as_str(), accounts::email::TYPE_ID);
        accounts::email::AccountConfig::from_value(stored.config).unwrap()
    };
    let config = pending(&fixture.config);
    assert!(
        config.import_pending,
        "nothing reaches Mail before the review"
    );
    assert_eq!(config.imap_host, "imap.acme.test");
    assert_eq!(
        accounts::SecretVault::open_read_only_in(&fixture.config)
            .get(&account)
            .unwrap()
            .password,
        PASSWORD
    );

    let reviewed = review(&fixture.config, account.as_str()).unwrap();
    assert_eq!(reviewed.item_count(), 1);
    let mut journal = ServiceJournal::new(reviewed);
    let report = run_saved(&fixture, &mut journal, &AtomicBool::new(false)).unwrap();
    assert_eq!(report.landed, 1);
    assert!(!pending(&fixture.config).import_pending);
    let again = run_saved(&fixture, &mut journal, &AtomicBool::new(false)).unwrap();
    assert_eq!((again.landed, again.already_there), (0, 1));
    assert_no_secret(&fixture.home, PASSWORD);
    assert_no_secret(fixture.store.root(), PASSWORD);

    // An account the person set up another way is never taken over.
    let mut store = accounts::AccountStore::load_in(&fixture.config);
    let mut other = accounts::Account::new(accounts::AccountId::new("me@acme.test"), "email");
    other.config = json!({"email": "me@acme.test"});
    store.upsert(other);
    store.save_in(&fixture.config).unwrap();
    assert_eq!(
        connect(
            &fixture.config,
            ServiceAddress {
                kind: ServiceKind::Mail,
                server: String::new(),
                login: "me@acme.test".into(),
                secret: Zeroizing::new(PASSWORD.into()),
            },
        ),
        Err(ServiceError::AlreadyThere)
    );
}
