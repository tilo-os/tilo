//! A person's calendars on their holder, against a calendar server the test
//! starts: an account is added only once its server took the sign-in, its
//! entries are kept and read with every device off, a category is the
//! server's own field, an invitation is one entry, and only the person and
//! the agent they named reach any of it.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the holder a test binds is kept until the test ends; dropping it earlier closes what it drives"
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

use collab_types::WorkspaceId;
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionHost as ActionService, ActionHostError as ActionError, Caller, CatalogueQuery,
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget, person_accounts,
    person_vault,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_user_identity::DeviceCert;

use super::super::HostWorkspaceActions as HolderActions;
use super::super::tests::{caller, settled};
use super::accounts_tests::key_of;
use crate::workspaces::Workspaces;
use crate::workspaces::fixtures::TestHost as TestHolder;
use crate::workspaces::person_settings::calendars::{self as held, fake_caldav};
use crate::workspaces::tests::{governor, test_host as test_holder};

/// The sign-in no answer, file or request may show.
const PASSWORD: &str = "marker-7f3a-calendar-password";

/// What Ada has: a calendar server that knows her, the holder that keeps her
/// calendars, a device of hers and the workspace its page is in.
struct Ada {
    server: fake_caldav::Server,
    workspaces: Workspaces,
    actions: HolderActions,
    device: DeviceCert,
    workspace_id: WorkspaceId,
    _holder: TestHolder,
    _dir: tempfile::TempDir,
}

impl Ada {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let holder = test_holder(dir.path(), &key, &owner).await;
        let workspaces = holder.workspaces().clone();
        let actions = HolderActions::new();
        actions.attach(workspaces.clone());
        let device = key_of(&SigningKey::from_bytes(&rand::random()), "studio");
        let workspace_id = workspaces.create("Calendars").await.unwrap().workspace_id;
        let server = fake_caldav::Server::start("ada", PASSWORD);
        server.add_calendar("work", "Work");
        Self {
            server,
            workspaces,
            actions,
            device,
            workspace_id,
            _holder: holder,
            _dir: dir,
        }
    }

    fn page(&self) -> impl ActionService + Clone + Send + 'static {
        self.actions
            .in_workspace_as(self.workspace_id, &self.device)
    }

    fn herself(&self) -> Caller {
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
    ) -> Result<Value, ActionError> {
        let target = RemoteActionTarget {
            provider: person_accounts::PROVIDER.to_owned(),
            address: ActionAddress::new(held::RESOURCE, action),
        };
        asked(&self.page(), caller, target, argument).await
    }

    /// Ada adds her account with the password she types.
    async fn add(&self) -> Value {
        self.ask(
            &self.herself(),
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
    page: &(impl ActionService + Sync),
    caller: &Caller,
    target: RemoteActionTarget,
    argument: Value,
) -> Result<Value, ActionError> {
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
        other => panic!("the holder answers at once, not {other:?}"),
    }
}

fn nobody(what: &str, answer: Result<Value, ActionError>) {
    let refused = answer.expect_err(what);
    assert!(
        matches!(refused, ActionError::UnknownAction),
        "{what}: {refused:?}"
    );
}

fn refused(what: &str, answer: Result<Value, ActionError>) -> String {
    match answer.expect_err(what) {
        ActionError::Refused { reason } => reason,
        other => panic!("{what}: {other:?}"),
    }
}

/// What the catalogue offers `caller` of a person's accounts on `page`, each
/// action under its resource, in order: nothing when it is offered none.
async fn offered(page: &(impl ActionService + Sync), caller: &Caller) -> Vec<String> {
    let query = CatalogueQuery {
        provider: Some(person_accounts::PROVIDER.into()),
        max_results: 100,
        ..CatalogueQuery::default()
    };
    let mut rows: Vec<String> = settled(page.query(caller, query))
        .await
        .map(|page| {
            page.rows
                .into_iter()
                .filter_map(|row| row.action)
                .map(|action| format!("{}#{}", action.address.resource, action.address.action))
                .collect()
        })
        .unwrap_or_default();
    rows.sort();
    rows
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
async fn a_calendar_on_the_persons_own_server_is_kept_read_and_filed_by_their_holder() {
    let ada = Ada::new().await;
    let her = ada.herself();
    ada.server.put(
        "work",
        "review.ics",
        &meeting(
            "review-1@example.org",
            "Review",
            "20261117T100000",
            "CATEGORIES:Board\r\n",
        ),
    );
    ada.server.put(
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
    ada.server.add_calendar("zz-holidays", "Holidays");
    ada.server.make_read_only("zz-holidays");

    // A server that offers no encryption is refused before anything is sent.
    let clear = ada
        .ask(
            &her,
            held::ADD,
            json!({"serverUrl": "http://calendar.example.test/dav/", "username": "ada", "password": PASSWORD}),
        )
        .await;
    assert!(refused("a sign-in in the clear", clear).contains("without HTTPS"));
    let wrong = ada
        .ask(
            &her,
            held::ADD,
            json!({"serverUrl": ada.server.url(), "username": "ada", "password": "not-hers"}),
        )
        .await;
    refused("a sign-in the server does not take", wrong);
    let none = ada
        .ask(&her, held::LIST, json!({}))
        .await
        .expect("the list reads");
    assert_eq!(
        none["accounts"],
        json!([]),
        "nothing is kept of a refused account"
    );

    let account = ada.add().await;
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

    // Every occurrence, with every device off: the holder read them itself.
    assert_eq!(
        ada.week(&her).await,
        [
            "Standup 2026-11-16T09:30:00+01:00",
            "Standup 2026-11-17T09:30:00+01:00",
            "Review 2026-11-17T10:00:00+01:00",
            "Standup 2026-11-19T09:30:00+01:00",
            "Standup 2026-11-20T09:30:00+01:00",
        ]
    );
    let board = ada
        .ask(
            &her,
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
    let free = ada
        .ask(
            &her,
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
    ada.server.put(
        "work",
        "review.ics",
        &meeting(
            "review-1@example.org",
            "Review",
            "20261117T150000",
            "CATEGORIES:Board\r\n",
        ),
    );
    ada.server.remove("work", "standup.ics");
    ada.ask(&her, held::SYNC, json!({}))
        .await
        .expect("the server is asked now");
    assert_eq!(ada.week(&her).await, ["Review 2026-11-17T15:00:00+01:00"]);

    // A category is written into the server's own field, and every line
    // this calendar does not model stays as the server had it.
    let filed = ada
        .ask(
            &her,
            held::SET_CATEGORIES,
            json!({"entry": review, "categories": ["Board", "Deal, signed"]}),
        )
        .await
        .expect("an entry is filed");
    assert_eq!(filed["categories"], json!(["Board", "Deal, signed"]));
    let on_server = ada.server.held("work");
    assert_eq!(on_server.len(), 1);
    let text = &on_server[0].1;
    assert!(
        text.contains("CATEGORIES:Board,Deal\\, signed\r\n"),
        "{text}"
    );
    assert_eq!(text.matches("CATEGORIES").count(), 1, "{text}");
    assert!(text.contains("X-SERVER-NOTE:kept as it is\r\n"), "{text}");
    let read = ada
        .ask(
            &her,
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
    let shut = ada
        .ask(
            &her,
            held::ADD_INVITATION,
            json!({"ics": invitation("REQUEST", "20261119T140000"), "calendar": "Holidays"}),
        )
        .await;
    assert!(refused("a calendar she may only read", shut).contains("read-only"));
    let add = |ics: String| ada.ask(&her, held::ADD_INVITATION, json!({"ics": ics}));
    let made = add(invitation("REQUEST", "20261119T140000"))
        .await
        .expect("an invitation goes in");
    assert_eq!(made["made"], "created", "{made}");
    assert_eq!(ada.server.held("work").len(), 2);
    assert!(
        ada.server
            .held("work")
            .iter()
            .all(|(_, text)| !text.contains("METHOD")),
        "an entry a server keeps has no method"
    );
    let again = add(invitation("REQUEST", "20261119T140000"))
        .await
        .expect("the same again");
    assert_eq!(again["made"], "unchanged", "{again}");
    assert_eq!(ada.server.held("work").len(), 2, "one entry, not two");
    let moved = add(invitation("REQUEST", "20261119T160000"))
        .await
        .expect("a later version");
    assert_eq!(moved["made"], "updated", "{moved}");
    assert_eq!(
        ada.week(&her).await,
        [
            "Review 2026-11-17T15:00:00+01:00",
            "Offsite 2026-11-19T16:00:00+01:00"
        ]
    );
    let gone = add(invitation("CANCEL", "20261119T160000"))
        .await
        .expect("its cancellation");
    assert_eq!(gone["made"], "cancelled", "{gone}");
    assert_eq!(ada.server.held("work").len(), 1);

    // A server that never learned to tell what changed is read whole.
    ada.server.without_sync_reports();
    ada.server.put(
        "work",
        "late.ics",
        &meeting("late-2@example.org", "Late", "20261120T170000", ""),
    );
    ada.ask(&her, held::SYNC, json!({}))
        .await
        .expect("it is read whole");
    assert_eq!(
        ada.week(&her).await,
        [
            "Review 2026-11-17T15:00:00+01:00",
            "Late 2026-11-20T17:00:00+01:00"
        ]
    );

    // The sign-in is in no answer, in nothing kept beside the vault, and
    // went to the server only as its credentials.
    let listed = ada
        .ask(&her, held::LIST, json!({}))
        .await
        .expect("the list reads");
    assert!(!listed.to_string().contains(PASSWORD));
    let root = ada
        .workspaces
        .person_settings_root(&ada.device.user_public_key);
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
    let requests = ada.server.requests();
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

    let left = ada
        .ask(&her, held::REMOVE, json!({"account": id}))
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
    let ada = Ada::new().await;
    let her = ada.herself();
    ada.server.put(
        "work",
        "review.ics",
        &meeting("review-1@example.org", "Review", "20261117T100000", ""),
    );
    let id = ada.add().await["id"].as_str().expect("its id").to_owned();
    let assistant = ada.agent("assistant");

    // Nothing is granted by default.
    nobody(
        "an agent she did not name",
        ada.ask(&assistant, held::LIST, json!({})).await,
    );
    let naming = RemoteActionTarget {
        provider: person_accounts::PROVIDER.to_owned(),
        address: ActionAddress::new(person_accounts::RESOURCE, person_accounts::ASSISTANT),
    };
    asked(
        &ada.page(),
        &her,
        naming,
        json!({"account": id, "agent": "assistant", "may": true}),
    )
    .await
    .expect("she names her assistant for the calendar account");

    let listed = ada
        .ask(&assistant, held::LIST, json!({}))
        .await
        .expect("it lists");
    assert_eq!(listed["accounts"][0]["id"], id.as_str());
    assert_eq!(
        ada.week(&assistant).await,
        ["Review 2026-11-17T10:00:00+01:00"]
    );
    let invitation = meeting("offsite-9@example.org", "Offsite", "20261119T140000", "")
        .replace("VERSION:2.0\r\n", "VERSION:2.0\r\nMETHOD:REQUEST\r\n");
    let made = ada
        .ask(&assistant, held::ADD_INVITATION, json!({"ics": invitation}))
        .await
        .expect("it puts an invitation in");
    assert_eq!(made["made"], "created");

    // What is the person's alone is not there for it, and nobody else is
    // anybody to her calendars.
    nobody(
        "removing is hers",
        ada.ask(&assistant, held::REMOVE, json!({"account": id}))
            .await,
    );
    nobody(
        "a typed password is hers",
        ada.ask(
            &assistant,
            held::ADD,
            json!({"serverUrl": ada.server.url(), "username": "ada", "password": PASSWORD}),
        )
        .await,
    );
    nobody(
        "another agent",
        ada.ask(&ada.agent("stranger"), held::LIST, json!({})).await,
    );
    let bob = key_of(&SigningKey::from_bytes(&rand::random()), "laptop");
    let bobs_page = ada.actions.in_workspace_as(ada.workspace_id, &bob);
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
    ada.ask(&her, held::REMOVE, json!({"account": id}))
        .await
        .expect("she removes it");
    let page = ada.page();
    let keep = |belongs: String| {
        asked(
            &page,
            &her,
            person_vault::target(person_vault::ADD),
            json!({"kind": "login", "belongsTo": [belongs], "username": "ada", "secret": PASSWORD}),
        )
    };
    let hers = keep(ada.server.url())
        .await
        .expect("she keeps her calendar's login");
    let elsewhere = keep("https://elsewhere.example".to_owned())
        .await
        .expect("and another");
    let wrong = ada
        .ask(
            &assistant,
            held::ADD_WITH_ITEM,
            json!({"serverUrl": ada.server.url(), "password": elsewhere["reference"]}),
        )
        .await;
    refused("an item that belongs elsewhere", wrong);
    let added = ada
        .ask(
            &assistant,
            held::ADD_WITH_ITEM,
            json!({"serverUrl": ada.server.url(), "password": hers["reference"]}),
        )
        .await
        .expect("her agent adds the account with a password it never sees");
    assert_eq!(added["username"], "ada", "the item's own name signs in");
    assert!(!added.to_string().contains(PASSWORD));
    assert_eq!(
        ada.week(&her).await.len(),
        2,
        "and her calendar is read again"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_catalogue_offers_her_calendars_to_the_agent_she_named_and_to_nobody_else() {
    let ada = Ada::new().await;
    let her = ada.herself();
    let page = ada.page();
    let id = ada.add().await["id"].as_str().expect("its id").to_owned();
    let assistant = ada.agent("assistant");
    let name = |may: bool| {
        let naming = RemoteActionTarget {
            provider: person_accounts::PROVIDER.to_owned(),
            address: ActionAddress::new(person_accounts::RESOURCE, person_accounts::ASSISTANT),
        };
        asked(
            &page,
            &her,
            naming,
            json!({"account": id, "agent": "assistant", "may": may}),
        )
    };

    // Nothing is offered by default.
    assert!(
        offered(&page, &assistant).await.is_empty(),
        "an agent she did not name"
    );
    name(true)
        .await
        .expect("she names her assistant for the calendar account");

    // What it may ask and nothing that is hers alone: no account added with
    // a typed password, none removed. Named for a calendar, it is offered
    // nothing of mail.
    assert_eq!(
        offered(&page, &assistant).await,
        [
            "calendars#add-invitation",
            "calendars#entries",
            "calendars#free",
            "calendars#list",
            "calendars#set-categories",
            "calendars#sync",
        ]
    );
    assert!(
        offered(&page, &ada.agent("stranger")).await.is_empty(),
        "another agent on her device"
    );
    assert!(
        offered(&page, &her).await.is_empty(),
        "her own page asks directly and is offered no row"
    );
    let bob = key_of(&SigningKey::from_bytes(&rand::random()), "laptop");
    let bobs_page = ada.actions.in_workspace_as(ada.workspace_id, &bob);
    let bobs_agent =
        Caller::delegated_agent("assistant", bob.user_public_key, bob.device_public_key);
    assert!(
        offered(&bobs_page, &bobs_agent).await.is_empty(),
        "an agent of the same name that another person keeps"
    );

    // Her word taken back takes the rows with it.
    name(false).await.expect("she takes her word back");
    assert!(
        offered(&page, &assistant).await.is_empty(),
        "an agent she no longer names"
    );
}

/// What an automation that waits on an entry's time is handed: every
/// occurrence as the pull of her own server left it, for the agent she named
/// and for nobody else.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_times_her_calendars_hold_are_read_for_the_agent_she_named_alone() {
    let ada = Ada::new().await;
    let her = ada.herself();
    // A series over the end of summer time, at 09:30 where she lives, with a
    // day it leaves out and a day moved to the afternoon; and a whole day.
    let series = meeting(
        "standup-1@example.org",
        "Standup",
        "20261019T093000",
        "RRULE:FREQ=DAILY;COUNT=9\r\nEXDATE;TZID=Europe/Berlin:20261021T093000\r\n",
    )
    .replace(
        "END:VCALENDAR\r\n",
        "BEGIN:VEVENT\r\nUID:standup-1@example.org\r\nSUMMARY:Standup\r\n\
         RECURRENCE-ID;TZID=Europe/Berlin:20261022T093000\r\n\
         DTSTART;TZID=Europe/Berlin:20261022T140000\r\nDURATION:PT30M\r\nEND:VEVENT\r\n\
         END:VCALENDAR\r\n",
    );
    ada.server.put("work", "standup.ics", &series);
    ada.server.put(
        "work",
        "offsite.ics",
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Example//Server//EN\r\nBEGIN:VEVENT\r\n\
         UID:offsite-1@example.org\r\nSUMMARY:Offsite\r\nDTSTART;VALUE=DATE:20261023\r\n\
         DTEND;VALUE=DATE:20261024\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
    );
    let id = ada.add().await["id"].as_str().expect("its id").to_owned();
    let person = ada.device.user_public_key;
    let workspace = ada.workspace_id.to_string();
    // From Monday 19 October 2026 to before Wednesday 28 October, in UTC.
    let (from, to) = (1_792_368_000_000_i64, 1_793_145_600_000_i64);
    let read = |agent: &'static str| {
        ada.workspaces
            .calendar_occurrences_for(&person, &workspace, agent, from, to)
    };

    // Nothing of her calendar for an agent she did not name.
    assert_eq!(read("assistant").await, Ok(Vec::new()));
    let naming = RemoteActionTarget {
        provider: person_accounts::PROVIDER.to_owned(),
        address: ActionAddress::new(person_accounts::RESOURCE, person_accounts::ASSISTANT),
    };
    asked(
        &ada.page(),
        &her,
        naming,
        json!({"account": id, "agent": "assistant", "may": true}),
    )
    .await
    .expect("she names her assistant for the calendar account");
    assert_eq!(read("stranger").await, Ok(Vec::new()));

    // Every occurrence at the moment it starts: 09:30 where she lives is
    // 07:30 UTC in summer time and 08:30 after it ended on the 25th; the day
    // left out is not there, the moved one is where it was moved to under
    // the date it had in its series, and the whole day begins at her
    // midnight.
    let held: Vec<(String, String, i64, bool)> = read("assistant")
        .await
        .expect("her assistant's read")
        .iter()
        .map(|occurrence| {
            (
                occurrence["title"].as_str().unwrap().to_owned(),
                occurrence["occurrence"].as_str().unwrap().to_owned(),
                occurrence["startMs"].as_i64().unwrap(),
                occurrence["moved"].as_bool().unwrap(),
            )
        })
        .collect();
    let expected: Vec<(String, String, i64, bool)> = [
        ("Standup", "2026-10-19", 1_792_395_000_000_i64, false),
        ("Standup", "2026-10-20", 1_792_481_400_000_i64, false),
        ("Standup", "2026-10-22", 1_792_670_400_000_i64, true),
        ("Offsite", "2026-10-23", 1_792_706_400_000_i64, false),
        ("Standup", "2026-10-23", 1_792_740_600_000_i64, false),
        ("Standup", "2026-10-24", 1_792_827_000_000_i64, false),
        ("Standup", "2026-10-25", 1_792_917_000_000_i64, false),
        ("Standup", "2026-10-26", 1_793_003_400_000_i64, false),
        ("Standup", "2026-10-27", 1_793_089_800_000_i64, false),
    ]
    .into_iter()
    .map(|(title, day, at, moved): (&str, &str, i64, bool)| {
        (title.to_owned(), day.to_owned(), at, moved)
    })
    .collect();
    assert_eq!(held, expected);
}
