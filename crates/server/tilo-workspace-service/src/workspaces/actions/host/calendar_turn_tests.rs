//! A person's calendars and a turn this server carries out for them: the
//! turn of the agent they named for the account reads what the calendars
//! hold with no device behind it; every other turn reaches nothing and is
//! offered nothing; a narrowed agent reads where a rule of its names the
//! read; and no turn changes a calendar, so their calendar server is asked
//! to read and nothing else, and writes to nobody.

use tilo_action_policy::{ActionKind, Answer, KindSet, Level, Rule, TreePath};

use super::super::super::tests::{admit, governed};
use super::super::turn_use_tests::{not_her_agents_turns, on_turn};
use super::*;
use crate::workspaces::hosted_use::HostedTurn;

const SCOUT: &str = "scout";

/// The week her review is in.
fn week() -> Value {
    json!({"from": "2026-11-16", "to": "2026-11-23", "zone": "Europe/Berlin"})
}

/// Ada in a workspace that admits her on a device, and her calendar server,
/// which holds one meeting Bo invited her to.
struct Hers {
    server: fake_caldav::Server,
    workspaces: Workspaces,
    workspace: String,
    workspace_id: WorkspaceId,
    service: HolderActions,
    owner: DeviceCert,
    ada: DeviceCert,
    _holder: TestHolder,
    _dir: tempfile::TempDir,
}

impl Hers {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let holder = test_holder(dir.path(), &key, &owner).await;
        let workspaces = holder.workspaces().clone();
        // Her calendar server is this machine: the operator lets the server
        // call its own network, which it is otherwise kept from.
        workspaces.call_private_model_addresses(true);
        let (workspace, workspace_id, service) = governed(&workspaces).await;
        let ada = admit(&workspaces, &workspace, &owner, 1).await;
        let server = fake_caldav::Server::start(ADDRESS, PASSWORD);
        server.add_calendar("work", "Work");
        server.put(
            "work",
            "review.ics",
            &inviting(
                "review-1@example.test",
                "Review",
                "20261118T140000",
                "bo@elsewhere.test",
                &[ADDRESS],
            ),
        );
        Self {
            server,
            workspaces,
            workspace,
            workspace_id,
            service,
            owner,
            ada,
            _holder: holder,
            _dir: dir,
        }
    }

    fn target(resource: &str, action: &str) -> RemoteActionTarget {
        RemoteActionTarget {
            provider: person_accounts::PROVIDER.to_owned(),
            address: ActionAddress::new(resource, action),
        }
    }

    /// What Ada is answered on her own device.
    async fn herself(&self, target: RemoteActionTarget, argument: Value) -> Value {
        let page = self.service.in_workspace_as(self.workspace_id, &self.ada);
        asked(&page, &caller(&self.ada), target, argument)
            .await
            .expect("her own asking, on her own device")
    }

    /// She adds her account with the password she types: its id.
    async fn adds(&self) -> String {
        let adding =
            json!({"serverUrl": self.server.url(), "username": ADDRESS, "password": PASSWORD});
        let added = self
            .herself(Self::target(held::RESOURCE, held::ADD), adding)
            .await;
        added["id"].as_str().expect("its id").to_owned()
    }

    /// She says whether `agent` may use the calendar account `account`.
    async fn names(&self, account: &str, agent: &str, may: bool) {
        let naming = Self::target(person_accounts::RESOURCE, person_accounts::ASSISTANT);
        self.herself(
            naming,
            json!({"account": account, "agent": agent, "may": may}),
        )
        .await;
    }

    /// Ada, as the keeper an agent's entry names.
    fn keeper(&self) -> String {
        hex::encode(self.ada.user_public_key)
    }

    /// The turn `caller` acts on in her workspace, for an agent `keeper`
    /// keeps and `bound` narrows.
    const fn turn<'a>(
        &self,
        caller: &'a Caller,
        keeper: &'a str,
        bound: Option<&'a [Rule]>,
    ) -> HostedTurn<'a> {
        HostedTurn {
            workspace: self.workspace_id,
            caller,
            keeper,
            bound,
        }
    }

    /// What `turn` is answered when it asks `action` of her calendars.
    async fn asks(
        &self,
        turn: &HostedTurn<'_>,
        action: &str,
        argument: Value,
    ) -> Result<Value, ActionError> {
        let Value::Object(argument) = argument else {
            panic!("arguments are an object");
        };
        let request = InvokeRequest {
            invocation: InvocationId::new(),
            target: Self::target(held::RESOURCE, action),
            argument: InvokeArgument::Json(argument),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        };
        self.workspaces.calendars_for_turn(turn, &request).await
    }

    /// The calendar actions `turn` is offered, by their ids.
    async fn offered(&self, turn: &HostedTurn<'_>) -> Vec<String> {
        let rows = self.workspaces.calendars_offered(turn).await;
        rows.into_iter()
            .filter_map(|row| row.action.map(|action| action.address.action))
            .collect()
    }

    /// How often her calendar server was asked since this was last read:
    /// each time to read, and it wrote to nobody.
    fn asked_only_to_read(&self, what: &str) -> usize {
        let methods: Vec<String> = self
            .server
            .requests()
            .into_iter()
            .map(|request| request.method)
            .collect();
        assert!(
            methods
                .iter()
                .all(|method| matches!(method.as_str(), "PROPFIND" | "REPORT" | "GET")),
            "{what}: her calendar server was asked for more than a read: {methods:?}"
        );
        assert_eq!(
            self.server.mailed(),
            Vec::<String>::new(),
            "{what}: her calendar server wrote to people in her name"
        );
        methods.len()
    }
}

/// Nothing is granted by default. Once she named it, the turn of her agent
/// is offered the reads, lists her account without its sign-in, reads what
/// her calendars hold and when she is free. What changes a calendar or an
/// account is not there for a turn, and no other turn reaches anything.
/// When she takes her word back it reaches nothing again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_turn_of_the_agent_she_named_reads_her_calendars_and_changes_nothing() {
    let hers = Hers::new().await;
    let (scout, keeper) = (on_turn(&hers.ada, &hers.workspace, SCOUT), hers.keeper());
    let turn = hers.turn(&scout, &keeper, None);

    let account = hers.adds().await;
    assert!(hers.offered(&turn).await.is_empty(), "not named yet");
    nobody(
        "not named yet",
        hers.asks(&turn, held::ENTRIES, week()).await,
    );

    hers.names(&account, SCOUT, true).await;
    assert_eq!(
        hers.offered(&turn).await,
        [held::LIST, held::ENTRIES, held::FREE]
    );
    let listed = hers
        .asks(&turn, held::LIST, json!({}))
        .await
        .expect("its turn lists the account she named it for");
    assert_eq!(listed["accounts"][0]["id"], account.as_str(), "{listed}");
    assert!(
        !listed.to_string().contains(PASSWORD),
        "never the sign-in: {listed}"
    );
    let read = hers
        .asks(&turn, held::ENTRIES, week())
        .await
        .expect("its turn reads what her calendars hold");
    let entries = read["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 1, "{read}");
    assert_eq!(entries[0]["title"], "Review", "{read}");
    let free = hers
        .asks(&turn, held::FREE, week())
        .await
        .expect("and when she is free");
    assert!(free["free"].is_array(), "{free}");

    // What changes a calendar or an account, and asking its server now, are
    // not there for a turn, whatever it hands over.
    let entry = entries[0]["entry"].as_str().expect("its id").to_owned();
    let lunch = carried(
        "REQUEST",
        &inviting(
            "lunch-1@example.test",
            "Lunch",
            "20261119T120000",
            "bo@elsewhere.test",
            &[ADDRESS],
        ),
    );
    let adding = json!({"serverUrl": hers.server.url(), "username": ADDRESS, "password": PASSWORD});
    for (action, argument) in [
        (held::ADD_INVITATION, json!({"ics": lunch})),
        (
            held::SET_CATEGORIES,
            json!({"entry": entry, "categories": ["Board"]}),
        ),
        (held::SYNC, json!({})),
        (held::REMOVE, json!({"account": account})),
        (held::ADD, adding),
    ] {
        nobody(action, hers.asks(&turn, action, argument).await);
    }
    assert_eq!(
        hers.server.held("work").len(),
        1,
        "her calendar holds what it held"
    );
    assert!(hers.asked_only_to_read("the turn of the agent she named") > 0);

    // Every caller that is not a turn of her own agent, an agent of hers
    // she did not name, and another member's agent under the same id.
    let bo = admit(&hers.workspaces, &hers.workspace, &hers.owner, 2).await;
    let mut others = not_her_agents_turns(&hers.ada, &bo, &hers.workspace);
    others.push((
        "another member's own agent under the id she named",
        on_turn(&bo, &hers.workspace, SCOUT),
        hex::encode(bo.user_public_key),
    ));
    others.push((
        "an agent of hers she did not name",
        on_turn(&hers.ada, &hers.workspace, "other"),
        keeper.clone(),
    ));
    for (who, asks, kept_by) in others {
        let turn = hers.turn(&asks, &kept_by, None);
        nobody(who, hers.asks(&turn, held::ENTRIES, week()).await);
        assert!(hers.offered(&turn).await.is_empty(), "{who}");
    }
    assert_eq!(
        hers.asked_only_to_read("turns that are not her agent's"),
        0,
        "no calendar server was asked for them"
    );

    hers.names(&account, SCOUT, false).await;
    nobody(
        "after she took her word back",
        hers.asks(&turn, held::ENTRIES, week()).await,
    );
    assert!(hers.offered(&turn).await.is_empty());
}

/// An agent its entry narrows reads her calendars only where a rule of its
/// bound names exactly that read: no rule, a rule for the whole provider, a
/// rule for another action and a rule that denies it all refuse, and it is
/// offered only what a rule names. A rule that names a change makes it no
/// action of a turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_narrowed_agent_reads_her_calendars_only_where_a_rule_names_the_read() {
    let hers = Hers::new().await;
    let account = hers.adds().await;
    hers.names(&account, "sub-agent", true).await;
    let (sub, keeper) = (
        on_turn(&hers.ada, &hers.workspace, "sub-agent"),
        hers.keeper(),
    );
    let outside = KindSet::of(ActionKind::Outside);
    let named = |action: &str, answer: Answer| {
        tilo_automation::action_rule(
            person_accounts::PROVIDER,
            held::RESOURCE,
            None,
            action,
            outside,
            answer,
        )
    };
    let whole_provider = Rule::new(
        TreePath::root()
            .any(Level::Workspace)
            .app(person_accounts::PROVIDER),
        outside,
        Answer::Allow,
    );
    for (why, bound) in [
        ("no rule at all", Vec::new()),
        (
            "a rule for the whole provider",
            vec![whole_provider.clone()],
        ),
        (
            "a rule for another action",
            vec![named(held::LIST, Answer::Allow)],
        ),
        (
            "a rule that denies it",
            vec![whole_provider, named(held::ENTRIES, Answer::Deny)],
        ),
    ] {
        let turn = hers.turn(&sub, &keeper, Some(&bound));
        let reads = hers.asks(&turn, held::ENTRIES, week()).await;
        assert!(
            matches!(reads, Err(ActionError::NotPermitted)),
            "{why}: {reads:?}"
        );
        let offered = hers.offered(&turn).await;
        assert!(
            !offered.iter().any(|action| action == held::ENTRIES),
            "{why}: it is not offered what it would be refused: {offered:?}"
        );
    }

    let bound = vec![named(held::ENTRIES, Answer::Allow)];
    let turn = hers.turn(&sub, &keeper, Some(&bound));
    assert_eq!(hers.offered(&turn).await, [held::ENTRIES]);
    hers.asks(&turn, held::ENTRIES, week())
        .await
        .expect("the read its rule names");

    let bound = vec![named(held::SET_CATEGORIES, Answer::Allow)];
    let turn = hers.turn(&sub, &keeper, Some(&bound));
    nobody(
        "a change its rule names",
        hers.asks(
            &turn,
            held::SET_CATEGORIES,
            json!({"entry": "none", "categories": []}),
        )
        .await,
    );
    assert!(hers.offered(&turn).await.is_empty());
    hers.asked_only_to_read("a narrowed agent");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_reads_an_entrys_notes_whole_when_short_and_by_their_beginning_when_long() {
    let hers = Hers::new().await;
    // Each entry has notes, and a reminder that has words of its own.
    let noted = |uid: &str, title: &str, start: &str, notes: &str| {
        inviting(uid, title, start, "bo@elsewhere.test", &[ADDRESS]).replace(
            "END:VEVENT",
            &format!(
                "DESCRIPTION:{notes}\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\n\
                 DESCRIPTION:It is soon\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\nEND:VEVENT"
            ),
        )
    };
    hers.server.put(
        "work",
        "review.ics",
        &noted(
            "review-1@example.test",
            "Review",
            "20261118T140000",
            "Bring the numbers\\, all of them.",
        ),
    );
    // A text somebody pasted into an entry: longer than a read answers.
    let pasted = "word ".repeat(600);
    hers.server.put(
        "work",
        "paper.ics",
        &noted(
            "paper-1@example.test",
            "Paper",
            "20261119T140000",
            pasted.trim_end(),
        ),
    );
    let (scout, keeper) = (on_turn(&hers.ada, &hers.workspace, SCOUT), hers.keeper());
    let turn = hers.turn(&scout, &keeper, None);
    let account = hers.adds().await;
    hers.names(&account, SCOUT, true).await;

    let read = hers
        .asks(&turn, held::ENTRIES, week())
        .await
        .expect("its turn reads what her calendars hold");
    let notes_of = |title: &str| {
        read["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .find(|entry| entry["title"] == title)
            .and_then(|entry| entry["notes"].as_str())
            .unwrap_or_else(|| panic!("no notes of {title}: {read}"))
            .to_owned()
    };
    assert_eq!(
        notes_of("Review"),
        "Bring the numbers, all of them.",
        "the entry's own notes as text, and not its reminder's words"
    );
    let cut = notes_of("Paper");
    assert_eq!(
        cut.chars().count(),
        2_001,
        "the beginning of a long text, and a mark that more follows"
    );
    assert!(cut.starts_with("word word ") && cut.ends_with('\u{2026}'));
    assert_eq!(
        hers.server.mailed(),
        Vec::<String>::new(),
        "her calendar server wrote to nobody"
    );
}

/// A calendar connected by her telling her assistant where it is.
#[path = "calendar_connect_tests.rs"]
mod connect;
