//! Explicit private-container test: actual production Slack owner-bus reader.
use super::*;
use crate::owner_migration::{ChromiumBrand, ChromiumKey};
use std::{
    io::BufRead,
    path::PathBuf,
    sync::{Arc, Mutex},
};

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
struct Bus {
    child: std::process::Child,
    address: String,
    socket: PathBuf,
}
impl Bus {
    fn start() -> Self {
        let owner = unsafe { libc::getuid() };
        assert_ne!(owner, 0);
        assert_eq!(owner, unsafe { libc::geteuid() });
        let socket = PathBuf::from(format!("/run/user/{owner}/bus"));
        assert!(!socket.exists(), "requires a fresh isolated owner bus");
        let mut child = std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .arg(format!("--address=unix:path={}", socket.display()))
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut address = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        assert!(!address.trim().is_empty());
        Self {
            child,
            address: address.trim().into(),
            socket,
        }
    }
}
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if self.socket.exists() {
            std::fs::remove_file(&self.socket).unwrap();
        }
    }
}
struct Service {
    case: Case,
    evidence: Arc<Mutex<Evidence>>,
}
#[zbus::interface(name = "org.freedesktop.Secret.Service")]
impl Service {
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
fn server(bus: &Bus, case: Case, evidence: &Arc<Mutex<Evidence>>) -> zbus::Connection {
    tilo_rt::app()
        .block_on(async {
            let replacement = zbus::connection::Builder::address(bus.address.as_str())
                .unwrap()
                .build()
                .await
                .unwrap();
            zbus::connection::Builder::address(bus.address.as_str())
                .unwrap()
                .name(SERVICE)
                .unwrap()
                .serve_at(
                    SERVICE_PATH,
                    Service {
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
        })
        .unwrap()
}
#[test]
fn native_selected_slack_key_qualifies_only_on_isolated_real_secret_service() {
    // An explicit native image marker prevents accidental host-keyring contact,
    // even when a developer runs all tests with this qualification feature.
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let marker = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open("/run/tilo-native-chromium-qualification")
        .expect("explicit owned native-container fixture required");
    let facts = marker.metadata().unwrap();
    assert!(facts.is_file() && facts.uid() == 0 && facts.mode() & 0o022 == 0);
    for case in [
        Case::Present,
        Case::Missing,
        Case::Ambiguous,
        Case::ForeignAttributes,
        Case::OwnerChanged,
        Case::WrongSession,
        Case::LockedDenied,
    ] {
        let bus = Bus::start();
        let evidence = Arc::new(Mutex::new(Evidence::default()));
        let service = server(&bus, case, &evidence);
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("witness");
        std::fs::write(&path, b"unchanged source").unwrap();
        let before = std::fs::metadata(&path).unwrap();
        let directory = std::fs::File::open(root.path()).unwrap();
        let request = ChromiumKey::new(&directory, ChromiumBrand::Slack452178).unwrap();
        assert_eq!(evidence.lock().unwrap().searches, 0);
        assert!(
            ChromiumKey::new(
                &std::fs::File::open("/run").unwrap(),
                ChromiumBrand::Slack452178
            )
            .is_err()
        );
        assert_eq!(evidence.lock().unwrap().searches, 0);
        let result = request.read_blocking();
        match case {
            Case::Present => {
                let value = result.unwrap().unwrap();
                assert_eq!(value.expose(), PASSWORD.as_bytes());
                assert!(!format!("{value:?}").contains(PASSWORD));
            }
            Case::Missing => assert!(result.unwrap().is_none()),
            _ => assert!(matches!(result, Err(SecretError::Denied))),
        }
        let observed = evidence.lock().unwrap();
        assert_eq!(observed.searches, 1);
        let read = usize::from(matches!(
            case,
            Case::Present | Case::OwnerChanged | Case::WrongSession
        ));
        assert_eq!(observed.reads, read);
        assert_eq!(observed.opens, read);
        assert_eq!(observed.closes, read);
        drop(observed);
        assert_eq!(std::fs::read(&path).unwrap(), b"unchanged source");
        let after = std::fs::metadata(&path).unwrap();
        assert_eq!(
            (
                after.dev(),
                after.ino(),
                after.uid(),
                after.mode(),
                after.mtime(),
                after.mtime_nsec()
            ),
            (
                before.dev(),
                before.ino(),
                before.uid(),
                before.mode(),
                before.mtime(),
                before.mtime_nsec()
            )
        );
        drop(service);
        drop(bus);
    }
}
