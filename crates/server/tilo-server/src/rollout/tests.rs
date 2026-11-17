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
}

impl Fake {
    fn new(name: &'static str, fleet: &Arc<Mutex<Fleet>>) -> Self {
        Self {
            name,
            fleet: Arc::clone(fleet),
            pending: Mutex::new(None),
            short: Vec::new(),
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
        Ok(())
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
            "attic hands off leases",
            "attic suspends machines"
        ],
        "the copies are checked under the lease, and the work handed off before the restart"
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
