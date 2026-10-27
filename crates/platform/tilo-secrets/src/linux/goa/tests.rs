//! Native-only qualification in a dedicated container under an ordinary UID.
//! The fixture binds the actual owner bus path; no host keyring or routing seam.
use super::*;
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const ACCOUNT_PATH: &str = "/org/gnome/OnlineAccounts/Accounts/account_1";
const ITEM_PATH: &str = "/org/freedesktop/secrets/item/selected";
const SESSION_PATH: &str = "/org/freedesktop/secrets/session/selected";
const IDENTITY: &str = "person@example.test";
const VARIANT: &str = "{'access_token': <'synthetic-native-access'>, 'refresh_token': <'synthetic-native-refresh'>, 'access_token_expires_at': <int64 2000000000000000>}";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scenario {
    Positive,
    DisabledFiles,
    AbsentFiles,
    ForeignIdentity,
    ForeignProvider,
    ForeignId,
    AmbiguousItems,
    ForeignAttributes,
    OwnerChanged,
}
#[derive(Default)]
struct Evidence {
    searches: usize,
    reads: usize,
    sessions: usize,
    closes: usize,
    owner_changed: bool,
}
struct Bus {
    daemon: std::process::Child,
    address: String,
    socket: PathBuf,
}
impl Bus {
    fn start() -> Self {
        let uid = unsafe { libc::getuid() };
        assert!(uid != 0 && unsafe { libc::geteuid() } == uid);
        let socket = PathBuf::from(format!("/run/user/{uid}/bus"));
        assert!(
            !socket.exists(),
            "native qualification requires a fresh isolated owner bus"
        );
        let mut daemon = std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .arg(format!("--address=unix:path={}", socket.display()))
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("native fixture image must provide dbus-daemon");
        let mut address = String::new();
        std::io::BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        assert!(
            !address.trim().is_empty(),
            "private fixture bus failed to start"
        );
        Self {
            daemon,
            address: address.trim().into(),
            socket,
        }
    }
}
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        if self.socket.exists() {
            std::fs::remove_file(&self.socket).unwrap();
        }
    }
}

struct AccountInterface(Scenario);
#[zbus::interface(name = "org.gnome.OnlineAccounts.Account")]
impl AccountInterface {
    #[zbus(property)]
    fn provider_type(&self) -> &str {
        if self.0 == Scenario::ForeignProvider {
            "ms_graph"
        } else {
            "google"
        }
    }
    #[zbus(property)]
    fn id(&self) -> &str {
        if self.0 == Scenario::ForeignId {
            "other_account"
        } else {
            "account_1"
        }
    }
    #[zbus(property)]
    fn identity(&self) -> &str {
        if self.0 == Scenario::ForeignIdentity {
            "other@example.test"
        } else {
            IDENTITY
        }
    }
    #[zbus(property)]
    fn files_disabled(&self) -> bool {
        self.0 == Scenario::DisabledFiles
    }
}
struct OAuthInterface;
#[zbus::interface(name = "org.gnome.OnlineAccounts.OAuth2Based")]
impl OAuthInterface {
    #[zbus(property)]
    fn client_id(&self) -> &str {
        "synthetic-original-installed-client"
    }
    #[zbus(property)]
    fn client_secret(&self) -> &str {
        "synthetic-private-installed-secret"
    }
}
struct FilesInterface;
#[zbus::interface(name = "org.gnome.OnlineAccounts.Files")]
impl FilesInterface {
    #[zbus(property)]
    fn uri(&self) -> &str {
        "google-drive://person@example.test/"
    }
}
struct ServiceInterface {
    evidence: Arc<Mutex<Evidence>>,
    scenario: Scenario,
}
#[zbus::interface(name = "org.freedesktop.Secret.Service")]
impl ServiceInterface {
    #[zbus(out_args("unlocked", "locked"))]
    fn search_items(
        &self,
        attributes: HashMap<String, String>,
    ) -> (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) {
        assert_eq!(
            attributes,
            HashMap::from([("goa-identity".into(), "google:gen13:account_1".into())])
        );
        let mut evidence = self.evidence.lock().unwrap();
        evidence.searches = evidence.searches.saturating_add(1);
        let mut unlocked = vec![OwnedObjectPath::try_from(ITEM_PATH).unwrap()];
        if self.scenario == Scenario::AmbiguousItems {
            unlocked.push(
                OwnedObjectPath::try_from("/org/freedesktop/secrets/item/ambiguous").unwrap(),
            );
        }
        (unlocked, Vec::new())
    }
    #[zbus(out_args("output", "session"))]
    fn open_session(&self, algorithm: &str, input: OwnedValue) -> (OwnedValue, OwnedObjectPath) {
        assert_eq!(algorithm, "plain");
        assert!(String::try_from(input).unwrap().is_empty());
        let mut evidence = self.evidence.lock().unwrap();
        evidence.sessions = evidence.sessions.saturating_add(1);
        (
            OwnedValue::try_from(Value::from("")).unwrap(),
            OwnedObjectPath::try_from(SESSION_PATH).unwrap(),
        )
    }
}
struct ItemInterface {
    evidence: Arc<Mutex<Evidence>>,
    scenario: Scenario,
    replacement: zbus::Connection,
}
#[zbus::interface(name = "org.freedesktop.Secret.Item")]
impl ItemInterface {
    #[zbus(property)]
    fn attributes(&self) -> HashMap<String, String> {
        let identity = if self.scenario == Scenario::ForeignAttributes {
            "google:gen13:other_account"
        } else {
            "google:gen13:account_1"
        };
        HashMap::from([("goa-identity".into(), identity.into())])
    }
    async fn get_secret(
        &self,
        session: OwnedObjectPath,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> (WireSecret,) {
        assert_eq!(session.as_str(), SESSION_PATH);
        {
            let mut evidence = self.evidence.lock().unwrap();
            evidence.reads = evidence.reads.saturating_add(1);
        }
        if self.scenario == Scenario::OwnerChanged {
            connection.release_name(GOA).await.unwrap();
            self.replacement.request_name(GOA).await.unwrap();
            self.evidence.lock().unwrap().owner_changed = true;
        }
        ((
            session,
            Vec::new(),
            VARIANT.as_bytes().to_vec(),
            "text/plain; charset=utf-8".into(),
        ),)
    }
}
struct SessionInterface(Arc<Mutex<Evidence>>);
#[zbus::interface(name = "org.freedesktop.Secret.Session")]
impl SessionInterface {
    fn close(&self) {
        let mut evidence = self.0.lock().unwrap();
        evidence.closes = evidence.closes.saturating_add(1);
    }
}

fn serve(bus: &Bus, scenario: Scenario, evidence: &Arc<Mutex<Evidence>>) -> zbus::Connection {
    tilo_rt::app()
        .block_on(async {
            let replacement = zbus::connection::Builder::address(bus.address.as_str())
                .unwrap()
                .build()
                .await
                .unwrap();
            let builder = zbus::connection::Builder::address(bus.address.as_str())
                .unwrap()
                .name(GOA)
                .unwrap()
                .name(SERVICE)
                .unwrap()
                .serve_at(ACCOUNT_PATH, AccountInterface(scenario))
                .unwrap()
                .serve_at(ACCOUNT_PATH, OAuthInterface)
                .unwrap()
                .serve_at(
                    SERVICE_PATH,
                    ServiceInterface {
                        evidence: evidence.clone(),
                        scenario,
                    },
                )
                .unwrap()
                .serve_at(
                    ITEM_PATH,
                    ItemInterface {
                        evidence: evidence.clone(),
                        scenario,
                        replacement,
                    },
                )
                .unwrap()
                .serve_at(SESSION_PATH, SessionInterface(evidence.clone()))
                .unwrap();
            let builder = if scenario == Scenario::AbsentFiles {
                builder
            } else {
                builder.serve_at(ACCOUNT_PATH, FilesInterface).unwrap()
            };
            builder.build().await.unwrap()
        })
        .unwrap()
}

fn qualify_scenario(scenario: Scenario) {
    let bus = Bus::start();
    let evidence = Arc::new(Mutex::new(Evidence::default()));
    let server = serve(&bus, scenario, &evidence);
    let profile = tempfile::tempdir().unwrap();
    let held = std::fs::File::open(profile.path()).unwrap();
    let request = crate::owner_migration::GoaCredential::new(
        &held,
        GoaProvider::Google,
        "account_1",
        IDENTITY,
    )
    .unwrap();
    let source_path = profile.path().join("source-witness");
    std::fs::write(&source_path, b"untouched synthetic source").unwrap();
    let source_metadata = std::fs::metadata(&source_path).unwrap();
    let foreign = std::fs::File::open("/run").unwrap();
    assert!(matches!(
        crate::owner_migration::GoaCredential::new(
            &foreign,
            GoaProvider::Google,
            "account_1",
            IDENTITY
        ),
        Err(SecretError::Denied)
    ));
    {
        let facts = evidence.lock().unwrap();
        assert_eq!(facts.searches, 0);
        assert_eq!(facts.reads, 0);
    }
    let result = request.read_blocking();
    if scenario == Scenario::Positive {
        let credentials = result.unwrap();
        let registration = credentials.registration.unwrap();
        assert!(registration.client_id == "synthetic-original-installed-client");
        assert!(registration.client_secret.as_str() == "synthetic-private-installed-secret");
        assert!(credentials.secret.expose() == VARIANT.as_bytes());
        assert!(!format!("{registration:?}").contains("synthetic-private-installed-secret"));
        assert!(!format!("{:?}", credentials.secret).contains("synthetic-native-refresh"));
    } else {
        assert!(
            matches!(result, Err(SecretError::Denied)),
            "scenario {scenario:?} did not refuse"
        );
    }
    let facts = evidence.lock().unwrap();
    match scenario {
        Scenario::Positive => {
            assert_eq!(facts.searches, 1);
            assert_eq!(facts.reads, 1);
            assert_eq!(facts.sessions, 1);
            assert_eq!(facts.closes, 1);
        }
        Scenario::OwnerChanged => {
            assert_eq!(facts.reads, 1);
            assert!(facts.owner_changed);
            assert_eq!(facts.closes, 1);
        }
        Scenario::AmbiguousItems | Scenario::ForeignAttributes => {
            assert_eq!(facts.searches, 1);
            assert_eq!(facts.reads, 0);
            assert_eq!(facts.sessions, 0);
        }
        _ => {
            assert_eq!(facts.searches, 0);
            assert_eq!(facts.reads, 0);
        }
    }
    drop(facts);
    assert!(std::fs::read(&source_path).unwrap() == b"untouched synthetic source");
    let after = std::fs::metadata(&source_path).unwrap();
    use std::os::unix::fs::MetadataExt;
    assert_eq!(after.mode(), source_metadata.mode());
    assert_eq!(after.uid(), source_metadata.uid());
    assert_eq!(after.ino(), source_metadata.ino());
    assert_eq!(after.mtime(), source_metadata.mtime());
    assert_eq!(after.mtime_nsec(), source_metadata.mtime_nsec());
    drop(server);
}

/// Called only by the parent's explicit native-container qualification entry.
pub(super) fn qualify() {
    for scenario in [
        Scenario::Positive,
        Scenario::DisabledFiles,
        Scenario::AbsentFiles,
        Scenario::ForeignIdentity,
        Scenario::ForeignProvider,
        Scenario::ForeignId,
        Scenario::AmbiguousItems,
        Scenario::ForeignAttributes,
        Scenario::OwnerChanged,
    ] {
        qualify_scenario(scenario);
    }
}
