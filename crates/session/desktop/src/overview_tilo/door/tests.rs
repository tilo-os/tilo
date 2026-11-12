//! The door's read becomes the overview's Agents rows, and a row's Revoke
//! goes to the door the page serves.

use std::cell::RefCell;
use std::sync::Arc;

use serde_json::Value;
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
};
use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};

use super::{DoorRead, agent_door};
use crate::overview_panel::OverviewPick;
use crate::overview_tilo::{TiloDesk, TiloItem, TiloRow, rows};

fn en() {
    ui::i18n::I18nManager::init("en-US");
}

/// The agent door of a host: what it let in, read live, and the grants the
/// person took back there.
#[derive(Default)]
struct DoorHost {
    grants: Vec<agent_door::Grant>,
    /// The state the door's read last answered.
    read: RefCell<Option<InvocationState>>,
    /// The grants a revoke named, in the order they were asked.
    revoked: RefCell<Vec<String>>,
}

impl DoorHost {
    fn with(grants: Vec<agent_door::Grant>) -> Self {
        Self {
            grants,
            ..Self::default()
        }
    }
}

impl ActionHost for DoorHost {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        Pending::ready(Ok(CataloguePage {
            exhausted: true,
            ..CataloguePage::default()
        }))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        if request.target == agent_door::target(agent_door::REVOKE) {
            let InvokeArgument::Json(argument) = &request.argument else {
                return Pending::ready(Err(ActionHostError::InvalidRequest {
                    reason: "a revoke names its grant".into(),
                }));
            };
            let grant: agent_door::GrantRef =
                serde_json::from_value(Value::Object(argument.clone())).unwrap_or_default();
            self.revoked.borrow_mut().push(grant.grant);
            return Pending::ready(Ok(InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Completed {
                    output: ActionOutput::default(),
                },
            }));
        }
        // The door's read: what a live door holds now.
        let door = agent_door::Door {
            endpoint: "https://mcp.tilo.sh".into(),
            grants: self.grants.clone(),
            ..agent_door::Door::default()
        };
        let state = InvocationState::new(
            request.invocation,
            InvocationUpdate::Item {
                invocation: request.invocation,
                sequence: 1,
                value: serde_json::to_value(&door).unwrap_or_default(),
            },
        );
        *self.read.borrow_mut() = Some(state.clone());
        Pending::ready(Ok(state.receipt()))
    }

    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.read
            .borrow()
            .clone()
            .filter(|state| state.invocation() == *id)
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionHostError>> {
        Pending::ready(Ok(()))
    }
}

fn caller() -> Caller {
    Caller::local_person("ada".to_owned()).in_workspace("ws-build".to_owned())
}

/// An agent the door let into every workspace, working now.
fn working() -> agent_door::Grant {
    agent_door::Grant {
        id: "g1".into(),
        agent: "Claude Code".into(),
        reach: agent_door::Reach::Account,
        granted_ms: 1,
        runs: vec![agent_door::Run {
            name: "Claude Code #2".into(),
            started_ms: 2,
        }],
        ..agent_door::Grant::default()
    }
}

fn items(rows: &[TiloRow]) -> Vec<&TiloItem> {
    rows.iter()
        .filter_map(|row| match row {
            TiloRow::Item(item) => Some(item),
            TiloRow::Caption(_) => None,
        })
        .collect()
}

fn facts(item: &TiloItem) -> Vec<&str> {
    item.facts.iter().map(|fact| fact.text.as_str()).collect()
}

/// The desk the page fills from the read: nothing else of the person's Tilo
/// is known from a door.
fn desk(read: &DoorRead) -> TiloDesk {
    TiloDesk {
        door: read.door().cloned(),
        ..TiloDesk::default()
    }
}

#[test]
fn the_doors_answer_becomes_the_overviews_agents_rows() {
    en();
    let host = DoorHost::with(vec![working()]);
    let mut read = DoorRead::default();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
    assert!(read.follow(&host, &caller(), &wake), "the first answer");
    assert!(
        !read.follow(&host, &caller(), &wake),
        "the same answer again"
    );

    let rows = rows(&desk(&read));
    let agent = items(&rows)[0];
    assert_eq!(agent.title, "Claude Code", "whose grant it is");
    assert_eq!(
        facts(agent),
        ["All your workspaces", "Working now"],
        "what it may reach, and that it works"
    );
    assert_eq!(
        agent.action,
        Some(("Revoke".to_owned(), OverviewPick::RevokeGrant("g1".into()))),
        "the row is the person's say over it"
    );
}

#[test]
fn an_agent_at_rest_says_where_it_reaches_and_nothing_of_work() {
    en();
    let at_rest = agent_door::Grant {
        runs: Vec::new(),
        reach: agent_door::Reach::Workspace {
            id: "ws-build".into(),
            name: "Build".into(),
        },
        ..working()
    };
    let host = DoorHost::with(vec![at_rest]);
    let mut read = DoorRead::default();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
    assert!(read.follow(&host, &caller(), &wake));
    let rows = rows(&desk(&read));
    assert_eq!(facts(items(&rows)[0]), ["Build"]);
}

/// A row's Revoke is the grant's, asked of the door the page serves: the
/// person's root signs the device's revocation from there, as the card's
/// Revoke does.
#[test]
fn a_rows_revoke_reaches_the_door() {
    en();
    let host = DoorHost::with(vec![working()]);
    let mut read = DoorRead::default();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
    assert!(read.follow(&host, &caller(), &wake));
    let action = items(&rows(&desk(&read)))[0].action.clone();
    let Some((label, OverviewPick::RevokeGrant(grant))) = action else {
        panic!("the agents' row offers to take its grant back");
    };
    assert_eq!(label, "Revoke");
    DoorRead::revoke(&host, &caller(), &grant);
    assert_eq!(*host.revoked.borrow(), [grant], "the door took it back");
}

#[test]
fn a_door_with_no_grants_leaves_the_overview_without_agents() {
    en();
    let host = DoorHost::default();
    let mut read = DoorRead::default();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
    assert!(read.follow(&host, &caller(), &wake), "it answered, empty");
    assert_eq!(read.door().map(|door| door.grants.len()), Some(0));
    let rows = rows(&desk(&read));
    assert!(
        !rows
            .iter()
            .any(|row| matches!(row, TiloRow::Caption(caption) if caption == "Agents")),
        "no agents, no section"
    );
}
