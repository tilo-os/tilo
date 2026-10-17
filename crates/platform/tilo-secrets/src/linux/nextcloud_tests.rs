//! Exercise the typed migration reader against a private, read-only Secret Service.
use super::*;
use std::io::BufRead;
use std::sync::{Arc, Mutex};

const ITEM: &str = "/org/freedesktop/secrets/item/selected";
const SESSION: &str = "/org/freedesktop/secrets/session/selected";
const KEY: &str = "ada:https://cloud.test/:0";

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.Secret.Error")]
enum Error {
    NoSession(String),
    AccessDenied(String),
    IsLocked(String),
}
#[derive(Default)]
struct Evidence {
    sessions: usize,
    searches: usize,
    reads: usize,
    proofs: usize,
}
struct Fixture {
    scenario: String,
    evidence: Arc<Mutex<Evidence>>,
}
#[zbus::interface(name = "org.freedesktop.Secret.Service")]
impl Fixture {
    fn open_session(&self, algorithm: &str, input: Value<'_>) -> (Value<'static>, OwnedObjectPath) {
        assert_eq!(algorithm, "plain");
        drop(input);
        let mut evidence = self.evidence.lock().unwrap();
        evidence.sessions = evidence.sessions.checked_add(1).unwrap();
        drop(evidence);
        (Value::from(""), OwnedObjectPath::try_from(SESSION).unwrap())
    }
    fn search_items(
        &self,
        attributes: HashMap<String, String>,
    ) -> Result<(Vec<OwnedObjectPath>, Vec<OwnedObjectPath>), Error> {
        let searches = self.record_search(attributes);
        if self.scenario == "search-stale" && searches == 1 {
            return Err(Error::NoSession("restarted".into()));
        }
        if self.scenario == "search-refused" && searches == 1 {
            return Err(Error::AccessDenied("proof forgotten".into()));
        }
        let item = OwnedObjectPath::try_from(ITEM).unwrap();
        Ok(match self.scenario.as_str() {
            "absent" => (vec![], vec![]),
            "ambiguous" => (vec![item.clone(), item], vec![]),
            "locked" | "cancelled" => (vec![], vec![item]),
            _ => (vec![item], vec![]),
        })
    }
    fn unlock(
        &self,
        objects: Vec<OwnedObjectPath>,
    ) -> Result<(Vec<OwnedObjectPath>, OwnedObjectPath), Error> {
        drop(objects);
        // Neither lock refusal nor prompt dismissal is an absent credential.
        if self.scenario == "cancelled" {
            return Ok((vec![], OwnedObjectPath::try_from("/prompt").unwrap()));
        }
        Err(Error::IsLocked("unlock refused".into()))
    }
    fn get_secrets(
        &self,
        items: Vec<OwnedObjectPath>,
        session: OwnedObjectPath,
    ) -> Result<HashMap<OwnedObjectPath, WireSecret>, Error> {
        let (item, reads) = self.record_read(items, &session);
        match self.scenario.as_str() {
            "read-stale" if reads == 1 => {
                return Err(Error::NoSession("restarted".into()));
            }
            "always-stale" => return Err(Error::NoSession("restarted again".into())),
            "read-refused" if reads == 1 => {
                return Err(Error::AccessDenied("proof forgotten".into()));
            }
            "always-refused" | "unproved-denial" => {
                return Err(Error::AccessDenied("refused again".into()));
            }
            "read-locked" => return Err(Error::IsLocked("locked".into())),
            _ => {}
        }
        if self.scenario == "missing-secret" {
            return Ok(HashMap::new());
        }
        let value = match self.scenario.as_str() {
            "empty" => vec![],
            "oversized" => vec![b'x'; 4097],
            "invalid-utf8" => vec![255],
            _ => b"synthetic-selected-credential".to_vec(),
        };
        Ok(HashMap::from([(
            item,
            (session, vec![], value, "text/plain".into()),
        )]))
    }
}
impl Fixture {
    fn record_search(&self, attributes: HashMap<String, String>) -> usize {
        assert_eq!(
            attributes,
            HashMap::from([
                ("user".into(), KEY.into()),
                ("server".into(), "Nextcloud".into()),
                ("type".into(), "plaintext".into()),
            ])
        );
        drop(attributes);
        let mut evidence = self.evidence.lock().unwrap();
        evidence.searches = evidence.searches.checked_add(1).unwrap();
        evidence.searches
    }

    fn record_read(
        &self,
        items: Vec<OwnedObjectPath>,
        session: &OwnedObjectPath,
    ) -> (OwnedObjectPath, usize) {
        assert_eq!(items, vec![OwnedObjectPath::try_from(ITEM).unwrap()]);
        assert_eq!(session.as_str(), SESSION);
        let item = items.into_iter().next().unwrap();
        let mut evidence = self.evidence.lock().unwrap();
        evidence.reads = evidence.reads.checked_add(1).unwrap();
        (item, evidence.reads)
    }
}
struct Proof {
    evidence: Arc<Mutex<Evidence>>,
    challenge: Vec<u8>,
}
#[zbus::interface(name = "so.tilo.Secrets1")]
impl Proof {
    fn caller_challenge(&self) -> Vec<u8> {
        self.challenge.clone()
    }
    fn prove_caller(&self, program: Vec<u8>, proof: Vec<u8>) -> String {
        assert!(!program.is_empty());
        assert!(!proof.is_empty());
        drop((program, proof));
        let mut evidence = self.evidence.lock().unwrap();
        evidence.proofs = evidence.proofs.checked_add(1).unwrap();
        drop(evidence);
        "synthetic-owner".into()
    }
}
struct DismissedPrompt;
#[zbus::interface(name = "org.freedesktop.Secret.Prompt")]
impl DismissedPrompt {
    async fn prompt(
        &self,
        window_id: &str,
        #[zbus(signal_emitter)] emitter: zbus::object_server::SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let _ = window_id;
        Self::completed(&emitter, true, Value::from(""))
            .await
            .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))
    }
    #[zbus(signal)]
    async fn completed(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        dismissed: bool,
        result: Value<'_>,
    ) -> zbus::Result<()>;
}
struct Bus(std::process::Child);
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

const SCENARIO: &str = "TILO_NEXTCLOUD_READER_TEST";

fn run_scenarios() {
    for scenario in [
        "present",
        "absent",
        "ambiguous",
        "locked",
        "cancelled",
        "read-locked",
        "missing-secret",
        "empty",
        "oversized",
        "invalid-utf8",
        "search-stale",
        "read-stale",
        "always-stale",
        "search-refused",
        "read-refused",
        "always-refused",
        "unproved-denial",
    ] {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "linux::nextcloud_tests::nextcloud_read_only_session_recovery",
                "--nocapture",
            ])
            .env(SCENARIO, scenario)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{scenario}: {} {}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
fn nextcloud_read_only_session_recovery() {
    let Ok(scenario) = std::env::var(SCENARIO) else {
        run_scenarios();
        return;
    };
    let scratch = tempfile::tempdir().unwrap();
    let mut daemon = Bus(std::process::Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .arg(format!(
            "--address=unix:path={}",
            scratch.path().join("bus").display()
        ))
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("private test bus"));
    let mut address = String::new();
    std::io::BufReader::new(daemon.0.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    assert!(!address.trim().is_empty());
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("DBUS_SESSION_BUS_ADDRESS", address.trim());
    let evidence = Arc::new(Mutex::new(Evidence::default()));
    let _server = zbus::blocking::connection::Builder::address(address.trim())
        .unwrap()
        .name(SERVICE)
        .unwrap()
        .serve_at(
            SERVICE_PATH,
            Fixture {
                scenario: scenario.clone(),
                evidence: evidence.clone(),
            },
        )
        .unwrap()
        .serve_at(
            "/so/tilo/Secrets1",
            Proof {
                evidence: evidence.clone(),
                challenge: vec![7; 32],
            },
        )
        .unwrap()
        .serve_at("/prompt", DismissedPrompt)
        .unwrap()
        .build()
        .unwrap();
    let proof = scenario.contains("refused");
    if proof {
        crate::device_proof::arm(&[42; 32]);
        // This is a disposable test subprocess; never change the parent process.
        assert_eq!(unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) }, 0);
    }
    let credential =
        crate::owner_migration::NextcloudCredential::new("https://cloud.test", "ada", "0").unwrap();
    let result = credential.read_blocking();
    match scenario.as_str() {
        "present" | "search-stale" | "read-stale" | "search-refused" | "read-refused" => {
            assert_eq!(
                result.unwrap().unwrap().expose(),
                b"synthetic-selected-credential"
            );
        }
        "absent" => assert!(result.unwrap().is_none()),
        "always-stale" | "always-refused" | "unproved-denial" => {
            assert!(matches!(result, Err(SecretError::Failed(_))));
        }
        _ => assert!(matches!(result, Err(SecretError::Denied))),
    }
    assert_evidence(&evidence.lock().unwrap(), &scenario, proof);
}

fn assert_evidence(evidence: &Evidence, scenario: &str, proof: bool) {
    let retried = scenario.contains("stale") || proof;
    assert_eq!(evidence.sessions, if retried { 2 } else { 1 });
    assert_eq!(evidence.proofs, if proof { 2 } else { 0 });
    assert_eq!(evidence.searches, if retried { 2 } else { 1 });
    let expected_reads = match scenario {
        "absent" | "ambiguous" | "locked" | "cancelled" => 0,
        "read-stale" | "always-stale" | "read-refused" | "always-refused" => 2,
        _ => 1,
    };
    assert_eq!(evidence.reads, expected_reads);
}
