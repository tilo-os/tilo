//! Explicit private-container test: actual production Slack owner-bus reader.
use super::*;
use std::collections::HashMap;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
const SERVICE: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
type WireSecret = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use tilo_test_env::PrivateBus;

const ITEM: &str = "/org/freedesktop/secrets/item/selected";
const SESSION: &str = "/org/freedesktop/secrets/session/selected";
const PASSWORD: &str = "synthetic-owner-password";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Case {
    Present,
    Missing,
    Ambiguous,
    ForeignAttributes,
    OwnerChanged,
    WrongSession,
    LockedDenied,
}
#[derive(Default)]
struct Evidence {
    searches: usize,
    reads: usize,
    opens: usize,
    closes: usize,
}
/// The owner bus path itself, which the fixture binds.
fn start_owner_bus() -> PrivateBus {
    let owner = unsafe { libc::getuid() };
    assert_ne!(owner, 0);
    assert_eq!(owner, unsafe { libc::geteuid() });
    let socket = PathBuf::from(format!("/run/user/{owner}/bus"));
    assert!(!socket.exists(), "requires a fresh isolated owner bus");
    PrivateBus::start_at(&socket)
}
struct SecretService {
    case: Case,
    evidence: Arc<Mutex<Evidence>>,
}
#[zbus::interface(name = "org.freedesktop.Secret.Service")]
impl SecretService {
    #[zbus(out_args("unlocked", "locked"))]
    fn search_items(
        &self,
        attributes: HashMap<String, String>,
    ) -> (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) {
        assert_eq!(
            attributes,
            HashMap::from([("application".into(), "Slack".into())])
        );
        self.evidence.lock().unwrap().searches += 1;
        let item = OwnedObjectPath::try_from(ITEM).unwrap();
        match self.case {
            Case::Missing => (vec![], vec![]),
            Case::Ambiguous => (
                vec![
                    item,
                    OwnedObjectPath::try_from("/org/freedesktop/secrets/item/other").unwrap(),
                ],
                vec![],
            ),
            Case::LockedDenied => (vec![], vec![item]),
            _ => (vec![item], vec![]),
        }
    }
    #[zbus(out_args("output", "session"))]
    fn open_session(&self, algorithm: &str, input: OwnedValue) -> (OwnedValue, OwnedObjectPath) {
        assert_eq!(algorithm, "plain");
        assert!(String::try_from(input).unwrap().is_empty());
        self.evidence.lock().unwrap().opens += 1;
        (
            OwnedValue::try_from(Value::from("")).unwrap(),
            OwnedObjectPath::try_from(SESSION).unwrap(),
        )
    }
    #[zbus(out_args("unlocked", "prompt"))]
    fn unlock(
        &self,
        _items: Vec<OwnedObjectPath>,
    ) -> zbus::fdo::Result<(Vec<OwnedObjectPath>, OwnedObjectPath)> {
        assert_eq!(self.case, Case::LockedDenied);
        Err(zbus::fdo::Error::AccessDenied(
            "synthetic declined unlock".into(),
        ))
    }
}
struct Item {
    case: Case,
    evidence: Arc<Mutex<Evidence>>,
    replacement: zbus::Connection,
}
#[zbus::interface(name = "org.freedesktop.Secret.Item")]
impl Item {
    #[zbus(property)]
    fn attributes(&self) -> HashMap<String, String> {
        HashMap::from([(
            "application".into(),
            if self.case == Case::ForeignAttributes {
                "chrome"
            } else {
                "Slack"
            }
            .into(),
        )])
    }
    async fn get_secret(
        &self,
        session: OwnedObjectPath,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> (WireSecret,) {
        assert_eq!(session.as_str(), SESSION);
        self.evidence.lock().unwrap().reads += 1;
        if self.case == Case::OwnerChanged {
            connection.release_name(SERVICE).await.unwrap();
            self.replacement.request_name(SERVICE).await.unwrap();
        }
        let returned = if self.case == Case::WrongSession {
            OwnedObjectPath::try_from("/org/freedesktop/secrets/session/foreign").unwrap()
        } else {
            session
        };
        ((
            returned,
            vec![],
            PASSWORD.as_bytes().to_vec(),
            "text/plain; charset=utf-8".into(),
        ),)
    }
}
struct Session(Arc<Mutex<Evidence>>);
#[zbus::interface(name = "org.freedesktop.Secret.Session")]
impl Session {
    fn close(&self) {
        self.0.lock().unwrap().closes += 1;
    }
}
fn server(
    bus: &PrivateBus,
    case: Case,
    evidence: &Arc<Mutex<Evidence>>,
) -> (zbus::Connection, tokio::runtime::Runtime) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let connection = runtime.block_on(async {
        let replacement = zbus::connection::Builder::address(bus.address())
            .unwrap()
            .build()
            .await
            .unwrap();
        zbus::connection::Builder::address(bus.address())
            .unwrap()
            .name(SERVICE)
            .unwrap()
            .serve_at(
                SERVICE_PATH,
                SecretService {
                    case,
                    evidence: evidence.clone(),
                },
            )
            .unwrap()
            .serve_at(
                ITEM,
                Item {
                    case,
                    evidence: evidence.clone(),
                    replacement,
                },
            )
            .unwrap()
            .serve_at(SESSION, Session(evidence.clone()))
            .unwrap()
            .build()
            .await
            .unwrap()
    });
    (connection, runtime)
}

#[test]
fn native_selected_encrypted_slack_discovery_capture_seals_and_reopens_isolated_custody() {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let marker = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open("/run/tilo-native-chromium-qualification")
        .expect("explicit isolated fixture required");
    let facts = marker.metadata().unwrap();
    assert!(facts.is_file() && facts.uid() == 0 && facts.mode() & 0o022 == 0);
    assert_ne!(unsafe { libc::getuid() }, 0);
    assert_eq!(unsafe { libc::getuid() }, unsafe { libc::geteuid() });
    let package =
        imager_backend::setup::service_sessions::source::slack_linux::SlackApplication::at(
            Path::new("/fixture/slack"),
        )
        .expect("exact publicly pinned Slack package is mandatory");
    for case in [Case::Present, Case::Missing] {
        let bus = start_owner_bus();
        let evidence = Arc::new(Mutex::new(Evidence::default()));
        let service = server(&bus, case, &evidence);
        let home = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let profile = home.path().join(".config/Slack");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::set_permissions(&profile, std::fs::Permissions::from_mode(0o700)).unwrap();
        let database = rusqlite::Connection::open(profile.join("Cookies")).unwrap();
        database.execute_batch("CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT);INSERT INTO meta VALUES('version','23');CREATE TABLE cookies(creation_utc INTEGER,host_key TEXT,name TEXT,value TEXT,path TEXT,expires_utc INTEGER,is_secure INTEGER,is_httponly INTEGER,samesite INTEGER,has_expires INTEGER,encrypted_value BLOB,top_frame_site_key TEXT,source_scheme INTEGER,source_port INTEGER);").unwrap();
        // Independent OpenSSL ciphertext: existing synthetic-owner-password,
        // PBKDF2-SHA1 saltysalt/1, AES128CBC spaces IV. Never a user session.
        let encrypted =
            hex::decode("763131e13a0e822076151cad52880a2953bc9b7b75c5919b5f016f52db17787076816f")
                .unwrap();
        database.execute("INSERT INTO cookies VALUES(1,'.slack.com','selected','', '/',0,1,1,-1,0,?1,'',2,443)",rusqlite::params![encrypted]).unwrap();
        drop(database);
        let original = std::fs::read(profile.join("Cookies")).unwrap();
        let inventory = imager_backend::account_transfer::discover(
            &imager_backend::account_transfer::SourceRoots {
                home: home.path().to_owned(),
                config: home.path().join(".config"),
            },
        );
        let sources = service_profiles::discover_with_package(
            home.path(),
            &inventory.candidates,
            &|candidate| {
                if candidate.provider == imager_backend::account_transfer::Provider::Slack {
                    Some(Arc::clone(&package))
                } else {
                    None
                }
            },
        )
        .unwrap();
        let mut selection = Selection::from_discovery(sources).unwrap();
        assert_eq!(selection.detected.len(), 1);
        assert_eq!(selection.detected[0].2, Presence::NeedsAccess);
        assert!(selection.selected().is_empty());
        let id = selection.detected[0].0.clone();
        assert_eq!(selection.report()["sources"][0]["selectable"], true);
        assert_eq!(evidence.lock().unwrap().searches, 0);
        selection.set(&id, Service::Slack, true).unwrap();
        assert_eq!(evidence.lock().unwrap().searches, 0);
        let captured = selection
            .selected()
            .capture([7; 32], "isolated-slack-operation".into());
        if case == Case::Missing {
            assert!(captured.is_err());
            assert_eq!(evidence.lock().unwrap().reads, 0);
        } else {
            let captured = captured.unwrap();
            let (artifact, declaration) = captured.seal(b"synthetic transfer password").unwrap();
            let target = tempfile::Builder::new()
                .permissions(std::fs::Permissions::from_mode(0o700))
                .tempdir()
                .unwrap();
            let destination = target.path().join("custody");
            assert!(
                imager_backend::setup::service_sessions::import(
                    &artifact,
                    &declaration,
                    b"synthetic transfer password",
                    [8; 32],
                    "isolated-slack-operation",
                    &destination
                )
                .is_err()
            );
            assert!(!destination.exists());
            imager_backend::setup::service_sessions::import(
                &artifact,
                &declaration,
                b"synthetic transfer password",
                [7; 32],
                "isolated-slack-operation",
                &destination,
            )
            .unwrap();
            let account = accounts::AccountId::from(declaration.sessions[0].id.clone());
            let session = account_custody::Custodian::for_root(Some(destination))
                .service_session(&account, [7; 32], "isolated-slack-operation")
                .unwrap();
            assert_eq!(session.cookies[0].value, "synthetic-encrypted-session");
            assert_eq!(session.service, Service::Slack);
            assert!(!format!("{session:?}").contains("synthetic-encrypted-session"));
            assert!(
                !selection
                    .report()
                    .to_string()
                    .contains("synthetic-encrypted-session")
            );
            let observed = evidence.lock().unwrap();
            assert_eq!(observed.searches, 1);
            assert_eq!(observed.reads, 1);
            assert_eq!(observed.closes, 1);
        }
        assert_eq!(std::fs::read(profile.join("Cookies")).unwrap(), original);
        package.validate().unwrap();
        drop(service);
        drop(bus);
    }
}
