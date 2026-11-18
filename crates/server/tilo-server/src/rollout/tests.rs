//! The rollout's order, on stand-in servers that share one update lease, with
//! the restart and the health verdict played by the test.

use std::sync::{Arc, Mutex};

use super::*;

/// The admin workspace's update lease as every one of the person's servers
/// reads it, and what each server did, in order.
#[derive(Default)]
struct Fleet {
    holder: Option<&'static str>,
    log: Vec<String>,
}

struct Fake {
    name: &'static str,
    fleet: Arc<Mutex<Fleet>>,
    pending: Mutex<Option<String>>,
    short: Vec<String>,
    /// What suspending its machines answers.
    suspend: Result<(), String>,
}

impl Fake {
    fn new(name: &'static str, fleet: &Arc<Mutex<Fleet>>) -> Self {
        Self {
            name,
            fleet: Arc::clone(fleet),
            pending: Mutex::new(None),
            short: Vec::new(),
            suspend: Ok(()),
        }
    }

    fn did(&self, what: &str) {
        self.fleet
            .lock()
            .unwrap()
            .log
            .push(format!("{} {what}", self.name));
    }
}

impl Machine for Fake {
    async fn hold_update_lease(&self) -> Result<bool, String> {
        let holder = &mut self.fleet.lock().unwrap().holder;
        if holder.is_some_and(|holder| holder != self.name) {
            return Ok(false);
        }
        *holder = Some(self.name);
        Ok(true)
    }

    async fn release_update_lease(&self) -> Result<(), String> {
        {
            let holder = &mut self.fleet.lock().unwrap().holder;
            if *holder == Some(self.name) {
                *holder = None;
            }
        }
        self.did("releases");
        Ok(())
    }

    async fn short_of_copies(&self) -> Result<Vec<String>, String> {
        self.did("checks copies");
        Ok(self.short.clone())
    }

    async fn hand_off_leases(&self) -> Result<(), String> {
        self.did("hands off leases");
        Ok(())
    }

    async fn suspend_machines(&self) -> Result<(), String> {
        self.did("suspends machines");
        self.suspend.clone()
    }

    fn pending(&self) -> Option<String> {
        self.pending.lock().unwrap().clone()
    }

    fn set_pending(&self, target: Option<&str>) -> Result<(), String> {
        *self.pending.lock().unwrap() = target.map(str::to_owned);
        Ok(())
    }
}

fn waits(answer: &Answer) -> bool {
    matches!(answer, Answer::Wait { .. })
}

/// Two servers with the same update staged: the first restarts only after
/// it drained, the second waits until the first came back healthy, and then
/// takes its turn.
#[tokio::test]
async fn servers_restart_one_at_a_time_and_drain_first() {
    let fleet = Arc::new(Mutex::new(Fleet::default()));
    let attic = Fake::new("attic", &fleet);
    let cellar = Fake::new("cellar", &fleet);

    assert_eq!(prepare(&attic, "v2").await.unwrap(), Answer::Restart);
    assert_eq!(
        fleet.lock().unwrap().log,
        [
            "attic checks copies",
            "attic suspends machines",
            "attic hands off leases"
        ],
        "the copies are checked under the lease, the machines suspended before their leases \
         go, and the work handed off before the restart"
    );
    let cellar_first = prepare(&cellar, "v2").await.unwrap();
    assert!(waits(&cellar_first), "{cellar_first:?}");
    assert_eq!(cellar.pending(), None, "the waiting server drained nothing");

    // The attic restarts; the cellar asks again while it is down and is
    // still told to wait.
    assert!(waits(&prepare(&cellar, "v2").await.unwrap()));
    renew(&attic).await.unwrap();
    assert_eq!(booted(&attic, "v2").await.unwrap(), Answer::Released);
    assert_eq!(attic.pending(), None);

    assert_eq!(prepare(&cellar, "v2").await.unwrap(), Answer::Restart);
    assert_eq!(booted(&cellar, "v2").await.unwrap(), Answer::Released);
    assert_eq!(fleet.lock().unwrap().holder, None);
}

/// A generation that fails its health verdict is taken back by the boot
/// loader; the server comes up on the old one holding the lease, so no other
/// server starts the update, and it does not restart into it again.
#[tokio::test]
async fn a_failing_update_halts_the_rollout() {
    let fleet = Arc::new(Mutex::new(Fleet::default()));
    let attic = Fake::new("attic", &fleet);
    let cellar = Fake::new("cellar", &fleet);

    assert_eq!(prepare(&attic, "v2").await.unwrap(), Answer::Restart);
    // Rolled back: v1 came up and is healthy.
    assert_eq!(
        booted(&attic, "v1").await.unwrap(),
        Answer::Halted {
            target: "v2".into(),
            booted: "v1".into()
        }
    );
    assert_eq!(fleet.lock().unwrap().holder, Some("attic"));
    assert!(waits(&prepare(&cellar, "v2").await.unwrap()));
    let again = prepare(&attic, "v2").await.unwrap();
    assert!(
        waits(&again),
        "the update that failed is not booted again: {again:?}"
    );

    // A fixed update resumes the rollout from the server that holds it.
    assert_eq!(prepare(&attic, "v3").await.unwrap(), Answer::Restart);
    assert_eq!(booted(&attic, "v3").await.unwrap(), Answer::Released);
    assert_eq!(prepare(&cellar, "v3").await.unwrap(), Answer::Restart);
}

/// A server whose workspaces would fall short of their copies does not
/// restart and lets the lease go, so another server can update meanwhile.
#[tokio::test]
async fn a_server_short_of_copies_waits_and_lets_the_lease_go() {
    let fleet = Arc::new(Mutex::new(Fleet::default()));
    let mut attic = Fake::new("attic", &fleet);
    attic.short = vec!["photos".into()];
    let cellar = Fake::new("cellar", &fleet);

    let answer = prepare(&attic, "v2").await.unwrap();
    assert!(
        matches!(&answer, Answer::Wait { why } if why.contains("photos")),
        "{answer:?}"
    );
    assert_eq!(attic.pending(), None);
    assert!(
        !fleet
            .lock()
            .unwrap()
            .log
            .iter()
            .any(|step| step.contains("hands off")),
        "nothing was drained"
    );
    assert_eq!(prepare(&cellar, "v2").await.unwrap(), Answer::Restart);
}

/// A server whose machines do not all suspend does not restart: it hands
/// off nothing, keeps no pending update, and lets the lease go.
#[tokio::test]
async fn a_server_whose_machines_stay_awake_waits_and_hands_off_nothing() {
    let fleet = Arc::new(Mutex::new(Fleet::default()));
    let mut attic = Fake::new("attic", &fleet);
    attic.suspend = Err("ws/default (Running)".into());
    let cellar = Fake::new("cellar", &fleet);

    let answer = prepare(&attic, "v2").await.unwrap();
    assert!(
        matches!(&answer, Answer::Wait { why } if why.contains("ws/default")),
        "{answer:?}"
    );
    assert_eq!(attic.pending(), None);
    assert_eq!(
        fleet.lock().unwrap().log,
        [
            "attic checks copies",
            "attic suspends machines",
            "attic releases"
        ],
        "no lease is handed off while a machine is still awake here"
    );
    assert_eq!(prepare(&cellar, "v2").await.unwrap(), Answer::Restart);
}

/// A boot with no update pending changes nothing.
#[tokio::test]
async fn a_boot_without_an_update_changes_nothing() {
    let fleet = Arc::new(Mutex::new(Fleet::default()));
    let attic = Fake::new("attic", &fleet);
    assert_eq!(booted(&attic, "v1").await.unwrap(), Answer::Nothing);
    assert_eq!(fleet.lock().unwrap().holder, None);
}

/// The update service's requests and the daemon's answers are one JSON line
/// each, as `tilo-server-update-apply` reads them.
#[test]
fn requests_and_answers_are_one_json_line() {
    let request = serde_json::to_string(&Request::Prepare {
        target: "sha256:ab".into(),
    })
    .unwrap();
    assert_eq!(request, r#"{"ask":"prepare","target":"sha256:ab"}"#);
    assert_eq!(
        serde_json::to_string(&Answer::Restart).unwrap(),
        r#"{"answer":"restart"}"#
    );
}

/// A workspace keeps a server from restarting while one of its copies is
/// still catching up, or while the server is one of too few current copies
/// of a kind for the workspace's policy to hold without it.
#[test]
fn a_workspace_short_without_this_server_holds_its_restart() {
    use tilo_peer_state::{CopyPolicy, CopyRole, CopyStatus, HolderCopy, Missing, Shortfall};
    let holder = |device: u8, role| HolderCopy {
        device: [device; 32],
        role,
        current: true,
    };
    let status = |key_holders, holders: Vec<HolderCopy>, shortfall| CopyStatus {
        policy: CopyPolicy {
            key_holders,
            stores: 1,
        },
        chosen: key_holders > 0,
        holders,
        shortfall,
        base_maker: None,
    };
    let attic = [1; 32];
    let both = vec![
        holder(1, CopyRole::KeyHolder),
        holder(2, CopyRole::KeyHolder),
        holder(3, CopyRole::Store),
    ];
    assert_eq!(
        short_without(&status(1, both.clone(), Vec::new()), &attic),
        None
    );
    let why = short_without(&status(2, both.clone(), Vec::new()), &attic).unwrap();
    assert!(
        why.contains("wants 2 current key holders and keeps 1"),
        "{why}"
    );
    let catching_up = vec![Shortfall::Holder {
        device: [2; 32],
        missing: Missing::History { from: attic },
    }];
    let why = short_without(&status(1, both, catching_up), &attic).unwrap();
    assert!(why.contains("catching up"), "{why}");
    // The default asks nothing of a restart: the server alone comes back.
    let alone = vec![holder(1, CopyRole::KeyHolder)];
    assert_eq!(short_without(&status(0, alone, Vec::new()), &attic), None);
    // A kind already short, which this server is none of, waits for no one.
    let no_store = vec![
        holder(1, CopyRole::KeyHolder),
        holder(2, CopyRole::KeyHolder),
    ];
    assert_eq!(
        short_without(&status(1, no_store, Vec::new()), &attic),
        None
    );
}
