//! A person's calendars on their host, against a calendar server the test
//! starts: an account is added only once its server took the sign-in, its
//! entries are kept and read with every device off, a category is the
//! server's own field, an invitation is one entry, and only the person and
//! the agent they named reach any of it.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the host a test binds is kept until the test ends; dropping it earlier closes what it drives"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard, and one account's life is told in order"
)]

use std::path::{Path, PathBuf};

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionTarget, person_accounts, person_vault,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_relay_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use super::super::HostWorkspaceActions;
use super::super::tests::{caller, settled};
use super::accounts_tests::key_of;
use crate::workspaces::Workspaces;
use crate::workspaces::fixtures::TestHost;
use crate::workspaces::person_settings::calendars::{self as held, fake_caldav};
use crate::workspaces::tests::{governor, test_host};

/// The sign-in no answer, file or request may show.
const PASSWORD: &str = "marker-7f3a-calendar-password";

/// Ada's desk: a calendar server that knows her, a host, a device of hers
/// and the workspace its page is in.
struct Desk {
    server: fake_caldav::Server,
    workspaces: Workspaces,
    host: HostWorkspaceActions,
    device: DeviceCert,
    workspace_id: WorkspaceId,
    _relay: TestHost,
    _dir: tempfile::TempDir,
}

impl Desk {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let relay = test_host(dir.path(), &key, &owner).await;
        let workspaces = relay.workspaces().clone();
        let host = HostWorkspaceActions::new();
        host.attach(workspaces.clone());
        let device = key_of(&SigningKey::from_bytes(&rand::random()), "studio");
        let workspace_id = workspaces.create("Calendars").await.unwrap().workspace_id;
        let server = fake_caldav::Server::start("ada", PASSWORD);
        server.add_calendar("work", "Work");
        Self {
            server,
            workspaces,
            host,
            device,
            workspace_id,
            _relay: relay,
            _dir: dir,
        }
    }

    fn page(&self) -> impl ActionHost + Clone + Send + 'static {
        self.host.in_workspace_as(self.workspace_id, &self.device)
    }

    fn ada(&self) -> Caller {
        caller(&self.device)
    }

    /// The agent `name` on Ada's device, over the connection it certified.
    fn agent(&self, name: &str) -> Caller {
        Caller::delegated_agent(
            name,
            self.device.user_public_key,
            self.device.device_public_key,
        )
    }

    /// What `caller` is answered when it asks `action` of the calendars.
    async fn ask(
        &self,
        caller: &Caller,
        action: &str,
        argument: Value,
    ) -> Result<Value, ActionHostError> {
        let target = RemoteActionTarget {
            provider: person_accounts::PROVIDER.to_owned(),
            address: ActionAddress::new(held::RESOURCE, action),
        };
        asked(&self.page(), caller, target, argument).await
    }

    /// Ada adds her account with the password she types.
    async fn add(&self) -> Value {
        self.ask(
            &self.ada(),
            held::ADD,
            json!({"serverUrl": self.server.url(), "username": "ada", "password": PASSWORD, "name": "Mine"}),
        )
        .await
        .expect("her own server takes her sign-in")
    }

    /// The starts of what the calendars hold in the week of 16 November.
    async fn week(&self, caller: &Caller) -> Vec<String> {
        let read = self
            .ask(
                caller,
                held::ENTRIES,
                json!({"from": "2026-11-16", "to": "2026-11-23", "zone": "Europe/Berlin"}),
            )
            .await
            .expect("a span reads");
        read["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .map(|entry| {
                format!(
                    "{} {}",
                    entry["title"].as_str().unwrap(),
                    entry["start"].as_str().unwrap()
                )
            })
            .collect()
    }
}

async fn asked(
    page: &(impl ActionHost + Sync),
    caller: &Caller,
    target: RemoteActionTarget,
    argument: Value,
) -> Result<Value, ActionHostError> {
    let argument = match argument {
        Value::Object(map) if !map.is_empty() => InvokeArgument::Json(map),
        _ => InvokeArgument::None,
    };
    let request = InvokeRequest {
        invocation: InvocationId::new(),
        target,
        argument,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    };
    let receipt = settled(page.invoke(caller, request)).await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("the host answers at once, not {other:?}"),
    }
}

fn nobody(what: &str, answer: Result<Value, ActionHostError>) {
    let refused = answer.expect_err(what);
    assert!(
        matches!(refused, ActionHostError::UnknownAction),
        "{what}: {refused:?}"
    );
}

fn refused(what: &str, answer: Result<Value, ActionHostError>) -> String {
    match answer.expect_err(what) {
        ActionHostError::Refused { reason } => reason,
        other => panic!("{what}: {other:?}"),
    }
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else {
            found.push(path);
        }
    }
    found
}

/// A meeting on the server: one entry, with a line this calendar does not
/// model.
fn meeting(uid: &str, summary: &str, start: &str, more: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//Server//EN\r\nBEGIN:VEVENT\r\n\
         UID:{uid}\r\nSUMMARY:{summary}\r\nDTSTART;TZID=Europe/Berlin:{start}\r\nDURATION:PT30M\r\n\
         X-SERVER-NOTE:kept as it is\r\n{more}END:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_calendar_on_the_persons_own_server_is_kept_read_and_filed_by_their_host() {
    let desk = Desk::new().await;
    let ada = desk.ada();
    desk.server.put(
        "work",
        "review.ics",
        &meeting(
            "review-1@example.org",
            "Review",
            "20261117T100000",
            "CATEGORIES:Board\r\n",
        ),
    );
    desk.server.put(
        "work",
        "standup.ics",
        &meeting(
            "standup-1@example.org",
            "Standup",
            "20261116T093000",
            "RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR\r\nEXDATE;TZID=Europe/Berlin:20261118T093000\r\n",
        ),
    );

    // A calendar she may read and not write.
    desk.server.add_calendar("zz-holidays", "Holidays");
    desk.server.make_read_only("zz-holidays");

    // A server that offers no encryption is refused before anything is sent.
    let clear = desk
        .ask(
            &ada,
            held::ADD,
            json!({"serverUrl": "http://calendar.example.test/dav/", "username": "ada", "password": PASSWORD}),
        )
        .await;
    assert!(refused("a sign-in in the clear", clear).contains("without HTTPS"));
    let wrong = desk
        .ask(
            &ada,
            held::ADD,
            json!({"serverUrl": desk.server.url(), "username": "ada", "password": "not-hers"}),
        )
        .await;
    refused("a sign-in the server does not take", wrong);
    let none = desk
        .ask(&ada, held::LIST, json!({}))
        .await
        .expect("the list reads");
    assert_eq!(
        none["accounts"],
        json!([]),
        "nothing is kept of a refused account"
    );

    let account = desk.add().await;
    assert_eq!(account["name"], "Mine");
    assert_eq!(account["hasSecret"], true);
    assert_eq!(account["calendars"][0]["name"], "Work");
    assert_eq!(account["calendars"][0]["readOnly"], false);
    assert_eq!(account["calendars"][1]["name"], "Holidays");
    assert_eq!(account["calendars"][1]["readOnly"], true);
    assert!(
        account["syncedAtMs"].as_i64().is_some_and(|at| at > 0),
        "{account}"
    );
    let id = account["id"].as_str().expect("its id").to_owned();
    assert!(id.starts_with("caldav:ada@127.0.0.1:"), "{id}");

    // Every occurrence, with every device off: the host read them itself.
    assert_eq!(
        desk.week(&ada).await,
        [
            "Standup 2026-11-16T09:30:00+01:00",
            "Standup 2026-11-17T09:30:00+01:00",
            "Review 2026-11-17T10:00:00+01:00",
            "Standup 2026-11-19T09:30:00+01:00",
            "Standup 2026-11-20T09:30:00+01:00",
        ]
    );
    let board = desk
        .ask(
            &ada,
            held::ENTRIES,
            json!({"from": "2026-11-16", "to": "2026-11-23", "zone": "Europe/Berlin", "category": "board"}),
        )
        .await
        .expect("a category narrows a span");
    assert_eq!(board["entries"].as_array().map(Vec::len), Some(1));
    assert_eq!(board["entries"][0]["categories"], json!(["Board"]));
    assert_eq!(board["entries"][0]["account"], id.as_str());
    let review = board["entries"][0]["entry"]
        .as_str()
        .expect("its id")
        .to_owned();
    let free = desk
        .ask(
            &ada,
            held::FREE,
            json!({"from": "2026-11-17", "to": "2026-11-18", "zone": "Europe/Berlin", "dayEnd": "12:00"}),
        )
        .await
        .expect("free time reads");
    let gaps: Vec<i64> = free["free"]
        .as_array()
        .expect("gaps")
        .iter()
        .filter_map(|gap| gap["minutes"].as_i64())
        .collect();
    assert_eq!(gaps, [30, 90], "09:00 to 09:30 and 10:30 to 12:00");

    // What another client does on the server is here after the next pull:
    // a moved entry at its new time, a removed one gone.
    desk.server.put(
        "work",
        "review.ics",
        &meeting(
            "review-1@example.org",
            "Review",
            "20261117T150000",
            "CATEGORIES:Board\r\n",
        ),
    );
    desk.server.remove("work", "standup.ics");
    desk.ask(&ada, held::SYNC, json!({}))
        .await
        .expect("the server is asked now");
    assert_eq!(desk.week(&ada).await, ["Review 2026-11-17T15:00:00+01:00"]);

    // A category is written into the server's own field, and every line
    // this calendar does not model stays as the server had it.
    let filed = desk
        .ask(
            &ada,
            held::SET_CATEGORIES,
            json!({"entry": review, "categories": ["Board", "Deal, signed"]}),
        )
        .await
        .expect("an entry is filed");
    assert_eq!(filed["categories"], json!(["Board", "Deal, signed"]));
    let on_server = desk.server.held("work");
    assert_eq!(on_server.len(), 1);
    let text = &on_server[0].1;
    assert!(
        text.contains("CATEGORIES:Board,Deal\\, signed\r\n"),
        "{text}"
    );
    assert_eq!(text.matches("CATEGORIES").count(), 1, "{text}");
    assert!(text.contains("X-SERVER-NOTE:kept as it is\r\n"), "{text}");
    let read = desk
        .ask(
            &ada,
            held::ENTRIES,
            json!({"from": "2026-11-17", "to": "2026-11-18", "category": "Deal, signed"}),
        )
        .await
        .expect("and read back");
    assert_eq!(
        read["entries"][0]["categories"],
        json!(["Board", "Deal, signed"])
    );
    assert_eq!(
        read["zone"], "Europe/Berlin",
        "a reader who names no zone gets the calendar's"
    );

    // An invitation is one entry, however often it comes.
    let invitation = |method: &str, start: &str| {
        meeting("offsite-9@example.org", "Offsite", start, "").replace(
            "VERSION:2.0\r\n",
            &format!("VERSION:2.0\r\nMETHOD:{method}\r\n"),
        )
    };
    let shut = desk
        .ask(
            &ada,
            held::ADD_INVITATION,
            json!({"ics": invitation("REQUEST", "20261119T140000"), "calendar": "Holidays"}),
        )
        .await;
    assert!(refused("a calendar she may only read", shut).contains("read-only"));
    let add = |ics: String| desk.ask(&ada, held::ADD_INVITATION, json!({"ics": ics}));
    let made = add(invitation("REQUEST", "20261119T140000"))
        .await
        .expect("an invitation goes in");
    assert_eq!(made["made"], "created", "{made}");
    assert_eq!(desk.server.held("work").len(), 2);
    assert!(
        desk.server
            .held("work")
            .iter()
            .all(|(_, text)| !text.contains("METHOD")),
        "an entry a server keeps has no method"
    );
    let again = add(invitation("REQUEST", "20261119T140000"))
        .await
        .expect("the same again");
    assert_eq!(again["made"], "unchanged", "{again}");
    assert_eq!(desk.server.held("work").len(), 2, "one entry, not two");
    let moved = add(invitation("REQUEST", "20261119T160000"))
        .await
        .expect("a later version");
    assert_eq!(moved["made"], "updated", "{moved}");
    assert_eq!(
        desk.week(&ada).await,
        [
            "Review 2026-11-17T15:00:00+01:00",
            "Offsite 2026-11-19T16:00:00+01:00"
        ]
    );
    let gone = add(invitation("CANCEL", "20261119T160000"))
        .await
        .expect("its cancellation");
    assert_eq!(gone["made"], "cancelled", "{gone}");
    assert_eq!(desk.server.held("work").len(), 1);

    // A server that never learned to tell what changed is read whole.
    desk.server.without_sync_reports();
    desk.server.put(
        "work",
        "late.ics",
        &meeting("late-2@example.org", "Late", "20261120T170000", ""),
    );
    desk.ask(&ada, held::SYNC, json!({}))
        .await
        .expect("it is read whole");
    assert_eq!(
        desk.week(&ada).await,
        [
            "Review 2026-11-17T15:00:00+01:00",
            "Late 2026-11-20T17:00:00+01:00"
        ]
    );

    // The sign-in is in no answer, in nothing kept beside the vault, and
    // went to the server only as its credentials.
    let listed = desk
        .ask(&ada, held::LIST, json!({}))
        .await
        .expect("the list reads");
    assert!(!listed.to_string().contains(PASSWORD));
    let root = desk
        .workspaces
        .person_settings_root(&desk.device.user_public_key);
    let vault = accounts::vault_dir_in(&root);
    let kept: Vec<PathBuf> = files_under(&root)
        .into_iter()
        .filter(|file| !file.starts_with(&vault))
        .collect();
    assert!(
        kept.iter()
            .any(|file| file.to_string_lossy().contains("calendars")),
        "{kept:?}"
    );
    for file in kept {
        let text = String::from_utf8_lossy(&std::fs::read(&file).unwrap()).into_owned();
        assert!(!text.contains(PASSWORD), "{}", file.display());
    }
    let requests = desk.server.requests();
    assert!(requests.len() > 10);
    for request in requests {
        assert!(
            request.header("Authorization").is_some(),
            "{} {}",
            request.method,
            request.target
        );
        assert!(!request.target.contains(PASSWORD) && !request.body_text().contains(PASSWORD));
    }

    let left = desk
        .ask(&ada, held::REMOVE, json!({"account": id}))
        .await
        .expect("she removes it");
    assert_eq!(left["accounts"], json!([]));
    assert!(
        files_under(&root)
            .iter()
            .all(|file| !file.to_string_lossy().contains("calendars")),
        "what was kept of it goes with it"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_person_and_the_agent_they_named_reach_their_calendars() {
    let desk = Desk::new().await;
    let ada = desk.ada();
    desk.server.put(
        "work",
        "review.ics",
        &meeting("review-1@example.org", "Review", "20261117T100000", ""),
    );
    let id = desk.add().await["id"].as_str().expect("its id").to_owned();
    let assistant = desk.agent("assistant");

    // Nothing is granted by default.
    nobody(
        "an agent she did not name",
        desk.ask(&assistant, held::LIST, json!({})).await,
    );
    let naming = RemoteActionTarget {
        provider: person_accounts::PROVIDER.to_owned(),
        address: ActionAddress::new(person_accounts::RESOURCE, person_accounts::ASSISTANT),
    };
    asked(
        &desk.page(),
        &ada,
        naming,
        json!({"account": id, "agent": "assistant", "may": true}),
    )
    .await
    .expect("she names her assistant for the calendar account");

    let listed = desk
        .ask(&assistant, held::LIST, json!({}))
        .await
        .expect("it lists");
    assert_eq!(listed["accounts"][0]["id"], id.as_str());
    assert_eq!(
        desk.week(&assistant).await,
        ["Review 2026-11-17T10:00:00+01:00"]
    );
    let invitation = meeting("offsite-9@example.org", "Offsite", "20261119T140000", "")
        .replace("VERSION:2.0\r\n", "VERSION:2.0\r\nMETHOD:REQUEST\r\n");
    let made = desk
        .ask(&assistant, held::ADD_INVITATION, json!({"ics": invitation}))
        .await
        .expect("it puts an invitation in");
    assert_eq!(made["made"], "created");

    // What is the person's alone is not there for it, and nobody else is
    // anybody to her calendars.
    nobody(
        "removing is hers",
        desk.ask(&assistant, held::REMOVE, json!({"account": id}))
            .await,
    );
    nobody(
        "a typed password is hers",
        desk.ask(
            &assistant,
            held::ADD,
            json!({"serverUrl": desk.server.url(), "username": "ada", "password": PASSWORD}),
        )
        .await,
    );
    nobody(
        "another agent",
        desk.ask(&desk.agent("stranger"), held::LIST, json!({}))
            .await,
    );
    let bob = key_of(&SigningKey::from_bytes(&rand::random()), "laptop");
    let bobs_page = desk.host.in_workspace_as(desk.workspace_id, &bob);
    let target = RemoteActionTarget {
        provider: person_accounts::PROVIDER.to_owned(),
        address: ActionAddress::new(held::RESOURCE, held::LIST),
    };
    let his = asked(&bobs_page, &caller(&bob), target, json!({}))
        .await
        .expect("his own list");
    assert_eq!(
        his["accounts"],
        json!([]),
        "another person reads their own, which is empty"
    );

    // By reference: the vault decides. A login that belongs to the server
    // adds the account, one that belongs elsewhere is never sent there.
    desk.ask(&ada, held::REMOVE, json!({"account": id}))
        .await
        .expect("she removes it");
    let page = desk.page();
    let keep = |belongs: String| {
        asked(
            &page,
            &ada,
            person_vault::target(person_vault::ADD),
            json!({"kind": "login", "belongsTo": [belongs], "username": "ada", "secret": PASSWORD}),
        )
    };
    let hers = keep(desk.server.url())
        .await
        .expect("she keeps her calendar's login");
    let elsewhere = keep("https://elsewhere.example".to_owned())
        .await
        .expect("and another");
    let wrong = desk
        .ask(
            &assistant,
            held::ADD_WITH_ITEM,
            json!({"serverUrl": desk.server.url(), "password": elsewhere["reference"]}),
        )
        .await;
    refused("an item that belongs elsewhere", wrong);
    let added = desk
        .ask(
            &assistant,
            held::ADD_WITH_ITEM,
            json!({"serverUrl": desk.server.url(), "password": hers["reference"]}),
        )
        .await
        .expect("her agent adds the account with a password it never sees");
    assert_eq!(added["username"], "ada", "the item's own name signs in");
    assert!(!added.to_string().contains(PASSWORD));
    assert_eq!(
        desk.week(&ada).await.len(),
        2,
        "and her calendar is read again"
    );
}
