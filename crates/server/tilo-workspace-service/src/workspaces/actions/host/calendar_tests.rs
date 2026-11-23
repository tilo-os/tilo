//! A workspace's calendars on the host that serves it, against a calendar
//! server the test starts: an account is added only once its server took the
//! sign-in, its entries are kept and read with every device off, a category is
//! the server's own field, and an invitation is one entry. Every member of the
//! workspace reaches them, and nobody outside it does; what an agent a member
//! certified changes waits for that member's answer, unless their allowance
//! says otherwise, and what it may change at all is an invitation to its person
//! and nothing the person entered themselves. The calendar server is told to
//! write to nobody.

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
use std::sync::{Arc, Mutex};

use collab_types::WorkspaceId;
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::remote::person_asking::{ActionName, Asked, Thing, Words};
use tilo_actions::remote::{
    ActionCallError as ActionError, ActionProvider as ActionService, Caller, CatalogueQuery,
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget, person_accounts,
    person_vault,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_server_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

use super::super::ServiceWorkspaceActions as HolderActions;
use super::super::tests::{admit, agent_caller, caller, governed, settled};
use crate::workspaces::fixtures::TestHost as TestHolder;
use crate::workspaces::fixtures::test_host as test_holder;
use crate::workspaces::person_asking::Allowing;
use crate::workspaces::person_settings::calendars::{self as held, fake_caldav};
use crate::workspaces::tests::governor;
use crate::workspaces::{AsksPeople, PersonAnswer, Workspaces};

/// A device of the person `root`.
fn key_of(root: &SigningKey, label: &str) -> DeviceCert {
    let key = SigningKey::from_bytes(&rand::random());
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

/// A turn this server carries out reads her calendars and changes nothing.
#[path = "calendar_turn_tests.rs"]
mod turn;

/// An account connected for reading only is read, and never written to.
#[path = "calendar_read_only_tests.rs"]
mod read_only;

/// The sign-in no answer, file or request may show.
const PASSWORD: &str = "marker-7f3a-calendar-password";

/// The address her calendar server writes to her at, where her account
/// signs in with it.
const ADDRESS: &str = "ada@example.test";

/// What Ada has: a calendar server that knows her, the workspace the holder
/// keeps its calendars in, a device she is admitted on and its page.
struct Ada {
    /// The name her calendar account signs in with.
    user: &'static str,
    server: fake_caldav::Server,
    workspaces: Workspaces,
    actions: HolderActions,
    owner: DeviceCert,
    device: DeviceCert,
    workspace: String,
    workspace_id: WorkspaceId,
    _holder: TestHolder,
    _dir: tempfile::TempDir,
}

impl Ada {
    async fn new() -> Self {
        Self::signing_in_as("ada").await
    }

    /// Ada, whose calendar account signs in as `user`.
    async fn signing_in_as(user: &'static str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let holder = test_holder(dir.path(), &key, &owner).await;
        let workspaces = holder.workspaces().clone();
        // Her calendar server is this machine: the operator lets the server
        // call its own network, which it is otherwise kept from.
        workspaces.call_private_model_addresses(true);
        let (workspace, workspace_id, actions) = governed(&workspaces).await;
        let device = admit(&workspaces, &workspace, &owner, 1).await;
        let server = fake_caldav::Server::start(user, PASSWORD);
        server.add_calendar("work", "Work");
        Self {
            user,
            server,
            workspaces,
            actions,
            owner,
            device,
            workspace,
            workspace_id,
            _holder: holder,
            _dir: dir,
        }
    }

    /// Where the workspace keeps its calendars, its sign-ins among them.
    fn root(&self) -> PathBuf {
        self.workspaces
            .workspace_root(&self.workspace_id.to_string())
    }

    /// The occurrences of the workspace's calendars between Monday
    /// 19 October 2026 and before Wednesday 28 October, as an automation
    /// waiting on an entry's time reads them for `person`.
    async fn occurrences(&self, person: &[u8; 32]) -> Result<Vec<Value>, String> {
        self.workspaces
            .calendar_occurrences_for(
                self.workspace_id,
                person,
                1_792_368_000_000,
                1_793_145_600_000,
            )
            .await
    }

    fn page(&self) -> impl ActionService + Clone + Send + 'static {
        self.actions
            .in_workspace_as(self.workspace_id, &self.device)
    }

    fn herself(&self) -> Caller {
        caller(&self.device)
    }

    /// The agent `name`, on a key of its own Ada's root certified.
    fn agent(&self, name: &str) -> Caller {
        agent_caller(&self.device, name)
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
            json!({"serverUrl": self.server.url(), "username": self.user, "password": PASSWORD, "name": "Mine"}),
        )
        .await
        .expect("her own server takes her sign-in")
    }

    /// Ada adds her account and hands her session to her assistant: that
    /// agent, on her device.
    async fn with_her_assistant(&self) -> Caller {
        self.add().await;
        self.agent("assistant")
    }

    /// What `caller` is answered when it puts the invitation `ics` in.
    async fn put(&self, caller: &Caller, ics: &str) -> Result<Value, ActionError> {
        self.ask(caller, held::ADD_INVITATION, json!({"ics": ics}))
            .await
    }

    /// Her calendar server wrote to nobody by itself.
    fn nobody_was_mailed(&self, what: &str) {
        assert_eq!(
            self.server.mailed(),
            Vec::<String>::new(),
            "{what}: her calendar server wrote to people in her name"
        );
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
        signed: None,
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
    let root = ada.root();
    assert!(
        !accounts::vault_dir_in(&root).exists(),
        "the host keeps no vault of its own beside the workspace's"
    );
    let kept: Vec<PathBuf> = files_under(&root);
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

/// Every member of the workspace reaches the calendars it keeps, with their
/// own device and with an agent they certified; an agent's
/// change waits for its person's answer, and a refusal changes nothing. A
/// member of another workspace, a person who is no member, a bare agent turn
/// and an app are nobody to them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_member_reaches_the_workspaces_calendars_and_an_agents_change_asks_its_person() {
    let ada = Ada::new().await;
    let her = ada.herself();
    ada.server.put(
        "work",
        "review.ics",
        &meeting("review-1@example.org", "Review", "20261117T100000", ""),
    );
    let id = ada.add().await["id"].as_str().expect("its id").to_owned();
    let assistant = ada.agent("assistant");

    // She reads it, the agent she certified reads it, and
    // so does every other member of the workspace.
    let listed = ada
        .ask(&assistant, held::LIST, json!({}))
        .await
        .expect("it lists");
    assert_eq!(listed["accounts"][0]["id"], id.as_str());
    assert_eq!(
        ada.week(&assistant).await,
        ["Review 2026-11-17T10:00:00+01:00"]
    );
    let bo = admit(&ada.workspaces, &ada.workspace, &ada.owner, 2).await;
    let bos_page = ada.actions.in_workspace_as(ada.workspace_id, &bo);
    let target = RemoteActionTarget {
        provider: person_accounts::PROVIDER.to_owned(),
        address: ActionAddress::new(held::RESOURCE, held::LIST),
    };
    let theirs = asked(&bos_page, &caller(&bo), target, json!({}))
        .await
        .expect("a member's list");
    assert_eq!(
        theirs["accounts"][0]["id"],
        id.as_str(),
        "a member reads the account the workspace keeps"
    );

    // A change the agent makes waits for her answer, and her allowing it is
    // what puts it in.
    let invitation = meeting("offsite-9@example.org", "Offsite", "20261119T140000", "")
        .replace("VERSION:2.0\r\n", "VERSION:2.0\r\nMETHOD:REQUEST\r\n");
    let unasked = ada
        .ask(&assistant, held::ADD_INVITATION, json!({"ics": invitation}))
        .await;
    assert!(
        matches!(unasked, Err(ActionError::ConfirmationRequired { .. })),
        "with nobody to ask through: {unasked:?}"
    );
    let allowing = Answering::with(Some(ConfirmationAnswer::AllowOnce));
    ada.workspaces.ask_people(allowing.clone());
    let made = ada
        .ask(&assistant, held::ADD_INVITATION, json!({"ics": invitation}))
        .await
        .expect("she allowed what she was shown");
    assert_eq!(made["made"], "created");
    assert_eq!(allowing.count(), 1, "the change asked her");

    // What she refuses changes nothing: her account is not removed and none
    // is added with a password the agent typed.
    ada.workspaces
        .ask_people(Answering::with(Some(ConfirmationAnswer::Deny)));
    for (what, action, argument) in [
        ("removing the account", held::REMOVE, json!({"account": id})),
        (
            "adding one with a typed password",
            held::ADD,
            json!({"serverUrl": ada.server.url(), "username": "ada", "password": PASSWORD}),
        ),
    ] {
        let refused = ada.ask(&assistant, action, argument).await;
        assert!(
            matches!(refused, Err(ActionError::Refused { .. })),
            "{what}: {refused:?}"
        );
    }
    let kept = ada
        .ask(&her, held::LIST, json!({}))
        .await
        .expect("the list reads");
    assert_eq!(
        kept["accounts"][0]["id"],
        id.as_str(),
        "what she refused changed nothing"
    );

    // Nobody outside the workspace is anybody to its calendars, by any road.
    let (elsewhere, elsewhere_id, _) = governed(&ada.workspaces).await;
    let cy = admit(&ada.workspaces, &elsewhere, &ada.owner, 3).await;
    let cys_page = ada.actions.in_workspace_as(ada.workspace_id, &cy);
    let target = RemoteActionTarget {
        provider: person_accounts::PROVIDER.to_owned(),
        address: ActionAddress::new(held::RESOURCE, held::LIST),
    };
    let his = asked(&cys_page, &caller(&cy), target.clone(), json!({})).await;
    nobody("a member of another workspace", his);
    nobody(
        "their agent on their own device",
        asked(
            &cys_page,
            &agent_caller(&cy, "assistant"),
            target.clone(),
            json!({}),
        )
        .await,
    );
    let stranger = key_of(&SigningKey::from_bytes(&rand::random()), "laptop");
    let strangers_page = ada.actions.in_workspace_as(ada.workspace_id, &stranger);
    nobody(
        "a person who is no member",
        asked(
            &strangers_page,
            &caller(&stranger),
            target.clone(),
            json!({}),
        )
        .await,
    );
    let app =
        Caller::local_app("so.tilo.calendar").acting_for(hex::encode(ada.device.user_public_key));
    nobody(
        "an app",
        asked(&ada.page(), &app, target.clone(), json!({})).await,
    );
    // What the workspace in which Cy is a member keeps is none of this one's.
    let elsewhere_page = ada.actions.in_workspace_as(elsewhere_id, &cy);
    let theirs = asked(&elsewhere_page, &caller(&cy), target, json!({}))
        .await
        .expect("his own workspace's list");
    assert_eq!(theirs["accounts"], json!([]), "{theirs}");

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
    let outside = keep("https://elsewhere.example".to_owned())
        .await
        .expect("and another");
    ada.workspaces.ask_people(allowing.clone());
    let wrong = ada
        .ask(
            &assistant,
            held::ADD_WITH_ITEM,
            json!({"serverUrl": ada.server.url(), "password": outside["reference"]}),
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

/// An entry in which `organizer` invites `invited`, as a calendar holds one.
fn inviting(uid: &str, summary: &str, start: &str, organizer: &str, invited: &[&str]) -> String {
    let people: String = std::iter::once(format!("ORGANIZER;CN=Organiser:mailto:{organizer}\r\n"))
        .chain(invited.iter().map(|address| {
            format!("ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{address}\r\n")
        }))
        .collect();
    meeting(uid, summary, start, &people)
}

/// `entry` as a message carries it, under `method`.
fn carried(method: &str, entry: &str) -> String {
    entry.replace(
        "VERSION:2.0\r\n",
        &format!("VERSION:2.0\r\nMETHOD:{method}\r\n"),
    )
}

/// The lines of the entry `text` that name somebody, each whole.
fn naming_lines(text: &str) -> Vec<String> {
    text.replace("\r\n ", "")
        .lines()
        .filter(|line| line.starts_with("ORGANIZER") || line.starts_with("ATTENDEE"))
        .map(str::to_owned)
        .collect()
}

/// Ada answering every question about her calendar one way, or not at all,
/// and what she was asked.
struct Answering {
    answer: Option<ConfirmationAnswer>,
    questions: Mutex<Vec<([u8; 32], PendingConfirmation)>>,
}

impl Answering {
    fn with(answer: Option<ConfirmationAnswer>) -> Arc<Self> {
        Arc::new(Self {
            answer,
            questions: Mutex::default(),
        })
    }

    /// How many questions she was put.
    fn count(&self) -> usize {
        self.questions.lock().unwrap().len()
    }

    /// What the rows labelled `label` of the last question say, in order.
    fn rows(&self, label: &str) -> Vec<String> {
        self.questions
            .lock()
            .unwrap()
            .last()
            .map(|(_, question)| {
                question
                    .arguments
                    .iter()
                    .filter(|(labelled, _)| labelled == label)
                    .map(|(_, value)| value.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl AsksPeople for Answering {
    fn ask(&self, person: [u8; 32], question: PendingConfirmation) -> PersonAnswer {
        self.questions.lock().unwrap().push((person, question));
        let answer = self
            .answer
            .clone()
            .ok_or_else(|| "nobody answered".to_owned());
        Box::pin(async move { answer })
    }
}

/// An agent a member certified writes the text it
/// hands over itself, and reads the identity of every entry the calendar
/// holds. So what its person entered themselves is not its to change: a
/// cancellation under that identity removes nothing, another time under it
/// moves nothing, whatever sender the text names, and the same holds for an
/// entry in which the person invites others. The person changes what is
/// theirs by asking.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_she_entered_herself_is_not_her_assistants_to_change_or_remove() {
    let ada = Ada::signing_in_as(ADDRESS).await;
    // Friday's board meeting, which she entered herself, and a dinner she
    // invites Bo to, as another client of hers left them on her server.
    ada.server.put(
        "work",
        "board.ics",
        &meeting(
            "board-7@example.org",
            "Board meeting",
            "20261120T100000",
            "",
        ),
    );
    ada.server.put(
        "work",
        "dinner.ics",
        &inviting(
            "dinner-3@example.org",
            "Dinner",
            "20261119T190000",
            ADDRESS,
            &["bo@example.test"],
        ),
    );
    let assistant = ada.with_her_assistant().await;
    let answering = Answering::with(Some(ConfirmationAnswer::AllowOnce));
    ada.workspaces.ask_people(answering.clone());
    // It reads what her calendar holds, as it may.
    assert_eq!(
        ada.week(&assistant).await,
        [
            "Dinner 2026-11-19T19:00:00+01:00",
            "Board meeting 2026-11-20T10:00:00+01:00",
        ]
    );
    let before = ada.server.held("work");

    let board = |start: &str| meeting("board-7@example.org", "Board meeting", start, "");
    let dinner = |start: &str, invited: &[&str]| {
        inviting("dinner-3@example.org", "Dinner", start, ADDRESS, invited)
    };
    let attempts = [
        (
            "a cancellation of the meeting she entered herself",
            carried("CANCEL", &board("20261120T100000")),
        ),
        (
            "the meeting she entered herself, at another time",
            carried("REQUEST", &board("20261120T160000")),
        ),
        (
            "a cancellation of her meeting that names a sender of its own",
            carried(
                "CANCEL",
                &inviting(
                    "board-7@example.org",
                    "Board meeting",
                    "20261120T100000",
                    "mallory@elsewhere.test",
                    &[ADDRESS],
                ),
            ),
        ),
        (
            "a cancellation of the dinner she invites to",
            carried("CANCEL", &dinner("20261119T190000", &["bo@example.test"])),
        ),
        (
            "the dinner she invites to, moved and with a stranger",
            carried(
                "REQUEST",
                &dinner(
                    "20261119T210000",
                    &["bo@example.test", "eve@elsewhere.test"],
                ),
            ),
        ),
    ];
    for (what, text) in attempts {
        let answered = ada.put(&assistant, &text).await;
        assert_eq!(
            ada.server.held("work"),
            before,
            "{what}: her assistant changed what she entered herself, and nobody asked her \
             ({answered:?})"
        );
        let why = refused(what, answered);
        assert!(why.contains("its person"), "{what}: {why}");
        assert_eq!(
            answering.count(),
            0,
            "{what}: what is hers is refused, and she is not asked to give it away"
        );
    }
    ada.nobody_was_mailed("what her assistant tried");

    // She changes what is hers by asking, and nobody is asked for her.
    let moved = ada
        .put(
            &ada.herself(),
            &carried("REQUEST", &board("20261120T160000")),
        )
        .await
        .expect("her own entry, by her own word");
    assert_eq!(moved["made"], "updated", "{moved}");
    assert_eq!(answering.count(), 0);
}

/// An invitation to her is her assistant's to put in, to bring up to date
/// and to take out again for the sender it came from, and for nobody else,
/// once she allowed each of them: with nobody to ask through, with no answer
/// and on her no, nothing goes in and nothing comes out. What her calendar
/// server is sent tells it to write to nobody: every line that names
/// somebody says the server is not the one to, and the removal says to
/// answer nobody.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invitation_to_her_goes_in_and_out_once_she_allowed_it_and_no_server_writes_of_it() {
    let ada = Ada::signing_in_as(ADDRESS).await;
    let assistant = ada.with_her_assistant().await;
    let offsite = |method: &str, start: &str| {
        carried(
            method,
            &inviting(
                "offsite-9@example.org",
                "Offsite",
                start,
                "bo@example.test",
                &[ADDRESS, "cy@example.test"],
            ),
        )
    };
    // What the server was asked while her account was added does not count.
    let _ = ada.server.requests();

    let allowing = Answering::with(Some(ConfirmationAnswer::AllowOnce));
    ada.workspaces.ask_people(allowing.clone());
    let made = ada
        .put(&assistant, &offsite("REQUEST", "20261119T140000"))
        .await
        .expect("she allowed the invitation in");
    assert_eq!(made["made"], "created", "{made}");
    let moved = ada
        .put(&assistant, &offsite("REQUEST", "20261119T160000"))
        .await
        .expect("its sender's later version, which she allowed");
    assert_eq!(moved["made"], "updated", "{moved}");
    assert_eq!(allowing.count(), 2, "each version asked her");

    // A cancellation that names another sender takes nothing out, and asks
    // her nothing: it is not hers to allow.
    let forged = carried(
        "CANCEL",
        &inviting(
            "offsite-9@example.org",
            "Offsite",
            "20261119T160000",
            "mallory@elsewhere.test",
            &[ADDRESS],
        ),
    );
    let answered = ada.put(&assistant, &forged).await;
    assert_eq!(
        ada.server.held("work").len(),
        1,
        "a cancellation from somebody else than its sender removed the entry ({answered:?})"
    );
    refused("a cancellation from somebody else", answered);
    assert_eq!(allowing.count(), 2);

    // With nobody to ask through, nothing comes out either.
    ada.workspaces.ask_people(Answering::with(None));
    let unanswered = ada
        .put(&assistant, &offsite("CANCEL", "20261119T160000"))
        .await;
    assert!(
        matches!(unanswered, Err(ActionError::ConfirmationRequired { .. })),
        "with nobody to ask through: {unanswered:?}"
    );
    assert_eq!(ada.server.held("work").len(), 1, "the entry is still there");

    ada.workspaces.ask_people(allowing.clone());
    let gone = ada
        .put(&assistant, &offsite("CANCEL", "20261119T160000"))
        .await
        .expect("its sender's cancellation, which she allowed");
    assert_eq!(gone["made"], "cancelled", "{gone}");
    assert!(ada.server.held("work").is_empty());
    assert_eq!(allowing.count(), 3);

    let requests = ada.server.requests();
    let stored: Vec<String> = requests
        .iter()
        .filter(|request| request.method == "PUT")
        .map(test_http::Request::body_text)
        .collect();
    assert_eq!(stored.len(), 2, "the invitation and its later version");
    for text in &stored {
        let naming = naming_lines(text);
        assert_eq!(naming.len(), 3, "{text}");
        for line in naming {
            assert!(
                line.contains(";SCHEDULE-AGENT=CLIENT"),
                "her calendar server is left to write to whoever this line names: {line}"
            );
        }
    }
    let removal = requests
        .iter()
        .find(|request| request.method == "DELETE")
        .expect("the removal");
    assert_eq!(
        removal.header("Schedule-Reply"),
        Some("F"),
        "a removal tells her calendar server to answer nobody"
    );
    ada.nobody_was_mailed("an invitation put in, brought up to date and taken out");
}

/// Her calendar server schedules by itself, as the ones people rent do: an
/// entry in which she invites others has it write to them, unless what it is
/// sent tells it not to. So an entry is stored telling it, whoever asked for
/// it and whatever the text says of that itself, and nobody is written to in
/// her name.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_her_calendar_server_is_sent_tells_it_to_write_to_nobody() {
    let ada = Ada::signing_in_as(ADDRESS).await;
    let her = ada.herself();
    ada.add().await;
    let dinner = |method: &str, invited: &[&str]| {
        carried(
            method,
            &inviting(
                "dinner-3@example.org",
                "Dinner",
                "20261119T190000",
                ADDRESS,
                invited,
            ),
        )
    };
    let _ = ada.server.requests();

    let made = ada
        .put(
            &her,
            &dinner("REQUEST", &["bo@example.test", "eve@elsewhere.test"]),
        )
        .await
        .expect("her own entry, by her own word");
    assert_eq!(made["made"], "created", "{made}");
    ada.nobody_was_mailed("an entry in which she invites two people");
    let on_server = ada.server.held("work");
    assert_eq!(on_server.len(), 1);
    let naming = naming_lines(&on_server[0].1);
    assert_eq!(naming.len(), 3, "{naming:?}");
    for line in naming {
        assert!(
            line.contains(";SCHEDULE-AGENT=CLIENT"),
            "her calendar server is left to write to whoever this line names: {line}"
        );
    }

    // Text that says of itself that the server is the one to write, and to
    // do it now, is not stored saying so.
    let insisting = dinner(
        "REQUEST",
        &["bo@example.test", "eve@elsewhere.test", "cy@example.test"],
    )
    .replace(
        "ATTENDEE;PARTSTAT",
        "ATTENDEE;SCHEDULE-AGENT=SERVER;SCHEDULE-FORCE-SEND=REQUEST;PARTSTAT",
    );
    let more = ada.put(&her, &insisting).await.expect("one more guest");
    assert_eq!(more["made"], "updated", "{more}");
    ada.nobody_was_mailed("text that tells the server to write");
    let text = ada.server.held("work")[0].1.replace("\r\n ", "");
    assert!(
        !text.contains("SCHEDULE-AGENT=SERVER") && !text.contains("SCHEDULE-FORCE-SEND"),
        "{text}"
    );
    assert_eq!(text.matches("SCHEDULE-AGENT=CLIENT").count(), 4, "{text}");

    // She takes it out again, and nobody is told.
    let gone = ada
        .put(
            &her,
            &dinner("CANCEL", &["bo@example.test", "eve@elsewhere.test"]),
        )
        .await
        .expect("she takes it out");
    assert_eq!(gone["made"], "cancelled", "{gone}");
    let removal = ada
        .server
        .requests()
        .into_iter()
        .find(|request| request.method == "DELETE")
        .expect("the removal");
    assert_eq!(removal.header("Schedule-Reply"), Some("F"));
    ada.nobody_was_mailed("an entry taken out");
}

/// Text that names people and is no invitation to her (she is its organiser,
/// or it invites others and not her) is put into her calendar only once she
/// was shown every address it names and allowed it. With nobody to ask
/// through, with no answer and on her no, nothing is put. What a question
/// cannot show whole is refused before she is asked. Her own entry asks her
/// nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_entry_that_names_people_waits_for_her_answer() {
    let ada = Ada::signing_in_as(ADDRESS).await;
    let assistant = ada.with_her_assistant().await;
    let dinner = carried(
        "REQUEST",
        &inviting(
            "dinner-3@example.org",
            "Dinner",
            "20261119T190000",
            ADDRESS,
            &["bo@example.test", "eve@elsewhere.test"],
        ),
    );

    let unasked = ada.put(&assistant, &dinner).await;
    assert!(
        ada.server.held("work").is_empty(),
        "her assistant put an entry in which she invites people into her calendar, and nobody \
         asked her ({unasked:?})"
    );
    assert!(
        matches!(unasked, Err(ActionError::ConfirmationRequired { .. })),
        "with nobody to ask through: {unasked:?}"
    );

    let silent = Answering::with(None);
    ada.workspaces.ask_people(silent.clone());
    let unanswered = ada.put(&assistant, &dinner).await;
    assert!(
        matches!(unanswered, Err(ActionError::ConfirmationRequired { .. })),
        "with no answer: {unanswered:?}"
    );
    assert!(ada.server.held("work").is_empty(), "nothing unallowed");
    let (person, question) = silent.questions.lock().unwrap()[0].clone();
    assert_eq!(
        person, ada.device.user_public_key,
        "her calendar, her answer"
    );
    assert_eq!(question.agent_id, "assistant");
    assert_eq!(question.action_label, "put this entry into the calendar");
    assert_eq!(question.resource_label, "Mine");
    assert_eq!(silent.rows("Calendar"), ["Work"]);
    assert_eq!(silent.rows("Title"), ["Dinner"]);
    assert_eq!(silent.rows("Names"), ["3 addresses"]);
    assert_eq!(silent.rows("Organised by"), [ADDRESS]);
    assert_eq!(
        silent.rows("Invites"),
        ["bo@example.test", "eve@elsewhere.test"]
    );

    ada.workspaces
        .ask_people(Answering::with(Some(ConfirmationAnswer::Deny)));
    let denied = ada.put(&assistant, &dinner).await;
    assert!(
        matches!(denied, Err(ActionError::Refused { .. })),
        "{denied:?}"
    );
    assert!(ada.server.held("work").is_empty(), "nothing she refused");

    let allowing = Answering::with(Some(ConfirmationAnswer::AllowOnce));
    ada.workspaces.ask_people(allowing.clone());
    let made = ada
        .put(&assistant, &dinner)
        .await
        .expect("she allowed what she was shown");
    assert_eq!(made["made"], "created", "{made}");
    assert_eq!(allowing.count(), 1, "one question for one entry");
    assert_eq!(naming_lines(&ada.server.held("work")[0].1).len(), 3);

    // People it names without inviting her: she is asked as well.
    let party = carried(
        "REQUEST",
        &inviting(
            "party-4@example.org",
            "Party",
            "20261121T190000",
            "mallory@elsewhere.test",
            &["eve@elsewhere.test"],
        ),
    );
    ada.put(&assistant, &party)
        .await
        .expect("she allowed what she was shown");
    assert_eq!(allowing.count(), 2);
    assert_eq!(allowing.rows("Organised by"), ["mallory@elsewhere.test"]);
    assert_eq!(allowing.rows("Invites"), ["eve@elsewhere.test"]);

    // More addresses than a question shows, and an address she could
    // misread: refused with nobody asked.
    let guests: Vec<String> = (0..21)
        .map(|guest| format!("guest{guest}@example.test"))
        .collect();
    let guests: Vec<&str> = guests.iter().map(String::as_str).collect();
    let crowd = carried(
        "REQUEST",
        &inviting(
            "crowd-5@example.org",
            "Crowd",
            "20261122T190000",
            ADDRESS,
            &guests,
        ),
    );
    let why = refused(
        "more addresses than a question shows",
        ada.put(&assistant, &crowd).await,
    );
    assert!(why.contains("22 addresses"), "{why}");
    let misread = carried(
        "REQUEST",
        &inviting(
            "misread-6@example.org",
            "Misread",
            "20261122T200000",
            ADDRESS,
            &["b\u{f6}@example.test"],
        ),
    );
    refused(
        "an address she could misread",
        ada.put(&assistant, &misread).await,
    );
    assert_eq!(
        allowing.count(),
        2,
        "she is asked nothing about what cannot be shown whole"
    );
    assert_eq!(ada.server.held("work").len(), 2);

    // Her own entry asks her nothing.
    let lunch = carried(
        "REQUEST",
        &inviting(
            "lunch-8@example.org",
            "Lunch",
            "20261123T120000",
            ADDRESS,
            &["bo@example.test"],
        ),
    );
    ada.put(&ada.herself(), &lunch)
        .await
        .expect("her own entry, by her own word");
    assert_eq!(allowing.count(), 2, "her own entry asks her nothing");
    ada.nobody_was_mailed("entries that name people");
}

/// What she let her assistant always do goes ahead unasked and is counted,
/// until she takes it back: then it asks again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_she_always_allows_is_put_unasked_until_she_takes_it_back() {
    let ada = Ada::signing_in_as(ADDRESS).await;
    let assistant = ada.with_her_assistant().await;
    let person = ada.device.user_public_key;
    let refusing = Answering::with(Some(ConfirmationAnswer::Deny));
    ada.workspaces.ask_people(refusing.clone());
    let dinner = |uid: &str| {
        carried(
            "REQUEST",
            &inviting(
                uid,
                "Dinner",
                "20261119T190000",
                ADDRESS,
                &["bo@example.test"],
            ),
        )
    };

    // The server lets a permission stay when it reads her own answer
    // always to a question it put; the test stands in for that moment.
    let permission = ada
        .workspaces
        .let_always(
            &person,
            Allowing {
                scope: Asked {
                    workspace: ada.workspace_id.to_string(),
                    agent: "assistant".to_owned(),
                    action: ActionName {
                        provider: person_accounts::PROVIDER.to_owned(),
                        kind: person_accounts::PROVIDER.to_owned(),
                        action: held::ADD_INVITATION.to_owned(),
                    },
                    on: Some(Thing {
                        document: String::new(),
                        resource: held::RESOURCE.to_owned(),
                    }),
                },
                name: "Assistant".to_owned(),
                words: Words {
                    action: "Add invitation to calendar".to_owned(),
                    on: "Calendars".to_owned(),
                    place: "Calendars".to_owned(),
                },
                asked_by: None,
            },
        )
        .expect("it stays");

    let made = ada
        .put(&assistant, &dinner("dinner-3@example.org"))
        .await
        .expect("what she always allows asks her nothing");
    assert_eq!(made["made"], "created", "{made}");
    assert_eq!(refusing.count(), 0, "she is not asked");
    assert_eq!(
        ada.workspaces.permissions_of(&person).permissions[0].used_times,
        1,
        "and its use is counted for her to see"
    );
    ada.nobody_was_mailed("an entry she always allows");

    assert_eq!(
        ada.workspaces
            .take_back_permission(&person, &permission.id)
            .await,
        Ok(true)
    );
    let again = ada.put(&assistant, &dinner("dinner-4@example.org")).await;
    assert!(
        matches!(again, Err(ActionError::Refused { .. })),
        "taken back, it asks again and she says no: {again:?}"
    );
    assert_eq!(refusing.count(), 1);
    assert_eq!(ada.server.held("work").len(), 1);
}

/// The catalogue offers a member's agent what it may ask of the workspace's
/// calendars, and nothing that is a person's own: no account added with a
/// typed password, none removed. The person's own page asks directly and is
/// offered no row, and nobody outside the workspace is offered one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_catalogue_offers_the_workspaces_calendars_to_its_members_agents_alone() {
    let ada = Ada::new().await;
    let her = ada.herself();
    let page = ada.page();
    ada.add().await;
    let assistant = ada.agent("assistant");
    let rows = [
        "calendars#add-invitation",
        "calendars#entries",
        "calendars#free",
        "calendars#list",
        "calendars#set-categories",
        "calendars#sync",
    ];

    let calendar_rows = |rows: Vec<String>| -> Vec<String> {
        rows.into_iter()
            .filter(|row| row.starts_with("calendars#"))
            .collect()
    };
    assert_eq!(
        calendar_rows(offered(&page, &assistant).await),
        rows,
        "an agent's page offers what it may do with the workspace's calendars"
    );
    let bo = admit(&ada.workspaces, &ada.workspace, &ada.owner, 2).await;
    let bos_page = ada.actions.in_workspace_as(ada.workspace_id, &bo);
    assert_eq!(
        calendar_rows(offered(&bos_page, &agent_caller(&bo, "assistant")).await),
        rows,
        "another member's agent reads the same calendars"
    );
    assert!(
        offered(&page, &her).await.is_empty(),
        "her own page asks directly and is offered no row"
    );
    let stranger = key_of(&SigningKey::from_bytes(&rand::random()), "laptop");
    let strangers_page = ada.actions.in_workspace_as(ada.workspace_id, &stranger);
    assert!(
        offered(&strangers_page, &agent_caller(&stranger, "assistant"))
            .await
            .is_empty(),
        "an agent of a person who is no member"
    );
    let (elsewhere, _, _) = governed(&ada.workspaces).await;
    let cy = admit(&ada.workspaces, &elsewhere, &ada.owner, 3).await;
    let cys_page = ada.actions.in_workspace_as(ada.workspace_id, &cy);
    assert!(
        offered(&cys_page, &agent_caller(&cy, "assistant"))
            .await
            .is_empty(),
        "an agent of a member of another workspace"
    );
}

/// What an automation that waits on an entry's time is handed: every
/// occurrence as the pull of her own server left it, for a member of the
/// workspace and for nobody else.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_times_the_workspaces_calendars_hold_are_read_for_its_members_alone() {
    let ada = Ada::new().await;
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
    ada.add().await;
    let person = ada.device.user_public_key;

    // Nothing of the workspace's calendars for a person who is no member.
    let stranger = SigningKey::from_bytes(&rand::random());
    assert_eq!(
        ada.occurrences(&stranger.verifying_key().to_bytes()).await,
        Ok(Vec::new())
    );
    assert!(
        ada.occurrences(&person).await.is_ok(),
        "a member reads them"
    );

    // Every occurrence at the moment it starts: 09:30 where she lives is
    // 07:30 UTC in summer time and 08:30 after it ended on the 25th; the day
    // left out is not there, the moved one is where it was moved to under
    // the date it had in its series, and the whole day begins at her
    // midnight.
    let held: Vec<(String, String, i64, bool)> = ada
        .occurrences(&person)
        .await
        .expect("a member's read")
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

/// The id her calendars answer for the entry titled `title` in the week of
/// 16 November, as `caller` reads it.
async fn entry_titled(ada: &Ada, caller: &Caller, title: &str) -> String {
    let read = ada
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
        .find(|entry| entry["title"] == title)
        .and_then(|entry| entry["entry"].as_str())
        .unwrap_or_else(|| panic!("no entry titled {title}: {read}"))
        .to_owned()
}

/// What `caller` is answered when it files `entry` under `categories`.
async fn filed(
    ada: &Ada,
    caller: &Caller,
    entry: &str,
    categories: &[&str],
) -> Result<Value, ActionError> {
    ada.ask(
        caller,
        held::SET_CATEGORIES,
        json!({"entry": entry, "categories": categories}),
    )
    .await
}

/// Filing an entry under categories rewrites it on its server. So what she
/// entered herself is her assistant's to file only once she was shown the
/// entry and the words and allowed it: with nobody to ask through, with no
/// answer and on her no, the entry stays as it was. Every change it makes
/// asks her, and her own filing asks her nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_she_entered_herself_is_filed_by_her_assistant_only_once_she_allowed_it() {
    let ada = Ada::signing_in_as(ADDRESS).await;
    // Friday's board meeting, which she entered and filed herself, as
    // another client of hers left it on her server.
    ada.server.put(
        "work",
        "board.ics",
        &meeting(
            "board-7@example.org",
            "Board meeting",
            "20261120T100000",
            "CATEGORIES:Board\r\n",
        ),
    );
    let assistant = ada.with_her_assistant().await;
    let board = entry_titled(&ada, &assistant, "Board meeting").await;
    let before = ada.server.held("work");

    let unasked = filed(&ada, &assistant, &board, &["Cancelled"]).await;
    assert_eq!(
        ada.server.held("work"),
        before,
        "her assistant filed the meeting she entered herself under words of its own, and \
         nobody asked her ({unasked:?})"
    );
    assert!(
        matches!(unasked, Err(ActionError::ConfirmationRequired { .. })),
        "with nobody to ask through: {unasked:?}"
    );

    let silent = Answering::with(None);
    ada.workspaces.ask_people(silent.clone());
    let unanswered = filed(&ada, &assistant, &board, &["Cancelled"]).await;
    assert!(
        matches!(unanswered, Err(ActionError::ConfirmationRequired { .. })),
        "with no answer: {unanswered:?}"
    );
    assert_eq!(ada.server.held("work"), before, "nothing unallowed");
    let (person, question) = silent.questions.lock().unwrap()[0].clone();
    assert_eq!(
        person, ada.device.user_public_key,
        "her calendar, her answer"
    );
    assert_eq!(question.agent_id, "assistant");
    assert_eq!(question.action_label, "file this entry under categories");
    assert_eq!(question.resource_label, "Mine");
    assert_eq!(silent.rows("Calendar"), ["Work"]);
    assert_eq!(silent.rows("Title"), ["Board meeting"]);
    assert_eq!(silent.rows("Filed under"), ["Board"]);
    assert_eq!(silent.rows("To file under"), ["Cancelled"]);

    ada.workspaces
        .ask_people(Answering::with(Some(ConfirmationAnswer::Deny)));
    let denied = filed(&ada, &assistant, &board, &["Cancelled"]).await;
    assert!(
        matches!(denied, Err(ActionError::Refused { .. })),
        "{denied:?}"
    );
    assert_eq!(ada.server.held("work"), before, "nothing she refused");

    let allowing = Answering::with(Some(ConfirmationAnswer::AllowOnce));
    ada.workspaces.ask_people(allowing.clone());
    let done = filed(&ada, &assistant, &board, &["Board", "Quarter"])
        .await
        .expect("she allowed what she was shown");
    assert_eq!(done["categories"], json!(["Board", "Quarter"]));
    assert_eq!(allowing.count(), 1, "one question for one entry");
    assert_eq!(allowing.rows("To file under"), ["Board", "Quarter"]);
    let on_server = ada.server.held("work");
    let text = &on_server[0].1;
    assert!(text.contains("CATEGORIES:Board,Quarter\r\n"), "{text}");
    assert!(text.contains("X-SERVER-NOTE:kept as it is\r\n"), "{text}");

    // What her assistant put in itself, and every other change it makes,
    // waits for her answer as well.
    let offsite = carried(
        "REQUEST",
        &inviting(
            "offsite-9@example.org",
            "Offsite",
            "20261119T140000",
            "bo@example.test",
            &[ADDRESS],
        ),
    );
    ada.put(&assistant, &offsite)
        .await
        .expect("she allowed the invitation in");
    assert_eq!(allowing.count(), 2, "the invitation asked her too");
    let its_own = entry_titled(&ada, &assistant, "Offsite").await;
    let travel = filed(&ada, &assistant, &its_own, &["Travel"])
        .await
        .expect("she allowed what it put in itself to be filed");
    assert_eq!(travel["categories"], json!(["Travel"]));
    assert_eq!(allowing.count(), 3, "each change asked her");

    // Her own filing asks her nothing.
    filed(&ada, &ada.herself(), &board, &["Board"])
        .await
        .expect("her own entry, by her own word");
    assert_eq!(allowing.count(), 3, "her own filing asks her nothing");
    ada.nobody_was_mailed("entries filed");
}

/// An entry is rewritten whole when it is filed, and a calendar server that
/// schedules by itself writes to the invited when an entry its person
/// organises changes. So her assistant files no entry whose lines leave that
/// writing to the server: it is refused with nobody asked, the entry stays as
/// it was, and nobody is written to in her name.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn filing_what_she_invites_others_to_is_not_her_assistants_and_writes_to_nobody() {
    let ada = Ada::signing_in_as(ADDRESS).await;
    // A dinner she invites Bo to, as another client of hers left it on her
    // server: its lines leave writing to Bo to the server.
    ada.server.put(
        "work",
        "dinner.ics",
        &inviting(
            "dinner-3@example.org",
            "Dinner",
            "20261119T190000",
            ADDRESS,
            &["bo@example.test"],
        ),
    );
    let assistant = ada.with_her_assistant().await;
    let answering = Answering::with(Some(ConfirmationAnswer::AllowOnce));
    ada.workspaces.ask_people(answering.clone());
    let dinner = entry_titled(&ada, &assistant, "Dinner").await;
    let before = ada.server.held("work");

    let answered = filed(&ada, &assistant, &dinner, &["Private"]).await;
    ada.nobody_was_mailed("the dinner she invites to, filed by her assistant");
    assert_eq!(
        ada.server.held("work"),
        before,
        "her assistant rewrote the dinner she invites to ({answered:?})"
    );
    let why = refused(
        "an entry whose lines leave writing to her calendar server",
        answered,
    );
    assert!(why.contains("its person"), "{why}");
    assert_eq!(
        answering.count(),
        0,
        "she is not asked to allow what would have her server write to people"
    );
}

/// An invitation that reached her calendar another way than through her
/// assistant (her calendar server filed it itself, or another client of hers
/// did) is not her assistant's to change or remove, whatever organiser the
/// text it hands over names: that text is its own writing, and naming the
/// organiser the entry has proves nothing. Only what it put in itself is its
/// own to bring up to date and take out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invitation_her_assistant_did_not_put_in_is_not_its_to_change_or_remove() {
    let ada = Ada::signing_in_as(ADDRESS).await;
    let review = |start: &str| {
        inviting(
            "review-5@example.org",
            "Review",
            start,
            "bo@example.test",
            &[ADDRESS, "cy@example.test"],
        )
    };
    // Bo's review, to which he invites her, as her calendar server filed it.
    ada.server
        .put("work", "review.ics", &review("20261118T100000"));
    let assistant = ada.with_her_assistant().await;
    let answering = Answering::with(Some(ConfirmationAnswer::AllowOnce));
    ada.workspaces.ask_people(answering.clone());
    assert_eq!(
        ada.week(&assistant).await,
        ["Review 2026-11-18T10:00:00+01:00"]
    );
    let before = ada.server.held("work");

    let attempts = [
        (
            "a cancellation that names the organiser the entry has",
            carried("CANCEL", &review("20261118T100000")),
        ),
        (
            "another time under the name of the organiser the entry has",
            carried("REQUEST", &review("20261118T170000")),
        ),
    ];
    for (what, text) in attempts {
        let answered = ada.put(&assistant, &text).await;
        assert_eq!(
            ada.server.held("work"),
            before,
            "{what}: her assistant changed an invitation it did not put in, by naming its \
             organiser, and nobody asked her ({answered:?})"
        );
        let why = refused(what, answered);
        assert!(why.contains("its person"), "{what}: {why}");
        assert_eq!(
            answering.count(),
            0,
            "{what}: what reached her calendar another way is refused, and she is not asked \
             to give it away"
        );
    }
    ada.nobody_was_mailed("what her assistant tried");

    // She changes it by asking, as she changes everything that is hers.
    let moved = ada
        .put(
            &ada.herself(),
            &carried("REQUEST", &review("20261118T170000")),
        )
        .await
        .expect("her own calendar, by her own word");
    assert_eq!(moved["made"], "updated", "{moved}");
    assert_eq!(answering.count(), 0);
}

/// Her calendar server follows the standard's table of what a change to an
/// entry makes it send (RFC 6638, 3.2.1.2): a line that goes from left to the
/// server to `SCHEDULE-AGENT=CLIENT` has it tell whoever the line names that
/// the event is off. So what she changes herself, in an entry she organises
/// that another client stored with its lines left to the server, goes up
/// with those lines as the server holds them. The server goes on writing to
/// the people it already wrote to, calls nothing off, and is started writing
/// to nobody: somebody the change newly names is the client's. When she takes
/// the entry out, the server tells the people it invited (3.2.1.3) and
/// nobody else.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn her_own_change_to_an_entry_her_server_writes_for_calls_nothing_off() {
    let ada = Ada::signing_in_as(ADDRESS).await;
    let her = ada.herself();
    ada.add().await;
    let lunch = |start: &str, invited: &[&str]| {
        inviting("lunch-4@example.org", "Lunch", start, ADDRESS, invited)
    };
    // Another client stored it long ago, and the server invited both.
    let stored = lunch(
        "20261118T120000",
        &["bo@example.test", "eve@elsewhere.test"],
    );
    ada.server.put("work", "lunch.ics", &stored);

    let moved = ada
        .put(
            &her,
            &carried(
                "REQUEST",
                &lunch(
                    "20261118T130000",
                    &["bo@example.test", "eve@elsewhere.test", "cy@example.test"],
                ),
            ),
        )
        .await
        .expect("her own entry, by her own word");
    assert_eq!(moved["made"], "updated", "{moved}");
    let mailed = ada.server.mailed();
    assert!(
        !mailed.iter().any(|sent| sent.starts_with("CANCEL")),
        "her calendar server told people that her lunch is off, which it is not: {mailed:?}"
    );
    // The table's row SERVER, column SERVER, for the two it already wrote
    // to; its row <Absent>, column CLIENT, for the one the change adds.
    assert_eq!(
        mailed,
        [
            "REQUEST to bo@example.test",
            "REQUEST to eve@elsewhere.test"
        ],
        "the server goes on writing to those it already wrote to and starts writing to nobody"
    );
    let now = naming_lines(&ada.server.held("work")[0].1);
    assert_eq!(now.len(), 4, "{now:?}");
    for line in naming_lines(&stored) {
        assert!(
            now.contains(&line),
            "a line the server writes for changed: {line} is not in {now:?}"
        );
    }
    assert!(
        now.iter().any(|line| {
            line.contains("cy@example.test") && line.contains(";SCHEDULE-AGENT=CLIENT")
        }),
        "{now:?}"
    );

    let gone = ada
        .put(
            &her,
            &carried("CANCEL", &lunch("20261118T130000", &["bo@example.test"])),
        )
        .await
        .expect("she takes it out");
    assert_eq!(gone["made"], "cancelled", "{gone}");
    assert!(ada.server.held("work").is_empty());
    assert_eq!(
        ada.server.mailed()[2..],
        ["CANCEL to bo@example.test", "CANCEL to eve@elsewhere.test"],
        "the server tells those it invited that the lunch is off, and nobody else"
    );
}
